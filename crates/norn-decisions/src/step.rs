//! Exact schema checks at every helper invocation, including each fold step.
use std::collections::BTreeMap;

use norn_expr::schema::Schema;
use zen_engine::{
    Variable,
    meter::{Meter, cost},
    nodes::DecisionContracts,
};

use crate::convert::from_variable;

#[derive(Debug, Default)]
pub(crate) struct StepContracts(BTreeMap<String, (Schema, Schema)>);

impl StepContracts {
    pub(crate) fn insert_fold(
        &mut self,
        key: &str,
        helper: &norn_ir::RuleRoot,
        fold: &norn_ir::jdm::OrderedFold,
    ) -> Result<(), crate::AdmissionError> {
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
        self.0.insert(key.to_owned(), (input, output));
        Ok(())
    }
}

impl DecisionContracts for StepContracts {
    fn check(
        &self,
        key: &str,
        value: &Variable,
        input: bool,
        meter: Option<&Meter>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let Some((before, after)) = self.0.get(key) else {
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
        let contracts = StepContracts(BTreeMap::from([("step".into(), (record.clone(), record))]));
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
