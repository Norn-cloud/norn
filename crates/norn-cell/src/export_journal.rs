//! Transactional journal of the cell commits that still need object-store export.

use std::collections::{BTreeMap, BTreeSet};
#[cfg(feature = "native")]
use std::sync::Arc;

use norn_ir::RuntimeValue as Value;
use serde::Serialize;
#[cfg(feature = "native")]
use tokio::sync::Notify;

use crate::engine::{SqlEngine, SqlRow, params};
use crate::error::CellError;
use crate::export::{EXPORT_FORMAT, ExportRecord, SourceEvidenceEntry};
use crate::model::Model;
use crate::read::{LedgerEntry, StoreReader, to_i64, to_u64};
use crate::store::{CellStore, CommitBatch, Consumption, DefinitionInstallRecord, Receipt};
use crate::system::{GENESIS_HASH, META};

pub(crate) const JOURNAL_DDL: &str = r#"CREATE TABLE IF NOT EXISTS "_norn_export_journal" (
  "commit_seq" INTEGER NOT NULL,
  "hash" TEXT NOT NULL,
  "record" TEXT NOT NULL,
  PRIMARY KEY ("commit_seq")
)"#;

/// Create the local export journal table in the store's initialization transaction.
pub(crate) fn init(engine: &impl SqlEngine) -> Result<(), CellError> {
    engine.execute_batch(JOURNAL_DDL)?;
    crate::export_journal_residue::init(engine)
}

/// Read the hash-verified journal preimage without recursively verifying its
/// events. Historical event verification uses this to recover its pinned model.
pub(crate) fn read_record(
    engine: &impl SqlEngine,
    commit_seq: u64,
) -> Result<Option<ExportRecord>, CellError> {
    let row: Option<(String, String)> = engine.query_optional(
        r#"SELECT "hash", "record" FROM "_norn_export_journal" WHERE "commit_seq" = ?1"#,
        params![to_i64(commit_seq, "commit_seq")?],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let Some((stored_hash, serialized)) = row else {
        return Ok(None);
    };
    let record = crate::payload_storage::decode_export(engine, &serialized, commit_seq)?;
    if record.format != EXPORT_FORMAT
        || record.receipt.body.commit_seq != commit_seq
        || record.hash()? != stored_hash
    {
        return Err(CellError::LedgerCorrupt {
            seq: commit_seq,
            reason: "export journal record does not verify".to_owned(),
        });
    }
    Ok(Some(record))
}

/// Capture the exact input being consumed before the commit marks it consumed.
pub(crate) fn consumed_input<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    consumed: Option<&Consumption>,
) -> Result<Option<Value>, CellError> {
    let Some(consumed) = consumed else {
        return Ok(None);
    };

    let not_pending = || CellError::NotPending(consumed.clone());
    match consumed {
        Consumption::Observation { observation_id } => {
            let pending = reader
                .pending_observation(observation_id)?
                .ok_or_else(not_pending)?;
            let members = if observation_id.starts_with(crate::mapping_page::PAGE_PREFIX) {
                let mut members = Vec::new();
                for id in crate::mapping_page::members(&pending.observed)? {
                    members.extend(reader.pending_observation(&id)?);
                }
                members
            } else if observation_id.starts_with("mapping-bundle:") {
                reprocessed_target(&pending.observed, observation_id)
                    .map(|id| reader.pending_observation(id))
                    .transpose()?
                    .flatten()
                    .into_iter()
                    .collect()
            } else {
                Vec::new()
            };
            let source_evidence =
                read_source_evidence(reader.conn, &pending.observation_id, &members)?;
            preimage(&ConsumedInput::Observation {
                position: pending.position,
                observation_id: pending.observation_id,
                observed: pending.observed,
                members,
                source_evidence,
            })
        }
        Consumption::Result {
            command_id,
            generation,
        } => {
            let pending = reader
                .pending_result(command_id, *generation)?
                .ok_or_else(not_pending)?;
            let runs = reader.runs(command_id)?;
            preimage(&ConsumedInput::Result {
                position: pending.position,
                commit_seq: pending.commit_seq,
                command: pending.command,
                result: pending.result,
                runs,
            })
        }
        Consumption::Timer {
            timer_id,
            incarnation,
        } => {
            let pending = reader
                .pending_timer(timer_id)?
                .filter(|row| row.commit_seq == *incarnation)
                .ok_or_else(not_pending)?;
            preimage(&ConsumedInput::Timer {
                commit_seq: pending.commit_seq,
                timer: pending.timer,
            })
        }
    }
}

