//! VIEW-ROWS-1 / VIEW-ENGINE-1: an admitted transform over one authorized cell snapshot.
//!
//! One transaction reads every scanned input's rows (under the host's per-row
//! access decision), the link edges the plan joins along and the verified
//! ledger head, then runs the exact interpreter. Rows the reader may count
//! but not read are excluded and counted; quarantined rows are excluded with
//! their finding. Either makes the result incomplete. Nothing partial is
//! returned: any bound refuses the whole read.

use std::collections::BTreeMap;

use norn_checker::transform::eval::{self, EvalError, InputRow, TransformInput};
use norn_checker::transform::{MAX_ROWS, Step, TransformPlan};
use norn_ir::RuntimeValue;
use serde::{Deserialize, Serialize};

use super::{Engine, refused};
use crate::reactive::{BudgetedLinksRead, BudgetedRead, DecodeBudget, ReactiveLimits};
use crate::{CellError, CellStore, LedgerHead, ObjectRecord, engine::SqlEngine};

/// What the reader may do with one input row (HOST-READ-2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowAccess {
    /// Population and Values: the row is an input.
    Read,
    /// Population only: the row is excluded and counted.
    Count,
    /// Neither: the row is invisible.
    None,
}

/// One input row an output row derives from (VIEW-LINEAGE-1).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TransformMember {
    /// The input alias.
    pub alias: String,
    /// The object type read.
    pub object_type: String,
    /// The object id.
    pub id: String,
    /// The version read.
    pub version: u64,
}

/// One output row.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TransformRow {
    /// The row's stable id: the content hash of its grain values.
    pub id: String,
    /// The grain values, in grain order.
    pub key: Vec<RuntimeValue>,
    /// Every column value.
    pub values: BTreeMap<String, RuntimeValue>,
    /// The input rows it derives from, ordered.
    pub members: Vec<TransformMember>,
}

/// The result envelope of one transform read; construction grants no authority.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TransformResult {
    /// The view artifact.
    pub transform: String,
    /// Its exact version.
    pub version: String,
    /// Content identity of the admitted plan under the installed definitions.
    pub plan_pin: String,
    /// The installed definitions' pin.
    pub definition_pin: String,
    /// The evaluator.
    pub engine: Engine,
    /// The verified ledger head read.
    pub revision: LedgerHead,
    /// Output columns, in schema order.
    pub columns: Vec<String>,
    /// The grain columns.
    pub grain: Vec<String>,
    /// Output rows, ordered by grain (nulls last).
    pub rows: Vec<TransformRow>,
    /// Rows the reader may count but not read, excluded, by input alias.
    pub withheld: BTreeMap<String, u64>,
    /// Quarantined rows excluded: `alias/id` → durable finding id.
    pub exclusions: BTreeMap<String, String>,
    /// No row was withheld or excluded.
    pub complete: bool,
    /// Content-addressed rows, lineage and coverage.
    pub lineage_pin: String,
}

/// One snapshot's readable rows, withheld counts and quarantine exclusions.
type Inputs = (
    TransformInput,
    BTreeMap<String, u64>,
    BTreeMap<String, String>,
);

/// The exact engine's read bounds: rows across all inputs and decoded bytes.
fn limits() -> ReactiveLimits {
    let rows = u32::try_from(MAX_ROWS).unwrap_or(u32::MAX);
    ReactiveLimits {
        max_records: rows,
        max_canonical_bytes: 256 * 1024 * 1024,
        max_fanout_per_commit: 64,
        max_scan: MAX_ROWS.saturating_mul(2),
        max_candidate_bytes_per_commit: 256 * 1024 * 1024,
    }
}

/// The stable refusal of an interpreter failure.
pub(crate) const fn eval_refusal(error: &EvalError) -> &'static str {
    match error {
        EvalError::BoundExceeded => "core.compute.bound_exceeded",
        EvalError::Input(_) => "core.compute.stale_input",
        EvalError::Expression(_) => "core.compute.expression_failed",
    }
}

impl<E: SqlEngine> CellStore<E> {
    /// VIEW-ROWS-1 / VIEW-ENGINE-1: evaluate an admitted transform over the
    /// current world, reading each input row under `access`.
    ///
    /// The host admits the plan against the installed definitions and decides
    /// the read gate (VIEW-READ-1) before calling this.
    ///
    /// # Errors
    /// Refuses a scope mismatch, an unknown type, any exceeded bound or a
    /// failed evaluation; host failures pass through.
    pub fn evaluate_transform(
        &self,
        plan: &TransformPlan,
        world: &str,
        scope: &str,
        access: impl FnMut(&ObjectRecord) -> Result<RowAccess, CellError>,
    ) -> Result<TransformResult, CellError> {
        self.evaluate_transform_with(plan, (world, scope), access, Engine::Cell, |plan, input| {
            eval::evaluate(plan, input).map_err(|error| refused(eval_refusal(&error)))
        })
    }

