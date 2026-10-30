//! The four-verb [`CellHost`] trait (T4-CELL-1, ruling 2026-09-25 §5).
//!
//! The impure perimeter of a tenant cell is exactly four verbs, exposed to the
//! kernel as this one trait and implemented once per host (the native
//! `norn-cell` test driver, the TypeScript Durable Objects shim, celld). It
//! replaces the 2026-09-09 `DurableContext`: there is no `run_step`,
//! `await_signal`, or child spawn, and no workflow API of any kind.
//!
//! 1. **commit** ([`CellHost::commit`]) — apply fact and link writes, append ledger
//!    events, and enqueue the resulting commands and timers **in one
//!    transaction**, while consuming the delivery that triggered the turn.
//!    There is no dual write inside the cell.
//! 2. **perform** ([`CellHost::perform`]) — execute the oldest pending
//!    command against the outside world, idempotently, and hand its result back
//!    as a kernel input.
//! 3. **observe** ([`CellHost::observe`]) — hand the next external observation
//!    (connector, CDC, webhook) back as a kernel input, with its provenance.
//! 4. **clock** ([`CellHost::clock`]) — hand the next due timer back as a
//!    kernel input.
//!
//! Plus the two hooks the ruling names beside them: [`CellHost::serialize_by`]
//! (per-entity ordering on hosts with concurrent writers, §7) and
//! [`CellHost::publish`] (reactive-query push after a commit, §10), and the
//! read side [`CellHost::load`], which is not an effect: it reads the cell's
//! own committed state, which is a function of the commit order.
//!
//! # Delivery and exactly-once consumption
//!
//! `perform`, `observe`, and `clock` *peek*: each returns the next pending
//! delivery without removing it, and returns the same delivery again until a
//! commit consumes it ([`CellCommit::consumes`]). Consumption is part of the
//! commit transaction, so a crash between delivery and commit redelivers the
//! input, and a committed turn never sees its input again. `perform` executes
//! a command at most once per generation: a redelivered result is the recorded
//! one.
//!
//! # Synchronous and sans-IO
//!
//! The trait is synchronous and its data is plain serializable values: the
//! kernel stays `wasm32`, performs no IO itself, and the host decides how a
//! verb reaches storage or the network. [`check_commit`] is the pure,
//! host-independent admission check every host runs before applying an
//! ordinary commit, so all hosts refuse the same commits in the same order.
//! The kernel's parent/owned-row action uses the separate in-process
//! [`OwnedRowsCommitProof`] path; portable serialized commits never carry that
//! authority.

use std::fmt;

use norn_ir::posting::AccountingError;
use norn_ir::{DataQualityOutcome, InvariantId, InvariantOwner};
use serde::{Deserialize, Serialize};

pub use norn_ir::{
    InterfaceDef, Link, LinkCardinality, LinkId, Multiplicity, ObjectTypeDef, ObjectTypeName,
    PropertyTypeDef, ResourceStatus,
};

/// An object's property values, a command payload, or a timer payload: an
/// exact runtime object (#2770, [`norn_ir::RuntimeObject`]). Numbers are
/// exact decimals; nothing on the runtime path passes through `f64`.
pub type JsonObject = norn_ir::RuntimeObject;

/// One exact runtime value ([`norn_ir::RuntimeValue`]).
pub type JsonValue = norn_ir::RuntimeValue;

use crate::effect::{CommandId, Effect, Generation, Timer, TimerId};
use crate::observations::LogicalTime;
use crate::snapshot_observe::{ObservationAuthorization, ObservationEvent};

/// One-use in-process authority for the kernel's atomic owned-row commit.
///
/// The proof has no wire representation and no public constructor. It is
/// created only after the kernel has prepared and authorized the complete
/// parent/child mutation, then consumed by the host admission check.
pub struct OwnedRowsCommitProof {
    max_events: u32,
    consumes: Option<Consumed>,
    reads: Vec<FactExpectation>,
    writes: Vec<FactWrite>,
    links: Vec<LinkWrite>,
    link_reads: Vec<LinkReadExpectation>,
}

impl OwnedRowsCommitProof {
    pub(crate) fn new<E, C, T>(commit: &CellCommit<E, C, T>, max_events: u32) -> Self {
        Self {
            max_events,
            consumes: commit.consumes.clone(),
            reads: commit.reads.clone(),
            writes: commit.writes.clone(),
            links: commit.links.clone(),
            link_reads: commit.link_reads.clone(),
        }
    }