const QUARANTINED_INBOX_PRED: &str = "(i.\"observation_id\" LIKE 'mapping-conflict:%' \
    OR i.\"observation_id\" LIKE 'mapping-identity:%' \
    OR (i.\"observation_id\" LIKE 'mapping-bundle:%' \
        AND COALESCE(json_array_length(i.\"observed\", '$.input.diagnostics'), 0) > 0))";

fn reprocessed_target<'a>(observed: &'a Value, observation_id: &str) -> Option<&'a str> {
    observed
        .get("input")
        .and_then(|input| input.get("reprocesses"))
        .and_then(Value::as_str)
        .filter(|target| !target.is_empty() && *target != observation_id)
}

fn valid_bundle_observation(obs_id: &str, observed: &Value, require_quarantined: bool) -> bool {
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
    if format!("mapping-bundle:{}", bundle.digest) != obs_id {
        return false;
    }
    if require_quarantined {
        !bundle.diagnostics.is_empty() && bundle.reprocesses.is_none()
    } else {
        bundle.diagnostics.is_empty()
    }
}

fn valid_quarantined_observation(obs_id: &str, observed: &Value) -> bool {
    if obs_id.starts_with("mapping-bundle:") {
        return valid_bundle_observation(obs_id, observed, true);
    }
    let Ok(obs) = norn_ir::runtime::decode::<norn_kernel::Observed<Value>>(observed) else {
        return false;
    };
    if obs.id.0 != obs_id {
        return false;
    }
    if obs_id.starts_with("mapping-conflict:") {
        norn_ir::runtime::decode::<crate::mapping_conflict::MappingConflict>(&obs.input)
            .is_ok_and(|conflict| format!("mapping-conflict:{}", conflict.finding.id) == obs_id)
    } else if obs_id.starts_with("mapping-identity:") {
        norn_ir::runtime::decode::<crate::mapping_identity::IdentityEvidence>(&obs.input)
            .is_ok_and(|ev| format!("mapping-identity:{}", ev.finding.id) == obs_id)
    } else {
        false
    }
}

pub(crate) fn members_valid(
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
                    && valid_bundle_observation(&member.observation_id, &member.observed, false)
            })
        })
    } else if observation_id.starts_with("mapping-bundle:") {
        members.is_empty()
            || (members.len() == 1
                && members.first().is_some_and(|member| {
                    Some(member.observation_id.as_str())
                        == reprocessed_target(observed, observation_id)
                        && valid_bundle_observation(&member.observation_id, &member.observed, true)
                }))
    } else {
        members.is_empty()
    }
}

