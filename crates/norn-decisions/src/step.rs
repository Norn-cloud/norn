//! Exact schema checks at every helper invocation, including each fold step.
use std::collections::BTreeMap;

use norn_expr::schema::{Schema, fold_step_schemas};
use norn_ir::{
    JsonValue,
    jdm::{FoldOrderKind, OrderedFold},
};
use std::collections::BTreeSet;
use std::sync::Arc;
use zen_engine::{TzdbLookup, Variable, meter::Meter, nodes::DecisionContracts};

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
        let (input, output) = self.insert_with_paths(key, helper, "step.input", "step.output")?;
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
        self.insert_with_paths(key, helper, "inputSchema", "outputSchema")
    }

    fn insert_with_paths(
        &mut self,
        key: &str,
        helper: &norn_ir::RuleRoot,
        input_path: &str,
        output_path: &str,
    ) -> Result<(Schema, Schema), crate::AdmissionError> {
        let input = crate::contract::closed_record(helper, "inputSchema", input_path)?;
        let output = crate::contract::closed_record(helper, "outputSchema", output_path)?;
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
            .map(drop)
    }

    fn check_declared(
        &self,
        key: &str,
        value: &Variable,
        input: bool,
        meter: Option<&Meter>,
        project: bool,
    ) -> Result<norn_expr::Value, Box<dyn std::error::Error>> {
        let (before, after) = self
            .schemas
            .get(key)
            .ok_or_else(|| format!("sub-decision `{key}` has no admitted schema contract"))?;
        let schema = if input { before } else { after };
        if let Some(meter) = meter {
            meter.charge(boundary_units(value, meter.remaining()))?;
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
            .map_err(|error| format!("{}: {}", error.path, error.reason))?;
        Ok(value)
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

    fn check(
        &self,
        key: &str,
        value: &Variable,
        input: bool,
        meter: Option<&Meter>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        self.cross(key, value, input, meter).map(drop)
    }

    /// An ordinary invocation: zen hands the helper its caller's whole merged
    /// context, so the input is checked on the helper's declared members only
    /// (each present and well typed, nested records exact); the output is
    /// checked exactly. What crosses the boundary is the checked value itself:
    /// the declared members only, with temporal values in the canonical string
    /// form the schema checked, so a helper never reads an undeclared caller
    /// member and a caller never receives an unchecked representation (#1708).
    /// A key without an admitted contract is refused.
    fn cross(
        &self,
        key: &str,
        value: &Variable,
        input: bool,
        meter: Option<&Meter>,
    ) -> Result<Option<Variable>, Box<dyn std::error::Error>> {
        let checked = self.check_declared(key, value, input, meter, input)?;
        Ok(Some(crate::convert::to_variable_exact(&checked)?))
    }
}

/// Fuel for carrying `value` across a helper boundary, charged before it is
/// converted: one unit for every visited value (an empty string included),
/// plus the UTF-8 bytes of every string and every object key. A value's own
/// unit is paid when it is queued, so the walk stops (at `cap + 1`, which
/// exhausts the meter like any larger charge) before it queues or scans
/// anything past `cap`.
fn boundary_units(value: &Variable, cap: u64) -> u64 {
    boundary_walk(value, cap, &mut 0)
}

/// [`boundary_units`], counting in `queued` every value it queued.
fn boundary_walk(value: &Variable, cap: u64, queued: &mut u64) -> u64 {
    let over = cap.saturating_add(1);
    let bytes = |len: usize| u64::try_from(len).unwrap_or(u64::MAX);
    let mut units: u64 = 1;
    let mut pending = vec![value.clone()];
    *queued = 1;
    // Pays a child's unit and key bytes, then queues it; `false` once past `cap`.
    let mut enqueue =
        |units: &mut u64, key: usize, child: &Variable, pending: &mut Vec<Variable>| {
            *units = units.saturating_add(1).saturating_add(bytes(key));
            if *units > cap {
                return false;
            }
            *queued = queued.saturating_add(1);
            pending.push(child.clone());
            true
        };
    while let Some(current) = pending.pop() {
        if units > cap {
            return over;
        }
        match &current {
            Variable::String(text) => units = units.saturating_add(bytes(text.len())),
            Variable::Array(items) => {
                for item in items.borrow().iter() {
                    if !enqueue(&mut units, 0, item, &mut pending) {
                        return over;
                    }
                }
            }
            Variable::Object(members) => {
                for (key, item) in members.borrow().iter() {
                    if !enqueue(&mut units, key.len(), item, &mut pending) {
                        return over;
                    }
                }
            }
            Variable::Null | Variable::Bool(_) | Variable::Number(_) | Variable::Dynamic(_) => {}
        }
    }
    units.min(over)
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

    /// #1708: what crosses is exactly what was checked: a decomposed key and
    /// its composed spelling stay two distinct members, spelled as given.
    #[test]
    fn crossing_keeps_every_checked_key_and_string_spelled_as_given() -> Checked {
        let (composed, decomposed) = ("\u{e9}", "e\u{301}");
        let record = Schema::Object {
            properties: BTreeMap::from([
                (composed.to_owned(), Schema::String(None)),
                (decomposed.to_owned(), Schema::String(None)),
            ]),
            required: [composed.to_owned(), decomposed.to_owned()].into(),
        };
        let contracts = StepContracts {
            schemas: BTreeMap::from([("step".into(), (record.clone(), record))]),
            ..StepContracts::default()
        };
        let value = Value::object([
            (composed, Value::from(decomposed)),
            (decomposed, Value::from(composed)),
        ]);
        let variable = crate::convert::to_variable_exact(&value)?;
        for input in [true, false] {
            let crossed = contracts
                .cross("step", &variable, input, Some(&Meter::new(1_000)))?
                .ok_or("a checked value crosses")?;
            assert_eq!(from_variable(&crossed)?, value);
        }
        Ok(())
    }

    /// #1708: the boundary charge counts every visited value (an empty
    /// string included) and every object key's bytes.
    #[test]
    fn boundary_fuel_counts_key_bytes_and_empty_strings() -> Checked {
        let units = |value: &Value| -> Result<u64, Box<dyn std::error::Error>> {
            Ok(boundary_units(&to_variable(value)?, u64::MAX))
        };
        let short = units(&Value::object([("a", Value::from(""))]))?;
        let long = units(&Value::object([("abcdef", Value::from(""))]))?;
        assert_eq!(long, short.saturating_add(5));
        let none = units(&Value::Array(Vec::new()))?;
        let one = units(&Value::Array(vec![Value::from("")]))?;
        let two = units(&Value::Array(vec![Value::from(""), Value::from("")]))?;
        assert_eq!((one, two), (none.saturating_add(1), none.saturating_add(2)));
        let meter = Meter::new(1_000);
        let value = to_variable(&Value::object([("n", Value::from(1))]))?;
        contracts().check("step", &value, true, Some(&meter))?;
        assert_eq!(meter.used(), boundary_units(&value, u64::MAX));
        assert_eq!(boundary_units(&value, 1), 2);
        Ok(())
    }

    /// #1708: past the budget the walk neither scans nor queues the rest of a
    /// large array or object.
    #[test]
    fn boundary_fuel_stops_queueing_once_the_budget_is_spent() -> Checked {
        let strings = Value::Array(vec![Value::from(""); 10_000]);
        let members = Value::Object(
            (0..10_000)
                .map(|n| (format!("k{n}"), Value::Null))
                .collect(),
        );
        for value in [&strings, &members] {
            let variable = to_variable(value)?;
            let mut queued = 0;
            assert_eq!(boundary_walk(&variable, 1, &mut queued), 2);
            assert_eq!(queued, 1);
            let mut all = 0;
            let units = boundary_walk(&variable, u64::MAX, &mut all);
            assert_eq!(all, 10_001);
            assert_eq!(boundary_units(&variable, units), units);
        }
        Ok(())
    }
}
