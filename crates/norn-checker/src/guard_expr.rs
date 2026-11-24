//! Record-level ZEN guard expression admission and budgeted evaluation for
//! declarative definitions such as automations (`AUTOMATION-1`).
//!
//! Uses `norn-expr` directly so every guard is checked under the exact same
//! `ZenTypeEnv` and `norn-zen` profile as laws and actions, and evaluated
//! deterministically under an explicit fuel budget.

use std::collections::{BTreeMap, BTreeSet};

use norn_expr::{
    EntityDefinitions, EnvOptions, EvaluationBudget, Expression, Type, Value, ZenTypeEnv,
    check_expression,
};
use norn_ir::{InterfaceDef, ObjectTypeDef, ObjectTypeName, PropertyTypeDef, RuntimeValue};

/// Fuel budget for evaluating one record guard expression.
pub const GUARD_EVALUATION_BUDGET: u64 = 10_000;

/// Check that `source` is a well-typed boolean ZEN expression over `self` of
/// `object_type` under the declared entity model.
///
/// # Errors
/// Returns human-readable diagnostic messages when the environment cannot be
/// derived, `source` fails to parse or compile, or type/profile checking fails.
pub fn check_record_guard(
    object_types: &[ObjectTypeDef],
    interfaces: &[InterfaceDef],
    property_types: &[PropertyTypeDef],
    object_type: &str,
    source: &str,
) -> Result<(), Vec<String>> {
    let definitions = EntityDefinitions {
        property_types,
        interfaces,
        object_types,
        extensions: &[],
    };
    let base = ZenTypeEnv::from_artifact_set_for_checking(
        &definitions,
        &BTreeSet::new(),
        EnvOptions::default(),
    )
    .map_err(|errors| {
        errors
            .into_iter()
            .map(|error| error.to_string())
            .collect::<Vec<_>>()
    })?;
    let env = base
        .bind_self(&ObjectTypeName(object_type.to_owned()))
        .map_err(|error| vec![error.to_string()])?;
    let expression = Expression::compile(source).map_err(|error| vec![error.to_string()])?;
    check_expression(&expression, &env, &Type::Bool).map_err(|diagnostics| {
        diagnostics
            .into_iter()
            .map(|diagnostic| diagnostic.message)
            .collect::<Vec<_>>()
    })?;
    Ok(())
}

/// Evaluate an admitted boolean ZEN expression `source` against one record's
/// property values (`self`).
///
/// # Errors
/// Returns an error string when compilation fails, values cannot be converted,
/// the fuel budget is exhausted, evaluation fails, or the result is not boolean.
pub fn evaluate_record_guard(
    source: &str,
    values: &BTreeMap<String, RuntimeValue>,
) -> Result<bool, String> {
    let expression = Expression::compile(source).map_err(|error| error.to_string())?;
    let mut self_fields = BTreeMap::new();
    for (key, value) in values {
        let converted = Value::try_from(value).map_err(|error| error.to_string())?;
        self_fields.insert(key.clone(), converted);
    }
    let context = Value::object([("self", Value::Object(self_fields))]);
    let evaluation = expression
        .evaluate(&context, EvaluationBudget::new(GUARD_EVALUATION_BUDGET))
        .map_err(|error| error.to_string())?;
    match evaluation.value() {
        Value::Bool(flag) => Ok(*flag),
        other @ (Value::Null
        | Value::Number(_)
        | Value::String(_)
        | Value::Array(_)
        | Value::Object(_)) => Err(format!(
            "guard expression returned {other:?}, expected bool"
        )),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "unit tests")]
mod tests {
    use super::*;
    use norn_ir::{Decimal, RuntimeValue};

    fn sample_type() -> ObjectTypeDef {
        serde_json::from_value(serde_json::json!({
            "name": "demo.Item",
            "properties": [
                {
                    "id": "status",
                    "type": {
                        "kind": "enum",
                        "values": [{ "value": "open" }, { "value": "stalled" }]
                    }
                },
                { "id": "score", "type": { "kind": "integer" } }
            ]
        }))
        .unwrap()
    }

    #[test]
    fn admits_valid_boolean_guard_and_evaluates_exact_values() {
        let object_types = [sample_type()];
        assert!(
            check_record_guard(
                &object_types,
                &[],
                &[],
                "demo.Item",
                "self.status == 'stalled' and self.score < 60",
            )
            .is_ok()
        );
        assert!(
            check_record_guard(&object_types, &[], &[], "demo.Item", "self.score + 1").is_err()
        );
        assert!(
            check_record_guard(&object_types, &[], &[], "demo.Item", "self.unknown == true")
                .is_err()
        );

        let mut values = BTreeMap::new();
        values.insert("status".to_owned(), RuntimeValue::String("stalled".into()));
        values.insert("score".to_owned(), RuntimeValue::Number(Decimal::from(42)));
        assert_eq!(
            evaluate_record_guard("self.status == 'stalled' and self.score < 60", &values),
            Ok(true)
        );
        values.insert("score".to_owned(), RuntimeValue::Number(Decimal::from(80)));
        assert_eq!(
            evaluate_record_guard("self.status == 'stalled' and self.score < 60", &values),
            Ok(false)
        );
    }
}