    pub(crate) const fn max_events(&self) -> u32 {
        self.max_events
    }

    pub(crate) const fn consumes(&self) -> Option<&Consumed> {
        self.consumes.as_ref()
    }

    pub(crate) fn reads(&self) -> &[FactExpectation] {
        &self.reads
    }

    pub(crate) fn writes(&self) -> &[FactWrite] {
        &self.writes
    }

    pub(crate) fn links(&self) -> &[LinkWrite] {
        &self.links
    }

    pub(crate) fn link_reads(&self) -> &[LinkReadExpectation] {
        &self.link_reads
    }
}

/// Identity of one ontology object in a cell: its type and identity key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FactKey {
    /// The object type.
    pub object_type: ObjectTypeName,
    /// The object's identity key value.
    pub key: String,
}

impl FactKey {
    /// The key of object `key` of type `object_type`.
    #[must_use]
    pub fn new(object_type: impl Into<String>, key: impl Into<String>) -> Self {
        Self {
            object_type: ObjectTypeName(object_type.into()),
            key: key.into(),
        }
    }
}

impl fmt::Display for FactKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.object_type.as_str(), self.key)
    }
}

/// Version of one object, bumped by every committed write to it.
///
/// [`FactVersion::ABSENT`] (`0`) means the object was never written. A delete
/// bumps the version and leaves a tombstone, so versions never repeat for a
/// key and a compare-and-swap can never be fooled by delete-then-recreate.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct FactVersion(pub u64);

impl FactVersion {
    /// The version of an object that was never written.
    pub const ABSENT: Self = Self(0);

    /// The next version, or `None` on overflow.
    #[must_use]
    pub const fn checked_next(self) -> Option<Self> {
        match crate::version_check::checked_next_version(self.0) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }
}

impl fmt::Display for FactVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "v{}", self.0)
    }
}

/// The committed state of one object as [`CellHost::load`] reads it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FactState {
    /// The object's version ([`FactVersion::ABSENT`] if never written).
    pub version: FactVersion,
    /// Its property values; `None` when absent or deleted.
    pub values: Option<JsonObject>,
}

/// One bounded fact read, including the concrete type when the requested
/// type is a supertype. The kernel uses `state` for CAS and preconditions and
/// the concrete type for object-level read authorization.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BoundedFact {
    /// Concrete object type when the object exists; `None` when absent.
    pub concrete_type: Option<ObjectTypeName>,
    /// The committed state at the requested key's version boundary.
    pub state: FactState,
}

impl FactState {
    /// Whether the object currently exists.
    #[must_use]
    pub const fn exists(&self) -> bool {
        self.values.is_some()
    }
}

/// A compare-and-swap expectation on an object the turn read but does not
/// write (for example an object a guard read).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FactExpectation {
    /// The object.
    pub key: FactKey,
    /// The version the turn read.
    pub expected: FactVersion,
}

/// One live target observed while reading a complete bounded link slice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LinkReadTarget {
    /// The observed link identity.
    pub key: LinkKey,
    /// The version observed for that link.
    pub version: FactVersion,
}

/// A complete, bounded read of one source/link adjacency set.
///
/// Unlike a sequence of per-link expectations this also protects against
/// phantom additions and removals: the commit compares the entire ordered
/// target set, including each target link's version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LinkReadExpectation {
    /// The source object whose adjacency set was read.
    pub source: FactKey,
    /// The declared link name.
    pub link: String,
    /// Every live target observed, in canonical target-key order.
    pub targets: Vec<LinkReadTarget>,
}

/// What a write does to an object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum FactMutation {
    /// Replace the object's property values (create or update).
    Put {
        /// The complete property values after the write.
        values: JsonObject,
    },
    /// Delete the object, leaving a tombstone version.
    Delete,
}

/// One object write, guarded by the version the turn read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FactWrite {
    /// The object.
    pub key: FactKey,
    /// The version the turn read; the write commits only if it is current.
    pub expected: FactVersion,
    /// The mutation.
    pub mutation: FactMutation,
}

/// Identity of one link between two objects: its source, the link id
/// declared on the source's type, and its target (#3013).
///
/// Links are versioned like objects: every committed change to a link bumps
/// its version, and a removed link keeps its version, so a compare-and-swap
/// on a link can never be fooled by remove-then-add.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LinkKey {
    /// The source object.
    pub source: FactKey,
    /// The link id on the source object's type.
    pub link: String,
    /// The target object.
    pub target: FactKey,
}

