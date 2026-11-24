//! Record-level ZEN guard expression admission and budgeted evaluation for
//! declarative definitions such as automations (`AUTOMATION-1`).
//!
//! Uses `norn-expr` directly so every guard is checked under the exact same
//! `ZenTypeEnv` and `norn-zen` profile as laws and actions, and evaluated
//! deterministically under an explicit fuel budget.

use std::collections::{BTreeMap, BTreeSet};

use norn_expr::{
    ACTOR_VARIABLE, EntityDefinitions, EnvOptions, EvaluationBudget, Expression, FieldSource,
    SELF_VARIABLE, Type, Value, ZenTypeEnv, check_expression, expression_reads,
};
use norn_ir::{InterfaceDef, ObjectTypeDef, ObjectTypeName, PropertyTypeDef, RuntimeValue};

/// Fuel budget for evaluating one record guard expression.
pub const GUARD_EVALUATION_BUDGET: u64 = 10_000;

/// Check that `source` is a well-typed boolean ZEN expression over `self`'s
/// **stored properties** of `object_type` under the declared entity model.
///
/// The transactional automation outbox snapshots stored property values only
/// (`ObjectRecord::values`), never computed properties, forward/inverse links,
/// or a trigger `actor` principal. Admission therefore binds `self` with
/// [`ZenTypeEnv::bind_self_to_stored_fields`] and refuses computed properties,
/// links, and `actor` references with clear findings.
///
/// # Errors
/// Returns human-readable diagnostic messages when the environment cannot be
/// derived, `source` fails to parse or compile, references computed fields,
/// links, or `actor`, or fails type/profile checking.
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

    let mut computed_fields = BTreeSet::new();
    let mut link_fields = BTreeSet::new();
    if let Some(record) = base.record(object_type) {
        for (field_name, field) in &record.fields {
            if matches!(field.source, FieldSource::Property { read_only: true }) {
                computed_fields.insert(field_name.clone());
            } else if matches!(
                field.source,
                FieldSource::Link { .. } | FieldSource::Inverse { .. }
            ) {
                link_fields.insert(field_name.clone());
            }
        }
    }

    let env = base
        .bind_self_to_stored_fields(&ObjectTypeName(object_type.to_owned()), &BTreeSet::new())
        .map_err(|error| vec![error.to_string()])?;
    let expression = Expression::compile(source).map_err(|error| vec![error.to_string()])?;

    let mut shape_errors = Vec::new();
    if let Ok(actor_reads) = expression_reads(source, ACTOR_VARIABLE)
        && (actor_reads.whole_root || !actor_reads.fields.is_empty())
    {
        shape_errors.push(
            "guard expression cannot reference `actor`; only stored properties on `self` are available"
                .to_owned(),
        );
    }
    if let Ok(self_reads) = expression_reads(source, SELF_VARIABLE) {
        for field in &self_reads.fields {
            if computed_fields.contains(field) {
                shape_errors.push(format!(
                    "guard expression cannot read computed property `{field}` on `{object_type}`; only stored properties are available"
                ));
            } else if link_fields.contains(field) {
                shape_errors.push(format!(
                    "guard expression cannot traverse link `{field}` on `{object_type}`; only stored properties are available"
                ));
            }
        }
    }
    if !shape_errors.is_empty() {
        return Err(shape_errors);
    }

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
        values.insert(
            "score".to_owned(),
            RuntimeValue::Number(Decimal::from(42_i64)),
        );
        assert_eq!(
            evaluate_record_guard("self.status == 'stalled' and self.score < 60", &values),
            Ok(true)
        );
        values.insert(
            "score".to_owned(),
            RuntimeValue::Number(Decimal::from(80_i64)),
        );
        assert_eq!(
            evaluate_record_guard("self.status == 'stalled' and self.score < 60", &values),
            Ok(false)
        );
    }

    #[test]
    fn refuses_computed_properties_links_and_actor_in_record_guards() {
        let owner: ObjectTypeDef = serde_json::from_value(serde_json::json!({
            "name": "demo.Owner",
            "properties": [
                { "id": "name", "type": { "kind": "text" } }
            ]
        }))
        .unwrap();
        let item: ObjectTypeDef = serde_json::from_value(serde_json::json!({
            "name": "demo.RichItem",
            "properties": [
                { "id": "score", "type": { "kind": "integer" } },
                {
                    "id": "doubled",
                    "type": { "kind": "integer" },
                    "computed": "self.score * 2"
                }
            ],
            "links": [
                {
                    "id": "owner",
                    "target": "demo.Owner",
                    "cardinality": { "to": "one", "from": "many", "required": true },
                    "inverse": "richItems"
                }
            ]
        }))
        .unwrap();
        let types = [owner, item];

        // Stored property passes.
        assert!(check_record_guard(&types, &[], &[], "demo.RichItem", "self.score > 10").is_ok());

        // Computed property refuses with a clear finding.
        let computed_err =
            check_record_guard(&types, &[], &[], "demo.RichItem", "self.doubled > 20").unwrap_err();
        assert!(
            computed_err
                .iter()
                .any(|msg| msg.contains("computed property") && msg.contains("doubled")),
            "{computed_err:?}"
        );

        // Forward link refuses with a clear finding.
        let link_err = check_record_guard(
            &types,
            &[],
            &[],
            "demo.RichItem",
            "self.owner.name == 'alice'",
        )
        .unwrap_err();
        assert!(
            link_err
                .iter()
                .any(|msg| msg.contains("link") && msg.contains("owner")),
            "{link_err:?}"
        );

        // Inverse link refuses with a clear finding.
        let inverse_err =
            check_record_guard(&types, &[], &[], "demo.Owner", "len(self.richItems) > 0")
                .unwrap_err();
        assert!(
            inverse_err
                .iter()
                .any(|msg| msg.contains("link") && msg.contains("richItems")),
            "{inverse_err:?}"
        );

        // Ambient `actor` variable refuses with a clear finding.
        let actor_err = check_record_guard(
            &types,
            &[],
            &[],
            "demo.RichItem",
            "actor.subjectId == 'alice'",
        )
        .unwrap_err();
        assert!(
            actor_err.iter().any(|msg| msg.contains("actor")),
            "{actor_err:?}"
        );
    }
}
