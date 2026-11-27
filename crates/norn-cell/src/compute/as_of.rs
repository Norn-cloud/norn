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
use crate::{CellError, KnowledgeCutoff, LedgerHead, MAIN_WORLD, ObjectRecord, StoreReader};

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
    /// frontier, read strictly from the partial covering index
    /// `_norn_fact_values/root_first_version` (`WHERE version = 1`). Every
    /// subject starts at `version = 1`, so scanning is strictly bounded by
    /// `cap + 1` distinct pre-cutoff IDs with zero post-cutoff rows, zero
    /// later-version rows, and zero temporary B-tree.
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
            "SELECT id FROM _norn_fact_values INDEXED BY \"_norn_fact_values/root_first_version\" \
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

fn definition_version_matches_mapping<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    def_ver: u64,
    mapping_id: &str,
    expected_binding: &str,
) -> Result<bool, CellError> {
    let Some(installed) = reader.installed_definitions(Some(def_ver))? else {
        return Ok(false);
    };
    let artifacts: Vec<norn_ir::ArtifactEnvelope> = serde_json::from_str(&installed.canonical_set)?;
    for artifact in &artifacts {
        if artifact.kind() == norn_ir::ArtifactKind::Mapping && artifact.artifact_id == mapping_id {
            return Ok(super::freshness::mapping_binding_key(artifact)? == expected_binding);
        }
    }
    Ok(false)
}

fn commit_seq_matches_mapping_binding<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    commit_seq: u64,
    mapping_id: &str,
    expected_binding: &str,
) -> Result<bool, CellError> {
    let Some(def_ver) = reader.definitions_version_at(commit_seq)? else {
        return Ok(false);
    };
    definition_version_matches_mapping(reader, def_ver, mapping_id, expected_binding)
}

/// Earliest `commit_seq` of the contiguous tail of `_norn_definition_version`
/// rows up to `conn_seq` where `mapping_id` had `expected_binding`.
fn mapping_binding_valid_since<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    conn_seq: u64,
    mapping_id: &str,
    expected_binding: &str,
) -> Result<Option<u64>, CellError> {
    use crate::engine::{SqlRow, params};
    let max_seq = i64::try_from(conn_seq).map_err(|_| CellError::OutOfRange("commit_seq"))?;
    let defs = reader.conn.query(
        "SELECT version, commit_seq FROM _norn_definition_version \
         WHERE commit_seq <= ?1 ORDER BY commit_seq DESC",
        params![max_seq],
    )?;
    let mut valid_since: Option<u64> = None;
    for values in defs {
        let row = SqlRow(values);
        let (Ok(def_ver), Ok(seq)) = (
            u64::try_from(row.get::<i64>(0)?),
            u64::try_from(row.get::<i64>(1)?),
        ) else {
            break;
        };
        if definition_version_matches_mapping(reader, def_ver, mapping_id, expected_binding)? {
            valid_since = Some(seq);
        } else {
            break;
        }
    }
    Ok(valid_since)
}

fn connection_world_and_commit_seq<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    world: &str,
    conn: &ObjectRecord,
) -> Result<Option<(String, u64)>, CellError> {
    use crate::engine::params;
    let ver =
        i64::try_from(conn.version).map_err(|_| CellError::OutOfRange("connection version"))?;
    let mut chosen = world;
    let mut seq: Option<i64> = reader.conn.query_optional(
        "SELECT commit_seq FROM _norn_fact_values \
         WHERE world=?1 AND root_type=?2 AND id=?3 AND version=?4",
        params![
            world,
            super::freshness::CONNECTION_TYPE,
            conn.id.as_str(),
            ver
        ],
        |row| row.get(0),
    )?;
    if seq.is_none() && world != MAIN_WORLD {
        chosen = MAIN_WORLD;
        seq = reader.conn.query_optional(
            "SELECT commit_seq FROM _norn_fact_values \
             WHERE world=?1 AND root_type=?2 AND id=?3 AND version=?4",
            params![
                MAIN_WORLD,
                super::freshness::CONNECTION_TYPE,
                conn.id.as_str(),
                ver
            ],
            |row| row.get(0),
        )?;
    }
    Ok(seq
        .and_then(|s| u64::try_from(s).ok())
        .map(|s| (chosen.to_owned(), s)))
}

