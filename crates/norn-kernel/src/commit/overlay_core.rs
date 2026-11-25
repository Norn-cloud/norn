//! Row and edge selection after canonical identity and CAS have been checked.
//! Slots identify proposed live rows. Unwritten identities retain their snapshot row.
//! Verus checks the laws below on this file (`scripts/verify-kernel-overlay-verus.sh`).

#![deny(clippy::default_numeric_fallback)]

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

#[derive(Clone, Copy)]
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub(crate) enum RowChange {
    Put { slot: usize, version: u64 },
    Delete { version: u64 },
}

#[cfg_attr(verus_keep_ghost, verus_verify)]
pub(crate) enum RowSource<'p, T> {
    Before,
    Put { row: &'p T, version: u64 },
    Deleted { tombstone: u64 },
}

/// Delta to a canonical before-image; a redundant add or remove is a no-op.
#[derive(Clone, Copy)]
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub(crate) enum EdgeDelta {
    Insert,
    Remove,
    Keep,
}

#[cfg(verus_keep_ghost)]
verus! {
    /// The row a sealed overlay reports for one canonical identity. `change` is the
    /// identity's admitted write, if any; `puts` holds the proposed live rows by slot.
    spec fn row_after<'p, T>(puts: Seq<T>, change: Option<RowChange>, r: Option<RowSource<'p, T>>) -> bool {
        match change {
            Some(RowChange::Put { slot, version }) => if slot < puts.len() {
                r matches Some(RowSource::Put { row, version: v }) && *row == puts[slot as int] && v == version
            } else {
                r is None
            },
            Some(RowChange::Delete { version }) => r == Some(RowSource::<'p, T>::Deleted { tombstone: version }),
            None => r == Some(RowSource::<'p, T>::Before),
        }
    }

    /// Canonical delta: only a change of membership is a delta.
    spec fn delta(before: bool, add: bool) -> EdgeDelta {
        if before == add { EdgeDelta::Keep } else if add { EdgeDelta::Insert } else { EdgeDelta::Remove }
    }

    spec fn apply(member: bool, d: EdgeDelta) -> bool {
        match d {
            EdgeDelta::Insert => true,
            EdgeDelta::Remove => false,
            EdgeDelta::Keep => member,
        }
    }

    /// Membership of one canonical edge after folding the batch's changes in input order.
    spec fn fold(before: bool, adds: Seq<bool>) -> bool
        decreases adds.len(),
    {
        if adds.len() == 0 { before } else { fold(apply(before, delta(before, adds[0])), adds.drop_first()) }
    }

    /// An unwritten identity keeps its snapshot row (overlay frame).
    proof fn overlay_frame<'p, T>(puts: Seq<T>, r: Option<RowSource<'p, T>>)
        requires row_after(puts, None, r),
        ensures r == Some(RowSource::<'p, T>::Before),
    {}

    /// A written slot is never answered from the pre-state (Lane 2a witness 1).
    proof fn written_never_before<'p, T>(puts: Seq<T>, change: RowChange, r: Option<RowSource<'p, T>>)
        requires row_after(puts, Some(change), r),
        ensures !(r matches Some(RowSource::Before)),
    {}

    /// A deleted identity has no live row: its values are never read (K8, witness 2).
    proof fn deletion_excludes<'p, T>(puts: Seq<T>, version: u64, r: Option<RowSource<'p, T>>)
        requires row_after(puts, Some(RowChange::Delete { version }), r),
        ensures
            !(r matches Some(RowSource::Put { .. })),
            !(r matches Some(RowSource::Before)),
    {}

    /// Two reads of one sealed overlay agree.
    proof fn row_after_functional<'p, T>(
        puts: Seq<T>, change: Option<RowChange>, a: Option<RowSource<'p, T>>, b: Option<RowSource<'p, T>>,
    )
        requires row_after(puts, change, a), row_after(puts, change, b),
        ensures a == b,
    {}

    /// Applying a delta yields exactly the requested membership.
    proof fn edge_apply_exact(before: bool, add: bool)
        ensures apply(before, delta(before, add)) == add,
    {}

    /// A delta is a no-op once applied (idempotence) and never redundant.
    proof fn edge_idempotent(before: bool, add: bool)
        ensures
            delta(apply(before, delta(before, add)), add) == EdgeDelta::Keep,
            delta(before, add) != EdgeDelta::Keep ==> before != add,
    {}

    /// Folding a nonempty change list leaves the last requested membership, whatever
    /// the before-image; an empty list leaves the before-image.
    proof fn edge_fold_exact(before: bool, adds: Seq<bool>)
        ensures fold(before, adds) == if adds.len() == 0 { before } else { adds.last() },
        decreases adds.len(),
    {
        if adds.len() > 0 {
            edge_fold_exact(apply(before, delta(before, adds[0])), adds.drop_first());
            if adds.len() > 1 {
                assert(adds.drop_first().last() == adds.last());
            }
        }
    }
}

#[cfg_attr(verus_keep_ghost, verus_spec(r => ensures
    #![verifier::proof_note("resolve")]
    row_after(puts@, change, r),
))]
#[allow(
    clippy::manual_map,
    reason = "Verus checks this match; `Option::map` would hide the row behind a closure"
)]
pub(crate) fn resolve<T>(puts: &[T], change: Option<RowChange>) -> Option<RowSource<'_, T>> {
    match change {
        Some(RowChange::Put { slot, version }) => match puts.get(slot) {
            Some(row) => Some(RowSource::Put { row, version }),
            None => None,
        },
        Some(RowChange::Delete { version }) => Some(RowSource::Deleted { tombstone: version }),
        None => Some(RowSource::Before),
    }
}

#[cfg_attr(verus_keep_ghost, verus_spec(r => ensures
    #![verifier::proof_note("edge_delta")]
    r == delta(before, add),
))]
pub(crate) const fn edge_delta(before: bool, add: bool) -> EdgeDelta {
    match (before, add) {
        (false, true) => EdgeDelta::Insert,
        (true, false) => EdgeDelta::Remove,
        (false, false) | (true, true) => EdgeDelta::Keep,
    }
}
