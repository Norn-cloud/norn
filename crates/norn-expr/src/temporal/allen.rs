//! Allen's thirteen interval relations over proper half-open intervals.
//!
//! An interval is `[start, end)` with `start < end` (Core 01 temporal
//! context): the start instant belongs to it and the end instant does not.
//! The relations are pure order algebra over already resolved instants. No
//! clock, calendar or timezone is consulted, so the same code classifies
//! canonical UTC ordering keys, dates or integers.
//!
//! [`AllenRelation::holds`] states each relation by its endpoint conditions,
//! and [`allen_relation`] classifies a pair by comparing endpoints. Both
//! delegate directly to the inline-Verus-verified pure core in `allen_core.rs`
//! (`scripts/verify-owl-time-inline-verus.sh`), paired with the Lean reference
//! algebra in `verification/owl-time-v1/AllenIntervals.lean`.

use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};

pub use super::allen_core::AllenRelation;
use super::allen_core::{self, EndpointOrder};

impl fmt::Debug for AllenRelation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Before => "Before",
            Self::Meets => "Meets",
            Self::Overlaps => "Overlaps",
            Self::Starts => "Starts",
            Self::During => "During",
            Self::Finishes => "Finishes",
            Self::Equals => "Equals",
            Self::FinishedBy => "FinishedBy",
            Self::Contains => "Contains",
            Self::StartedBy => "StartedBy",
            Self::OverlappedBy => "OverlappedBy",
            Self::MetBy => "MetBy",
            Self::After => "After",
        })
    }
}

impl PartialEq for AllenRelation {
    fn eq(&self, other: &Self) -> bool {
        core::mem::discriminant(self) == core::mem::discriminant(other)
    }
}

impl Eq for AllenRelation {}

impl Hash for AllenRelation {
    fn hash<H: Hasher>(&self, state: &mut H) {
        core::mem::discriminant(self).hash(state);
    }
}

const fn endpoint_order(ordering: Ordering) -> EndpointOrder {
    match ordering {
        Ordering::Less => EndpointOrder::Less,
        Ordering::Equal => EndpointOrder::Equal,
        Ordering::Greater => EndpointOrder::Greater,
    }
}

impl AllenRelation {
    /// All thirteen relations, each listed once.
    pub const ALL: [Self; 13] = [
        Self::Before,
        Self::Meets,
        Self::Overlaps,
        Self::Starts,
        Self::During,
        Self::Finishes,
        Self::Equals,
        Self::FinishedBy,
        Self::Contains,
        Self::StartedBy,
        Self::OverlappedBy,
        Self::MetBy,
        Self::After,
    ];

    /// The relation that holds from the second interval to the first exactly
    /// when `self` holds from the first to the second.
    #[must_use]
    pub const fn converse(self) -> Self {
        allen_core::converse(self)
    }

    /// Whether two intervals in this relation have an instant in common.
    ///
    /// False exactly for before, meets, met-by and after: adjacent half-open
    /// intervals `[a, b)` and `[b, c)` meet without sharing `b`.
    #[must_use]
    pub const fn shares_instant(self) -> bool {
        allen_core::shares_instant(self)
    }

    /// Whether this relation holds from `[start, end)` to
    /// `[other_start, other_end)`.
    ///
    /// Returns `false` when either interval is improper (`start >= end`).
    #[must_use]
    pub fn holds<T: Ord>(self, start: &T, end: &T, other_start: &T, other_end: &T) -> bool {
        allen_core::holds(
            self,
            endpoint_order(start.cmp(end)),
            endpoint_order(other_start.cmp(other_end)),
            endpoint_order(start.cmp(other_start)),
            endpoint_order(end.cmp(other_end)),
            endpoint_order(end.cmp(other_start)),
            endpoint_order(other_end.cmp(start)),
        )
    }
}

/// The one Allen relation from `[start, end)` to `[other_start, other_end)`.
///
/// Returns `None` when either interval is improper (`start >= end`), as the
/// `norn-zen/7` interval builtins refuse such intervals.
#[must_use]
pub fn allen_relation<T: Ord>(
    start: &T,
    end: &T,
    other_start: &T,
    other_end: &T,
) -> Option<AllenRelation> {
    allen_core::classify(
        endpoint_order(start.cmp(end)),
        endpoint_order(other_start.cmp(other_end)),
        endpoint_order(start.cmp(other_start)),
        endpoint_order(end.cmp(other_end)),
        endpoint_order(end.cmp(other_start)),
        endpoint_order(other_end.cmp(start)),
    )
}