pub(super) fn conn_matches_mapping_binding<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    world: &str,
    conn: &ObjectRecord,
    mapping_id: &str,
    expected_binding: &str,
) -> Result<bool, CellError> {
    if conn
        .values
        .get("mapping_id")
        .and_then(norn_ir::RuntimeValue::as_str)
        != Some(mapping_id)
    {
        return Ok(false);
    }
    let Some((_, commit_seq)) = connection_world_and_commit_seq(reader, world, conn)? else {
        return Ok(false);
    };
    commit_seq_matches_mapping_binding(reader, commit_seq, mapping_id, expected_binding)
}

fn load_authenticated_connection<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    hist_world: &str,
    conn_id: &str,
    ver: i64,
) -> Result<(Option<ObjectRecord>, u64), CellError> {
    use crate::engine::params;
    let ver_u64 = u64::try_from(ver).map_err(|_| CellError::OutOfRange("connection version"))?;
    let rec = reader.object_at_version(
        hist_world,
        super::freshness::CONNECTION_TYPE,
        conn_id,
        ver_u64,
    )?;
    let seq: i64 = reader.conn.query_row(
        "SELECT commit_seq FROM _norn_fact_values \
         WHERE world=?1 AND root_type=?2 AND id=?3 AND version=?4",
        params![hist_world, super::freshness::CONNECTION_TYPE, conn_id, ver],
        |row| row.get(0),
    )?;
    let seq_u64 = u64::try_from(seq).map_err(|_| CellError::OutOfRange("commit_seq"))?;
    Ok((rec, seq_u64))
}

fn next_indexed_mapping_id<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    hist_world: &str,
    conn_id: &str,
    after: Option<&str>,
) -> Result<Option<String>, CellError> {
    use crate::engine::params;
    match after {
        None => reader.conn.query_optional(
            "SELECT COALESCE(json_extract(snapshot, '$.values.mapping_id'), '') AS mid \
             FROM _norn_fact_values INDEXED BY \"_norn_fact_values/connection_mapping\" \
             WHERE world=?1 AND root_type='source.Connection' AND id=?2 \
             ORDER BY mid ASC LIMIT 1",
            params![hist_world, conn_id],
            |row| row.get(0),
        ),
        Some(prev) => reader.conn.query_optional(
            "SELECT COALESCE(json_extract(snapshot, '$.values.mapping_id'), '') AS mid \
             FROM _norn_fact_values INDEXED BY \"_norn_fact_values/connection_mapping\" \
             WHERE world=?1 AND root_type='source.Connection' AND id=?2 \
               AND COALESCE(json_extract(snapshot, '$.values.mapping_id'), '') > ?3 \
             ORDER BY mid ASC LIMIT 1",
            params![hist_world, conn_id, prev],
            |row| row.get(0),
        ),
    }
}

/// Indexed lookup of the latest version `<= max_ver` committed before
/// `valid_since` or under a different `mapping_id`. Returns `Ok(None)` when
/// the distinct-mapping bound is exceeded (caller fails closed).
fn latest_foreign_connection_version<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    hist_world: &str,
    conn_id: &str,
    max_ver: i64,
    valid_since: u64,
    mapping_id: &str,
) -> Result<Option<Option<i64>>, CellError> {
    use crate::engine::params;
    let valid_since_i64 =
        i64::try_from(valid_since).map_err(|_| CellError::OutOfRange("commit_seq"))?;
    let mut boundary: Option<i64> = reader.conn.query_optional(
        "SELECT version FROM _norn_fact_values INDEXED BY \"_norn_fact_values/connection_commit\" \
         WHERE world=?1 AND root_type='source.Connection' AND id=?2 \
           AND commit_seq<?3 AND version<=?4 \
         ORDER BY commit_seq DESC, version DESC LIMIT 1",
        params![hist_world, conn_id, valid_since_i64, max_ver],
        |row| row.get(0),
    )?;
    let mut cursor: Option<String> = None;
    for _ in 0..super::freshness::MAX_HISTORICAL_CONNECTIONS {
        let Some(mid) = next_indexed_mapping_id(reader, hist_world, conn_id, cursor.as_deref())?
        else {
            return Ok(Some(boundary));
        };
        if mid != mapping_id {
            let ver: Option<i64> = reader.conn.query_optional(
                "SELECT version FROM _norn_fact_values INDEXED BY \"_norn_fact_values/connection_mapping\" \
                 WHERE world=?1 AND root_type='source.Connection' AND id=?2 \
                   AND COALESCE(json_extract(snapshot, '$.values.mapping_id'), '')=?3 \
                   AND version<=?4 \
                 ORDER BY version DESC LIMIT 1",
                params![hist_world, conn_id, mid.as_str(), max_ver],
                |row| row.get(0),
            )?;
            boundary = match (boundary, ver) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (a, b) => a.or(b),
            };
        }
        cursor = Some(mid);
    }
    Ok(None)
}

