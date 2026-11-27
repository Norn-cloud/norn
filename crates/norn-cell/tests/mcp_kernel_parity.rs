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
            Self::RootDefaultInput => format!("$.input.{field} ?? 'root-fallback'"),
            Self::ActorSubject => "actor.subjectId".to_owned(),
            Self::RootActorSubject => "$.actor.subjectId".to_owned(),
            Self::InputOrActor => format!("input.{field} ?? actor.subjectId"),
            Self::Constant => "'fixed'".to_owned(),
            Self::Malformed => "1 +".to_owned(),
        }
    }

    const fn is_malformed(self) -> bool {
        matches!(self, Self::Malformed)
    }

    const fn is_rejected_at_eval(self) -> bool {
        matches!(
            self,
            Self::Malformed | Self::RootDefaultInput | Self::RootActorSubject
        )
    }
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

#[allow(clippy::unwrap_used, reason = "constant valid test fixture")]
fn base_action() -> ActionRoot {
    let envelope: Value = serde_json::from_str(include_str!(
        "../../../fixtures/ir/retail-reorder.action.json"
    ))
    .unwrap();
    let payload = envelope.get("payload").cloned().unwrap();
    let mut action: ActionRoot = serde_json::from_value(payload).unwrap();
    action.preconditions.clear();
    action.writes.clear();
    action.effect_program = None;
    action
}

fn expr_kind_strategy() -> impl Strategy<Value = ExprKind> {
    prop_oneof![
        Just(ExprKind::TrimInput),
        Just(ExprKind::DefaultInput),
        Just(ExprKind::RootDefaultInput),
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
fn build_action(
    required_mask: [bool; 4],
    steps: &[(usize, ExprKind, bool)],
    decimal_shape: DecimalShape,
) -> ActionRoot {
    const FIELDS: [&str; 4] = ["a", "b", "c", "d"];
    let mut action = base_action();
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
    action
}

fn schema_keys(schema: &JsonObject) -> (BTreeSet<String>, BTreeSet<String>) {
    let props = schema
        .get("properties")
        .and_then(JsonValue::as_object)
        .map(|map| map.keys().cloned().collect())
        .unwrap_or_default();
    let required = schema
        .get("required")
        .and_then(JsonValue::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(JsonValue::as_string)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
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

        // 5. Positive parity + decimal wire parity: on evaluable ZEN without
        //    contradictory actor-derived + input_required=true steps on the
        //    same field, supplying `projected_required` succeeds and `amount`
        //    accepts wire strings (and null iff the schema is nullable) while
        //    refusing raw JSON numbers.
        let has_rejected_eval = steps
            .iter()
            .any(|(_, kind, _)| kind.is_rejected_at_eval());
        let has_contradiction = action
            .derive
            .iter()
            .any(|step| step.input_required && actor_fields.contains(&step.field));
        let has_unfilled_optional_trim = steps
            .iter()
            .any(|(_, kind, req)| !req && matches!(kind, ExprKind::TrimInput));
        if !has_rejected_eval && !has_contradiction && !has_unfilled_optional_trim {
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
                "supplying MCP required keys failed kernel preparation: {prepared:?}"
            );
            check_decimal_wire_parity(&action, &valid, decimal_shape)?;
        }
    }
}
