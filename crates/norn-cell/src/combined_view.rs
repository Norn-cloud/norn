//! Domain-agnostic read-only multi-cell combined management view (`READ-LOCAL-1`).
//!
//! Projects multiple independent [`CellStore`] instances side-by-side at pinned
//! local cuts without commingling ledgers, currencies, or write authority.
//! Each cell snapshot verifies a single stable ledger head, definition pin,
//! durable source cursor table, live feed frontier, and pending-observation
//! identity set across all evaluated questions and declared coverage gates, and
//! fails closed (`core.compute.stale_input`) if any boundary disagrees or shifts
//! during the read. Visibility precedes data quality (`HOST-READ-2`), and raw
//! ledger hashes are never exposed (`DATA-VALUES-1`).

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use norn_checker::metric::compute::QuestionPlan;
use norn_checker::transform::TransformPlan;
use norn_ir::RuntimeValue;
use norn_ir::cell_id::CellId;
use norn_ir::data_quality::DataQualityFindingStatus;
use norn_kernel::FactKey;
use norn_kernel::authz::CommitBinding;
use norn_kernel::read_envelope::{
    AuthorityPin, Completeness, LocalFrontier, ReadContext, ReadFormat, ReadSemantics, ReadToken,
};
use norn_kernel::world::pin::SnapshotPin;
use serde::{Deserialize, Serialize};

use crate::compute::transform::{RowAccess, TransformResult, TransformRow};
use crate::compute::{Freshness, Frontier, QueryContext, QueryResult, Watermark};
use crate::engine::SqlEngine;
use crate::error::CellError;
use crate::read::{LedgerHead, ObjectRecord};
use crate::snapshot::Main;
use crate::source_cursor::{PendingSourceCapture, SourceCursorRecord};
use crate::store::CellStore;

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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconciliationComparison {
    /// Observed quantity must equal expected quantity exactly (for example
    /// physical on-hand count vs derived lot remaining quantity).
    Eq,
    /// Observed quantity must not exceed expected quantity (for example
    /// available quantity vs on-hand quantity).
    Lte,
}

/// Domain-agnostic reconciliation and timestamp-freshness gate between two
/// admitted transforms sharing a grain key at the same pinned cut.
pub struct ReconciliationGate<'a> {
    /// Gate identifier reported in [`CellCutSnapshot::failed_coverage_gates`].
    pub name: &'a str,
    /// Admitted transform enumerating expected population rows and values.
    pub expected_plan: &'a TransformPlan,
    /// Admitted transform enumerating observed rows, values, and timestamps.
    pub observed_plan: &'a TransformPlan,
    /// Shared grain columns matching each expected row to one observed row.
    pub key_columns: &'a [&'a str],
    /// Numeric column name in `expected_plan`.
    pub expected_value_column: &'a str,
    /// Numeric column name in `observed_plan`.
    pub observed_value_column: &'a str,
    /// Comparison required between observed and expected numeric values.
    pub comparison: ReconciliationComparison,
    /// RFC 3339 timestamp column in `observed_plan` checked for freshness (`<= now_ms`, age `<= max_age_ms`).
    pub observed_timestamp_column: &'a str,
    /// Maximum allowed age (milliseconds) relative to `QueryContext::now_ms`.
    pub max_age_ms: u64,
    /// Pre-evaluation authorization check for `expected_plan` (`VIEW-READ-1`).
    pub authorize_expected: TransformAuthorizer<'a>,
    /// Pre-evaluation authorization check for `observed_plan` (`VIEW-READ-1`).
    pub authorize_observed: TransformAuthorizer<'a>,
    /// Per-row visibility callback (`HOST-READ-2`).
    pub row_access: RowAccessDecider<'a>,
}

/// Domain-agnostic expected-grain completeness gate ensuring every expected
/// grain key emitted by an admitted transform is covered by an evaluated
/// question's buckets (optionally requiring non-null, unsuppressed values).
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
    /// Required feed names whose durable cursors and live watermarks must match and be fresh.
    pub required_feeds: &'a [&'a str],
    /// Required live feed-frontier reader checked before and after evaluation so stale or
    /// mid-read shifted cursors/watermarks fail closed (`core.compute.stale_input`).
    pub read_frontier: FrontierReader<'a>,
    /// Minimum acceptable watermark heartbeat timestamp (milliseconds since epoch).
    pub min_fresh_ms: u64,
    /// Evidence object types scanned for open data-quality findings.
    pub evidence_types: &'a [&'a str],
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

