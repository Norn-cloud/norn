//! Decision-output envelope limits and an aggregate meter for a synchronous turn.
//!
//! The per-decision limits are the versioned profile values in
//! [`EnvelopeLimits`]. Aggregate count limits are host supplied until the
//! qualification work in `T3-BUDGET-1` publishes calibrated values.

use crate::decider::EnvelopeKind;
use crate::limits::EnvelopeLimits;

/// Counts in one decision output, or in the effects of one `step`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EnvelopeCounts {
    pub(crate) events: usize,
    pub(crate) commands: usize,
    pub(crate) timers: usize,
}

impl EnvelopeCounts {
    pub(crate) const fn new(events: usize, commands: usize, timers: usize) -> Self {
        Self {
            events,
            commands,
            timers,
        }
    }
}

/// A per-decision limit violation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EnvelopeExceeded {
    pub(crate) kind: EnvelopeKind,
    pub(crate) limit: u32,
    pub(crate) actual: usize,
}

/// Checks one decision's event, command, and timer counts.
pub(crate) fn check_decision_envelope(
    limits: &EnvelopeLimits,
    counts: EnvelopeCounts,
) -> Result<(), EnvelopeExceeded> {
    for (kind, limit, actual) in [
        (EnvelopeKind::Events, limits.max_events, counts.events),
        (EnvelopeKind::Commands, limits.max_commands, counts.commands),
        (EnvelopeKind::Timers, limits.max_timers, counts.timers),
    ] {
        // A count that does not fit in u32 exceeds every per-decision cap.
        if u32::try_from(actual).map_or(true, |actual| actual > limit) {
            return Err(EnvelopeExceeded {
                kind,
                limit,
                actual,
            });
        }
    }
    Ok(())
}

/// Explicit aggregate event, command, and timer limits for one cell turn.
///
/// The values are supplied by the host because `norn.limits/1` does not yet
/// define calibrated aggregate count caps. They are not part of, or hashed
/// into, that resource profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TurnEnvelopeLimits {
    /// Maximum events across all commits admitted to this turn.
    pub max_events: u64,
    /// Maximum commands across all commits admitted to this turn.
    pub max_commands: u64,
    /// Maximum timers across all commits admitted to this turn.
    pub max_timers: u64,
}

/// Envelope counts committed so far in one turn.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TurnEnvelopeUsage {
    /// Events committed so far.
    pub events: u64,
    /// Commands committed so far.
    pub commands: u64,
    /// Timers committed so far.
    pub timers: u64,
}

/// Shared, synchronous aggregate envelope meter for a multi-commit turn.
///
/// A host creates one meter at the start of a turn and passes it to each
/// `commit_decision_with_meter` or `commit_action_with_meter` call. Successful
/// commits charge the meter; a replay or refusal leaves it unchanged. The
/// default per-decision limits are `EnvelopeLimits::V1`; use [`Self::with_limits`]
/// when the host has an explicitly selected per-decision profile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnEnvelopeMeter {
    decision_limits: EnvelopeLimits,
    limits: TurnEnvelopeLimits,
    used: TurnEnvelopeUsage,
}

impl TurnEnvelopeMeter {
    /// Starts a turn meter with the default v1 per-decision limits and
    /// host-supplied aggregate limits.
    #[must_use]
    pub const fn new(limits: TurnEnvelopeLimits) -> Self {
        Self::with_limits(EnvelopeLimits::V1, limits)
    }

    /// Starts a turn meter with explicit per-decision and aggregate limits.
    #[must_use]
    pub const fn with_limits(decision_limits: EnvelopeLimits, limits: TurnEnvelopeLimits) -> Self {
        Self {
            decision_limits,
            limits,
            used: TurnEnvelopeUsage {
                events: 0,
                commands: 0,
                timers: 0,
            },
        }
    }

    /// The per-decision limits checked by commits using this meter.
    #[must_use]
    pub const fn decision_limits(&self) -> EnvelopeLimits {
        self.decision_limits
    }

    /// The aggregate limits for this turn.
    #[must_use]
    pub const fn limits(&self) -> TurnEnvelopeLimits {
        self.limits
    }

    /// The successfully committed envelope counts so far.
    #[must_use]
    pub const fn used(&self) -> TurnEnvelopeUsage {
        self.used
    }

    pub(crate) fn check(&self, counts: EnvelopeCounts) -> Result<(), TurnEnvelopeExceeded> {
        for (kind, used, limit, added) in [
            (
                EnvelopeKind::Events,
                self.used.events,
                self.limits.max_events,
                counts.events,
            ),
            (
                EnvelopeKind::Commands,
                self.used.commands,
                self.limits.max_commands,
                counts.commands,
            ),
            (
                EnvelopeKind::Timers,
                self.used.timers,
                self.limits.max_timers,
                counts.timers,
            ),
        ] {
            let added = u64::try_from(added).unwrap_or(u64::MAX);
            let actual = used.checked_add(added);
            if actual.is_none_or(|actual| actual > limit) {
                return Err(TurnEnvelopeExceeded {
                    kind,
                    limit,
                    actual: actual.unwrap_or(u64::MAX),
                });
            }
        }
        Ok(())
    }

    pub(crate) fn charge(&mut self, counts: EnvelopeCounts) {
        self.used.events = self
            .used
            .events
            .saturating_add(u64::try_from(counts.events).unwrap_or(u64::MAX));
        self.used.commands = self
            .used
            .commands
            .saturating_add(u64::try_from(counts.commands).unwrap_or(u64::MAX));
        self.used.timers = self
            .used
            .timers
            .saturating_add(u64::try_from(counts.timers).unwrap_or(u64::MAX));
    }
}

/// An aggregate turn envelope limit violation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TurnEnvelopeExceeded {
    pub(crate) kind: EnvelopeKind,
    pub(crate) limit: u64,
    pub(crate) actual: u64,
}
