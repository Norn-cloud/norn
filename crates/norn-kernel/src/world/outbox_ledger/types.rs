//! Core immutable record, proof, and projection types for [`super::OperationLedger`].

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::super::outbox::{OperationKey, OutboxState, Queue};

/// A 32-byte SHA-256 digest encoded as 64 lowercase hexadecimal digits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct OutboxDigest([u8; 32]);

impl OutboxDigest {
    /// Wrap 32 raw digest bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The digest's 32 raw bytes.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Parse 64 lowercase hexadecimal digits into an [`OutboxDigest`].
    #[must_use]
    pub fn from_hex(hex: &str) -> Option<Self> {
        let (pairs, rest) = hex.as_bytes().as_chunks::<2>();
        if pairs.len() != 32 || !rest.is_empty() {
            return None;
        }
        let mut bytes = [0_u8; 32];
        for (byte, [high, low]) in bytes.iter_mut().zip(pairs) {
            *byte = hex_nibble(*high)?
                .checked_mul(16)?
                .checked_add(hex_nibble(*low)?)?;
        }
        Some(Self(bytes))
    }

    /// Render the digest as 64 lowercase hexadecimal digits.
    #[must_use]
    pub fn to_hex(&self) -> String {
        use std::fmt::Write as _;
        self.0
            .iter()
            .fold(String::with_capacity(64), |mut text, byte| {
                // Writing into a `String` is infallible.
                let _written = write!(text, "{byte:02x}");
                text
            })
    }
}

impl fmt::Display for OutboxDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_hex())
    }
}

impl Serialize for OutboxDigest {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_hex())
    }
}

impl<'de> Deserialize<'de> for OutboxDigest {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        Self::from_hex(&text).ok_or_else(|| {
            serde::de::Error::custom("outbox digest must be 64 lowercase hex digits")
        })
    }
}

const fn hex_nibble(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => digit.checked_sub(b'0'),
        b'a'..=b'f' => match digit.checked_sub(b'a') {
            Some(offset) => offset.checked_add(10),
            None => None,
        },
        _ => None,
    }
}

/// Immutable primary key of one generation of a business operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GenerationKey {
    /// Generation-free identity of the business operation.
    pub operation: OperationKey,
    /// Strictly increasing generation number within `operation`.
    pub generation: u64,
}

/// Declared deduplication contract of the external provider performing this
/// operation (`docs/spec/core/07-world-boundary.md`, `EFFECT-IDENTITY-1`).
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "camelCase")]
pub enum ProviderDedupContract {
    /// The provider does not guarantee idempotent deduplication across
    /// reissues. Ambiguous dispatches require durable reconciliation before
    /// issuing a newer generation.
    #[default]
    None,
    /// The provider guarantees idempotent deduplication keyed by the
    /// generation-free [`OperationKey`]. An ambiguous dispatch may be reissued
    /// automatically only when the payload digest is unchanged and no restore
    /// fence is active.
    OperationKeyIdempotent,
}

/// Immutable proof that `(operation, generation)` was durably claimed before
/// external dispatch at `claim_commit_seq`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DispatchProof {
    /// The claimed operation.
    pub operation: OperationKey,
    /// The claimed generation.
    pub generation: u64,
    /// The durable commit sequence that recorded the claim.
    pub claim_commit_seq: u64,
}

/// How an [`OutboxRecordKind::Outcome`] was established.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "camelCase", deny_unknown_fields)]
pub enum OutcomeEvidence {
    /// Provider response tied to its original claim by [`DispatchProof`].
    DispatchProof {
        /// Exact claim proof of the generation that produced this outcome.
        proof: DispatchProof,
    },
    /// Cell-resolved without external dispatch (for example a `Queue::Parked`
    /// cell-issued receipt or a fork's simulated outcome).
    Resolved,
}

/// Provenance recorded on a generation's settled [`RecordedOutcome`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "camelCase", deny_unknown_fields)]
pub enum OutcomeSource {
    /// Established by a provider reply matching the generation's [`DispatchProof`].
    DispatchProof {
        /// The claim commit sequence from the verified [`DispatchProof`].
        claim_commit_seq: u64,
    },
    /// Resolved by the cell without an external dispatch.
    Resolved,
    /// Explicitly confirmed by durable reconciliation evidence.
    Reconciliation,
}

/// Durable reconciliation decision recorded for an ambiguous or fenced generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "camelCase", deny_unknown_fields)]
pub enum ReconciliationDecision {
    /// Provider lookup or operator evidence confirmed the external outcome of
    /// `generation`.
    ConfirmedOutcome {
        /// Whether the confirmed external outcome succeeded.
        ok: bool,
        /// Canonical SHA-256 digest of the confirmed outcome payload.
        outcome_digest: OutboxDigest,
    },
    /// Explicitly authorizes issuing `next_generation` (strictly above the
    /// active generation and any active restore fence).
    AuthorizeReissue {
        /// The exact newer generation authorized for reissue.
        next_generation: u64,
    },
    /// Marks the operation for separately authorized compensation.
    RequireCompensation,
}

