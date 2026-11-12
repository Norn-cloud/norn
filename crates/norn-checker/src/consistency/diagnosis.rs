//! DIAG-CORE-1 / DIAG-REPAIR-1: irreducible conflicts and editable-only
//! correction proposals. This analysis grants no admission or proof authority.
use std::collections::{BTreeMap, BTreeSet, VecDeque};

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
    /// Complete only after the entire hitting-set tree finishes. Exhaustion
    /// returns unknown, never partial core/correction lists.
    /// These are relaxation proposals; exact edits require ordinary admission.
    pub corrections: BTreeSet<BTreeSet<usize>>,
    /// Actual oracle queries (including independent final checks).
    pub calls: usize,
    /// Configured whole-search query ceiling; disclose with the oracle bounds.
    pub max_calls: usize,
}

/// Refuse an independent validation batch that cannot fit in the remaining budget.
pub(crate) fn require_validation_budget<E>(
    cores: &BTreeSet<BTreeSet<usize>>,
    corrections: &BTreeSet<BTreeSet<usize>>,
    remaining: usize,
) -> Result<(), Unknown<E>> {
    let required = cores
        .iter()
        .chain(corrections)
        .try_fold(0_usize, |sum, set| {
            sum.checked_add(set.len())?.checked_add(1)
        });
    if required.is_none_or(|count| count > remaining) {
        return Err(Unknown::Budget);
    }
    Ok(())
}

/// DIAG-CORE-1: `core` is unsatisfiable and dropping any one member makes it
/// satisfiable. Search proposes cores; only checked ones are returned.
fn verify_core<E>(
    unsat: &mut impl FnMut(&BTreeSet<usize>) -> Result<bool, Unknown<E>>,
    core: &BTreeSet<usize>,
) -> Result<(), Unknown<E>> {
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
    Ok(())
}

