//! ASOF-KNOWLEDGE-1: cutoff-bound reader for historical question evaluation.
//!
//! [`AsOfReader`] is constructed only from an authenticated [`KnowledgeCutoff`]
//! and a seeded [`crate::history::HistoryRead`] under the caller's single store
//! transaction. It intentionally does not implement `Deref<Target = StoreReader>`
//! or expose its underlying reader, so historical evaluation helpers cannot
//! accidentally reach live-world state, live links, or `CurrentLinks`.

use std::collections::{BTreeMap, BTreeSet};

use norn_checker::metric::compute::{
    QuestionPlan, UNSUPPORTED_ENGINE,
    contribution::{Contribution, as_of, project},
};
use norn_ir::{DataQualityFinding, RuntimeValue};

use super::{Contributions, QueryContext, historical, refused};
use crate::engine::SqlEngine;
use crate::reactive::{BudgetedRead, DecodeBudget};
use crate::{CellError, KnowledgeCutoff, LedgerHead, ObjectRecord, StoreReader};

/// Versioned snapshots of one subject committed on or before a cutoff.
pub(crate) type HistoricalVersions = Vec<(u64, BTreeMap<String, RuntimeValue>)>;

/// Collected membership, exclusions, and contributions for one snapshot scan.
#[derive(Default)]
pub(crate) struct CollectedMembers {
    pub(crate) contributions: Contributions,
    pub(crate) exclusions: BTreeMap<String, String>,
    pub(crate) members: BTreeSet<String>,
    pub(crate) finding_members: BTreeSet<String>,
    pub(crate) unlinked: u64,
    pub(crate) unlinked_members: BTreeSet<String>,
}

/// Cutoff-bound historical reader scoped to one authenticated frontier.
pub(crate) struct AsOfReader<'r, 'c, E: SqlEngine> {
    reader: &'r StoreReader<'c, E>,
    history: crate::history::HistoryRead<'r, 'c, E>,
    cutoff: KnowledgeCutoff,
    revision: LedgerHead,
    receipt_hash: Option<String>,
    model_hash: String,
}

impl<'r, 'c, E: SqlEngine> AsOfReader<'r, 'c, E> {
    /// Authenticate `cutoff` at `now_ms` and seed one bounded historical reader
    /// under the caller's transaction snapshot.
    pub(crate) fn open(
        reader: &'r StoreReader<'c, E>,
        cutoff: KnowledgeCutoff,
        now_ms: u64,
        budget: &mut DecodeBudget,
    ) -> Result<Self, CellError> {
        let (revision, frontier_receipt) = historical::frame(reader, cutoff, now_ms, budget)?;
        let receipt_hash = frontier_receipt.as_ref().map(|r| r.hash.clone());
        let history = reader.history_read_seeded(frontier_receipt);
        let model_hash = reader.model().model_hash().to_owned();
        Ok(Self {
            reader,
            history,
            cutoff,
            revision,
            receipt_hash,
            model_hash,
        })
    }

    /// The authenticated ledger head at this reader's knowledge frontier.
    pub(crate) const fn revision(&self) -> &LedgerHead {
        &self.revision
    }

    /// The authenticated frontier receipt hash, if any commit exists by cutoff.
    pub(crate) fn receipt_hash(&self) -> Option<&str> {
        self.receipt_hash.as_deref()
    }

    /// Bounded root-type population known on or before the cutoff frontier.
    pub(crate) fn population_ids(
        &self,
        world: &str,
        kind: &str,
        max: usize,
        budget: &mut DecodeBudget,
    ) -> Result<(Vec<String>, bool), CellError> {
        historical::history_ids(self.reader, world, kind, self.cutoff.frontier, max, budget)
    }

    /// Distinct subject IDs of `kind` committed on or before the cutoff
    /// frontier, read strictly from `version = 1` entries in the covering index
    /// `_norn_fact_values/root_frontier` (`commit_seq <= head`). Every subject
    /// in `_norn_fact_values` starts at `version = 1`, so each pre-cutoff ID
    /// appears at most once with zero post-cutoff rows scanned, zero temporary
    /// B-tree, and zero bound exhaustion from repeated report versions.
    pub(crate) fn distinct_ids(
        &self,
        world: &str,
        kind: &str,
        max: usize,
        budget: &mut DecodeBudget,
    ) -> Result<(Vec<String>, bool), CellError> {
        use crate::engine::{SqlRow, params};
        self.reader.check_history("historical distinct ids")?;
        let root = self.reader.root_type(kind)?;
        let cap = max.min(budget.remaining_rows());
        if cap == 0 || budget.remaining_bytes() < 64 {
            return Ok((Vec::new(), false));
        }
        let head = i64::try_from(self.cutoff.frontier)
            .map_err(|_n| refused("core.compute.bound_exceeded"))?;
        let limit = i64::try_from(cap.saturating_add(1))
            .map_err(|_n| refused("core.compute.bound_exceeded"))?;
        let rows = self.reader.conn.query(
            "SELECT id FROM _norn_fact_values INDEXED BY \"_norn_fact_values/root_frontier\" \
             WHERE world=?1 AND root_type=?2 AND commit_seq<=?3 AND version=1 \
             ORDER BY commit_seq, id LIMIT ?4",
            params![world, root, head, limit],
        )?;
        if rows.len() > cap {
            return Ok((Vec::new(), false));
        }
        let mut ids = Vec::with_capacity(rows.len());
        for row in rows {
            let id: String = SqlRow(row).get(0)?;
            ids.push(id);
        }
        ids.sort();
        charge_distinct_ids(ids, budget)
    }

