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

fn load_authenticated_connection<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    hist_world: &str,
    conn_id: &str,
    ver: i64,
) -> Result<(Option<ObjectRecord>, u64), CellError> {
    use crate::engine::params;
    let ver_u64 = u64::try_from(ver).map_err(|_err| CellError::OutOfRange("connection version"))?;
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
    let seq_u64 = u64::try_from(seq).map_err(|_err| CellError::OutOfRange("commit_seq"))?;
    Ok((rec, seq_u64))
}

const DIRECT_SCAN_THRESHOLD: i64 = 4_096;
const MAX_SKIP_SCAN_STEPS: usize = 64;

fn str_field<'a>(rec: &'a ObjectRecord, key: &str) -> Option<&'a str> {
    rec.values.get(key).and_then(norn_ir::RuntimeValue::as_str)
}

fn verify_all_connection_versions<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    hist_world: &str,
    conn_id: &str,
    max_ver: i64,
) -> Result<(), CellError> {
    use crate::engine::{SqlRow, params};
    let mut expected = 1_i64;
    while expected <= max_ver {
        let rows = reader.conn.query(
            "SELECT version FROM _norn_fact_values \
             WHERE world=?1 AND root_type=?2 AND id=?3 AND version>=?4 AND version<=?5 \
             ORDER BY version ASC LIMIT 512",
            params![
                hist_world,
                super::freshness::CONNECTION_TYPE,
                conn_id,
                expected,
                max_ver
            ],
        )?;
        if rows.is_empty() {
            let _ = load_authenticated_connection(reader, hist_world, conn_id, expected)?;
            return Err(CellError::OutOfRange("connection version"));
        }
        for values in rows {
            let ver = SqlRow(values).get::<i64>(0)?;
            let check_ver = if ver == expected { ver } else { expected };
            let _ = load_authenticated_connection(reader, hist_world, conn_id, check_ver)?;
            if ver != expected {
                return Err(CellError::OutOfRange("connection version"));
            }
            expected = expected.saturating_add(1);
        }
    }
    Ok(())
}

fn next_indexed_text<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    index: &str,
    expr: &str,
    extra_where: &str,
    (hist_world, conn_id, after): (&str, &str, Option<&str>),
) -> Result<Option<String>, CellError> {
    use crate::engine::SqlValue;
    let tail = if after.is_some() {
        format!(" AND {expr} > ?3")
    } else {
        String::new()
    };
    let sql = format!(
        "SELECT {expr} AS val FROM _norn_fact_values INDEXED BY \"{index}\" \
         WHERE world=?1 AND root_type='source.Connection' AND id=?2{extra_where}{tail} \
         ORDER BY val ASC LIMIT 1"
    );
    let mut args = vec![SqlValue::from(hist_world), SqlValue::from(conn_id)];
    if let Some(prev) = after {
        args.push(SqlValue::from(prev));
    }
    reader.conn.query_optional(&sql, &args, |row| row.get(0))
}

fn scan_foreign_mapping_in_range<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    hist_world: &str,
    conn_id: &str,
    (after_seq, max_ver): (i64, i64),
    mapping_id: &str,
) -> Result<Option<i64>, CellError> {
    use crate::engine::params;
    reader.conn.query_optional(
        "SELECT version FROM _norn_fact_values \
         WHERE world=?1 AND root_type='source.Connection' AND id=?2 \
           AND version>?3 AND version<=?4 \
           AND COALESCE(json_extract(snapshot, '$.values.mapping_id'), '')!=?5 \
         ORDER BY version DESC LIMIT 1",
        params![hist_world, conn_id, after_seq, max_ver, mapping_id],
        |row| row.get(0),
    )
}

