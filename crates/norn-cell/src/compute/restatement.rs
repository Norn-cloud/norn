//! RESTATE-HISTORY-1 / OUTPUT-ATOMIC: private rebuild, governed atomic selection.
//!
//! The native restatement lifecycle owns approval/activation. This host adapter
//! only stages candidates; it cannot approve them. One receipt-bound fact holds
//! every output, so an activation never exposes a partially rebuilt generation.
use super::{
    Engine, HistoricalQuestionResult, QueryContext, QueryResult, QueryRow, historical,
    ivm::IncrementalQuestion, refused,
};
use crate::{
    CellError, CellStore, CommitBatch, CommitPoint, FactWriteRow, GuardedError, KnowledgeCutoff,
    ObjectRecord, StoreReader,
    engine::SqlEngine,
    reactive::{DecodeBudget, ReactiveLimits},
};
use norn_checker::metric::compute::QuestionPlan;
use norn_ir::{ResourceLimitProfile, RuntimeValue};
use norn_kernel::LogicalTime;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

/// Native concept whose declared actions own restatement approval and activation.
pub const RESTATEMENT_TYPE: &str = "core.analytics.Restatement";
const INVALID: &str = "core.world.invalid_restatement";
const MAX_SLICES: usize = 16;

/// A declared question at one effective/knowledge-time slice.
pub struct RestatementSlice<'a> {
    /// Stable output identity within the generation.
    pub id: &'a str,
    /// The same admitted bounded plan used by exact and incremental execution.
    pub plan: QuestionPlan,
    /// Trusted current authorization and retained source frontier evidence.
    pub context: QueryContext,
    /// Inclusive authenticated knowledge coordinate.
    pub cutoff: KnowledgeCutoff,
}
/// Exact dry-run difference; numeric membership stays separate from enrichment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RestatementDelta {
    /// Previous interpretation, mapping and source frontier, when present.
    pub previous_query: Option<String>,
    /// Previous admitted definition pin.
    pub previous_definition: Option<String>,
    /// Previous mapping pin.
    pub previous_mapping: Option<String>,
    /// Inclusive old source frontier.
    pub previous_frontier: Option<u64>,
    /// Inclusive previous knowledge clock, paired with the source frontier.
    pub previous_knowledge_time_ms: Option<u64>,
    /// Exact old buckets, including versioned numeric contributions to retract.
    pub retract: BTreeMap<String, QueryRow>,
    /// Exact replacement buckets; unchanged contributions retain their identities.
    pub replace: BTreeMap<String, QueryRow>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Generation {
    previous: Option<String>,
    reason: String,
    outputs: BTreeMap<String, HistoricalQuestionResult>,
    deltas: BTreeMap<String, RestatementDelta>,
}
/// A complete analytical generation selected by the native lifecycle.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestatementView {
    /// Content identity of the entire output bundle and dry-run differences.
    pub generation: String,
    /// Prior complete generation, retained for drill-through.
    pub previous_generation: Option<String>,
    /// Recorded analytical reason.
    pub reason: String,
    /// Always `restated`; no posted-book reconciliation is implied.
    pub label: &'static str,
    /// A replacement is building or awaiting governed approval/activation.
    pub stale: bool,
    /// All declared bitemporal slices from the same complete generation.
    pub outputs: BTreeMap<String, HistoricalQuestionResult>,
    /// Exact dry-run changes retained with that generation.
    pub deltas: BTreeMap<String, RestatementDelta>,
}
/// Opaque writer token bound to the exact native slot incarnation.
pub struct RestatementBuild {
    instance: Arc<()>,
    id: String,
    context: QueryContext,
    version: u64,
    values: BTreeMap<String, RuntimeValue>,
}
/// A fully computed candidate; construction alone does not grant approval.
pub struct PreparedRestatement {
    build: RestatementBuild,
    encoded: String,
    digest: String,
}
impl PreparedRestatement {
    pub(crate) fn matches_context(&self, world: &str, scope: &str) -> bool {
        self.build.context.world == world && self.build.context.scope == scope
    }
}
fn check_slot_size(values: &BTreeMap<String, RuntimeValue>) -> Result<(), CellError> {
    let bound = ResourceLimitProfile::norn_limits_v1()
        .slice
        .max_canonical_bytes
        .saturating_sub(8192);
    if u64::try_from(crate::codec::canonical_json(values)?.len())
        .map_err(|_size| refused(INVALID))?
        > bound
    {
        return Err(refused("core.compute.bound_exceeded"));
    }
    Ok(())
}
fn candidate_values(
    build: &RestatementBuild,
    encoded: &str,
    digest: &str,
) -> BTreeMap<String, RuntimeValue> {
    let mut values = build.values.clone();
    values.insert("candidate".into(), RuntimeValue::from(encoded));
    values.insert("candidate_digest".into(), RuntimeValue::from(digest));
    values.insert("status".into(), RuntimeValue::from("building"));
    values
}
fn text<'a>(values: &'a BTreeMap<String, RuntimeValue>, key: &str) -> Result<&'a str, CellError> {
    values
        .get(key)
        .and_then(RuntimeValue::as_str)
        .ok_or_else(|| refused(INVALID))
}
fn decode(encoded: &str, digest: &str) -> Result<Option<Generation>, CellError> {
    if encoded.is_empty() && digest.is_empty() {
        return Ok(None);
    }
    let generation: Generation =
        serde_json::from_str(encoded).map_err(|_invalid| refused(INVALID))?;
    if crate::codec::hash_json(&generation)? != digest
        || generation.outputs.is_empty()
        || generation.outputs.values().any(|a| !a.result.complete)
        || generation.outputs.keys().ne(generation.deltas.keys())
    {
        return Err(refused(INVALID));
    }
    Ok(Some(generation))
}
fn selected(values: &BTreeMap<String, RuntimeValue>) -> Result<(&str, &str), CellError> {
    use super::restatement_state::{Phase, Selection, select};
    let phase = match text(values, "status")? {
        "building" => Phase::Building,
        "ready" => Phase::Ready,
        "approved" => Phase::Approved,
        "active" => Phase::Active,
        _ => return Err(refused(INVALID)),
    };
    let old = (text(values, "active")?, text(values, "active_digest")?);
    let new = (
        text(values, "candidate")?,
        text(values, "candidate_digest")?,
    );
    match select(
        phase,
        !old.0.is_empty() && !old.1.is_empty(),
        !new.0.is_empty() && !new.1.is_empty(),
    ) {
        Some(Selection::Previous) => Ok(old),
        Some(Selection::Candidate) => Ok(new),
        None if phase == Phase::Building && old.0.is_empty() && old.1.is_empty() => Ok(old),
        None => Err(refused(INVALID)),
    }
}
fn authorized(context: &QueryContext, candidate: &QueryContext) -> bool {
    (
        &context.world,
        &context.scope,
        &context.population_pin,
        context.authorization_epoch,
    ) == (
        &candidate.world,
        &candidate.scope,
        &candidate.population_pin,
        candidate.authorization_epoch,
    )
}
fn slot<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    context: &QueryContext,
    id: &str,
    version: u64,
) -> Result<BTreeMap<String, RuntimeValue>, CellError> {
    let limits = ReactiveLimits::from_profile(&ResourceLimitProfile::norn_limits_v1());
    let proof = reader
        .fact_version_evidence_with_budget(
            &context.world,
            RESTATEMENT_TYPE,
            id,
            version,
            &mut DecodeBudget::new(&limits),
        )?
        .ok_or_else(|| refused("core.compute.bound_exceeded"))?;
    let snapshot = proof.snapshot();
    if snapshot.scope != context.scope {
        return Err(refused("core.compute.stale_input"));
    }
    snapshot.values.clone().ok_or_else(|| refused(INVALID))
}
impl<E: SqlEngine> CellStore<E> {
    /// OUTPUT-ATOMIC: retain the complete visible bundle before starting any work.
    /// A failed/interrupted rebuild remains visible as stale across restart.
    /// # Errors
    /// Scope/version conflict, an in-progress build, empty reason or unbounded output set.
    pub fn begin_restatement(
        &mut self,
        id: &str,
        context: QueryContext,
        expected_version: u64,
        reason: &str,
        slice_ids: &BTreeSet<String>,
    ) -> Result<RestatementBuild, CellError> {
        self.check_question_scope(&context.scope)?;
        if id.is_empty()
            || reason.trim().is_empty()
            || slice_ids.is_empty()
            || slice_ids.len() > MAX_SLICES
            || slice_ids.iter().any(String::is_empty)
        {
            return Err(refused(INVALID));
        }
        let mut values = BTreeMap::from([
            ("id".into(), RuntimeValue::from(id)),
            ("status".into(), RuntimeValue::from("building")),
            ("reason".into(), RuntimeValue::from(reason)),
            (
                "slice_ids".into(),
                RuntimeValue::from(serde_json::to_string(slice_ids)?),
            ),
            ("active".into(), RuntimeValue::from("")),
            ("active_digest".into(), RuntimeValue::from("")),
            ("candidate".into(), RuntimeValue::from("")),
            ("candidate_digest".into(), RuntimeValue::from("")),
        ]);
        if expected_version != 0 {
            let old = slot(&self.read(), &context, id, expected_version)?;
            if text(&old, "status")? != "active" {
                return Err(refused(INVALID));
            }
            let (encoded, digest) = selected(&old)?;
            let prior = decode(encoded, digest)?.ok_or_else(|| refused(INVALID))?;
            if prior.outputs.keys().ne(slice_ids.iter()) {
                return Err(refused(INVALID));
            }
            if prior
                .outputs
                .values()
                .any(|answer| !authorized(&context, &answer.result.context))
            {
                return Err(refused("core.compute.stale_input"));
            }
            values.insert("active".into(), RuntimeValue::from(encoded));
            values.insert("active_digest".into(), RuntimeValue::from(digest));
        }
        let mut write = values.clone();
        let transition = if expected_version == 0 {
            None
        } else {
            write.insert("status".into(), RuntimeValue::from("active"));
            Some("rebuild")
        };
        let version = self.write_restatement(
            id,
            &context,
            expected_version,
            write,
            transition,
            &mut |_| Ok(()),
        )?;
        Ok(RestatementBuild {
            instance: Arc::clone(&self.instance_identity),
            id: id.into(),
            context,
            version,
            values,
        })
    }
    /// Resume only the private build; approval is still a declared native action.
    /// # Errors
    /// Missing/unproven slot, scope mismatch or a slot that is not building.
    pub fn resume_restatement(
        &self,
        id: &str,
        context: QueryContext,
    ) -> Result<RestatementBuild, CellError> {
        self.check_question_scope(&context.scope)?;
        self.conn.transaction(|| {
            let version = self.read().version(&context.world, RESTATEMENT_TYPE, id)?;
            let values = slot(&self.read(), &context, id, version)?;
            if text(&values, "status")? != "building" {
                return Err(refused(INVALID));
            }
            Ok(RestatementBuild {
                instance: Arc::clone(&self.instance_identity),
                id: id.into(),
                context,
                version,
                values,
            })
        })
    }
    /// RESTATE-HISTORY-1: compute every slice privately and compare receipt deltas
    /// with the exact evaluator before a candidate can be staged for approval.
    /// Failure writes no candidate or slot state; the separately committed build
    /// remains resumable through `resume_restatement`, including after restart.
    /// # Errors
    /// Missing/duplicate/incomplete slices, authorization changes or correspondence failure.
    pub fn prepare_restatement(
        &self,
        build: RestatementBuild,
        slices: Vec<RestatementSlice<'_>>,
        mut visible: impl FnMut(&ObjectRecord) -> Result<bool, CellError>,
    ) -> Result<PreparedRestatement, CellError> {
        if !Arc::ptr_eq(&self.instance_identity, &build.instance) {
            return Err(refused(INVALID));
        }
        let expected: BTreeSet<String> = serde_json::from_str(text(&build.values, "slice_ids")?)
            .map_err(|_invalid| refused(INVALID))?;
        if expected.is_empty()
            || expected.len() > MAX_SLICES
            || expected.iter().any(String::is_empty)
            || slices.len() != expected.len()
            || slices
                .iter()
                .map(|s| s.id.to_owned())
                .collect::<BTreeSet<_>>()
                != expected
        {
            return Err(refused(INVALID));
        }
        if slices
            .iter()
            .any(|slice| !authorized(&build.context, &slice.context))
        {
            return Err(refused("core.compute.stale_input"));
        }
        let (encoded, previous_digest) = selected(&build.values)?;
        let previous = decode(encoded, previous_digest)?;
        if previous.as_ref().is_some_and(|g| {
            g.outputs.keys().cloned().collect::<BTreeSet<_>>() != expected
                || g.outputs
                    .values()
                    .any(|a| !authorized(&build.context, &a.result.context))
        }) {
            return Err(refused(INVALID));
        }
        let mut generation = Generation {
            previous: previous.as_ref().map(|_| previous_digest.into()),
            reason: text(&build.values, "reason")?.into(),
            outputs: BTreeMap::new(),
            deltas: BTreeMap::new(),
        };
        for slice in slices {
            let answer = self.evaluate_question_known_at(
                &slice.plan,
                slice.context.clone(),
                slice.cutoff,
                &mut visible,
            )?;
            if !answer.result.complete {
                return Err(refused(INVALID));
            }
            let old = previous.as_ref().and_then(|g| g.outputs.get(slice.id));
            if let Some(old) = old {
                self.check_restatement_delta(old, &slice, &answer.result, &mut visible)?;
            }
            generation.deltas.insert(
                slice.id.into(),
                RestatementDelta {
                    previous_query: old.map(|a| a.result.query_pin.clone()),
                    previous_definition: old.map(|a| a.result.definition_pin.clone()),
                    previous_mapping: old.map(|a| a.result.context.mapping_pin.clone()),
                    previous_knowledge_time_ms: old.map(|a| a.knowledge_time_ms),
                    previous_frontier: old.map(|a| a.result.known_as_of),
                    retract: old.map_or_else(BTreeMap::new, |a| a.result.rows.clone()),
                    replace: answer.result.rows.clone(),
                },
            );
            generation.outputs.insert(slice.id.into(), answer);
        }
        let digest = crate::codec::hash_json(&generation)?;
        let encoded = serde_json::to_string(&generation)?;
        check_slot_size(&candidate_values(&build, &encoded, &digest))?;
        Ok(PreparedRestatement {
            build,
            encoded,
            digest,
        })
    }
    fn check_restatement_delta(
        &self,
        old: &HistoricalQuestionResult,
        slice: &RestatementSlice<'_>,
        exact: &QueryResult,
        visible: &mut impl FnMut(&ObjectRecord) -> Result<bool, CellError>,
    ) -> Result<(), CellError> {
        let prior = KnowledgeCutoff {
            recorded_at: LogicalTime::from_millis(old.knowledge_time_ms),
            frontier: old.result.known_as_of,
        };
        if slice.cutoff.frontier < prior.frontier || slice.cutoff.recorded_at < prior.recorded_at {
            return Err(refused(INVALID));
        }
        let pin = historical::query_pin(
            self.model().model_hash(),
            &slice.plan,
            &old.result.definition_pin,
            Some(prior),
        )?;
        // A changed interpretation is an explicit complete rebuild, never reused deltas.
        if pin != old.result.query_pin
            || old.result.context.mapping_pin != slice.context.mapping_pin
        {
            return Ok(());
        }
        if slice.cutoff.frontier.saturating_sub(prior.frontier) > 10_000 {
            return Err(refused("core.compute.bound_exceeded"));
        }
        let mut ivm = IncrementalQuestion::rebuild_known_at(
            self,
            &slice.plan,
            old.result.context.clone(),
            prior,
            &mut *visible,
        )?;
        for frontier in prior.frontier.saturating_add(1)..=slice.cutoff.frontier {
            let receipt = self
                .read()
                .receipt_at(frontier)?
                .ok_or_else(|| refused(INVALID))?;
            let mut context = slice.context.clone();
            // Retain the already selected clock; the authenticated frontier still
            // limits knowledge to this receipt, even when later receipts share it.
            context.now_ms = receipt.body.logical_time.max(prior.recorded_at.as_millis());
            let cutoff = KnowledgeCutoff {
                recorded_at: LogicalTime::from_millis(context.now_ms),
                frontier,
            };
            ivm.advance_known_at(self, context, cutoff, &mut *visible)?;
        }
        ivm.advance_known_at(self, slice.context.clone(), slice.cutoff, &mut *visible)?;
        let mut actual = ivm.result().clone();
        actual.engine = Engine::Cell;
        if &actual != exact {
            return Err(refused(INVALID));
        }
        Ok(())
    }
    /// Stage a complete candidate without changing which output readers see.
    /// # Errors
    /// A stale build token or any transactional failure; the prior bundle remains visible.
    pub fn stage_restatement(&mut self, prepared: PreparedRestatement) -> Result<(), CellError> {
        self.stage_restatement_probed(prepared, &mut |_| Ok(()))
    }
    /// Fault-injectable staging; [`CommitPoint`] models interruption before/after durability.
    /// # Errors
    /// Same as staging, plus the probe's error.
    pub fn stage_restatement_probed(
        &mut self,
        prepared: PreparedRestatement,
        probe: &mut dyn FnMut(CommitPoint) -> Result<(), CellError>,
    ) -> Result<(), CellError> {
        if !Arc::ptr_eq(&self.instance_identity, &prepared.build.instance) {
            return Err(refused(INVALID));
        }
        let PreparedRestatement {
            build,
            encoded,
            digest,
        } = prepared;
        let values = candidate_values(&build, &encoded, &digest);
        self.write_restatement(
            &build.id,
            &build.context,
            build.version,
            values,
            Some("stage"),
            probe,
        )
        .map(|_version| ())
    }
    fn write_restatement(
        &mut self,
        id: &str,
        context: &QueryContext,
        expected: u64,
        values: BTreeMap<String, RuntimeValue>,
        transition: Option<&str>,
        probe: &mut dyn FnMut(CommitPoint) -> Result<(), CellError>,
    ) -> Result<u64, CellError> {
        check_slot_size(&values)?;
        let batch = CommitBatch {
            world: context.world.clone(),
            scope: context.scope.clone(),
            logical_time: context.now_ms,
            writes: vec![FactWriteRow::Put {
                object_type: RESTATEMENT_TYPE.into(),
                key: id.into(),
                values,
            }],
            facts: transition
                .map(|transition| norn_ir::FactChange::Transition {
                    object_type: norn_ir::ObjectTypeName(RESTATEMENT_TYPE.into()),
                    key: id.into(),
                    lifecycle: norn_ir::PropertyId("status".into()),
                    transition: norn_ir::TransitionId(transition.into()),
                })
                .into_iter()
                .collect(),
            ..CommitBatch::default()
        };
        let ((), outcome) = self
            .commit_guarded(
                &batch,
                |reader| {
                    if reader.version(&context.world, RESTATEMENT_TYPE, id)? != expected {
                        return Err(refused(INVALID));
                    }
                    Ok(())
                },
                probe,
            )
            .map_err(|e| match e {
                GuardedError::Guard(e) | GuardedError::Store(e) => e,
            })?;
        outcome
            .receipt()
            .body
            .written
            .iter()
            .rfind(|row| row.object_type == RESTATEMENT_TYPE && row.key == id)
            .map(|row| row.version)
            .ok_or_else(|| refused(INVALID))
    }
    /// OUTPUT-ATOMIC: authenticate one slot incarnation, then select all outputs.
    /// Current authorization is required even when reading a retained generation.
    /// # Errors
    /// No complete generation, corrupt/unbound evidence or revoked population authority.
    pub fn read_restatement(
        &self,
        id: &str,
        context: &QueryContext,
    ) -> Result<RestatementView, CellError> {
        self.conn.transaction(|| {
            self.read_restatement_at_version(
                id,
                self.read().version(&context.world, RESTATEMENT_TYPE, id)?,
                context,
            )
        })
    }
    /// Reproduce a prior complete answer from its immutable receipt-bound slot version.
    /// # Errors
    /// Same integrity and current-authorization refusals as the live read.
    pub fn read_restatement_at_version(
        &self,
        id: &str,
        version: u64,
        context: &QueryContext,
    ) -> Result<RestatementView, CellError> {
        self.check_question_scope(&context.scope)?;
        let values = slot(&self.read(), context, id, version)?;
        let (encoded, digest) = selected(&values)?;
        let generation = decode(encoded, digest)?.ok_or_else(|| refused(INVALID))?;
        if generation
            .outputs
            .values()
            .any(|a| !authorized(context, &a.result.context))
        {
            return Err(refused("core.compute.stale_input"));
        }
        Ok(RestatementView {
            generation: digest.into(),
            previous_generation: generation.previous,
            reason: generation.reason,
            label: "restated",
            stale: text(&values, "status")? != "active",
            outputs: generation.outputs,
            deltas: generation.deltas,
        })
    }
}
