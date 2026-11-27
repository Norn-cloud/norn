//! Differential MCP projection <-> kernel input preparation parity tests.
//!
//! Verifies that `norn_projections::mcp::action_tool_with_server_filled` (fed
//! `norn_kernel::actor_derived_fields`, exactly as `norn-cell-do` serves MCP)
//! agrees with `norn_kernel::prepare_action_input_for_call` across randomized
//! derivations and decimal wire schemas.

use std::collections::BTreeSet;

use norn_ir::{
    ActionInputDerivation, ActionRoot, Decimal, JsonObject, JsonValue, RuntimeObject, RuntimeValue,
    ZenExpression,
};
use norn_kernel::{ActionRefusal, actor_derived_fields, prepare_action_input_for_call};
use norn_projections::mcp::{action_tool, action_tool_with_server_filled};
use proptest::prelude::*;
use serde_json::{Value, json};

#[derive(Debug, Clone, Copy)]
enum ExprKind {
    TrimInput,
    DefaultInput,
    RootDefaultInput,
    DollarNonRootInput,
    ClosureConstant,
    ActorSubject,
    RootActorSubject,
    InputOrActor,
    Constant,
    Malformed,
}

impl ExprKind {
    fn render(self, field: &str) -> String {
        match self {
            Self::TrimInput => format!("trim(input.{field})"),
            Self::DefaultInput => format!("input.{field} ?? 'fallback'"),
            Self::RootDefaultInput => format!("$root.input.{field} ?? 'root-fallback'"),
            Self::DollarNonRootInput => format!("$.input.{field} ?? 'dollar-fallback'"),
            Self::ClosureConstant => "len(map([1], #)) > 0 ? 'yes' : 'no'".to_owned(),
            Self::ActorSubject => "actor.subjectId".to_owned(),
            Self::RootActorSubject => "$root.actor.subjectId".to_owned(),
            Self::InputOrActor => format!("input.{field} ?? actor.subjectId"),
            Self::Constant => "'fixed'".to_owned(),
            Self::Malformed => "1 +".to_owned(),
        }
    }

    const fn is_malformed(self) -> bool {
        matches!(self, Self::Malformed)
    }
}

const FIELDS: [&str; 4] = ["a", "b", "c", "d"];

fn field_is_optional_at_step(
    required_mask: [bool; 4],
    steps: &[(usize, bool)],
    step_index: usize,
    field_idx: usize,
) -> bool {
    let filled_earlier = steps
        .get(..step_index)
        .is_some_and(|prior| prior.iter().any(|&(idx, _)| idx == field_idx));
    if filled_earlier {
        return false;
    }
    let schema_required = required_mask.get(field_idx).copied().unwrap_or(false);
    let any_optional_derive = steps
        .iter()
        .any(|&(idx, input_required)| idx == field_idx && !input_required);
    !schema_required || any_optional_derive
}

fn has_runtime_null_trim(
    steps: &[(usize, ExprKind, bool)],
    projected_required: &BTreeSet<String>,
) -> bool {
    steps
        .iter()
        .enumerate()
        .any(|(step_index, &(idx, kind, _))| {
            if !matches!(kind, ExprKind::TrimInput) {
                return false;
            }
            let field = FIELDS.get(idx).copied().unwrap_or("a");
            let supplied = projected_required.contains(field);
            let filled_earlier = steps
                .get(..step_index)
                .is_some_and(|prior| prior.iter().any(|&(p_idx, _, _)| p_idx == idx));
            !supplied && !filled_earlier
        })
}