/// Indexed lookup of the latest version `<= max_ver` committed before
/// `valid_since` or under a different `mapping_id`. Uses a PK-bounded scan
/// within `DIRECT_SCAN_THRESHOLD` and an indexed skip-scan beyond it, falling
/// back to the PK-bounded scan after `MAX_SKIP_SCAN_STEPS` distinct keys so
/// arbitrary post-pin distinct mappings keep work strictly flat.
fn latest_foreign_connection_version<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    hist_world: &str,
    conn_id: &str,
    max_ver: i64,
    valid_since: u64,
    mapping_id: &str,
) -> Result<Option<i64>, CellError> {
    use crate::engine::params;
    let valid_since_i64 =
        i64::try_from(valid_since).map_err(|_err| CellError::OutOfRange("commit_seq"))?;
    let mut boundary: Option<i64> = reader.conn.query_optional(
        "SELECT version FROM _norn_fact_values INDEXED BY \"_norn_fact_values/connection_commit\" \
         WHERE world=?1 AND root_type='source.Connection' AND id=?2 \
           AND commit_seq<?3 AND version<=?4 \
         ORDER BY commit_seq DESC, version DESC LIMIT 1",
        params![hist_world, conn_id, valid_since_i64, max_ver],
        |row| row.get(0),
    )?;
    let after_seq = boundary.unwrap_or(0);
    if max_ver.saturating_sub(after_seq) <= DIRECT_SCAN_THRESHOLD {
        let foreign = scan_foreign_mapping_in_range(
            reader,
            hist_world,
            conn_id,
            (after_seq, max_ver),
            mapping_id,
        )?;
        return Ok(foreign.or(boundary));
    }
    let mut cursor: Option<String> = None;
    let mut visited = 0_usize;
    while let Some(mid) = next_indexed_text(
        reader,
        "_norn_fact_values/connection_mapping",
        "COALESCE(json_extract(snapshot, '$.values.mapping_id'), '')",
        "",
        (hist_world, conn_id, cursor.as_deref()),
    )? {
        visited = visited.saturating_add(1);
        if visited > MAX_SKIP_SCAN_STEPS {
            let foreign = scan_foreign_mapping_in_range(
                reader,
                hist_world,
                conn_id,
                (after_seq, max_ver),
                mapping_id,
            )?;
            return Ok(foreign.or(boundary));
        }
        if mid != mapping_id
            && let Some(found) = reader.conn.query_optional(
                "SELECT version FROM _norn_fact_values INDEXED BY \"_norn_fact_values/connection_mapping\" \
                 WHERE world=?1 AND root_type='source.Connection' AND id=?2 \
                   AND COALESCE(json_extract(snapshot, '$.values.mapping_id'), '')=?3 \
                   AND version>?4 AND version<=?5 \
                 ORDER BY version DESC LIMIT 1",
                params![hist_world, conn_id, mid.as_str(), after_seq, max_ver],
                |row| row.get::<i64>(0),
            )?
        {
            boundary = Some(boundary.map_or(found, |prev| prev.max(found)));
        }
        cursor = Some(mid);
    }
    Ok(boundary)
}

/// Authenticate every skipped version in `(v_before, v_first)` and return the
/// subset of `active` dataset keys carried by any foreign `failed` report
/// (`None` means a skipped version was invalid/non-failed or exceeded bounds).
fn verify_failed_gap<'a, E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    hist_world: &str,
    conn_id: &str,
    (v_before, v_first): (Option<i64>, i64),
    bv: Option<i64>,
    (valid_since, mapping_id): (u64, &str),
    active: &BTreeMap<&'a str, &str>,
) -> Result<Option<BTreeSet<&'a str>>, CellError> {
    use crate::engine::{SqlRow, params};
    let gap_start = v_before.map_or(1, |v| v.saturating_add(1));
    if gap_start >= v_first {
        return Ok(Some(BTreeSet::new()));
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
        return Ok(None);
    }
    let mut carried = BTreeSet::new();
    for values in rows {
        let fv = SqlRow(values).get::<i64>(0)?;
        let (Some(rec), seq) = load_authenticated_connection(reader, hist_world, conn_id, fv)?
        else {
            return Ok(None);
        };
        if str_field(&rec, "status") != Some("failed") {
            return Ok(None);
        }
        let is_foreign = bv.is_some_and(|b| fv <= b)
            || seq < valid_since
            || str_field(&rec, "mapping_id") != Some(mapping_id);
        if is_foreign {
            let failed_cursors = super::freshness::strict_connection_cursors(&rec);
            for (&ds, &cj) in active {
                if failed_cursors.get(ds).map(String::as_str) == Some(cj) {
                    carried.insert(ds);
                }
            }
        }
    }
    Ok(Some(carried))
}

