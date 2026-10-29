//! Norn `metering` feature: a deterministic operation budget.
//!
//! A [`Meter`] is a shared counter of abstract work units with a fixed limit. The
//! expression VM charges it for every executed opcode and for data-proportional work
//! (string, collection and regex builtins), and zen-engine charges graph node and
//! decision-table row visits. Units depend only on the program and its input values,
//! never on time, hash seeds, caching or the target, so the same evaluation exhausts
//! the same budget at the same count natively and on `wasm32-unknown-unknown`.
//!
//! Once a charge exceeds the limit the meter stays exhausted: every later charge fails
//! with the same [`BudgetExhausted`] value, even if an intermediate caller swallowed
//! the first error.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use thiserror::Error;

use crate::variable::Variable;

/// Returned when a charge would take a [`Meter`] past its limit.
///
/// `used` is the total that the failing charge would have reached (so `used > limit`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error("operation budget exhausted: {used} units needed, limit {limit}")]
pub struct BudgetExhausted {
    pub limit: u64,
    pub used: u64,
}

/// Shared, cloneable operation budget. Clones charge the same counter.
#[derive(Debug, Clone)]
pub struct Meter(Arc<Inner>);

#[derive(Debug)]
struct Inner {
    limit: u64,
    used: AtomicU64,
    /// `used` value of the first failing charge; 0 while not exhausted.
    exhausted_at: AtomicU64,
    /// Values visited while measuring data-proportional charges (diagnostics/tests).
    scan_steps: AtomicU64,
}

impl Meter {
    pub fn new(limit: u64) -> Self {
        Self(Arc::new(Inner {
            limit,
            used: AtomicU64::new(0),
            exhausted_at: AtomicU64::new(0),
            scan_steps: AtomicU64::new(0),
        }))
    }

    pub fn limit(&self) -> u64 {
        self.0.limit
    }

    /// Values visited so far while *measuring* charges (not charged work). Measuring
    /// stops once a cost exceeds the remaining budget, so this stays O(budget) even for
    /// arbitrarily large inputs.
    pub fn scan_steps(&self) -> u64 {
        self.0.scan_steps.load(Ordering::Relaxed)
    }

    pub(crate) fn record_scan(&self, steps: u64) {
        self.0.scan_steps.fetch_add(steps, Ordering::Relaxed);
    }

    /// Units still available before the limit.
    pub fn remaining(&self) -> u64 {
        self.0.limit.saturating_sub(self.used())
    }

    /// Units charged so far (not counting a failing charge).
    pub fn used(&self) -> u64 {
        self.0.used.load(Ordering::Relaxed)
    }

    /// The first exhaustion, if the budget has been exceeded.
    pub fn exhausted(&self) -> Option<BudgetExhausted> {
        match self.0.exhausted_at.load(Ordering::Relaxed) {
            0 => None,
            used => Some(BudgetExhausted {
                limit: self.0.limit,
                used,
            }),
        }
    }

    /// Adds `units`, or fails without adding them if that would exceed the limit.
    pub fn charge(&self, units: u64) -> Result<(), BudgetExhausted> {
        if let Some(exhausted) = self.exhausted() {
            return Err(exhausted);
        }

        let used = self.used().saturating_add(units);
        if used > self.0.limit {
            self.0.exhausted_at.store(used, Ordering::Relaxed);
            return Err(BudgetExhausted {
                limit: self.0.limit,
                used,
            });
        }

        self.0.used.store(used, Ordering::Relaxed);
        Ok(())
    }
}

/// Cost model. Every executed opcode costs [`cost::OPCODE`]; the constants below are
/// charged on top of it. Sizes are shallow: string length in bytes, array or object
/// length in elements, 1 for anything else.
pub mod cost {
    use super::Variable;

    /// Every executed VM opcode (closure bodies are opcodes, so every iteration of
    /// `map`, `filter`, `flatMap`, ... is charged).
    pub const OPCODE: u64 = 1;
    /// Compiling a regular expression (`matches`, `extract`), plus the pattern length.
    pub const REGEX_COMPILE: u64 = 64;
    /// Checked temporal arithmetic (in addition to shallow argument sizes).
    pub const TEMPORAL_ARITHMETIC: u64 = 8;
    /// One zen-engine graph node visit.
    pub const NODE_VISIT: u64 = 1;
    /// One zen-engine decision-table row considered.
    pub const TABLE_ROW: u64 = 1;
    /// One element of a zen-engine transform-attributes loop, charged up front for the
    /// whole input array.
    pub const TRANSFORM_ITEM: u64 = 1;

    /// Shallow size of a value, used by data-proportional charges.
    pub fn size(value: &Variable) -> u64 {
        match value {
            Variable::String(s) => s.len() as u64,
            Variable::Array(a) => a.borrow().len() as u64,
            Variable::Object(o) => o.borrow().len() as u64,
            _ => 1,
        }
    }

    /// A container's length plus the shallow size of each direct child: the data that
    /// one-level builtins (`flatten`, `merge`, the flatten opcode) traverse.
    ///
    /// Bounded by `cap` (the meter's remaining units): the outer length is counted first
    /// in O(1), and the child scan stops as soon as the total exceeds `cap`. Any result
    /// above `cap` exhausts the meter the same way, so the value is clamped to `cap + 1`
    /// and never depends on how far past the budget the input goes. `steps` counts the
    /// children visited.
    pub fn nested_size_capped(value: &Variable, cap: u64, steps: &mut u64) -> u64 {
        fn scan<'a>(
            len: usize,
            children: impl Iterator<Item = &'a Variable>,
            cap: u64,
            steps: &mut u64,
        ) -> u64 {
            let mut acc = len as u64;
            if acc > cap {
                return cap.saturating_add(1);
            }
            for child in children {
                *steps += 1;
                acc = acc.saturating_add(size(child));
                if acc > cap {
                    return cap.saturating_add(1);
                }
            }
            acc
        }
        match value {
            Variable::Array(a) => {
                let a = a.borrow();
                scan(a.len(), a.iter(), cap, steps)
            }
            Variable::Object(o) => {
                let o = o.borrow();
                scan(o.len(), o.values(), cap, steps)
            }
            other => size(other),
        }
    }

    /// Recursive size, for builtins whose work is proportional to all nested data
    /// (`mergeDeep`, `fuzzyMatch`). The walk stops once the total exceeds `cap`
    /// (the caller passes the meter's remaining units), so measuring is itself bounded
    /// by the budget; any result above `cap` is clamped to `cap + 1`. `steps` counts the
    /// values visited.
    pub fn deep_size_capped(value: &Variable, cap: u64, steps: &mut u64) -> u64 {
        fn walk(value: &Variable, acc: &mut u64, cap: u64, steps: &mut u64) {
            if *acc > cap {
                return;
            }
            *steps += 1;
            match value {
                Variable::Array(a) => {
                    *acc = acc.saturating_add(1);
                    for v in a.borrow().iter() {
                        walk(v, acc, cap, steps);
                        if *acc > cap {
                            return;
                        }
                    }
                }
                Variable::Object(o) => {
                    *acc = acc.saturating_add(1);
                    for (_, v) in o.borrow().iter() {
                        walk(v, acc, cap, steps);
                        if *acc > cap {
                            return;
                        }
                    }
                }
                other => *acc = acc.saturating_add(size(other)),
            }
        }
        let mut acc = 0;
        walk(value, &mut acc, cap, steps);
        acc.min(cap.saturating_add(1))
    }
}
