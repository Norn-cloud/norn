//! Domain-agnostic read-only multi-cell combined management view (`READ-LOCAL-1`).
//!
//! Projects multiple independent [`CellStore`] instances side-by-side at pinned
//! local cuts without commingling ledgers, currencies, or write authority.
//! Each cell snapshot fences the entire read with [`crate::StoreReader::snapshot_version`],
//! verifies a single stable ledger head, definition pin, durable source cursor
//! table, live feed frontier, and pending-observation identity set across all
//! evaluated questions and declared coverage gates, and fails closed
//! (`core.compute.stale_input`) if any boundary disagrees or shifts during the
//! read. Visibility precedes data quality (`HOST-READ-2`), and disclosed answers
//! apply `DATA-VALUES-1` keyed revision/finding/lineage handles after internal
//! pin verification.

mod gates;
mod spec;

use std::collections::{BTreeMap, BTreeSet};

use norn_checker::metric::compute::QuestionPlan;
use norn_checker::transform::TransformPlan;
use norn_ir::RuntimeValue;
use norn_ir::cell_id::CellId;
use norn_kernel::FactKey;
use norn_kernel::authz::CommitBinding;
use norn_kernel::read_envelope::{
    AuthorityPin, Completeness, LocalFrontier, ReadContext, ReadFormat, ReadSemantics, ReadToken,
};
use norn_kernel::world::pin::SnapshotPin;
use serde::{Deserialize, Serialize};

use crate::compute::lineage::{keyed_finding_handle, keyed_lineage_handle, keyed_revision_handle};
use crate::compute::transform::RowAccess;
use crate::compute::{ALL_MEMBERS, Freshness, Frontier, QueryContext, QueryResult, Watermark};
use crate::engine::SqlEngine;
use crate::error::CellError;
use crate::read::{LedgerHead, ObjectRecord};
use crate::snapshot::Main;
use crate::source_cursor::PendingSourceCapture;
use crate::store::CellStore;
use crate::system::GENESIS_HASH;

use self::gates::{
    check_missing_feeds, count_open_findings, count_scoped_pending_observations, durable_feed_cut,
    evaluate_all_gates, pending_identities, verify_durable_cursors_match, verify_live_frontier,
};

pub use self::spec::{
    CellCoverageSpec, CoverageTransformSpec, ExpectedGrainSpec, ReconciliationSpec,
    SpecCellBinding, evaluate_cell_cut_from_spec, evaluate_combined_management_view_from_spec,
};

/// Per-row authorization callback (`VIEW-READ-1` / `VIEW-ROWS-1` / `HOST-READ-2`).
pub type RowAccessDecider<'a> = &'a dyn Fn(&ObjectRecord) -> Result<RowAccess, CellError>;

/// Pre-evaluation collection and transform authorization callback (`VIEW-READ-1`).
///
/// Hosts certifying whole-cell completeness must refuse callers lacking
/// whole-population read authority here before scanning rows so invisible
/// (`RowAccess::None`) rows never disclose their existence (`HOST-READ-2`).
pub type TransformAuthorizer<'a> = &'a dyn Fn(&TransformPlan) -> Result<(), CellError>;

/// Host live feed-frontier reader verified before and after question evaluation.
pub type FrontierReader<'a> =
    &'a dyn Fn() -> Result<(Frontier, BTreeMap<String, BTreeMap<String, Watermark>>), CellError>;

/// Declared transform whose boolean coverage column must equal `true` on every
/// visible row for the cell cut to be marked complete.
pub struct CoverageTransformGate<'a> {
    /// Declared transform identifier reported in [`CellCutSnapshot::failed_coverage_gates`].
    pub name: &'a str,
    /// Admitted transform plan evaluated at the same pinned cut.
    pub plan: &'a TransformPlan,
    /// Boolean output column name that must evaluate to `true` on every row.
    pub required_true_column: &'a str,
    /// Pre-evaluation transform and collection authorization check (`VIEW-READ-1`).
    pub authorize_transform: TransformAuthorizer<'a>,
    /// Host row-access authorization callback (`VIEW-READ-1` / `VIEW-ROWS-1`).
    pub row_access: RowAccessDecider<'a>,
}

/// Numeric reconciliation comparison between an observed transform row and an
/// expected population transform row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconciliationComparison {
    /// Observed quantity must equal expected quantity exactly (missing expected key defaults to 0).
    Eq,
    /// Observed quantity must not exceed expected quantity (missing expected key defaults to 0).
    Lte,
}

