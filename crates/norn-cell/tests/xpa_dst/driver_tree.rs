//! Layer 4C — Test-harness deterministic xP&A driver-tree ripple & convergence
//! model (`sampled`, test-harness model).
//!
//! Propagates headcount & overtime shift cost -> store operating cost & EBITDA
//! -> cash & Open-To-Buy (OTB) constraint in exact 6-decimal `i128` arithmetic
//! via Kahn topological sorting, refusing circular dependency loops fail-closed.
//!
//! **Honest evidence note:** Production S2 OTB/Merchandising and S5
//! Workforce/Payroll pack/kernel seams are not yet on `main` (`unknown` /
//! pending S2–S5). This module provides `sampled` evidence for the test-harness
//! exact topological ripple model only, never claiming `runtime-enforced`
//! production S2/S5 enforcement.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use norn_checker::metric::compute::arithmetic::FACTOR;

use super::prng::{SplitMix64, decimal_from_scaled, div_i128, scaled};

/// Pure-exact xP&A driver node formula operating on 6-decimal scaled `i128` values.
#[derive(Debug, Clone)]
pub enum DriverFormula {
    Input(i128),
    Add(String, String),
    Sub(String, String),
    /// Exact scaled multiplication `(a * b) / FACTOR`.
    Mul(String, String),
    /// `max(0, a)` for cash/OTB constraint floors.
    ClampNonNegative(String),
}

/// Error returned by the test-harness deterministic driver-tree evaluator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriverTreeError {
    CycleDetected { nodes: Vec<String> },
    MissingDependency { node: String, dependency: String },
    Overflow { node: String },
}

/// Test-harness deterministic topological driver-tree evaluator verifying
/// ripple convergence and zero silent circular loops (`sampled`).
#[derive(Debug, Default, Clone)]
pub struct DriverTree {
    nodes: BTreeMap<String, DriverFormula>,
}

impl DriverTree {
    pub fn set(&mut self, name: impl Into<String>, formula: DriverFormula) {
        self.nodes.insert(name.into(), formula);
    }

    /// Evaluate every node in deterministic Kahn topological order.
    ///
    /// # Errors
    /// Returns [`DriverTreeError::CycleDetected`] if any cycle exists, or
    /// missing-dependency / overflow errors.
    pub fn evaluate(&self) -> Result<BTreeMap<String, i128>, DriverTreeError> {
        let order = self.topological_order()?;
        let mut values: BTreeMap<String, i128> = BTreeMap::new();
        for name in order {
            let formula = self
                .nodes
                .get(&name)
                .ok_or_else(|| DriverTreeError::Overflow { node: name.clone() })?;
            let val = eval_formula(&name, formula, &values)?;
            values.insert(name, val);
        }
        Ok(values)
    }

    fn topological_order(&self) -> Result<Vec<String>, DriverTreeError> {
        let mut indegree: BTreeMap<String, usize> = BTreeMap::new();
        let mut dependents: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (name, formula) in &self.nodes {
            indegree.entry(name.clone()).or_insert(0);
            for dep in formula_deps(formula) {
                if !self.nodes.contains_key(dep) {
                    return Err(DriverTreeError::MissingDependency {
                        node: name.clone(),
                        dependency: dep.to_owned(),
                    });
                }
                let deg = indegree.entry(name.clone()).or_insert(0);
                *deg = deg.saturating_add(1);
                dependents
                    .entry(dep.to_owned())
                    .or_default()
                    .push(name.clone());
            }
        }
        let mut ready: VecDeque<String> = indegree
            .iter()
            .filter_map(|(k, &d)| (d == 0).then_some(k.clone()))
            .collect();
        let mut order = Vec::with_capacity(self.nodes.len());
        while let Some(node) = ready.pop_front() {
            order.push(node.clone());
            if let Some(children) = dependents.get(&node) {
                for child in children {
                    if let Some(deg) = indegree.get_mut(child) {
                        *deg = deg.saturating_sub(1);
                        if *deg == 0 {
                            ready.push_back(child.clone());
                        }
                    }
                }
            }
        }
        if order.len() != self.nodes.len() {
            let visited: BTreeSet<&str> = order.iter().map(String::as_str).collect();
            let cyclic = self
                .nodes
                .keys()
                .filter(|k| !visited.contains(k.as_str()))
                .cloned()
                .collect();
            return Err(DriverTreeError::CycleDetected { nodes: cyclic });
        }
        Ok(order)
    }
}

