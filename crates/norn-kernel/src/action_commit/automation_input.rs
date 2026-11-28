//! Static admission of an automation's `then.input` against its target
//! action's closed input schema (`AUTOMATION-1`).

use std::collections::{BTreeMap, BTreeSet};

use norn_expr::schema::Schema;
use norn_ir::{ActionRoot, ResourceLimitProfile, RuntimeValue};

use crate::cell_host::JsonObject;

use super::ActionRefusal;
use super::input::{
    action_link_key, actor_derived_fields, check_call_receipt_input, check_canonical_object_bound,
    reserved_action_identity, rfc6901_token,
};
use super::post_slice::clock_derived_fields;

/// The shortest target id the runtime can inject when the target id schema
/// leaves it unconstrained (an object key is nonblank). See
/// [`shortest_target_id`].
const SHORTEST_TARGET_ID: &str = "_";

/// The legal target id with the smallest canonical encoding, which admission
/// injects to measure the receipt input: a refused static input then
/// overflows the bound for every matched record, and longer ids stay bounded
/// per call at dispatch. An action input string schema admits only `enum`
/// as a constraint (`norn_expr::schema`, which refuses `const`,
/// `minLength` and `pattern`), so an enum on the target id field is the one
/// lower bound to derive; without one, the bound is one byte. `None` when
/// the enum admits no nonblank id: no run can address an object.
fn shortest_target_id(action: &ActionRoot, target_id_field: &str) -> Option<RuntimeValue> {
    let encoded_len = |id: &str| {
        norn_ir::canonical::canonical_json_of(&RuntimeValue::String(id.into()))
            .map_or(usize::MAX, |bytes| bytes.len())
    };
    let Some(values) = action
        .input_schema
        .get("properties")
        .and_then(|properties| properties.as_object())
        .and_then(|properties| properties.get(target_id_field))
        .and_then(|schema| schema.as_object())
        .and_then(|schema| schema.get("enum"))
        .and_then(|values| values.as_array())
    else {
        return Some(RuntimeValue::String(SHORTEST_TARGET_ID.into()));
    };
    values
        .iter()
        .filter_map(|value| value.as_string())
        .filter(|id| !id.trim().is_empty())
        .min_by_key(|id| (encoded_len(id), *id))
        .map(|id| RuntimeValue::String(id.into()))
}

/// One defect reported by [`validate_automation_static_input`] when admitting
/// an automation's `then.input` against its target action's closed input schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomationInputViolation {
    /// RFC 6901 JSON pointer under `/then/input`.
    pub pointer: String,
    /// Human-readable reason the static input was refused.
    pub message: String,
}

/// Converts a schema violation path anchored at `base_pointer` (where
/// `norn_expr::schema::Schema` appends `.member` and `[index]`) to an
/// RFC 6901 pointer.
fn nested_schema_pointer(base_pointer: &str, schema_path: &str) -> String {
    let Some(suffix) = schema_path.strip_prefix(base_pointer) else {
        return base_pointer.to_owned();
    };
    if suffix.is_empty() {
        return base_pointer.to_owned();
    }
    let mut out = base_pointer.to_owned();
    let mut rest = suffix;
    while !rest.is_empty() {
        if let Some(after_dot) = rest.strip_prefix('.') {
            let end = after_dot.find(['.', '[']).unwrap_or(after_dot.len());
            if let Some(segment) = after_dot.get(..end) {
                out.push('/');
                out.push_str(&rfc6901_token(segment));
            }
            rest = after_dot.get(end..).unwrap_or("");
        } else if let Some(after_bracket) = rest.strip_prefix('[') {
            if let Some(close) = after_bracket.find(']') {
                if let Some(index) = after_bracket.get(..close) {
                    out.push('/');
                    out.push_str(index);
                }
                rest = after_bracket.get(close.saturating_add(1)..).unwrap_or("");
            } else {
                break;
            }
        } else {
            break;
        }
    }
    out
}