fn verify_failed_gap<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    hist_world: &str,
    conn_id: &str,
    v_before: Option<i64>,
    v_first: i64,
) -> Result<bool, CellError> {
    use crate::engine::{SqlRow, params};
    let gap_start = v_before.map_or(1, |v| v.saturating_add(1));
    if gap_start >= v_first {
        return Ok(true);
    }
    let rows = reader.conn.query(
        "SELECT version FROM _norn_fact_values \
         WHERE world=?1 AND root_type=?2 AND id=?3 AND version>=?4 AND version<?5 \
         ORDER BY version DESC LIMIT 4097",
        params![
            hist_world,
            super::freshness::CONNECTION_TYPE,
            conn_id,
            gap_start,
            v_first
        ],
    )?;
    if rows.len() > super::freshness::MAX_HISTORICAL_CONNECTIONS {
        return Ok(false);
    }
    for values in rows {
        let fv = SqlRow(values).get::<i64>(0)?;
        let (Some(rec), _) = load_authenticated_connection(reader, hist_world, conn_id, fv)? else {
            return Ok(false);
        };
        if rec
            .values
            .get("status")
            .and_then(norn_ir::RuntimeValue::as_str)
            != Some("failed")
        {
            return Ok(false);
        }
    }
    Ok(true)
}

fn next_indexed_watermark<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    hist_world: &str,
    conn_id: &str,
    after: Option<&str>,
) -> Result<Option<String>, CellError> {
    use crate::engine::params;
    match after {
        None => reader.conn.query_optional(
            "SELECT COALESCE(json_extract(snapshot, '$.values.watermark'), '') AS wm \
             FROM _norn_fact_values INDEXED BY \"_norn_fact_values/connection_watermark\" \
             WHERE world=?1 AND root_type='source.Connection' AND id=?2 \
               AND COALESCE(json_extract(snapshot, '$.values.status'), '')!='failed' \
             ORDER BY wm ASC LIMIT 1",
            params![hist_world, conn_id],
            |row| row.get(0),
        ),
        Some(prev) => reader.conn.query_optional(
            "SELECT COALESCE(json_extract(snapshot, '$.values.watermark'), '') AS wm \
             FROM _norn_fact_values INDEXED BY \"_norn_fact_values/connection_watermark\" \
             WHERE world=?1 AND root_type='source.Connection' AND id=?2 \
               AND COALESCE(json_extract(snapshot, '$.values.status'), '')!='failed' \
               AND COALESCE(json_extract(snapshot, '$.values.watermark'), '') > ?3 \
             ORDER BY wm ASC LIMIT 1",
            params![hist_world, conn_id, prev],
            |row| row.get(0),
        ),
    }
}

