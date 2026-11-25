//! D03 (#1699): the outbox command lifecycle as one total, pure transition.
//!
//! D03 names the lifecycle requested → dispatched → succeeded / failed /
//! outcome-unknown → reconciliation-required → compensation-pending →
//! compensated, ending when a commit consumes a recorded result. Every state
//! is one [`OutboxState`] variant and every change one [`OutboxEvent`]; a
//! host applies a change only through [`step`], which either returns the next
//! state and generation or refuses with a typed [`OutboxRefusal`]. A new
//! lifecycle condition is a new variant, so every `match` over the lifecycle
//! breaks until it handles it.
//!
//! Laws (exhaustive-finite evidence in this module's tests, over every
//! `(state, event)` pair and a bounded breadth-first exploration):
//!
//! - **Total:** [`step`] returns for every input and never panics.
//! - **Terminal absorbs:** [`OutboxState::Compensated`] and
//!   [`OutboxState::Consumed`] refuse every event with
//!   [`OutboxRefusal::Terminal`].
//! - **Monotone generation:** an accepted event never lowers the generation.
//! - **Fenced never claimed:** a row in
//!   [`OutboxState::ReconciliationRequired`] refuses every
//!   [`OutboxEvent::Claim`]; only [`OutboxEvent::Reconciled`] at a generation
//!   above the fence returns it to [`OutboxState::Requested`].
//! - **One claim per generation:** [`OutboxEvent::Claim`] succeeds only from
//!   [`OutboxState::Requested`] at the row's generation, and no sequence of
//!   events returns a claimed generation to `Requested`.
//! - **No silent divergence:** a reissue at the row's generation with a
//!   different payload is [`OutboxRefusal::PayloadDiverged`], never a no-op.
//!
//! [`Queue`] is the queue a command waits in, set once when it is first
//! enqueued and stored with it, never re-derived from its mutable payload.
//! [`OperationKey`] is the identity of the business operation: it takes no
//! generation, so a reissue cannot change it.

use norn_ir::cell_id::CellId;
use serde::{Deserialize, Serialize};

use crate::CommandId;

/// The queue a pending command waits in, fixed at its first enqueue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Queue {
    /// Performed in strict enqueue order: the head blocks the commands behind
    /// it until a commit consumes its result.
    Fifo,
    /// Waits outside the FIFO until the cell resolves it (PARKED-1); never
    /// claimed or performed against the outside world.
    Parked,
}

impl Queue {
    /// The stored column value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fifo => "fifo",
            Self::Parked => "parked",
        }
    }

    /// The queue a stored column value names; `None` for any other text, so
    /// an unreadable row fails closed rather than defaulting to a queue.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "fifo" => Some(Self::Fifo),
            "parked" => Some(Self::Parked),
            _ => None,
        }
    }
}

/// One outbox row's lifecycle state (D03).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OutboxState {
    /// Enqueued and not yet claimed at its generation.
    Requested,
    /// Claimed for external dispatch at `generation`.
    Dispatched {
        /// The claimed generation.
        generation: u64,
    },
    /// A successful result is recorded and waits for a consuming commit.
    Succeeded,
    /// A failed result is recorded and waits for a consuming commit.
    Failed,
    /// Claimed at `generation` with no recorded result: the external outcome
    /// is not known to this process.
    OutcomeUnknown {
        /// The claimed generation.
        generation: u64,
    },
    /// A restore fenced the row at `fenced_at`; nothing is claimed until a
    /// reconciliation authorizes a later generation.
    ReconciliationRequired {
        /// The generation the restore fenced.
        fenced_at: u64,
    },
    /// Reconciliation chose compensation, which has failed `attempts` times.
    CompensationPending {
        /// Failed compensation attempts so far.
        attempts: u32,
    },
    /// Compensation completed. Terminal.
    Compensated,
    /// Commit `commit_seq` consumed the recorded result. Terminal.
    Consumed {
        /// The consuming commit.
        commit_seq: u64,
    },
}

impl OutboxState {
    /// Whether the state absorbs every event.
    #[must_use]
    pub const fn is_terminal(self) -> bool {
        match self {
            Self::Compensated | Self::Consumed { .. } => true,
            Self::Requested
            | Self::Dispatched { .. }
            | Self::Succeeded
            | Self::Failed
            | Self::OutcomeUnknown { .. }
            | Self::ReconciliationRequired { .. }
            | Self::CompensationPending { .. } => false,
        }
    }

