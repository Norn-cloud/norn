//! FLOW-KEY-1, FLOW-TIME-1 and FLOW-ORDER-1 correspondence scenarios.
#![allow(clippy::expect_used, missing_docs, reason = "integration assertions")]

use norn_kernel::flow_identity::FlowStepIdentity;
use norn_kernel::timer_schedule::{TimerDeadline, deadline, effective_now, next_due};
use norn_testkit::proptest::prelude::*;

fn identity<'a>(instance: &'a str, step: &'a str) -> FlowStepIdentity<'a> {
    FlowStepIdentity {
        scope: "tenant/dev",
        world: "main",
        flow: "test.retry",
        version: "1",
        instance,
        step,
    }
}

#[test]
fn persisted_keys_retain_the_version_one_encoding() {
    let step = identity("run", "charge");
    assert_eq!(
        step.attempt_key(0).expect("key").0,
        "flow-attempt:d3f593d10af0d6af9ec828637045c52c7137042cc35a845172f077403ed50e15"
    );
    assert_eq!(
        step.operation_key().expect("key").0,
        "flow-operation:897f14ae62ab4734acf38ac72358e00126d48ce577f75fdb2284a7c1c698c3e3"
    );
}

#[test]
fn keys_frame_components_and_separate_provider_operation_from_attempt() {
    let left = identity("a:b", "c");
    let right = identity("a", "b:c");
    assert_ne!(
        left.attempt_key(0).expect("key"),
        right.attempt_key(0).expect("key")
    );
    assert_ne!(
        left.attempt_key(0).expect("key"),
        left.attempt_key(1).expect("key")
    );
    assert_ne!(
        left.operation_key().expect("key"),
        left.attempt_key(0).expect("key")
    );
    assert_eq!(
        left.operation_key().expect("key"),
        left.operation_key().expect("key")
    );
    for changed in [
        FlowStepIdentity {
            scope: "other/dev",
            ..left
        },
        FlowStepIdentity {
            world: "fork",
            ..left
        },
        FlowStepIdentity {
            flow: "test.other",
            ..left
        },
        FlowStepIdentity {
            version: "2",
            ..left
        },
        FlowStepIdentity {
            instance: "other",
            ..left
        },
        FlowStepIdentity {
            step: "other",
            ..left
        },
    ] {
        assert_ne!(
            left.attempt_key(0).expect("key"),
            changed.attempt_key(0).expect("key")
        );
        assert_ne!(
            left.operation_key().expect("key"),
            changed.operation_key().expect("key")
        );
    }
}

#[test]
fn virtual_time_boundary_ties_backwards_clock_and_overflow() {
    let timers = [
        TimerDeadline { due: 10, id: "b" },
        TimerDeadline { due: 10, id: "a" },
    ];
    assert_eq!(next_due(&timers, 9, 0), None);
    assert_eq!(next_due(&timers, 10, 0).map(|t| t.id), Some("a"));
    assert_eq!(next_due(&timers, 0, 10).map(|t| t.id), Some("a"));
    assert_eq!(next_due(&[], u64::MAX, 0), None);
    assert_eq!(deadline(0, 10, 0), Some(10));
    assert_eq!(deadline(0, u64::MAX, 1), None);
}

proptest! {
    #[test]
    fn keys_are_stable_and_distinguish_generated_intents(
        a in ("[a-z:]{1,20}", "[a-z:]{1,20}", any::<u64>()),
        b in ("[a-z:]{1,20}", "[a-z:]{1,20}", any::<u64>()),
    ) {
        let ka = identity(&a.0, &a.1).attempt_key(a.2).expect("canonical");
        prop_assert_eq!(&ka, &identity(&a.0, &a.1).attempt_key(a.2).expect("canonical"));
        let kb = identity(&b.0, &b.1).attempt_key(b.2).expect("canonical");
        prop_assert_eq!(ka == kb, a == b);
    }

    #[test]
    fn scheduling_matches_wide_arithmetic(sample: u64, floor: u64, delay: u64) {
        let expected = u128::from(sample.max(floor)).checked_add(u128::from(delay)).expect("two u64 values fit u128");
        prop_assert_eq!(deadline(sample, floor, delay), u64::try_from(expected).ok());
    }

    #[test]
    fn due_selection_is_permutation_invariant(
        entries in prop::collection::vec((any::<u64>(), "[a-z]{1,10}"), 0..64),
        sample: u64, floor: u64,
    ) {
        let mut timers: Vec<_> = entries.iter().map(|(due, id)| TimerDeadline { due: *due, id }).collect();
        let selected = next_due(&timers, sample, floor).copied();
        timers.reverse();
        prop_assert_eq!(selected, next_due(&timers, sample, floor).copied());
        let expected = entries.iter().filter(|(due, _)| *due <= effective_now(sample, floor)).min();
        prop_assert_eq!(selected.map(|t| (t.due, t.id)), expected.map(|(due, id)| (*due, id.as_str())));
    }
}