impl LinkKey {
    /// The link `link` from `source` to `target`.
    #[must_use]
    pub fn new(source: FactKey, link: impl Into<String>, target: FactKey) -> Self {
        Self {
            source,
            link: link.into(),
            target,
        }
    }
}

impl fmt::Display for LinkKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} -{}-> {}", self.source, self.link, self.target)
    }
}

/// The committed state of one link as [`CellHost::load_link`] reads it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkState {
    /// The link's version ([`FactVersion::ABSENT`] if never written).
    pub version: FactVersion,
    /// Its link property values; `None` when absent or removed.
    pub values: Option<JsonObject>,
}

impl LinkState {
    /// Whether the link currently exists.
    #[must_use]
    pub const fn exists(&self) -> bool {
        self.values.is_some()
    }
}

/// What a link write does.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum LinkMutation {
    /// Add the link, or replace its link property values. Both endpoints
    /// must exist once the commit's object writes apply.
    Add {
        /// The complete link property values after the write.
        values: JsonObject,
    },
    /// Remove the link, keeping its version.
    Remove,
}

/// One link write, guarded by the version the turn read, exactly like a
/// [`FactWrite`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkWrite {
    /// The link.
    pub key: LinkKey,
    /// The version the turn read; the write commits only if it is current.
    pub expected: FactVersion,
    /// The mutation.
    pub mutation: LinkMutation,
}

/// Opaque identity of one external observation; the host ingests each id at
/// most once.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ObservationId(pub String);

/// Where an observed fact came from (ruling 2026-09-25 §2: provenance is
/// mandatory for ingested facts).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Provenance {
    /// The source system (`ERPNext`).
    pub source: String,
    /// The record's identity in the source (`Employee/EMP182`).
    pub source_id: String,
    /// When the host observed it.
    pub observed_at: LogicalTime,
    /// The mapping version that produced the input.
    pub mapping_version: String,
}

/// One external observation delivered by [`CellHost::observe`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observed<I> {
    /// The observation's identity.
    pub id: ObservationId,
    /// Its provenance.
    pub provenance: Provenance,
    /// The kernel input it maps to.
    pub input: I,
}

/// The result of one performed command, delivered by [`CellHost::perform`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Performed<I> {
    /// The command that was performed.
    pub command: CommandId,
    /// The generation it was performed under.
    pub generation: Generation,
    /// The kernel input its result maps to.
    pub input: I,
}

/// Which pending delivery a commit consumes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum Consumed {
    /// A due timer delivered by [`CellHost::clock`]. The incarnation is part
    /// of the identity: a timer rescheduled since delivery (even with the same
    /// id, due time, and payload) is a different timer.
    Timer {
        /// The timer.
        id: TimerId,
        /// The ledger position of the commit that scheduled it.
        incarnation: LedgerSeq,
    },
    /// A command result delivered by [`CellHost::perform`].
    Result {
        /// The command.
        command: CommandId,
        /// The generation it was performed under.
        generation: Generation,
    },
    /// An observation delivered by [`CellHost::observe`].
    Observation {
        /// The observation.
        id: ObservationId,
    },
}

impl fmt::Display for Consumed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Timer { id, incarnation } => {
                write!(f, "timer {} scheduled at {}", id.0, incarnation.0)
            }
            Self::Result {
                command,
                generation,
            } => write!(f, "result of {} generation {}", command.0, generation.0),
            Self::Observation { id } => write!(f, "observation {}", id.0),
        }
    }
}

/// A due timer delivered by [`CellHost::clock`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DueTimer<T> {
    /// The timer as scheduled.
    pub timer: Timer<T>,
    /// The ledger position of the commit that scheduled this incarnation.
    pub incarnation: LedgerSeq,
}

impl<T> DueTimer<T> {
    /// The consumption reference for this delivered timer.
    #[must_use]
    pub fn consumed(&self) -> Consumed {
        Consumed::Timer {
            id: self.timer.id.clone(),
            incarnation: self.incarnation,
        }
    }
}

impl<I> Performed<I> {
    /// The consumption reference for this delivered result.
    #[must_use]
    pub fn consumed(&self) -> Consumed {
        Consumed::Result {
            command: self.command.clone(),
            generation: self.generation,
        }
    }
}