    /// Subjects deleted under another model by the cutoff frontier.
    pub(crate) fn foreign_model_deletions(
        &self,
        world: &str,
        kind: &str,
        budget: &DecodeBudget,
    ) -> Result<BTreeSet<String>, CellError> {
        historical::foreign_model_deletions(
            self.reader,
            world,
            kind,
            self.cutoff.frontier,
            &self.model_hash,
            budget,
        )
    }

    /// Authenticated incarnation of `(world, kind, id)` known at the cutoff.
    pub(crate) fn record(
        &mut self,
        world: &str,
        kind: &str,
        id: &str,
        budget: &mut DecodeBudget,
    ) -> Result<BudgetedRead, CellError> {
        historical::historical_record(
            &mut self.history,
            world,
            kind,
            id,
            self.cutoff,
            &self.model_hash,
            budget,
        )
    }

    /// Stored incarnations of `(world, kind, id)` committed by the cutoff frontier.
    pub(crate) fn versions(
        &self,
        world: &str,
        kind: &str,
        id: &str,
        budget: &mut DecodeBudget,
    ) -> Result<Option<HistoricalVersions>, CellError> {
        let Some(versions) = self.reader.object_versions_with_budget(
            world,
            kind,
            id,
            Some(self.cutoff.frontier),
            budget,
        )?
        else {
            return Ok(None);
        };
        Ok(Some(
            versions
                .into_iter()
                .map(|version| (version.version, version.values))
                .collect(),
        ))
    }

    /// Open quarantine findings for `record` at this cutoff, failing closed on
    /// linked governing-law recovery without cutoff-indexed link history.
    pub(crate) fn findings(
        &self,
        world: &str,
        record: &ObjectRecord,
    ) -> Result<Vec<DataQualityFinding>, CellError> {
        self.reader
            .question_findings_at_cutoff(world, record, self.cutoff.frontier)
    }

    /// Scope, visibility, and cutoff-bound quarantine gating for one historical row.
    pub(crate) fn include_record(
        &self,
        context: &QueryContext,
        record: &ObjectRecord,
        exclusions: &mut BTreeMap<String, String>,
        finding_members: &mut BTreeSet<String>,
        visible: &mut impl FnMut(&ObjectRecord) -> Result<bool, CellError>,
    ) -> Result<bool, CellError> {
        if record.scope != context.scope {
            return Err(refused("core.compute.stale_input"));
        }
        if !visible(record)? {
            return Ok(false);
        }
        finding_members.insert(record.id.clone());
        if let Some(finding) = self.findings(&context.world, record)?.first() {
            exclusions.insert(record.id.clone(), finding.id.clone());
            return Ok(false);
        }
        Ok(true)
    }

    /// Historical contributions for one subject at this cutoff.
    ///
    /// Linked-time questions (`plan.time_link()`) fail closed immediately because
    /// cutoff-indexed link reconstruction is not yet qualified.
    pub(crate) fn subject_contributions(
        &self,
        plan: &QuestionPlan,
        context: &QueryContext,
        (id, record): (&str, &ObjectRecord),
        budget: &mut DecodeBudget,
    ) -> Result<(Vec<(u64, Contribution)>, bool), CellError> {
        if plan.time_link().is_some() {
            return Err(refused(UNSUPPORTED_ENGINE));
        }
        if !plan.as_of() {
            return Ok((
                project(plan, &record.values)
                    .map_err(refused)?
                    .map_or_default(|value| vec![(record.version, value)]),
                false,
            ));
        }
        let versions = self
            .versions(&context.world, plan.grain().as_str(), id, budget)?
            .ok_or_else(|| refused("core.compute.bound_exceeded"))?;
        Ok((as_of(plan, &versions).map_err(refused)?, false))
    }

    /// Evaluate every subject known at this cutoff into `CollectedMembers`.
    pub(crate) fn collect_members(
        &mut self,
        plan: &QuestionPlan,
        context: &QueryContext,
        max: usize,
        visible: &mut impl FnMut(&ObjectRecord) -> Result<bool, CellError>,
        budget: &mut DecodeBudget,
    ) -> Result<CollectedMembers, CellError> {
        let (ids, complete) =
            self.population_ids(&context.world, plan.grain().as_str(), max, budget)?;
        if !complete {
            return Err(refused("core.compute.bound_exceeded"));
        }
        let mut collected = CollectedMembers::default();
        for id in ids {
            let record = match self.record(&context.world, plan.grain().as_str(), &id, budget)? {
                BudgetedRead::Found(record) => record,
                BudgetedRead::Missing => continue,
                BudgetedRead::Exhausted => return Err(refused("core.compute.bound_exceeded")),
            };
            collected.members.insert(id.clone());
            if !self.include_record(
                context,
                &record,
                &mut collected.exclusions,
                &mut collected.finding_members,
                visible,
            )? {
                continue;
            }
            let (entries, timeless) =
                self.subject_contributions(plan, context, (&id, &record), budget)?;
            collected.unlinked = collected.unlinked.saturating_add(u64::from(timeless));
            if timeless {
                collected.unlinked_members.insert(id.clone());
            }
            if !entries.is_empty() {
                collected.contributions.insert(id, entries);
            }
        }
        Ok(collected)
    }
}

fn charge_distinct_ids(
    ids: Vec<String>,
    budget: &mut DecodeBudget,
) -> Result<(Vec<String>, bool), CellError> {
    for id in &ids {
        let bytes = u64::try_from(id.len().saturating_add(64))
            .map_err(|_n| refused("core.compute.bound_exceeded"))?;
        if !budget.charge_rows(1) || !budget.charge(bytes) {
            return Ok((Vec::new(), false));
        }
    }
    Ok((ids, true))
}
