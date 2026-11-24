//! Exact schema checks at every helper invocation, including each fold step.
use std::collections::BTreeMap;

use norn_expr::schema::{Schema, fold_step_schemas};
use norn_ir::{
    JsonValue,
    jdm::{FoldOrderKind, OrderedFold},
};
use std::collections::BTreeSet;
use std::sync::Arc;
use zen_engine::{
    TzdbLookup, Variable,
    meter::{Meter, cost},
    nodes::DecisionContracts,
};

use crate::convert::from_variable;

#[derive(Debug, Default)]
pub(crate) struct StepContracts {
    pub(crate) assignment_fuel: Vec<norn_expr::graph::BoundAssignment>,
    pub(crate) schemas: BTreeMap<String, (Schema, Schema)>,
    pub(crate) expressions:
        BTreeMap<(String, String, String), zen_engine::nodes::ExpressionContract>,
    pub(crate) folds: BTreeMap<String, crate::fold::FoldPlan>,
}

impl StepContracts {
    pub(crate) fn bind_expressions_in(
        &mut self,
        root: &norn_ir::RuleRoot,
        tzdb: &norn_expr::temporal::TzdbBundle,
    ) -> Result<(), crate::AdmissionError> {
        let assignments = norn_expr::graph::bind_in(root, tzdb).map_err(|reason| {
            crate::AdmissionError::InvalidSchema {
                rule_id: root.rule_id.clone(),
                path: "nornType".into(),
                reason,
            }
        })?;
        self.assignment_fuel.extend(assignments.iter().cloned());
        for binding in assignments {
            let key = (binding.node, binding.key, binding.source);
            let contract = zen_engine::nodes::ExpressionContract {
                source: binding.runtime_source,
                exact: binding.exact,
            };
            if self.expressions.insert(key, contract).is_some() {
                return Err(crate::AdmissionError::InvalidSchema {
                    rule_id: root.rule_id.clone(),
                    path: "graph".into(),
                    reason: "ambiguous exact expression binding in closure".into(),
                });
            }
        }
        Ok(())
    }

    pub(crate) fn insert_fold(
        &mut self,
        key: &str,
        helper: &norn_ir::RuleRoot,
        fold: &norn_ir::jdm::OrderedFold,
    ) -> Result<(), crate::AdmissionError> {
        let (expected, accumulator) =
            validate_fold(fold).map_err(|error| crate::AdmissionError::InvalidSchema {
                rule_id: helper.rule_id.clone(),
                path: "orderedFold".into(),
                reason: error.to_string(),
            })?;
        let (input, output) = self.insert_helper(key, helper)?;
        if input != expected || output != accumulator {
            return Err(crate::AdmissionError::InvalidSchema {
                rule_id: helper.rule_id.clone(),
                path: "orderedFold".into(),
                reason: "helper schemas must equal {accumulator, item, index} -> accumulator"
                    .into(),
            });
        }
        Ok(())
    }

    /// Registers `helper`'s declared closed input and output records under
    /// its call `key`, so every invocation of an ordinary or fold helper is
    /// checked against them at runtime ([`DecisionContracts::check`], #1708).
    pub(crate) fn insert_helper(
        &mut self,
        key: &str,
        helper: &norn_ir::RuleRoot,
    ) -> Result<(Schema, Schema), crate::AdmissionError> {
        let input = crate::contract::closed_record(helper, "inputSchema")?;
        let output = crate::contract::closed_record(helper, "outputSchema")?;
        self.schemas
            .insert(key.to_owned(), (input.clone(), output.clone()));
        Ok((input, output))
    }

    /// The exact check of a fold step (`{accumulator, item, index}`) or its
    /// result against `key`'s declared records: no undeclared member passes.
    pub(crate) fn check_exact(
        &self,
        key: &str,
        value: &Variable,
        input: bool,
        meter: Option<&Meter>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.check_declared(key, value, input, meter, false)
    }

    fn check_declared(
        &self,
        key: &str,
        value: &Variable,
        input: bool,
        meter: Option<&Meter>,
        project: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let (before, after) = self
            .schemas
            .get(key)
            .ok_or_else(|| format!("sub-decision `{key}` has no admitted schema contract"))?;
        let schema = if input { before } else { after };
        if let Some(meter) = meter {
            meter.charge(cost::deep_size_capped(value, meter.remaining(), &mut 0))?;
        }
        let mut value = from_variable(value)?;
        if project
            && let (norn_expr::Value::Object(members), Some(declared)) =
                (&mut value, schema.properties())
        {
            members.retain(|name, _| declared.contains_key(name));
        }
        schema
            .check(&value, if input { "step.input" } else { "step.output" })
            .map_err(|error| format!("{}: {}", error.path, error.reason).into())
    }
}

impl DecisionContracts for StepContracts {
    fn expression(
        &self,
        node: &str,
        key: &str,
        source: &str,
    ) -> Option<zen_engine::nodes::ExpressionContract> {
        self.expressions
            .get(&(node.to_owned(), key.to_owned(), source.to_owned()))
            .cloned()
    }