/// Read pending quarantined inbox rows first captured between `commit_seq - 1`
/// and `commit_seq` together with late or quarantined `_norn_source_evidence`
/// attribution rows captured in that window.
pub(crate) fn read_late_source_evidence<E: SqlEngine>(
    conn: &E,
    commit_seq: u64,
) -> Result<(Vec<crate::PendingObservation>, Vec<SourceEvidenceEntry>), CellError> {
    if commit_seq == 0 {
        return Ok((Vec::new(), Vec::new()));
    }
    let prev_seq = to_i64(commit_seq - 1, "source evidence commit_seq")?;
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
                    OR (i.\"consumed_commit_seq\" IS NULL AND {QUARANTINED_INBOX_PRED})) \
             ORDER BY e.\"observation_id\", e.\"connection_id\""
        ),
        params![prev_seq, current_seq],
    )?;
    if rows.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    let mut quarantined = Vec::new();
    let mut seen_quarantined = BTreeSet::new();
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
        if consumed.is_none() && seen_quarantined.insert(obs_id.clone()) {
            let earlier = conn.query_optional(
                &format!(
                    "SELECT 1 FROM \"{table}\" \
                     WHERE \"observation_id\" = ?1 AND \"captured_commit_seq\" < ?2 LIMIT 1"
                ),
                params![obs_id.as_str(), prev_seq],
                |_row| Ok(()),
            )?;
            if earlier.is_none() {
                quarantined.push(crate::PendingObservation {
                    position: to_u64(pos, "position")?,
                    observation_id: obs_id,
                    observed: serde_json::from_str(&observed_json)?,
                });
            }
        }
    }
    quarantined.sort_by(|a, b| {
        a.position
            .cmp(&b.position)
            .then_with(|| a.observation_id.cmp(&b.observation_id))
    });
    append_legacy_sentinels(conn, &mut out)?;
    Ok((quarantined, out))
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