fn apply_rewind_witness<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    hist_world: &str,
    conn_id: &str,
    witness_ver: i64,
    (valid_since, mapping_id): (u64, &str),
    active: &mut BTreeMap<&str, &str>,
) -> Result<Option<bool>, CellError> {
    let (Some(witness), witness_seq) =
        load_authenticated_connection(reader, hist_world, conn_id, witness_ver)?
    else {
        return Ok(Some(false));
    };
    let valid_witness = witness_seq >= valid_since
        && str_field(&witness, "mapping_id") == Some(mapping_id)
        && str_field(&witness, "status") != Some("failed");
    if valid_witness {
        let witness_cursors = super::freshness::strict_connection_cursors(&witness);
        active.retain(|&ds, cj| witness_cursors.get(ds).map(String::as_str) == Some(*cj));
        if active.is_empty() {
            return Ok(Some(true));
        }
    }
    Ok(None)
}

fn scan_rewind_in_range<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    hist_world: &str,
    conn_id: &str,
    (lower, upper): (i64, i64),
    (initial_wm, after_wm, limit): (&str, Option<&str>, usize),
    binding: (u64, &str),
    active: &mut BTreeMap<&str, &str>,
) -> Result<bool, CellError> {
    use crate::engine::{SqlRow, SqlValue};
    let limit_i64 = i64::try_from(limit).map_err(|_err| CellError::OutOfRange("rewind limit"))?;
    let tail = if after_wm.is_some() {
        " AND COALESCE(json_extract(snapshot, '$.values.watermark'), '')>?7"
    } else {
        ""
    };
    let mut args = vec![
        SqlValue::from(hist_world),
        SqlValue::from(conn_id),
        SqlValue::from(lower),
        SqlValue::from(upper),
        SqlValue::from(initial_wm),
        SqlValue::from(limit_i64),
    ];
    if let Some(prev) = after_wm {
        args.push(SqlValue::from(prev));
    }
    let sql = format!(
        "SELECT COALESCE(json_extract(snapshot, '$.values.watermark'), '') AS wm, \
                MAX(version) AS max_ver \
         FROM _norn_fact_values \
         INDEXED BY \"_norn_fact_values/connection_nonfailed\" \
         WHERE world=?1 AND root_type='source.Connection' AND id=?2 \
           AND COALESCE(json_extract(snapshot, '$.values.status'), '')!='failed' \
           AND version>?3 AND version<?4 \
           AND COALESCE(json_extract(snapshot, '$.values.watermark'), '')!=?5{tail} \
         GROUP BY wm \
         ORDER BY wm ASC \
         LIMIT ?6"
    );
    let rows = reader.conn.query(&sql, &args)?;
    for values in rows {
        let witness_ver = SqlRow(values).get::<i64>(1)?;
        if let Some(done) =
            apply_rewind_witness(reader, hist_world, conn_id, witness_ver, binding, active)?
        {
            return Ok(done);
        }
    }
    Ok(active.is_empty())
}