fn build_type_admitted_steps(
    required_mask: [bool; 4],
    field_is_actor: [bool; 4],
    field_input_required: [bool; 4],
    raw_steps: &[(usize, bool)],
) -> Vec<(usize, ExprKind, bool)> {
    let skeleton: Vec<(usize, bool)> = raw_steps
        .iter()
        .map(|&(idx, _)| {
            let is_actor = field_is_actor.get(idx).copied().unwrap_or(false);
            let schema_req = required_mask.get(idx).copied().unwrap_or(false);
            let field_req = field_input_required.get(idx).copied().unwrap_or(false);
            (idx, !is_actor && schema_req && field_req)
        })
        .collect();
    raw_steps
        .iter()
        .enumerate()
        .map(|(step_index, &(idx, prefer_primary))| {
            let is_actor = field_is_actor.get(idx).copied().unwrap_or(false);
            let req = skeleton.get(step_index).is_some_and(|&(_, r)| r);
            let is_optional = field_is_optional_at_step(required_mask, &skeleton, step_index, idx);
            let kind = if is_actor {
                if is_optional && prefer_primary {
                    ExprKind::InputOrActor
                } else {
                    ExprKind::ActorSubject
                }
            } else if is_optional {
                if prefer_primary {
                    ExprKind::DefaultInput
                } else {
                    ExprKind::Constant
                }
            } else if prefer_primary {
                ExprKind::TrimInput
            } else {
                ExprKind::Constant
            };
            (idx, kind, req)
        })
        .collect()
}

#[derive(Debug, Clone, Copy)]
enum DecimalShape {
    Scalar,
    NullableArray,
    NullableArrayNullFirst,
}

impl DecimalShape {
    fn to_json(self) -> Value {
        match self {
            Self::Scalar => json!({"type": "number", "nornType": {"kind": "decimal", "scale": 2}}),
            Self::NullableArray => {
                json!({"type": ["number", "null"], "nornType": {"kind": "decimal", "scale": 2}})
            }
            Self::NullableArrayNullFirst => {
                json!({"type": ["null", "number"], "nornType": {"kind": "decimal", "scale": 2}})
            }
        }
    }

    const fn allows_null(self) -> bool {
        match self {
            Self::Scalar => false,
            Self::NullableArray | Self::NullableArrayNullFirst => true,
        }
    }
}

fn expr_kind_strategy() -> impl Strategy<Value = ExprKind> {
    prop_oneof![
        Just(ExprKind::TrimInput),
        Just(ExprKind::DefaultInput),
        Just(ExprKind::RootDefaultInput),
        Just(ExprKind::DollarNonRootInput),
        Just(ExprKind::ClosureConstant),
        Just(ExprKind::ActorSubject),
        Just(ExprKind::RootActorSubject),
        Just(ExprKind::InputOrActor),
        Just(ExprKind::Constant),
        Just(ExprKind::Malformed),
    ]
}

fn decimal_shape_strategy() -> impl Strategy<Value = DecimalShape> {
    prop_oneof![
        Just(DecimalShape::Scalar),
        Just(DecimalShape::NullableArray),
        Just(DecimalShape::NullableArrayNullFirst),
    ]
}

fn schema_has_raw_decimal_number(value: &JsonValue) -> bool {
    match value {
        JsonValue::Object(map) => {
            let is_decimal = map
                .get("nornType")
                .and_then(JsonValue::as_object)
                .and_then(|site| site.get("kind"))
                .and_then(JsonValue::as_string)
                == Some("decimal");
            if is_decimal {
                let raw_number = match map.get("type") {
                    Some(JsonValue::String(kind)) => kind == "number",
                    Some(JsonValue::Array(items)) => {
                        items.iter().any(|item| item.as_string() == Some("number"))
                    }
                    None
                    | Some(
                        JsonValue::Null
                        | JsonValue::Bool(_)
                        | JsonValue::Number(_)
                        | JsonValue::Object(_),
                    ) => false,
                };
                if raw_number {
                    return true;
                }
            }
            map.values().any(schema_has_raw_decimal_number)
        }
        JsonValue::Array(items) => items.iter().any(schema_has_raw_decimal_number),
        JsonValue::Null | JsonValue::Bool(_) | JsonValue::Number(_) | JsonValue::String(_) => false,
    }
}

