//! VIEW-MAT-1: a materialized transform's stored result in its cell.
//!
//! One result per transform is kept, computed over every input row, with the
//! plan pin and a digest of the whole input snapshot it read. It is served
//! only to a reader who may read every input row: hidden rows can change
//! visible ones through a window, a dedupe, a left join or an aggregate, so a
//! stored result cannot be filtered per reader exactly. Served, it is exact
//! when the current snapshot has the same digest, and otherwise labelled stale
//! with the ledger head it was computed at. Any other reader gets a live
//! evaluation and never changes what is stored. The table is a cache of a
//! derived read: no commit reads it and the ledger never depends on it.

use std::collections::BTreeMap;

use norn_checker::transform::eval;
use norn_checker::transform::{Materialization, TransformPlan};
use serde::{Deserialize, Serialize};

use super::transform::{RowAccess, Snapshot, TransformResult, eval_refusal};
use super::{Engine, refused};
use crate::engine::{SqlEngine, params};
use crate::read::{to_i64, to_u64};
use crate::{CellError, CellStore, LedgerHead, ObjectRecord};

pub(crate) const MATERIALIZATION_DDL: &str = r#"CREATE TABLE IF NOT EXISTS "_norn_transform_materialization" (
  "transform" TEXT NOT NULL,
  "plan_pin" TEXT NOT NULL,
  "input_digest" TEXT NOT NULL,
  "refreshed_at_ms" INTEGER NOT NULL,
  "result" TEXT NOT NULL,
  "result_digest" TEXT NOT NULL,
  PRIMARY KEY ("transform")
)"#;

/// Create the materialization table in the store's initialization transaction.
pub(crate) fn init(engine: &impl SqlEngine) -> Result<(), CellError> {
    engine.execute_batch(MATERIALIZATION_DDL)
}

/// What a served read says about the stored result it came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MaterializationState {
    /// The verified ledger head the stored result was computed at.
    pub frontier: LedgerHead,
    /// When it was computed, in milliseconds since the Unix epoch.
    pub refreshed_at_ms: u64,
    /// The inputs changed since: the rows are the frontier's, not the
    /// current snapshot's.
    pub stale: bool,
}

/// One transform read under VIEW-MAT-1.
#[derive(Debug, Clone, PartialEq)]
pub struct Served {
    /// The rows served.
    pub result: TransformResult,
    /// The stored result they came from, or `None` for a live evaluation.
    pub materialization: Option<MaterializationState>,
}

/// One stored result, verified against its digest.
struct Stored {
    plan_pin: String,
    input_digest: String,
    refreshed_at_ms: u64,
    result: TransformResult,
}

/// The identity of a whole input snapshot: every scanned row's id and
/// version, every joined link edge and every quarantine exclusion.
fn input_digest(snapshot: &Snapshot) -> Result<String, CellError> {
    let rows: BTreeMap<&str, Vec<(&str, u64)>> = snapshot
        .input
        .rows
        .iter()
        .map(|(alias, rows)| {
            (
                alias.as_str(),
                rows.iter()
                    .map(|row| (row.id.as_str(), row.version))
                    .collect(),
            )
        })
        .collect();
    let links: Vec<(&str, &str, &BTreeMap<String, Vec<String>>)> = snapshot
        .input
        .links
        .iter()
        .map(|((alias, link), edges)| (alias.as_str(), link.as_str(), edges))
        .collect();
    crate::codec::hash_json(&(
        "norn.transform-input/1",
        &rows,
        &links,
        &snapshot.exclusions,
    ))
}

impl<E: SqlEngine> CellStore<E> {
    /// VIEW-MAT-1: read a transform, serving its stored result when the
    /// reader may read every input row and its refresh policy allows.
    ///
    /// An `on_read` transform whose stored result is missing, of another
    /// plan, or changed and older than its `maxAgeMs` is refreshed by such a
    /// reader's read. A transform that is not materialized is evaluated live.
    ///
    /// # Errors
    /// As [`CellStore::evaluate_transform`]; a stored result that does not
    /// verify is ledger corruption.
    pub fn serve_transform(
        &self,
        plan: &TransformPlan,
        (world, scope): (&str, &str),
        mut access: impl FnMut(&ObjectRecord) -> Result<RowAccess, CellError>,
        now_ms: u64,
    ) -> Result<Served, CellError> {
        self.conn.transaction(|| {
            let snapshot = self.transform_snapshot(plan, (world, scope), &mut access)?;
            let Some(policy) = plan.materialization else {
                return Ok(Served {
                    result: self.evaluate_snapshot(plan, snapshot)?,
                    materialization: None,
                });
            };
            let digest = input_digest(&snapshot)?;
            let (plan_pin, _) = self.transform_pins(plan)?;
            if snapshot.whole
                && let Some(stored) = self
                    .stored_materialization(&plan.artifact_id)?
                    .filter(|stored| stored.plan_pin == plan_pin)
            {
                let exact = stored.input_digest == digest;
                let within_age = match policy {
                    Materialization::Manual => true,
                    Materialization::OnRead {
                        max_age_ms: Some(age),
                    } => now_ms.saturating_sub(stored.refreshed_at_ms) <= age,
                    Materialization::OnRead { max_age_ms: None } => false,
                };
                if exact || within_age {
                    return Ok(Served {
                        materialization: Some(MaterializationState {
                            frontier: stored.result.revision.clone(),
                            refreshed_at_ms: stored.refreshed_at_ms,
                            stale: !exact,
                        }),
                        result: stored.result,
                    });
                }
            }
            let whole = snapshot.whole;
            let result = self.evaluate_snapshot(plan, snapshot)?;
            if whole && matches!(policy, Materialization::OnRead { .. }) {
                self.store_materialization(plan, &plan_pin, &digest, now_ms, &result)?;
                return Ok(Served {
                    materialization: Some(MaterializationState {
                        frontier: result.revision.clone(),
                        refreshed_at_ms: now_ms,
                        stale: false,
                    }),
                    result,
                });
            }
            Ok(Served {
                result,
                materialization: None,
            })
        })
    }

