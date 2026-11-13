//! ASOF-KNOWLEDGE-1: historical questions use the same exact contribution engine.
use super::{QueryContext, QueryResult, refused};
use crate::engine::{SqlEngine, SqlRow, params};
use crate::reactive::{BudgetedRead, DecodeBudget};
use crate::{CellError, CellStore, HistoricalEvidence, KnowledgeCutoff, ObjectRecord, StoreReader};
use norn_checker::metric::compute::{QuestionPlan, UNSUPPORTED_ENGINE};
use serde::{Deserialize, Serialize};

/// Read-only historical world with both the effective window and knowledge clock.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoricalQuestionResult {
    /// Historical commit authority; source world/scope remain in the result context.
    pub world: norn_kernel::World,
    /// Inclusive receipt clock, alongside `result.known_as_of`'s immutable frontier.
    pub knowledge_time_ms: u64,
    /// Existing exact query result, including effective cutoff and versioned members.
    pub result: QueryResult,
}
impl<E: SqlEngine> CellStore<E> {
    /// ASOF-KNOWLEDGE-1: evaluate a pinned history slice using the admitted query plan.
    /// Source freshness is evaluated at the historical clock; authorization is current.
    /// # Errors
    /// Changed interpretation, future/unproven frontier, scope/visibility or budget refusal.
    pub fn evaluate_question_known_at(
        &self,
        plan: &QuestionPlan,
        context: QueryContext,
        cutoff: KnowledgeCutoff,
        mut visible: impl FnMut(&ObjectRecord) -> Result<bool, CellError>,
    ) -> Result<HistoricalQuestionResult, CellError> {
        let snapshot = self.question_snapshot_at(plan, context, Some(cutoff), &mut visible)?;
        Ok(HistoricalQuestionResult {
            world: norn_kernel::World::Historical {
                commit: norn_kernel::CommitId(
                    snapshot
                        .historical_receipt
                        .unwrap_or_else(|| crate::system::GENESIS_HASH.into()),
                ),
            },
            knowledge_time_ms: cutoff.recorded_at.as_millis(),
            result: snapshot.result,
        })
    }
}
pub(super) fn slice_pin(base: &str, cutoff: KnowledgeCutoff) -> Result<String, CellError> {
    crate::codec::hash_json(&(
        base,
        cutoff.recorded_at.as_millis(),
        cutoff.frontier,
        "norn.query.asof/1",
    ))
}
pub(crate) fn historical_record<E: SqlEngine>(
    history: &mut crate::history::HistoryRead<'_, '_, E>,
    world: &str,
    kind: &str,
    id: &str,
    cutoff: KnowledgeCutoff,
    model: &str,
    budget: &mut DecodeBudget,
) -> Result<BudgetedRead, CellError> {
    match history.known_at(world, kind, id, cutoff, budget)? {
        HistoricalEvidence::Missing => Ok(BudgetedRead::Missing),
        HistoricalEvidence::Exhausted => Ok(BudgetedRead::Exhausted),
        HistoricalEvidence::Found(proof) => {
            let snapshot = proof.snapshot();
            if snapshot.model_hash != model {
                return Err(refused(UNSUPPORTED_ENGINE));
            }
            Ok(snapshot
                .values
                .clone()
                .map_or(BudgetedRead::Missing, |values| {
                    BudgetedRead::Found(ObjectRecord {
                        object_type: snapshot.object_type.clone(),
                        id: snapshot.key.clone(),
                        scope: snapshot.scope.clone(),
                        version: snapshot.version,
                        values,
                    })
                }))
        }
    }
}
pub(super) fn history_ids<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    world: &str,
    kind: &str,
    frontier: u64,
    max: usize,
    budget: &mut DecodeBudget,
) -> Result<(Vec<String>, bool), CellError> {
    let root = reader.root_type(kind)?;
    let cap = budget.remaining_rows();
    if cap == 0 || budget.remaining_bytes() < 64 {
        return Ok((vec![], false));
    }
    let limit = i64::try_from(cap.saturating_add(1))
        .map_err(|_n| refused("core.compute.bound_exceeded"))?;
    let head = i64::try_from(frontier).map_err(|_n| refused("core.compute.bound_exceeded"))?;
    let suffix = "FROM _norn_fact_values INDEXED BY \"_norn_fact_values/root_frontier\" WHERE world=?1 AND root_type=?2 AND commit_seq<=?3 ORDER BY commit_seq,id,version LIMIT ?4";
    let bindings = params![world, root, head, limit];
    let lengths = reader
        .conn
        .query(&format!("SELECT octet_length(id)+64 {suffix}"), bindings)?;
    if lengths.len() > cap {
        return Ok((vec![], false));
    }
    for row in lengths {
        let bytes: i64 = SqlRow(row).get(0)?;
        if !budget.charge_rows(1)
            || !budget
                .charge(u64::try_from(bytes).map_err(|_n| refused("core.compute.bound_exceeded"))?)
        {
            return Ok((vec![], false));
        }
    }
    let mut ids = std::collections::BTreeSet::new();
    for row in reader
        .conn
        .query(&format!("SELECT id {suffix}"), bindings)?
    {
        ids.insert(SqlRow(row).get::<String>(0)?);
    }
    if ids.len() > max {
        return Ok((vec![], false));
    }
    Ok((ids.into_iter().collect(), true))
}