fn restore_quarantined_observations<'a, E: SqlEngine>(
    conn: &E,
    prev_seq: i64,
    quarantined: &'a [crate::PendingObservation],
    entries: &[SourceEvidenceEntry],
    mismatch: &impl Fn() -> CellError,
) -> Result<BTreeSet<&'a str>, CellError> {
    let inbox = crate::system::INBOX;
    let mut prev_pos = 0_u64;
    let mut restored_ids = BTreeSet::new();
    for row in quarantined {
        if row.position == 0
            || row.position <= prev_pos
            || !restored_ids.insert(row.observation_id.as_str())
            || !valid_quarantined_observation(&row.observation_id, &row.observed)
            || !entries.iter().any(|entry| {
                entry.observation_id == row.observation_id && entry.captured_commit_seq == prev_seq
            })
        {
            return Err(mismatch());
        }
        prev_pos = row.position;
        conn.execute(
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

/// Validate and restore quarantined inbox rows and late `_norn_source_evidence`
/// rows carried by one [`ExportRecord`] before replaying commit `commit_seq`.
pub(crate) fn restore_late_source_evidence<E: SqlEngine>(
    conn: &E,
    commit_seq: u64,
    quarantined: &[crate::PendingObservation],
    entries: &[SourceEvidenceEntry],
) -> Result<(), CellError> {
    if quarantined.is_empty() && entries.is_empty() {
        return Ok(());
    }
    let mismatch =
        || CellError::HistoricalValueUnproven(format!("source evidence at commit {commit_seq}"));
    if commit_seq == 0 {
        return Err(mismatch());
    }
    let prev_seq = to_i64(commit_seq - 1, "source evidence commit_seq")?;
    let current_seq = to_i64(commit_seq, "commit_seq")?;
    let newly_restored =
        restore_quarantined_observations(conn, prev_seq, quarantined, entries, &mismatch)?;
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
                && !newly_restored.contains(entry.observation_id.as_str())
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
                "SELECT i.\"consumed_commit_seq\", CASE WHEN {QUARANTINED_INBOX_PRED} THEN 1 ELSE 0 END \
                 FROM \"{inbox}\" i WHERE i.\"observation_id\" = ?1"
            ),
            params![entry.observation_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let Some((consumed, is_quarantined)) = state else {
            return Err(mismatch());
        };
        let allowed = newly_restored.contains(entry.observation_id.as_str())
            || (!entry.connection_id.is_empty()
                && (consumed.is_some_and(|seq| seq < current_seq)
                    || (consumed.is_none() && is_quarantined == 1)));
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

fn read_source_evidence<E: SqlEngine>(
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
pub(crate) fn restore_source_evidence<E: SqlEngine>(
    conn: &E,
    commit_seq: u64,
    observation_id: &str,
    members: &[crate::PendingObservation],
    entries: &[SourceEvidenceEntry],
) -> Result<bool, CellError> {
    if entries.is_empty() {
        return Ok(true);
    }
    if commit_seq == 0 {
        return Ok(false);
    }
    let current_seq = to_i64(commit_seq, "commit_seq")?;
    let table = crate::source_cursor::SOURCE_EVIDENCE;
    let mut prev_key: Option<(&str, &str)> = None;
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
        let seq_ok = if entry.captured_commit_seq == -1 {
            entry.connection_id.is_empty()
                && entries.iter().any(|other| {
                    other.observation_id == entry.observation_id
                        && !other.connection_id.is_empty()
                        && other.captured_commit_seq >= 0
                        && other.captured_commit_seq < current_seq
                })
                && legacy_sentinel_allowed(conn, &entry.observation_id)?
        } else {
            entry.captured_commit_seq >= 0 && entry.captured_commit_seq < current_seq
        };
        if !allowed || !seq_ok {
            return Ok(false);
        }
        let existing: Option<i64> = conn.query_optional(
            &format!(
                "SELECT \"captured_commit_seq\" FROM \"{table}\" \
                 WHERE \"observation_id\" = ?1 AND \"connection_id\" = ?2"
            ),
            params![entry.observation_id.as_str(), entry.connection_id.as_str()],
            |row| row.get(0),
        )?;
        if existing.is_some_and(|seq| seq != entry.captured_commit_seq) {
            return Ok(false);
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
    Ok(true)
}

/// Restore one consumed observation, its page or reprocessed members, and its
/// exported `_norn_source_evidence` rows during `rebuild_export_history`.
pub(crate) fn restore_observation_input<E: SqlEngine>(
    conn: &E,
    record: &ExportRecord,
    (position, observation_id, observed): (u64, &str, &Value),
    members: &[crate::PendingObservation],
    source_evidence: &[SourceEvidenceEntry],
) -> Result<(), CellError> {
    let seq = record.receipt.body.commit_seq;
    let mismatch = || CellError::HistoricalValueUnproven(format!("consumed input at commit {seq}"));
    if position == 0
        || record.batch.consumes
            != Some(crate::Consumption::Observation {
                observation_id: observation_id.to_owned(),
            })
        || !members_valid(observation_id, observed, members)
    {
        return Err(mismatch());
    }
    let inbox = crate::system::INBOX;
    conn.execute(
        &format!(
            "INSERT INTO \"{inbox}\" (\"position\", \"observation_id\", \"observed\", \"consumed_commit_seq\") \
             VALUES (?1, ?2, ?3, NULL)"
        ),
        params![
            to_i64(position, "position")?,
            observation_id,
            crate::codec::canonical_json(observed)?
        ],
    )
    .map_err(|_invalid| mismatch())?;
    for member in members {
        let member_pos = to_i64(member.position, "position")?;
        let member_json = crate::codec::canonical_json(&member.observed)?;
        let existing: Option<(i64, String, Option<i64>)> = conn.query_optional(
            &format!(
                "SELECT \"position\", \"observed\", \"consumed_commit_seq\" \
                 FROM \"{inbox}\" WHERE \"observation_id\" = ?1"
            ),
            params![member.observation_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        if let Some((pos, obs_text, consumed)) = existing {
            if !observation_id.starts_with("mapping-bundle:")
                || consumed.is_some()
                || pos != member_pos
                || obs_text != member_json
            {
                return Err(mismatch());
            }
        } else {
            conn.execute(
                &format!(
                    "INSERT INTO \"{inbox}\" (\"position\", \"observation_id\", \"observed\", \"consumed_commit_seq\") \
                     VALUES (?1, ?2, ?3, NULL)"
                ),
                params![member_pos, member.observation_id.as_str(), member_json],
            )
            .map_err(|_invalid| mismatch())?;
        }
    }
    if !restore_source_evidence(conn, seq, observation_id, members, source_evidence)? {
        return Err(mismatch());
    }
    Ok(())
}

/// Decode one `ConsumedInput::Observation` preimage without buffering decimals.
pub(crate) fn decode_observation_input(input: &Value) -> Result<ConsumedInput, String> {
    let row: crate::PendingObservation = norn_ir::runtime::decode(input)?;
    let members = input
        .get("members")
        .map_or_else(|| Ok(Vec::new()), norn_ir::runtime::decode)?;
    let source_evidence = input
        .get("sourceEvidence")
        .map_or_else(|| Ok(Vec::new()), norn_ir::runtime::decode)?;
    Ok(ConsumedInput::Observation {
        position: row.position,
        observation_id: row.observation_id,
        observed: row.observed,
        members,
        source_evidence,
    })
}

/// The preimage of a consumed delivery, as recorded in the export.
#[derive(Debug, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub(crate) enum ConsumedInput {
    Observation {
        position: u64,
        observation_id: String,
        observed: Value,
        /// OBS-PAGE-1: a page's members this commit consumes with it, in page
        /// order; absent for every other delivery.
        #[serde(skip_serializing_if = "Vec::is_empty")]
        members: Vec<crate::PendingObservation>,
        /// Durable `_norn_source_evidence` attribution rows for this delivery.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        source_evidence: Vec<SourceEvidenceEntry>,
    },
    Result {
        position: u64,
        commit_seq: u64,
        command: crate::store::OutboxCommand,
        result: Option<Value>,
        runs: Vec<crate::read::RunRecord>,
    },
    Timer {
        commit_seq: u64,
        timer: crate::store::TimerEntry,
    },
}

/// `input` as an exact runtime value.
fn preimage(input: &ConsumedInput) -> Result<Option<Value>, CellError> {
    norn_ir::runtime::encode(input)
        .map(Some)
        .map_err(|reason| CellError::LedgerCorrupt {
            seq: 0,
            reason: format!("the consumed input does not encode: {reason}"),
        })
}

/// The cell's identity bindings, as its export records carry them.
fn bindings(engine: &impl SqlEngine) -> Result<BTreeMap<String, String>, CellError> {
    let mut bindings = BTreeMap::new();
    let rows = engine.query(&format!(
        "SELECT \"key\", \"value\" FROM \"{META}\" WHERE \"key\" LIKE 'binding:%' ORDER BY \"key\""
    ), &[])?;
    for values in rows {
        let row = SqlRow(values);
        let key: String = row.get(0)?;
        let value: String = row.get(1)?;
        if let Some(key) = key.strip_prefix("binding:") {
            bindings.insert(key.to_owned(), value);
        }
    }
    Ok(bindings)
}

/// Persist a commit's replayable export record in the same engine transaction.
///
/// INGEST-JOURNAL-MEMORY-1: `entries` are the ledger entries this commit's
/// writer just appended (`append_events`), so the record is built from memory
/// instead of reading the ledger span back. Every `created` and `modified`
/// event is still proven here, in every build, against this commit's
/// persisted receipt (#1527 S1): the writer proved it before appending, but
/// only against its pending snapshot digests, and the exporter's own check
/// ([`read_record`]) runs after this transaction can no longer roll back.
/// Debug builds also read the span back and prove it, as the journal did
/// before, and refuse the commit if the two ever differ.
#[allow(
    clippy::too_many_arguments,
    reason = "one commit's record fields, all written in the same transaction"
)]
pub(crate) fn record<E: SqlEngine>(
    engine: &E,
    model: &Model,
    batch: &CommitBatch,
    quantity_input: Option<Value>,
    receipt: &Receipt,
    consumed_input: Option<Value>,
    definition_install: Option<DefinitionInstallRecord>,
    storage_version: Option<u32>,
    automation: crate::automation_outbox::RecordedAutomation,
    entries: Vec<LedgerEntry>,
) -> Result<(), CellError> {
    let commit_seq = receipt.body.commit_seq;
    let prior: Option<(i64, String)> = engine
        .query_optional(
            r#"SELECT "commit_seq", "hash" FROM "_norn_export_journal" ORDER BY "commit_seq" DESC LIMIT 1"#,
            &[],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
    let previous = match prior {
        Some((seq, hash)) if to_u64(seq, "export commit_seq")? < commit_seq => hash,
        Some((seq, _)) => {
            return Err(CellError::LedgerCorrupt {
                seq: to_u64(seq, "export commit_seq")?,
                reason: "export journal is ahead of the committed receipt".to_owned(),
            });
        }
        None if commit_seq == 1 => GENESIS_HASH.to_owned(),
        // A pre-export cell file has receipts but no original CommitBatch payloads.
        // Keep new writes working while leaving a visible chain gap that the exporter
        // rejects instead of presenting a partial export as complete.
        None => String::new(),
    };

    if u64::try_from(entries.len()).map_err(|_overflow| CellError::OutOfRange("event count"))?
        != receipt.body.event_count
        || entries
            .first()
            .is_some_and(|entry| entry.seq != receipt.body.first_event_seq)
        || entries.iter().any(|entry| entry.commit_seq != commit_seq)
    {
        return Err(CellError::LedgerCorrupt {
            seq: commit_seq,
            reason: "export journal event span disagrees with the receipt".to_owned(),
        });
    }
    // Each proven event as a ledger reader decodes it, against the receipt
    // just inserted: one read-only pass, so the receipt is verified once.
    let receipts = std::cell::RefCell::default();
    let reader = StoreReader::new(engine, model).with_transaction_verified_receipts(&receipts);
    for entry in &entries {
        reader.verify_durable_event(&entry.world, entry.commit_seq, &entry.event)?;
    }
    let (quarantined_observations, source_evidence) =
        read_late_source_evidence(engine, commit_seq)?;
    let bindings = bindings(engine)?;
    let export = ExportRecord {
        format: EXPORT_FORMAT.to_owned(),
        store_format: engine.query_row(
            &format!("SELECT \"value\" FROM \"{META}\" WHERE \"key\"='format'"),
            &[],
            |row| row.get(0),
        )?,
        ddl_hash: model.ddl_hash().to_owned(),
        model_hash: model.model_hash().to_owned(),
        ddl_sql: model.ddl_sql().to_owned(),
        bindings,
        previous,
        batch: batch.clone(),
        quantity_input,
        receipt: receipt.clone(),
        entries,
        consumed_input,
        initial_model: (commit_seq == 1 && definition_install.is_none())
            .then(|| model.definitions_snapshot()),
        definition_install,
        storage_version,
        automation_continuation: automation.continuation,
        automation_inline: automation.inline,
        source_evidence,
        quarantined_observations,
    };
    // The record's hash and its stored encoding both start from its canonical
    // text; canonicalize it once. A steady-state main-world record stores its
    // residue alone (EXPORT-RESIDUE-1); the hash is the full record's either way.
    let canonical = crate::codec::canonical_json(&export)?;
    let hash = norn_ir::canonical::HashProfile::Norn1.hash_canonical_bytes(canonical.as_bytes());
    let serialized = if crate::export_journal_residue::eligible(&export) {
        crate::export_journal_residue::encode(engine, &export, &canonical)?
    } else {
        crate::payload_storage::encode_export(engine, &export, &canonical)?
    };
    engine.execute(
        r#"INSERT INTO "_norn_export_journal" ("commit_seq", "hash", "record") VALUES (?1, ?2, ?3)"#,
        params![to_i64(commit_seq, "commit_seq")?, hash, serialized],
    )?;
    Ok(())
}

impl<E: SqlEngine> CellStore<E> {
    /// Return the current commit head without rechecking the complete ledger.
    ///
    /// # Errors
    ///
    /// Engine or sequence-range errors while reading the receipt head.
    pub fn export_commit_count(&self) -> Result<u64, CellError> {
        // S8 (#1551): export reads wait until the history scrub completes.
        self.read().check_history("export reads")?;
        Ok(self.read().next_commit_seq()?.saturating_sub(1))
    }

    /// Return a portable record after verifying its hash, events and versioned
    /// cell identity against the owning store. Unqualified history stays local.
    ///
    /// # Errors
    ///
    /// `core.world.fork_evidence_forbidden` for a commit minted after the
    /// fork's lineage left main (FORK-EVIDENCE-1); [`CellError::Verifying`]
    /// while the history scrub is incomplete (S8); [`CellError::Fenced`] on a
    /// fenced cell; engine errors, invalid JSON, or an export record whose
    /// content hash does not verify.
    pub fn export_record(&self, commit_seq: u64) -> Result<Option<ExportRecord>, CellError> {
        // The lineage's main horizon, not this fork's base: a fork of a fork
        // inherits its parent fork's minted commits below its own base.
        if self
            .fork_pin()
            .is_some_and(|pin| commit_seq > pin.main_horizon)
        {
            return Err(CellError::ForkEvidenceForbidden);
        }
        // S8 (#1551): portable export records wait for the history scrub.
        self.read().check_history("export reads")?;
        let Some(record) = self.local_export_record(commit_seq)? else {
            return Ok(None);
        };
        let Some(record_cell_id) = crate::cell_identity::from_bindings(&record.bindings)? else {
            return Err(CellError::BindingMismatch {
                key: crate::cell_identity::KEY.to_owned(),
                expected: "a qualified cell export".to_owned(),
                found: String::new(),
            });
        };
        self.require_cell_id(&record_cell_id)?;
        Ok(Some(record))
    }

    /// [`Self::export_record`] for the store's own use: verified local
    /// history without portable cell provenance, and in a fork its minted
    /// commits too.
    pub(crate) fn local_export_record(
        &self,
        commit_seq: u64,
    ) -> Result<Option<ExportRecord>, CellError> {
        let Some(record) = read_record(&self.conn, commit_seq)? else {
            return Ok(None);
        };
        for entry in &record.entries {
            self.verifier()
                .verify_durable_event(&entry.world, entry.commit_seq, &entry.event)?;
        }
        Ok(Some(record))
    }

    /// A wake-up signal for an exporter waiting for new committed records.
    #[cfg(feature = "native")]
    #[must_use]
    pub fn export_notification(&self) -> Arc<Notify> {
        Arc::clone(&self.export_notify)
    }

    /// Whether a restored command is fenced from automatic external execution.
    ///
    /// A fence is read-only here; hosts must not clear it or resend that
    /// command generation without an explicit reconciliation policy.
    ///
    /// # Errors
    ///
    /// A SQLite read failure.
    pub fn perform_is_fenced(&self, command_id: &str) -> Result<bool, CellError> {
        Ok(self.perform_fence_generation(command_id)?.is_some())
    }

    /// The generation fenced by the most recent restore, when present.
    /// Hosts may authorize a later generation only after ledgered reconciliation.
    ///
    /// # Errors
    /// A store read or corrupt fence value.
    pub fn perform_fence_generation(&self, command_id: &str) -> Result<Option<u64>, CellError> {
        self.read().check_live()?;
        let fenced: Option<String> = self.conn.query_optional(
            &format!("SELECT \"value\" FROM \"{META}\" WHERE \"key\" = ?1"),
            params![format!("restore:perform_fence:{command_id}")],
            |row| row.get(0),
        )?;
        fenced
            .map(|value| {
                value
                    .parse::<u64>()
                    .map_err(|error| CellError::LedgerCorrupt {
                        seq: 0,
                        reason: format!("invalid restore fence generation: {error}"),
                    })
            })
            .transpose()
    }
}
