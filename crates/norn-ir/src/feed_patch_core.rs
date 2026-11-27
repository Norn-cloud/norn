//! Standalone pure observation reducer and instant-ordering core (`FEED-MODE-1`,
//! Lane C3 / Issue #1924).
//!
//! This file compiles standalone (`core` only, zero external crate dependencies,
//! zero trait bounds on the reducer functions) so Charon/Aeneas can extract it
//! directly with `--start-from` without pulling `serde`, `schemars`,
//! `std::collections`, or `Clone` trait dictionaries into the proof boundary.
//! Wire serialization, `RuntimeValue` adapters, and `BTreeMap` iteration stay
//! glue in `crate::feed`.

/// Tri-state field patch operation (core representation of `feed::FieldPatch`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatchOp<V> {
    /// Omitted in a partial patch; preserve the prior value.
    Absent,
    /// Explicitly `null`; clear a nullable field to the null sentinel.
    ExplicitNull,
    /// Concrete non-null value; overwrite the field.
    Value(V),
}

/// Tri-state single-valued link patch operation (core representation of
/// `feed::LinkPatchOp`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkOp<T> {
    /// Omitted in a partial patch; preserve the prior link target.
    Absent,
    /// Explicitly `null`; withdraw the link target.
    Clear,
    /// Concrete target key; overwrite the link target.
    Target(T),
}

/// Three-way ordering outcome for typed source versions (standalone `core` enum).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionCmp {
    /// Left version strictly precedes right version.
    Less,
    /// Versions denote the same point in the order.
    Equal,
    /// Left version strictly follows right version.
    Greater,
}

/// Pure field-level observation reducer: applies a single [`PatchOp`] to an
/// optional prior value.
#[must_use]
pub fn reduce_field_core<V>(prior: Option<V>, patch: PatchOp<V>, null_value: V) -> Option<V> {
    match patch {
        PatchOp::Absent => prior,
        PatchOp::ExplicitNull => Some(null_value),
        PatchOp::Value(value) => Some(value),
    }
}

/// Associative, idempotent monoid composition of two [`PatchOp`] updates in
/// chronological order (`earlier` followed by `later`), with identity
/// [`PatchOp::Absent`].
#[must_use]
pub fn compose_field_core<V>(earlier: PatchOp<V>, later: PatchOp<V>) -> PatchOp<V> {
    match later {
        PatchOp::Absent => earlier,
        PatchOp::ExplicitNull => PatchOp::ExplicitNull,
        PatchOp::Value(value) => PatchOp::Value(value),
    }
}

/// Pure link-level observation reducer: applies a single [`LinkOp`] to an
/// optional prior target.
#[must_use]
pub fn reduce_link_core<T>(prior: Option<T>, op: LinkOp<T>) -> Option<T> {
    match op {
        LinkOp::Absent => prior,
        LinkOp::Clear => None,
        LinkOp::Target(target) => Some(target),
    }
}

/// Associative, idempotent monoid composition of two [`LinkOp`] updates in
/// chronological order (`earlier` followed by `later`), with identity
/// [`LinkOp::Absent`].
#[must_use]
pub fn compose_link_core<T>(earlier: LinkOp<T>, later: LinkOp<T>) -> LinkOp<T> {
    match later {
        LinkOp::Absent => earlier,
        LinkOp::Clear => LinkOp::Clear,
        LinkOp::Target(target) => LinkOp::Target(target),
    }
}

/// Pure lexicographic comparison of two normalized UTC instants
/// `(epoch_seconds, nanos)` with `0 <= nanos < 1_000_000_000`.
#[must_use]
pub const fn compare_instant_core(sa: i64, na: u32, sb: i64, nb: u32) -> VersionCmp {
    if sa != sb {
        if sa < sb {
            VersionCmp::Less
        } else {
            VersionCmp::Greater
        }
    } else if na != nb {
        if na < nb {
            VersionCmp::Less
        } else {
            VersionCmp::Greater
        }
    } else {
        VersionCmp::Equal
    }
}
