//! Export and fail-closed replay validation for `_norn_source_evidence` and
//! pending mapping-evidence inbox rows (`mapping-bundle:`, `mapping-conflict:`,
//! `mapping-identity:`).

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::RuntimeValue as Value;

use crate::engine::{SqlEngine, SqlRow, params};
use crate::error::CellError;
use crate::export::{ExportRecord, SourceEvidenceEntry};
use crate::read::{StoreReader, to_i64, to_u64};

const EVIDENCE_INBOX_PRED: &str = "(i.\"observation_id\" LIKE 'mapping-bundle:%' \
    OR i.\"observation_id\" LIKE 'mapping-conflict:%' \
    OR i.\"observation_id\" LIKE 'mapping-identity:%')";

pub(crate) fn reprocessed_target<'a>(observed: &'a Value, observation_id: &str) -> Option<&'a str> {
    observed
        .get("input")
        .and_then(|input| input.get("reprocesses"))
        .and_then(Value::as_str)
        .filter(|target| !target.is_empty() && *target != observation_id)
}

fn valid_bundle_observation<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    obs_id: &str,
    observed: &Value,
    diagnostics_check: Option<bool>,
) -> bool {
    if !obs_id.starts_with("mapping-bundle:") {
        return false;
    }
    let Ok(obs) = norn_ir::runtime::decode::<norn_kernel::Observed<Value>>(observed) else {
        return false;
    };
    if obs.id.0 != obs_id {
        return false;
    }
    let Ok(bundle) = norn_ir::runtime::decode::<crate::mapping_bundle::MappingBundle>(&obs.input)
    else {
        return false;
    };
    if format!("mapping-bundle:{}", bundle.digest) != obs_id
        || reader.recheck_mapping_bundle(&bundle).is_err()
    {
        return false;
    }
    match diagnostics_check {
        Some(true) => !bundle.diagnostics.is_empty() && bundle.reprocesses.is_none(),
        Some(false) => bundle.diagnostics.is_empty(),
        None => true,
    }
}

fn valid_evidence_observation<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    obs_id: &str,
    observed: &Value,
) -> bool {
    if obs_id.starts_with("mapping-bundle:") {
        return valid_bundle_observation(reader, obs_id, observed, None);
    }
    let Ok(obs) = norn_ir::runtime::decode::<norn_kernel::Observed<Value>>(observed) else {
        return false;
    };
    if obs.id.0 != obs_id {
        return false;
    }
    if obs_id.starts_with("mapping-conflict:") {
        norn_ir::runtime::decode::<crate::mapping_conflict::MappingConflict>(&obs.input).is_ok_and(
            |conflict| {
                format!("mapping-conflict:{}", conflict.finding.id) == obs_id
                    && reader.recheck_mapping_conflict(&conflict).is_ok()
            },
        )
    } else if obs_id.starts_with("mapping-identity:") {
        norn_ir::runtime::decode::<crate::mapping_identity::IdentityEvidence>(&obs.input)
            .is_ok_and(|ev| reader.recheck_identity_evidence(&ev, obs_id).is_ok())
    } else {
        false
    }
}

pub(crate) fn members_valid<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    observation_id: &str,
    observed: &Value,
    members: &[crate::PendingObservation],
) -> bool {
    let mut seen = BTreeSet::new();
    for member in members {
        if member.position == 0
            || member.observation_id == observation_id
            || !seen.insert(member.observation_id.as_str())
        {
            return false;
        }
    }
    if observation_id.starts_with(crate::mapping_page::PAGE_PREFIX) {
        crate::mapping_page::members(observed).is_ok_and(|ids| {
            members.iter().all(|member| {
                ids.contains(&member.observation_id)
                    && valid_bundle_observation(
                        reader,
                        &member.observation_id,
                        &member.observed,
                        Some(false),
                    )
            })
        })
    } else if observation_id.starts_with("mapping-bundle:") {
        members.is_empty()
            || (members.len() == 1
                && members.first().is_some_and(|member| {
                    Some(member.observation_id.as_str())
                        == reprocessed_target(observed, observation_id)
                        && valid_bundle_observation(
                            reader,
                            &member.observation_id,
                            &member.observed,
                            Some(true),
                        )
                }))
    } else if observation_id.starts_with("mapping-conflict:") {
        members.is_empty()
            || (members.len() == 1
                && members.first().is_some_and(|member| {
                    Some(member.observation_id.as_str()) == conflict_accepted_bundle_id(observed)
                        && valid_bundle_observation(
                            reader,
                            &member.observation_id,
                            &member.observed,
                            None,
                        )
                }))
    } else {
        members.is_empty()
    }
}

