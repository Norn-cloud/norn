//! Pure verified core of Allen's thirteen interval relations over proper
//! half-open intervals `[start, end)` (`start < end`).
//!
//! Verus checks the executable contracts and unbounded integer interval laws
//! directly on this file (`scripts/verify-owl-time-inline-verus.sh`).
//! `allen.rs` wraps these operations for any `T: Ord` and provides trait impls.

#![deny(clippy::default_numeric_fallback)]

#[cfg(verus_keep_ghost)]
use vstd::prelude::*;

/// Pairwise order outcome between two interval endpoints.
#[derive(Clone, Copy)]
#[cfg_attr(verus_keep_ghost, verus_verify)]
pub(crate) enum EndpointOrder {
    Less,
    Equal,
    Greater,
}

/// One of Allen's thirteen relations between two proper half-open intervals.
///
/// Each variant reads "the first interval *relation* the second". The
/// endpoint conditions are those of [`AllenRelation::holds`].
#[derive(Clone, Copy)]
#[cfg_attr(verus_keep_ghost, verus_verify)]
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

#[cfg(verus_keep_ghost)]
verus! {
    spec fn order_int(left: int, right: int) -> EndpointOrder {
        if left < right {
            EndpointOrder::Less
        } else if left == right {
            EndpointOrder::Equal
        } else {
            EndpointOrder::Greater
        }
    }

    spec fn spec_converse(relation: AllenRelation) -> AllenRelation {
        match relation {
            AllenRelation::Before => AllenRelation::After,
            AllenRelation::Meets => AllenRelation::MetBy,
            AllenRelation::Overlaps => AllenRelation::OverlappedBy,
            AllenRelation::Starts => AllenRelation::StartedBy,
            AllenRelation::During => AllenRelation::Contains,
            AllenRelation::Finishes => AllenRelation::FinishedBy,
            AllenRelation::Equals => AllenRelation::Equals,
            AllenRelation::FinishedBy => AllenRelation::Finishes,
            AllenRelation::Contains => AllenRelation::During,
            AllenRelation::StartedBy => AllenRelation::Starts,
            AllenRelation::OverlappedBy => AllenRelation::Overlaps,
            AllenRelation::MetBy => AllenRelation::Meets,
            AllenRelation::After => AllenRelation::Before,
        }
    }

    spec fn spec_shares_instant(relation: AllenRelation) -> bool {
        match relation {
            AllenRelation::Before
            | AllenRelation::Meets
            | AllenRelation::MetBy
            | AllenRelation::After => false,
            AllenRelation::Overlaps
            | AllenRelation::Starts
            | AllenRelation::During
            | AllenRelation::Finishes
            | AllenRelation::Equals
            | AllenRelation::FinishedBy
            | AllenRelation::Contains
            | AllenRelation::StartedBy
            | AllenRelation::OverlappedBy => true,
        }
    }

    spec fn spec_holds_orders(
        relation: AllenRelation,
        start_end: EndpointOrder,
        other_start_end: EndpointOrder,
        start_other_start: EndpointOrder,
        end_other_end: EndpointOrder,
        end_other_start: EndpointOrder,
        other_end_start: EndpointOrder,
    ) -> bool {
        if !(start_end == EndpointOrder::Less && other_start_end == EndpointOrder::Less) {
            false
        } else {
            match relation {
                AllenRelation::Before => end_other_start == EndpointOrder::Less,
                AllenRelation::Meets => end_other_start == EndpointOrder::Equal,
                AllenRelation::Overlaps => {
                    start_other_start == EndpointOrder::Less
                        && end_other_start == EndpointOrder::Greater
                        && end_other_end == EndpointOrder::Less
                }
                AllenRelation::Starts => {
                    start_other_start == EndpointOrder::Equal
                        && end_other_end == EndpointOrder::Less
                }
                AllenRelation::During => {
                    start_other_start == EndpointOrder::Greater
                        && end_other_end == EndpointOrder::Less
                }
                AllenRelation::Finishes => {
                    start_other_start == EndpointOrder::Greater
                        && end_other_end == EndpointOrder::Equal
                }
                AllenRelation::Equals => {
                    start_other_start == EndpointOrder::Equal
                        && end_other_end == EndpointOrder::Equal
                }
                AllenRelation::FinishedBy => {
                    start_other_start == EndpointOrder::Less
                        && end_other_end == EndpointOrder::Equal
                }
                AllenRelation::Contains => {
                    start_other_start == EndpointOrder::Less
                        && end_other_end == EndpointOrder::Greater
                }
                AllenRelation::StartedBy => {
                    start_other_start == EndpointOrder::Equal
                        && end_other_end == EndpointOrder::Greater
                }
                AllenRelation::OverlappedBy => {
                    start_other_start == EndpointOrder::Greater
                        && other_end_start == EndpointOrder::Greater
                        && end_other_end == EndpointOrder::Greater
                }
                AllenRelation::MetBy => other_end_start == EndpointOrder::Equal,
                AllenRelation::After => other_end_start == EndpointOrder::Less,
            }
        }
    }

    spec fn spec_classify_orders(
        start_end: EndpointOrder,
        other_start_end: EndpointOrder,
        start_other_start: EndpointOrder,
        end_other_end: EndpointOrder,
        end_other_start: EndpointOrder,
        other_end_start: EndpointOrder,
    ) -> Option<AllenRelation> {
        if !(start_end == EndpointOrder::Less && other_start_end == EndpointOrder::Less) {
            None
        } else {
            Some(match (start_other_start, end_other_end) {
                (EndpointOrder::Equal, EndpointOrder::Equal) => AllenRelation::Equals,
                (EndpointOrder::Equal, EndpointOrder::Less) => AllenRelation::Starts,
                (EndpointOrder::Equal, EndpointOrder::Greater) => AllenRelation::StartedBy,
                (EndpointOrder::Less, EndpointOrder::Equal) => AllenRelation::FinishedBy,
                (EndpointOrder::Greater, EndpointOrder::Equal) => AllenRelation::Finishes,
                (EndpointOrder::Less, EndpointOrder::Greater) => AllenRelation::Contains,
                (EndpointOrder::Greater, EndpointOrder::Less) => AllenRelation::During,
                (EndpointOrder::Less, EndpointOrder::Less) => match end_other_start {
                    EndpointOrder::Less => AllenRelation::Before,
                    EndpointOrder::Equal => AllenRelation::Meets,
                    EndpointOrder::Greater => AllenRelation::Overlaps,
                },
                (EndpointOrder::Greater, EndpointOrder::Greater) => match other_end_start {
                    EndpointOrder::Less => AllenRelation::After,
                    EndpointOrder::Equal => AllenRelation::MetBy,
                    EndpointOrder::Greater => AllenRelation::OverlappedBy,
                },
            })
        }
    }

    spec fn spec_holds_int(
        relation: AllenRelation,
        start: int,
        end: int,
        other_start: int,
        other_end: int,
    ) -> bool {
        spec_holds_orders(
            relation,
            order_int(start, end),
            order_int(other_start, other_end),
            order_int(start, other_start),
            order_int(end, other_end),
            order_int(end, other_start),
            order_int(other_end, start),
        )
    }

    spec fn spec_classify_int(
        start: int,
        end: int,
        other_start: int,
        other_end: int,
    ) -> Option<AllenRelation> {
        spec_classify_orders(
            order_int(start, end),
            order_int(other_start, other_end),
            order_int(start, other_start),
            order_int(end, other_end),
            order_int(end, other_start),
            order_int(other_end, start),
        )
    }

    spec fn in_interval(instant: int, start: int, end: int) -> bool {
        start <= instant && instant < end
    }

    spec fn later_start(a: int, b: int) -> int {
        if a >= b { a } else { b }
    }

    /// Jointly exhaustive and classifier-consistent: for every pair of proper
    /// intervals, `classify` returns `Some(r)` and `holds(s)` is true iff `s == r`.
    proof fn exactly_one_relation_holds(
        relation: AllenRelation,
        start: int,
        end: int,
        other_start: int,
        other_end: int,
    )
        requires
            start < end,
            other_start < other_end,
        ensures
            spec_classify_int(start, end, other_start, other_end).is_some(),
            spec_holds_int(relation, start, end, other_start, other_end)
                == (spec_classify_int(start, end, other_start, other_end) == Some(relation)),
    {}

    /// Pairwise disjoint across all 13 relations: two holding relations are equal.
    proof fn pairwise_disjoint(
        first: AllenRelation,
        second: AllenRelation,
        start: int,
        end: int,
        other_start: int,
        other_end: int,
    )
        requires
            start < end,
            other_start < other_end,
            spec_holds_int(first, start, end, other_start, other_end),
            spec_holds_int(second, start, end, other_start, other_end),
        ensures
            first == second,
    {
        exactly_one_relation_holds(first, start, end, other_start, other_end);
        exactly_one_relation_holds(second, start, end, other_start, other_end);
    }

    /// Converse involution and predicate symmetry on swapped intervals.
    proof fn converse_holds_iff(
        relation: AllenRelation,
        start: int,
        end: int,
        other_start: int,
        other_end: int,
    )
        requires
            start < end,
            other_start < other_end,
        ensures
            spec_converse(spec_converse(relation)) == relation,
            spec_holds_int(relation, start, end, other_start, other_end)
                == spec_holds_int(spec_converse(relation), other_start, other_end, start, end),
    {}

    /// Converse symmetry and involution: swapping proper intervals maps the
    /// classification to its converse.
    proof fn converse_symmetry_and_involution(
        relation: AllenRelation,
        start: int,
        end: int,
        other_start: int,
        other_end: int,
    )
        requires
            start < end,
            other_start < other_end,
        ensures
            spec_converse(spec_converse(relation)) == relation,
            spec_holds_int(relation, start, end, other_start, other_end)
                == spec_holds_int(spec_converse(relation), other_start, other_end, start, end),
            spec_classify_int(other_start, other_end, start, end)
                == Some(spec_converse(spec_classify_int(start, end, other_start, other_end).unwrap())),
    {
        converse_holds_iff(relation, start, end, other_start, other_end);
        let rel = spec_classify_int(start, end, other_start, other_end).unwrap();
        exactly_one_relation_holds(rel, start, end, other_start, other_end);
        converse_holds_iff(rel, start, end, other_start, other_end);
        exactly_one_relation_holds(spec_converse(rel), other_start, other_end, start, end);
    }

    /// Boundary law: `shares_instant` equals `start < other_end && other_start < end`,
    /// any shared instant implies it, the later start witnesses it iff true, and
    /// adjacent `[a, b)` and `[b, c)` meet without sharing an instant.
    proof fn sharing_an_instant_is_half_open_overlap(
        start: int,
        end: int,
        other_start: int,
        other_end: int,
        instant: int,
    )
        requires
            start < end,
            other_start < other_end,
        ensures
            spec_classify_int(start, end, other_start, other_end).is_some(),
            spec_shares_instant(spec_classify_int(start, end, other_start, other_end).unwrap())
                == (start < other_end && other_start < end),
            (in_interval(instant, start, end) && in_interval(instant, other_start, other_end))
                ==> spec_shares_instant(spec_classify_int(start, end, other_start, other_end).unwrap()),
            (in_interval(later_start(start, other_start), start, end)
                && in_interval(later_start(start, other_start), other_start, other_end))
                == spec_shares_instant(spec_classify_int(start, end, other_start, other_end).unwrap()),
            end == other_start ==> (
                spec_classify_int(start, end, other_start, other_end) == Some(AllenRelation::Meets)
                && !spec_shares_instant(AllenRelation::Meets)
            ),
    {}

    /// Fail closed: an improper interval (`start >= end`) has no relation and satisfies none.
    proof fn improper_intervals_have_no_relation(
        relation: AllenRelation,
        start: int,
        end: int,
        other_start: int,
        other_end: int,
    )
        requires
            start >= end || other_start >= other_end,
        ensures
            spec_classify_int(start, end, other_start, other_end).is_none(),
            !spec_holds_int(relation, start, end, other_start, other_end),
    {}

    /// All 9 scheduling compositions over three proper intervals.
    proof fn scheduling_compositions(
        a_start: int,
        a_end: int,
        b_start: int,
        b_end: int,
        c_start: int,
        c_end: int,
    )
        requires
            a_start < a_end,
            b_start < b_end,
            c_start < c_end,
        ensures
            (spec_classify_int(a_start, a_end, b_start, b_end) == Some(AllenRelation::Before)
                && spec_classify_int(b_start, b_end, c_start, c_end) == Some(AllenRelation::Before))
                ==> spec_classify_int(a_start, a_end, c_start, c_end) == Some(AllenRelation::Before),
            (spec_classify_int(a_start, a_end, b_start, b_end) == Some(AllenRelation::After)
                && spec_classify_int(b_start, b_end, c_start, c_end) == Some(AllenRelation::After))
                ==> spec_classify_int(a_start, a_end, c_start, c_end) == Some(AllenRelation::After),
            (spec_classify_int(a_start, a_end, b_start, b_end) == Some(AllenRelation::During)
                && spec_classify_int(b_start, b_end, c_start, c_end) == Some(AllenRelation::During))
                ==> spec_classify_int(a_start, a_end, c_start, c_end) == Some(AllenRelation::During),
            (spec_classify_int(a_start, a_end, b_start, b_end) == Some(AllenRelation::Contains)
                && spec_classify_int(b_start, b_end, c_start, c_end) == Some(AllenRelation::Contains))
                ==> spec_classify_int(a_start, a_end, c_start, c_end) == Some(AllenRelation::Contains),
            (spec_classify_int(a_start, a_end, b_start, b_end) == Some(AllenRelation::Equals)
                && spec_classify_int(b_start, b_end, c_start, c_end) == Some(AllenRelation::Equals))
                ==> spec_classify_int(a_start, a_end, c_start, c_end) == Some(AllenRelation::Equals),
            (spec_classify_int(a_start, a_end, b_start, b_end) == Some(AllenRelation::Meets)
                && spec_classify_int(b_start, b_end, c_start, c_end) == Some(AllenRelation::Meets))
                ==> spec_classify_int(a_start, a_end, c_start, c_end) == Some(AllenRelation::Before),
            (spec_classify_int(a_start, a_end, b_start, b_end) == Some(AllenRelation::Before)
                && spec_classify_int(b_start, b_end, c_start, c_end) == Some(AllenRelation::Meets))
                ==> spec_classify_int(a_start, a_end, c_start, c_end) == Some(AllenRelation::Before),
            (spec_classify_int(a_start, a_end, b_start, b_end) == Some(AllenRelation::Meets)
                && spec_classify_int(b_start, b_end, c_start, c_end) == Some(AllenRelation::Before))
                ==> spec_classify_int(a_start, a_end, c_start, c_end) == Some(AllenRelation::Before),
            (spec_classify_int(a_start, a_end, b_start, b_end) == Some(AllenRelation::During)
                && spec_classify_int(b_start, b_end, c_start, c_end) == Some(AllenRelation::Before))
                ==> spec_classify_int(a_start, a_end, c_start, c_end) == Some(AllenRelation::Before),
    {}

    /// Concrete reachability witnesses for all 9 scheduling compositions (non-vacuity).
    proof fn scheduling_compositions_reachable()
        ensures
            spec_classify_int(0, 1, 2, 3) == Some(AllenRelation::Before)
                && spec_classify_int(2, 3, 4, 5) == Some(AllenRelation::Before),
            spec_classify_int(4, 5, 2, 3) == Some(AllenRelation::After)
                && spec_classify_int(2, 3, 0, 1) == Some(AllenRelation::After),
            spec_classify_int(2, 3, 1, 4) == Some(AllenRelation::During)
                && spec_classify_int(1, 4, 0, 5) == Some(AllenRelation::During),
            spec_classify_int(0, 5, 1, 4) == Some(AllenRelation::Contains)
                && spec_classify_int(1, 4, 2, 3) == Some(AllenRelation::Contains),
            spec_classify_int(0, 1, 0, 1) == Some(AllenRelation::Equals)
                && spec_classify_int(0, 1, 0, 1) == Some(AllenRelation::Equals),
            spec_classify_int(0, 1, 1, 2) == Some(AllenRelation::Meets)
                && spec_classify_int(1, 2, 2, 3) == Some(AllenRelation::Meets),
            spec_classify_int(0, 1, 2, 3) == Some(AllenRelation::Before)
                && spec_classify_int(2, 3, 3, 4) == Some(AllenRelation::Meets),
            spec_classify_int(0, 1, 1, 2) == Some(AllenRelation::Meets)
                && spec_classify_int(1, 2, 3, 4) == Some(AllenRelation::Before),
            spec_classify_int(1, 2, 0, 3) == Some(AllenRelation::During)
                && spec_classify_int(0, 3, 4, 5) == Some(AllenRelation::Before),
    {}
}

