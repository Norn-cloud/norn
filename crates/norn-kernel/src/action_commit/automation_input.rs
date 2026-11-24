//! Static admission of an automation's `then.input` against its target
//! action's closed input schema (`AUTOMATION-1`).

use std::collections::{BTreeMap, BTreeSet};

use norn_expr::schema::Schema;
use norn_ir::{ActionRoot, ResourceLimitProfile, RuntimeValue};

use crate::cell_host::JsonObject;

use super::input::{
    actor_derived_fields, check_action_identity_input, check_canonical_object_bound,
};
use super::types::ActionRefusal;

/// One defect reported by [`validate_automation_static_input`] when admitting
/// an automation's `then.input` against its target action's closed input schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutomationInputViolation {
    /// RFC 6901 JSON pointer under `/then/input`.
    pub pointer: String,
    /// Human-readable reason the static input was refused.
    pub message: String,
}

fn rfc6901_token(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
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
    check_bounds_and_identity(action, supplied)?;

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

    let actor_fields = actor_derived_fields(action);
    let mut violations = Vec::new();
    check_reserved_and_required(
        action,
        target_id_field,
        &actor_fields,
        required,
        supplied,
        &mut violations,
    );
    check_supplied_values(
        action,
        target_id_field,
        &actor_fields,
        properties,
        supplied,
        &mut violations,
    );
    if violations.is_empty() {
        Ok(())
    } else {
        Err(violations)
    }
}

fn check_bounds_and_identity(
    action: &ActionRoot,
    supplied: &JsonObject,
) -> Result<(), Vec<AutomationInputViolation>> {
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
    if let Err(error) = check_action_identity_input::<()>(action, supplied, None, max_input_bytes) {
        let (pointer, message) = if let ActionRefusal::InvalidInput { field } = error {
            (
                format!("/then/input/{}", rfc6901_token(&field)),
                format!("static input must not supply reserved field `{field}`"),
            )
        } else {
            (
                "/then/input".to_owned(),
                format!("static input failed identity check: {error:?}"),
            )
        };
        return Err(vec![AutomationInputViolation { pointer, message }]);
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
            "actionVersion": "0.1",
            "actionId": "demo.order.adjust",
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
        assert!(validate_automation_static_input(&action, Some(&with_reserved_actor)).is_err());
    }
}