fn conflict_accepted_bundle_id(observed: &Value) -> Option<&str> {
    observed
        .get("input")
        .and_then(|input| input.get("acceptedBundleId"))
        .and_then(Value::as_str)
        .filter(|id| id.starts_with("mapping-bundle:"))
}

fn already_exported_pending<E: SqlEngine>(
    conn: &E,
    obs_id: &str,
    prev_seq: i64,
) -> Result<bool, CellError> {
    let table = crate::source_cursor::SOURCE_EVIDENCE;
    let inbox = crate::system::INBOX;
    // Only non-negative capture seqs represent an earlier export;
    // legacy sentinel `-1` is synthesized on first named promotion.
    let direct = conn.query_optional(
        &format!(
            "SELECT 1 FROM \"{table}\" \
             WHERE \"observation_id\" = ?1 \
               AND \"captured_commit_seq\" >= 0 AND \"captured_commit_seq\" < ?2 \
             LIMIT 1"
        ),
        params![obs_id, prev_seq],
        |_row| Ok(()),
    )?;
    if direct.is_some() {
        return Ok(true);
    }
    if !obs_id.starts_with("mapping-bundle:") {
        return Ok(false);
    }
    let via_conflict = conn.query_optional(
        &format!(
            "SELECT 1 FROM \"{inbox}\" c \
             JOIN \"{table}\" e ON e.\"observation_id\" = c.\"observation_id\" \
             WHERE substr(c.\"observation_id\", 1, 17) = 'mapping-conflict:' \
               AND json_extract(c.\"observed\", '$.input.acceptedBundleId') = ?1 \
               AND e.\"captured_commit_seq\" >= 0 AND e.\"captured_commit_seq\" < ?2 \
             LIMIT 1"
        ),
        params![obs_id, prev_seq],
        |_row| Ok(()),
    )?;
    Ok(via_conflict.is_some())
}