/// Validate an automation's static `then.input` (or `None` when omitted)
/// against `action`'s closed input schema, accounting for the target object id
/// field injected at runtime and server-derived actor/optional fields.
///
/// Reuses the exact same closed schema parser, strict wire decimal decoder,
/// and reserved identity-field checks (`ACTION-ACTOR-1`) as
/// [`super::prepare_action_input_for_call`], so invalid automations refuse at
/// admission instead of failing on every run.
///
/// # Errors
/// Returns one or more [`AutomationInputViolation`] items when the static
/// input violates the action's closed input contract.
pub fn validate_automation_static_input(
    action: &ActionRoot,
    static_input: Option<&JsonObject>,
) -> Result<(), Vec<AutomationInputViolation>> {
    let Some(program) = &action.effect_program else {
        return Err(vec![AutomationInputViolation {
            pointer: "/then/actionId".to_owned(),
            message: "target action has no executable effect program".to_owned(),
        }]);
    };
    let target_id_field = program.target().object_id_field;
    let empty = JsonObject::new();
    let supplied = static_input.unwrap_or(&empty);
    check_bounds_and_identity(supplied)?;

    let raw_schema = norn_ir::JsonValue::Object(action.input_schema.clone());
    let parsed = Schema::parse_action(&raw_schema, "/then/input").map_err(|violation| {
        vec![AutomationInputViolation {
            pointer: nested_schema_pointer("/then/input", &violation.path),
            message: violation.reason,
        }]
    })?;
    let Schema::Object {
        properties,
        required,
    } = &parsed
    else {
        return Err(vec![AutomationInputViolation {
            pointer: "/then/input".to_owned(),
            message: "target action input schema must be a closed object".to_owned(),
        }]);
    };

    let mut server_fields = actor_derived_fields(action);
    server_fields.extend(clock_derived_fields(action));
    let mut violations = Vec::new();
    check_reserved_and_required(
        action,
        target_id_field,
        &server_fields,
        required,
        supplied,
        &mut violations,
    );
    check_supplied_values(
        action,
        target_id_field,
        &server_fields,
        properties,
        supplied,
        &mut violations,
    );
    if violations.is_empty() {
        check_receipt_input(action, target_id_field, supplied)
    } else {
        Err(violations)
    }
}

/// #1704: the action-specific receipt input bound, on the input the runtime
/// will submit (the static input plus the injected target id).
fn check_receipt_input(
    action: &ActionRoot,
    target_id_field: &str,
    supplied: &JsonObject,
) -> Result<(), Vec<AutomationInputViolation>> {
    let Some(target_id) = shortest_target_id(action, target_id_field) else {
        return Err(vec![AutomationInputViolation {
            pointer: "/then/actionId".to_owned(),
            message: format!(
                "target action's `{target_id_field}` enum admits no nonblank target id, so no run can address an object"
            ),
        }]);
    };
    let mut call_input = supplied.clone();
    call_input.insert(target_id_field.to_owned(), target_id);
    check_call_receipt_input::<()>(action, &call_input).map_err(|refusal| {
        let message = if let ActionRefusal::InputBoundExceeded {
            subject,
            actual_bytes,
            max_bytes,
        } = &refusal
        {
            format!(
                "static input with the injected target id is {actual_bytes} canonical bytes, over the {max_bytes}-byte {subject} bound of the target action"
            )
        } else {
            format!("static input is refused by the target action: {refusal:?}")
        };
        vec![AutomationInputViolation {
            pointer: "/then/input".to_owned(),
            message,
        }]
    })
}

fn check_bounds_and_identity(supplied: &JsonObject) -> Result<(), Vec<AutomationInputViolation>> {
    let profile = ResourceLimitProfile::norn_limits_v1();
    let max_input_bytes = usize::try_from(profile.slice.max_canonical_bytes).unwrap_or(usize::MAX);
    if let Err(error) =
        check_canonical_object_bound::<()>("automation static input", supplied, max_input_bytes)
    {
        return Err(vec![AutomationInputViolation {
            pointer: "/then/input".to_owned(),
            message: format!("static input exceeds canonical size bound: {error:?}"),
        }]);
    }
    if let Some((field, path)) = reserved_action_identity(supplied) {
        return Err(vec![AutomationInputViolation {
            pointer: format!("/then/input{path}"),
            message: format!("static input must not supply reserved field `{field}`"),
        }]);
    }
    Ok(())
}