    /// Whether a result is recorded and waits for a consuming commit.
    #[must_use]
    pub const fn has_result(self) -> bool {
        match self {
            Self::Succeeded | Self::Failed => true,
            Self::Requested
            | Self::Dispatched { .. }
            | Self::OutcomeUnknown { .. }
            | Self::ReconciliationRequired { .. }
            | Self::CompensationPending { .. }
            | Self::Compensated
            | Self::Consumed { .. } => false,
        }
    }
}

/// One change a host applies to an outbox row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum OutboxEvent {
    /// The command is enqueued again at `generation`; `same_payload` says
    /// whether its canonical payload equals the stored one.
    Reissue {
        /// The reissued generation.
        generation: u64,
        /// Whether the payload is byte-identical to the stored one.
        same_payload: bool,
    },
    /// A durable claim for external dispatch at `generation`.
    Claim {
        /// The generation being claimed.
        generation: u64,
    },
    /// The dispatched generation's result is recorded.
    Result {
        /// The generation the result belongs to.
        generation: u64,
        /// Whether the result is a success.
        ok: bool,
    },
    /// A result recorded without a dispatch: a parked command the cell
    /// resolved, or a fork's simulated result.
    Resolved {
        /// The generation the result belongs to.
        generation: u64,
        /// Whether the result is a success.
        ok: bool,
    },
    /// The host restarted: a dispatch it did not see complete is unknown.
    CrashSeen,
    /// A restore fenced the row at the generation it held then.
    Restored {
        /// The row's generation when it was restored.
        generation_at_restore: u64,
    },
    /// Ledgered reconciliation authorizes `generation` after a fence.
    Reconciled {
        /// The authorized generation.
        generation: u64,
    },
    /// A compensation attempt ended.
    Compensation {
        /// Whether it completed.
        ok: bool,
    },
    /// Commit `commit_seq` consumes the recorded result.
    Consume {
        /// The consuming commit.
        commit_seq: u64,
    },
}

/// Why [`step`] refused an event. Nothing changes on a refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, thiserror::Error)]
pub enum OutboxRefusal {
    /// The event names a generation other than the row's (older, or for a
    /// claim or result, not the current one).
    #[error("the event names a generation other than the outbox row's")]
    StaleGeneration,
    /// A reissue at the row's own generation carries a different payload.
    #[error("a reissue at the outbox row's generation carries a different payload")]
    PayloadDiverged,
    /// The row is fenced at `fenced_at` pending reconciliation.
    #[error("the outbox row is fenced at generation {fenced_at} pending reconciliation")]
    Fenced {
        /// The fenced generation.
        fenced_at: u64,
    },
    /// The event does not apply to the row's current state.
    #[error("the event does not apply to the outbox row's state")]
    NotPending,
    /// The row is terminal and absorbs every event.
    #[error("the outbox row is terminal")]
    Terminal,
}

/// The state and generation of a row a first enqueue creates at
/// `generation`.
#[must_use]
pub const fn enqueued(generation: u64) -> (OutboxState, u64) {
    (OutboxState::Requested, generation)
}

/// Apply `event` to a row in `state` at `generation`.
///
/// Total and pure: the result depends only on the arguments, and every input
/// either yields the next `(state, generation)` or a typed refusal. See the
/// module documentation for the laws it keeps.
///
/// # Errors
///
/// An [`OutboxRefusal`] naming why the event does not apply; the row is then
/// unchanged.
#[must_use = "a refused event leaves the row unchanged; the caller must not write"]
pub const fn step(
    state: OutboxState,
    generation: u64,
    event: OutboxEvent,
) -> Result<(OutboxState, u64), OutboxRefusal> {
    if state.is_terminal() {
        return Err(OutboxRefusal::Terminal);
    }
    match event {
        OutboxEvent::Reissue {
            generation: next,
            same_payload,
        } => reissue(state, generation, next, same_payload),
        OutboxEvent::Claim {
            generation: claimed,
        } => claim(state, generation, claimed),
        OutboxEvent::Result { generation: of, ok } => record(state, generation, of, ok),
        OutboxEvent::Resolved { generation: of, ok } => resolve(state, generation, of, ok),
        OutboxEvent::CrashSeen => Ok((crash_seen(state), generation)),
        OutboxEvent::Restored {
            generation_at_restore,
        } => restored(state, generation, generation_at_restore),
        OutboxEvent::Reconciled {
            generation: authorized,
        } => reconciled(state, generation, authorized),
        OutboxEvent::Compensation { ok } => compensation(state, generation, ok),
        OutboxEvent::Consume { commit_seq } => consume(state, generation, commit_seq),
    }
}