/// Domain-agnostic reconciliation and optional timestamp-freshness gate between
/// two admitted transforms sharing a grain key at the same pinned cut.
pub struct ReconciliationGate<'a> {
    /// Gate identifier reported in [`CellCutSnapshot::failed_coverage_gates`].
    pub name: &'a str,
    /// Admitted transform enumerating expected population rows and values.
    pub expected_plan: &'a TransformPlan,
    /// Admitted transform enumerating observed rows, values, and optional timestamps.
    pub observed_plan: &'a TransformPlan,
    /// Shared grain columns matching each expected row to one observed row.
    pub key_columns: &'a [&'a str],
    /// Numeric column name in `expected_plan`.
    pub expected_value_column: &'a str,
    /// Numeric column name in `observed_plan`.
    pub observed_value_column: &'a str,
    /// Comparison required between observed and expected numeric values.
    pub comparison: ReconciliationComparison,
    /// Optional RFC 3339 timestamp column in `observed_plan` checked for freshness (`<= now_ms`, age `<= max_age_ms`).
    pub observed_timestamp_column: Option<&'a str>,
    /// Maximum allowed age (milliseconds) relative to `QueryContext::now_ms` when `observed_timestamp_column` is set.
    pub max_age_ms: Option<u64>,
    /// Optional exact-match column filters applied to `expected_plan` rows before reconciling.
    pub expected_filters: &'a [(&'a str, RuntimeValue)],
    /// Optional exact-match column filters applied to `observed_plan` rows before reconciling.
    pub observed_filters: &'a [(&'a str, RuntimeValue)],
    /// When `true`, every observed row must have a matching expected row (even when observed is `0`).
    pub require_expected_match: bool,
    /// Pre-evaluation authorization check for `expected_plan` (`VIEW-READ-1`).
    pub authorize_expected: TransformAuthorizer<'a>,
    /// Pre-evaluation authorization check for `observed_plan` (`VIEW-READ-1`).
    pub authorize_observed: TransformAuthorizer<'a>,
    /// Per-row visibility callback (`HOST-READ-2`).
    pub row_access: RowAccessDecider<'a>,
}

/// Domain-agnostic expected-grain completeness gate ensuring every expected
/// grain key emitted by an admitted transform is covered by an evaluated
/// question's buckets matching `required_bucket_filters`.
pub struct ExpectedGrainGate<'a> {
    /// Gate identifier reported in [`CellCutSnapshot::failed_coverage_gates`].
    pub name: &'a str,
    /// Admitted transform enumerating every required grain tuple.
    pub expected_plan: &'a TransformPlan,
    /// Column names in `expected_plan` forming the required grain tuple.
    pub expected_columns: &'a [&'a str],
    /// Question name in [`CellViewInput::questions`] that must cover every expected tuple.
    pub question_name: &'a str,
    /// Zero-based indices into `QueryRow::bucket` corresponding to `expected_columns`.
    pub bucket_indices: &'a [usize],
    /// When `true`, a covered bucket with `value == None` or `suppressed == true` fails the gate.
    pub require_non_null_value: bool,
    /// Optional exact-match filters on bucket positions (for example pinning a target ISO week).
    pub required_bucket_filters: &'a [(usize, RuntimeValue)],
    /// Pre-evaluation authorization check for `expected_plan` (`VIEW-READ-1`).
    pub authorize_transform: TransformAuthorizer<'a>,
    /// Per-row visibility callback (`HOST-READ-2`).
    pub row_access: RowAccessDecider<'a>,
}

/// Classification of a multi-cell read-only projection (`READ-LOCAL-1`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CombinedViewKind {
    /// Side-by-side read-only management view across independent legal-entity
    /// cells. Preserves each cell's native book and currency and is explicitly
    /// not a consolidated financial statement.
    CombinedManagementView,
}