/// Verified converse table.
#[must_use]
#[cfg_attr(verus_keep_ghost, verus_spec(r => ensures
    #![verifier::proof_note("converse")]
    r == spec_converse(relation),
))]
pub(crate) const fn converse(relation: AllenRelation) -> AllenRelation {
    match relation {
        AllenRelation::Before => AllenRelation::After,
        AllenRelation::Meets => AllenRelation::MetBy,
        AllenRelation::Overlaps => AllenRelation::OverlappedBy,
        AllenRelation::Starts => AllenRelation::StartedBy,
        AllenRelation::During => AllenRelation::Contains,
        AllenRelation::Finishes => AllenRelation::FinishedBy,
        AllenRelation::Equals => AllenRelation::Equals,
        AllenRelation::FinishedBy => AllenRelation::Finishes,
        AllenRelation::Contains => AllenRelation::During,
        AllenRelation::StartedBy => AllenRelation::Starts,
        AllenRelation::OverlappedBy => AllenRelation::Overlaps,
        AllenRelation::MetBy => AllenRelation::Meets,
        AllenRelation::After => AllenRelation::Before,
    }
}

/// Verified half-open overlap predicate over Allen relations.
#[must_use]
#[cfg_attr(verus_keep_ghost, verus_spec(r => ensures
    #![verifier::proof_note("shares_instant")]
    r == spec_shares_instant(relation),
))]
pub(crate) const fn shares_instant(relation: AllenRelation) -> bool {
    match relation {
        AllenRelation::Before
        | AllenRelation::Meets
        | AllenRelation::MetBy
        | AllenRelation::After => false,
        AllenRelation::Overlaps
        | AllenRelation::Starts
        | AllenRelation::During
        | AllenRelation::Finishes
        | AllenRelation::Equals
        | AllenRelation::FinishedBy
        | AllenRelation::Contains
        | AllenRelation::StartedBy
        | AllenRelation::OverlappedBy => true,
    }
}