impl<I> Observed<I> {
    /// The consumption reference for this delivered observation.
    #[must_use]
    pub fn consumed(&self) -> Consumed {
        Consumed::Observation {
            id: self.id.clone(),
        }
    }
}

/// Everything one turn commits, atomically (the `commit` verb's argument).
///
/// `E` is the ledger event type, `C` the outbound command payload, and `T`
/// the timer payload (the decider's input type, see [`Timer`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CellCommit<E, C, T> {
    /// The delivery this turn consumes, if any.
    pub consumes: Option<Consumed>,
    /// Version expectations on objects read but not written.
    pub reads: Vec<FactExpectation>,
    /// Object writes, each guarded by the version it read.
    pub writes: Vec<FactWrite>,
    /// New ledger events, in order.
    pub events: Vec<E>,
    /// Commands and timer changes to enqueue, applied in order.
    pub effects: Vec<Effect<C, T>>,
    /// Link writes, each guarded by the version it read, applied after the
    /// object writes (#3013). Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<LinkWrite>,
    /// Complete bounded link sets read by the turn, guarded against phantom
    /// additions/removals at commit. Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub link_reads: Vec<LinkReadExpectation>,
}

impl<E, C, T> CellCommit<E, C, T> {
    /// A commit that changes nothing.
    #[must_use]
    pub const fn empty() -> Self {
        Self {
            consumes: None,
            reads: Vec::new(),
            writes: Vec::new(),
            events: Vec::new(),
            effects: Vec::new(),
            links: Vec::new(),
            link_reads: Vec::new(),
        }
    }

    /// The same commit, consuming `consumed`.
    #[must_use]
    pub fn consuming(mut self, consumed: Consumed) -> Self {
        self.consumes = Some(consumed);
        self
    }
}

impl<E, C, T> Default for CellCommit<E, C, T> {
    fn default() -> Self {
        Self::empty()
    }
}

/// Position of one commit in the cell's ledger; the first commit is `1`.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct LedgerSeq(pub u64);

impl LedgerSeq {
    /// The next position, or `None` on overflow.
    #[must_use]
    pub const fn checked_next(self) -> Option<Self> {
        match self.0.checked_add(1) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }
}

/// An object's version after a commit wrote it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WrittenVersion {
    /// The object.
    pub key: FactKey,
    /// Its new version.
    pub version: FactVersion,
}

/// What an accepted commit reports.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommitReceipt {
    /// The commit's ledger position.
    pub seq: LedgerSeq,
    /// Every written object and its new version, in write order.
    pub written: Vec<WrittenVersion>,
    /// Every written link and its new version, in write order. Omitted when
    /// empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub linked: Vec<WrittenLinkVersion>,
}

/// A link's version after a commit wrote it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WrittenLinkVersion {
    /// The link.
    pub key: LinkKey,
    /// Its new version.
    pub version: FactVersion,
}

/// The new versions an admitted commit assigns ([`check_commit`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CommitVersions {
    /// Each object write's new version, in write order.
    pub written: Vec<WrittenVersion>,
    /// Each link write's new version, in write order.
    pub linked: Vec<WrittenLinkVersion>,
}