fn append_conflict_dependencies<E: SqlEngine>(
    conn: &E,
    prev_seq: i64,
    pending: &mut Vec<crate::PendingObservation>,
    seen_pending: &mut BTreeSet<String>,
) -> Result<(), CellError> {
    let inbox = crate::system::INBOX;
    let dep_ids: Vec<String> = pending
        .iter()
        .filter(|row| row.observation_id.starts_with("mapping-conflict:"))
        .filter_map(|row| conflict_accepted_bundle_id(&row.observed).map(str::to_owned))
        .collect();
    for dep_id in dep_ids {
        if !seen_pending.insert(dep_id.clone())
            || already_exported_pending(conn, &dep_id, prev_seq)?
        {
            continue;
        }
        let dep_row: Option<(i64, String)> = conn.query_optional(
            &format!(
                "SELECT \"position\", \"observed\" FROM \"{inbox}\" \
                 WHERE \"observation_id\" = ?1 AND \"consumed_commit_seq\" IS NULL"
            ),
            params![dep_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if let Some((pos, observed_json)) = dep_row {
            pending.push(crate::PendingObservation {
                position: to_u64(pos, "position")?,
                observation_id: dep_id,
                observed: serde_json::from_str(&observed_json)?,
            });
        }
    }
    Ok(())
}

/// Read pending mapping-evidence inbox rows first attributed between
/// `commit_seq - 1` and `commit_seq` together with late or pending
/// `_norn_source_evidence` attribution rows captured in that window.
pub(crate) fn read_late_source_evidence<E: SqlEngine>(
    conn: &E,
    commit_seq: u64,
) -> Result<(Vec<crate::PendingObservation>, Vec<SourceEvidenceEntry>), CellError> {
    if commit_seq == 0 {
        return Ok((Vec::new(), Vec::new()));
    }
    let prev_seq = to_i64(commit_seq.saturating_sub(1), "source evidence commit_seq")?;
    let current_seq = to_i64(commit_seq, "commit_seq")?;
    let table = crate::source_cursor::SOURCE_EVIDENCE;
    let inbox = crate::system::INBOX;
    let rows = conn.query(
        &format!(
            "SELECT e.\"observation_id\", e.\"connection_id\", e.\"captured_commit_seq\", \
                    i.\"position\", i.\"observed\", i.\"consumed_commit_seq\" \
             FROM \"{table}\" e \
             JOIN \"{inbox}\" i ON i.\"observation_id\" = e.\"observation_id\" \
             WHERE e.\"captured_commit_seq\" = ?1 \
               AND ((i.\"consumed_commit_seq\" IS NOT NULL AND i.\"consumed_commit_seq\" < ?2) \
                    OR (i.\"consumed_commit_seq\" IS NULL AND {EVIDENCE_INBOX_PRED})) \
             ORDER BY e.\"observation_id\", e.\"connection_id\""
        ),
        params![prev_seq, current_seq],
    )?;
    if rows.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    let mut pending = Vec::new();
    let mut seen_pending = BTreeSet::new();
    let mut out = Vec::with_capacity(rows.len());
    for values in rows {
        let row = SqlRow(values);
        let obs_id: String = row.get(0)?;
        let conn_id: String = row.get(1)?;
        let cap_seq: i64 = row.get(2)?;
        let pos: i64 = row.get(3)?;
        let observed_json: String = row.get(4)?;
        let consumed: Option<i64> = row.get(5)?;
        out.push(SourceEvidenceEntry {
            observation_id: obs_id.clone(),
            connection_id: conn_id,
            captured_commit_seq: cap_seq,
        });
        if consumed.is_none()
            && seen_pending.insert(obs_id.clone())
            && !already_exported_pending(conn, &obs_id, prev_seq)?
        {
            pending.push(crate::PendingObservation {
                position: to_u64(pos, "position")?,
                observation_id: obs_id,
                observed: serde_json::from_str(&observed_json)?,
            });
        }
    }
    append_conflict_dependencies(conn, prev_seq, &mut pending, &mut seen_pending)?;
    pending.sort_by(|a, b| {
        a.position
            .cmp(&b.position)
            .then_with(|| a.observation_id.cmp(&b.observation_id))
    });
    append_legacy_sentinels(conn, &mut out)?;
    Ok((pending, out))
}

fn append_legacy_sentinels<E: SqlEngine>(
    conn: &E,
    out: &mut Vec<SourceEvidenceEntry>,
) -> Result<(), CellError> {
    let table = crate::source_cursor::SOURCE_EVIDENCE;
    let mut obs_ids: Vec<String> = out
        .iter()
        .map(|entry| entry.observation_id.clone())
        .collect();
    obs_ids.dedup();
    for obs_id in obs_ids {
        let legacy = conn.query_optional(
            &format!(
                "SELECT 1 FROM \"{table}\" \
                 WHERE \"observation_id\" = ?1 AND \"connection_id\" = '' \
                   AND \"captured_commit_seq\" = -1"
            ),
            params![obs_id.as_str()],
            |_row| Ok(()),
        )?;
        if legacy.is_some() {
            out.push(SourceEvidenceEntry {
                observation_id: obs_id,
                connection_id: String::new(),
                captured_commit_seq: -1,
            });
        }
    }
    out.sort_by(|a, b| {
        a.observation_id
            .cmp(&b.observation_id)
            .then_with(|| a.connection_id.cmp(&b.connection_id))
            .then_with(|| a.captured_commit_seq.cmp(&b.captured_commit_seq))
    });
    out.dedup();
    Ok(())
}

fn legacy_sentinel_allowed<E: SqlEngine>(conn: &E, obs_id: &str) -> Result<bool, CellError> {
    let table = crate::source_cursor::SOURCE_EVIDENCE;
    let rows = conn.query(
        &format!(
            "SELECT \"connection_id\", \"captured_commit_seq\" FROM \"{table}\" \
             WHERE \"observation_id\" = ?1"
        ),
        params![obs_id],
    )?;
    if rows.is_empty() {
        return Ok(true);
    }
    for values in rows {
        let row = SqlRow(values);
        let cid: String = row.get(0)?;
        let cap: i64 = row.get(1)?;
        if cid.is_empty() && cap == -1 {
            return Ok(true);
        }
    }
    Ok(false)
}

fn restore_pending_observations<'a, E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    prev_seq: i64,
    pending: &'a [crate::PendingObservation],
    entries: &[SourceEvidenceEntry],
    mismatch: &impl Fn() -> CellError,
) -> Result<BTreeSet<&'a str>, CellError> {
    let inbox = crate::system::INBOX;
    let table = crate::source_cursor::SOURCE_EVIDENCE;
    let mut prev_pos = 0_u64;
    let mut restored_ids = BTreeSet::new();
    for row in pending {
        let has_prev_evidence = entries.iter().any(|entry| {
            entry.observation_id == row.observation_id && entry.captured_commit_seq == prev_seq
        });
        let is_legacy_conflict_dep = !has_prev_evidence
            && row.observation_id.starts_with("mapping-bundle:")
            && pending.iter().any(|other| {
                other.observation_id.starts_with("mapping-conflict:")
                    && other.position > row.position
                    && conflict_accepted_bundle_id(&other.observed)
                        == Some(row.observation_id.as_str())
                    && entries.iter().any(|entry| {
                        entry.observation_id == other.observation_id
                            && entry.captured_commit_seq == prev_seq
                    })
            })
            && reader
                .conn
                .query_optional(
                    &format!("SELECT 1 FROM \"{table}\" WHERE \"observation_id\" = ?1 LIMIT 1"),
                    params![row.observation_id.as_str()],
                    |_row| Ok(()),
                )?
                .is_none();
        if row.position == 0
            || row.position <= prev_pos
            || !restored_ids.insert(row.observation_id.as_str())
            || (!has_prev_evidence && !is_legacy_conflict_dep)
            || !valid_evidence_observation(reader, &row.observation_id, &row.observed)
        {
            return Err(mismatch());
        }
        prev_pos = row.position;
        reader
            .conn
            .execute(
                &format!(
                    "INSERT INTO \"{inbox}\" (\"position\", \"observation_id\", \"observed\", \"consumed_commit_seq\") \
                     VALUES (?1, ?2, ?3, NULL)"
                ),
                params![
                    to_i64(row.position, "position")?,
                    row.observation_id.as_str(),
                    crate::codec::canonical_json(&row.observed)?
                ],
            )
            .map_err(|_invalid| mismatch())?;
    }
    Ok(restored_ids)
}

