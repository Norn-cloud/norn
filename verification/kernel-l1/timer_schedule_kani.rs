//! Bounded checks of the production scheduling functions, not a scheduler model.
#[path = "../../crates/norn-kernel/src/timer_schedule.rs"]
mod timer_schedule;

#[kani::proof]
fn deadline_never_wraps_or_precedes_floor() {
    let sample: u64 = kani::any();
    let floor: u64 = kani::any();
    let delay: u64 = kani::any();
    let now = timer_schedule::effective_now(sample, floor);
    assert!(now >= floor && now >= sample);
    match timer_schedule::deadline(sample, floor, delay) {
        Some(due) => {
            assert!(due >= now);
            assert_eq!(due - now, delay);
        }
        None => assert!(delay > u64::MAX - now),
    }
}

#[kani::proof]
#[kani::unwind(5)]
fn selection_is_due_minimum_and_permutation_invariant() {
    use timer_schedule::{TimerDeadline, effective_now, next_due};
    let sample: u64 = kani::any();
    let floor: u64 = kani::any();
    let a = TimerDeadline {
        due: kani::any(),
        id: "a",
    };
    let b = TimerDeadline {
        due: kani::any(),
        id: "b",
    };
    let c = TimerDeadline {
        due: kani::any(),
        id: "c",
    };
    let timers = [c, a, b];
    let permuted = [b, c, a];
    let selected = next_due(&timers, sample, floor);
    assert_eq!(selected, next_due(&permuted, sample, floor));
    let now = effective_now(sample, floor);
    for timer in &timers {
        if timer.due <= now {
            let chosen = selected.expect("eligible timer must be selected");
            assert!((chosen.due, chosen.id) <= (timer.due, timer.id));
        }
    }
    if let Some(chosen) = selected {
        assert!(chosen.due <= now);
    }
}
