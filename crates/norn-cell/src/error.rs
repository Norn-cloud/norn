//! The store's typed domain errors.

use norn_ir::canonical::CanonicalJsonError;
use norn_ir::{DataQualityOutcome, InvariantId, InvariantOwner};
use norn_projections::SqliteDdlError;

use crate::store::{CommitPoint, Consumption};

/// Native SQLite failure payload. Portable engines use [`CellError::Engine`].
///
/// Keep the variant matchable in every profile: Cargo can unify this crate's
/// `native` feature independently of a consumer's features. Without `native`,
/// the payload is uninhabited, so a portable engine cannot construct it.
#[cfg(feature = "native")]
pub type SqliteError = rusqlite::Error;
/// Uninhabited SQLite failure payload when the native adapter is absent.
#[cfg(not(feature = "native"))]
pub type SqliteError = std::convert::Infallible;

/// Why a store operation failed. A failed commit leaves the store unchanged.
#[derive(Debug, thiserror::Error)]
pub enum CellError {
    /// ID-STABLE-1: canonical authored creation cannot reuse a retained identity.
    #[error("{code}: {message}", code = norn_checker::IDENTITY_REUSE, message = norn_checker::IDENTITY_REUSE_MESSAGE)]
    IdentityReuse,

    /// An effective property uses semantics the kernel cannot evaluate.
    #[error(transparent)]
    EffectiveValues(#[from] norn_kernel::EffectiveValuesError),
    /// ID-UNIQUE-1: two live objects share a normalized key in one scope/world.
    #[error("{code}: normalized key {owner}/{key} collides", code = norn_checker::IDENTITY_COLLISION)]
    IdentityCollision {
        /// The declaring root; no row values or identities are disclosed.
        owner: String,
        /// The declared key name.
        key: String,
        /// Internal lookup subjects for authorized diagnostics; never displayed.
        conflicts: [String; 2],
    },
    /// OBS-DISPUTE-LOOKUP-1: retry after three unstable source-evidence snapshots.
    #[error("core.boundary.source_evidence_changed")]
    SourceEvidenceChanged,
    /// OBS-EVIDENCE-1: an open source dispute excludes this governed subject.
    #[error("core.boundary.import_finding: disputed source identity")]
    SourceIdentityDisputed,
    /// Core 08 has not retained the source-dispute evidence frontier for this world.
    #[error("core.world.source_dispute_unqualified")]
    SourceDisputeWorldUnqualified,
    /// Captured relation/delete output requires a processing profile beyond CP2.
    #[error("unsupported mapping processing profile")]
    UnsupportedMappingProfile,
    /// The kernel refused preparation of source observations.
    #[error("observation preparation: {0}")]
    ObservationPreparation(Box<norn_kernel::snapshot_observe::ObservationRefusal>),
    /// The kernel or store refused the atomic observation commit.
    #[error("observation commit: {0}")]
    ObservationCommit(Box<norn_kernel::CommitError<CellError>>),
    /// OBS-EVIDENCE-1: retained evidence disagrees with cell-owned recomputation.
    #[error("core.boundary.bundle_mismatch")]
    MappingBundleMismatch,
    /// Law-bearing definitions predate authenticated law retention.
    #[error(
        "core.boundary.legacy_law_snapshot: legacy stores must be recreated; migration is out of scope"
    )]
    LegacyLawSnapshot,
    /// A finding has no admitted definition version to authenticate its rules.
    #[error(
        "core.boundary.unpinned_finding: legacy stores must be recreated; migration is out of scope"
    )]
    UnpinnedFinding,
    /// Observation quality evaluation could not complete.
    #[error("quality evaluation: {0}")]
    QualityEvaluation(Box<norn_kernel::snapshot_observe::ObservationRefusal>),
    /// Correction values disagree with their immutable proof.
    #[error("quality evaluation: unproven correction values")]
    UnprovenCorrection,
    /// CP2 cannot resolve an applicable inherited/effective law without B2.
    #[error("core.laws.unsupported_semantics: law {law_id} scope unsupported until B2")]
    UnsupportedLawScope {
        /// Retained law whose complete scope cannot yet be resolved.
        law_id: String,
    },
    /// Deterministic query semantics or snapshot preconditions refused.
    #[error("{0}")]
    ComputeRefused(&'static str),
    /// RUN-LEDGER-1: a run start or run write was refused (`run.*`).
    #[error("{0}")]
    RunRefused(&'static str),
    /// DECISION-LEDGER-1: a decision resolution was refused (`decision.*`).
    #[error("{0}")]
    DecisionRefused(&'static str),
    /// MANIFEST-1: a dataset manifest was refused (`dataset.*`, `bulk.*`).
    #[error("{code}: {detail}")]
    DatasetRefused {
        /// The stable refusal code.
        code: &'static str,
        /// What was refused, for the caller.
        detail: String,
    },
    /// A historical input was recorded after the requested knowledge cutoff.
    #[error("core.world.temporal_leakage")]
    TemporalLeakage,
    /// WORLD-ISOLATE-1: a fork never claims, dispatches or records a
    /// performed command.
    #[error("core.world.perform_forbidden")]
    WorldPerformForbidden,
    /// FORK-SIMULATE-1: only a fork records simulated results.
    #[error("core.world.simulation_forbidden")]
    WorldSimulationForbidden,
    /// FORK-PIN-1: a fork is created only at its parent's verified head.
    #[error("core.world.fork_stale_base: pinned at commit {expected}, head is commit {head}")]
    ForkStaleBase {
        /// The commit count the caller pinned.
        expected: u64,
        /// The parent's verified commit count.
        head: u64,
    },
    /// FORK-PIN-1: a fork's world is never `main`, empty or its parent's.
    #[error("core.world.fork_invalid_world")]
    ForkInvalidWorld,
    /// FORK-EVIDENCE-1: a fork never exports what it minted, or its receipt
    /// log, as evidence.
    #[error("core.world.fork_evidence_forbidden")]
    ForkEvidenceForbidden,

    /// Missing or mismatched independent historical value proof.
    #[error("historical value proof refused: {0}")]
    HistoricalValueUnproven(String),

    /// Pure current-kernel quantity admission refused the whole transaction.
    #[error("quantity: {0}")]
    Quantity(#[from] norn_kernel::quantity::QuantityError),
    /// Query dependency capture could not be completed within its hard bounds.
    #[error("query dependency capture exceeds 4096 entries or 256 KiB")]
    DependencyBudgetExceeded,

    /// The SQLite engine failed (constraint violations included).
    #[error("sqlite: {0}")]
    Sqlite(#[from] SqliteError),
    /// A host SQL engine failed, or returned an invalid row.
    #[error("sql engine: {0}")]
    Engine(String),
    /// The object types do not project to DDL.
    #[error("ddl projection: {0}")]
    Projection(#[from] SqliteDdlError),
    /// The effective view of the object types could not be derived: the
    /// bounded inheritance classification was exhausted (more than 4,096
    /// object types, or another budget). The model is refused; no query ever
    /// sees an empty view.
    #[error(transparent)]
    EffectiveViewRefused(#[from] norn_checker::EffectiveViewRefused),
    /// A payload does not (de)serialize.
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    /// Typed data does not encode to, or decode from, an exact runtime
    /// value.
    #[error("json: {0}")]
    Encoding(String),
    /// A value is not an exact runtime value (#2770).
    #[error("value: {0}")]
    Value(#[from] norn_ir::RuntimeValueError),
    /// A payload does not canonicalize (Norn1).
    #[error("canonical json: {0}")]
    Canonical(#[from] CanonicalJsonError),
    /// Unsupported pre-alpha cells must be recreated, never migrated.
    #[error(
        "unsupported pre-alpha store version `{found}` (expected `{expected}`); recreate the cell"
    )]
    StoreFormat {
        /// The file's format.
        found: String,
        /// The format this build reads and writes.
        expected: String,
    },
    /// ENGINE-UPGRADE-1: the cell's storage version is newer than this
    /// engine's; a newer release upgraded it (#1494).
    #[error(
        "{code}: cell storage version {cell} (written by {writer}) is newer than this engine's {engine}; open it with a release that reads storage version {cell}",
        code = norn_checker::ENGINE_DOWNGRADE
    )]
    EngineDowngrade {
        /// The cell's storage version.
        cell: u32,
        /// The release label that wrote it.
        writer: String,
        /// The storage version this engine writes.
        engine: u32,
    },
    /// ENGINE-UPGRADE-1: the cell's engine stamp is not one this engine can
    /// read, so it cannot order the cell against itself (#1494).
    #[error(
        "{code}: cell engine stamp {found:?} is not a version this engine reads",
        code = norn_checker::ENGINE_UNKNOWN_VERSION
    )]
    EngineUnknownVersion {
        /// The stored stamp, truncated.
        found: String,
    },
    /// AUTOMATION-OUTBOX-1 (#1704): the replay of an exported automation
    /// continuation is not provably equivalent to the live commit: its cursor
    /// or inline candidate set differs from the exported evidence, a version
    /// 2 record lacks that evidence, or a version 1 record (which has none)
    /// orders NFC-ambiguous target ids. The restore writes nothing.
    #[error(
        "{code}: automation continuation of commit {commit_seq} (matcher version {matcher_version}) does not replay equivalently in this engine",
        code = norn_checker::ENGINE_UNKNOWN_VERSION
    )]
    AutomationMatcherUnreplayable {
        /// The commit whose continuation does not replay.
        commit_seq: u64,
        /// The matcher version recorded on the exported continuation.
        matcher_version: u32,
    },
    /// ENGINE-UPGRADE-1: the cell's storage version has no in-place upgrade
    /// path in this engine, or a migration refused its data (#1494).
    #[error(
        "{code}: cell storage version {cell} cannot be upgraded to {engine} in place: {reason}",
        code = norn_checker::ENGINE_UPGRADE_UNSUPPORTED
    )]
    EngineUpgradeUnsupported {
        /// The cell's storage version.
        cell: u32,
        /// The storage version this engine writes.
        engine: u32,
        /// Why, naming the bridge release when there is one.
        reason: String,
    },
    /// The active definitions version changed since the installer planned.
    #[error("definitions version conflict: expected {expected:?}, found {actual:?}")]
    DefinitionVersionConflict {
        /// Version the request was planned against.
        expected: Option<u64>,
        /// Version currently installed in the cell.
        actual: Option<u64>,
    },
    /// A retained definition's current revision changed since the caller read it.
    #[error("retained definition {id} revision conflict: expected {expected:?}, found {actual:?}")]
    RetainedDefinitionConflict {
        /// The retained definition id.
        id: String,
        /// Revision the request was planned against.
        expected: Option<u64>,
        /// Revision currently installed.
        actual: Option<u64>,
    },
    /// A declassification's exact current fact version no longer matches.
    #[error(
        "marking declassification version conflict for {object_type}/{key}: expected {expected}, found {actual:?}"
    )]
    MarkingVersionConflict {
        /// The concrete object type.
        object_type: String,
        /// The object's key.
        key: String,
        /// Version the request authorized.
        expected: u64,
        /// Current version, or `None` when the object is absent.
        actual: Option<u64>,
    },
    /// A declassification is malformed or does not match its ledger event.
    #[error("marking declassification refused: {0}")]
    MarkingDeclassificationInvalid(String),
    /// A retained admission receipt is malformed, has the wrong digest, or
    /// does not match the active definition version during readmission.
    #[error("definition admission receipt refused: {0}")]
    DefinitionReceiptInvalid(String),
    /// A store migration would remove or reinterpret live data.
    #[error("definitions migration refused: {0}")]
    DefinitionMigrationRefused(String),
    /// Candidate invariants reject live objects. The findings are capped by
    /// the migration validator before this error is constructed.
    #[error(
        "candidate definitions violate object invariants: {violations:?} (truncated={truncated})"
    )]
    DefinitionInvariantViolation {
        /// Up to 32 invariant findings, each with its object's world, type,
        /// and id.
        violations: Vec<DefinitionInvariantFinding>,
        /// Whether validation stopped after filling the bounded list.
        truncated: bool,
    },
    /// Candidate invariant validation exhausted its cumulative scan or fuel
    /// budget. Nothing from the install transaction is committed.
    #[error("candidate invariant validation exceeded its {resource:?} budget ({limit} units)")]
    DefinitionInvariantBudgetExceeded {
        /// Which cumulative validation budget was exhausted.
        resource: DefinitionInvariantBudget,
        /// The row or fuel limit.
        limit: u64,
        /// Number of affected rows already evaluated (or the row count at the
        /// fuel failure).
        scanned: usize,
        /// The object being evaluated when fuel ran out, if applicable.
        object: Option<DefinitionInvariantObject>,
    },
    /// Candidate invariant evaluation could not resolve or interpret the
    /// complete stored object values.
    #[error("candidate invariant evaluation refused for {object}: {reason}")]
    DefinitionInvariantEvaluationRefused {
        /// The object whose values could not be evaluated.
        object: DefinitionInvariantObject,
        /// The fail-closed reason.
        reason: String,
    },
    /// HOST-INSTALL-LIVE-1: live objects break the candidate's governing
    /// laws, or a candidate law could not be evaluated on them (unchecked is
    /// never admitted). Evaluated on the migration candidate's view of each
    /// object and of the links its laws read. Nothing is installed.
    #[error("candidate governing laws refuse {} live object finding(s) of {checked} checked", findings.len())]
    DefinitionLawRefused {
        /// Every finding, violations and unchecked objects alike.
        findings: Vec<DefinitionLawFinding>,
        /// How many live objects were evaluated.
        checked: u64,
    },
    /// HOST-INSTALL-LIVE-1: the candidate's governing laws would need more
    /// live objects evaluated than one install checks. Nothing is installed.
    #[error(
        "candidate governing laws would need more than {limit} live objects checked (at {object_type})"
    )]
    DefinitionLawUnbounded {
        /// The object type being checked when the bound was reached.
        object_type: String,
        /// The bound.
        limit: u64,
    },
    /// Candidate identity facts exceed the bounded trusted projection.
    #[error("identity projection budget exceeded: {0}")]
    ProjectionBudgetExceeded(String),
    /// A candidate identity row would be rejected by the trusted projection.
    #[error("identity input is invalid: {0}")]
    IdentityInputInvalid(String),
    /// The object type is not in the store's model.
    #[error("unknown object type `{0}`")]
    UnknownObjectType(String),
    /// The property is not a stored property of the object type.
    #[error("`{object_type}` has no stored property `{property}`")]
    UnknownProperty {
        /// The object type.
        object_type: String,
        /// The property.
        property: String,
    },
    /// The property is computed and therefore read-only.
    #[error("`{object_type}.{property}` is computed and read-only")]
    ReadOnlyProperty {
        /// The object type.
        object_type: String,
        /// The property.
        property: String,
    },
    /// A lifecycle property was written other than through a transition.
    #[error(
        "`{object_type}.{property}` is a lifecycle property; it changes only through a transition"
    )]
    LifecycleWrite {
        /// The object type.
        object_type: String,
        /// The property.
        property: String,
    },
    /// The object already exists.
    #[error("`{object_type}` `{id}` already exists in world `{world}`")]
    ObjectExists {
        /// The object type.
        object_type: String,
        /// The object id.
        id: String,
        /// The world.
        world: String,
    },
    /// The object does not exist (as this type).
    #[error("`{object_type}` `{id}` does not exist in world `{world}`")]
    ObjectMissing {
        /// The object type.
        object_type: String,
        /// The object id.
        id: String,
        /// The world.
        world: String,
    },
    /// A non-nullable property has no value and no default.
    #[error("`{owner}.{property}` needs a value")]
    MissingValue {
        /// The object type or `<type>.<link>`.
        owner: String,
        /// The property.
        property: String,
    },
    /// A value does not have the property's encoding.
    #[error("`{owner}.{property}` expects {expected}")]
    ValueType {
        /// The object type or `<type>.<link>`.
        owner: String,
        /// The property.
        property: String,
        /// What the encoding admits.
        expected: &'static str,
    },
    /// The lifecycle or transition is not declared.
    #[error("`{object_type}` declares no transition `{transition}` of lifecycle `{lifecycle}`")]
    UnknownTransition {
        /// The object type.
        object_type: String,
        /// The lifecycle property.
        lifecycle: String,
        /// The transition id.
        transition: String,
    },
    /// The transition does not leave the object's current state.
    #[error("transition `{transition}` does not leave state `{state}`")]
    TransitionNotAllowed {
        /// The transition id.
        transition: String,
        /// The current state.
        state: String,
    },
    /// The link is not declared on the object type or its supertypes.
    #[error("`{object_type}` has no link `{link}`")]
    UnknownLink {
        /// The object type.
        object_type: String,
        /// The link id.
        link: String,
    },
    /// The idempotency key was committed with a different request.
    #[error("idempotency key `{0}` was committed with a different request")]
    IdempotencyConflict(String),
    /// The consumed delivery is not pending.
    #[error("{0} is not pending")]
    NotPending(Consumption),
    /// A perform claim did not name the current unperformed outbox head.
    #[error(
        "perform claim for `{command_id}` generation {generation} is not the unperformed outbox head"
    )]
    PerformClaimNotPending {
        /// The command id.
        command_id: String,
        /// The claimed generation.
        generation: u64,
    },
    /// PARKED-1: a perform claim names a parked command, which is never
    /// performed. No build before parked commands could record one, so a
    /// retained journal that does is refused by name rather than replayed.
    #[error(
        "restore.parked_claim_unsupported: perform claim for parked command `{command_id}` generation {generation}"
    )]
    ParkedClaimUnsupported {
        /// The command id.
        command_id: String,
        /// The claimed generation.
        generation: u64,
    },
    /// A command generation already has a durable perform claim.
    #[error("perform claim for `{command_id}` generation {generation} already exists")]
    PerformAlreadyClaimed {
        /// The command id.
        command_id: String,
        /// The claimed generation.
        generation: u64,
    },
    /// A durable claim exists without a recorded external outcome.
    #[error("perform outcome for `{command_id}` generation {generation} is unknown")]
    PerformOutcomeUnknown {
        /// The command id.
        command_id: String,
        /// The claimed generation.
        generation: u64,
    },
    /// The ledger hash chain or a receipt does not verify.
    #[error("ledger corrupt at seq {seq}: {reason}")]
    LedgerCorrupt {
        /// The first ledger sequence number (or commit sequence number, for a
        /// receipt) that fails.
        seq: u64,
        /// What failed.
        reason: String,
    },
    /// OPEN-CHECKPOINT-1 (#1551 round 4): the cell opened from its
    /// checkpoint and its history scrub has not finished, so a read whose
    /// answer depends on the history being complete (an as-of read, a
    /// ledger page over the covered range, an idempotency lookup that could
    /// miss a covered receipt) is refused. Retryable: nothing changed.
    #[error("{code}: the cell is still verifying its history ({0}); retry", code = crate::scrub::VERIFYING)]
    Verifying(&'static str),
    /// OPEN-CHECKPOINT-1 (#1551 round 4): the history scrub found a defect.
    /// The cell is fenced: it refuses every commit and history read, and its
    /// next open verifies from genesis.
    #[error("{code}: the cell's history did not verify: {0}", code = crate::scrub::FENCED)]
    Fenced(String),
    /// A stored value does not decode with its column's encoding.
    #[error("stored value `{table}`.`{column}` is corrupt")]
    CorruptValue {
        /// The table.
        table: String,
        /// The column.
        column: String,
    },
    /// The file was created for a different object model, or its physical
    /// property columns match neither the raw nor the generated layout.
    #[error("store schema mismatch: file has {found}, model is {expected}")]
    SchemaMismatch {
        /// The model's DDL hash or column layout.
        expected: String,
        /// The file's DDL hash or column layout.
        found: String,
    },
    /// The file is bound to another value of an identity key (for example the
    /// tenant scope it was created for).
    #[error("store is bound to {key} {found:?}, not {expected:?}")]
    BindingMismatch {
        /// The binding key.
        key: String,
        /// The value the caller expected.
        expected: String,
        /// The value the file is bound to.
        found: String,
    },
    /// A number does not fit the store's integer range.
    #[error("{0} is out of range")]
    OutOfRange(&'static str),
    /// OBS-LINK-DISPUTE-1: a page would capture a dispute row (a conflict or
    /// identity finding) after a record it commits, so that record's commit
    /// would see a dispute one-by-one delivery commits it before. Nothing
    /// was captured; the host delivers the page's records one at a time
    /// (capture, then commit, each in order).
    #[error(
        "a mapping page captures a dispute after a record it commits; deliver its records one at a time"
    )]
    MappingPageSpansDispute,
    /// A fault probe stopped the commit (deterministic simulation only).
    #[error("injected crash at {0:?}")]
    InjectedCrash(CommitPoint),
}