/// Verified endpoint-condition predicate for one Allen relation.
#[must_use]
#[allow(
    clippy::match_like_matches_macro,
    reason = "explicit exhaustive enum matches for Verus VIR/THIR agreement"
)]
#[cfg_attr(verus_keep_ghost, verus_spec(r => ensures
    #![verifier::proof_note("holds")]
    r == spec_holds_orders(
        relation,
        start_end,
        other_start_end,
        start_other_start,
        end_other_end,
        end_other_start,
        other_end_start,
    ),
))]
pub(crate) const fn holds(
    relation: AllenRelation,
    start_end: EndpointOrder,
    other_start_end: EndpointOrder,
    start_other_start: EndpointOrder,
    end_other_end: EndpointOrder,
    end_other_start: EndpointOrder,
    other_end_start: EndpointOrder,
) -> bool {
    match (start_end, other_start_end) {
        (EndpointOrder::Less, EndpointOrder::Less) => match relation {
            AllenRelation::Before => match end_other_start {
                EndpointOrder::Less => true,
                EndpointOrder::Equal | EndpointOrder::Greater => false,
            },
            AllenRelation::Meets => match end_other_start {
                EndpointOrder::Equal => true,
                EndpointOrder::Less | EndpointOrder::Greater => false,
            },
            AllenRelation::Overlaps => match (start_other_start, end_other_start, end_other_end) {
                (EndpointOrder::Less, EndpointOrder::Greater, EndpointOrder::Less) => true,
                (
                    EndpointOrder::Less,
                    EndpointOrder::Greater,
                    EndpointOrder::Equal | EndpointOrder::Greater,
                )
                | (
                    EndpointOrder::Less,
                    EndpointOrder::Less | EndpointOrder::Equal,
                    EndpointOrder::Less | EndpointOrder::Equal | EndpointOrder::Greater,
                )
                | (
                    EndpointOrder::Equal | EndpointOrder::Greater,
                    EndpointOrder::Less | EndpointOrder::Equal | EndpointOrder::Greater,
                    EndpointOrder::Less | EndpointOrder::Equal | EndpointOrder::Greater,
                ) => false,
            },
            AllenRelation::Starts => match (start_other_start, end_other_end) {
                (EndpointOrder::Equal, EndpointOrder::Less) => true,
                (EndpointOrder::Equal, EndpointOrder::Equal | EndpointOrder::Greater)
                | (
                    EndpointOrder::Less | EndpointOrder::Greater,
                    EndpointOrder::Less | EndpointOrder::Equal | EndpointOrder::Greater,
                ) => false,
            },
            AllenRelation::During => match (start_other_start, end_other_end) {
                (EndpointOrder::Greater, EndpointOrder::Less) => true,
                (EndpointOrder::Greater, EndpointOrder::Equal | EndpointOrder::Greater)
                | (
                    EndpointOrder::Less | EndpointOrder::Equal,
                    EndpointOrder::Less | EndpointOrder::Equal | EndpointOrder::Greater,
                ) => false,
            },
            AllenRelation::Finishes => match (start_other_start, end_other_end) {
                (EndpointOrder::Greater, EndpointOrder::Equal) => true,
                (EndpointOrder::Greater, EndpointOrder::Less | EndpointOrder::Greater)
                | (
                    EndpointOrder::Less | EndpointOrder::Equal,
                    EndpointOrder::Less | EndpointOrder::Equal | EndpointOrder::Greater,
                ) => false,
            },
            AllenRelation::Equals => match (start_other_start, end_other_end) {
                (EndpointOrder::Equal, EndpointOrder::Equal) => true,
                (EndpointOrder::Equal, EndpointOrder::Less | EndpointOrder::Greater)
                | (
                    EndpointOrder::Less | EndpointOrder::Greater,
                    EndpointOrder::Less | EndpointOrder::Equal | EndpointOrder::Greater,
                ) => false,
            },
            AllenRelation::FinishedBy => match (start_other_start, end_other_end) {
                (EndpointOrder::Less, EndpointOrder::Equal) => true,
                (EndpointOrder::Less, EndpointOrder::Less | EndpointOrder::Greater)
                | (
                    EndpointOrder::Equal | EndpointOrder::Greater,
                    EndpointOrder::Less | EndpointOrder::Equal | EndpointOrder::Greater,
                ) => false,
            },
            AllenRelation::Contains => match (start_other_start, end_other_end) {
                (EndpointOrder::Less, EndpointOrder::Greater) => true,
                (EndpointOrder::Less, EndpointOrder::Less | EndpointOrder::Equal)
                | (
                    EndpointOrder::Equal | EndpointOrder::Greater,
                    EndpointOrder::Less | EndpointOrder::Equal | EndpointOrder::Greater,
                ) => false,
            },
            AllenRelation::StartedBy => match (start_other_start, end_other_end) {
                (EndpointOrder::Equal, EndpointOrder::Greater) => true,
                (EndpointOrder::Equal, EndpointOrder::Less | EndpointOrder::Equal)
                | (
                    EndpointOrder::Less | EndpointOrder::Greater,
                    EndpointOrder::Less | EndpointOrder::Equal | EndpointOrder::Greater,
                ) => false,
            },
            AllenRelation::OverlappedBy => {
                match (start_other_start, other_end_start, end_other_end) {
                    (EndpointOrder::Greater, EndpointOrder::Greater, EndpointOrder::Greater) => {
                        true
                    }
                    (
                        EndpointOrder::Greater,
                        EndpointOrder::Greater,
                        EndpointOrder::Less | EndpointOrder::Equal,
                    )
                    | (
                        EndpointOrder::Greater,
                        EndpointOrder::Less | EndpointOrder::Equal,
                        EndpointOrder::Less | EndpointOrder::Equal | EndpointOrder::Greater,
                    )
                    | (
                        EndpointOrder::Less | EndpointOrder::Equal,
                        EndpointOrder::Less | EndpointOrder::Equal | EndpointOrder::Greater,
                        EndpointOrder::Less | EndpointOrder::Equal | EndpointOrder::Greater,
                    ) => false,
                }
            }
            AllenRelation::MetBy => match other_end_start {
                EndpointOrder::Equal => true,
                EndpointOrder::Less | EndpointOrder::Greater => false,
            },
            AllenRelation::After => match other_end_start {
                EndpointOrder::Less => true,
                EndpointOrder::Equal | EndpointOrder::Greater => false,
            },
        },
        (EndpointOrder::Less, EndpointOrder::Equal | EndpointOrder::Greater)
        | (
            EndpointOrder::Equal | EndpointOrder::Greater,
            EndpointOrder::Less | EndpointOrder::Equal | EndpointOrder::Greater,
        ) => false,
    }
}

