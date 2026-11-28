//! Feed frontier, evidence quality, inbox quarantine, and transform coverage
//! gates evaluated inside [`super::evaluate_cell_cut`].

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use norn_checker::transform::TransformPlan;
use norn_ir::data_quality::DataQualityFindingStatus;
use norn_ir::mapping::MappedRecord;
use norn_ir::{Decimal, RuntimeValue};
use norn_kernel::Observed;

use crate::compute::transform::{RowAccess, TransformResult, TransformRow};
use crate::compute::{ALL_MEMBERS, Frontier, QueryContext, QueryResult, Watermark};
use crate::engine::SqlEngine;
use crate::error::CellError;
use crate::mapping_bundle::MappingBundle;
use crate::mapping_conflict::MappingConflict;
use crate::mapping_identity::IdentityEvidence;
use crate::read::{LedgerHead, ObjectRecord, PendingObservation, StoreReader};
use crate::source_cursor::{PendingSourceCapture, SourceCursorRecord};
use crate::store::CellStore;

use super::{
    CellViewInput, ExpectedGrainGate, ReconciliationComparison, ReconciliationGate,
    RowAccessDecider, TransformAuthorizer,
};

pub(super) type DurableFeedCut = (
    BTreeMap<String, BTreeMap<String, SourceCursorRecord>>,
    BTreeMap<String, Vec<PendingSourceCapture>>,
);

pub(super) fn pending_identities<E: SqlEngine>(
    store: &CellStore<E>,
) -> Result<Vec<(u64, String)>, CellError> {
    Ok(store
        .read()
        .pending_observations()?
        .into_iter()
        .map(|row| (row.position, row.observation_id))
        .collect())
}

fn authorized_evidence_types<E: SqlEngine>(
    input: &CellViewInput<'_, E>,
) -> Result<BTreeSet<String>, CellError> {
    let mut allowed = BTreeSet::new();
    for ty in input.evidence_types {
        let gate = ObjectRecord {
            object_type: (*ty).to_owned(),
            id: ALL_MEMBERS.to_owned(),
            scope: input.context.scope.clone(),
            version: 0,
            values: BTreeMap::new(),
        };
        if matches!((input.evidence_access)(&gate)?, RowAccess::Read) {
            allowed.insert((*ty).to_owned());
        }
    }
    Ok(allowed)
}

enum PendingCandidate {
    Bundle(MappingBundle),
    Conflict(MappingConflict),
    Identity(String, IdentityEvidence),
}

impl PendingCandidate {
    fn mapping_id(&self) -> &str {
        match self {
            Self::Bundle(bundle) => bundle.input.mapping.as_str(),
            Self::Conflict(conflict) => conflict.incoming.input.mapping.as_str(),
            Self::Identity(_, evidence) => evidence.bundle.input.mapping.as_str(),
        }
    }
}

fn verify_or_ignore(result: Result<(), CellError>) -> Result<bool, CellError> {
    if let Err(err) = result {
        if crate::mapping_page::is_store_failure(&err) {
            return Err(err);
        }
        return Ok(false);
    }
    Ok(true)
}

fn decode_pending_candidate(obs: &PendingObservation) -> Option<PendingCandidate> {
    let observed: Observed<RuntimeValue> = norn_ir::runtime::decode(&obs.observed).ok()?;
    if observed.id.0 != obs.observation_id {
        return None;
    }
    if obs.observation_id.starts_with("mapping-bundle:") {
        let bundle: MappingBundle = norn_ir::runtime::decode(&observed.input).ok()?;
        (obs.observation_id == format!("mapping-bundle:{}", bundle.digest))
            .then_some(PendingCandidate::Bundle(bundle))
    } else if obs.observation_id.starts_with("mapping-conflict:") {
        let conflict: MappingConflict = norn_ir::runtime::decode(&observed.input).ok()?;
        (obs.observation_id == format!("mapping-conflict:{}", conflict.finding.id))
            .then_some(PendingCandidate::Conflict(conflict))
    } else if obs.observation_id.starts_with("mapping-identity:") {
        let evidence: IdentityEvidence = norn_ir::runtime::decode(&observed.input).ok()?;
        (obs.observation_id == format!("mapping-identity:{}", evidence.finding.id)).then_some(
            PendingCandidate::Identity(obs.observation_id.clone(), evidence),
        )
    } else {
        None
    }
}