const fn reissue(
    state: OutboxState,
    generation: u64,
    next: u64,
    same_payload: bool,
) -> Result<(OutboxState, u64), OutboxRefusal> {
    if next < generation {
        return Err(OutboxRefusal::StaleGeneration);
    }
    if next == generation {
        return if same_payload {
            Ok((state, generation))
        } else {
            Err(OutboxRefusal::PayloadDiverged)
        };
    }
    match state {
        // A newer generation replaces the pending one in place; a recorded
        // result or claim of the older generation no longer applies.
        OutboxState::Requested
        | OutboxState::Dispatched { .. }
        | OutboxState::Succeeded
        | OutboxState::Failed
        | OutboxState::OutcomeUnknown { .. } => Ok((OutboxState::Requested, next)),
        // A reissue never lifts a fence: only reconciliation does.
        OutboxState::ReconciliationRequired { fenced_at } => {
            Ok((OutboxState::ReconciliationRequired { fenced_at }, next))
        }
        OutboxState::CompensationPending { .. }
        | OutboxState::Compensated
        | OutboxState::Consumed { .. } => Err(OutboxRefusal::NotPending),
    }
}

const fn claim(
    state: OutboxState,
    generation: u64,
    claimed: u64,
) -> Result<(OutboxState, u64), OutboxRefusal> {
    match state {
        OutboxState::Requested => {
            if claimed == generation {
                Ok((
                    OutboxState::Dispatched {
                        generation: claimed,
                    },
                    generation,
                ))
            } else {
                Err(OutboxRefusal::StaleGeneration)
            }
        }
        OutboxState::ReconciliationRequired { fenced_at } => {
            Err(OutboxRefusal::Fenced { fenced_at })
        }
        OutboxState::Dispatched { .. }
        | OutboxState::Succeeded
        | OutboxState::Failed
        | OutboxState::OutcomeUnknown { .. }
        | OutboxState::CompensationPending { .. }
        | OutboxState::Compensated
        | OutboxState::Consumed { .. } => Err(OutboxRefusal::NotPending),
    }
}

const fn outcome(ok: bool) -> OutboxState {
    if ok {
        OutboxState::Succeeded
    } else {
        OutboxState::Failed
    }
}

const fn record(
    state: OutboxState,
    generation: u64,
    of: u64,
    ok: bool,
) -> Result<(OutboxState, u64), OutboxRefusal> {
    match state {
        OutboxState::Dispatched {
            generation: claimed,
        }
        | OutboxState::OutcomeUnknown {
            generation: claimed,
        } => {
            if of == generation && claimed == generation {
                Ok((outcome(ok), generation))
            } else {
                Err(OutboxRefusal::StaleGeneration)
            }
        }
        OutboxState::ReconciliationRequired { fenced_at } => {
            Err(OutboxRefusal::Fenced { fenced_at })
        }
        OutboxState::Requested
        | OutboxState::Succeeded
        | OutboxState::Failed
        | OutboxState::CompensationPending { .. }
        | OutboxState::Compensated
        | OutboxState::Consumed { .. } => Err(OutboxRefusal::NotPending),
    }
}

const fn resolve(
    state: OutboxState,
    generation: u64,
    of: u64,
    ok: bool,
) -> Result<(OutboxState, u64), OutboxRefusal> {
    match state {
        OutboxState::Requested => {
            if of == generation {
                Ok((outcome(ok), generation))
            } else {
                Err(OutboxRefusal::StaleGeneration)
            }
        }
        OutboxState::ReconciliationRequired { fenced_at } => {
            Err(OutboxRefusal::Fenced { fenced_at })
        }
        OutboxState::Dispatched { .. }
        | OutboxState::Succeeded
        | OutboxState::Failed
        | OutboxState::OutcomeUnknown { .. }
        | OutboxState::CompensationPending { .. }
        | OutboxState::Compensated
        | OutboxState::Consumed { .. } => Err(OutboxRefusal::NotPending),
    }
}