/// Verified comparison classifier for two intervals.
#[must_use]
#[cfg_attr(verus_keep_ghost, verus_spec(r => ensures
    #![verifier::proof_note("classify")]
    r == spec_classify_orders(
        start_end,
        other_start_end,
        start_other_start,
        end_other_end,
        end_other_start,
        other_end_start,
    ),
))]
pub(crate) const fn classify(
    start_end: EndpointOrder,
    other_start_end: EndpointOrder,
    start_other_start: EndpointOrder,
    end_other_end: EndpointOrder,
    end_other_start: EndpointOrder,
    other_end_start: EndpointOrder,
) -> Option<AllenRelation> {
    match (start_end, other_start_end) {
        (EndpointOrder::Less, EndpointOrder::Less) => {
            Some(match (start_other_start, end_other_end) {
                (EndpointOrder::Equal, EndpointOrder::Equal) => AllenRelation::Equals,
                (EndpointOrder::Equal, EndpointOrder::Less) => AllenRelation::Starts,
                (EndpointOrder::Equal, EndpointOrder::Greater) => AllenRelation::StartedBy,
                (EndpointOrder::Less, EndpointOrder::Equal) => AllenRelation::FinishedBy,
                (EndpointOrder::Greater, EndpointOrder::Equal) => AllenRelation::Finishes,
                (EndpointOrder::Less, EndpointOrder::Greater) => AllenRelation::Contains,
                (EndpointOrder::Greater, EndpointOrder::Less) => AllenRelation::During,
                (EndpointOrder::Less, EndpointOrder::Less) => match end_other_start {
                    EndpointOrder::Less => AllenRelation::Before,
                    EndpointOrder::Equal => AllenRelation::Meets,
                    EndpointOrder::Greater => AllenRelation::Overlaps,
                },
                (EndpointOrder::Greater, EndpointOrder::Greater) => match other_end_start {
                    EndpointOrder::Less => AllenRelation::After,
                    EndpointOrder::Equal => AllenRelation::MetBy,
                    EndpointOrder::Greater => AllenRelation::OverlappedBy,
                },
            })
        }
        (EndpointOrder::Less, EndpointOrder::Equal | EndpointOrder::Greater)
        | (
            EndpointOrder::Equal | EndpointOrder::Greater,
            EndpointOrder::Less | EndpointOrder::Equal | EndpointOrder::Greater,
        ) => None,
    }
}
