//! Ordered-fold transition relation; application state is entirely declared.
use crate::{
    convert::{from_variable, value_to_canonical},
    evaluate::poll_once,
    step::StepContracts,
};
use norn_expr::schema::Schema;
use norn_ir::{
    Rounding as IrRounding,
    jdm::{FoldOrderKind, OrderedFold},
};
use std::collections::BTreeSet;
use zen_engine::{
    Decision, EvaluationOptions, Isolate, Variable,
    meter::Meter,
    nodes::DecisionContracts,
    scaled::{Rounding, ScaledArithmetic},
};

type Failure = Box<dyn std::error::Error>;

#[derive(Debug)]
pub(crate) struct FoldPlan {
    pub(crate) declaration: OrderedFold,
    pub(crate) selector: String,
    pub(crate) key: String,
    pub(crate) helper: Decision,
    pub(crate) item_schema: Schema,
    pub(crate) state_schema: Schema,
}

impl FoldPlan {
    pub(crate) fn run(
        &self,
        input: &Variable,
        meter: &Meter,
        contracts: &StepContracts,
    ) -> Result<Variable, Failure> {
        let f = &self.declaration;
        let profile = ScaledArithmetic {
            scale: f.arithmetic.scale,
            rounding: match f.arithmetic.rounding {
                IrRounding::Refuse => Rounding::Refuse,
                IrRounding::TowardZero => Rounding::TowardZero,
                IrRounding::HalfEven => Rounding::HalfEven,
                IrRounding::HalfAwayFromZero => Rounding::HalfAwayFromZero,
            },
        };
        let meter = meter.with_arithmetic(profile);
        inspect(input, &meter, profile)?;
        let mut isolate = Isolate::with_environment(input.clone());
        isolate.set_meter(Some(meter.clone()));
        let selected = isolate.run_standard(&self.selector)?;
        let Variable::Array(items) = selected else {
            return Err("fold input must be a complete array".into());
        };
        let items = items.borrow();
        if items.len() > usize::try_from(f.max_items)? {
            return Err("fold item bound exceeded".into());
        }
        let ordered = ordered_items(&items, f, &self.item_schema, &meter, profile)?;
        let mut state = isolate.run_standard(f.initial.as_str())?;
        check_state(
            &state,
            &self.state_schema,
            f.max_accumulator_bytes,
            &meter,
            profile,
        )?;
        state = crate::convert::to_variable(&from_variable(&state)?)?;
        for (index, (_, item)) in ordered.into_iter().enumerate() {
            meter.charge(1)?;
            let step = Variable::from_object(
                [
                    (Variable::key("accumulator"), state),
                    (Variable::key("item"), item.clone()),
                    (Variable::key("index"), Variable::Number(index.into())),
                ]
                .into_iter()
                .collect(),
            );
            contracts.check(&self.key, &step, true, Some(&meter))?;
            let response = poll_once(self.helper.evaluate_metered(
                step,
                EvaluationOptions::default(),
                meter.clone(),
            ))??;
            state = response.result;
            contracts.check(&self.key, &state, false, Some(&meter))?;
            check_state(
                &state,
                &self.state_schema,
                f.max_accumulator_bytes,
                &meter,
                profile,
            )?;
            state = crate::convert::to_variable(&from_variable(&state)?)?;
        }
        Ok(state)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum OrderKey {
    String(String),
    Integer(i128),
}

fn inspect(value: &Variable, meter: &Meter, profile: ScaledArithmetic) -> Result<(), Failure> {
    meter.charge(1)?;
    match value {
        Variable::Number(n) => {
            profile.coefficient(*n)?;
        }
        Variable::String(s) => meter.charge(u64::try_from(s.len())?)?,
        Variable::Array(a) => {
            for v in a.borrow().iter() {
                inspect(v, meter, profile)?;
            }
        }
        Variable::Object(o) => {
            for (key, v) in o.borrow().iter() {
                meter.charge(u64::try_from(key.len())?)?;
                inspect(v, meter, profile)?;
            }
        }
        Variable::Null | Variable::Bool(_) => {}
        Variable::Dynamic(_) => return Err("unsupported fold value".into()),
    }
    Ok(())
}

fn check_state(
    value: &Variable,
    schema: &Schema,
    bound: u32,
    meter: &Meter,
    profile: ScaledArithmetic,
) -> Result<(), Failure> {
    inspect(value, meter, profile)?;
    let value = from_variable(value)?;
    schema.check(&value, "accumulator").map_err(|e| e.reason)?;
    let bytes = norn_ir::canonical::serialize(&value_to_canonical(&value))?;
    meter.charge(u64::try_from(bytes.len())?)?;
    if bytes.len() > usize::try_from(bound)? {
        return Err("fold accumulator byte bound exceeded".into());
    }
    Ok(())
}

pub(crate) fn fold_key(rule: &str, node: &str) -> String {
    format!("\0fold:{}:{rule}{node}", rule.len())
}

fn ordered_items<'a>(
    items: &'a [Variable],
    f: &OrderedFold,
    item_schema: &Schema,
    meter: &Meter,
    profile: ScaledArithmetic,
) -> Result<Vec<(Vec<OrderKey>, &'a Variable)>, Failure> {
    let mut ordered = Vec::with_capacity(items.len());
    let mut identities = BTreeSet::new();
    for item in items {
        inspect(item, meter, profile)?;
        item_schema
            .check(&from_variable(item)?, "item")
            .map_err(|e| e.reason)?;
        let mut keys = Vec::new();
        for key in &f.order {
            let value = item
                .as_object()
                .ok_or("item is not a record")?
                .borrow()
                .get_str(&key.property)
                .cloned()
                .ok_or("missing order key")?;
            keys.push(match (key.kind, value) {
                (FoldOrderKind::String, Variable::String(s)) => {
                    OrderKey::String(norn_ir::canonical::nfc(s.as_str()))
                }
                (FoldOrderKind::Integer, Variable::Number(n)) if n.fract().is_zero() => {
                    OrderKey::Integer(
                        n.mantissa()
                            .checked_div(10_i128.pow(n.scale()))
                            .ok_or("integer order overflow")?,
                    )
                }
                _ => return Err("invalid order value".into()),
            });
        }
        let identity = keys.last().cloned().ok_or("missing identity")?;
        let OrderKey::String(ref text) = identity else {
            return Err("non-string identity".into());
        };
        meter.charge(
            u64::try_from(text.len().saturating_add(1))?
                .saturating_mul(u64::try_from(identities.len().saturating_add(1))?),
        )?;
        if !identities.insert(identity) {
            return Err("duplicate fold identity".into());
        }
        ordered.push((keys, item));
    }
    // Insertion sort has a fixed metered comparison schedule, including refusal.
    for i in 1..ordered.len() {
        let mut j = i;
        while let Some(previous) = j.checked_sub(1) {
            let left = &ordered.get(previous).ok_or("sort index")?.0;
            let right = &ordered.get(j).ok_or("sort index")?.0;
            for key in left.iter().chain(right) {
                meter.charge(match key {
                    OrderKey::String(s) => u64::try_from(s.len())?.saturating_add(1),
                    OrderKey::Integer(_) => 1,
                })?;
            }
            if left <= right {
                break;
            }
            ordered.swap(previous, j);
            j = previous;
        }
    }
    Ok(ordered)
}
