//! Shape of a law's declared exception tree, and the tree fold over already
//! evaluated conditions. Structural only: expression evaluation, budgets and
//! commit-path semantics stay with their callers.
//!
//! The decisions live in `shape.rs`, over abstract identities, where they are
//! proved against the Lean semantics (`verification/semantics-v1`, #718 U1).
//! This module supplies the string-level declaration checks and identities.
use norn_ir::LawException;
use std::collections::BTreeSet;

mod shape;

pub use shape::{ExceptionTreeRefusal, MAX_LAW_EXCEPTIONS, codes};

/// Check that `exceptions` form one tree rooted at `law_id`: bounded, unique
/// nonblank ids distinct from the law, nonblank rationales, valid sources,
/// every `overrides` target present, and no cycle. Overlap between siblings is
/// not checked: lift-only siblings always agree.
///
/// # Errors
/// The first refusal in declaration order.
pub fn check_tree(law_id: &str, exceptions: &[LawException]) -> Result<(), ExceptionTreeRefusal> {
    let ids: Vec<&str> = exceptions.iter().map(|e| e.id.as_str()).collect();
    let parents: Vec<&str> = exceptions.iter().map(|e| e.overrides.as_str()).collect();
    let declared: Vec<bool> = exceptions
        .iter()
        .map(|e| {
            !e.id.trim().is_empty()
                && !e.rationale.trim().is_empty()
                && crate::deontic::valid_source(&e.source)
        })
        .collect();
    shape::check_shape(&law_id, &ids, &parents, &declared)
}

/// Fold an admitted tree over its evaluated conditions (`matched[i]` is the
/// value of `exceptions[i].condition`). An exception applies when its
/// condition holds and no exception overriding it applies. Returns whether the
/// law is lifted (some exception overriding the law applies) and the ids of
/// the applied exceptions whose ancestors' conditions all hold, sorted.
/// Declaration order never affects the result.
///
/// The caller must have passed [`check_tree`]; a `matched` of another length
/// treats the missing conditions as false.
#[must_use]
pub fn fold<'a>(
    law_id: &str,
    exceptions: &'a [LawException],
    matched: &[bool],
) -> (bool, BTreeSet<&'a str>) {
    let ids: Vec<&str> = exceptions.iter().map(|e| e.id.as_str()).collect();
    let parents: Vec<&str> = exceptions.iter().map(|e| e.overrides.as_str()).collect();
    let mut level = vec![false; exceptions.len()];
    let mut next = vec![false; exceptions.len()];
    let lifted = shape::lifted(&law_id, &ids, &parents, matched, &mut level, &mut next);
    let mut applied = vec![false; exceptions.len()];
    shape::applied_exceptions(&law_id, &ids, &parents, matched, &level, &mut applied);
    let applied = exceptions
        .iter()
        .zip(applied)
        .filter_map(|(exception, applied)| applied.then_some(exception.id.as_str()))
        .collect();
    (lifted, applied)
}

#[cfg(test)]
mod tests {
    use super::shape;
    use proptest::prelude::*;
    use std::collections::{BTreeMap, BTreeSet};

    /// The recursive fold this module ran before the level fold (#718 U1),
    /// kept as the differential oracle.
    fn legacy_fold<'a>(
        law: &str,
        ids: &[&'a str],
        parents: &[&'a str],
        matched: &[bool],
    ) -> (bool, BTreeSet<&'a str>) {
        let mut children: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
        for (index, parent) in parents.iter().enumerate() {
            children.entry(*parent).or_default().push(index);
        }
        let mut applied = BTreeSet::new();
        let lifted = legacy_lifts(law, ids, matched, &children, &mut applied, 0);
        (lifted, applied)
    }

    fn legacy_lifts<'a>(
        node: &str,
        ids: &[&'a str],
        matched: &[bool],
        children: &BTreeMap<&str, Vec<usize>>,
        applied: &mut BTreeSet<&'a str>,
        depth: usize,
    ) -> bool {
        let mut any = false;
        if depth > ids.len() {
            return any;
        }
        for &index in children.get(node).into_iter().flatten() {
            let Some(id) = ids.get(index) else {
                continue;
            };
            if !matched.get(index).copied().unwrap_or(false) {
                continue;
            }
            let overridden =
                legacy_lifts(id, ids, matched, children, applied, depth.saturating_add(1));
            if !overridden {
                applied.insert(*id);
                any = true;
            }
        }
        any
    }

    fn level_fold<'a>(
        law: &str,
        ids: &[&'a str],
        parents: &[&'a str],
        matched: &[bool],
    ) -> (bool, BTreeSet<&'a str>) {
        let mut level = vec![false; ids.len()];
        let mut next = vec![false; ids.len()];
        let lifted = shape::lifted(&law, ids, parents, matched, &mut level, &mut next);
        let mut applied = vec![false; ids.len()];
        shape::applied_exceptions(&law, ids, parents, matched, &level, &mut applied);
        let applied = ids
            .iter()
            .zip(applied)
            .filter_map(|(id, applied)| applied.then_some(*id))
            .collect();
        (lifted, applied)
    }

    const NAMES: [&str; 6] = ["law", "a", "b", "c", "d", "e"];

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(4096))]

        /// `lifted` equals the recursive fold on every input, including
        /// cycles, duplicates, dangling targets and short `matched`; the
        /// applied set equals it on every admitted tree.
        #[test]
        fn level_fold_matches_the_recursive_fold(
            tree in proptest::collection::vec(
                (proptest::sample::select(&NAMES[..]), proptest::sample::select(&NAMES[..])),
                0..=6,
            ),
            matched in proptest::collection::vec(any::<bool>(), 0..=7),
        ) {
            let ids: Vec<&str> = tree.iter().map(|(id, _)| *id).collect();
            let parents: Vec<&str> = tree.iter().map(|(_, parent)| *parent).collect();
            let (legacy_lifted, legacy_applied) = legacy_fold("law", &ids, &parents, &matched);
            let (lifted, applied) = level_fold("law", &ids, &parents, &matched);
            prop_assert_eq!(lifted, legacy_lifted);
            let declared = vec![true; ids.len()];
            if shape::check_shape(&"law", &ids, &parents, &declared).is_ok() {
                prop_assert_eq!(applied, legacy_applied);
            }
        }
    }
}
