//! Coverage counters recorded while a machine runs.
//!
//! `proptest-state-machine` drives [`StateMachineTest`] through associated
//! functions (no `self`), so counters live in a thread-local that [`run`]
//! resets before and drains after the run. The runner never forks, so every
//! transition is applied on the calling thread.
//!
//! [`StateMachineTest`]: proptest_state_machine::StateMachineTest
//! [`run`]: crate::run

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fmt;

use crate::model::Outcome;
use norn_kernel::EnvelopeKind;

thread_local! {
    static COUNTS: RefCell<BTreeMap<String, u64>> = const { RefCell::new(BTreeMap::new()) };
}

/// Labels for every kernel outcome; a decider whose model can reach them all
/// should assert `coverage.assert_covered(KERNEL_OUTCOMES)`.
pub const KERNEL_OUTCOMES: &[&str] = &[
    Outcome::Accepted.label(),
    Outcome::Rejected.label(),
    Outcome::ReadOnlyWorld.label(),
    Outcome::PerformOutsideMain.label(),
    Outcome::EnvelopeExceeded(EnvelopeKind::Events).label(),
    Outcome::EnvelopeExceeded(EnvelopeKind::Commands).label(),
    Outcome::EnvelopeExceeded(EnvelopeKind::Timers).label(),
];

pub(crate) fn hit(label: &str) {
    COUNTS.with(|counts| {
        let mut counts = counts.borrow_mut();
        let slot = counts.entry(label.to_owned()).or_insert(0);
        *slot = slot.saturating_add(1);
    });
}

pub(crate) fn hit_prefixed(prefix: &str, label: &str) {
    hit(&format!("{prefix}:{label}"));
}

pub(crate) fn reset() {
    COUNTS.with(|counts| counts.borrow_mut().clear());
}

pub(crate) fn take() -> Coverage {
    Coverage {
        counts: COUNTS.with(|counts| std::mem::take(&mut *counts.borrow_mut())),
    }
}

/// How often each labelled transition, outcome, world, event, and effect was
/// applied during one [`run`](crate::run).
///
/// Labels: `input:<model label>`, `event:<model label>`, `outcome:<kind>`
/// (see [`KERNEL_OUTCOMES`]), `world:main|fork|historical`, and
/// `effect:perform|schedule|cancel_timer`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Coverage {
    counts: BTreeMap<String, u64>,
}

impl Coverage {
    /// How many times `label` was hit.
    #[must_use]
    pub fn count(&self, label: &str) -> u64 {
        self.counts.get(label).copied().unwrap_or(0)
    }

    /// All counters, ordered by label.
    pub fn iter(&self) -> impl Iterator<Item = (&str, u64)> {
        self.counts
            .iter()
            .map(|(label, count)| (label.as_str(), *count))
    }

    /// The labels in `required` that were never hit.
    #[must_use]
    pub fn missing<'a>(&self, required: &[&'a str]) -> Vec<&'a str> {
        required
            .iter()
            .copied()
            .filter(|label| self.count(label) == 0)
            .collect()
    }

    /// Asserts every label in `required` was hit at least once.
    ///
    /// # Panics
    ///
    /// Panics listing the missing labels and the full counter table.
    #[track_caller]
    pub fn assert_covered(&self, required: &[&str]) {
        let missing = self.missing(required);
        assert!(
            missing.is_empty(),
            "coverage gap: {missing:?} never exercised; raise cases or reweight strategies\n{self}"
        );
    }
}

impl fmt::Display for Coverage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (label, count) in &self.counts {
            writeln!(f, "{count:>8}  {label}")?;
        }
        Ok(())
    }
}