/// Validate and restore pending evidence inbox rows and late `_norn_source_evidence`
/// rows carried by one [`ExportRecord`] before replaying commit `commit_seq`.
pub(crate) fn restore_late_source_evidence<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    commit_seq: u64,
    pending: &[crate::PendingObservation],
    entries: &[SourceEvidenceEntry],
) -> Result<(), CellError> {
    if pending.is_empty() && entries.is_empty() {
        return Ok(());
    }
    let mismatch =
        || CellError::HistoricalValueUnproven(format!("source evidence at commit {commit_seq}"));
    if commit_seq == 0 {
        return Err(mismatch());
    }
    let prev_seq = to_i64(commit_seq.saturating_sub(1), "source evidence commit_seq")?;
    let current_seq = to_i64(commit_seq, "commit_seq")?;
    let newly_restored =
        restore_pending_observations(reader, prev_seq, pending, entries, &mismatch)?;
    let conn = reader.conn;
    let inbox = crate::system::INBOX;
    let table = crate::source_cursor::SOURCE_EVIDENCE;
    let mut prev_key: Option<(&str, &str)> = None;
    for entry in entries {
        let key = (entry.observation_id.as_str(), entry.connection_id.as_str());
        if prev_key.is_some_and(|prior| key <= prior) {
            return Err(mismatch());
        }
        prev_key = Some(key);
        if entry.captured_commit_seq == -1 {
            let paired = entry.connection_id.is_empty()
                && entries.iter().any(|other| {
                    other.observation_id == entry.observation_id
                        && !other.connection_id.is_empty()
                        && other.captured_commit_seq == prev_seq
                });
            if !paired || !legacy_sentinel_allowed(conn, &entry.observation_id)? {
                return Err(mismatch());
            }
            continue;
        }
        if entry.captured_commit_seq != prev_seq {
            return Err(mismatch());
        }
        let existing_ev = conn.query_optional(
            &format!(
                "SELECT 1 FROM \"{table}\" WHERE \"observation_id\" = ?1 AND \"connection_id\" = ?2"
            ),
            params![entry.observation_id.as_str(), entry.connection_id.as_str()],
            |_row| Ok(()),
        )?;
        if existing_ev.is_some() {
            return Err(mismatch());
        }
        let state: Option<(Option<i64>, i64)> = conn.query_optional(
            &format!(
                "SELECT i.\"consumed_commit_seq\", CASE WHEN {EVIDENCE_INBOX_PRED} THEN 1 ELSE 0 END \
                 FROM \"{inbox}\" i WHERE i.\"observation_id\" = ?1"
            ),
            params![entry.observation_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let Some((consumed, is_evidence)) = state else {
            return Err(mismatch());
        };
        let allowed = newly_restored.contains(entry.observation_id.as_str())
            || (!entry.connection_id.is_empty()
                && (consumed.is_some_and(|seq| seq < current_seq)
                    || (consumed.is_none() && is_evidence == 1)));
        if !allowed {
            return Err(mismatch());
        }
    }
    for entry in entries {
        conn.execute(
            &format!(
                "INSERT OR IGNORE INTO \"{table}\" \
                 (\"observation_id\", \"connection_id\", \"captured_commit_seq\") \
                 VALUES (?1, ?2, ?3)"
            ),
            params![
                entry.observation_id.as_str(),
                entry.connection_id.as_str(),
                entry.captured_commit_seq
            ],
        )?;
    }
    Ok(())
}

pub(crate) fn read_source_evidence<E: SqlEngine>(
    conn: &E,
    observation_id: &str,
    members: &[crate::PendingObservation],
) -> Result<Vec<SourceEvidenceEntry>, CellError> {
    let table = crate::source_cursor::SOURCE_EVIDENCE;
    let mut ids = Vec::with_capacity(members.len().saturating_add(1));
    ids.push(observation_id);
    for member in members {
        ids.push(member.observation_id.as_str());
    }
    let mut out = Vec::new();
    for id in ids {
        let rows = conn.query(
            &format!(
                "SELECT \"observation_id\", \"connection_id\", \"captured_commit_seq\" \
                 FROM \"{table}\" WHERE \"observation_id\" = ?1 \
                 ORDER BY \"connection_id\", \"captured_commit_seq\""
            ),
            params![id],
        )?;
        for values in rows {
            let row = SqlRow(values);
            out.push(SourceEvidenceEntry {
                observation_id: row.get(0)?,
                connection_id: row.get(1)?,
                captured_commit_seq: row.get(2)?,
            });
        }
    }
    out.sort_by(|a, b| {
        a.observation_id
            .cmp(&b.observation_id)
            .then_with(|| a.connection_id.cmp(&b.connection_id))
            .then_with(|| a.captured_commit_seq.cmp(&b.captured_commit_seq))
    });
    out.dedup();
    Ok(out)
}

/// Restore exported `_norn_source_evidence` rows for one consumed observation
/// and its page or reprocessed members during `rebuild_export_history`.
/// Existing attributions must match verbatim; new attributions are bounded to
/// the current capture window (`commit_seq - 1`) or a valid legacy promotion.
pub(crate) fn restore_source_evidence<E: SqlEngine>(
    conn: &E,
    commit_seq: u64,
    observation_id: &str,
    members: &[crate::PendingObservation],
    entries: &[SourceEvidenceEntry],
    pre_existing_ids: &BTreeSet<&str>,
) -> Result<bool, CellError> {
    if commit_seq == 0 {
        return Ok(false);
    }
    let existing: BTreeMap<(String, String), i64> =
        read_source_evidence(conn, observation_id, members)?
            .into_iter()
            .map(|entry| {
                (
                    (entry.observation_id, entry.connection_id),
                    entry.captured_commit_seq,
                )
            })
            .collect();
    if entries.is_empty() {
        return Ok(existing.is_empty());
    }
    let prev_seq = to_i64(commit_seq.saturating_sub(1), "source evidence commit_seq")?;
    let mut prev_key: Option<(&str, &str)> = None;
    let mut seen_existing = 0_usize;
    for entry in entries {
        let key = (entry.observation_id.as_str(), entry.connection_id.as_str());
        if prev_key.is_some_and(|prior| key <= prior) {
            return Ok(false);
        }
        prev_key = Some(key);
        let allowed = entry.observation_id == observation_id
            || members
                .iter()
                .any(|member| member.observation_id == entry.observation_id);
        if !allowed {
            return Ok(false);
        }
        if let Some(&prior_seq) =
            existing.get(&(entry.observation_id.clone(), entry.connection_id.clone()))
        {
            if prior_seq != entry.captured_commit_seq {
                return Ok(false);
            }
            seen_existing = seen_existing.saturating_add(1);
            continue;
        }
        let new_ok = if entry.captured_commit_seq == -1 {
            entry.connection_id.is_empty()
                && entries.iter().any(|other| {
                    other.observation_id == entry.observation_id
                        && !other.connection_id.is_empty()
                        && other.captured_commit_seq == prev_seq
                })
                && legacy_sentinel_allowed(conn, &entry.observation_id)?
        } else {
            entry.captured_commit_seq == prev_seq
                && (!pre_existing_ids.contains(entry.observation_id.as_str())
                    || !entry.connection_id.is_empty())
        };
        if !new_ok {
            return Ok(false);
        }
    }
    if seen_existing != existing.len() {
        return Ok(false);
    }
    let table = crate::source_cursor::SOURCE_EVIDENCE;
    for entry in entries {
        conn.execute(
            &format!(
                "INSERT OR IGNORE INTO \"{table}\" \
                 (\"observation_id\", \"connection_id\", \"captured_commit_seq\") \
                 VALUES (?1, ?2, ?3)"
            ),
            params![
                entry.observation_id.as_str(),
                entry.connection_id.as_str(),
                entry.captured_commit_seq
            ],
        )?;
    }
    Ok(true)
}

fn valid_evidence_with_pending<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    obs_id: &str,
    observed: &Value,
    pending_observations: &[crate::PendingObservation],
) -> Result<bool, CellError> {
    if valid_evidence_observation(reader, obs_id, observed) {
        return Ok(true);
    }
    // Under `accept-first` or pre-v13 conflict consumption, the conflict's
    // accepted first bundle is NOT consumed and may be carried in the same
    // record's `pending_observations` (restored right after `restore_input`).
    // Recheck inside a savepoint without leaving a duplicate inbox row behind.
    if !obs_id.starts_with("mapping-conflict:") {
        return Ok(false);
    }
    let Some(accepted_id) = conflict_accepted_bundle_id(observed) else {
        return Ok(false);
    };
    let Some(pending) = pending_observations
        .iter()
        .find(|p| p.observation_id == accepted_id)
    else {
        return Ok(false);
    };
    if pending.position == 0
        || !valid_bundle_observation(reader, &pending.observation_id, &pending.observed, None)
    {
        return Ok(false);
    }
    let inbox = crate::system::INBOX;
    let already_in_inbox = reader
        .conn
        .query_optional(
            &format!("SELECT 1 FROM \"{inbox}\" WHERE \"observation_id\" = ?1"),
            params![accepted_id],
            |_row| Ok(()),
        )?
        .is_some();
    if already_in_inbox {
        return Ok(false);
    }
    reader
        .conn
        .execute_batch("SAVEPOINT \"norn_conflict_recheck\"")?;
    let inserted = reader.conn.execute(
        &format!(
            "INSERT INTO \"{inbox}\" (\"position\", \"observation_id\", \"observed\", \"consumed_commit_seq\") \
             VALUES (?1, ?2, ?3, NULL)"
        ),
        params![
            to_i64(pending.position, "position")?,
            pending.observation_id.as_str(),
            crate::codec::canonical_json(&pending.observed)?,
        ],
    );
    let ok = inserted.is_ok() && valid_evidence_observation(reader, obs_id, observed);
    reader.conn.execute_batch(
        "ROLLBACK TO SAVEPOINT \"norn_conflict_recheck\"; \
             RELEASE SAVEPOINT \"norn_conflict_recheck\"",
    )?;
    Ok(ok)
}

fn ensure_pending_inbox_row<E: SqlEngine>(
    conn: &E,
    position: u64,
    observation_id: &str,
    observed: &Value,
    allow_existing: bool,
    mismatch: &impl Fn() -> CellError,
) -> Result<bool, CellError> {
    let inbox = crate::system::INBOX;
    let pos_i64 = to_i64(position, "position")?;
    let obs_json = crate::codec::canonical_json(observed)?;
    let existing: Option<(i64, String, Option<i64>)> = conn.query_optional(
        &format!(
            "SELECT \"position\", \"observed\", \"consumed_commit_seq\" \
             FROM \"{inbox}\" WHERE \"observation_id\" = ?1"
        ),
        params![observation_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    if let Some((pos, obs_text, consumed)) = existing {
        if !allow_existing || consumed.is_some() || pos != pos_i64 || obs_text != obs_json {
            return Err(mismatch());
        }
        Ok(true)
    } else {
        conn.execute(
            &format!(
                "INSERT INTO \"{inbox}\" (\"position\", \"observation_id\", \"observed\", \"consumed_commit_seq\") \
                 VALUES (?1, ?2, ?3, NULL)"
            ),
            params![pos_i64, observation_id, obs_json],
        )
        .map_err(|_invalid| mismatch())?;
        Ok(false)
    }
}

/// Restore one consumed observation, its page or reprocessed members, and its
/// exported `_norn_source_evidence` rows during `rebuild_export_history`.
pub(crate) fn restore_observation_input<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    record: &ExportRecord,
    (position, observation_id, observed): (u64, &str, &Value),
    members: &[crate::PendingObservation],
    source_evidence: &[SourceEvidenceEntry],
) -> Result<(), CellError> {
    let seq = record.receipt.body.commit_seq;
    let mismatch = || CellError::HistoricalValueUnproven(format!("consumed input at commit {seq}"));
    let is_evidence_obs = observation_id.starts_with("mapping-bundle:")
        || observation_id.starts_with("mapping-conflict:")
        || observation_id.starts_with("mapping-identity:");
    if position == 0
        || record.batch.consumes
            != Some(crate::Consumption::Observation {
                observation_id: observation_id.to_owned(),
            })
        || !members_valid(reader, observation_id, observed, members)
    {
        return Err(mismatch());
    }
    let mut pre_existing = BTreeSet::new();
    // Restore members first so a conflict row whose accepted bundle was still
    // pending at resolution time can recheck against that bundle in `_norn_inbox`.
    for member in members {
        if ensure_pending_inbox_row(
            reader.conn,
            member.position,
            &member.observation_id,
            &member.observed,
            true,
            &mismatch,
        )? {
            pre_existing.insert(member.observation_id.as_str());
        }
    }
    if is_evidence_obs
        && !valid_evidence_with_pending(
            reader,
            observation_id,
            observed,
            &record.pending_observations,
        )?
    {
        return Err(mismatch());
    }
    if ensure_pending_inbox_row(
        reader.conn,
        position,
        observation_id,
        observed,
        is_evidence_obs,
        &mismatch,
    )? {
        pre_existing.insert(observation_id);
    }
    if !restore_source_evidence(
        reader.conn,
        seq,
        observation_id,
        members,
        source_evidence,
        &pre_existing,
    )? {
        return Err(mismatch());
    }
    Ok(())
}