const fn crash_seen(state: OutboxState) -> OutboxState {
    match state {
        OutboxState::Dispatched { generation } => OutboxState::OutcomeUnknown { generation },
        OutboxState::Requested
        | OutboxState::Succeeded
        | OutboxState::Failed
        | OutboxState::OutcomeUnknown { .. }
        | OutboxState::ReconciliationRequired { .. }
        | OutboxState::CompensationPending { .. }
        | OutboxState::Compensated
        | OutboxState::Consumed { .. } => state,
    }
}

const fn restored(
    state: OutboxState,
    generation: u64,
    at_restore: u64,
) -> Result<(OutboxState, u64), OutboxRefusal> {
    if at_restore != generation {
        return Err(OutboxRefusal::StaleGeneration);
    }
    match state {
        // The fence takes precedence over a saved claim and a saved result:
        // whether the outside world saw this generation is not known.
        OutboxState::Requested
        | OutboxState::Dispatched { .. }
        | OutboxState::Succeeded
        | OutboxState::Failed
        | OutboxState::OutcomeUnknown { .. }
        | OutboxState::ReconciliationRequired { .. } => Ok((
            OutboxState::ReconciliationRequired {
                fenced_at: generation,
            },
            generation,
        )),
        OutboxState::CompensationPending { .. }
        | OutboxState::Compensated
        | OutboxState::Consumed { .. } => Ok((state, generation)),
    }
}

const fn reconciled(
    state: OutboxState,
    generation: u64,
    authorized: u64,
) -> Result<(OutboxState, u64), OutboxRefusal> {
    match state {
        OutboxState::ReconciliationRequired { fenced_at } => {
            if authorized != generation {
                Err(OutboxRefusal::StaleGeneration)
            } else if authorized > fenced_at {
                Ok((OutboxState::Requested, generation))
            } else {
                Err(OutboxRefusal::Fenced { fenced_at })
            }
        }
        OutboxState::Requested
        | OutboxState::Dispatched { .. }
        | OutboxState::Succeeded
        | OutboxState::Failed
        | OutboxState::OutcomeUnknown { .. }
        | OutboxState::CompensationPending { .. }
        | OutboxState::Compensated
        | OutboxState::Consumed { .. } => Err(OutboxRefusal::NotPending),
    }
}

const fn compensation(
    state: OutboxState,
    generation: u64,
    ok: bool,
) -> Result<(OutboxState, u64), OutboxRefusal> {
    let attempts = match state {
        OutboxState::ReconciliationRequired { .. } => 0,
        OutboxState::CompensationPending { attempts } => attempts,
        OutboxState::Requested
        | OutboxState::Dispatched { .. }
        | OutboxState::Succeeded
        | OutboxState::Failed
        | OutboxState::OutcomeUnknown { .. }
        | OutboxState::Compensated
        | OutboxState::Consumed { .. } => return Err(OutboxRefusal::NotPending),
    };
    if ok {
        Ok((OutboxState::Compensated, generation))
    } else {
        Ok((
            OutboxState::CompensationPending {
                attempts: attempts.saturating_add(1),
            },
            generation,
        ))
    }
}

const fn consume(
    state: OutboxState,
    generation: u64,
    commit_seq: u64,
) -> Result<(OutboxState, u64), OutboxRefusal> {
    match state {
        OutboxState::Succeeded | OutboxState::Failed => {
            Ok((OutboxState::Consumed { commit_seq }, generation))
        }
        OutboxState::ReconciliationRequired { fenced_at } => {
            Err(OutboxRefusal::Fenced { fenced_at })
        }
        OutboxState::Requested
        | OutboxState::Dispatched { .. }
        | OutboxState::OutcomeUnknown { .. }
        | OutboxState::CompensationPending { .. }
        | OutboxState::Compensated
        | OutboxState::Consumed { .. } => Err(OutboxRefusal::NotPending),
    }
}

/// The identity of one business operation: what a deduplicating provider
/// keys an external effect by. It is derived from the cell and the command
/// alone and takes no generation, so reissuing a command at a newer
/// generation can never change it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OperationKey([u8; 32]);

/// The domain separator of [`OperationKey::derive`].
const OPERATION_KEY_DOMAIN: &[u8] = b"norn.operation-key.v1\0";

