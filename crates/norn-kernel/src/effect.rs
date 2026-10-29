//! The closed effect ADT and the command/timer data it carries.
//!
//! A decision never performs anything. It returns [`Effect`] values that the
//! host enqueues in the same transaction as the decision's events (the
//! `commit` verb), then executes (`perform`) or delivers when due (`clock`).
//! Every effect is plain serializable data: no closures, no trait objects.

use serde::{Deserialize, Serialize};

use crate::observations::LogicalTime;

/// Stable identity of one outbound command.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CommandId(pub String);

/// Stable identity of one timer; scheduling the same id again replaces it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TimerId(pub String);

/// Idempotency key the `perform` boundary presents to the external system.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct IdempotencyKey(pub String);

/// Request generation of a command (ruling 2026-09-27 §4).
///
/// A result is accepted only for the generation it was dispatched under; a
/// re-issued or cancelled request bumps the generation and fences late
/// attempts.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Generation(pub u64);

impl Generation {
    /// The next generation, or `None` on overflow.
    #[must_use]
    pub const fn checked_next(self) -> Option<Self> {
        match self.0.checked_add(1) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }
}

/// Relevant-state dependency token pinned at dispatch (ruling 2026-09-27 §4).
///
/// Opaque to this crate: a versioned slice/predicate token or a conservative
/// relevant-collection epoch that covers membership, order, and absence.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DependencyToken(pub String);

/// Identity of a command's immutable inputs, pinned at dispatch
/// (ruling 2026-09-27 §4).
///
/// Opaque to this crate: a canonical digest over the input values and
/// immutable input content digests (the Function invocation's input identity,
/// T1-FN-1). The result receipt reports the identity it actually consumed.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct InputIdentity(pub String);

/// Owner version of the destination a result will be written to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OwnerVersion(pub u64);

/// How the cell decides whether a command's result is still fresh when it
/// returns (ruling 2026-09-27 §4). Reconciliation itself is T2-RES-1.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum FreshnessPolicy {
    /// State-sensitive: the result is fresh only if the relevant state the
    /// token covers is unchanged.
    RelevantState {
        /// Token pinned at dispatch.
        token: DependencyToken,
    },
    /// Transform over immutable inputs: fresh only if **both** hold at
    /// acceptance —
    ///
    /// 1. the receipt's consumed input identity equals `inputs` (the result
    ///    was computed from exactly the inputs pinned at dispatch), and
    /// 2. the destination still has owner version `expected` (checked as a
    ///    CAS in the same commit that accepts the result).
    ///
    /// Either check alone is insufficient: an unchanged destination does not
    /// make a result computed from different inputs fresh. The reconciler
    /// (T2-RES-1) must apply both.
    DestinationCas {
        /// Immutable-input identity pinned at dispatch.
        inputs: InputIdentity,
        /// Owner version expected at acceptance.
        expected: OwnerVersion,
    },
    /// The result is an append-only observation that stays historically
    /// useful; it is never stale.
    AppendOnly,
}

/// One outbound command, executed by the host's `perform` verb.
///
/// Its result returns to the cell as an input to a later decision; it is a
/// proposal, not a state transition.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Command<C> {
    /// Stable identity of the command.
    pub id: CommandId,
    /// Request generation the result must match.
    pub generation: Generation,
    /// Key the external system deduplicates on.
    pub idempotency_key: IdempotencyKey,
    /// Freshness policy applied when the result returns.
    pub freshness: FreshnessPolicy,
    /// Deadline after which the result is no longer awaited.
    pub deadline: Option<LogicalTime>,
    /// Typed command payload.
    pub payload: C,
}

/// One timer, delivered back to the cell by the host's `clock` verb when due.
///
/// The payload is the decider's own input type: when the timer fires, the host
/// hands `payload` to the next `decide` unchanged, so no conversion step exists
/// at the boundary.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Timer<T> {
    /// Stable identity of the timer.
    pub id: TimerId,
    /// When the timer is due.
    pub due: LogicalTime,
    /// The input delivered to `decide` when the timer fires.
    pub payload: T,
}

/// The closed set of effects a decision may request.
///
/// `C` is the outbound command payload; `T` is the timer payload, which is the
/// decider's input type (see [`Timer`]).
///
/// Compensation, human tasks, and retries are not kernel primitives
/// (ruling 2026-09-25 §4): compensation is a rule on a failure event, a task
/// is a fact, a retry is a new command or timer.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effect<C, T> {
    /// Enqueue a command for `perform`. Only the main world may perform.
    Perform(Command<C>),
    /// Enqueue (or replace) a timer for `clock`.
    Schedule(Timer<T>),
    /// Cancel a pending timer. Cancelling an absent timer is a no-op.
    CancelTimer(TimerId),
}

impl<C, T> Effect<C, T> {
    /// Whether this effect reaches the outside world.
    #[must_use]
    pub const fn is_perform(&self) -> bool {
        matches!(self, Self::Perform(_))
    }

    /// Whether this effect touches the timer table.
    #[must_use]
    pub const fn is_timer(&self) -> bool {
        matches!(self, Self::Schedule(_) | Self::CancelTimer(_))
    }
}