/// Read-only specification for evaluating one cell's slice of a combined view.
pub struct CellViewInput<'a, E: SqlEngine> {
    /// Cell store to inspect at a single verified read cut.
    pub store: &'a CellStore<E>,
    /// Anchor object key covered by `authority` for `READ-LOCAL-1` pinning.
    pub anchor_key: &'a FactKey,
    /// Authorized PDP binding pinning this cell's read cut (`READ-LOCAL-1`).
    pub authority: &'a CommitBinding,
    /// Query context pinning world, scope, mapping pin, clock, cursors, and watermarks.
    pub context: QueryContext,
    /// Cell handle key used to key `DATA-VALUES-1` revision, finding, and lineage handles.
    pub handle_key: &'a [u8],
    /// Whether the caller holds whole-world unmarked Values disclosure (`DATA-VALUES-1`).
    pub whole_world_values: bool,
    /// Per-member Values access callback applied to every [`QueryResult`] row (`DATA-VALUES-1`).
    pub member_access: RowAccessDecider<'a>,
    /// Minimum bucket size disclosed when any contributing member lacks Values (`DATA-VALUES-1`, D53).
    pub min_disclosed_bucket: usize,
    /// Required feed names whose durable cursors and live watermarks must match and be fresh.
    pub required_feeds: &'a [&'a str],
    /// Required live feed-frontier reader checked before and after evaluation so stale or
    /// mid-read shifted cursors/watermarks fail closed (`core.compute.stale_input`).
    pub read_frontier: FrontierReader<'a>,
    /// Minimum acceptable watermark heartbeat timestamp (milliseconds since epoch).
    pub min_fresh_ms: u64,
    /// Evidence object types scanned for open data-quality findings.
    pub evidence_types: &'a [&'a str],
    /// Monitored `(mapping_id, evidence_object_type)` pairs for scoped inbox quarantine diagnostics.
    pub feed_mappings: &'a [(&'a str, &'a str)],
    /// Per-row visibility callback checked before reading evidence findings (`HOST-READ-2`).
    pub evidence_access: RowAccessDecider<'a>,
    /// Declared transforms whose boolean coverage column must be `true` on every row.
    pub coverage_transforms: &'a [CoverageTransformGate<'a>],
    /// Declared reconciliation and freshness gates over admitted transforms.
    pub reconciliation_gates: &'a [ReconciliationGate<'a>],
    /// Declared expected-grain coverage gates binding evaluated questions to expected populations.
    pub expected_grain_gates: &'a [ExpectedGrainGate<'a>],
    /// Named admitted questions to evaluate at this cell's pinned cut.
    pub questions: &'a [(&'a str, &'a QuestionPlan)],
}

/// Pinned local frontier, coverage, and disclosed question answers for one cell.
///
/// Omits raw ledger hashes (`DATA-VALUES-1`) and keys answer lineage/revision/finding
/// handles according to [`CellViewInput::whole_world_values`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CellCutSnapshot {
    /// Durable cell identity bound to this store.
    pub cell_id: CellId,
    /// Scoped authority boundary of this cell.
    pub scope: String,
    /// World name evaluated (for example `"main"`).
    pub world: String,
    /// Verified commit count at the read cut.
    pub revision: u64,
    /// Last ledger event sequence number at the read cut.
    pub ledger_seq: u64,
    /// Active installed definitions version at the read cut.
    pub definition_version: u64,
    /// Active installed definitions admission receipt digest (or model hash).
    pub definition_pin: String,
    /// Verified `READ-LOCAL-1` authority pin for this cell's read cut.
    pub authority_pin: AuthorityPin,
    /// Verified `READ-LOCAL-1` read token binding frontier, definitions, and authority.
    pub read_token: ReadToken,
    /// Source cursor frontier pinned to every answer in this cut.
    pub source_cursors: Frontier,
    /// Partition watermarks pinned to every answer in this cut.
    pub watermarks: BTreeMap<String, BTreeMap<String, Watermark>>,
    /// Required feeds that are absent, incomplete, pending, or older than `min_fresh_ms`.
    pub missing_feeds: BTreeSet<String>,
    /// Open data-quality findings across authorized visible evidence rows (`HOST-READ-2`).
    pub open_findings: usize,
    /// Unconsumed inbox observations quarantined at mapping time.
    pub pending_observations: usize,
    /// Declared coverage, reconciliation, or expected-grain gates that failed at this cut.
    pub failed_coverage_gates: BTreeSet<String>,
    /// True only when all answers are complete and fresh with zero open feed, evidence, or coverage issues.
    pub complete: bool,
    /// Disclosed question answers keyed by declared question name, withheld (`None`)
    /// whenever `complete` is `false`.
    pub answers: Option<BTreeMap<String, QueryResult>>,
}

/// Read-only combined projection across multiple independent cells (`READ-LOCAL-1`).
///
/// Carries no cell handles or write authority; every cell retains its own local
/// frontier, definition pin, and coverage diagnostics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CombinedViewReport {
    /// Explicit view classification.
    pub view_kind: CombinedViewKind,
    /// Always `false` for [`CombinedViewKind::CombinedManagementView`].
    pub is_consolidated_financial_statement: bool,
    /// Always `true`: combined views never grant or execute writes.
    pub read_only: bool,
    /// Independent per-cell snapshots in caller-specified order.
    pub cells: Vec<CellCutSnapshot>,
}