/// Why a commit was refused. On every variant nothing was applied: no write,
/// no event, no enqueue, no consumption.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CommitError<X> {
    /// An owned child or manifest was changed without the trusted atomic
    /// parent-and-rows action, or a parent delete omitted that action.
    #[error("{key} is protected by an owned child collection transaction")]
    OwnedRowsProtected {
        /// The protected object or parent.
        key: FactKey,
    },
    /// An object appears more than once across `reads` and `writes`.
    #[error("fact {key} appears more than once in one commit")]
    DuplicateKey {
        /// The repeated object.
        key: FactKey,
    },
    /// The consumed delivery is not pending (already consumed, rescheduled,
    /// re-issued, or never delivered).
    #[error("{consumed} is not pending")]
    NotPending {
        /// The reference the commit named.
        consumed: Consumed,
    },
    /// Compare-and-swap failed: the object changed since the turn read it.
    #[error("fact {key} version conflict: expected {expected}, current {current}")]
    Conflict {
        /// The object.
        key: FactKey,
        /// The version the turn read.
        expected: FactVersion,
        /// The version now committed.
        current: FactVersion,
    },
    /// A link appears more than once in `links`.
    #[error("link {key} appears more than once in one commit")]
    DuplicateLink {
        /// The repeated link.
        key: Box<LinkKey>,
    },
    /// An object or link identity exceeds its admitted UTF-8 byte budget.
    #[error("{field} is {actual_bytes} bytes; the limit is {max_bytes} bytes")]
    IdentifierTooLong {
        /// The identity component that exceeded its limit.
        field: &'static str,
        /// The component's UTF-8 byte length.
        actual_bytes: usize,
        /// The maximum permitted byte length.
        max_bytes: usize,
    },
    /// Compare-and-swap failed on a link: it changed since the turn read it.
    #[error("link {key} version conflict: expected {expected}, current {current}")]
    LinkConflict {
        /// The link.
        key: Box<LinkKey>,
        /// The version the turn read.
        expected: FactVersion,
        /// The version now committed.
        current: FactVersion,
    },
    /// A complete bounded link set read by an action changed before commit.
    #[error("link targets changed after reading {link} from {source_key}")]
    LinkReadConflict {
        /// The source whose adjacency set was read.
        source_key: FactKey,
        /// The declared link.
        link: String,
    },
    /// A host cannot verify the complete target set needed for a link read.
    #[error("host cannot verify the bounded link read {link} from {source_key}")]
    UnsupportedLinkRead {
        /// The source whose adjacency set was read.
        source_key: FactKey,
        /// The declared link.
        link: String,
    },
    /// A source/link pair appears more than once in link read expectations.
    #[error("link read {link} from {source_key} appears more than once")]
    DuplicateLinkRead {
        /// The source whose adjacency set was read.
        source_key: FactKey,
        /// The declared link.
        link: String,
    },
    /// A link-read set exceeds the shared admitted count or byte bound.
    #[error("link read {link} from {source_key} has size {actual}; limit is {max}")]
    LinkReadTooLarge {
        /// The source whose adjacency set was read.
        source_key: FactKey,
        /// The declared link.
        link: String,
        /// Observed target count or aggregate target-key bytes.
        actual: usize,
        /// Maximum admitted count or byte size, matching `actual`.
        max: usize,
    },
    /// A target in a complete link-read set does not match its source/link or
    /// appears out of canonical order.
    #[error("link read {link} from {source_key} is not a canonical complete set")]
    InvalidLinkRead {
        /// The source whose adjacency set was read.
        source_key: FactKey,
        /// The declared link.
        link: String,
    },
    /// A link would be added to an object that does not exist once the
    /// commit's object writes apply.
    #[error("link {key} would dangle: {missing} does not exist")]
    DanglingLink {
        /// The link.
        key: Box<LinkKey>,
        /// The missing endpoint.
        missing: FactKey,
    },
    /// A link names a link its source's type does not declare, or a target
    /// type the declaration does not admit (#3018). A host without a model
    /// declares no links.
    #[error("link {key} is not a declared link to an admitted target type")]
    LinkTarget {
        /// The link.
        key: Box<LinkKey>,
    },
    /// The commit would give a to-one link two targets from one source, or a
    /// from-one link two sources into one target (#3018).
    #[error("link {key} exceeds its declared cardinality at {endpoint}")]
    LinkCardinality {
        /// The first link write that exceeds it.
        key: Box<LinkKey>,
        /// The endpoint with too many links.
        endpoint: FactKey,
    },
    /// A touched source object has no link for a required link declaration.
    #[error("{parent} has no required link {link}")]
    LinkRequired {
        /// The parent object.
        parent: FactKey,
        /// The required link id.
        link: String,
    },
    /// An object is written through a type other than its live concrete type
    /// (#3018: one object, one concrete type; delete it first).
    #[error("{key} is live as {}; write it as that type", concrete.as_str())]
    TypeMismatch {
        /// The write.
        key: FactKey,
        /// The live object's concrete type.
        concrete: ObjectTypeName,
    },
    /// An effective property default cannot be represented as an exact
    /// runtime value, so the post-commit object cannot be materialized.
    #[error("{key}: effective object values were refused: {reason}")]
    EffectiveValues {
        /// The object whose effective values failed to materialize.
        key: FactKey,
        /// Why a declared default was not an exact runtime value.
        reason: String,
    },
    /// A complete acyclic relation proof exceeded the commit traversal budget.
    #[error("{key}: {reason}")]
    TraversalLimitExceeded {
        /// The source whose relation could not be proven within the budget.
        key: FactKey,
        /// The exhausted traversal budget.
        reason: String,
    },
    /// A declared accounting-leg posting group is malformed, unbalanced, or
    /// attempts to mutate an already committed leg. Nothing was applied.
    #[error("accounting posting for {key} was refused: {reason:?}")]
    AccountingPosting {
        /// The leg record that failed admission.
        key: FactKey,
        /// The stable accounting authority refusal.
        reason: AccountingError,
    },
    /// A final object value violates a declared object or interface
    /// invariant. The owning declaration and id form its stable identity.
    #[error("{key}: invariant {} on {:?} was refused: {message}", id.as_str(), owner)]
    InvariantViolation {
        /// The object whose final value failed admission.
        key: Box<FactKey>,
        /// The object type or interface that declared the invariant.
        owner: InvariantOwner,
        /// The invariant identity within its owner.
        id: InvariantId,
        /// The invariant's human-readable refusal message.
        message: String,
        /// Why evaluation did not produce `true`.
        outcome: Box<DataQualityOutcome>,
    },
    /// Runtime evaluation of all final values in one commit exceeded the
    /// existing deterministic expression budget.
    #[error("invariant evaluation for {key} exceeded the {limit}-unit commit budget")]
    InvariantBudgetExceeded {
        /// The object being evaluated when the budget was exhausted.
        key: FactKey,
        /// The per-commit unit ceiling.
        limit: u64,
    },
    /// A runtime type catalog references an interface that it did not retain.
    #[error("{key}: invariant model is missing interface {interface}")]
    InvariantModel {
        /// The object whose model resolution failed.
        key: FactKey,
        /// The unresolved interface name.
        interface: String,
    },
    /// An observed write did not match its opaque prepared authorization, or
    /// its findings differed from the shared final-state evaluation.
    #[error("observation authorization did not match its prepared commit")]
    ObservationAuthorization,
    /// The complete commit, including generated findings and
    /// bookkeeping events, exceeds the existing event envelope.
    #[error("commit emitted {actual} events, limit is {limit}")]
    EventLimitExceeded {
        /// The profile's event ceiling.
        limit: u32,
        /// Total event count after private findings were appended.
        actual: usize,
    },
    /// A version or the ledger sequence would overflow.
    #[error("fact version or ledger sequence overflow")]
    Overflow,
    /// The host failed; nothing was applied.
    #[error("cell host failure")]
    Host(#[source] X),
}