/// The immutable fact carried by one [`OutboxRecord`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum OutboxRecordKind {
    /// First enqueue or reissue request for `(operation, generation)`.
    Requested {
        /// Queue assigned at first enqueue; immutable across generations.
        queue: Queue,
        /// Canonical SHA-256 digest of the command payload.
        payload_digest: OutboxDigest,
        /// Provider deduplication contract declared for this generation.
        dedup_contract: ProviderDedupContract,
        /// Commit sequence that enqueued this generation.
        commit_seq: u64,
    },
    /// Durable claim before external dispatch of `(operation, generation)`.
    Dispatched {
        /// Commit sequence that durably recorded the claim.
        claim_commit_seq: u64,
    },
    /// Recorded outcome for `(operation, generation)`.
    Outcome {
        /// Evidence binding this outcome to its generation.
        evidence: OutcomeEvidence,
        /// Whether the outcome is a success.
        ok: bool,
        /// Canonical SHA-256 digest of the recorded outcome payload.
        outcome_digest: OutboxDigest,
        /// Commit sequence at which this outcome was recorded.
        commit_seq: u64,
    },
    /// Host restart observed an in-flight dispatch whose external outcome is
    /// unknown; preserves the generation's [`DispatchProof`].
    CrashObserved,
    /// Restore fenced the operation at `generation`.
    RestoreFenced {
        /// Commit sequence / restore marker sequence.
        commit_seq: u64,
    },
    /// Durable reconciliation decision for an ambiguous or fenced generation.
    Reconciliation {
        /// The reconciliation decision taken.
        decision: ReconciliationDecision,
        /// Commit sequence that durably recorded reconciliation.
        commit_seq: u64,
    },
    /// Compensation attempt outcome for `generation`.
    Compensation {
        /// Whether compensation completed.
        ok: bool,
        /// Commit sequence recording the compensation attempt.
        commit_seq: u64,
    },
    /// Consuming commit settled the active generation's recorded outcome.
    Consumed {
        /// Commit sequence that consumed the outcome.
        commit_seq: u64,
    },
}

/// One append-only, immutable outbox ledger record keyed by `(operation, generation)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OutboxRecord {
    /// The generation-free business operation identity.
    pub operation: OperationKey,
    /// The generation this record pertains to.
    pub generation: u64,
    /// The immutable lifecycle fact recorded for `(operation, generation)`.
    pub kind: OutboxRecordKind,
}

impl OutboxRecord {
    /// The `(operation, generation)` key of this record.
    #[must_use]
    pub const fn key(&self) -> GenerationKey {
        GenerationKey {
            operation: self.operation,
            generation: self.generation,
        }
    }
}

/// Outcome of applying one [`OutboxRecord`] to an [`super::OperationLedger`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ApplyStatus {
    /// The record advanced or appended immutable state on the ledger.
    Applied,
    /// The record was an exact idempotent duplicate of already-recorded state.
    IdempotentDuplicate,
}

/// Why [`super::OperationLedger::start`], [`super::OperationLedger::apply`], or
/// [`super::OperationLedger::replay`] refused a record. A refusal never mutates state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, thiserror::Error)]
pub enum OutboxLedgerRefusal {
    /// Cannot replay an empty record slice.
    #[error("cannot replay an empty outbox record log")]
    EmptyLog,
    /// The first record for an operation must be `Requested`.
    #[error("an operation ledger must begin with a Requested record")]
    NotInitialized,
    /// The record targets a different [`OperationKey`] than this ledger.
    #[error("record operation key does not match this outbox ledger")]
    OperationMismatch,
    /// The record names a generation that has never been requested.
    #[error("record references an unknown outbox generation")]
    UnknownGeneration,
    /// The record names an older or non-active generation where only the active
    /// generation is permitted.
    #[error("record names a stale or non-active outbox generation")]
    StaleGeneration,
    /// Same-generation `Requested` carried a different payload digest or dedup
    /// contract.
    #[error("same-generation reissue carries a different payload digest or dedup contract")]
    PayloadDiverged,
    /// A newer generation tried to move the operation to a different [`Queue`].
    #[error("outbox queue is fixed at first enqueue and cannot change across generations")]
    QueueChanged,
    /// A `Queue::Parked` command cannot be claimed for external dispatch.
    #[error("parked outbox commands cannot be claimed for external dispatch")]
    ParkedCannotDispatch,
    /// The generation was already durably claimed (`Dispatched`).
    #[error("outbox generation has already been durably claimed")]
    AlreadyClaimed,
    /// The supplied [`DispatchProof`] does not match the generation's durable claim.
    #[error("dispatch proof does not match the generation's durable claim")]
    InvalidDispatchProof,
    /// A conflicting outcome was submitted for a generation that already has a
    /// recorded outcome.
    #[error("generation already has a different recorded outcome")]
    OutcomeDiverged,
    /// Reissuing over an ambiguous dispatch requires durable reconciliation (or
    /// `ProviderDedupContract::OperationKeyIdempotent` with an identical payload).
    #[error(
        "ambiguous dispatch at generation {generation} requires durable reconciliation before reissue"
    )]
    AmbiguousDispatchRequiresReconciliation {
        /// The generation whose external outcome is still ambiguous.
        generation: u64,
    },
    /// A restore fence at `fenced_at` blocks this transition until durable
    /// reconciliation resolves or authorizes a generation strictly above it.
    #[error("outbox operation is fenced at generation {fenced_at} pending reconciliation")]
    Fenced {
        /// Highest generation fenced by a restore.
        fenced_at: u64,
    },
    /// The record does not apply to the operation's current lifecycle state.
    #[error("record does not apply to the outbox operation's current state")]
    NotPending,
    /// The operation is terminal (`Consumed` or `Compensated`) and refuses
    /// further lifecycle transitions (except late verified dispatch-proof
    /// evidence attaching to a prior dispatched generation).
    #[error("outbox operation is terminal")]
    Terminal,
}