fn prove_rewind_in_range<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    hist_world: &str,
    conn_id: &str,
    (v_first, cur_ver): (i64, i64),
    cur_wm: &str,
    (valid_since, mapping_id): (u64, &str),
    active: &mut BTreeMap<&str, &str>,
) -> Result<bool, CellError> {
    use crate::engine::params;
    if v_first >= cur_ver {
        return Ok(false);
    }
    let mut cursor: Option<String> = None;
    for _ in 0..super::freshness::MAX_HISTORICAL_CONNECTIONS {
        let Some(wm) = next_indexed_watermark(reader, hist_world, conn_id, cursor.as_deref())?
        else {
            return Ok(active.is_empty());
        };
        if wm != cur_wm
            && let Some(witness_ver) = reader.conn.query_optional(
                "SELECT version FROM _norn_fact_values \
                 INDEXED BY \"_norn_fact_values/connection_watermark\" \
                 WHERE world=?1 AND root_type='source.Connection' AND id=?2 \
                   AND COALESCE(json_extract(snapshot, '$.values.status'), '')!='failed' \
                   AND COALESCE(json_extract(snapshot, '$.values.watermark'), '')=?3 \
                   AND version>?4 AND version<?5 \
                 ORDER BY version DESC LIMIT 1",
                params![hist_world, conn_id, wm.as_str(), v_first, cur_ver],
                |row| row.get::<i64>(0),
            )?
        {
            let (Some(witness), witness_seq) =
                load_authenticated_connection(reader, hist_world, conn_id, witness_ver)?
            else {
                return Ok(false);
            };
            let valid_witness = witness_seq >= valid_since
                && witness
                    .values
                    .get("mapping_id")
                    .and_then(norn_ir::RuntimeValue::as_str)
                    == Some(mapping_id)
                && witness
                    .values
                    .get("status")
                    .and_then(norn_ir::RuntimeValue::as_str)
                    != Some("failed");
            if valid_witness {
                let witness_cursors = super::freshness::strict_connection_cursors(&witness);
                active.retain(|&ds, cj| witness_cursors.get(ds).map(String::as_str) == Some(*cj));
                if active.is_empty() {
                    return Ok(true);
                }
            }
        }
        cursor = Some(wm);
    }
    Ok(false)
}

fn first_run_version_valid<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    hist_world: &str,
    conn_id: &str,
    v_first: i64,
    (valid_since, mapping_id): (u64, &str),
    active: &BTreeMap<&str, &str>,
) -> Result<bool, CellError> {
    let (Some(first_rec), first_seq) =
        load_authenticated_connection(reader, hist_world, conn_id, v_first)?
    else {
        return Ok(false);
    };
    let first_cursors = super::freshness::strict_connection_cursors(&first_rec);
    Ok(first_seq >= valid_since
        && first_rec
            .values
            .get("mapping_id")
            .and_then(norn_ir::RuntimeValue::as_str)
            == Some(mapping_id)
        && first_rec
            .values
            .get("status")
            .and_then(norn_ir::RuntimeValue::as_str)
            != Some("failed")
        && active
            .iter()
            .all(|(&ds, &cj)| first_cursors.get(ds).map(String::as_str) == Some(cj)))
}

fn indexed_run_bounds<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    hist_world: &str,
    conn_id: &str,
    cur_wm: &str,
    (lower, cur_ver): (i64, i64),
) -> Result<Option<(i64, Option<i64>)>, CellError> {
    use crate::engine::params;
    let Some(v_first) = reader.conn.query_optional(
        "SELECT version FROM _norn_fact_values \
         INDEXED BY \"_norn_fact_values/connection_watermark\" \
         WHERE world=?1 AND root_type='source.Connection' AND id=?2 \
           AND COALESCE(json_extract(snapshot, '$.values.status'), '')!='failed' \
           AND COALESCE(json_extract(snapshot, '$.values.watermark'), '')=?3 \
           AND version>?4 AND version<=?5 \
         ORDER BY version ASC LIMIT 1",
        params![hist_world, conn_id, cur_wm, lower, cur_ver],
        |row| row.get::<i64>(0),
    )?
    else {
        return Ok(None);
    };
    let v_before: Option<i64> = reader.conn.query_optional(
        "SELECT version FROM _norn_fact_values \
         INDEXED BY \"_norn_fact_values/connection_nonfailed\" \
         WHERE world=?1 AND root_type='source.Connection' AND id=?2 \
           AND COALESCE(json_extract(snapshot, '$.values.status'), '')!='failed' \
           AND version<?3 \
         ORDER BY version DESC LIMIT 1",
        params![hist_world, conn_id, v_first],
        |row| row.get(0),
    )?;
    Ok(Some((v_first, v_before)))
}