impl<X> CommitError<X> {
    /// Stable machine-readable code of the refusal.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::DuplicateKey { .. } => "commit.duplicate_key",
            Self::NotPending { .. } => "commit.not_pending",
            Self::Conflict { .. } => "commit.conflict",
            Self::DuplicateLink { .. } => "commit.duplicate_link",
            Self::IdentifierTooLong { .. } => "commit.identifier_too_long",
            Self::LinkConflict { .. } => "commit.link_conflict",
            Self::LinkReadConflict { .. } => "commit.link_read_conflict",
            Self::UnsupportedLinkRead { .. } => "commit.unsupported_link_read",
            Self::DuplicateLinkRead { .. } => "commit.duplicate_link_read",
            Self::LinkReadTooLarge { .. } => "commit.link_read_too_large",
            Self::InvalidLinkRead { .. } => "commit.invalid_link_read",
            Self::DanglingLink { .. } => "commit.dangling_link",
            Self::LinkTarget { .. } => "commit.link_target",
            Self::LinkCardinality { .. } => "commit.link_cardinality",
            Self::LinkRequired { .. } => "commit.link_required",
            Self::OwnedRowsProtected { .. } => "commit.owned_rows_protected",
            Self::TypeMismatch { .. } => "commit.type_mismatch",
            Self::EffectiveValues { .. } => "commit.effective_values",
            Self::TraversalLimitExceeded { .. } => "commit.traversal_limit_exceeded",
            Self::AccountingPosting { reason, .. } => reason.code(),
            Self::InvariantViolation { .. } => "commit.invariant_violation",
            Self::InvariantBudgetExceeded { .. } => "commit.invariant_budget_exceeded",
            Self::InvariantModel { .. } => "commit.invariant_model",
            Self::ObservationAuthorization => "commit.observation_authorization",
            Self::EventLimitExceeded { .. } => "commit.event_limit",
            Self::Overflow => "commit.overflow",
            Self::Host(_) => "commit.host",
        }
    }
}

pub use crate::commit_check::{CommitModel, CommitView, check_commit, check_commit_owned_rows};