fn capture_cut_anchor<E: SqlEngine>(
    input: &CellViewInput<'_, E>,
) -> Result<(SnapshotPin, LedgerHead), CellError> {
    if input.authority.resource_kind != input.anchor_key.object_type.as_str()
        || input.authority.resource_id != input.anchor_key.key
    {
        return Err(CellError::Encoding(
            "read authority resource does not match the question".to_owned(),
        ));
    }
    if let Some(cell_scope) = crate::definition_scope::bound_scope(&input.store.conn)?
        && cell_scope != input.context.scope
    {
        return Err(CellError::ComputeRefused("core.compute.stale_input"));
    }
    input.store.snapshot::<Main, _>(|snapshot| {
        let pin = snapshot.pin().clone();
        if pin.world.encode() != input.context.world {
            return Err(CellError::ComputeRefused("core.compute.stale_input"));
        }
        let Some(anchor) =
            snapshot.object(input.anchor_key.object_type.as_str(), &input.anchor_key.key)?
        else {
            return Err(CellError::ComputeRefused("core.compute.stale_input"));
        };
        if anchor.scope != input.context.scope {
            return Err(CellError::ComputeRefused("core.compute.stale_input"));
        }
        Ok((pin, snapshot.head().clone()))
    })
}

fn build_read_token(
    pin: SnapshotPin,
    authority_pin: AuthorityPin,
    complete: bool,
    exact_freshness: bool,
) -> ReadToken {
    ReadToken {
        format: ReadFormat::V1,
        context: ReadContext {
            frontier: LocalFrontier {
                cell: pin.cell,
                world: pin.world.encode(),
                revision: pin.revision,
            },
            definition_pin: pin.definitions,
            authority_pin,
        },
        completeness: if complete {
            Completeness::Complete
        } else {
            Completeness::Partial
        },
        semantics: if exact_freshness {
            ReadSemantics::Exact
        } else {
            ReadSemantics::Stale
        },
    }
}

fn evaluate_pinned_questions<E: SqlEngine>(
    input: &CellViewInput<'_, E>,
    head_before: &LedgerHead,
    definition_pin: &str,
    evaluate: &mut impl FnMut(&str, &QuestionPlan, QueryContext) -> Result<QueryResult, CellError>,
) -> Result<BTreeMap<String, QueryResult>, CellError> {
    let stale = || CellError::ComputeRefused("core.compute.stale_input");
    let mut answers = BTreeMap::new();
    for (name, plan) in input.questions {
        let (expected_query_pin, expected_def_pin) = input.store.question_pins(plan, None)?;
        if expected_def_pin != definition_pin {
            return Err(stale());
        }
        let result = evaluate(name, plan, input.context.clone())?;
        if result.query_pin != expected_query_pin
            || result.revision != *head_before
            || result.known_as_of != head_before.commits
            || result.definition_pin != definition_pin
            || result.context != input.context
        {
            return Err(stale());
        }
        answers.insert((*name).to_owned(), result);
    }
    Ok(answers)
}

