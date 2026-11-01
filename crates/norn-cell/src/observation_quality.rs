//! OBS-EVIDENCE-1: persist one proven finding per identity across redelivery.
use crate::{CellError, CommitBatch, StoreReader, engine::{SqlEngine, params}, read::to_i64, system::LEDGER};
use norn_ir::{DataQualityFinding, RuntimeValue};
use std::collections::BTreeSet;
impl<E: SqlEngine> StoreReader<'_, E> {
    fn verified_finding_at(&self, seq: u64) -> Result<Option<RuntimeValue>, CellError> {
        let Some(entry) = self
            .ledger_page(seq.saturating_sub(1), 1)?
            .into_iter()
            .find(|entry| entry.seq == seq)
        else {
            return Ok(None);
        };
        self.verified_history_event(&entry)
    }
}

/// Normalize findings inside the same capture transaction, before hashing and append.
/// The retry cutoff preserves the exact first request's normalized bytes.
pub(crate) fn deduplicate<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    batch: &mut CommitBatch,
    before_commit: u64,
) -> Result<(), CellError> {
    let events = std::mem::take(&mut batch.events);
    let mut seen = BTreeSet::new();
    for event in events {
        let finding = (event.get("op").and_then(RuntimeValue::as_str)
            == Some("data_quality_finding"))
        .then(|| {
            event
                .get("fact")
                .and_then(|fact| norn_ir::runtime::decode::<DataQualityFinding>(fact).ok())
        })
        .flatten();
        let mut known = false;
        if let Some(finding) = finding {
            if !seen.insert(finding.id.clone()) {
                continue;
            }
            let rows = reader.conn.query(
                &format!("SELECT seq FROM {LEDGER} WHERE world = ?1 AND json_extract(event, '$.op') = 'data_quality_finding' AND json_extract(event, '$.fact.id') = ?2 AND commit_seq < ?3 ORDER BY seq LIMIT 65"),
                params![&batch.world, finding.id, to_i64(before_commit, "finding cutoff")?],
            )?;
            if rows.len() > 64 {
                return Err(CellError::OutOfRange("finding dedup candidates"));
            }
            for values in rows {
                let seq: i64 = crate::engine::SqlRow(values).get(0)?;
                if reader
                    .verified_finding_at(crate::read::to_u64(seq, "finding sequence")?)?
                    .is_some()
                {
                    known = true;
                    break;
                }
            }
        }
        if !known {
            batch.events.push(event);
        }
    }
    Ok(())
}
