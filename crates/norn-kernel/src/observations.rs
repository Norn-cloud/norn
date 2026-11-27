//! Turn inputs supplied by the host: logical time, entropy, and world.
//!
//! The kernel never reads a clock or a random source. Whatever a decision needs
//! of either arrives here, is recorded with the turn, and is therefore
//! replayable.

use serde::{Deserialize, Serialize};

/// A point in time as delivered by the host, in milliseconds since the Unix
/// epoch.
///
/// It is an *input* (the host's `clock`), never a reading taken by the kernel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct LogicalTime(u64);

impl LogicalTime {
    /// The Unix epoch.
    pub const EPOCH: Self = Self(0);

    /// Wraps a millisecond timestamp.
    #[must_use]
    pub const fn from_millis(millis: u64) -> Self {
        Self(millis)
    }

    /// The timestamp in milliseconds since the Unix epoch.
    #[must_use]
    pub const fn as_millis(self) -> u64 {
        self.0
    }

    /// `self + millis`, or `None` on overflow.
    #[must_use]
    pub const fn checked_add_millis(self, millis: u64) -> Option<Self> {
        match self.0.checked_add(millis) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }
}

/// 32 bytes of host-supplied entropy for one turn.
///
/// Identifiers or samples a decision derives from it are reproducible because
/// the bytes are recorded with the turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Entropy([u8; 32]);

impl Entropy {
    /// Wraps 32 entropy bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The entropy bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// Opaque identifier of a forked world.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorldId(pub String);

/// Opaque identifier of a committed ledger position.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CommitId(pub String);

/// The world a turn runs in (ruling 2026-09-25 §2).
///
/// Only [`World::Main`] may `perform`; a fork produces plans and diffs only; a
/// historical view is read-only.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum World {
    /// The tenant's authoritative world.
    Main,
    /// A scenario fork: it may commit into itself but never performs.
    Fork {
        /// The fork's identity.
        id: WorldId,
    },
    /// A read-only view as of one commit.
    Historical {
        /// The commit the view is pinned to.
        commit: CommitId,
    },
}

impl World {
    /// Whether commands may be performed against the outside world.
    #[must_use]
    pub const fn may_perform(&self) -> bool {
        matches!(self, Self::Main)
    }

    /// Whether the world accepts new events at all.
    #[must_use]
    pub const fn is_writable(&self) -> bool {
        !matches!(self, Self::Historical { .. })
    }
}

/// Core 08 (`WORLD-FORK`): explicit bitemporal scenario clock separating
/// effective business time from recorded ledger/knowledge time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScenarioClock {
    /// Effective business time observed by scenario evaluation and rules.
    pub effective: LogicalTime,
    /// Recorded ledger/knowledge time stamped on fork commits and timers.
    pub recorded: LogicalTime,
}

impl ScenarioClock {
    /// Constructs a scenario clock with independent effective and recorded instants.
    #[must_use]
    pub const fn new(effective: LogicalTime, recorded: LogicalTime) -> Self {
        Self {
            effective,
            recorded,
        }
    }

    /// Constructs a scenario clock where effective and recorded time start equal.
    #[must_use]
    pub const fn synchronized(now: LogicalTime) -> Self {
        Self {
            effective: now,
            recorded: now,
        }
    }

    /// Monotonically advances the effective clock to `effective`, returning
    /// `None` if `effective` would move backward.
    #[must_use]
    pub const fn advance_effective_to(self, effective: LogicalTime) -> Option<Self> {
        if effective.as_millis() < self.effective.as_millis() {
            return None;
        }
        Some(Self {
            effective,
            recorded: self.recorded,
        })
    }

    /// Monotonically advances the recorded clock to `recorded`, returning
    /// `None` if `recorded` would move backward.
    #[must_use]
    pub const fn advance_recorded_to(self, recorded: LogicalTime) -> Option<Self> {
        if recorded.as_millis() < self.recorded.as_millis() {
            return None;
        }
        Some(Self {
            effective: self.effective,
            recorded,
        })
    }

    /// Advances the effective clock by `delta_ms`, or `None` on overflow.
    #[must_use]
    pub const fn advance_effective_by(self, delta_ms: u64) -> Option<Self> {
        match self.effective.checked_add_millis(delta_ms) {
            Some(effective) => Some(Self {
                effective,
                recorded: self.recorded,
            }),
            None => None,
        }
    }

    /// Advances the recorded clock by `delta_ms`, or `None` on overflow.
    #[must_use]
    pub const fn advance_recorded_by(self, delta_ms: u64) -> Option<Self> {
        match self.recorded.checked_add_millis(delta_ms) {
            Some(recorded) => Some(Self {
                effective: self.effective,
                recorded,
            }),
            None => None,
        }
    }

    /// Advances both effective and recorded clocks by `delta_ms`, or `None` on overflow.
    #[must_use]
    pub const fn advance_both_by(self, delta_ms: u64) -> Option<Self> {
        match (
            self.effective.checked_add_millis(delta_ms),
            self.recorded.checked_add_millis(delta_ms),
        ) {
            (Some(effective), Some(recorded)) => Some(Self {
                effective,
                recorded,
            }),
            _ => None,
        }
    }
}

/// Everything nondeterministic a turn may observe, supplied by the host.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observations {
    /// The host's logical time for this turn.
    pub now: LogicalTime,
    /// Entropy for this turn.
    pub entropy: Entropy,
    /// The world the turn runs in.
    pub world: World,
}

impl Observations {
    /// Observations for a turn in the main world.
    #[must_use]
    pub const fn main(now: LogicalTime, entropy: Entropy) -> Self {
        Self {
            now,
            entropy,
            world: World::Main,
        }
    }

    /// Observations for a turn in a simulated fork world under its virtual
    /// recorded clock and recorded entropy (`WORLD-FORK`).
    #[must_use]
    pub const fn fork(id: WorldId, recorded: LogicalTime, entropy: Entropy) -> Self {
        Self {
            now: recorded,
            entropy,
            world: World::Fork { id },
        }
    }

    /// Observations for a read-only historical evaluation pinned to `commit`.
    #[must_use]
    pub const fn historical(commit: CommitId, now: LogicalTime, entropy: Entropy) -> Self {
        Self {
            now,
            entropy,
            world: World::Historical { commit },
        }
    }
}

/// Core 08 (`WORLD-FORK`): replayable turn observations for a simulated fork
/// world, carrying both virtual effective business time and virtual recorded
/// ledger time alongside recorded entropy and world identity.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ScenarioObservations {
    /// Virtual effective business clock and virtual recorded ledger clock.
    pub clock: ScenarioClock,
    /// Deterministic recorded entropy for this turn.
    pub entropy: Entropy,
    /// Execution world (`World::Fork { id }`).
    pub world: World,
}

impl ScenarioObservations {
    /// Project into single-clock kernel [`Observations`] using the virtual
    /// recorded clock for ledger stamping and timer evaluation.
    #[must_use]
    pub fn as_observations(&self) -> Observations {
        Observations {
            now: self.clock.recorded,
            entropy: self.entropy,
            world: self.world.clone(),
        }
    }
}
