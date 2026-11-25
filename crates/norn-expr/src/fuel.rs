//! One monotonic fuel tank per decision, query or commit turn (P1).
//!
//! A turn owns exactly one [`TurnFuel`]. Every evaluation it runs borrows its
//! [`EvaluationBudget`] from the tank's remainder and is charged what it used,
//! so nested or looped evaluations share one limit: none can restart from the
//! full limit, and none can multiply it. The tank is neither `Clone` nor
//! `Copy`, has no refill or reset, and spent units never decrease.

use norn_ir::ResourceLimitProfile;

use crate::{EvaluationAttempt, EvaluationBudget, ExprError};

/// The operation units one turn may spend across every evaluation it runs.
#[derive(Debug, PartialEq, Eq)]
pub struct TurnFuel {
    limit: u64,
    spent: u64,
}

impl Default for TurnFuel {
    /// The decide fuel of the shipped limit profile `norn.limits/1`, the same
    /// limit as [`EvaluationBudget::default`].
    fn default() -> Self {
        Self::new(EvaluationBudget::default().units())
    }
}

impl TurnFuel {
    /// A full tank of `limit` units.
    #[must_use]
    pub const fn new(limit: u64) -> Self {
        Self { limit, spent: 0 }
    }

    /// The per-turn fuel of `profile` (`turn.fuel_units`): one action or
    /// commit turn, including every derivation, rule and law it evaluates.
    #[must_use]
    pub const fn turn(profile: &ResourceLimitProfile) -> Self {
        Self::new(profile.turn.fuel_units)
    }

    /// The per-decision fuel of `profile` (`decide.fuel_units`).
    #[must_use]
    pub const fn decide(profile: &ResourceLimitProfile) -> Self {
        Self::new(profile.decide.fuel_units)
    }

    /// The tank's fixed limit.
    #[must_use]
    pub const fn limit(&self) -> u64 {
        self.limit
    }

    /// Units spent so far (never above [`Self::limit`]).
    #[must_use]
    pub const fn spent(&self) -> u64 {
        self.spent
    }

    /// Units still available.
    #[must_use]
    pub const fn remaining(&self) -> u64 {
        self.limit.saturating_sub(self.spent)
    }

    /// The budget one evaluation may borrow: exactly the remainder.
    #[must_use]
    pub const fn lend(&self) -> EvaluationBudget {
        EvaluationBudget::new(self.remaining())
    }

    /// Runs one metered evaluation on a budget lent from the remainder and
    /// charges what it used, whether it succeeded or failed. The charge is
    /// capped at the lent budget, so a callee can never report more than it
    /// borrowed, and the tank only ever drains.
    pub fn spend(
        &mut self,
        evaluate: impl FnOnce(EvaluationBudget) -> EvaluationAttempt,
    ) -> EvaluationAttempt {
        let budget = self.lend();
        let attempt = evaluate(budget);
        self.spent = self
            .spent
            .saturating_add(attempt.units_used.min(budget.units()));
        attempt
    }

    /// Charges `units` of data-dependent work up front.
    ///
    /// # Errors
    ///
    /// [`ExprError::BudgetExhausted`] when the charge exceeds the remainder.
    /// Exhaustion is terminal: the tank is drained, so every later charge or
    /// evaluation in the turn refuses too.
    pub fn charge(&mut self, units: u64) -> Result<(), ExprError> {
        let needed = self.spent.saturating_add(units);
        if needed <= self.limit {
            self.spent = needed;
            return Ok(());
        }
        self.spent = self.limit;
        Err(ExprError::BudgetExhausted {
            limit: self.limit,
            used: needed,
        })
    }

    /// Spends up to `units` for work already admitted against
    /// [`Self::remaining`], saturating at the limit (never refunds).
    pub fn burn(&mut self, units: u64) {
        self.spent = self.spent.saturating_add(units).min(self.limit);
    }

    /// Charges a sort of `len` items up front at [`sort_cost`], so the
    /// verdict depends on the input's size and never on its arrangement.
    ///
    /// # Errors
    ///
    /// As [`Self::charge`].
    pub fn charge_sort(&mut self, len: usize) -> Result<(), ExprError> {
        self.charge(sort_cost(u64::try_from(len).unwrap_or(u64::MAX)))
    }
}

/// The order-independent cost of sorting `len` items: `len · ⌈log₂ len⌉`
/// (at least `len`), saturating at `u64::MAX`.
#[must_use]
pub fn sort_cost(len: u64) -> u64 {
    let log = if len <= 1 {
        1
    } else {
        // ⌈log₂ len⌉ for len ≥ 2: the bit width of len − 1.
        u64::BITS.saturating_sub(len.saturating_sub(1).leading_zeros())
    };
    len.saturating_mul(u64::from(log))
}

#[cfg(test)]
mod tests {
    use super::{TurnFuel, sort_cost};
    use crate::{EvaluationAttempt, EvaluationBudget, ExprError, Expression, Value};
    use proptest::prelude::*;

    fn eval(fuel: &mut TurnFuel, source: &str) -> EvaluationAttempt {
        let expression = Expression::compile(source).expect("compiles");
        fuel.spend(|budget| expression.evaluate_metered(&Value::Null, budget))
    }

