//! Ingress limits at the kernel boundary (T2-LIM-1, ruling 2026-09-27 §1).
//!
//! The limit values are one versioned [`ResourceLimitProfile`] (defined and
//! content-hashed in `norn-ir`; the shipped default is `norn.limits/1`). This
//! module holds the pure, cheap checks the kernel can apply today:
//!
//! - envelope counts (events, commands, timers) — enforced by
//!   [`step`](crate::step) against [`EnvelopeLimits`];
//! - envelope bytes — [`check_envelope_bytes`], called by the host on the bytes
//!   it serializes (the kernel is generic over event/command types and does
//!   not serialize them);
//! - the declared input slice — [`SliceMeter`], which a slice reader charges
//!   per record **while reading**, so an oversized slice is refused before it
//!   is assembled and before any `decide` runs.
//!
//! A host can enforce caller-configured aggregate event/command/timer counts
//! with [`crate::TurnEnvelopeMeter`]. The limit profile does not define those
//! aggregate counts yet. Fuel calibration, whole-host turn budgeting, and
//! allocation metering remain `T3-BUDGET-1` (#2775). Every number in the
//! shipped profile is a v1 qualification target pending measurement.

use std::fmt;

use serde::{Deserialize, Serialize};

pub use norn_ir::limit_profile::{
    DecideBudget, EnvelopeLimits, HelperLimits, LIMIT_PROFILE_HASH_PROFILE, LimitProfileRef,
    MemoryLimits, NORN_LIMITS_PROFILE_ID, NORN_LIMITS_V1_SHA256, NORN_LIMITS_V1_VERSION,
    ResourceLimitProfile, SliceLimits, TurnBudget,
};

/// Which ingress limit was exceeded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IngressLimit {
    /// [`SliceLimits::max_canonical_bytes`].
    SliceBytes,
    /// [`SliceLimits::max_records`].
    SliceRecords,
    /// [`SliceLimits::max_relation_depth`].
    SliceRelationDepth,
    /// [`EnvelopeLimits::max_output_bytes`].
    EnvelopeBytes,
}

impl fmt::Display for IngressLimit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::SliceBytes => "slice bytes",
            Self::SliceRecords => "slice records",
            Self::SliceRelationDepth => "slice relation depth",
            Self::EnvelopeBytes => "envelope bytes",
        })
    }
}

/// An input refused at the boundary: no decision ran, so there are no events
/// and no effects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, thiserror::Error)]
#[serde(deny_unknown_fields)]
#[error("{kind} limit exceeded: {actual} > {limit}")]
pub struct IngressLimitExceeded {
    /// Which limit.
    pub kind: IngressLimit,
    /// The limit.
    pub limit: u64,
    /// The value that would have been admitted (saturating).
    pub actual: u64,
}

/// Checks the serialized size of one decision envelope.
///
/// # Errors
///
/// Returns [`IngressLimitExceeded`] with [`IngressLimit::EnvelopeBytes`] when
/// `serialized_bytes` exceeds [`EnvelopeLimits::max_output_bytes`].
pub const fn check_envelope_bytes(
    limits: &EnvelopeLimits,
    serialized_bytes: u64,
) -> Result<(), IngressLimitExceeded> {
    if serialized_bytes > limits.max_output_bytes {
        return Err(IngressLimitExceeded {
            kind: IngressLimit::EnvelopeBytes,
            limit: limits.max_output_bytes,
            actual: serialized_bytes,
        });
    }
    Ok(())
}

/// Charges a declared input slice against [`SliceLimits`] while it is read.
///
/// The reader calls [`SliceMeter::admit_record`] **before** appending each
/// selected record. The first refusal is sticky: every later call returns the
/// same error, so a reader cannot resume past a limit. A refused slice must
/// not reach `decide`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SliceMeter {
    limits: SliceLimits,
    bytes: u64,
    records: u32,
    refused: Option<IngressLimitExceeded>,
}

impl SliceMeter {
    /// A meter for `limits` with nothing admitted.
    #[must_use]
    pub const fn new(limits: SliceLimits) -> Self {
        Self {
            limits,
            bytes: 0,
            records: 0,
            refused: None,
        }
    }

    /// A meter for the slice limits of `profile`.
    #[must_use]
    pub const fn for_profile(profile: &ResourceLimitProfile) -> Self {
        Self::new(profile.slice)
    }

    /// Admits one record of `canonical_bytes` canonical input bytes reached at
    /// `relation_depth` (0 for a root record). Checks, in order: relation
    /// depth, record count, cumulative bytes. On refusal nothing is charged.
    ///
    /// # Errors
    ///
    /// Returns the first exceeded limit, or the earlier refusal if the meter
    /// has already refused.
    pub fn admit_record(
        &mut self,
        canonical_bytes: u64,
        relation_depth: u32,
    ) -> Result<(), IngressLimitExceeded> {
        if let Some(refused) = self.refused {
            return Err(refused);
        }
        let records = self.records.saturating_add(1);
        let bytes = self.bytes.saturating_add(canonical_bytes);
        let exceeded = if relation_depth > self.limits.max_relation_depth {
            Some((
                IngressLimit::SliceRelationDepth,
                u64::from(self.limits.max_relation_depth),
                u64::from(relation_depth),
            ))
        } else if records > self.limits.max_records {
            Some((
                IngressLimit::SliceRecords,
                u64::from(self.limits.max_records),
                u64::from(records),
            ))
        } else if bytes > self.limits.max_canonical_bytes {
            Some((
                IngressLimit::SliceBytes,
                self.limits.max_canonical_bytes,
                bytes,
            ))
        } else {
            None
        };
        if let Some((kind, limit, actual)) = exceeded {
            let refused = IngressLimitExceeded {
                kind,
                limit,
                actual,
            };
            self.refused = Some(refused);
            return Err(refused);
        }
        self.records = records;
        self.bytes = bytes;
        Ok(())
    }

    /// Canonical bytes admitted so far.
    #[must_use]
    pub const fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Records admitted so far.
    #[must_use]
    pub const fn records(&self) -> u32 {
        self.records
    }

    /// The refusal, if the meter has refused.
    #[must_use]
    pub const fn refused(&self) -> Option<IngressLimitExceeded> {
        self.refused
    }
}