fn prove_rewind_in_range<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    hist_world: &str,
    conn_id: &str,
    (lower, upper): (i64, i64),
    initial_wm: &str,
    binding: (u64, &str),
    active: &mut BTreeMap<&str, &str>,
) -> Result<bool, CellError> {
    use crate::engine::params;
    if lower >= upper {
        return Ok(false);
    }
    if upper.saturating_sub(lower) <= DIRECT_SCAN_THRESHOLD {
        return scan_rewind_in_range(
            reader,
            hist_world,
            conn_id,
            (lower, upper),
            (
                initial_wm,
                None,
                super::freshness::MAX_HISTORICAL_CONNECTIONS,
            ),
            binding,
            active,
        );
    }
    let mut cursor: Option<String> = None;
    let mut visited = 0_usize;
    let mut checked = 0_usize;
    while let Some(wm) = next_indexed_text(
        reader,
        "_norn_fact_values/connection_watermark",
        "COALESCE(json_extract(snapshot, '$.values.watermark'), '')",
        " AND COALESCE(json_extract(snapshot, '$.values.status'), '')!='failed'",
        (hist_world, conn_id, cursor.as_deref()),
    )? {
        visited = visited.saturating_add(1);
        if visited > MAX_SKIP_SCAN_STEPS {
            let remaining = super::freshness::MAX_HISTORICAL_CONNECTIONS.saturating_sub(checked);
            return scan_rewind_in_range(
                reader,
                hist_world,
                conn_id,
                (lower, upper),
                (initial_wm, cursor.as_deref(), remaining),
                binding,
                active,
            );
        }
        if wm != initial_wm
            && let Some(witness_ver) = reader.conn.query_optional(
                "SELECT version FROM _norn_fact_values \
                 INDEXED BY \"_norn_fact_values/connection_watermark\" \
                 WHERE world=?1 AND root_type='source.Connection' AND id=?2 \
                   AND COALESCE(json_extract(snapshot, '$.values.status'), '')!='failed' \
                   AND COALESCE(json_extract(snapshot, '$.values.watermark'), '')=?3 \
                   AND version>?4 AND version<?5 \
                 ORDER BY version DESC LIMIT 1",
                params![hist_world, conn_id, wm.as_str(), lower, upper],
                |row| row.get::<i64>(0),
            )?
        {
            checked = checked.saturating_add(1);
            if let Some(done) =
                apply_rewind_witness(reader, hist_world, conn_id, witness_ver, binding, active)?
            {
                return Ok(done);
            }
        }
        cursor = Some(wm);
    }
    Ok(active.is_empty())
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
        && str_field(&first_rec, "mapping_id") == Some(mapping_id)
        && str_field(&first_rec, "status") != Some("failed")
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
    binding: (u64, &str),
    (max_ver, initial_wm): (i64, String),
    mut active: BTreeMap<&str, &str>,
) -> Result<bool, CellError> {
    let (valid_since, mapping_id) = binding;
    let lower = bv.unwrap_or(0);
    let mut cur_ver = max_ver;
    let mut cur_wm = initial_wm.clone();
    for _ in 0..super::freshness::MAX_HISTORICAL_CONNECTIONS {
        let Some((v_first, v_before)) =
            indexed_run_bounds(reader, hist_world, conn_id, &cur_wm, (lower, cur_ver))?
        else {
            return Ok(false);
        };
        if !first_run_version_valid(reader, hist_world, conn_id, v_first, binding, &active)? {
            return Ok(false);
        }
        let Some(carried_keys) = verify_failed_gap(
            reader,
            hist_world,
            conn_id,
            (v_before, v_first),
            bv,
            binding,
            &active,
        )?
        else {
            return Ok(false);
        };
        if !carried_keys.is_empty() {
            let mut carried: BTreeMap<&str, &str> = active
                .iter()
                .filter(|(ds, _)| carried_keys.contains(*ds))
                .map(|(&ds, &cj)| (ds, cj))
                .collect();
            if !prove_rewind_in_range(
                reader,
                hist_world,
                conn_id,
                (lower, max_ver),
                &initial_wm,
                binding,
                &mut carried,
            )? {
                return Ok(false);
            }
            active.retain(|ds, _| !carried_keys.contains(ds));
            if active.is_empty() {
                return Ok(true);
            }
        }
        let Some(prev_ver) = v_before else {
            return Ok(true);
        };
        let (Some(prev_rec), prev_seq) =
            load_authenticated_connection(reader, hist_world, conn_id, prev_ver)?
        else {
            return Ok(true);
        };
        if str_field(&prev_rec, "status") == Some("failed") {
            return Ok(false);
        }
        let prev_cursors = super::freshness::strict_connection_cursors(&prev_rec);
        active.retain(|&ds, cj| prev_cursors.get(ds).map(String::as_str) == Some(*cj));
        if active.is_empty() {
            return Ok(true);
        }
        let prev_foreign = bv.is_some_and(|b| prev_ver <= b)
            || prev_seq < valid_since
            || str_field(&prev_rec, "mapping_id") != Some(mapping_id);
        if prev_foreign {
            return prove_rewind_in_range(
                reader,
                hist_world,
                conn_id,
                (lower, max_ver),
                &initial_wm,
                binding,
                &mut active,
            );
        }
        let Some(next_wm) = str_field(&prev_rec, "watermark") else {
            return Ok(false);
        };
        cur_ver = prev_ver;
        next_wm.clone_into(&mut cur_wm);
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
    let status = str_field(conn, "status");
    let status_ok = status == Some("ok") || (allow_partial && status == Some("partial"));
    if required.is_empty() || !status_ok || str_field(conn, "mapping_id") != Some(mapping_id) {
        return Ok(false);
    }
    let Some(cur_wm) = str_field(conn, "watermark") else {
        return Ok(false);
    };
    let latest_cursors = super::freshness::strict_connection_cursors(conn);
    if required
        .iter()
        .any(|(ds, cj)| latest_cursors.get(ds) != Some(cj))
    {
        return Ok(false);
    }
    let Some((hist_world, conn_seq)) =
        super::freshness::connection_world_and_commit_seq(reader, world, conn)?
    else {
        return Ok(false);
    };
    let max_ver =
        i64::try_from(conn.version).map_err(|_err| CellError::OutOfRange("connection version"))?;
    if reader.has_unverified_external_writes()? {
        verify_all_connection_versions(reader, &hist_world, &conn.id, max_ver)?;
    }
    let (Some(auth_conn), auth_seq) =
        load_authenticated_connection(reader, &hist_world, &conn.id, max_ver)?
    else {
        return Ok(false);
    };
    if auth_seq != conn_seq || auth_conn.values != conn.values {
        return Ok(false);
    }
    let Some(valid_since) = super::freshness::mapping_binding_valid_since(
        reader,
        conn_seq,
        mapping_id,
        expected_binding,
    )?
    else {
        return Ok(false);
    };
    let bv = latest_foreign_connection_version(
        reader,
        &hist_world,
        &conn.id,
        max_ver,
        valid_since,
        mapping_id,
    )?;
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

#[cfg(all(test, feature = "native"))]
#[allow(clippy::expect_used, reason = "unit test for connection cursor proofs")]
mod tests {
    use super::*;
    use crate::MAIN_WORLD;
    use crate::test_door::TestCommit as _;

    fn open_conn_store() -> (crate::CellStore<crate::engine::NativeEngine>, String) {
        let obj_json = serde_json::json!({
            "name": "source.Connection",
            "properties": [
                {"id": "status", "type": {"kind": "text"}},
                {"id": "mapping_id", "type": {"kind": "text"}},
                {"id": "watermark", "type": {"kind": "text"}}
            ]
        });
        let def: norn_ir::ObjectTypeDef =
            serde_json::from_value(obj_json.clone()).expect("connection type");
        let model = crate::Model::from_definitions(&[def], &[]).expect("model");
        let mut store = crate::CellStore::open_in_memory(model.clone()).expect("store");
        let entity: norn_ir::ArtifactEnvelope = serde_json::from_value(serde_json::json!({
            "irVersion": "0.1",
            "artifactId": "source.Connection.entity",
            "kind": "entity",
            "name": "Connection",
            "namespace": "source",
            "version": "1.0.0",
            "lifecycle": "active",
            "dependencies": [],
            "scope": {},
            "provenance": {"source": "system", "sourcePath": "source.Connection.entity.json"},
            "payload": {"entityVersion": "2.0", "objectType": obj_json}
        }))
        .expect("entity artifact");
        let mapping: norn_ir::ArtifactEnvelope = serde_json::from_value(serde_json::json!({
            "irVersion": "0.1",
            "artifactId": "m.mapping",
            "kind": "mapping",
            "name": "M",
            "namespace": "m",
            "version": "1.0.0",
            "lifecycle": "active",
            "dependencies": [],
            "scope": {},
            "provenance": {"source": "system", "sourcePath": "m.mapping.json"},
            "payload": {}
        }))
        .expect("mapping artifact");
        let binding = super::super::freshness::mapping_binding_key(&mapping).expect("binding");
        let canonical_set = serde_json::to_string(&vec![entity, mapping]).expect("canonical_set");
        let input = crate::InstalledDefinitionInput {
            canonical_set,
            registry: Vec::new(),
            receipt_sha256: "a".repeat(64),
            admission_receipt: None,
            readmission: None,
            installed_at: 1,
            diff: serde_json::json!({
                "added": [], "removed": [], "changed": [], "narrowing": []
            }),
            layout: None,
        };
        store
            .install_definitions(model, input, Some(0), "synthetic")
            .expect("install");
        (store, binding)
    }

    fn put_conn(
        store: &mut crate::CellStore<crate::engine::NativeEngine>,
        mapping_id: &str,
        status: &str,
        watermark: &str,
        time: u64,
    ) {
        store
            .commit(&crate::CommitBatch {
                world: MAIN_WORLD.into(),
                scope: "synthetic".into(),
                logical_time: time,
                writes: vec![crate::FactWriteRow::Put {
                    object_type: "source.Connection".into(),
                    key: "conn".into(),
                    values: BTreeMap::from([
                        ("status".into(), RuntimeValue::from(status)),
                        ("mapping_id".into(), RuntimeValue::from(mapping_id)),
                        ("watermark".into(), RuntimeValue::from(watermark)),
                    ]),
                }],
                ..crate::CommitBatch::default()
            })
            .expect("commit");
    }

    fn pair_wm(first: &str, second: &str) -> String {
        format!(
            r#"{{"ds1":{{"version":"{first}","id":"1"}},"ds2":{{"version":"{second}","id":"2"}}}}"#
        )
    }

    fn proves_both(
        store: &crate::CellStore<crate::engine::NativeEngine>,
        binding: &str,
        first: &str,
        second: &str,
    ) -> bool {
        let reader = store.read();
        let conn = reader
            .object(MAIN_WORLD, "source.Connection", "conn")
            .expect("read")
            .expect("conn");
        let req = BTreeMap::from([
            ("ds1".into(), format!(r#"{{"version":"{first}","id":"1"}}"#)),
            (
                "ds2".into(),
                format!(r#"{{"version":"{second}","id":"2"}}"#),
            ),
        ]);
        connection_history_proves_cursors_at(
            &reader,
            MAIN_WORLD,
            &conn,
            ("m.mapping", binding),
            &req,
            false,
        )
        .expect("proof")
    }

    #[test]
    fn foreign_failed_only_requires_rewind_for_carried_dataset() {
        // B failed (X, Q) -> A (X, P) -> A (Y, P) -> A (X, P): X rewound, P was never carried.
        let (mut store, binding) = open_conn_store();
        put_conn(&mut store, "other.mapping", "failed", &pair_wm("X", "Q"), 2);
        put_conn(&mut store, "m.mapping", "ok", &pair_wm("X", "P"), 3);
        assert!(!proves_both(&store, &binding, "X", "P"));
        put_conn(&mut store, "m.mapping", "ok", &pair_wm("Y", "P"), 4);
        put_conn(&mut store, "m.mapping", "ok", &pair_wm("X", "P"), 5);
        assert!(proves_both(&store, &binding, "X", "P"));

        // B ok (Z, P) -> B failed (X, Q) -> A (X, P) -> A (Y, P) -> A (X, P): P carried from B ok!
        let (mut stale, stale_binding) = open_conn_store();
        put_conn(&mut stale, "other.mapping", "ok", &pair_wm("Z", "P"), 2);
        put_conn(&mut stale, "other.mapping", "failed", &pair_wm("X", "Q"), 3);
        put_conn(&mut stale, "m.mapping", "ok", &pair_wm("X", "P"), 4);
        put_conn(&mut stale, "m.mapping", "ok", &pair_wm("Y", "P"), 5);
        put_conn(&mut stale, "m.mapping", "ok", &pair_wm("X", "P"), 6);
        assert!(!proves_both(&stale, &stale_binding, "X", "P"));
    }

    #[test]
    fn over_4096_pre_pin_with_post_pin_distinct_entries_preserves_proof() {
        let (mut store, binding) = open_conn_store();
        put_conn(&mut store, "other.mapping", "ok", &pair_wm("X", "a"), 2);
        put_conn(&mut store, "m.mapping", "ok", &pair_wm("X", "b"), 3);
        put_conn(&mut store, "m.mapping", "ok", &pair_wm("X", "c"), 4);
        put_conn(&mut store, "m.mapping", "ok", &pair_wm("Y", "d"), 5);
        let target = pair_wm("X", "c");
        for step in 0_u64..4_096 {
            put_conn(
                &mut store,
                "m.mapping",
                "ok",
                &target,
                6_u64.saturating_add(step),
            );
        }
        let pinned = store
            .read()
            .object(MAIN_WORLD, "source.Connection", "conn")
            .expect("read")
            .expect("conn");
        assert!(pinned.version > 4_096);
        let req = BTreeMap::from([
            ("ds1".into(), r#"{"version":"X","id":"1"}"#.to_owned()),
            ("ds2".into(), r#"{"version":"c","id":"2"}"#.to_owned()),
        ]);
        assert!(
            connection_history_proves_cursors_at(
                &store.read(),
                MAIN_WORLD,
                &pinned,
                ("m.mapping", &binding),
                &req,
                false,
            )
            .expect("before post-pin")
        );
        for step in 0_u64..4_105 {
            let later_map = format!("0000-map-{step:05}");
            let later_wm = pair_wm(&format!("0000-{step:05}"), "z");
            put_conn(
                &mut store,
                &later_map,
                "ok",
                &later_wm,
                5_000_u64.saturating_add(step),
            );
        }
        assert!(
            connection_history_proves_cursors_at(
                &store.read(),
                MAIN_WORLD,
                &pinned,
                ("m.mapping", &binding),
                &req,
                false,
            )
            .expect("after post-pin")
        );
    }

    #[test]
    fn duplicate_non_initial_watermarks_after_rewind_are_pin_invariant() {
        // Initial (X, c) right after foreign (X, a) forces prove_rewind_in_range;
        // decisive rewind (Y, d) followed by 4,096 duplicate non-initial watermarks
        // (X, b) must stay provable after >64 earlier post-pin watermarks trigger fallback.
        let (mut store, binding) = open_conn_store();
        put_conn(&mut store, "other.mapping", "ok", &pair_wm("X", "a"), 2);
        let target = pair_wm("X", "c");
        put_conn(&mut store, "m.mapping", "ok", &target, 3);
        put_conn(&mut store, "m.mapping", "ok", &pair_wm("Y", "d"), 4);
        let dup_non_initial = pair_wm("X", "b");
        for step in 0_u64..4_096 {
            put_conn(
                &mut store,
                "m.mapping",
                "ok",
                &dup_non_initial,
                5_u64.saturating_add(step),
            );
        }
        put_conn(&mut store, "m.mapping", "ok", &target, 5_000);
        let pinned = store
            .read()
            .object(MAIN_WORLD, "source.Connection", "conn")
            .expect("read")
            .expect("conn");
        assert!(pinned.version > 4_096);
        let req = BTreeMap::from([
            ("ds1".into(), r#"{"version":"X","id":"1"}"#.to_owned()),
            ("ds2".into(), r#"{"version":"c","id":"2"}"#.to_owned()),
        ]);
        assert!(
            connection_history_proves_cursors_at(
                &store.read(),
                MAIN_WORLD,
                &pinned,
                ("m.mapping", &binding),
                &req,
                false,
            )
            .expect("before post-pin")
        );
        for step in 0_u64..128 {
            let later_wm = pair_wm(&format!("0000-{step:05}"), "z");
            put_conn(
                &mut store,
                "m.mapping",
                "ok",
                &later_wm,
                6_000_u64.saturating_add(step),
            );
        }
        assert!(
            connection_history_proves_cursors_at(
                &store.read(),
                MAIN_WORLD,
                &pinned,
                ("m.mapping", &binding),
                &req,
                false,
            )
            .expect("after post-pin")
        );
    }
}
