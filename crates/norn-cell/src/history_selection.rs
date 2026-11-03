//! ASOF-KNOWLEDGE-1: select the newest incarnation eligible at a knowledge cutoff.
/// Selection is independent of arrival order; future knowledge cannot replace it.
pub(crate) const fn select_known_version(
    cutoff: u64,
    selected: Option<u64>,
    version: u64,
    recorded_at: u64,
) -> Option<u64> {
    if recorded_at <= cutoff {
        match selected {
            Some(prior) if prior >= version => Some(prior),
            _ => Some(version),
        }
    } else {
        selected
    }
}

/// ASOF-KNOWLEDGE-1: event-free commits retain the preceding ledger sequence.
pub(crate) const fn receipt_last_sequence(first: u64, count: u64) -> Option<u64> {
    match first.checked_sub(1) {
        Some(prior) => prior.checked_add(count),
        None => None,
    }
}

/// RESTATE-HISTORY-1: a private generation advances at most one source receipt.
pub(crate) const fn next_known_slice(
    old_frontier: u64,
    old_clock: u64,
    new_frontier: u64,
    new_clock: u64,
) -> bool {
    match new_frontier.checked_sub(old_frontier) {
        Some(delta) => delta <= 1 && new_clock >= old_clock,
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{next_known_slice, receipt_last_sequence, select_known_version};
    use norn_testkit::proptest::prelude::*;
    proptest! {
        #[test]
        fn next_slice_matches_a_wide_no_rewind_no_gap_model(a in any::<u64>(),ka in any::<u64>(),b in any::<u64>(),kb in any::<u64>()) {
            prop_assert_eq!(next_known_slice(a,ka,b,kb),b>=a && u128::from(b)<=u128::from(a)+1 && kb>=ka);
        }
        #[test]
        fn receipt_prefix_matches_wide_arithmetic(first in any::<u64>(), count in any::<u64>()) {
            let expected=u128::from(first).checked_sub(1).and_then(|n|n.checked_add(u128::from(count))).and_then(|n|u64::try_from(n).ok());
            prop_assert_eq!(receipt_last_sequence(first,count),expected);
        }
        #[test]
        fn selection_equals_rebuild_and_later_knowledge_preserves_old_answer(
            recorded in prop::collection::vec(any::<u64>(), 0..64), cutoff in 0u64..u64::MAX,
        ) {
            let versions: Vec<_> = (1u64..).zip(recorded).collect();
            let selected = versions.iter().fold(None, |s, &(v,k)| select_known_version(cutoff,s,v,k));
            let rebuilt = versions.iter().filter(|(_,k)| *k<=cutoff).map(|(v,_)| *v).max();
            prop_assert_eq!(selected,rebuilt);
            let reversed = versions.iter().rev().fold(None, |s, &(v,k)| select_known_version(cutoff,s,v,k));
            prop_assert_eq!(selected,reversed);
            prop_assert_eq!(select_known_version(cutoff,selected,u64::MAX,cutoff.saturating_add(1)),selected);
        }
    }
}