fn prove_cursor_runs<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    hist_world: &str,
    conn_id: &str,
    bv: Option<i64>,
    (valid_since, mapping_id): (u64, &str),
    (mut cur_ver, mut cur_wm): (i64, String),
    mut active: BTreeMap<&str, &str>,
) -> Result<bool, CellError> {
    let lower = bv.unwrap_or(0);
    for _ in 0..super::freshness::MAX_HISTORICAL_CONNECTIONS {
        let Some((v_first, v_before)) =
            indexed_run_bounds(reader, hist_world, conn_id, &cur_wm, (lower, cur_ver))?
        else {
            return Ok(false);
        };
        if !first_run_version_valid(
            reader,
            hist_world,
            conn_id,
            v_first,
            (valid_since, mapping_id),
            &active,
        )? || !verify_failed_gap(reader, hist_world, conn_id, v_before, v_first)?
        {
            return Ok(false);
        }
        let Some(prev_ver) = v_before else {
            return Ok(true);
        };
        let (Some(prev_rec), prev_seq) =
            load_authenticated_connection(reader, hist_world, conn_id, prev_ver)?
        else {
            return Ok(true);
        };
        if prev_rec
            .values
            .get("status")
            .and_then(norn_ir::RuntimeValue::as_str)
            == Some("failed")
        {
            return Ok(false);
        }
        let prev_cursors = super::freshness::strict_connection_cursors(&prev_rec);
        active.retain(|&ds, cj| prev_cursors.get(ds).map(String::as_str) == Some(*cj));
        if active.is_empty() {
            return Ok(true);
        }
        let prev_foreign = bv.is_some_and(|b| prev_ver <= b)
            || prev_seq < valid_since
            || prev_rec
                .values
                .get("mapping_id")
                .and_then(norn_ir::RuntimeValue::as_str)
                != Some(mapping_id);
        if prev_foreign {
            return prove_rewind_in_range(
                reader,
                hist_world,
                conn_id,
                (v_first, cur_ver),
                &cur_wm,
                (valid_since, mapping_id),
                &mut active,
            );
        }
        let Some(next_wm) = prev_rec
            .values
            .get("watermark")
            .and_then(norn_ir::RuntimeValue::as_str)
        else {
            return Ok(false);
        };
        cur_ver = prev_ver;
        cur_wm = next_wm.to_owned();
    }
    Ok(false)
}

pub(super) fn connection_history_proves_cursors_at<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    world: &str,
    conn: &ObjectRecord,
    binding: (&str, &str),
    required: &BTreeMap<String, String>,
    allow_partial: bool,
) -> Result<bool, CellError> {
    reader.check_history("source connection history")?;
    let (mapping_id, expected_binding) = binding;
    let status = conn
        .values
        .get("status")
        .and_then(norn_ir::RuntimeValue::as_str);
    let status_ok = status == Some("ok") || (allow_partial && status == Some("partial"));
    if required.is_empty()
        || !status_ok
        || conn
            .values
            .get("mapping_id")
            .and_then(norn_ir::RuntimeValue::as_str)
            != Some(mapping_id)
    {
        return Ok(false);
    }
    let Some(cur_wm) = conn
        .values
        .get("watermark")
        .and_then(norn_ir::RuntimeValue::as_str)
    else {
        return Ok(false);
    };
    let latest_cursors = super::freshness::strict_connection_cursors(conn);
    if required
        .iter()
        .any(|(ds, cj)| latest_cursors.get(ds) != Some(cj))
    {
        return Ok(false);
    }
    let Some((hist_world, conn_seq)) = connection_world_and_commit_seq(reader, world, conn)? else {
        return Ok(false);
    };
    let max_ver =
        i64::try_from(conn.version).map_err(|_| CellError::OutOfRange("connection version"))?;
    let (Some(auth_conn), auth_seq) =
        load_authenticated_connection(reader, &hist_world, &conn.id, max_ver)?
    else {
        return Ok(false);
    };
    if auth_seq != conn_seq || auth_conn.values != conn.values {
        return Ok(false);
    }
    let Some(valid_since) =
        mapping_binding_valid_since(reader, conn_seq, mapping_id, expected_binding)?
    else {
        return Ok(false);
    };
    let Some(bv) = latest_foreign_connection_version(
        reader,
        &hist_world,
        &conn.id,
        max_ver,
        valid_since,
        mapping_id,
    )?
    else {
        return Ok(false);
    };
    if bv.is_some_and(|b| b >= max_ver) {
        return Ok(false);
    }
    let active: BTreeMap<&str, &str> = required
        .iter()
        .map(|(ds, cj)| (ds.as_str(), cj.as_str()))
        .collect();
    prove_cursor_runs(
        reader,
        &hist_world,
        &conn.id,
        bv,
        (valid_since, mapping_id),
        (max_ver, cur_wm.to_owned()),
        active,
    )
}