fn check_reserved_and_required(
    action: &ActionRoot,
    target_id_field: &str,
    actor_fields: &BTreeSet<String>,
    required: &BTreeSet<String>,
    supplied: &JsonObject,
    violations: &mut Vec<AutomationInputViolation>,
) {
    let mut server_supplied = BTreeSet::<String>::new();
    server_supplied.insert(target_id_field.to_owned());
    server_supplied.extend(actor_fields.iter().cloned());
    for step in &action.derive {
        if !step.input_required {
            server_supplied.insert(step.field.clone());
        }
    }

    if supplied.contains_key(target_id_field) {
        violations.push(AutomationInputViolation {
            pointer: format!("/then/input/{}", rfc6901_token(target_id_field)),
            message: format!(
                "target id field `{target_id_field}` is injected by the runtime from the matched record"
            ),
        });
    }
    for field in actor_fields {
        if supplied.contains_key(field) {
            violations.push(AutomationInputViolation {
                pointer: format!("/then/input/{}", rfc6901_token(field)),
                message: format!(
                    "field `{field}` is derived from the server actor and must not be set in static input"
                ),
            });
        }
    }
    let mut reported_missing = BTreeSet::<String>::new();
    for step in &action.derive {
        if step.input_required
            && step.field != target_id_field
            && !actor_fields.contains(&step.field)
            && !supplied.contains_key(&step.field)
            && reported_missing.insert(step.field.clone())
        {
            violations.push(AutomationInputViolation {
                pointer: format!("/then/input/{}", rfc6901_token(&step.field)),
                message: format!("required input field `{}` is missing", step.field),
            });
        }
    }
    for link in &action.create_links {
        let fields = [
            Some(link.target_id_field.as_str()),
            link.source.as_ref().map(|source| source.id_field.as_str()),
        ];
        for field in fields.into_iter().flatten() {
            if !server_supplied.contains(field)
                && !supplied.contains_key(field)
                && reported_missing.insert(field.to_owned())
            {
                violations.push(AutomationInputViolation {
                    pointer: format!("/then/input/{}", rfc6901_token(field)),
                    message: format!("required input field `{field}` is missing"),
                });
            }
        }
    }
    for field in required {
        if !server_supplied.contains(field)
            && !supplied.contains_key(field)
            && reported_missing.insert(field.clone())
        {
            violations.push(AutomationInputViolation {
                pointer: format!("/then/input/{}", rfc6901_token(field)),
                message: format!("required input field `{field}` is missing"),
            });
        }
    }
}