/// Apply `DATA-VALUES-1` (D52/D53) per-member Values withholding, minimum-bucket
/// suppression, `membership_digest` redaction, Population fail-closed checks
/// (`RowAccess::None`), and keyed revision/finding/lineage handles to one
/// [`QueryResult`] inside the caller's fenced snapshot.
fn disclose_query_result<E: SqlEngine>(
    input: &CellViewInput<'_, E>,
    result: &mut QueryResult,
) -> Result<(), CellError> {
    let reader = input.store.read();
    let min_bucket = input.min_disclosed_bucket.max(1);
    let mut any_withheld = false;
    for row in result.rows.values_mut() {
        let mut withheld = BTreeSet::new();
        let mut bucket_restricted = false;
        for id in row.members.keys() {
            let access = reader
                .object(&input.context.world, result.grain.as_str(), id)?
                .map(|obj| (input.member_access)(&obj))
                .transpose()?;
            match access {
                Some(RowAccess::Read) => {}
                Some(RowAccess::Count) => {
                    withheld.insert(id.clone());
                    any_withheld = true;
                    bucket_restricted = true;
                }
                Some(RowAccess::None) | None => {
                    return Err(CellError::ComputeRefused("core.compute.stale_input"));
                }
            }
        }
        if row.member_count.is_some() {
            let gate = ObjectRecord {
                object_type: result.grain.as_str().to_owned(),
                id: ALL_MEMBERS.to_owned(),
                scope: input.context.scope.clone(),
                version: 0,
                values: BTreeMap::new(),
            };
            match (input.member_access)(&gate)? {
                RowAccess::Read => {
                    if !input.whole_world_values {
                        withheld.insert(ALL_MEMBERS.to_owned());
                        any_withheld = true;
                        bucket_restricted = true;
                    }
                }
                RowAccess::Count => {
                    withheld.insert(ALL_MEMBERS.to_owned());
                    any_withheld = true;
                    bucket_restricted = true;
                }
                RowAccess::None => {
                    return Err(CellError::ComputeRefused("core.compute.stale_input"));
                }
            }
        }
        row.withhold(|id| withheld.contains(id), min_bucket);
        if bucket_restricted {
            row.membership_digest = None;
        }
    }
    let world = input.whole_world_values && !any_withheld;
    let genesis = result.revision.seq == 0 && result.revision.hash == GENESIS_HASH;
    if !(world || genesis) {
        result.revision.hash = keyed_revision_handle(input.handle_key, &result.revision.hash);
    }
    if !world {
        for finding in result.exclusions.values_mut() {
            *finding = keyed_finding_handle(input.handle_key, finding);
        }
    }
    result.lineage_ref = keyed_lineage_handle(input.handle_key, &result.lineage_ref);
    Ok(())
}

fn disclose_answers<E: SqlEngine>(
    input: &CellViewInput<'_, E>,
    answers: &mut BTreeMap<String, QueryResult>,
) -> Result<(), CellError> {
    for result in answers.values_mut() {
        disclose_query_result(input, result)?;
    }
    Ok(())
}

struct CutEvidence {
    pin: SnapshotPin,
    head: LedgerHead,
    definition_pin: String,
    answers: BTreeMap<String, QueryResult>,
    failed_coverage_gates: BTreeSet<String>,
    open_findings: usize,
    withheld_evidence: bool,
    pending_observations: usize,
    pending_captures: BTreeMap<String, Vec<PendingSourceCapture>>,
}

fn assemble_cell_cut_snapshot<E: SqlEngine>(
    input: &CellViewInput<'_, E>,
    ev: CutEvidence,
) -> CellCutSnapshot {
    let mut missing_feeds = check_missing_feeds(
        input.required_feeds,
        &input.context.source_cursors,
        &input.context.watermarks,
        input.min_fresh_ms,
    );
    for (feed, captures) in &ev.pending_captures {
        if !captures.is_empty() {
            missing_feeds.insert(feed.clone());
        }
    }
    let all_fresh = ev
        .answers
        .values()
        .all(|q| q.complete && matches!(q.freshness, Freshness::Exact | Freshness::Fresh));
    let complete = all_fresh
        && missing_feeds.is_empty()
        && ev.open_findings == 0
        && !ev.withheld_evidence
        && ev.pending_observations == 0
        && ev.failed_coverage_gates.is_empty();
    let authority_pin = AuthorityPin::from(input.authority);
    let cell_id = ev.pin.cell.clone();
    let definition_version = ev.pin.definitions.version;
    let read_token = build_read_token(
        ev.pin,
        authority_pin.clone(),
        complete,
        all_fresh && missing_feeds.is_empty(),
    );
    CellCutSnapshot {
        cell_id,
        scope: input.context.scope.clone(),
        world: input.context.world.clone(),
        revision: ev.head.commits,
        ledger_seq: ev.head.seq,
        definition_version,
        definition_pin: ev.definition_pin,
        authority_pin,
        read_token,
        source_cursors: input.context.source_cursors.clone(),
        watermarks: input.context.watermarks.clone(),
        missing_feeds,
        open_findings: ev.open_findings,
        pending_observations: ev.pending_observations,
        failed_coverage_gates: ev.failed_coverage_gates,
        complete,
        answers: complete.then_some(ev.answers),
    }
}