/// Pinned local frontier, coverage, and exact question answers for one cell.
///
/// Omits raw ledger hashes (`DATA-VALUES-1`) so partial or scoped cuts never
/// disclose value-committing hashes.
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
    /// Exact question answers keyed by declared question name, withheld (`None`)
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

type DurableFeedCut = (
    BTreeMap<String, BTreeMap<String, SourceCursorRecord>>,
    BTreeMap<String, Vec<PendingSourceCapture>>,
);

fn pending_identities<E: SqlEngine>(store: &CellStore<E>) -> Result<Vec<(u64, String)>, CellError> {
    Ok(store
        .read()
        .pending_observations()?
        .into_iter()
        .map(|row| (row.position, row.observation_id))
        .collect())
}

fn check_missing_feeds(
    required_feeds: &[&str],
    cursors: &Frontier,
    watermarks: &BTreeMap<String, BTreeMap<String, Watermark>>,
    min_fresh_ms: u64,
) -> BTreeSet<String> {
    required_feeds
        .iter()
        .filter(|feed| {
            let Some(parts) = cursors.get(**feed) else {
                return true;
            };
            if parts.is_empty() {
                return true;
            }
            let Some(wm_parts) = watermarks.get(**feed) else {
                return true;
            };
            wm_parts.keys().any(|part| !parts.contains_key(part))
                || parts.iter().any(|(part, consumed)| {
                    wm_parts.get(part).is_none_or(|wm| {
                        !wm.complete
                            || wm.observed != *consumed
                            || wm.heartbeat_ms.is_none_or(|hb| hb < min_fresh_ms)
                    })
                })
        })
        .map(|feed| (*feed).to_owned())
        .collect()
}

fn durable_feed_cut<E: SqlEngine>(
    store: &CellStore<E>,
    required_feeds: &[&str],
) -> Result<DurableFeedCut, CellError> {
    let reader = store.read();
    let mut cursors = BTreeMap::new();
    let mut pending = BTreeMap::new();
    for feed in required_feeds {
        cursors.insert((*feed).to_owned(), reader.source_cursors(feed)?);
        pending.insert((*feed).to_owned(), reader.pending_source_captures(feed)?);
    }
    Ok((cursors, pending))
}

fn verify_durable_cursors_match(
    required_feeds: &[&str],
    durable: &BTreeMap<String, BTreeMap<String, SourceCursorRecord>>,
    context_cursors: &Frontier,
) -> Result<(), CellError> {
    let stale = || CellError::ComputeRefused("core.compute.stale_input");
    for feed in required_feeds {
        let durable_parts = durable.get(*feed);
        let ctx_parts = context_cursors.get(*feed);
        let durable_len = durable_parts.map_or(0, BTreeMap::len);
        let ctx_len = ctx_parts.map_or(0, BTreeMap::len);
        if durable_len != ctx_len {
            return Err(stale());
        }
        if let (Some(d_map), Some(c_map)) = (durable_parts, ctx_parts) {
            for (part, consumed) in c_map {
                let Some(record) = d_map.get(part) else {
                    return Err(stale());
                };
                if record.cursor_json != *consumed {
                    return Err(stale());
                }
            }
        }
    }
    Ok(())
}

fn verify_live_frontier<E: SqlEngine>(input: &CellViewInput<'_, E>) -> Result<(), CellError> {
    let (cursors, watermarks) = (input.read_frontier)()?;
    if cursors != input.context.source_cursors || watermarks != input.context.watermarks {
        return Err(CellError::ComputeRefused("core.compute.stale_input"));
    }
    Ok(())
}