pub(super) fn query_pin(
    model: &str,
    plan: &QuestionPlan,
    definition: &str,
    cutoff: Option<KnowledgeCutoff>,
) -> Result<String, CellError> {
    let base = crate::codec::hash_json(&(
        definition,
        model,
        plan.question(),
        plan.definitions(),
        plan.max_items(),
        "norn.query/1",
    ))?;
    match cutoff {
        Some(cutoff) => slice_pin(&base, cutoff),
        None => Ok(base),
    }
}
pub(crate) fn frame<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    cutoff: KnowledgeCutoff,
    now: u64,
    budget: &mut DecodeBudget,
) -> Result<(crate::LedgerHead, Option<crate::Receipt>), CellError> {
    if now != cutoff.recorded_at.as_millis() {
        return Err(refused("core.compute.stale_input"));
    }
    let frame = reader
        .historical_frame_with_budget(cutoff, budget)?
        .ok_or_else(|| refused("core.compute.bound_exceeded"))?;
    let version = frame
        .receipt
        .as_ref()
        .map_or(0, |r| r.body.definitions_version);
    // The kernel identity in force, not the version number: a retained
    // (dashboard, agent, …) revision commits a new definitions version over
    // the same kernel set and receipt, which does not change what a
    // historical cutoff evaluates.
    let identity = |version: u64| -> Result<Option<String>, CellError> {
        if version == 0 {
            return Ok(None);
        }
        Ok(reader
            .installed_definitions(Some(version))?
            .map(|installed| installed.receipt_sha256))
    };
    let active = reader.active_definitions_version()?.unwrap_or(0);
    if active != version && identity(active)? != identity(version)? {
        return Err(refused(UNSUPPORTED_ENGINE));
    }
    Ok((frame.revision, frame.receipt))
}

#[cfg(all(test, feature = "native"))]
mod tests {
    use super::*;
    #[test]
    fn frontier_receipt_and_fact_share_one_bounded_authentication() {
        let definition:norn_ir::ObjectTypeDef=serde_json::from_value(serde_json::json!({"name":"budget.Item","properties":[{"id":"value","type":{"kind":"text"}}]})).expect("typed fixture");
        let model = crate::Model::from_definitions(&[definition], &[]).expect("model");
        let mut store = CellStore::open_in_memory(model).expect("store");
        store
            .commit(&crate::CommitBatch {
                world: crate::MAIN_WORLD.into(),
                scope: "synthetic".into(),
                logical_time: 7,
                writes: vec![crate::FactWriteRow::Put {
                    object_type: "budget.Item".into(),
                    key: "row".into(),
                    values: std::collections::BTreeMap::from([(
                        "value".into(),
                        norn_ir::RuntimeValue::from("seed"),
                    )]),
                }],
                ..crate::CommitBatch::default()
            })
            .expect("source");
        let reader = store.read();
        let cutoff = KnowledgeCutoff {
            recorded_at: norn_kernel::LogicalTime::from_millis(7),
            frontier: 1,
        };
        let lookup = |budget: &mut DecodeBudget| {
            let (_, receipt) = frame(&reader, cutoff, 7, budget).expect("frontier");
            reader
                .history_read_seeded(receipt)
                .known_at(crate::MAIN_WORLD, "budget.Item", "row", cutoff, budget)
                .expect("lookup")
        };
        let mut limits = crate::reactive::ReactiveLimits::from_profile(
            &norn_ir::ResourceLimitProfile::norn_limits_v1(),
        );
        let mut measured = DecodeBudget::new(&limits);
        assert!(matches!(
            lookup(&mut measured),
            crate::HistoricalEvidence::Found(_)
        ));
        limits.max_candidate_bytes_per_commit = limits
            .max_candidate_bytes_per_commit
            .checked_sub(measured.remaining_bytes())
            .expect("meter");
        assert!(matches!(
            lookup(&mut DecodeBudget::new(&limits)),
            crate::HistoricalEvidence::Found(_)
        ));
        limits.max_candidate_bytes_per_commit = limits
            .max_candidate_bytes_per_commit
            .checked_sub(1)
            .expect("positive budget");
        assert!(matches!(
            lookup(&mut DecodeBudget::new(&limits)),
            crate::HistoricalEvidence::Exhausted
        ));
    }
}