fn bundle_records_visible<E: SqlEngine>(
    input: &CellViewInput<'_, E>,
    allowed_evidence: &BTreeSet<String>,
    bundle: &MappingBundle,
) -> Result<bool, CellError> {
    if bundle.records.is_empty() {
        return Ok(true);
    }
    for rec in &bundle.records {
        if pending_record_readable(input, allowed_evidence, rec)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn verify_visible_pending_candidate<E: SqlEngine>(
    input: &CellViewInput<'_, E>,
    reader: &StoreReader<'_, E>,
    allowed_evidence: &BTreeSet<String>,
    candidate: PendingCandidate,
) -> Result<bool, CellError> {
    match candidate {
        PendingCandidate::Bundle(bundle) => {
            if !verify_or_ignore(reader.recheck_mapping_bundle(&bundle))? {
                return Ok(false);
            }
            bundle_records_visible(input, allowed_evidence, &bundle)
        }
        PendingCandidate::Conflict(conflict) => {
            // 1. Verify the incoming bundle itself (ignore invalid/forged candidates).
            if !verify_or_ignore(reader.recheck_mapping_bundle(&conflict.incoming))? {
                return Ok(false);
            }
            // 2. Apply row visibility on the rederived incoming bundle BEFORE looking up
            //    related retained evidence so excluded conflicts never leak refusals.
            if !bundle_records_visible(input, allowed_evidence, &conflict.incoming)? {
                return Ok(false);
            }
            // 3. Verify all committed bundles bound to this event and compare against
            //    related retained source-event evidence; propagate any verification error
            //    so a corrupted accepted or related bundle fails closed.
            let event_keys =
                crate::mapping_identity::keys(&conflict.incoming.raw, &conflict.incoming.input)?
                    .map(|k| norn_ir::canonical::nfc(&k));
            reader.verify_committed_event_bundles(&event_keys)?;
            let expected = reader.mapping_conflict(&conflict.incoming)?;
            Ok(expected.as_ref() == Some(&conflict))
        }
        PendingCandidate::Identity(stored_id, evidence) => {
            if !verify_or_ignore(reader.recheck_identity_evidence(&evidence, &stored_id))? {
                return Ok(false);
            }
            bundle_records_visible(input, allowed_evidence, &evidence.bundle)
        }
    }
}

fn pending_record_readable<E: SqlEngine>(
    input: &CellViewInput<'_, E>,
    allowed_evidence: &BTreeSet<String>,
    rec: &MappedRecord,
) -> Result<bool, CellError> {
    if !allowed_evidence.contains(rec.object_type.0.as_str()) {
        return Ok(false);
    }
    let Some(invocation) = rec.invocations.first() else {
        return Ok(false);
    };
    let record = ObjectRecord {
        object_type: rec.object_type.0.clone(),
        id: rec.key.clone(),
        scope: input.context.scope.clone(),
        version: 0,
        values: invocation.input.clone(),
    };
    Ok(matches!((input.evidence_access)(&record)?, RowAccess::Read))
}

/// Count unconsumed retained mapping bundles in the inbox scoped strictly to
/// `input.feed_mappings` and each mapping's authorized evidence type
/// (`HOST-READ-2`), filtering by configured mapping and readable type before
/// rechecking retained evidence, and applying row visibility on rederived
/// incoming conflict bundles before looking up related source-event evidence.
pub(super) fn count_scoped_pending_observations<E: SqlEngine>(
    input: &CellViewInput<'_, E>,
) -> Result<usize, CellError> {
    let allowed_evidence = authorized_evidence_types(input)?;
    if allowed_evidence.is_empty() || input.feed_mappings.is_empty() {
        return Ok(0);
    }
    let mappings: BTreeMap<&str, &str> = input.feed_mappings.iter().copied().collect();
    let reader = input.store.read();
    let mut count = 0_usize;
    for obs in reader.pending_observations()? {
        let Some(candidate) = decode_pending_candidate(&obs) else {
            continue;
        };
        let Some(expected_evidence) = mappings.get(candidate.mapping_id()).copied() else {
            continue;
        };
        if !allowed_evidence.contains(expected_evidence) {
            continue;
        }
        if verify_visible_pending_candidate(input, &reader, &allowed_evidence, candidate)? {
            count = count.saturating_add(1);
        }
    }
    Ok(count)
}

pub(super) fn check_missing_feeds(
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

pub(super) fn durable_feed_cut<E: SqlEngine>(
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

pub(super) fn verify_durable_cursors_match(
    required_feeds: &[&str],
    durable: &BTreeMap<String, BTreeMap<String, SourceCursorRecord>>,
    context_cursors: &Frontier,
) -> Result<(), CellError> {
    let stale = || CellError::ComputeRefused("core.compute.stale_input");
    for feed in required_feeds {
        let durable_parts = durable.get(*feed);
        let ctx_parts = context_cursors.get(*feed);
        if durable_parts.map_or(0, BTreeMap::len) != ctx_parts.map_or(0, BTreeMap::len) {
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

pub(super) fn verify_live_frontier<E: SqlEngine>(
    input: &CellViewInput<'_, E>,
) -> Result<(), CellError> {
    let (cursors, watermarks) = (input.read_frontier)()?;
    if cursors != input.context.source_cursors || watermarks != input.context.watermarks {
        return Err(CellError::ComputeRefused("core.compute.stale_input"));
    }
    Ok(())
}

/// Count open data-quality findings strictly over evidence rows the reader may
/// read (`HOST-READ-2`: visibility precedes data quality).
pub(super) fn count_open_findings<E: SqlEngine>(
    store: &CellStore<E>,
    world: &str,
    scope: &str,
    evidence_types: &[&str],
    evidence_access: RowAccessDecider<'_>,
) -> Result<(usize, bool), CellError> {
    let reader = store.read();
    let mut open = 0_usize;
    let mut withheld = false;
    let mut readable_subjects = Vec::new();
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
                let found = reader.review_findings(world, ty, id)?;
                open = open.saturating_add(
                    found
                        .into_iter()
                        .filter(|(f, _)| matches!(f.status, DataQualityFindingStatus::Open))
                        .count(),
                );
                readable_subjects.push(((*ty).to_owned(), id.clone()));
            }
            if page.len() < 1_000 {
                break;
            }
            after = page.last().cloned();
        }
    }
    if world == crate::MAIN_WORLD && !readable_subjects.is_empty() {
        open = open.saturating_add(reader.scoped_open_source_dispute_findings(
            scope,
            &readable_subjects,
            evidence_access,
        )?);
    }
    Ok((open, withheld))
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
    let now_nanos = now_ms
        .checked_rem(1_000)
        .and_then(|ms| ms.checked_mul(1_000_000))
        .and_then(|ns| u32::try_from(ns).ok())
        .unwrap_or(0);
    if (secs, nanos) > (now_secs, now_nanos) {
        return false;
    }
    let min_ms = now_ms.saturating_sub(max_age_ms);
    let Ok(min_secs) = i64::try_from(min_ms / 1_000) else {
        return false;
    };
    let min_nanos = min_ms
        .checked_rem(1_000)
        .and_then(|ms| ms.checked_mul(1_000_000))
        .and_then(|ns| u32::try_from(ns).ok())
        .unwrap_or(0);
    (secs, nanos) >= (min_secs, min_nanos)
}

fn row_key(row: &TransformRow, columns: &[&str]) -> Option<Vec<RuntimeValue>> {
    let mut key = Vec::with_capacity(columns.len());
    for col in columns {
        key.push(row.values.get(*col)?.clone());
    }
    Some(key)
}

fn index_observed_rows<'a>(
    gate: &ReconciliationGate<'_>,
    observed: &'a TransformResult,
    now_ms: u64,
) -> Option<HashMap<Vec<RuntimeValue>, &'a TransformRow>> {
    let mut obs_map = HashMap::with_capacity(observed.rows.len());
    for row in &observed.rows {
        let matches_filters = gate
            .observed_filters
            .iter()
            .all(|(col, want)| row.values.get(*col) == Some(want));
        if !matches_filters {
            continue;
        }
        if let Some(ts_col) = gate.observed_timestamp_column {
            let max_age = gate.max_age_ms?;
            if !instant_fresh(row.values.get(ts_col), now_ms, max_age) {
                return None;
            }
        }
        let key = row_key(row, gate.key_columns)?;
        if obs_map.insert(key, row).is_some() {
            return None;
        }
    }
    Some(obs_map)
}

fn compare_reconciliation(
    comparison: ReconciliationComparison,
    obs_val: Decimal,
    exp_val: Decimal,
) -> bool {
    match comparison {
        ReconciliationComparison::Eq => obs_val == exp_val,
        ReconciliationComparison::Lte => obs_val <= exp_val,
    }
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
    let Some(obs_map) = index_observed_rows(gate, observed, now_ms) else {
        return false;
    };
    let mut exp_keys = HashSet::with_capacity(expected.rows.len());
    for exp_row in &expected.rows {
        let matches_expected = gate
            .expected_filters
            .iter()
            .all(|(col, want)| exp_row.values.get(*col) == Some(want));
        if !matches_expected {
            continue;
        }
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
        if !compare_reconciliation(gate.comparison, *obs_val, *exp_val) {
            return false;
        }
        exp_keys.insert(key);
    }
    let zero = Decimal::from(0_i64);
    for (key, obs_row) in &obs_map {
        if exp_keys.contains(key) {
            continue;
        }
        if gate.require_expected_match {
            return false;
        }
        let Some(RuntimeValue::Number(obs_val)) = obs_row.values.get(gate.observed_value_column)
        else {
            return false;
        };
        if !compare_reconciliation(gate.comparison, *obs_val, zero) {
            return false;
        }
    }
    true
}

fn bucket_element(bucket: &RuntimeValue, index: usize) -> Option<&RuntimeValue> {
    if let RuntimeValue::Array(items) = bucket {
        items.get(index)
    } else if index == 0 {
        Some(bucket)
    } else {
        None
    }
}

fn bucket_tuple(bucket: &RuntimeValue, indices: &[usize]) -> Option<Vec<RuntimeValue>> {
    let mut tuple = Vec::with_capacity(indices.len());
    for &idx in indices {
        tuple.push(bucket_element(bucket, idx)?.clone());
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
        let matches_filters = gate
            .required_bucket_filters
            .iter()
            .all(|(idx, want)| bucket_element(&row.bucket, *idx) == Some(want));
        if !matches_filters {
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

pub(super) fn evaluate_all_gates<E: SqlEngine>(
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