    /// VIEW-MAT-1: recompute and store a materialized transform's result.
    /// `None` when the reader may not read every input row: such a reader
    /// never changes what is stored.
    ///
    /// # Errors
    /// As [`CellStore::evaluate_transform`]; refuses a plan that is not
    /// materialized.
    pub fn refresh_transform(
        &self,
        plan: &TransformPlan,
        (world, scope): (&str, &str),
        mut access: impl FnMut(&ObjectRecord) -> Result<RowAccess, CellError>,
        now_ms: u64,
    ) -> Result<Option<MaterializationState>, CellError> {
        if plan.materialization.is_none() {
            return Err(refused("core.compute.stale_input"));
        }
        self.conn.transaction(|| {
            let snapshot = self.transform_snapshot(plan, (world, scope), &mut access)?;
            if !snapshot.whole {
                return Ok(None);
            }
            let digest = input_digest(&snapshot)?;
            let (plan_pin, _) = self.transform_pins(plan)?;
            let result = self.evaluate_snapshot(plan, snapshot)?;
            self.store_materialization(plan, &plan_pin, &digest, now_ms, &result)?;
            Ok(Some(MaterializationState {
                frontier: result.revision,
                refreshed_at_ms: now_ms,
                stale: false,
            }))
        })
    }

    fn evaluate_snapshot(
        &self,
        plan: &TransformPlan,
        snapshot: Snapshot,
    ) -> Result<TransformResult, CellError> {
        let output =
            eval::evaluate(plan, &snapshot.input).map_err(|error| refused(eval_refusal(&error)))?;
        self.transform_envelope(plan, snapshot, output, Engine::Cell)
    }

    fn store_materialization(
        &self,
        plan: &TransformPlan,
        plan_pin: &str,
        input_digest: &str,
        now_ms: u64,
        result: &TransformResult,
    ) -> Result<(), CellError> {
        let text = crate::codec::canonical_json(result)?;
        let result_digest = crate::codec::hash_json(result)?;
        self.conn.execute(
            r#"INSERT OR REPLACE INTO "_norn_transform_materialization"
               ("transform", "plan_pin", "input_digest", "refreshed_at_ms", "result", "result_digest")
               VALUES (?1, ?2, ?3, ?4, ?5, ?6)"#,
            params![
                plan.artifact_id.as_str(),
                plan_pin,
                input_digest,
                to_i64(now_ms, "refreshed_at_ms")?,
                text,
                result_digest,
            ],
        )?;
        Ok(())
    }

    fn stored_materialization(&self, transform: &str) -> Result<Option<Stored>, CellError> {
        let row: Option<(String, String, i64, String, String)> = self.conn.query_optional(
            r#"SELECT "plan_pin", "input_digest", "refreshed_at_ms", "result", "result_digest"
               FROM "_norn_transform_materialization" WHERE "transform" = ?1"#,
            params![transform],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )?;
        let Some((plan_pin, input_digest, refreshed_at_ms, text, result_digest)) = row else {
            return Ok(None);
        };
        let corrupt = || CellError::CorruptValue {
            table: "_norn_transform_materialization".to_owned(),
            column: "result".to_owned(),
        };
        let result: TransformResult = serde_json::from_str(&text).map_err(|_error| corrupt())?;
        if crate::codec::hash_json(&result)? != result_digest || result.transform != transform {
            return Err(corrupt());
        }
        Ok(Some(Stored {
            plan_pin,
            input_digest,
            refreshed_at_ms: to_u64(refreshed_at_ms, "refreshed_at_ms")?,
            result,
        }))
    }
}