fn check_supplied_values(
    action: &ActionRoot,
    target_id_field: &str,
    actor_fields: &BTreeSet<String>,
    properties: &BTreeMap<String, Schema>,
    supplied: &JsonObject,
    violations: &mut Vec<AutomationInputViolation>,
) {
    for (name, raw_value) in supplied {
        if name == target_id_field || actor_fields.contains(name) {
            continue;
        }
        let field_pointer = format!("/then/input/{}", rfc6901_token(name));
        let Some(field_schema) = properties.get(name) else {
            violations.push(AutomationInputViolation {
                pointer: field_pointer,
                message: format!("input field `{name}` is not declared by the target action"),
            });
            continue;
        };
        if *raw_value == RuntimeValue::Null
            && action
                .derive
                .iter()
                .any(|step| !step.input_required && step.field == *name)
        {
            continue;
        }
        let decoded = match field_schema.decode_action_input(raw_value.clone(), &field_pointer) {
            Ok(decoded) => decoded,
            Err(violation) => {
                violations.push(AutomationInputViolation {
                    pointer: nested_schema_pointer(&field_pointer, &violation.path),
                    message: violation.reason,
                });
                continue;
            }
        };
        let link_endpoint = action.create_links.iter().any(|link| {
            link.target_id_field == *name
                || link
                    .source
                    .as_ref()
                    .is_some_and(|source| source.id_field == *name)
        });
        if link_endpoint && action_link_key(&decoded).is_none() {
            violations.push(AutomationInputViolation {
                pointer: field_pointer,
                message: format!("link endpoint field `{name}` must be a nonblank string"),
            });
            continue;
        }
        let expr_val = match norn_expr::Value::try_from(&decoded) {
            Ok(value) => value,
            Err(error) => {
                violations.push(AutomationInputViolation {
                    pointer: field_pointer,
                    message: error.to_string(),
                });
                continue;
            }
        };
        if let Err(violation) = field_schema.check(&expr_val, &field_pointer) {
            violations.push(AutomationInputViolation {
                pointer: nested_schema_pointer(&field_pointer, &violation.path),
                message: violation.reason,
            });
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "unit tests for static automation input admission"
)]
mod tests {
    use super::*;

    fn sample_action() -> ActionRoot {
        serde_json::from_value(serde_json::json!({
            "actionId": "demo.order.adjust",
            "scopeRequired": [],
            "idempotent": true,
            "ledgered": false,
            "outputSchema": { "type": "object", "additionalProperties": false, "properties": {} },
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "required": ["orderId", "amount", "updatedBy", "meta"],
                "properties": {
                    "orderId": { "type": "string" },
                    "amount": { "type": "number", "nornType": { "kind": "decimal", "scale": 2 } },
                    "updatedBy": { "type": "string" },
                    "meta": {
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["reason"],
                        "properties": {
                            "reason": { "type": "string" }
                        }
                    },
                    "tags": {
                        "type": "array",
                        "items": { "type": "string" }
                    }
                }
            },
            "derive": [
                {
                    "field": "updatedBy",
                    "expression": "actor.subjectId",
                    "inputRequired": false
                }
            ],
            "effectProgram": {
                "kind": "upsert_input_object",
                "objectIdField": "orderId",
                "objectKind": "demo.Order",
                "entityArtifactId": "demo.Order.entity",
                "entityArtifactVersion": "0.1.0"
            }
        }))
        .expect("sample action")
    }

    #[test]
    fn validates_required_nested_decimal_and_injected_fields() {
        let action = sample_action();

        let omitted = validate_automation_static_input(&action, None).unwrap_err();
        let pointers: BTreeSet<&str> = omitted.iter().map(|v| v.pointer.as_str()).collect();
        assert!(pointers.contains("/then/input/amount"), "{omitted:?}");
        assert!(pointers.contains("/then/input/meta"), "{omitted:?}");
        assert!(!pointers.contains("/then/input/orderId"), "{omitted:?}");
        assert!(!pointers.contains("/then/input/updatedBy"), "{omitted:?}");

        let valid = JsonObject::from([
            ("amount".to_owned(), RuntimeValue::String("12.50".into())),
            (
                "meta".to_owned(),
                RuntimeValue::Object(JsonObject::from([(
                    "reason".to_owned(),
                    RuntimeValue::String("stale".into()),
                )])),
            ),
            (
                "tags".to_owned(),
                RuntimeValue::Array(vec![RuntimeValue::String("auto".into())]),
            ),
        ]);
        assert_eq!(
            validate_automation_static_input(&action, Some(&valid)),
            Ok(())
        );

        let mut bad_scale = valid.clone();
        bad_scale.insert("amount".to_owned(), RuntimeValue::String("12.505".into()));
        assert!(validate_automation_static_input(&action, Some(&bad_scale)).is_err());

        let mut bad_nested = valid.clone();
        bad_nested.insert(
            "meta".to_owned(),
            RuntimeValue::Object(JsonObject::from([
                ("reason".to_owned(), RuntimeValue::String("stale".into())),
                ("extra".to_owned(), RuntimeValue::Bool(true)),
            ])),
        );
        assert!(validate_automation_static_input(&action, Some(&bad_nested)).is_err());

        let mut with_target_id = valid.clone();
        with_target_id.insert("orderId".to_owned(), RuntimeValue::String("o-1".into()));
        assert!(validate_automation_static_input(&action, Some(&with_target_id)).is_err());

        let mut with_actor_field = valid.clone();
        with_actor_field.insert(
            "updatedBy".to_owned(),
            RuntimeValue::String("spoofed".into()),
        );
        assert!(validate_automation_static_input(&action, Some(&with_actor_field)).is_err());

        let mut with_reserved_actor = valid;
        with_reserved_actor.insert("actor".to_owned(), RuntimeValue::String("spoofed".into()));
        let root = validate_automation_static_input(&action, Some(&with_reserved_actor))
            .expect_err("root actor is reserved");
        assert_eq!(
            root.first().map(|error| error.pointer.as_str()),
            Some("/then/input/actor")
        );

        let nested = RuntimeValue::from_json_text(r#"{"meta":{"a/b~c":[{"actor":"spoofed"}]}}"#)
            .expect("static input");
        let errors = validate_automation_static_input(&action, nested.as_object())
            .expect_err("nested actor is reserved");
        assert_eq!(
            errors.first().map(|error| error.pointer.as_str()),
            Some("/then/input/meta/a~1b~0c/0/actor")
        );
    }
    #[test]
    fn static_link_endpoints_reject_whitespace_without_restricting_other_strings() {
        let mut raw = serde_json::to_value(sample_action()).expect("action JSON");
        for field in ["sourceId", "targetId"] {
            raw["inputSchema"]["properties"][field] = serde_json::json!({ "type": "string" });
        }
        raw["createLinks"] = serde_json::json!([{
            "source": { "objectType": "demo.Order", "idField": "sourceId" },
            "link": "related",
            "targetType": "demo.Order",
            "targetIdField": "targetId",
        }]);
        let action: ActionRoot = serde_json::from_value(raw.clone()).expect("action with link");
        let input = RuntimeValue::from_json_text(
            r#"{"amount":"12.50","meta":{"reason":" "},"sourceId":"source","targetId":"target"}"#,
        )
        .expect("input");
        let input = input.as_object().expect("object");
        assert_eq!(
            validate_automation_static_input(&action, Some(input)),
            Ok(())
        );
        for (field, path) in [
            ("sourceId", "/then/input/sourceId"),
            ("targetId", "/then/input/targetId"),
        ] {
            let mut invalid = input.clone();
            invalid.insert(field.to_owned(), RuntimeValue::String(" \t ".into()));
            let errors = validate_automation_static_input(&action, Some(&invalid))
                .expect_err("blank link endpoint");
            assert_eq!(
                errors.first().map(|error| error.pointer.as_str()),
                Some(path)
            );

            let mut missing = input.clone();
            missing.remove(field);
            let errors = validate_automation_static_input(&action, Some(&missing))
                .expect_err("omitted link endpoint");
            assert_eq!(
                errors.first().map(|error| error.pointer.as_str()),
                Some(path)
            );
        }

        // A link endpoint matching the server-injected target id field stays exempt.
        raw["createLinks"] = serde_json::json!([{
            "source": { "objectType": "demo.Order", "idField": "orderId" },
            "link": "related",
            "targetType": "demo.Order",
            "targetIdField": "targetId",
        }]);
        let injected_source_action: ActionRoot =
            serde_json::from_value(raw).expect("action with injected source link");
        let mut without_source = input.clone();
        without_source.remove("sourceId");
        assert_eq!(
            validate_automation_static_input(&injected_source_action, Some(&without_source)),
            Ok(())
        );
    }

    fn input_with_tag(tag: String) -> JsonObject {
        JsonObject::from([
            ("amount".to_owned(), RuntimeValue::String("12.50".into())),
            (
                "meta".to_owned(),
                RuntimeValue::Object(JsonObject::from([(
                    "reason".to_owned(),
                    RuntimeValue::String("stale".into()),
                )])),
            ),
            (
                "tags".to_owned(),
                RuntimeValue::Array(vec![RuntimeValue::String(tag)]),
            ),
        ])
    }

    /// #1704: admission applies the receipt input bound the call meets at
    /// runtime, on the static input plus the injected target id, so a
    /// schema-valid input that every dispatch would refuse never installs.
    #[test]
    fn refuses_static_input_over_the_receipt_input_bound_with_the_injected_target() {
        let action = sample_action();
        let max_bytes = usize::try_from(
            ResourceLimitProfile::norn_limits_v1()
                .envelope
                .max_output_bytes,
        )
        .expect("bound");
        // The exact input the runtime submits: static input plus target id.
        let call_bytes = |input: &JsonObject| {
            let mut call = input.clone();
            call.insert(
                "orderId".to_owned(),
                RuntimeValue::String(SHORTEST_TARGET_ID.into()),
            );
            norn_ir::canonical::canonical_json_of(&call)
                .expect("canonical")
                .len()
        };
        let base = call_bytes(&input_with_tag(String::new()));
        let at_bound = input_with_tag("a".repeat(max_bytes.saturating_sub(base)));
        assert_eq!(call_bytes(&at_bound), max_bytes);
        assert_eq!(
            validate_automation_static_input(&action, Some(&at_bound)),
            Ok(())
        );
        let over = input_with_tag("a".repeat(max_bytes.saturating_sub(base).saturating_add(1)));
        let errors = validate_automation_static_input(&action, Some(&over))
            .expect_err("one byte over the receipt input bound");
        assert_eq!(
            errors
                .iter()
                .map(|error| (error.pointer.as_str(), error.message.as_str()))
                .collect::<Vec<_>>(),
            vec![(
                "/then/input",
                format!(
                    "static input with the injected target id is {} canonical bytes, over the {max_bytes}-byte action call receipt input bound of the target action",
                    max_bytes.saturating_add(1)
                )
                .as_str()
            )]
        );
        // The 40 KiB case from the audit: under the 256 KiB slice bound, over
        // the receipt input bound.
        assert!(
            validate_automation_static_input(&action, Some(&input_with_tag("a".repeat(40_960))))
                .is_err()
        );

        // An action without preparation whose input normalizes to itself
        // keeps the runtime fast path, which has no receipt input bound: its
        // admission does not invent one.
        let mut raw = serde_json::to_value(sample_action()).expect("action JSON");
        raw["derive"] = serde_json::json!([]);
        raw["inputSchema"]["required"] = serde_json::json!(["orderId", "meta"]);
        for field in ["amount", "updatedBy"] {
            raw["inputSchema"]["properties"]
                .as_object_mut()
                .expect("properties")
                .remove(field);
        }
        let plain: ActionRoot = serde_json::from_value(raw).expect("plain action");
        let mut large = input_with_tag("a".repeat(40_960));
        large.remove("amount");
        assert_eq!(
            validate_automation_static_input(&plain, Some(&large)),
            Ok(())
        );
    }

    /// #1704 round 2: when the target id schema is an enum, admission
    /// measures the legal id with the smallest canonical encoding, not a
    /// one-byte placeholder. With `AB` the shortest legal id, an input that
    /// would fit with `_` but not with `AB` can never run, so it is refused.
    #[test]
    fn measures_the_shortest_legal_enum_target_id() {
        let mut raw = serde_json::to_value(sample_action()).expect("action JSON");
        raw["inputSchema"]["properties"]["orderId"]["enum"] =
            serde_json::json!(["ABCD", "AB", "ABC"]);
        let action: ActionRoot = serde_json::from_value(raw).expect("enum action");
        let max_bytes = usize::try_from(
            ResourceLimitProfile::norn_limits_v1()
                .envelope
                .max_output_bytes,
        )
        .expect("bound");
        let call_bytes = |input: &JsonObject, id: &str| {
            let mut call = input.clone();
            call.insert("orderId".to_owned(), RuntimeValue::String(id.into()));
            norn_ir::canonical::canonical_json_of(&call)
                .expect("canonical")
                .len()
        };
        let base = call_bytes(&input_with_tag(String::new()), "AB");
        let fits = input_with_tag("a".repeat(max_bytes.saturating_sub(base)));
        assert_eq!(call_bytes(&fits, "AB"), max_bytes);
        assert_eq!(
            validate_automation_static_input(&action, Some(&fits)),
            Ok(())
        );
        // Astra's case: exactly at the bound with a one-byte id, one byte
        // over it with every legal id.
        let never_runs =
            input_with_tag("a".repeat(max_bytes.saturating_sub(base).saturating_add(1)));
        assert_eq!(call_bytes(&never_runs, SHORTEST_TARGET_ID), max_bytes);
        assert_eq!(call_bytes(&never_runs, "AB"), max_bytes.saturating_add(1));
        let errors = validate_automation_static_input(&action, Some(&never_runs))
            .expect_err("over the bound with every legal id");
        assert_eq!(
            errors
                .iter()
                .map(|error| (error.pointer.as_str(), error.message.as_str()))
                .collect::<Vec<_>>(),
            vec![(
                "/then/input",
                format!(
                    "static input with the injected target id is {} canonical bytes, over the {max_bytes}-byte action call receipt input bound of the target action",
                    max_bytes.saturating_add(1)
                )
                .as_str()
            )]
        );
    }

    /// An enum of only blank target ids admits no object key, so every run
    /// would be refused: admission refuses it instead of measuring `_`.
    #[test]
    fn refuses_a_target_id_enum_of_only_blank_values() {
        let mut raw = serde_json::to_value(sample_action()).expect("action JSON");
        raw["inputSchema"]["properties"]["orderId"]["enum"] = serde_json::json!(["", "  "]);
        let action: ActionRoot = serde_json::from_value(raw).expect("enum action");
        let errors =
            validate_automation_static_input(&action, Some(&input_with_tag(String::new())))
                .expect_err("no legal target id");
        assert_eq!(
            errors
                .iter()
                .map(|error| (error.pointer.as_str(), error.message.as_str()))
                .collect::<Vec<_>>(),
            vec![(
                "/then/actionId",
                "target action's `orderId` enum admits no nonblank target id, so no run can address an object"
            )]
        );
    }
}