/// How a host orders commits that touch an object (ruling 2026-09-25 §7).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum SerializationLane {
    /// One writer for the whole cell: total order over every commit.
    Cell,
    /// Total order per entity, causal order across entities.
    Entity {
        /// The entity whose lane the commit joins.
        key: FactKey,
    },
}

/// The impure perimeter of one tenant cell: four verbs plus the ordering and
/// publish hooks (see the module docs).
///
/// Implementations are synchronous. Every verb either completes or returns an
/// error having changed nothing; [`CellHost::commit`] is all-or-nothing over
/// writes, events, enqueues, and consumption.
pub trait CellHost {
    /// Ledger event type.
    type Event;
    /// Outbound command payload.
    type Command;
    /// Kernel input: what timers carry and what results and observations map
    /// to.
    type Input;
    /// Host failure.
    type Error: std::error::Error;

    /// Reads the committed state of one object (not an effect verb).
    /// Model-backed hosts expose valid temporal properties in canonical form
    /// under the concrete effective contract, including legacy stored values.
    /// Consumers may authorize or evaluate this state immediately; they do
    /// not normalize loaded values. Invalid observation evidence is retained.
    ///
    /// # Errors
    ///
    /// The host's read failure.
    fn load(&self, key: &FactKey) -> Result<FactState, Self::Error>;

    /// Whether this host implements identity/version-only reference reads.
    fn supports_reference_reads(&self) -> bool {
        false
    }

    /// Read only an object's concrete identity and version. A present identity
    /// carries an empty value map, never classified source properties.
    ///
    /// # Errors
    /// A host read failure. Unsupported hosts return no snapshot.
    fn load_reference(
        &self,
        _key: &FactKey,
        _max_bytes: u64,
    ) -> Result<Option<BoundedFact>, Self::Error> {
        Ok(None)
    }

    /// Reads one fact only when the host can charge its stored representation
    /// against `max_bytes` before decoding it. `None` means unsupported or
    /// over budget; callers that require a bounded slice must fail closed.
    ///
    /// # Errors
    ///
    /// Returns the host read failure.
    fn load_bounded(
        &self,
        key: &FactKey,
        max_bytes: u64,
    ) -> Result<Option<BoundedFact>, Self::Error> {
        let _ = (key, max_bytes);
        Ok(None)
    }

    /// Reads the committed state of one link (not an effect verb).
    /// Temporal properties follow the same read contract as [`Self::load`].
    ///
    /// # Errors
    ///
    /// The host's read failure.
    fn load_link(&self, key: &LinkKey) -> Result<LinkState, Self::Error>;

    /// Reads the complete, bounded live target set for one declared outgoing
    /// link. `None` means this host cannot provide an atomicity-safe list
    /// read; callers must fail closed when a declaration needs one.
    ///
    /// Implementations must return targets in key order and must refuse a
    /// list exceeding `max_targets` or aggregate target-key bytes exceeding
    /// `max_key_bytes`.
    ///
    /// # Errors
    ///
    /// Returns the host read failure.
    fn read_link_targets(
        &self,
        source: &FactKey,
        link: &str,
        max_targets: usize,
        max_key_bytes: u64,
    ) -> Result<Option<Vec<LinkReadTarget>>, Self::Error> {
        let _ = (source, link, max_targets, max_key_bytes);
        Ok(None)
    }

    /// **commit**: atomically consume the delivery, apply the writes, append
    /// the events as one ledger entry, and enqueue the effects, after
    /// [`check_commit`] admits it.
    ///
    /// Enqueue semantics, in effect order:
    ///
    /// - `Perform` appends the command to the outbox. A command whose id was
    ///   already enqueued at the same or a newer generation (pending or
    ///   completed) is a no-op, so a re-issue never re-executes; a newer
    ///   generation of a pending command replaces it in place and fences the
    ///   old generation's result.
    /// - `Schedule` inserts or replaces a timer by id; the new incarnation is
    ///   this commit's ledger position.
    /// - `CancelTimer` removes it (absent is a no-op).
    ///
    /// # Errors
    ///
    /// A [`CommitError`]; nothing was applied.
    fn commit(
        &mut self,
        commit: CellCommit<Self::Event, Self::Command, Self::Input>,
    ) -> Result<CommitReceipt, CommitError<Self::Error>>;