    /// One snapshot, one engine, one envelope: every engine evaluates the
    /// same authorized input and returns rows the envelope pins identically.
    pub(crate) fn evaluate_transform_with(
        &self,
        plan: &TransformPlan,
        (world, scope): (&str, &str),
        mut access: impl FnMut(&ObjectRecord) -> Result<RowAccess, CellError>,
        engine: Engine,
        run: impl FnOnce(&TransformPlan, &TransformInput) -> Result<eval::TransformOutput, CellError>,
    ) -> Result<TransformResult, CellError> {
        self.conn.transaction(|| {
            self.check_question_scope(scope)?;
            let revision = self.verified_head()?;
            let (input, withheld, exclusions) =
                self.transform_inputs(plan, world, scope, &mut access)?;
            let output = run(plan, &input)?;
            let definition_pin = self.transform_definition_pin()?;
            let plan_pin = crate::codec::hash_json(&(
                "norn.transform/1",
                &plan.artifact_id,
                &plan.version,
                &definition_pin,
            ))?;
            let types: BTreeMap<&str, &str> = plan
                .inputs
                .iter()
                .map(|(alias, scan)| (alias.as_str(), scan.object_type.as_str()))
                .collect();
            let rows = output
                .rows
                .into_iter()
                .map(|row| {
                    Ok(TransformRow {
                        id: crate::codec::hash_json(&("norn.transform-row/1", &row.key))?,
                        key: row.key,
                        values: row.values,
                        members: row
                            .members
                            .into_iter()
                            .map(|member| TransformMember {
                                object_type: types
                                    .get(member.alias.as_str())
                                    .map_or_else(String::new, |t| (*t).to_owned()),
                                alias: member.alias,
                                id: member.id,
                                version: member.version,
                            })
                            .collect(),
                    })
                })
                .collect::<Result<Vec<_>, CellError>>()?;
            let complete = withheld.is_empty() && exclusions.is_empty();
            let lineage_pin =
                crate::codec::hash_json(&(&plan_pin, &revision, &rows, &withheld, &exclusions))?;
            Ok(TransformResult {
                transform: plan.artifact_id.clone(),
                version: plan.version.clone(),
                plan_pin,
                definition_pin,
                engine,
                revision,
                columns: output.columns,
                grain: output.grain,
                rows,
                withheld,
                exclusions,
                complete,
                lineage_pin,
            })
        })
    }

    /// Read every scanned input under `access` and the link edges the plan
    /// joins along, inside the caller's transaction.
    fn transform_inputs(
        &self,
        plan: &TransformPlan,
        world: &str,
        scope: &str,
        access: &mut impl FnMut(&ObjectRecord) -> Result<RowAccess, CellError>,
    ) -> Result<Inputs, CellError> {
        let reader = self.read();
        let mut budget = DecodeBudget::new(&limits());
        let mut input = TransformInput::default();
        let mut withheld = BTreeMap::new();
        let mut exclusions = BTreeMap::new();
        for step in plan.nodes.values().map(|node| &node.step) {
            let Step::Scan { alias } = step else {
                continue;
            };
            let scan = plan
                .inputs
                .get(alias)
                .ok_or_else(|| refused("core.compute.stale_input"))?;
            self.model()
                .query_contract(&scan.object_type)
                .map_err(|_unknown| refused("core.compute.stale_input"))?;
            let (ids, complete) =
                reader.root_rows_with_budget(world, &scan.object_type, MAX_ROWS, &mut budget)?;
            if !complete {
                return Err(refused("core.compute.bound_exceeded"));
            }
            let mut rows = Vec::new();
            for (id, _) in ids {
                let record =
                    match reader.object_with_budget(world, &scan.object_type, &id, &mut budget)? {
                        BudgetedRead::Found(record) => record,
                        BudgetedRead::Missing => continue,
                        BudgetedRead::Exhausted => {
                            return Err(refused("core.compute.bound_exceeded"));
                        }
                    };
                if record.scope != scope {
                    return Err(refused("core.compute.stale_input"));
                }
                // Visibility precedes data quality (HOST-READ-2).
                match access(&record)? {
                    RowAccess::None => continue,
                    RowAccess::Count => {
                        let count = withheld.entry(alias.clone()).or_insert(0_u64);
                        *count = count.saturating_add(1);
                        continue;
                    }
                    RowAccess::Read => {}
                }
                if let Some(finding) = reader.question_findings(world, &record, None)?.first() {
                    exclusions.insert(format!("{alias}/{}", record.id), finding.id.clone());
                    continue;
                }
                rows.push(InputRow {
                    id: record.id,
                    version: record.version,
                    values: record.values,
                });
            }
            input.rows.insert(alias.clone(), rows);
        }
        for (alias, link) in &plan.links {
            let scan = plan
                .inputs
                .get(alias)
                .ok_or_else(|| refused("core.compute.stale_input"))?;
            let mut edges = BTreeMap::new();
            for row in input.rows.get(alias).into_iter().flatten() {
                match reader.link_targets_with_budget(
                    world,
                    &scan.object_type,
                    link,
                    &row.id,
                    &mut budget,
                )? {
                    BudgetedLinksRead::Complete(targets) => {
                        edges.insert(row.id.clone(), targets);
                    }
                    BudgetedLinksRead::Exhausted => {
                        return Err(refused("core.compute.bound_exceeded"));
                    }
                }
            }
            input.links.insert((alias.clone(), link.clone()), edges);
        }
        Ok((input, withheld, exclusions))
    }

    fn transform_definition_pin(&self) -> Result<String, CellError> {
        let reader = self.read();
        Ok(match reader.active_definitions_version()? {
            Some(version) => {
                reader
                    .installed_definitions(Some(version))?
                    .ok_or_else(|| refused("core.compute.stale_input"))?
                    .receipt_sha256
            }
            None => self.model().model_hash().to_owned(),
        })
    }
}