fn eval_formula(
    name: &str,
    formula: &DriverFormula,
    values: &BTreeMap<String, i128>,
) -> Result<i128, DriverTreeError> {
    let overflow = || DriverTreeError::Overflow {
        node: name.to_owned(),
    };
    match formula {
        DriverFormula::Input(v) => Ok(*v),
        DriverFormula::Add(a, b) => values
            .get(a)
            .copied()
            .unwrap_or(0)
            .checked_add(values.get(b).copied().unwrap_or(0))
            .ok_or_else(overflow),
        DriverFormula::Sub(a, b) => values
            .get(a)
            .copied()
            .unwrap_or(0)
            .checked_sub(values.get(b).copied().unwrap_or(0))
            .ok_or_else(overflow),
        DriverFormula::Mul(a, b) => {
            let prod = values
                .get(a)
                .copied()
                .unwrap_or(0)
                .checked_mul(values.get(b).copied().unwrap_or(0))
                .ok_or_else(overflow)?;
            Ok(div_i128(prod, FACTOR))
        }
        DriverFormula::ClampNonNegative(a) => Ok(values.get(a).copied().unwrap_or(0).max(0)),
    }
}

fn formula_deps(formula: &DriverFormula) -> Vec<&str> {
    match formula {
        DriverFormula::Input(_) => Vec::new(),
        DriverFormula::Add(a, b) | DriverFormula::Sub(a, b) | DriverFormula::Mul(a, b) => {
            vec![a.as_str(), b.as_str()]
        }
        DriverFormula::ClampNonNegative(a) => vec![a.as_str()],
    }
}

/// Verify test-harness deterministic driver-tree ripple & convergence across
/// headcount/shift cost -> store margin -> cash/OTB constraint, plus cycle
/// refusal (`sampled`, test-harness model; production S2/S5 seams pending).
///
/// # Errors
/// Returns an error if convergence, ripple delta, or cycle rejection fails.
pub fn run_driver_tree_ripple_and_cycle_check(seed: u64) -> Result<(), String> {
    let mut rng = SplitMix64::new(seed).fork(0xD217_E200);
    for _ in 0..32 {
        let headcount = scaled(i128::from(rng.range_u64(4, 25)));
        let shift_hours = scaled(i128::from(rng.range_u64(35, 55)));
        let hourly_rate = scaled(i128::from(rng.range_u64(40, 120)));
        let gross_margin = scaled(i128::from(rng.range_u64(120_000, 400_000)));
        let fixed_rent = scaled(i128::from(rng.range_u64(15_000, 50_000)));
        let cash_reserve = scaled(i128::from(rng.range_u64(25_000, 80_000)));
        let min_cash_floor = scaled(i128::from(rng.range_u64(10_000, 30_000)));

        let base_tree = build_store_otb_tree(
            headcount,
            shift_hours,
            hourly_rate,
            gross_margin,
            fixed_rent,
            cash_reserve,
            min_cash_floor,
        );
        let base_a = base_tree
            .evaluate()
            .map_err(|e| format!("base tree eval: {e:?}"))?;
        let base_b = base_tree
            .evaluate()
            .map_err(|e| format!("base tree repeat eval: {e:?}"))?;
        if base_a != base_b {
            return Err("driver tree evaluation was non-deterministic".to_owned());
        }

        let ot_tree = build_store_otb_tree(
            headcount,
            shift_hours.saturating_add(scaled(4)),
            hourly_rate,
            gross_margin,
            fixed_rent,
            cash_reserve,
            min_cash_floor,
        );
        let ot = ot_tree
            .evaluate()
            .map_err(|e| format!("overtime tree eval: {e:?}"))?;
        let expected_payroll_delta =
            div_i128(headcount.saturating_mul(4), FACTOR).saturating_mul(hourly_rate);
        let actual_payroll_delta = ot
            .get("payroll_cost")
            .copied()
            .unwrap_or(0)
            .saturating_sub(base_a.get("payroll_cost").copied().unwrap_or(0));
        if actual_payroll_delta != expected_payroll_delta {
            return Err(format!(
                "driver-tree ripple mismatch: actual={} expected={}",
                decimal_from_scaled(actual_payroll_delta)?,
                decimal_from_scaled(expected_payroll_delta)?
            ));
        }
        let ebitda_delta = ot
            .get("store_ebitda")
            .copied()
            .unwrap_or(0)
            .saturating_sub(base_a.get("store_ebitda").copied().unwrap_or(0));
        if ebitda_delta != expected_payroll_delta.saturating_neg() {
            return Err("EBITDA did not reflect exact negative payroll ripple".to_owned());
        }
        if ot.get("otb_budget").copied().unwrap_or(0)
            > base_a.get("otb_budget").copied().unwrap_or(0)
        {
            return Err("OTB budget must tighten monotonically under higher payroll".to_owned());
        }
    }

    assert_driver_tree_refuses_cycles()
}