/// Count open data-quality findings strictly over evidence rows the reader may
/// read (`HOST-READ-2`: visibility precedes data quality). `RowAccess::None`
/// rows are invisible and skipped before inspecting findings; `RowAccess::Count`
/// rows withhold finding counts and mark evidence coverage incomplete.
fn count_open_findings<E: SqlEngine>(
    store: &CellStore<E>,
    world: &str,
    scope: &str,
    evidence_types: &[&str],
    evidence_access: RowAccessDecider<'_>,
) -> Result<(usize, bool), CellError> {
    let reader = store.read();
    let mut open = 0_usize;
    let mut withheld = false;
    for ty in evidence_types {
        let mut after: Option<String> = None;
        loop {
            let page = reader.ids_after(world, ty, after.as_deref(), 1_000)?;
            if page.is_empty() {
                break;
            }
            for id in &page {
                let Some(record) = reader.object(world, ty, id)? else {
                    continue;
                };
                if record.scope != scope {
                    return Err(CellError::ComputeRefused("core.compute.stale_input"));
                }
                match evidence_access(&record)? {
                    RowAccess::None => continue,
                    RowAccess::Count => {
                        withheld = true;
                        continue;
                    }
                    RowAccess::Read => {}
                }
                let found = reader.observation_findings(world, ty, id)?;
                open = open.saturating_add(
                    found
                        .into_iter()
                        .filter(|f| matches!(f.status, DataQualityFindingStatus::Open))
                        .count(),
                );
            }
            if page.len() < 1_000 {
                break;
            }
            after = page.last().cloned();
        }
    }
    Ok((open, withheld))
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
    input.store.snapshot::<Main, _>(|snapshot| {
        let pin = snapshot.pin().clone();
        if pin.world.encode() != input.context.world {
            return Err(CellError::ComputeRefused("core.compute.stale_input"));
        }
        if snapshot
            .object(input.anchor_key.object_type.as_str(), &input.anchor_key.key)?
            .is_none()
        {
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

fn eval_pinned_transform<E: SqlEngine>(
    store: &CellStore<E>,
    ctx: &QueryContext,
    head_before: &LedgerHead,
    definition_pin: &str,
    plan: &TransformPlan,
    authorize: TransformAuthorizer<'_>,
    row_access: RowAccessDecider<'_>,
) -> Result<TransformResult, CellError> {
    authorize(plan)?;
    let tf = store.evaluate_transform(plan, &ctx.world, &ctx.scope, |row| row_access(row))?;
    if tf.revision != *head_before || tf.definition_pin != definition_pin {
        return Err(CellError::ComputeRefused("core.compute.stale_input"));
    }
    Ok(tf)
}

fn instant_fresh(value: Option<&RuntimeValue>, now_ms: u64, max_age_ms: u64) -> bool {
    let Some(RuntimeValue::String(text)) = value else {
        return false;
    };
    let Some((secs, nanos)) = norn_checker::transform::instant(text) else {
        return false;
    };
    let Ok(now_secs) = i64::try_from(now_ms / 1_000) else {
        return false;
    };
    let now_nanos = u32::try_from((now_ms % 1_000) * 1_000_000).unwrap_or(0);
    if (secs, nanos) > (now_secs, now_nanos) {
        return false;
    }
    let min_ms = now_ms.saturating_sub(max_age_ms);
    let Ok(min_secs) = i64::try_from(min_ms / 1_000) else {
        return false;
    };
    let min_nanos = u32::try_from((min_ms % 1_000) * 1_000_000).unwrap_or(0);
    (secs, nanos) >= (min_secs, min_nanos)
}

fn row_key(row: &TransformRow, columns: &[&str]) -> Option<Vec<RuntimeValue>> {
    let mut key = Vec::with_capacity(columns.len());
    for col in columns {
        key.push(row.values.get(*col)?.clone());
    }
    Some(key)
}

fn check_reconciliation_gate(
    gate: &ReconciliationGate<'_>,
    expected: &TransformResult,
    observed: &TransformResult,
    now_ms: u64,
) -> bool {
    if !expected.complete || !observed.complete {
        return false;
    }
    let mut obs_map: HashMap<Vec<RuntimeValue>, &TransformRow> =
        HashMap::with_capacity(observed.rows.len());
    for row in &observed.rows {
        if !instant_fresh(
            row.values.get(gate.observed_timestamp_column),
            now_ms,
            gate.max_age_ms,
        ) {
            return false;
        }
        let Some(key) = row_key(row, gate.key_columns) else {
            return false;
        };
        if obs_map.insert(key, row).is_some() {
            return false;
        }
    }
    for exp_row in &expected.rows {
        let Some(key) = row_key(exp_row, gate.key_columns) else {
            return false;
        };
        let Some(obs_row) = obs_map.get(&key) else {
            return false;
        };
        let (Some(RuntimeValue::Number(exp_val)), Some(RuntimeValue::Number(obs_val))) = (
            exp_row.values.get(gate.expected_value_column),
            obs_row.values.get(gate.observed_value_column),
        ) else {
            return false;
        };
        let matched = match gate.comparison {
            ReconciliationComparison::Eq => obs_val == exp_val,
            ReconciliationComparison::Lte => obs_val <= exp_val,
        };
        if !matched {
            return false;
        }
    }
    true
}

fn bucket_tuple(bucket: &RuntimeValue, indices: &[usize]) -> Option<Vec<RuntimeValue>> {
    let mut tuple = Vec::with_capacity(indices.len());
    match bucket {
        RuntimeValue::Array(items) => {
            for &idx in indices {
                tuple.push(items.get(idx)?.clone());
            }
        }
        scalar => {
            if indices != [0] {
                return None;
            }
            tuple.push(scalar.clone());
        }
    }
    Some(tuple)
}

fn check_expected_grain_gate(
    gate: &ExpectedGrainGate<'_>,
    expected: &TransformResult,
    answers: &BTreeMap<String, QueryResult>,
) -> bool {
    if !expected.complete {
        return false;
    }
    let Some(result) = answers.get(gate.question_name) else {
        return false;
    };
    let mut covered: HashSet<Vec<RuntimeValue>> = HashSet::with_capacity(result.rows.len());
    for row in result.rows.values() {
        if gate.require_non_null_value && (row.value.is_none() || row.suppressed) {
            continue;
        }
        if let Some(tuple) = bucket_tuple(&row.bucket, gate.bucket_indices) {
            covered.insert(tuple);
        }
    }
    expected
        .rows
        .iter()
        .all(|row| row_key(row, gate.expected_columns).is_some_and(|key| covered.contains(&key)))
}

fn evaluate_all_gates<E: SqlEngine>(
    input: &CellViewInput<'_, E>,
    head_before: &LedgerHead,
    definition_pin: &str,
    answers: &BTreeMap<String, QueryResult>,
) -> Result<BTreeSet<String>, CellError> {
    let mut failed = BTreeSet::new();
    for gate in input.coverage_transforms {
        let tf = eval_pinned_transform(
            input.store,
            &input.context,
            head_before,
            definition_pin,
            gate.plan,
            gate.authorize_transform,
            gate.row_access,
        )?;
        let all_true = tf.complete
            && tf.rows.iter().all(|row| {
                row.values.get(gate.required_true_column) == Some(&RuntimeValue::Bool(true))
            });
        if !all_true {
            failed.insert(gate.name.to_owned());
        }
    }
    for gate in input.reconciliation_gates {
        let expected = eval_pinned_transform(
            input.store,
            &input.context,
            head_before,
            definition_pin,
            gate.expected_plan,
            gate.authorize_expected,
            gate.row_access,
        )?;
        let observed = eval_pinned_transform(
            input.store,
            &input.context,
            head_before,
            definition_pin,
            gate.observed_plan,
            gate.authorize_observed,
            gate.row_access,
        )?;
        if !check_reconciliation_gate(gate, &expected, &observed, input.context.now_ms) {
            failed.insert(gate.name.to_owned());
        }
    }
    for gate in input.expected_grain_gates {
        let expected = eval_pinned_transform(
            input.store,
            &input.context,
            head_before,
            definition_pin,
            gate.expected_plan,
            gate.authorize_transform,
            gate.row_access,
        )?;
        if !check_expected_grain_gate(gate, &expected, answers) {
            failed.insert(gate.name.to_owned());
        }
    }
    Ok(failed)
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
/// or the ledger head, definition pin, query context, durable source cursor /
/// pending capture state, live feed frontier, or pending-observation identity
/// set shifts during evaluation, or propagates underlying store/evaluator errors.
pub fn evaluate_cell_cut<E: SqlEngine>(
    input: &CellViewInput<'_, E>,
    evaluate: &mut impl FnMut(&str, &QuestionPlan, QueryContext) -> Result<QueryResult, CellError>,
) -> Result<CellCutSnapshot, CellError> {
    let mut seen_questions = BTreeSet::new();
    for (name, _) in input.questions {
        if !seen_questions.insert(*name) {
            return Err(CellError::ComputeRefused("core.compute.duplicate_question"));
        }
    }
    verify_live_frontier(input)?;
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
    let answers = evaluate_pinned_questions(input, &head_before, &definition_pin, evaluate)?;
    let failed_coverage_gates = evaluate_all_gates(input, &head_before, &definition_pin, &answers)?;
    let (open_findings, withheld_evidence) = count_open_findings(
        input.store,
        &input.context.world,
        &input.context.scope,
        input.evidence_types,
        input.evidence_access,
    )?;
    let head_after = input.store.verified_head()?;
    let pending_after = pending_identities(input.store)?;
    let durable_feeds_after = durable_feed_cut(input.store, input.required_feeds)?;
    verify_live_frontier(input)?;
    if head_after != head_before
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
            pending_observations: pending_after.len(),
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
