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

pub use super::allen_core::AllenRelation;
use super::allen_core::{self, EndpointOrder};

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
        let start_end = endpoint_order(start.cmp(end));
        if !allen_core::is_less(start_end) {
            return false;
        }
        let other_start_end = endpoint_order(other_start.cmp(other_end));
        if !allen_core::is_less(other_start_end) {
            return false;
        }
        allen_core::holds(
            self,
            start_end,
            other_start_end,
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
    let start_end = endpoint_order(start.cmp(end));
    if !allen_core::is_less(start_end) {
        return None;
    }
    let other_start_end = endpoint_order(other_start.cmp(other_end));
    if !allen_core::is_less(other_start_end) {
        return None;
    }
    allen_core::classify(
        start_end,
        other_start_end,
        endpoint_order(start.cmp(other_start)),
        endpoint_order(end.cmp(other_end)),
        endpoint_order(end.cmp(other_start)),
        endpoint_order(other_end.cmp(start)),
    )
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::cmp::Ordering;
    use std::rc::Rc;

    use super::{AllenRelation, EndpointOrder, allen_core, allen_relation, endpoint_order};

    const ORDERINGS: [Ordering; 3] = [Ordering::Less, Ordering::Equal, Ordering::Greater];

    fn reference_holds(
        relation: AllenRelation,
        start: u8,
        end: u8,
        other_start: u8,
        other_end: u8,
    ) -> bool {
        if start >= end || other_start >= other_end {
            return false;
        }
        match relation {
            AllenRelation::Before => end < other_start,
            AllenRelation::Meets => end == other_start,
            AllenRelation::Overlaps => start < other_start && other_start < end && end < other_end,
            AllenRelation::Starts => start == other_start && end < other_end,
            AllenRelation::During => other_start < start && end < other_end,
            AllenRelation::Finishes => other_start < start && end == other_end,
            AllenRelation::Equals => start == other_start && end == other_end,
            AllenRelation::FinishedBy => start < other_start && end == other_end,
            AllenRelation::Contains => start < other_start && other_end < end,
            AllenRelation::StartedBy => start == other_start && other_end < end,
            AllenRelation::OverlappedBy => {
                other_start < start && start < other_end && other_end < end
            }
            AllenRelation::MetBy => other_end == start,
            AllenRelation::After => other_end < start,
        }
    }

    fn variant_index(relation: AllenRelation) -> usize {
        match relation {
            AllenRelation::Before => 0,
            AllenRelation::Meets => 1,
            AllenRelation::Overlaps => 2,
            AllenRelation::Starts => 3,
            AllenRelation::During => 4,
            AllenRelation::Finishes => 5,
            AllenRelation::Equals => 6,
            AllenRelation::FinishedBy => 7,
            AllenRelation::Contains => 8,
            AllenRelation::StartedBy => 9,
            AllenRelation::OverlappedBy => 10,
            AllenRelation::MetBy => 11,
            AllenRelation::After => 12,
        }
    }

    struct TrackedPoint {
        label: &'static str,
        value: u8,
        log: Rc<RefCell<Vec<(&'static str, &'static str)>>>,
    }

    impl PartialEq for TrackedPoint {
        fn eq(&self, other: &Self) -> bool {
            self.value == other.value
        }
    }

    impl Eq for TrackedPoint {}

    impl PartialOrd for TrackedPoint {
        fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
            Some(self.cmp(other))
        }
    }

    impl Ord for TrackedPoint {
        fn cmp(&self, other: &Self) -> Ordering {
            self.log.borrow_mut().push((self.label, other.label));
            self.value.cmp(&other.value)
        }
    }

    #[test]
    fn all_lists_all_thirteen_distinct_variants_in_canonical_order() {
        const EXPECTED: [AllenRelation; 13] = [
            AllenRelation::Before,
            AllenRelation::Meets,
            AllenRelation::Overlaps,
            AllenRelation::Starts,
            AllenRelation::During,
            AllenRelation::Finishes,
            AllenRelation::Equals,
            AllenRelation::FinishedBy,
            AllenRelation::Contains,
            AllenRelation::StartedBy,
            AllenRelation::OverlappedBy,
            AllenRelation::MetBy,
            AllenRelation::After,
        ];
        assert_eq!(AllenRelation::ALL, EXPECTED);
        let mut seen = [false; 13];
        for (slot, relation) in AllenRelation::ALL.into_iter().enumerate() {
            let idx = variant_index(relation);
            assert_eq!(idx, slot);
            assert!(!seen[idx], "duplicate variant in ALL: {relation:?}");
            seen[idx] = true;
        }
        assert_eq!(seen, [true; 13]);
    }

    #[test]
    fn structural_equality_supports_const_match_patterns() {
        const MEETS: AllenRelation = AllenRelation::Meets;
        const BEFORE: AllenRelation = AllenRelation::Before;
        for relation in AllenRelation::ALL {
            let matched = match relation {
                MEETS => "meets",
                BEFORE => "before",
                AllenRelation::Overlaps
                | AllenRelation::Starts
                | AllenRelation::During
                | AllenRelation::Finishes
                | AllenRelation::Equals
                | AllenRelation::FinishedBy
                | AllenRelation::Contains
                | AllenRelation::StartedBy
                | AllenRelation::OverlappedBy
                | AllenRelation::MetBy
                | AllenRelation::After => "other",
            };
            assert_eq!(
                matched,
                if relation == MEETS {
                    "meets"
                } else if relation == BEFORE {
                    "before"
                } else {
                    "other"
                }
            );
        }
    }

    #[test]
    fn wrappers_match_independent_reference_over_all_256_endpoint_tuples() {
        for start in 0_u8..4 {
            for end in 0_u8..4 {
                for other_start in 0_u8..4 {
                    for other_end in 0_u8..4 {
                        let expected: Vec<AllenRelation> = AllenRelation::ALL
                            .into_iter()
                            .filter(|relation| {
                                reference_holds(*relation, start, end, other_start, other_end)
                            })
                            .collect();
                        let classified = allen_relation(&start, &end, &other_start, &other_end);
                        if start < end && other_start < other_end {
                            assert_eq!(expected.len(), 1);
                            assert_eq!(classified, Some(expected[0]));
                        } else {
                            assert!(expected.is_empty());
                            assert_eq!(classified, None);
                        }
                        for relation in AllenRelation::ALL {
                            assert_eq!(
                                relation.holds(&start, &end, &other_start, &other_end),
                                reference_holds(relation, start, end, other_start, other_end),
                                "{relation:?} on [{start},{end}) vs [{other_start},{other_end})"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn endpoint_order_and_core_cover_all_729_ordering_combinations() {
        let order_code = |order: EndpointOrder| match order {
            EndpointOrder::Less => 0_u8,
            EndpointOrder::Equal => 1_u8,
            EndpointOrder::Greater => 2_u8,
        };
        assert_eq!(order_code(endpoint_order(Ordering::Less)), 0);
        assert_eq!(order_code(endpoint_order(Ordering::Equal)), 1);
        assert_eq!(order_code(endpoint_order(Ordering::Greater)), 2);

        let mut total = 0_usize;
        for first_span in ORDERINGS {
            for second_span in ORDERINGS {
                for starts_cmp in ORDERINGS {
                    for ends_cmp in ORDERINGS {
                        for end_vs_other_start in ORDERINGS {
                            for other_end_vs_start in ORDERINGS {
                                total = total.saturating_add(1);
                                let classified = allen_core::classify(
                                    endpoint_order(first_span),
                                    endpoint_order(second_span),
                                    endpoint_order(starts_cmp),
                                    endpoint_order(ends_cmp),
                                    endpoint_order(end_vs_other_start),
                                    endpoint_order(other_end_vs_start),
                                );
                                let proper =
                                    first_span == Ordering::Less && second_span == Ordering::Less;
                                assert_eq!(classified.is_some(), proper);
                                if !proper {
                                    for relation in AllenRelation::ALL {
                                        assert!(!allen_core::holds(
                                            relation,
                                            endpoint_order(first_span),
                                            endpoint_order(second_span),
                                            endpoint_order(starts_cmp),
                                            endpoint_order(ends_cmp),
                                            endpoint_order(end_vs_other_start),
                                            endpoint_order(other_end_vs_start),
                                        ));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        assert_eq!(total, 729);
    }

    #[test]
    fn wrappers_short_circuit_improper_intervals_and_preserve_cmp_order() {
        let make = |first_start, first_end, second_start, second_end| {
            let log = Rc::new(RefCell::new(Vec::new()));
            let point = |label, value| TrackedPoint {
                label,
                value,
                log: Rc::clone(&log),
            };
            (
                log,
                point("start", first_start),
                point("end", first_end),
                point("other_start", second_start),
                point("other_end", second_end),
            )
        };

        // Improper first interval short-circuits after exactly 1 comparison.
        for (first_start, first_end) in [(2, 2), (3, 1)] {
            let (log, start, end, other_start, other_end) = make(first_start, first_end, 0, 1);
            assert_eq!(allen_relation(&start, &end, &other_start, &other_end), None);
            assert_eq!(*log.borrow(), vec![("start", "end")]);

            log.borrow_mut().clear();
            assert!(!AllenRelation::Meets.holds(&start, &end, &other_start, &other_end));
            assert_eq!(*log.borrow(), vec![("start", "end")]);
        }

        // Improper second interval short-circuits after exactly 2 comparisons.
        for (second_start, second_end) in [(2, 2), (4, 1)] {
            let (log, start, end, other_start, other_end) = make(0, 1, second_start, second_end);
            assert_eq!(allen_relation(&start, &end, &other_start, &other_end), None);
            assert_eq!(
                *log.borrow(),
                vec![("start", "end"), ("other_start", "other_end")]
            );

            log.borrow_mut().clear();
            assert!(!AllenRelation::Meets.holds(&start, &end, &other_start, &other_end));
            assert_eq!(
                *log.borrow(),
                vec![("start", "end"), ("other_start", "other_end")]
            );
        }

        // Proper intervals compare the exact 6 endpoint pairs in the pinned order.
        let expected_order = vec![
            ("start", "end"),
            ("other_start", "other_end"),
            ("start", "other_start"),
            ("end", "other_end"),
            ("end", "other_start"),
            ("other_end", "start"),
        ];
        let (log, start, end, other_start, other_end) = make(0, 2, 2, 5);
        assert_eq!(
            allen_relation(&start, &end, &other_start, &other_end),
            Some(AllenRelation::Meets)
        );
        assert_eq!(*log.borrow(), expected_order);

        log.borrow_mut().clear();
        assert!(AllenRelation::Meets.holds(&start, &end, &other_start, &other_end));
        assert_eq!(*log.borrow(), expected_order);
    }
}