fn build_store_otb_tree(
    headcount: i128,
    shift_hours: i128,
    hourly_rate: i128,
    gross_margin: i128,
    fixed_rent: i128,
    cash_reserve: i128,
    min_cash_floor: i128,
) -> DriverTree {
    let mut tree = DriverTree::default();
    tree.set("headcount", DriverFormula::Input(headcount));
    tree.set("shift_hours", DriverFormula::Input(shift_hours));
    tree.set("hourly_rate", DriverFormula::Input(hourly_rate));
    tree.set("gross_margin", DriverFormula::Input(gross_margin));
    tree.set("fixed_rent", DriverFormula::Input(fixed_rent));
    tree.set("cash_reserve", DriverFormula::Input(cash_reserve));
    tree.set("min_cash_floor", DriverFormula::Input(min_cash_floor));
    tree.set(
        "total_hours",
        DriverFormula::Mul("headcount".into(), "shift_hours".into()),
    );
    tree.set(
        "payroll_cost",
        DriverFormula::Mul("total_hours".into(), "hourly_rate".into()),
    );
    tree.set(
        "store_opex",
        DriverFormula::Add("payroll_cost".into(), "fixed_rent".into()),
    );
    tree.set(
        "store_ebitda",
        DriverFormula::Sub("gross_margin".into(), "store_opex".into()),
    );
    tree.set(
        "projected_cash",
        DriverFormula::Add("cash_reserve".into(), "store_ebitda".into()),
    );
    tree.set(
        "raw_otb_headroom",
        DriverFormula::Sub("projected_cash".into(), "min_cash_floor".into()),
    );
    tree.set(
        "otb_budget",
        DriverFormula::ClampNonNegative("raw_otb_headroom".into()),
    );
    tree
}

fn assert_driver_tree_refuses_cycles() -> Result<(), String> {
    let mut cyclic = DriverTree::default();
    cyclic.set(
        "payroll_cost",
        DriverFormula::Add("otb_budget".into(), "base_cost".into()),
    );
    cyclic.set("base_cost", DriverFormula::Input(scaled(1_000)));
    cyclic.set(
        "store_ebitda",
        DriverFormula::Sub("base_cost".into(), "payroll_cost".into()),
    );
    cyclic.set(
        "otb_budget",
        DriverFormula::ClampNonNegative("store_ebitda".into()),
    );
    let cycle_res = cyclic.evaluate();
    if !matches!(cycle_res, Err(DriverTreeError::CycleDetected { .. })) {
        return Err(format!(
            "circular driver tree MUST refuse CycleDetected, got {cycle_res:?}"
        ));
    }
    Ok(())
}