/// Evaluate one cell's pinned read cut using the supplied question evaluator
/// (either the cell's exact evaluator or a synced OLAP engine).
///
/// # Errors
/// Returns `CellError::Encoding` if `authority` does not match `anchor_key`,
/// `CellError::SnapshotWorldMismatch` on a fork store,
/// `CellError::ComputeRefused("core.compute.duplicate_question")` if two
/// requested questions share the same name (validated before any evaluator call),
/// `CellError::ComputeRefused("core.compute.stale_input")` if the anchor
/// object is absent, durable source cursors disagree with `context.source_cursors`,
/// or the snapshot version, ledger head, definition pin, query context, durable
/// source cursor / pending capture state, live feed frontier, or
/// pending-observation identity set shifts during evaluation, or propagates
/// underlying store/evaluator errors.
pub fn evaluate_cell_cut<E: SqlEngine>(
    input: &CellViewInput<'_, E>,
    evaluate: &mut impl FnMut(&str, &QuestionPlan, QueryContext) -> Result<QueryResult, CellError>,
) -> Result<CellCutSnapshot, CellError> {
    if input.handle_key.len() < 32 {
        return Err(CellError::Encoding(
            "the lineage handle key is shorter than 32 bytes".to_owned(),
        ));
    }
    let mut seen_questions = BTreeSet::new();
    for (name, _) in input.questions {
        if !seen_questions.insert(*name) {
            return Err(CellError::ComputeRefused("core.compute.duplicate_question"));
        }
    }
    verify_live_frontier(input)?;
    let version_before = input.store.read().snapshot_version()?;
    let (pin, head_before) = capture_cut_anchor(input)?;
    let definition_pin = pin
        .definitions
        .admission_receipt
        .clone()
        .unwrap_or_else(|| pin.definitions.model_hash.clone());
    let pending_before = pending_identities(input.store)?;
    let durable_feeds_before = durable_feed_cut(input.store, input.required_feeds)?;
    verify_durable_cursors_match(
        input.required_feeds,
        &durable_feeds_before.0,
        &input.context.source_cursors,
    )?;
    let mut answers = evaluate_pinned_questions(input, &head_before, &definition_pin, evaluate)?;
    disclose_answers(input, &mut answers)?;
    let failed_coverage_gates = evaluate_all_gates(input, &head_before, &definition_pin, &answers)?;
    let (open_findings, withheld_evidence) = count_open_findings(
        input.store,
        &input.context.world,
        &input.context.scope,
        input.evidence_types,
        input.evidence_access,
    )?;
    let scoped_pending_observations = count_scoped_pending_observations(input)?;
    let pending_after = pending_identities(input.store)?;
    let durable_feeds_after = durable_feed_cut(input.store, input.required_feeds)?;
    let head_after = input.store.verified_head()?;
    let version_after = input.store.read().snapshot_version()?;
    verify_live_frontier(input)?;
    if version_after != version_before
        || head_after != head_before
        || pending_after != pending_before
        || durable_feeds_after != durable_feeds_before
    {
        return Err(CellError::ComputeRefused("core.compute.stale_input"));
    }
    Ok(assemble_cell_cut_snapshot(
        input,
        CutEvidence {
            pin,
            head: head_before,
            definition_pin,
            answers,
            failed_coverage_gates,
            open_findings,
            withheld_evidence,
            pending_observations: scoped_pending_observations,
            pending_captures: durable_feeds_after.1,
        },
    ))
}

/// Evaluate a read-only combined management view across independent cells
/// (`READ-LOCAL-1`), refusing duplicate cell identities and unstable cuts.
///
/// # Errors
/// Returns `CellError::ComputeRefused("core.compute.duplicate_cell")` if two
/// inputs resolve to the same [`CellId`], or propagates any error from
/// [`evaluate_cell_cut`].
pub fn evaluate_combined_management_view<E: SqlEngine>(
    inputs: &[CellViewInput<'_, E>],
    mut evaluate: impl FnMut(usize, &str, &QuestionPlan, QueryContext) -> Result<QueryResult, CellError>,
) -> Result<CombinedViewReport, CellError> {
    let mut seen_cells = BTreeSet::new();
    let mut cells = Vec::with_capacity(inputs.len());
    for (idx, input) in inputs.iter().enumerate() {
        let snap = evaluate_cell_cut(input, &mut |name, plan, ctx| evaluate(idx, name, plan, ctx))?;
        if !seen_cells.insert(snap.cell_id.clone()) {
            return Err(CellError::ComputeRefused("core.compute.duplicate_cell"));
        }
        cells.push(snap);
    }
    Ok(CombinedViewReport {
        view_kind: CombinedViewKind::CombinedManagementView,
        is_consolidated_financial_statement: false,
        read_only: true,
        cells,
    })
}
