//! Exhaustive-finite OUTPUT-ATOMIC selector; SQL crash/restart is runtime evidence.
#[path = "../../crates/norn-cell/src/compute/restatement_state.rs"]
mod state;
#[kani::proof]
fn approval_does_not_publish_and_absence_refuses() {
    let phase = match kani::any::<u8>() {
        0 => state::Phase::Building,
        1 => state::Phase::Ready,
        2 => state::Phase::Approved,
        _ => state::Phase::Active,
    };
    let previous = kani::any();
    let candidate = kani::any();
    let selected = state::select(phase, previous, candidate);
    assert_eq!(
        selected == Some(state::Selection::Candidate),
        phase == state::Phase::Active && candidate
    );
    assert_eq!(
        selected == Some(state::Selection::Previous),
        phase != state::Phase::Active && previous
    );
}
