//! OWL-Time Allen relations: bounded checks of the production classifier.
//!
//! The module below is `crates/norn-expr/src/temporal/allen.rs` itself, so the
//! harnesses run the shipped `AllenRelation::holds` and `allen_relation`.
//! Endpoints are symbolic `i64` values: every pair of proper `i64` intervals is
//! covered, and nothing outside `i64` is claimed.
#[path = "../../crates/norn-expr/src/temporal/allen.rs"]
mod allen;

use allen::{AllenRelation, allen_relation};

fn any_interval() -> (i64, i64) {
    (kani::any(), kani::any())
}

fn any_proper_interval() -> (i64, i64) {
    let (start, end) = any_interval();
    kani::assume(start < end);
    (start, end)
}

/// Jointly exhaustive and pairwise disjoint: exactly one of the thirteen
/// endpoint predicates holds, and it is the one the classifier returns.
#[kani::proof]
#[kani::unwind(14)]
fn exactly_one_relation_holds_and_the_classifier_returns_it() {
    let (start, end) = any_proper_interval();
    let (other_start, other_end) = any_proper_interval();
    let classified = allen_relation(&start, &end, &other_start, &other_end);
    let mut holding = 0_u8;
    for relation in AllenRelation::ALL {
        if relation.holds(&start, &end, &other_start, &other_end) {
            holding += 1;
            assert_eq!(classified, Some(relation));
        }
    }
    assert_eq!(holding, 1);
}

/// Converse symmetry: a relation holds one way exactly when its converse
/// holds the other way, and swapping the intervals swaps the classification.
#[kani::proof]
#[kani::unwind(14)]
fn the_converse_holds_for_the_swapped_intervals() {
    let (start, end) = any_proper_interval();
    let (other_start, other_end) = any_proper_interval();
    for relation in AllenRelation::ALL {
        assert_eq!(relation.converse().converse(), relation);
        assert_eq!(
            relation.holds(&start, &end, &other_start, &other_end),
            relation
                .converse()
                .holds(&other_start, &other_end, &start, &end)
        );
    }
    assert_eq!(
        allen_relation(&other_start, &other_end, &start, &end),
        allen_relation(&start, &end, &other_start, &other_end).map(AllenRelation::converse)
    );
}

/// Boundary law: the relation shares an instant exactly when the
/// `intervals_overlap` test (`aStart < bEnd && bStart < aEnd`) is true, an
/// instant in both intervals implies it, and when it is true the later start
/// is such an instant.
#[kani::proof]
fn sharing_an_instant_is_the_half_open_overlap_test() {
    let (start, end) = any_proper_interval();
    let (other_start, other_end) = any_proper_interval();
    let Some(relation) = allen_relation(&start, &end, &other_start, &other_end) else {
        unreachable!("proper intervals always have a relation");
    };
    let overlap = start < other_end && other_start < end;
    assert_eq!(relation.shares_instant(), overlap);

    let instant: i64 = kani::any();
    let in_both =
        start <= instant && instant < end && other_start <= instant && instant < other_end;
    assert!(!in_both || relation.shares_instant());

    let later_start = start.max(other_start);
    let witness = start <= later_start
        && later_start < end
        && other_start <= later_start
        && later_start < other_end;
    assert_eq!(witness, relation.shares_instant());

    // Adjacent `[a, b)` and `[b, c)` meet and share nothing.
    if end == other_start {
        assert_eq!(relation, AllenRelation::Meets);
        assert!(!relation.shares_instant());
    }
}

/// Fail closed: an improper interval has no relation and satisfies none.
#[kani::proof]
#[kani::unwind(14)]
fn improper_intervals_have_no_relation() {
    let (start, end) = any_interval();
    let (other_start, other_end) = any_interval();
    kani::assume(start >= end || other_start >= other_end);
    assert_eq!(allen_relation(&start, &end, &other_start, &other_end), None);
    for relation in AllenRelation::ALL {
        assert!(!relation.holds(&start, &end, &other_start, &other_end));
    }
}

/// Scheduling compositions over three proper intervals. Each `composes!`
/// line asserts one composition and covers it, so the run also shows that
/// every listed pair of relations is reachable.
#[kani::proof]
fn before_during_and_meets_compose() {
    let (a_start, a_end) = any_proper_interval();
    let (b_start, b_end) = any_proper_interval();
    let (c_start, c_end) = any_proper_interval();
    let first = allen_relation(&a_start, &a_end, &b_start, &b_end);
    let second = allen_relation(&b_start, &b_end, &c_start, &c_end);
    let composed = allen_relation(&a_start, &a_end, &c_start, &c_end);
    macro_rules! composes {
        ($first:ident then $second:ident gives $expected:ident, $label:literal) => {
            if first == Some(AllenRelation::$first) && second == Some(AllenRelation::$second) {
                kani::cover!(true, $label);
                assert_eq!(composed, Some(AllenRelation::$expected));
            }
        };
    }
    composes!(Before then Before gives Before, "Before then Before");
    composes!(After then After gives After, "After then After");
    composes!(During then During gives During, "During then During");
    composes!(Contains then Contains gives Contains, "Contains then Contains");
    composes!(Equals then Equals gives Equals, "Equals then Equals");
    composes!(Meets then Meets gives Before, "Meets then Meets");
    composes!(Before then Meets gives Before, "Before then Meets");
    composes!(Meets then Before gives Before, "Meets then Before");
    composes!(During then Before gives Before, "During then Before");
}