    /// Atomically commit an authorized business action and its checked quantity
    /// intent. Unsupported hosts fail closed without publishing business writes.
    ///
    /// # Errors
    /// A commit refusal; nothing was applied.
    fn commit_action_quantity(
        &mut self,
        _commit: CellCommit<Self::Event, Self::Command, Self::Input>,
        _request: &norn_ir::quantity_ledger::QuantityRequest,
        _idempotency_key: &str,
    ) -> Result<CommitReceipt, CommitError<Self::Error>> {
        Err(CommitError::ObservationAuthorization)
    }

    /// Commit an action whose kernel proof distinguishes reference existence
    /// from classified values. Hosts without this precision retain full
    /// lineage through their ordinary commit path.
    ///
    /// # Errors
    /// A commit refusal; nothing was applied.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "the one-use kernel proof is consumed at the host boundary"
    )]
    fn commit_action_references(
        &mut self,
        commit: CellCommit<Self::Event, Self::Command, Self::Input>,
        _proof: crate::action_commit::ActionReferenceReads,
        quantity: Option<(&norn_ir::quantity_ledger::QuantityRequest, &str)>,
    ) -> Result<CommitReceipt, CommitError<Self::Error>> {
        if let Some((request, key)) = quantity {
            self.commit_action_quantity(commit, request, key)
        } else {
            self.commit(commit)
        }
    }

    /// Commit the kernel-prepared parent and owned-row transaction.
    ///
    /// The opaque proof is not serializable or caller-constructible. Hosts
    /// that run kernel admission must override this method and pass the proof
    /// to `check_commit_owned_rows`; the default preserves compatibility for
    /// simple hosts whose ordinary `commit` already accepts kernel-prepared
    /// commits.
    ///
    /// # Errors
    ///
    /// A [`CommitError`]; nothing was applied.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "the one-use authorization is consumed at the host boundary"
    )]
    fn commit_owned_rows(
        &mut self,
        commit: CellCommit<Self::Event, Self::Command, Self::Input>,
        _proof: OwnedRowsCommitProof,
    ) -> Result<CommitReceipt, CommitError<Self::Error>> {
        self.commit(commit)
    }

    /// Commit a snapshot prepared by the kernel's observation reconciler.
    /// The opaque authorization is scoped to the exact prepared writes; a
    /// generic host implementation only accepts it when no invariant
    /// findings need the observation-specific admission path.
    ///
    /// # Errors
    ///
    /// A [`CommitError`]; nothing was applied.
    fn commit_observed(
        &mut self,
        commit: CellCommit<Self::Event, Self::Command, Self::Input>,
        authorization: ObservationAuthorization,
    ) -> Result<CommitReceipt, CommitError<Self::Error>>
    where
        Self::Event: From<ObservationEvent>,
        Self::Input: Serialize,
    {
        if !authorization.validates(&commit) || authorization.has_findings() {
            return Err(CommitError::ObservationAuthorization);
        }
        self.commit(commit)
    }

    /// **perform**: execute the oldest pending command, idempotently and with
    /// the host's bounded retry, and return its result. If that command was
    /// already performed and its result not yet consumed, return the recorded
    /// result without executing again. `None` when the outbox is empty.
    ///
    /// # Errors
    ///
    /// The host's failure; the command stays pending.
    fn perform(&mut self) -> Result<Option<Performed<Self::Input>>, Self::Error>;

    /// **observe**: the oldest pending external observation, not removed until
    /// consumed. `None` when there is none.
    ///
    /// # Errors
    ///
    /// The host's failure.
    fn observe(&mut self) -> Result<Option<Observed<Self::Input>>, Self::Error>;

    /// **clock**: the pending timer with the earliest due time (ties by id)
    /// that is due at the host's current time, not removed until consumed.
    /// `None` when no timer is due.
    ///
    /// # Errors
    ///
    /// The host's failure.
    fn clock(&mut self) -> Result<Option<DueTimer<Self::Input>>, Self::Error>;

    /// The ordering lane a commit touching `key` joins. Hosts without
    /// concurrent writers keep the default: one lane for the cell.
    fn serialize_by(&self, _key: &FactKey) -> SerializationLane {
        SerializationLane::Cell
    }

    /// Pushes an accepted commit to the cell's live subscribers (reactive
    /// queries, ruling 2026-09-25 §10). Called after `commit` succeeds; it is
    /// not part of the commit transaction, and subscribers resynchronize from
    /// the ledger if a push is lost.
    ///
    /// # Errors
    ///
    /// The host's failure; the commit itself stands.
    fn publish(&mut self, receipt: &CommitReceipt) -> Result<(), Self::Error>;
}