impl OperationKey {
    /// The operation key of `command` in `cell`: SHA-256 over a domain
    /// separator and the canonical JSON of `[cell, command]`.
    ///
    /// # Errors
    ///
    /// A canonical JSON encoding failure, as a message.
    pub fn derive(cell: &CellId, command: &CommandId) -> Result<Self, String> {
        let hex = norn_ir::canonical::domain_hash_hex(
            OPERATION_KEY_DOMAIN,
            &(cell.as_str(), command.0.as_str()),
        )
        .map_err(|error| error.to_string())?;
        Self::from_hex(&hex)
            .ok_or_else(|| format!("operation key digest is not 64 hex digits: {hex}"))
    }

    /// The key a 64-digit lowercase hex text names.
    #[must_use]
    pub fn from_hex(hex: &str) -> Option<Self> {
        let (pairs, rest) = hex.as_bytes().as_chunks::<2>();
        if pairs.len() != 32 || !rest.is_empty() {
            return None;
        }
        let mut bytes = [0_u8; 32];
        for (byte, [high, low]) in bytes.iter_mut().zip(pairs) {
            *byte = nibble(*high)?.checked_mul(16)?.checked_add(nibble(*low)?)?;
        }
        Some(Self(bytes))
    }

    /// The key's 32 bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// The key as 64 lowercase hex digits.
    #[must_use]
    pub fn to_hex(&self) -> String {
        use std::fmt::Write as _;
        self.0
            .iter()
            .fold(String::with_capacity(64), |mut text, byte| {
                // Writing to a `String` cannot fail.
                let _written = write!(text, "{byte:02x}");
                text
            })
    }
}