#[allow(clippy::unwrap_used, reason = "valid schema JSON builder")]
fn build_action_envelope(
    required_mask: [bool; 4],
    steps: &[(usize, ExprKind, bool)],
    decimal_shape: DecimalShape,
) -> (ActionRoot, Value) {
    let mut envelope: Value = serde_json::from_str(include_str!(
        "../../../fixtures/ir/retail-reorder.action.json"
    ))
    .unwrap();
    let payload = envelope.get("payload").cloned().unwrap();
    let mut action: ActionRoot = serde_json::from_value(payload).unwrap();
    action.preconditions.clear();
    action.writes.clear();
    action.effect_program = None;
    let required: Vec<&str> = FIELDS
        .iter()
        .zip(required_mask)
        .filter_map(|(&name, req)| req.then_some(name))
        .collect();
    action.input_schema = serde_json::from_value(json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "a": {"type": "string"},
            "b": {"type": "string"},
            "c": {"type": "string"},
            "d": {"type": "string"},
            "amount": decimal_shape.to_json()
        },
        "required": required
    }))
    .unwrap();
    action.derive = steps
        .iter()
        .map(|&(idx, kind, input_required)| {
            let field = FIELDS.get(idx).copied().unwrap_or("a");
            ActionInputDerivation {
                field: field.to_owned(),
                expression: ZenExpression(kind.render(field)),
                input_required,
            }
        })
        .collect();
    if let Some(obj) = envelope.as_object_mut() {
        obj.insert("payload".to_owned(), serde_json::to_value(&action).unwrap());
    }
    (action, envelope)
}

fn build_action(
    required_mask: [bool; 4],
    steps: &[(usize, ExprKind, bool)],
    decimal_shape: DecimalShape,
) -> ActionRoot {
    build_action_envelope(required_mask, steps, decimal_shape).0
}

fn schema_keys(schema: &JsonObject) -> (BTreeSet<String>, BTreeSet<String>) {
    let props = schema
        .get("properties")
        .and_then(JsonValue::as_object)
        .map_or_else(BTreeSet::new, |map| map.keys().cloned().collect());
    let required = schema
        .get("required")
        .and_then(JsonValue::as_array)
        .map_or_else(BTreeSet::new, |items| {
            items
                .iter()
                .filter_map(JsonValue::as_string)
                .map(str::to_owned)
                .collect()
        });
    (props, required)
}

fn check_decimal_wire_parity(
    action: &ActionRoot,
    base_valid: &RuntimeObject,
    decimal_shape: DecimalShape,
) -> Result<(), TestCaseError> {
    let mut with_string = base_valid.clone();
    with_string.insert(
        "amount".to_owned(),
        RuntimeValue::String("12.34".to_owned()),
    );
    let prepared_str = prepare_action_input_for_call::<()>(
        action,
        &with_string,
        "test.action",
        "0.1.0",
        "user:alice",
    );
    prop_assert!(
        prepared_str.is_ok(),
        "kernel rejected MCP wire decimal string: {prepared_str:?}"
    );

    let mut with_raw_number = base_valid.clone();
    let Ok(raw_decimal) = Decimal::parse_wire("12.34") else {
        return Err(TestCaseError::fail("constant decimal literal must parse"));
    };
    with_raw_number.insert("amount".to_owned(), RuntimeValue::Number(raw_decimal));
    let prepared_num = prepare_action_input_for_call::<()>(
        action,
        &with_raw_number,
        "test.action",
        "0.1.0",
        "user:alice",
    );
    prop_assert!(
        matches!(prepared_num, Err(ActionRefusal::InvalidInput { .. })),
        "kernel accepted raw JSON number for decimal input"
    );

    let mut with_null = base_valid.clone();
    with_null.insert("amount".to_owned(), RuntimeValue::Null);
    let prepared_null = prepare_action_input_for_call::<()>(
        action,
        &with_null,
        "test.action",
        "0.1.0",
        "user:alice",
    );
    if decimal_shape.allows_null() {
        prop_assert!(
            prepared_null.is_ok(),
            "kernel rejected null for nullable decimal schema: {prepared_null:?}"
        );
    } else {
        prop_assert!(
            matches!(prepared_null, Err(ActionRefusal::InvalidInput { .. })),
            "kernel accepted null for non-nullable scalar decimal schema"
        );
    }
    Ok(())
}

