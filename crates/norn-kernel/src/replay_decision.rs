//! K6 idempotency replay decision, the pure core of `action_commit::replay`.
//!
//! This file compiles standalone (core only): Charon extracts it with
//! `--start-from` and `verification/kernel-proof-v1/Binding` proves it. Record
//! decoding and the callers' early return stay glue in `action_commit`.

/// The identity of a call that an idempotency key binds: the action, the
/// target object, the call's input, the actor, and the writer-closure binding.
pub(crate) struct CallIdentity<A, O, I, B> {
    /// The action.
    pub(crate) action: A,
    /// The target object.
    pub(crate) object: O,
    /// The call's input, as supplied (not the derived effective input).
    pub(crate) input: I,
    /// The actor.
    pub(crate) actor: A,
    /// TXN-RULE-1 writer-closure binding, if any.
    pub(crate) rule_binding: B,
}

/// Whether a recorded call and a new call are the same call: every
/// component of their identity is equal. Any other difference, including a
/// different input, is a different call.
pub(crate) fn same_call<A: PartialEq, O: PartialEq, I: PartialEq, B: PartialEq>(
    recorded: &CallIdentity<A, O, I, B>,
    call: &CallIdentity<A, O, I, B>,
) -> bool {
    recorded.action == call.action
        && recorded.object == call.object
        && recorded.input == call.input
        && recorded.actor == call.actor
        && recorded.rule_binding == call.rule_binding
}

/// A call record that can say whether it binds the same call as another.
pub(crate) trait RecordedCall {
    /// Whether `self`, as recorded, binds the same call as `call`.
    fn same_call(&self, call: &Self) -> bool;
}

/// What an idempotency key holds.
pub(crate) enum Recorded<R> {
    /// Nothing was committed under the key.
    Empty,
    /// A record that does not decode.
    Unreadable,
    /// The committed call's record.
    Record(R),
}

/// What a repeated idempotency key decides.
pub(crate) enum ReplayDecision<R> {
    /// Nothing was committed under the key: run the call.
    Fresh,
    /// The same call was committed: return its recorded outcome, commit nothing.
    Replay(R),
    /// The key was used for another call, or its record is unreadable.
    Conflict,
}

/// Decide a call against what its idempotency key holds.
pub(crate) fn decide_replay<R: RecordedCall>(recorded: Recorded<R>, call: &R) -> ReplayDecision<R> {
    match recorded {
        Recorded::Empty => ReplayDecision::Fresh,
        Recorded::Record(record) => {
            if record.same_call(call) {
                ReplayDecision::Replay(record)
            } else {
                ReplayDecision::Conflict
            }
        }
        Recorded::Unreadable => ReplayDecision::Conflict,
    }
}
