//! Exact schema checks at every helper invocation, including each fold step.
use std::collections::BTreeMap;

use norn_expr::schema::{Schema, fold_step_schemas};
use norn_ir::{
    JsonValue,
    jdm::{FoldOrderKind, OrderedFold},
};
use std::collections::BTreeSet;
use zen_engine::{
    Variable,
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
    pub(crate) fn bind_expressions(
        &mut self,
        root: &norn_ir::RuleRoot,
    ) -> Result<(), crate::AdmissionError> {
        let assignments = norn_expr::graph::bind(root).map_err(|reason| {
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
        validate_fold(fold).map_err(|error| crate::AdmissionError::InvalidSchema {
            rule_id: helper.rule_id.clone(),
            path: "orderedFold".into(),
            reason: error.to_string(),
        })?;
        let parse = |document, path: &str| {
            Schema::parse(&norn_ir::JsonValue::Object(document), path).map_err(|error| {
                crate::AdmissionError::InvalidSchema {
                    rule_id: helper.rule_id.clone(),
                    path: error.path,
                    reason: error.reason,
                }
            })
        };
        let (expected, accumulator) =
            norn_expr::schema::fold_step_schemas(fold).map_err(|error| {
                crate::AdmissionError::InvalidSchema {
                    rule_id: helper.rule_id.clone(),
                    path: error.path,
                    reason: error.reason,
                }
            })?;
        let input = parse(helper.input_schema.clone(), "step.input")?;
        let output = parse(helper.output_schema.clone(), "step.output")?;
        if input != expected || output != accumulator {
            return Err(crate::AdmissionError::InvalidSchema {
                rule_id: helper.rule_id.clone(),
                path: "orderedFold".into(),
                reason: "helper schemas must equal {accumulator, item, index} -> accumulator"
                    .into(),
            });
        }
        self.schemas.insert(key.to_owned(), (input, output));
        Ok(())
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
        self.folds.get(key).map(|plan| {
            meter
                .ok_or_else(|| "fold requires shared fuel".into())
                .and_then(|meter| plan.run(input, meter, self))
        })
    }

    fn check(
        &self,
        key: &str,
        value: &Variable,
        input: bool,
        meter: Option<&Meter>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Some((before, after)) = self.schemas.get(key) else {
            return Ok(());
        };
        let schema = if input { before } else { after };
        if let Some(meter) = meter {
            meter.charge(cost::deep_size_capped(value, meter.remaining(), &mut 0))?;
        }
        let value = from_variable(value)?;
        schema
            .check(&value, if input { "step.input" } else { "step.output" })
            .map_err(|error| format!("{}: {}", error.path, error.reason).into())
    }
}

pub(crate) fn validate_fold(f: &OrderedFold) -> Result<(), Box<dyn std::error::Error>> {
    fold_step_schemas(f).map_err(|e| e.reason)?;
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
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convert::to_variable;
    use norn_expr::Value;

    #[test]
    fn every_invocation_checks_closed_input_and_complete_output_under_shared_fuel() {
        let record = Schema::Object {
            properties: BTreeMap::from([("n".into(), Schema::Integer)]),
            required: ["n".into()].into(),
        };
        let contracts = StepContracts {
            schemas: BTreeMap::from([("step".into(), (record.clone(), record))]),
            folds: BTreeMap::new(),
            assignment_fuel: Vec::new(),
            expressions: BTreeMap::new(),
        };
        let meter = Meter::new(100);
        let valid = to_variable(&Value::object([("n", Value::from(1))])).unwrap();
        for input in [true, false] {
            contracts
                .check("step", &valid, input, Some(&meter))
                .unwrap();
            for bad in [
                Value::object([("n", Value::from("1"))]),
                Value::object([("n", Value::from(1)), ("extra", Value::Null)]),
                Value::object::<&str>([]),
            ] {
                assert!(
                    contracts
                        .check("step", &to_variable(&bad).unwrap(), input, Some(&meter))
                        .is_err()
                );
            }
        }
        assert!(meter.used() > 0);
        assert!(
            contracts
                .check("step", &valid, true, Some(&Meter::new(0)))
                .is_err()
        );
    }
}