const fn nibble(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => digit.checked_sub(b'0'),
        b'a'..=b'f' => match digit.checked_sub(b'a') {
            Some(offset) => offset.checked_add(10),
            None => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet, VecDeque};

    use super::*;

    const GENERATIONS: [u64; 5] = [0, 1, 2, 3, u64::MAX];

    fn states() -> Vec<OutboxState> {
        let mut states = vec![
            OutboxState::Requested,
            OutboxState::Succeeded,
            OutboxState::Failed,
            OutboxState::Compensated,
        ];
        for generation in GENERATIONS {
            states.push(OutboxState::Dispatched { generation });
            states.push(OutboxState::OutcomeUnknown { generation });
            states.push(OutboxState::ReconciliationRequired {
                fenced_at: generation,
            });
            states.push(OutboxState::Consumed {
                commit_seq: generation,
            });
        }
        for attempts in [0, 1, u32::MAX] {
            states.push(OutboxState::CompensationPending { attempts });
        }
        states
    }

    fn events() -> Vec<OutboxEvent> {
        let mut events = vec![OutboxEvent::CrashSeen];
        for ok in [false, true] {
            events.push(OutboxEvent::Compensation { ok });
        }
        for generation in GENERATIONS {
            for flag in [false, true] {
                events.push(OutboxEvent::Reissue {
                    generation,
                    same_payload: flag,
                });
                events.push(OutboxEvent::Result {
                    generation,
                    ok: flag,
                });
                events.push(OutboxEvent::Resolved {
                    generation,
                    ok: flag,
                });
            }
            events.push(OutboxEvent::Claim { generation });
            events.push(OutboxEvent::Restored {
                generation_at_restore: generation,
            });
            events.push(OutboxEvent::Reconciled { generation });
            events.push(OutboxEvent::Consume {
                commit_seq: generation,
            });
        }
        events
    }

    /// Exhaustive-finite over every state shape, event shape and boundary
    /// generation: total, terminal absorbs, generation monotone, fenced rows
    /// never claimed, claims only from `Requested` at the row's generation,
    /// and divergence refused.
    #[test]
    fn every_state_event_pair_keeps_the_step_laws() {
        let mut checked = 0_usize;
        for state in states() {
            for generation in GENERATIONS {
                for event in events() {
                    let stepped = step(state, generation, event);
                    checked = checked.saturating_add(1);
                    if state.is_terminal() {
                        assert_eq!(stepped, Err(OutboxRefusal::Terminal), "{state:?} {event:?}");
                        continue;
                    }
                    if let Ok((_, next)) = stepped {
                        assert!(next >= generation, "{state:?}@{generation} {event:?}");
                    }
                    if let OutboxState::ReconciliationRequired { fenced_at } = state
                        && let OutboxEvent::Claim { .. } = event
                    {
                        assert_eq!(stepped, Err(OutboxRefusal::Fenced { fenced_at }));
                    }
                    if let OutboxEvent::Claim {
                        generation: claimed,
                    } = event
                    {
                        assert_eq!(
                            stepped.is_ok(),
                            state == OutboxState::Requested && claimed == generation,
                            "{state:?}@{generation} {event:?}"
                        );
                    }
                    if let OutboxEvent::Reissue {
                        generation: next,
                        same_payload,
                    } = event
                        && next == generation
                    {
                        let expected = if same_payload {
                            Ok((state, generation))
                        } else {
                            Err(OutboxRefusal::PayloadDiverged)
                        };
                        assert_eq!(stepped, expected, "{state:?}@{generation} {event:?}");
                    }
                }
            }
        }
        assert_eq!(
            Some(checked),
            states()
                .len()
                .checked_mul(GENERATIONS.len())
                .and_then(|pairs| pairs.checked_mul(events().len()))
        );
    }

    fn bump(outcomes: &mut BTreeMap<&'static str, usize>, key: &'static str) {
        let count = outcomes.entry(key).or_default();
        *count = count.saturating_add(1);
    }

    /// What the exploration tracks beside the row: the generations a claim
    /// succeeded at and the highest generation a restore fenced.
    #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
    struct World {
        state: OutboxState,
        generation: u64,
        claimed: BTreeSet<u64>,
        fence: Option<u64>,
    }

    /// Bounded breadth-first exploration of every reachable row from a first
    /// enqueue, over generations 0..=3: a generation is claimed at most once
    /// (`ExecOnce` for a deduplicating provider), a fenced generation is
    /// never claimed, and a claimed generation never returns to `Requested`.
    #[test]
    fn reachable_rows_claim_each_generation_at_most_once_and_never_behind_a_fence() {
        let bounded: Vec<OutboxEvent> = events()
            .into_iter()
            .filter(|event| match *event {
                OutboxEvent::Reissue { generation, .. }
                | OutboxEvent::Claim { generation }
                | OutboxEvent::Result { generation, .. }
                | OutboxEvent::Resolved { generation, .. }
                | OutboxEvent::Reconciled { generation }
                | OutboxEvent::Restored {
                    generation_at_restore: generation,
                } => generation <= 3,
                OutboxEvent::Consume { commit_seq } => commit_seq <= 1,
                OutboxEvent::CrashSeen | OutboxEvent::Compensation { .. } => true,
            })
            .collect();
        let start = World {
            state: enqueued(0).0,
            generation: enqueued(0).1,
            claimed: BTreeSet::new(),
            fence: None,
        };
        let mut seen = BTreeSet::from([start.clone()]);
        let mut queue = VecDeque::from([start]);
        let mut outcomes: BTreeMap<&'static str, usize> = BTreeMap::new();
        while let Some(world) = queue.pop_front() {
            for event in &bounded {
                let Ok((state, generation)) = step(world.state, world.generation, *event) else {
                    bump(&mut outcomes, "refused");
                    continue;
                };
                let mut next = World {
                    state,
                    generation,
                    claimed: world.claimed.clone(),
                    fence: world.fence,
                };
                if let OutboxEvent::Claim {
                    generation: claimed,
                } = *event
                {
                    assert!(
                        next.claimed.insert(claimed),
                        "generation {claimed} claimed twice from {world:?}"
                    );
                    assert!(
                        world.fence.is_none_or(|fence| claimed > fence),
                        "claim at {claimed} behind fence {:?}",
                        world.fence
                    );
                    bump(&mut outcomes, "claimed");
                }
                if let OutboxEvent::Restored {
                    generation_at_restore,
                } = *event
                {
                    next.fence = Some(next.fence.map_or(generation_at_restore, |fence| {
                        fence.max(generation_at_restore)
                    }));
                }
                if next.state == OutboxState::Requested {
                    assert!(
                        !next.claimed.contains(&next.generation),
                        "claimed generation {} returned to Requested",
                        next.generation
                    );
                }
                // Failed compensation attempts only count up; three bound it.
                let bounded_attempts = !matches!(
                    next.state,
                    OutboxState::CompensationPending { attempts } if attempts > 3
                );
                if bounded_attempts && seen.insert(next.clone()) {
                    queue.push_back(next);
                }
            }
        }
        assert!(outcomes.get("claimed").copied().unwrap_or(0) > 0);
        assert!(
            seen.iter()
                .any(|world| world.state == OutboxState::Compensated)
        );
        assert!(
            seen.iter()
                .any(|world| matches!(world.state, OutboxState::Consumed { .. }))
        );
    }

    #[test]
    fn a_fenced_row_is_claimed_only_after_reconciling_a_later_generation() {
        let (state, generation) = step(
            OutboxState::Requested,
            1,
            OutboxEvent::Restored {
                generation_at_restore: 1,
            },
        )
        .unwrap();
        assert_eq!(state, OutboxState::ReconciliationRequired { fenced_at: 1 });
        assert_eq!(
            step(state, generation, OutboxEvent::Reconciled { generation: 1 }),
            Err(OutboxRefusal::Fenced { fenced_at: 1 })
        );
        let (state, generation) = step(
            state,
            generation,
            OutboxEvent::Reissue {
                generation: 2,
                same_payload: false,
            },
        )
        .unwrap();
        assert_eq!(state, OutboxState::ReconciliationRequired { fenced_at: 1 });
        let (state, generation) =
            step(state, generation, OutboxEvent::Reconciled { generation: 2 }).unwrap();
        assert_eq!((state, generation), (OutboxState::Requested, 2));
        assert_eq!(
            step(state, generation, OutboxEvent::Claim { generation: 2 }),
            Ok((OutboxState::Dispatched { generation: 2 }, 2))
        );
    }

    #[test]
    fn a_crash_after_a_claim_is_outcome_unknown_and_a_late_result_still_records() {
        let (state, generation) = step(
            OutboxState::Requested,
            4,
            OutboxEvent::Claim { generation: 4 },
        )
        .unwrap();
        let (state, generation) = step(state, generation, OutboxEvent::CrashSeen).unwrap();
        assert_eq!(state, OutboxState::OutcomeUnknown { generation: 4 });
        assert_eq!(
            step(state, generation, OutboxEvent::Claim { generation: 4 }),
            Err(OutboxRefusal::NotPending)
        );
        assert_eq!(
            step(
                state,
                generation,
                OutboxEvent::Result {
                    generation: 3,
                    ok: true
                }
            ),
            Err(OutboxRefusal::StaleGeneration)
        );
        let (state, generation) = step(
            state,
            generation,
            OutboxEvent::Result {
                generation: 4,
                ok: false,
            },
        )
        .unwrap();
        assert_eq!(state, OutboxState::Failed);
        let (state, _) = step(state, generation, OutboxEvent::Consume { commit_seq: 9 }).unwrap();
        assert_eq!(state, OutboxState::Consumed { commit_seq: 9 });
        assert!(state.is_terminal());
    }

    #[test]
    fn queue_codec_round_trips_and_refuses_unknown_text() {
        for queue in [Queue::Fifo, Queue::Parked] {
            assert_eq!(Queue::parse(queue.as_str()), Some(queue));
        }
        for text in ["", "FIFO", "park", "fifo "] {
            assert_eq!(Queue::parse(text), None, "{text:?}");
        }
    }

    #[test]
    fn the_operation_key_ignores_generation_and_separates_cells_and_commands() {
        let cell = CellId::try_from("0123456789abcdef0123456789abcdef".to_owned()).unwrap();
        let other = CellId::try_from("fedcba9876543210fedcba9876543210".to_owned()).unwrap();
        let command = CommandId("charge/42".to_owned());
        let key = OperationKey::derive(&cell, &command).unwrap();
        assert_eq!(key, OperationKey::derive(&cell, &command).unwrap());
        assert_ne!(key, OperationKey::derive(&other, &command).unwrap());
        assert_ne!(
            key,
            OperationKey::derive(&cell, &CommandId("charge/43".to_owned())).unwrap()
        );
        assert_eq!(OperationKey::from_hex(&key.to_hex()), Some(key));
        assert_eq!(key.to_hex().len(), 64);
        for bad in [
            String::new(),
            "00".to_owned(),
            "g".repeat(64),
            "A".repeat(64),
        ] {
            assert_eq!(OperationKey::from_hex(&bad), None, "{bad:?}");
        }
    }
}