fn check_preparation_parity(
    action: &ActionRoot,
    steps: &[(usize, ExprKind, bool)],
    projected_required: &BTreeSet<String>,
    decimal_shape: DecimalShape,
) -> Result<(), TestCaseError> {
    // `$root` is unsupported by admitted evaluation and `$` is an undeclared
    // context identifier in standard ZEN; both exist here to test fail-closed
    // actor-derivation and non-root `$` parity in parts 2, 3, and 4.
    let has_root_expr = steps.iter().any(|(_, kind, _)| {
        matches!(
            kind,
            ExprKind::RootDefaultInput | ExprKind::DollarNonRootInput | ExprKind::RootActorSubject
        )
    });
    if has_root_expr {
        return Ok(());
    }
    let runtime_fails = steps.iter().any(|(_, kind, _)| kind.is_malformed())
        || has_runtime_null_trim(steps, projected_required);
    let mut valid = RuntimeObject::new();
    for req in projected_required {
        valid.insert(req.clone(), RuntimeValue::String(" ok ".to_owned()));
    }
    let prepared =
        prepare_action_input_for_call::<()>(action, &valid, "test.action", "0.1.0", "user:alice");
    if runtime_fails {
        let refused = matches!(prepared, Err(ActionRefusal::DerivationFailed { .. }));
        prop_assert!(
            refused,
            "kernel accepted malformed or null-trim derivation step: {prepared:?}"
        );
    } else {
        prop_assert!(
            prepared.is_ok(),
            "supplying MCP required keys failed kernel preparation: {prepared:?}"
        );
        check_decimal_wire_parity(action, &valid, decimal_shape)?;
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    #[test]
    fn mcp_projection_and_kernel_input_preparation_agree(
        required_mask in any::<[bool; 4]>(),
        steps in prop::collection::vec((0usize..4, expr_kind_strategy(), any::<bool>()), 0..6),
        decimal_shape in decimal_shape_strategy(),
    ) {
        let action = build_action(required_mask, &steps, decimal_shape);
        let actor_fields = actor_derived_fields(&action);
        let Ok(tool) = action_tool_with_server_filled(&action, &actor_fields) else {
            return Err(TestCaseError::fail("object action schema must project"));
        };
        let (projected_props, projected_required) = schema_keys(&tool.input_schema);

        // 1. No raw JSON number survives on any decimal schema node.
        prop_assert!(!schema_has_raw_decimal_number(&JsonValue::Object(
            tool.input_schema.clone()
        )));

        // 2. Standalone `action_tool` agrees with `action_tool_with_server_filled`
        //    on all well-formed ZEN expressions (malformed expressions rely on
        //    the kernel's fail-closed `actor_derived_fields` set).
        let has_malformed = steps.iter().any(|(_, kind, _)| kind.is_malformed());
        if !has_malformed {
            let Ok(standalone) = action_tool(&action) else {
                return Err(TestCaseError::fail("standalone projection must succeed"));
            };
            prop_assert_eq!(&standalone.input_schema, &tool.input_schema);
        }

        // 3. ACTION-ACTOR-1 parity: every kernel actor-derived field is absent
        //    from MCP properties and required, and supplying it is refused.
        for field in &actor_fields {
            prop_assert!(!projected_props.contains(field));
            prop_assert!(!projected_required.contains(field));
            let mut spoofed = RuntimeObject::new();
            spoofed.insert(field.clone(), RuntimeValue::String("spoof".to_owned()));
            let result = prepare_action_input_for_call::<()>(
                &action,
                &spoofed,
                "test.action",
                "0.1.0",
                "user:alice",
            );
            let actor_refused = matches!(
                result,
                Err(ActionRefusal::InvalidInput { field: ref refused })
                    if actor_fields.contains(refused)
            );
            prop_assert!(
                actor_refused,
                "kernel accepted spoofed actor-derived field `{field}`: {result:?}"
            );
        }

        // 4. Required-derivation parity: any non-actor field with an
        //    input_required=true derivation stays required in MCP, and omitting
        //    it is refused by the kernel.
        let required_non_actor: BTreeSet<String> = action
            .derive
            .iter()
            .filter(|step| step.input_required && !actor_fields.contains(&step.field))
            .map(|step| step.field.clone())
            .collect();
        for field in &required_non_actor {
            prop_assert!(projected_required.contains(field));
            let mut missing = RuntimeObject::new();
            for req in &projected_required {
                if req != field {
                    missing.insert(req.clone(), RuntimeValue::String(" ok ".to_owned()));
                }
            }
            let result = prepare_action_input_for_call::<()>(
                &action,
                &missing,
                "test.action",
                "0.1.0",
                "user:alice",
            );
            let missing_refused = matches!(result, Err(ActionRefusal::InvalidInput { .. }));
            prop_assert!(
                missing_refused,
                "kernel accepted missing required field `{field}`: {result:?}"
            );
        }

        // 5. Runtime parity + decimal wire parity: without contradictory
        //    actor-derived + input_required=true steps on the same field,
        //    supplying `projected_required` succeeds in plain evaluation
        //    unless a step is malformed or calls `trim(null)` at runtime.
        let has_contradiction = action
            .derive
            .iter()
            .any(|step| step.input_required && actor_fields.contains(&step.field));
        if !has_contradiction {
            check_preparation_parity(&action, &steps, &projected_required, decimal_shape)?;
        }
    }

    #[test]
    fn mcp_type_admitted_derivations_always_succeed_in_kernel(
        required_mask in any::<[bool; 4]>(),
        field_is_actor in any::<[bool; 4]>(),
        field_input_required in any::<[bool; 4]>(),
        raw_steps in prop::collection::vec((0usize..4, any::<bool>()), 0..6),
        decimal_shape in decimal_shape_strategy(),
    ) {
        let steps = build_type_admitted_steps(
            required_mask,
            field_is_actor,
            field_input_required,
            &raw_steps,
        );
        let (action, envelope) = build_action_envelope(required_mask, &steps, decimal_shape);
        let derive_findings: Vec<_> = norn_checker::check_artifact_json(&envelope)
            .into_iter()
            .filter(|diag| {
                diag.span
                    .as_ref()
                    .is_some_and(|span| span.pointer.starts_with("/payload/derive"))
            })
            .collect();
        prop_assert!(
            derive_findings.is_empty(),
            "type-admitted generator produced checker derivation findings: {derive_findings:?}"
        );
        let actor_fields = actor_derived_fields(&action);
        let Ok(tool) = action_tool_with_server_filled(&action, &actor_fields) else {
            return Err(TestCaseError::fail("object action schema must project"));
        };
        let Ok(standalone) = action_tool(&action) else {
            return Err(TestCaseError::fail("standalone projection must succeed"));
        };
        prop_assert_eq!(&standalone.input_schema, &tool.input_schema);
        let (_projected_props, projected_required) = schema_keys(&tool.input_schema);
        let mut valid = RuntimeObject::new();
        for req in &projected_required {
            valid.insert(req.clone(), RuntimeValue::String(" ok ".to_owned()));
        }
        let prepared = prepare_action_input_for_call::<()>(
            &action,
            &valid,
            "test.action",
            "0.1.0",
            "user:alice",
        );
        prop_assert!(
            prepared.is_ok(),
            "type-admitted MCP required keys failed kernel preparation: {prepared:?}"
        );
        check_decimal_wire_parity(&action, &valid, decimal_shape)?;
    }
}

fn has_derive_finding_at(envelope: &Value, pointer: &str) -> bool {
    norn_checker::check_artifact_json(envelope)
        .iter()
        .any(|diag| {
            diag.span
                .as_ref()
                .is_some_and(|span| span.pointer == pointer)
        })
}

#[test]
fn checker_refuses_invalid_coalesce_while_kernel_plain_eval_accepts_non_null_string() {
    // 1. Required schema field with input_required=true is non-optional String:
    //    static checker refuses `??`, while runtime plain evaluation accepts supplied string.
    let (required_coalesce, required_env) = build_action_envelope(
        [true, false, false, false],
        &[(0, ExprKind::DefaultInput, true)],
        DecimalShape::Scalar,
    );
    assert!(
        has_derive_finding_at(&required_env, "/payload/derive/0/expression"),
        "expected static checker refusal for `??` on non-optional required field"
    );
    let mut input = RuntimeObject::new();
    input.insert("a".to_owned(), RuntimeValue::String("ok".to_owned()));
    let res = prepare_action_input_for_call::<()>(
        &required_coalesce,
        &input,
        "test.action",
        "0.1.0",
        "user:alice",
    );
    assert!(
        res.is_ok(),
        "plain runtime evaluation should accept supplied string on `??`: {res:?}"
    );

    // 2. Earlier derivation restores non-optional String before step 1:
    //    static checker refuses step 1 `??`, while runtime plain evaluation succeeds.
    let (refilled_coalesce, refilled_env) = build_action_envelope(
        [false, false, false, false],
        &[
            (0, ExprKind::Constant, false),
            (0, ExprKind::DefaultInput, false),
        ],
        DecimalShape::Scalar,
    );
    assert!(
        has_derive_finding_at(&refilled_env, "/payload/derive/1/expression"),
        "expected static checker refusal for `??` after earlier derivation filled field"
    );
    let empty = RuntimeObject::new();
    let res = prepare_action_input_for_call::<()>(
        &refilled_coalesce,
        &empty,
        "test.action",
        "0.1.0",
        "user:alice",
    );
    assert!(
        res.is_ok(),
        "plain runtime evaluation should accept refilled string on `??`: {res:?}"
    );

    // 3. Optional derivation calling `trim(input.a)` is refused both statically and at runtime.
    let (optional_trim, optional_trim_env) = build_action_envelope(
        [false, false, false, false],
        &[(0, ExprKind::TrimInput, false)],
        DecimalShape::Scalar,
    );
    assert!(
        has_derive_finding_at(&optional_trim_env, "/payload/derive/0/expression"),
        "expected static checker refusal for `trim(...)` on optional field"
    );
    let res = prepare_action_input_for_call::<()>(
        &optional_trim,
        &empty,
        "test.action",
        "0.1.0",
        "user:alice",
    );
    assert!(
        matches!(res, Err(ActionRefusal::DerivationFailed { ref field, .. }) if field == "a"),
        "expected DerivationFailed for `trim(null)` at runtime: {res:?}"
    );
}

#[test]
fn closure_and_dollar_derivations_match_kernel_actor_classification() {
    let mut action = build_action([false, false, false, false], &[], DecimalShape::Scalar);
    action.derive = vec![
        ActionInputDerivation {
            field: "a".to_owned(),
            expression: ZenExpression("sum(map([1], #))".to_owned()),
            input_required: false,
        },
        ActionInputDerivation {
            field: "a".to_owned(),
            expression: ZenExpression("trim(input.a)".to_owned()),
            input_required: true,
        },
        ActionInputDerivation {
            field: "b".to_owned(),
            expression: ZenExpression("$.input.b ?? 'dollar-fallback'".to_owned()),
            input_required: false,
        },
        ActionInputDerivation {
            field: "b".to_owned(),
            expression: ZenExpression("trim(input.b)".to_owned()),
            input_required: true,
        },
        ActionInputDerivation {
            field: "c".to_owned(),
            expression: ZenExpression("len(map([1], #)) > 0 ? 'yes' : 'no'".to_owned()),
            input_required: false,
        },
    ];

    let actor_fields = actor_derived_fields(&action);
    assert_eq!(
        actor_fields,
        BTreeSet::from(["a".to_owned(), "c".to_owned()]),
        "closure expressions fail closed as actor-derived while bare `$` does not"
    );

    let standalone = action_tool(&action);
    let kernel_backed = action_tool_with_server_filled(&action, &actor_fields);
    assert!(
        standalone.is_ok(),
        "standalone projection must succeed: {standalone:?}"
    );
    assert!(
        kernel_backed.is_ok(),
        "kernel-backed projection must succeed: {kernel_backed:?}"
    );
    if let (Ok(standalone), Ok(kernel_backed)) = (standalone, kernel_backed) {
        assert_eq!(standalone.input_schema, kernel_backed.input_schema);

        let (props, required) = schema_keys(&standalone.input_schema);
        assert!(!props.contains("a"));
        assert!(!required.contains("a"));
        assert!(!props.contains("c"));
        assert!(!required.contains("c"));
        assert!(props.contains("b"));
        assert!(required.contains("b"));
    }

    let mut spoofed = RuntimeObject::new();
    spoofed.insert("a".to_owned(), RuntimeValue::String("spoof".to_owned()));
    let refused = prepare_action_input_for_call::<()>(
        &action,
        &spoofed,
        "test.action",
        "0.1.0",
        "user:alice",
    );
    assert!(
        matches!(refused, Err(ActionRefusal::InvalidInput { ref field }) if field == "a"),
        "kernel must refuse caller-supplied closure-derived field `a`: {refused:?}"
    );
}

#[test]
fn closure_only_derivation_omits_field_and_produces_expected_output() {
    let mut closure_only = build_action([false, false, false, false], &[], DecimalShape::Scalar);
    closure_only.derive = vec![ActionInputDerivation {
        field: "c".to_owned(),
        expression: ZenExpression("len(map([1], #)) > 0 ? 'yes' : 'no'".to_owned()),
        input_required: false,
    }];
    let closure_actor_fields = actor_derived_fields(&closure_only);
    assert_eq!(closure_actor_fields, BTreeSet::from(["c".to_owned()]));
    let standalone = action_tool(&closure_only);
    let kernel_backed = action_tool_with_server_filled(&closure_only, &closure_actor_fields);
    assert!(standalone.is_ok(), "{standalone:?}");
    assert!(kernel_backed.is_ok(), "{kernel_backed:?}");
    if let (Ok(standalone), Ok(kernel_backed)) = (standalone, kernel_backed) {
        assert_eq!(standalone.input_schema, kernel_backed.input_schema);
        let (props, required) = schema_keys(&standalone.input_schema);
        assert!(!props.contains("c"));
        assert!(!required.contains("c"));
    }
    let empty = RuntimeObject::new();
    let prepared = prepare_action_input_for_call::<()>(
        &closure_only,
        &empty,
        "test.action",
        "0.1.0",
        "user:alice",
    );
    assert!(
        prepared.is_ok(),
        "closure-only derivation must succeed when omitted: {prepared:?}"
    );
    if let Ok(prepared) = prepared {
        assert_eq!(
            prepared.effective_input().get("c"),
            Some(&RuntimeValue::String("yes".to_owned()))
        );
    }
}

#[test]
fn assignment_and_comparison_derivations_match_kernel_actor_classification() {
    let mut assign_vs_cmp = build_action([false, false, false, false], &[], DecimalShape::Scalar);
    assign_vs_cmp.derive = vec![
        ActionInputDerivation {
            field: "a".to_owned(),
            expression: ZenExpression("out.value = 'ok'; out.value".to_owned()),
            input_required: false,
        },
        ActionInputDerivation {
            field: "a".to_owned(),
            expression: ZenExpression("trim(input.a)".to_owned()),
            input_required: true,
        },
        ActionInputDerivation {
            field: "b".to_owned(),
            expression: ZenExpression("input.b == 'x' ? 'x' : 'y'".to_owned()),
            input_required: false,
        },
        ActionInputDerivation {
            field: "b".to_owned(),
            expression: ZenExpression("trim(input.b)".to_owned()),
            input_required: true,
        },
        ActionInputDerivation {
            field: "c".to_owned(),
            expression: ZenExpression("input.c != 'x' ? 'y' : 'x'".to_owned()),
            input_required: false,
        },
        ActionInputDerivation {
            field: "c".to_owned(),
            expression: ZenExpression("trim(input.c)".to_owned()),
            input_required: true,
        },
        ActionInputDerivation {
            field: "d".to_owned(),
            expression: ZenExpression(
                "len(input.d ?? '') <= 3 and len(input.d ?? '') >= 1 ? 'ok' : 'no'".to_owned(),
            ),
            input_required: false,
        },
        ActionInputDerivation {
            field: "d".to_owned(),
            expression: ZenExpression("trim(input.d)".to_owned()),
            input_required: true,
        },
    ];
    let assign_actor_fields = actor_derived_fields(&assign_vs_cmp);
    assert_eq!(assign_actor_fields, BTreeSet::from(["a".to_owned()]));
    let assign_standalone = action_tool(&assign_vs_cmp);
    let assign_kernel = action_tool_with_server_filled(&assign_vs_cmp, &assign_actor_fields);
    assert!(assign_standalone.is_ok(), "{assign_standalone:?}");
    assert!(assign_kernel.is_ok(), "{assign_kernel:?}");
    if let (Ok(standalone), Ok(kernel_backed)) = (assign_standalone, assign_kernel) {
        assert_eq!(standalone.input_schema, kernel_backed.input_schema);
        let (props, required) = schema_keys(&standalone.input_schema);
        assert!(!props.contains("a"));
        assert!(!required.contains("a"));
        assert_eq!(
            required,
            BTreeSet::from(["b".to_owned(), "c".to_owned(), "d".to_owned()])
        );
    }
}
