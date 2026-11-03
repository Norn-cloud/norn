//! DIAG-CORE-1 / DIAG-REPAIR-1: irreducible conflicts and editable-only
//! correction proposals. This analysis grants no admission or proof authority.
use std::collections::{BTreeSet, VecDeque};

use super::{ChaseBudget, ChaseError, ChaseOutcome, Fact, Theory, chase};

/// Largest supported declaration universe.
pub const MAX_DECLARATIONS: usize = 64;

/// One satisfiability query over declaration indices, relative to fixed assumptions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict<E> {
    /// A model exists.
    Sat,
    /// No model exists.
    Unsat,
    /// No claim, including when a budget was exhausted.
    Unknown(E),
}

/// Monotone conjunction backend; indices refer to the original declaration universe.
/// Removing a declaration must never restrict the set of models.
pub trait Oracle {
    /// Backend refusal detail.
    type Unknown;
    /// Decide the conjunction of the selected declarations and fixed background.
    fn check(&mut self, active: &BTreeSet<usize>) -> Verdict<Self::Unknown>;
}

/// Whole-search refusal; no unchecked minimality claim is returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unknown<E> {
    /// Whole-search oracle-call budget exhausted.
    Budget,
    /// The declaration universe exceeds `MAX_DECLARATIONS`.
    TooManyDeclarations,
    /// An oracle refused a subset query.
    Oracle(E),
    /// Invalid editable indices.
    InvalidEditable,
    /// A backend contradicted its own previous answers.
    InconsistentOracle,
}

/// Complete results relative to an oracle's explicitly recorded background.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnosis {
    /// Every irreducible unsatisfiable declaration subset, in deterministic order.
    pub cores: BTreeSet<BTreeSet<usize>>,
    /// All irreducible corrections using only explicitly editable declarations.
    /// These are relaxation proposals; exact edits require ordinary admission.
    pub corrections: BTreeSet<BTreeSet<usize>>,
    /// Actual oracle queries (including independent final checks).
    pub calls: usize,
    /// Configured whole-search query ceiling; disclose with the oracle bounds.
    pub max_calls: usize,
}

/// Enumerate conflicts and corrections by a hitting-set tree, shrinking each
/// conflict by deletion. "Minimal" means irreducible, not minimum cardinality.
/// DIAG-UNKNOWN-1: every final core/deletion and repair/restoration is checked;
/// any unknown aborts the result rather than being interpreted as SAT.
/// # Errors
/// Returns unknown on exhaustion, backend refusal, invalid editable indices,
/// or a declaration universe above [`MAX_DECLARATIONS`].
pub fn diagnose<O: Oracle>(
    oracle: &mut O,
    declarations: usize,
    editable: &BTreeSet<usize>,
    max_calls: usize,
) -> Result<Diagnosis, Unknown<O::Unknown>> {
    if declarations > MAX_DECLARATIONS {
        return Err(Unknown::TooManyDeclarations);
    }
    let all: BTreeSet<_> = (0..declarations).collect();
    if !editable.is_subset(&all) {
        return Err(Unknown::InvalidEditable);
    }
    let mut calls = 0;
    let mut unsat = |active: &BTreeSet<usize>| {
        if calls >= max_calls {
            return Err(Unknown::Budget);
        }
        calls = calls.saturating_add(1);
        match oracle.check(active) {
            Verdict::Sat => Ok(false),
            Verdict::Unsat => Ok(true),
            Verdict::Unknown(error) => Err(Unknown::Oracle(error)),
        }
    };
    let mut cores = BTreeSet::new();
    let mut corrections = BTreeSet::new();
    let mut queue = VecDeque::from([BTreeSet::new()]);
    let mut seen = BTreeSet::new();
    while let Some(removed) = queue.pop_front() {
        if !seen.insert(removed.clone()) {
            continue;
        }
        let mut active: BTreeSet<_> = all.difference(&removed).copied().collect();
        if unsat(&active)? {
            for member in active.clone() {
                active.remove(&member);
                if !unsat(&active)? {
                    active.insert(member);
                }
            }
            cores.insert(active.clone());
            for member in active {
                let mut next = removed.clone();
                next.insert(member);
                queue.push_back(next);
            }
        } else {
            let mut repair = removed.clone();
            for member in removed {
                active.insert(member);
                if unsat(&active)? {
                    active.remove(&member);
                } else {
                    repair.remove(&member);
                }
            }
            if repair.is_subset(editable) {
                corrections.insert(repair);
            }
        }
    }
    for core in &cores {
        if !unsat(core)? {
            return Err(Unknown::InconsistentOracle);
        }
        for member in core {
            let mut subset = core.clone();
            subset.remove(member);
            if unsat(&subset)? {
                return Err(Unknown::InconsistentOracle);
            }
        }
    }
    for repair in &corrections {
        let kept: BTreeSet<_> = all.difference(repair).copied().collect();
        if unsat(&kept)? {
            return Err(Unknown::InconsistentOracle);
        }
        for member in repair {
            let mut restored = kept.clone();
            restored.insert(*member);
            if !unsat(&restored)? {
                return Err(Unknown::InconsistentOracle);
            }
        }
    }
    Ok(Diagnosis {
        cores,
        corrections,
        calls,
        max_calls,
    })
}

/// All lowerings of one exact source declaration, kept or relaxed together.
#[derive(Debug, Clone)]
pub struct Declaration {
    /// Stable source member identity and RFC 6901 pointer.
    pub origin: super::RuleOrigin,
    /// Conjunctive rules generated from this declaration.
    pub theory: Theory,
    /// Input facts generated from this declaration.
    pub facts: Vec<Fact>,
}

/// Existing chase adapter. Its background facts are open-world, never a closed roster.
pub struct ChaseOracle<'a> {
    /// Fixed assumptions; never proposed as edits.
    pub background: &'a Declaration,
    /// Exact declaration grouping in source order.
    pub declarations: &'a [Declaration],
    /// Per-chase limits, including its internal minimization work.
    pub budget: ChaseBudget,
}

impl Oracle for ChaseOracle<'_> {
    type Unknown = ChaseError;
    fn check(&mut self, active: &BTreeSet<usize>) -> Verdict<ChaseError> {
        let mut theory = self.background.theory.clone();
        let mut facts = self.background.facts.clone();
        for index in active {
            let Some(declaration) = self.declarations.get(*index) else {
                return Verdict::Unknown(ChaseError::Internal("unknown declaration".into()));
            };
            theory
                .dependencies
                .extend(declaration.theory.dependencies.clone());
            facts.extend(declaration.facts.clone());
        }
        match chase(&theory, &facts, self.budget) {
            Ok(ChaseOutcome::Model(_)) => Verdict::Sat,
            Ok(ChaseOutcome::Conflict(_)) => Verdict::Unsat,
            Err(error) => Verdict::Unknown(error),
        }
    }
}