/// A concrete object identity included in an install-time invariant refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefinitionInvariantObject {
    /// The world's identity.
    pub world: String,
    /// The concrete object type.
    pub object_type: String,
    /// The object id.
    pub id: String,
}

impl std::fmt::Display for DefinitionInvariantObject {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}/{}", self.world, self.object_type, self.id)
    }
}

/// HOST-INSTALL-LIVE-1: one live object the candidate's governing laws
/// refuse, or could not be evaluated on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefinitionLawFinding {
    /// The object's type.
    pub object_type: String,
    /// The object's id.
    pub id: String,
    /// The law it breaks; `None` for an object no law could be evaluated on.
    pub law: Option<String>,
    /// What broke, or why it could not be evaluated.
    pub message: String,
    /// The object could not be evaluated (fuel, an unreadable link).
    pub unchecked: bool,
}

/// One bounded invariant finding for an existing object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DefinitionInvariantFinding {
    /// The affected object identity.
    pub object: DefinitionInvariantObject,
    /// Declaration that owns the invariant.
    pub owner: InvariantOwner,
    /// Invariant id within its owner.
    pub id: InvariantId,
    /// Human-readable invariant message.
    pub message: String,
    /// Why the invariant did not pass.
    pub outcome: DataQualityOutcome,
}

/// The bounded install-time invariant resource that was exhausted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefinitionInvariantBudget {
    /// More than 4,096 affected live objects would need validation.
    Rows,
    /// The aggregate invariant evaluation fuel was exhausted.
    Fuel,
}

#[cfg(all(test, feature = "native"))]
mod tests {
    use super::CellError;

    #[test]
    fn sqlite_error_preserves_native_conversion_display_and_source() {
        let error = CellError::from(rusqlite::Error::InvalidQuery);
        assert!(matches!(&error, CellError::Sqlite(_)));
        assert_eq!(
            error.to_string(),
            format!("sqlite: {}", rusqlite::Error::InvalidQuery)
        );
        assert!(
            std::error::Error::source(&error)
                .and_then(|source| source.downcast_ref::<rusqlite::Error>())
                .is_some_and(|source| matches!(source, rusqlite::Error::InvalidQuery))
        );
    }
}
