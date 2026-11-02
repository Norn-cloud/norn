//! ASOF-KNOWLEDGE-1: historical questions use the same exact contribution engine.
use super::{QueryContext, QueryResult, refused};
use crate::engine::{SqlEngine, SqlRow, params};
use crate::reactive::{BudgetedRead, DecodeBudget};
use crate::{CellError, CellStore, HistoricalEvidence, KnowledgeCutoff, ObjectRecord, StoreReader};
use norn_checker::metric::compute::{QuestionPlan, UNSUPPORTED_ENGINE};
use serde::Serialize;

/// Read-only historical world with both the effective window and knowledge clock.
#[derive(Debug, Clone, Serialize)]
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
pub(super) fn historical_record<E: SqlEngine>(
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
pub(super) fn frame<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    cutoff: KnowledgeCutoff,
    now: u64,
    budget: &mut DecodeBudget,
) -> Result<(crate::LedgerHead, Option<String>), CellError> {
    if now != cutoff.recorded_at.as_millis() {
        return Err(refused("core.compute.stale_input"));
    }
    let revision = reader
        .historical_revision_with_budget(cutoff, budget)?
        .ok_or_else(|| refused("core.compute.bound_exceeded"))?;
    let (version, pin) = if cutoff.frontier == 0 {
        (0, None)
    } else {
        let receipt = reader
            .receipt_at_with_budget(cutoff.frontier, budget)?
            .ok_or_else(|| refused("core.compute.bound_exceeded"))?;
        (receipt.body.definitions_version, Some(receipt.hash))
    };
    if reader.active_definitions_version()?.unwrap_or(0) != version {
        return Err(refused(UNSUPPORTED_ENGINE));
    }
    Ok((revision, pin))
}