/// Immutable recorded outcome attached to one generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecordedOutcome {
    /// How this outcome was established.
    pub source: OutcomeSource,
    /// Whether the outcome succeeded.
    pub ok: bool,
    /// Canonical SHA-256 digest of the outcome payload.
    pub outcome_digest: OutboxDigest,
    /// Commit sequence at which this outcome was recorded.
    pub commit_seq: u64,
}

/// One append-only reconciliation entry recorded on a generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReconciliationEntry {
    /// The reconciliation decision recorded.
    pub decision: ReconciliationDecision,
    /// Commit sequence at which reconciliation was recorded.
    pub commit_seq: u64,
}

/// Immutable per-generation history retained for `(operation, generation)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GenerationRecord {
    /// Generation number.
    pub generation: u64,
    /// Queue fixed at first enqueue.
    pub queue: Queue,
    /// Canonical payload digest of this generation.
    pub payload_digest: OutboxDigest,
    /// Provider deduplication contract declared for this generation.
    pub dedup_contract: ProviderDedupContract,
    /// Commit sequence that enqueued this generation.
    pub enqueue_commit_seq: u64,
    /// Durable dispatch proof if this generation was claimed (write-once).
    pub dispatch: Option<DispatchProof>,
    /// Whether a crash was observed while this generation's dispatch had no outcome.
    pub outcome_unknown: bool,
    /// Immutable outcome once established for this generation (write-once).
    pub outcome: Option<RecordedOutcome>,
    /// Restore fence commit sequence if restored at this generation.
    pub fenced_commit_seq: Option<u64>,
    /// Whether durable reconciliation confirmed this generation's outcome
    /// after its most recent restore fence (cleared on every `RestoreFenced`).
    pub reconciled_since_restore: bool,
    /// Append-only reconciliation records targeting this generation.
    pub reconciliations: Vec<ReconciliationEntry>,
    /// Commit sequence that consumed this generation's outcome, if any (write-once).
    pub consumed_commit_seq: Option<u64>,
}

impl GenerationRecord {
    /// Whether this generation was durably claimed and still has no outcome.
    #[must_use]
    pub const fn is_ambiguous_dispatch(&self) -> bool {
        self.dispatch.is_some() && self.outcome.is_none()
    }

    /// Whether durable reconciliation confirmed this generation's outcome
    /// after any restore fence on this generation.
    #[must_use]
    pub const fn has_reconciled_outcome(&self) -> bool {
        self.reconciled_since_restore
    }
}

/// Bound reissue authorization tying a reconciliation decision to the exact
/// source generation that authorized `next_generation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReissueAuthorization {
    /// The active generation that recorded `AuthorizeReissue`.
    pub from_generation: u64,
    /// The exact newer generation authorized for reissue.
    pub next_generation: u64,
}

/// Compensation lifecycle status on an [`super::OperationLedger`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "camelCase", deny_unknown_fields)]
pub enum CompensationStatus {
    /// Compensation was requested and has failed `attempts` times so far.
    Pending {
        /// The generation being compensated.
        for_generation: u64,
        /// Count of failed compensation attempts.
        attempts: u32,
    },
    /// Compensation succeeded. Terminal.
    Compensated {
        /// The generation that was compensated.
        for_generation: u64,
        /// Commit sequence recording completion.
        commit_seq: u64,
    },
}

/// Pure projection of the mutable queue head derived solely from an
/// [`super::OperationLedger`].
///
/// Reflects only the active generation: late evidence attaching to superseded
/// prior generations never alters `HeadProjection` (inspect prior successes via
/// [`super::OperationLedger::prior_confirmed_successes`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct HeadProjection {
    /// Business operation identity.
    pub operation: OperationKey,
    /// Queue fixed at first enqueue.
    pub queue: Queue,
    /// Current active generation.
    pub generation: u64,
    /// Payload digest of the active generation.
    pub payload_digest: OutboxDigest,
    /// Declared provider dedup contract of the active generation.
    pub dedup_contract: ProviderDedupContract,
    /// Derived D03 lifecycle state of the active head.
    pub state: OutboxState,
    /// Visible outcome of the active head (`None` if unfenced outcome absent or
    /// masked by an unreconciled restore fence). Never carries an older
    /// generation's outcome.
    pub outcome: Option<RecordedOutcome>,
}