/// DIAG-REPAIR-1: removing `repair` restores satisfiability and restoring any
/// one member breaks it again.
fn verify_correction<E>(
    unsat: &mut impl FnMut(&BTreeSet<usize>) -> Result<bool, Unknown<E>>,
    all: &BTreeSet<usize>,
    repair: &BTreeSet<usize>,
) -> Result<(), Unknown<E>> {
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
    Ok(())
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
    let calls = std::cell::Cell::new(0_usize);
    let mut unsat = |active: &BTreeSet<usize>| {
        if calls.get() >= max_calls {
            return Err(Unknown::Budget);
        }
        calls.set(calls.get().saturating_add(1));
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
            if active.len() > max_calls.saturating_sub(calls.get()) {
                return Err(Unknown::Budget);
            }
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
            if removed.len() > max_calls.saturating_sub(calls.get()) {
                return Err(Unknown::Budget);
            }
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
    require_validation_budget(&cores, &corrections, max_calls.saturating_sub(calls.get()))?;
    for core in &cores {
        verify_core(&mut unsat, core)?;
    }
    for repair in &corrections {
        verify_correction(&mut unsat, &all, repair)?;
    }
    Ok(Diagnosis {
        cores,
        corrections,
        calls: calls.get(),
        max_calls,
    })
}

/// Declaration removal can erase a priority path, so norm precedence's oracle
/// is not monotone. Enumerate subsets rather than pruning by monotone hitting sets.
///
/// Norm precedence's explainer (U3, #718): it runs only after the proved
/// decision core has refused the whole set, and grants no admission authority.
pub(crate) fn diagnose_subsets(
    oracle: &mut impl Oracle<Unknown = std::convert::Infallible>,
    declarations: usize,
    editable: &BTreeSet<usize>,
    max_calls: usize,
) -> Result<Diagnosis, Unknown<std::convert::Infallible>> {
    let all: BTreeSet<_> = (0..declarations).collect();
    let calls = std::cell::Cell::new(0_usize);
    let mut unsat = |active: &BTreeSet<usize>| {
        if calls.get() >= max_calls {
            return Err(Unknown::Budget);
        }
        calls.set(calls.get().saturating_add(1));
        Ok(matches!(oracle.check(active), Verdict::Unsat))
    };
    let mut cores = BTreeSet::new();
    let mut corrections = BTreeSet::new();
    if unsat(&all)? {
        // Non-monotone diagnosis visits every subset, including inherited rules.
        // The full-set query is included in that count. At least one minimal
        // core must also be independently revalidated after enumeration.
        let minimum_calls = u32::try_from(declarations)
            .ok()
            .and_then(|n| 1_usize.checked_shl(n))
            .and_then(|count| count.checked_add(1));
        if minimum_calls.is_none_or(|count| count > max_calls) {
            return Err(Unknown::Budget);
        }
        let mut pending = vec![all.clone()];
        let mut checked = BTreeMap::from([(all.clone(), true)]);
        while let Some(active) = pending.pop() {
            for member in &active {
                let mut subset = active.clone();
                subset.remove(member);
                if let std::collections::btree_map::Entry::Vacant(entry) =
                    checked.entry(subset.clone())
                {
                    entry.insert(unsat(&subset)?);
                    pending.push(subset);
                }
            }
        }
        for (active, conflict) in &checked {
            if *conflict {
                if !checked
                    .iter()
                    .any(|(subset, bad)| *bad && subset != active && subset.is_subset(active))
                {
                    cores.insert(active.clone());
                }
            } else {
                let repair: BTreeSet<_> = all.difference(active).copied().collect();
                if repair.is_subset(editable)
                    && !checked
                        .iter()
                        .any(|(kept, bad)| !bad && kept != active && active.is_subset(kept))
                {
                    corrections.insert(repair);
                }
            }
        }
    } else {
        corrections.insert(BTreeSet::new());
    }
    // Independent checks consume the same ceiling as subset recomputation.
    require_validation_budget(&cores, &corrections, max_calls.saturating_sub(calls.get()))?;
    for core in &cores {
        verify_core(&mut unsat, core)?;
    }
    for repair in &corrections {
        verify_correction(&mut unsat, &all, repair)?;
    }
    Ok(Diagnosis {
        cores,
        corrections,
        calls: calls.get(),
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

#[cfg(test)]
mod subset_tests {
    use super::*;
    use std::collections::BTreeSet;

    struct CountingOracle {
        calls: usize,
    }

    impl Oracle for CountingOracle {
        type Unknown = std::convert::Infallible;

        fn check(&mut self, active: &BTreeSet<usize>) -> Verdict<Self::Unknown> {
            use Verdict;
            self.calls = self.calls.saturating_add(1);
            if active.contains(&1) && (active.contains(&0) || active.contains(&2)) {
                Verdict::Unsat
            } else {
                Verdict::Sat
            }
        }
    }

    #[test]
    fn impossible_subset_search_stops_after_full_set_query() {
        for declarations in [12, 13, 32, 64, 65] {
            let mut oracle = CountingOracle { calls: 0 };
            assert_eq!(
                diagnose_subsets(&mut oracle, declarations, &BTreeSet::new(), 4096),
                Err(Unknown::Budget)
            );
            assert_eq!(oracle.calls, 1);
        }
    }

    struct SatCountingOracle {
        calls: usize,
    }

    impl Oracle for SatCountingOracle {
        type Unknown = std::convert::Infallible;

        fn check(&mut self, _active: &BTreeSet<usize>) -> Verdict<Self::Unknown> {
            self.calls = self.calls.saturating_add(1);
            Verdict::Sat
        }
    }

    #[test]
    fn satisfiable_sets_keep_two_call_fast_path_regardless_of_subset_count() {
        for declarations in [13, 64, 65] {
            let mut oracle = SatCountingOracle { calls: 0 };
            let result = diagnose_subsets(&mut oracle, declarations, &BTreeSet::new(), 4096)
                .expect("SAT needs no subset enumeration");
            assert!(result.cores.is_empty());
            assert_eq!(result.corrections, BTreeSet::from([BTreeSet::new()]));
            assert_eq!(result.calls, 2);
            assert_eq!(oracle.calls, 2);
            let mut oracle = SatCountingOracle { calls: 0 };
            assert_eq!(
                diagnose_subsets(&mut oracle, declarations, &BTreeSet::new(), 2)
                    .expect("exact SAT budget")
                    .calls,
                2
            );
            assert_eq!(oracle.calls, 2);
            let mut oracle = SatCountingOracle { calls: 0 };
            assert_eq!(
                diagnose_subsets(&mut oracle, declarations, &BTreeSet::new(), 1),
                Err(Unknown::Budget)
            );
            assert_eq!(oracle.calls, 1);
        }
    }

    #[test]
    fn twelve_declarations_search_and_recheck_overlapping_cores() {
        let editable = (0..12).collect();
        let mut oracle = CountingOracle { calls: 0 };
        // 4096 subsets, two three-query core checks, two repair/restoration
        // checks (two and three queries): the exact complete budget is 4107.
        let result = diagnose_subsets(&mut oracle, 12, &editable, 4107).expect("fits");
        assert_eq!(result.calls, 4107);
        assert_eq!(oracle.calls, result.calls);
        assert_eq!(
            result.cores,
            BTreeSet::from([BTreeSet::from([0, 1]), BTreeSet::from([1, 2])])
        );
        assert_eq!(
            result.corrections,
            BTreeSet::from([BTreeSet::from([1]), BTreeSet::from([0, 2])])
        );
        let mut oracle = CountingOracle { calls: 0 };
        assert_eq!(
            diagnose_subsets(&mut oracle, 12, &editable, result.calls - 1),
            Err(Unknown::Budget)
        );
        // Refuse the known-incomplete validation batch before querying it.
        assert_eq!(oracle.calls, 4096);
    }
}