    #[test]
    fn spends_monotonically_and_refuses_at_the_threshold() {
        let mut probe = TurnFuel::new(u64::MAX);
        let cost = eval(&mut probe, "1 + 2").units_used;
        assert!(cost > 0);
        // Exactly enough for two evaluations: both succeed, the third refuses.
        let mut fuel = TurnFuel::new(cost.saturating_mul(2));
        assert!(eval(&mut fuel, "1 + 2").result.is_ok());
        assert_eq!(fuel.spent(), cost);
        assert!(eval(&mut fuel, "1 + 2").result.is_ok());
        assert_eq!(fuel.remaining(), 0);
        let refused = eval(&mut fuel, "1 + 2");
        assert!(matches!(
            refused.result,
            Err(ExprError::BudgetExhausted { limit: 0, .. })
        ));
        assert_eq!(fuel.spent(), cost.saturating_mul(2));
    }

    #[test]
    fn a_callee_cannot_report_more_than_it_borrowed() {
        let mut fuel = TurnFuel::new(10);
        let attempt = fuel.spend(|_budget| EvaluationAttempt {
            result: Err(ExprError::BudgetExhausted {
                limit: 10,
                used: 99,
            }),
            units_used: 99,
        });
        assert_eq!(attempt.units_used, 99);
        assert_eq!(fuel.spent(), 10);
        assert_eq!(fuel.lend(), EvaluationBudget::new(0));
    }

    #[test]
    fn exhaustion_is_terminal() {
        let mut fuel = TurnFuel::new(5);
        assert_eq!(fuel.charge(5), Ok(()));
        assert_eq!(
            fuel.charge(1),
            Err(ExprError::BudgetExhausted { limit: 5, used: 6 })
        );
        let mut fuel = TurnFuel::new(5);
        assert!(fuel.charge(6).is_err());
        assert_eq!(fuel.remaining(), 0);
        assert!(fuel.charge(0).is_ok());
        assert!(fuel.charge(1).is_err());
    }

    #[test]
    fn sort_cost_is_n_ceil_log2_n() {
        assert_eq!(sort_cost(0), 0);
        assert_eq!(sort_cost(1), 1);
        assert_eq!(sort_cost(2), 2);
        assert_eq!(sort_cost(3), 6);
        assert_eq!(sort_cost(4), 8);
        assert_eq!(sort_cost(5), 15);
        assert_eq!(sort_cost(1024), 10_240);
        assert_eq!(sort_cost(u64::MAX), u64::MAX);
    }

    proptest! {
        /// Spent units never decrease and never pass the limit, whatever the
        /// mix of charges and evaluations.
        #[test]
        fn spent_is_monotonic_and_bounded(
            limit in 0u64..10_000,
            steps in proptest::collection::vec((any::<bool>(), 0u64..4_000), 0..24),
        ) {
            let mut fuel = TurnFuel::new(limit);
            let mut before = 0;
            for (evaluate, units) in steps {
                if evaluate {
                    let attempt = fuel.spend(|budget| EvaluationAttempt {
                        result: Err(ExprError::Evaluation { message: String::new() }),
                        units_used: units.min(budget.units()),
                    });
                    prop_assert!(attempt.units_used <= limit.saturating_sub(before));
                } else {
                    let charged = fuel.charge(units);
                    prop_assert_eq!(charged.is_ok(), before.saturating_add(units) <= limit);
                }
                prop_assert!(fuel.spent() >= before);
                prop_assert!(fuel.spent() <= fuel.limit());
                prop_assert_eq!(fuel.spent().saturating_add(fuel.remaining()), fuel.limit());
                before = fuel.spent();
            }
        }

        /// Nested and looped evaluations share the tank: the total any
        /// sequence of evaluations can be charged is at most the limit, so no
        /// nesting depth amplifies the budget.
        #[test]
        fn nesting_never_amplifies_the_budget(
            limit in 0u64..10_000,
            loops in 1usize..8,
            depth in 1usize..6,
        ) {
            let mut fuel = TurnFuel::new(limit);
            let mut lent_total: u64 = 0;
            for _ in 0..loops {
                for _ in 0..depth {
                    let attempt = fuel.spend(|budget| EvaluationAttempt {
                        result: Err(ExprError::Evaluation { message: String::new() }),
                        units_used: budget.units().div_ceil(2),
                    });
                    lent_total = lent_total.saturating_add(attempt.units_used);
                }
            }
            prop_assert!(lent_total <= limit);
            prop_assert_eq!(fuel.spent(), lent_total);
        }

        /// The sort charge depends only on the length: a refusal at the exact
        /// threshold is deterministic.
        #[test]
        fn sort_charge_refuses_exactly_past_its_cost(len in 0usize..5_000) {
            let cost = sort_cost(u64::try_from(len).expect("fits"));
            let mut exact = TurnFuel::new(cost);
            prop_assert!(exact.charge_sort(len).is_ok());
            if let Some(below) = cost.checked_sub(1) {
                let mut short = TurnFuel::new(below);
                prop_assert_eq!(
                    short.charge_sort(len),
                    Err(ExprError::BudgetExhausted { limit: below, used: cost })
                );
            }
        }
    }
}
