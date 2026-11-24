//! Row and edge selection after canonical identity and CAS have been checked.
//! Slots identify proposed live rows. Unwritten identities retain their snapshot row.

#[derive(Clone, Copy)]
pub(super) enum RowChange {
    Put { slot: usize, version: u64 },
    Delete { version: u64 },
}

pub(super) enum RowSource<'p, T> {
    Before,
    Put { row: &'p T, version: u64 },
    Deleted { tombstone: u64 },
}

pub(super) fn resolve<T>(puts: &[T], change: Option<RowChange>) -> Option<RowSource<'_, T>> {
    match change {
        Some(RowChange::Put { slot, version }) => {
            puts.get(slot).map(|row| RowSource::Put { row, version })
        }
        Some(RowChange::Delete { version }) => Some(RowSource::Deleted { tombstone: version }),
        None => Some(RowSource::Before),
    }
}

/// Delta to a canonical before-image; a redundant add or remove is a no-op.
#[derive(Clone, Copy)]
pub(super) enum EdgeDelta {
    Insert,
    Remove,
    Keep,
}
pub(super) const fn edge_delta(before: bool, add: bool) -> EdgeDelta {
    match (before, add) {
        (false, true) => EdgeDelta::Insert,
        (true, false) => EdgeDelta::Remove,
        (false,false) | (true,true) => EdgeDelta::Keep,
    }
}