    fn derive(
        &self,
        key: &str,
        input: &Variable,
        meter: Option<&Meter>,
    ) -> Option<Result<Variable, Box<dyn std::error::Error>>> {
        self.derive_with_tzdb(key, input, meter, None)
    }

    fn derive_with_tzdb(
        &self,
        key: &str,
        input: &Variable,
        meter: Option<&Meter>,
        tzdb: Option<&Arc<dyn TzdbLookup>>,
    ) -> Option<Result<Variable, Box<dyn std::error::Error>>> {
        self.folds.get(key).map(|plan| {
            meter
                .ok_or_else(|| "fold requires shared fuel".into())
                .and_then(|meter| plan.run(input, meter, tzdb, self))
        })
    }

    /// An ordinary invocation: zen hands the helper its caller's whole merged
    /// context, so the input is checked on the helper's declared members only
    /// (each present and well typed, nested records exact); the output is
    /// checked exactly. A key without an admitted contract is refused.
    fn check(
        &self,
        key: &str,
        value: &Variable,
        input: bool,
        meter: Option<&Meter>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.check_declared(key, value, input, meter, input)
    }
}

/// Validates a fold's profile, bounds and order keys, returning its derived
/// `({accumulator, item, index}, accumulator)` step schemas.
pub(crate) fn validate_fold(
    f: &OrderedFold,
) -> Result<(Schema, Schema), Box<dyn std::error::Error>> {
    let step = fold_step_schemas(f).map_err(|e| e.reason)?;
    if f.semantics_version != "1.0"
        || f.arithmetic.scale > 18
        || !(1..=4096).contains(&f.max_items)
        || !(1..=1_048_576).contains(&f.max_accumulator_bytes)
    {
        return Err("invalid fold profile or bounds".into());
    }
    let item =
        Schema::parse(&JsonValue::Object(f.item_schema.clone()), "item").map_err(|e| e.reason)?;
    let mut keys = BTreeSet::new();
    for key in &f.order {
        let typed = matches!(
            (
                key.kind,
                item.properties().and_then(|p| p.get(&key.property))
            ),
            (FoldOrderKind::String, Some(Schema::String(_)))
                | (FoldOrderKind::Integer, Some(Schema::Integer))
        );
        if !typed
            || !item.required().is_some_and(|r| r.contains(&key.property))
            || !keys.insert(&key.property)
        {
            return Err("invalid fold order key".into());
        }
    }
    if f.identity.trim().is_empty()
        || !f
            .order
            .last()
            .is_some_and(|k| k.property == f.identity && k.kind == FoldOrderKind::String)
    {
        return Err("fold order requires a final string identity".into());
    }
    Ok(step)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convert::to_variable;
    use norn_expr::Value;

    type Checked = Result<(), Box<dyn std::error::Error>>;

    fn contracts() -> StepContracts {
        let record = Schema::Object {
            properties: BTreeMap::from([("n".into(), Schema::Integer)]),
            required: ["n".into()].into(),
        };
        StepContracts {
            schemas: BTreeMap::from([("step".into(), (record.clone(), record))]),
            folds: BTreeMap::new(),
            assignment_fuel: Vec::new(),
            expressions: BTreeMap::new(),
        }
    }

    #[test]
    fn every_fold_step_checks_closed_input_and_complete_output_under_shared_fuel() -> Checked {
        let contracts = contracts();
        let meter = Meter::new(100);
        let valid = to_variable(&Value::object([("n", Value::from(1))]))?;
        for input in [true, false] {
            contracts.check_exact("step", &valid, input, Some(&meter))?;
            for bad in [
                Value::object([("n", Value::from("1"))]),
                Value::object([("n", Value::from(1)), ("extra", Value::Null)]),
                Value::object::<&str>([]),
            ] {
                let bad = to_variable(&bad)?;
                assert!(
                    contracts
                        .check_exact("step", &bad, input, Some(&meter))
                        .is_err()
                );
            }
        }
        assert!(meter.used() > 0);
        assert!(
            contracts
                .check_exact("step", &valid, true, Some(&Meter::new(0)))
                .is_err()
        );
        Ok(())
    }

    /// #1708: an ordinary invocation checks the helper's declared members of
    /// the caller's context and its exact output; an unknown key is refused.
    #[test]
    fn every_ordinary_invocation_checks_declared_input_and_exact_output() -> Checked {
        let contracts = contracts();
        let meter = Meter::new(1_000);
        let context = Value::object([("n", Value::from(1)), ("caller", Value::Null)]);
        contracts.check("step", &to_variable(&context)?, true, Some(&meter))?;
        let extra_output = to_variable(&context)?;
        assert!(
            contracts
                .check("step", &extra_output, false, Some(&meter))
                .is_err()
        );
        for bad in [
            Value::object([("n", Value::from("1")), ("caller", Value::Null)]),
            Value::object([("caller", Value::Null)]),
        ] {
            let bad = to_variable(&bad)?;
            for input in [true, false] {
                assert!(contracts.check("step", &bad, input, Some(&meter)).is_err());
            }
        }
        let valid = to_variable(&Value::object([("n", Value::from(1))]))?;
        assert!(
            contracts
                .check("other", &valid, true, Some(&meter))
                .is_err()
        );
        Ok(())
    }
}
