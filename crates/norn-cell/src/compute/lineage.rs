//! LINEAGE-SUM-1: native contribution drill-through, pinned to one exact result.
use super::{Freshness, QueryContext, QueryResult, Totals, decimal, refused};
use crate::{CellError, CellStore, ObjectRecord, engine::SqlEngine, reactive::BudgetedRead};
use norn_checker::metric::compute::QuestionPlan;
use norn_ir::{Decimal, FactRef, RuntimeObject};
use serde::Serialize;

/// One numeric contribution and its canonical fact incarnation.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NumericContribution {
    /// Stable versioned fact identity; never a catalog-enrichment edge.
    pub fact: FactRef,
    /// Sum operand, or numerator then denominator; no ratio attribution is invented.
    pub amounts: Vec<Option<Decimal>>,
    /// Emitting receipt identity; a reference, not an independently verifiable proof.
    pub receipt_hash: String,
    /// Cell commit position of the writer.
    pub commit_seq: u64,
    /// Knowledge time authenticated by the emitting receipt.
    pub recorded_at_ms: u64,
    /// The host withheld canonical property content, without hiding numeric membership.
    pub restricted: bool,
    /// All committed canonical properties, only when the host permits their disclosure.
    pub values: Option<RuntimeObject>,
}

/// Bounded native evidence for one result bucket, with its original frontier.
/// This is data, not an authorization or export capability.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NumericTrace {
    /// Source resolution stays closed until Core 07 supplies its authority-checked edge.
    pub source_edge_gap: &'static str,
    /// Revalidated exact cell result and definition/time/scope/frontier pins.
    pub result: QueryResult,
    /// Canonical selected grouping key.
    pub bucket: String,
    /// Deterministic identity order; restricted values stay visibly absent.
    pub contributions: Vec<NumericContribution>,
}

impl<E: SqlEngine> CellStore<E> {
    /// LINEAGE-SUM-1 / DATA-LINEAGE: resolve exact numeric membership in one transaction.
    /// The host supplies current population visibility and permission to reveal *all*
    /// canonical properties of each member. Otherwise values are visibly restricted.
    /// Source documents and catalog enrichment require their own authorized resolution.
    /// A changed result pin requires a fresh result, never a mixture of generations.
    /// # Errors
    /// Refuses nonexact/incomplete results, changed pins, missing evidence or budgets.
    pub fn trace_question(
        &self,
        plan: &QuestionPlan,
        context: QueryContext,
        expected_lineage: &str,
        bucket: &str,
        mut visible: impl FnMut(&ObjectRecord) -> Result<bool, CellError>,
        mut reveal_values: impl FnMut(&ObjectRecord) -> Result<bool, CellError>,
    ) -> Result<NumericTrace, CellError> {
        let incomplete = || refused("core.projection.lineage_incomplete");
        self.question_snapshot_with(plan, context, &mut visible, |snapshot, budget| {
            let result = snapshot.result;
            if !result.complete || result.freshness != Freshness::Exact {
                return Err(refused("core.compute.stale_input"));
            }
            if result.lineage_ref != expected_lineage {
                return Err(incomplete());
            }
            let row = result.rows.get(bucket).ok_or_else(incomplete)?;
            let mut totals = Totals::default();
            let mut contributions = Vec::with_capacity(row.members.len());
            let reader = self.read();
            for (id, member) in &row.members {
                let (_, value) = snapshot.contributions.get(id).ok_or_else(incomplete)?;
                totals.adjust(value, false)?;
                let BudgetedRead::Found(record) = reader.object_at_version_with_budget(
                    &result.context.world,
                    result.grain.as_str(),
                    id,
                    member.version,
                    budget,
                )?
                else {
                    return Err(incomplete());
                };
                let receipt = reader
                    .receipt_for_fact_version_with_budget(
                        &result.context.world,
                        &record.object_type,
                        id,
                        member.version,
                        budget,
                    )?
                    .ok_or_else(incomplete)?;
                if record.scope != result.context.scope
                    || receipt.body.commit_seq > result.revision.commits
                {
                    return Err(incomplete());
                }
                let allowed = reveal_values(&record)?;
                contributions.push(NumericContribution {
                    fact: FactRef::new(&record.object_type, id, member.version),
                    amounts: member.amounts.clone(),
                    receipt_hash: receipt.hash,
                    commit_seq: receipt.body.commit_seq,
                    recorded_at_ms: receipt.body.logical_time,
                    restricted: !allowed,
                    values: allowed.then_some(record.values),
                });
            }
            let sums = totals
                .0
                .get(bucket)
                .ok_or_else(incomplete)?
                .iter()
                .map(|n| decimal(*n))
                .collect::<Result<Vec<_>, _>>()?;
            if sums != row.totals {
                return Err(incomplete());
            }
            Ok(NumericTrace {
                source_edge_gap: "source edge: not yet (#401)",
                result,
                bucket: bucket.to_owned(),
                contributions,
            })
        })
    }
}
