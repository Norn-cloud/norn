//! Allen's thirteen interval relations over proper half-open intervals.
//!
//! An interval is `[start, end)` with `start < end` (Core 01 temporal
//! context): the start instant belongs to it and the end instant does not.
//! The relations are pure order algebra over already resolved instants. No
//! clock, calendar or timezone is consulted, so the same code classifies
//! canonical UTC ordering keys, dates or integers.
//!
//! [`AllenRelation::holds`] states each relation by its endpoint conditions,
//! and [`allen_relation`] classifies a pair by comparing endpoints. The laws
//! that tie them together (exactly one relation holds, converses swap the
//! intervals, and two intervals share an instant exactly when the relation is
//! none of before, meets, met-by or after) are checked in
//! `verification/owl-time-v1/`.

use std::cmp::Ordering;

/// One of Allen's thirteen relations between two proper half-open intervals.
///
/// Each variant reads "the first interval *relation* the second". The
/// endpoint conditions are those of [`AllenRelation::holds`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AllenRelation {
    /// The first ends before the second starts, with a gap between them.
    Before,
    /// The first ends exactly where the second starts.
    Meets,
    /// The first starts first and ends inside the second.
    Overlaps,
    /// Both start together and the first ends first.
    Starts,
    /// The first starts after and ends before the second.
    During,
    /// Both end together and the first starts later.
    Finishes,
    /// Both start together and end together.
    Equals,
    /// Both end together and the first starts earlier.
    FinishedBy,
    /// The first starts before and ends after the second.
    Contains,
    /// Both start together and the first ends later.
    StartedBy,
    /// The first starts inside the second and ends after it.
    OverlappedBy,
    /// The first starts exactly where the second ends.
    MetBy,
    /// The first starts after the second ends, with a gap between them.
    After,
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
        match self {
            Self::Before => Self::After,
            Self::Meets => Self::MetBy,
            Self::Overlaps => Self::OverlappedBy,
            Self::Starts => Self::StartedBy,
            Self::During => Self::Contains,
            Self::Finishes => Self::FinishedBy,
            Self::Equals => Self::Equals,
            Self::FinishedBy => Self::Finishes,
            Self::Contains => Self::During,
            Self::StartedBy => Self::Starts,
            Self::OverlappedBy => Self::Overlaps,
            Self::MetBy => Self::Meets,
            Self::After => Self::Before,
        }
    }

    /// Whether two intervals in this relation have an instant in common.
    ///
    /// False exactly for before, meets, met-by and after: adjacent half-open
    /// intervals `[a, b)` and `[b, c)` meet without sharing `b`.
    #[must_use]
    pub const fn shares_instant(self) -> bool {
        match self {
            Self::Before | Self::Meets | Self::MetBy | Self::After => false,
            Self::Overlaps
            | Self::Starts
            | Self::During
            | Self::Finishes
            | Self::Equals
            | Self::FinishedBy
            | Self::Contains
            | Self::StartedBy
            | Self::OverlappedBy => true,
        }
    }

    /// Whether this relation holds from `[start, end)` to
    /// `[other_start, other_end)`.
    ///
    /// Returns `false` when either interval is improper (`start >= end`).
    #[must_use]
    pub fn holds<T: Ord>(self, start: &T, end: &T, other_start: &T, other_end: &T) -> bool {
        if start >= end || other_start >= other_end {
            return false;
        }
        match self {
            Self::Before => end < other_start,
            Self::Meets => end == other_start,
            Self::Overlaps => start < other_start && other_start < end && end < other_end,
            Self::Starts => start == other_start && end < other_end,
            Self::During => other_start < start && end < other_end,
            Self::Finishes => other_start < start && end == other_end,
            Self::Equals => start == other_start && end == other_end,
            Self::FinishedBy => start < other_start && end == other_end,
            Self::Contains => start < other_start && other_end < end,
            Self::StartedBy => start == other_start && other_end < end,
            Self::OverlappedBy => other_start < start && start < other_end && other_end < end,
            Self::MetBy => other_end == start,
            Self::After => other_end < start,
        }
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
    if start >= end || other_start >= other_end {
        return None;
    }
    Some(match (start.cmp(other_start), end.cmp(other_end)) {
        (Ordering::Equal, Ordering::Equal) => AllenRelation::Equals,
        (Ordering::Equal, Ordering::Less) => AllenRelation::Starts,
        (Ordering::Equal, Ordering::Greater) => AllenRelation::StartedBy,
        (Ordering::Less, Ordering::Equal) => AllenRelation::FinishedBy,
        (Ordering::Greater, Ordering::Equal) => AllenRelation::Finishes,
        (Ordering::Less, Ordering::Greater) => AllenRelation::Contains,
        (Ordering::Greater, Ordering::Less) => AllenRelation::During,
        (Ordering::Less, Ordering::Less) => match end.cmp(other_start) {
            Ordering::Less => AllenRelation::Before,
            Ordering::Equal => AllenRelation::Meets,
            Ordering::Greater => AllenRelation::Overlaps,
        },
        (Ordering::Greater, Ordering::Greater) => match other_end.cmp(start) {
            Ordering::Less => AllenRelation::After,
            Ordering::Equal => AllenRelation::MetBy,
            Ordering::Greater => AllenRelation::OverlappedBy,
        },
    })
}
