//! Shared bounded cursors for `Phase::History` definition installs and
//! `Phase::Seed` genesis definition-pin folding (S10).

use super::durable_event::DurableEventCursor;
use super::history::{LEDGER_ENTRY_MAX_ROWS, WAKE_ROW_BUDGET};
use super::{RowBudget, VerifiedPins};
use crate::engine::SqlEngine;
use crate::export::ExportRecord;
use crate::payload_storage::ExportDecodeCursor;
use crate::read::StoreReader;
use crate::store::InstalledDefinitionSet;
use crate::{CellError, CellStore, Receipt};

/// Resumable verifier for one definition-install commit under [`RowBudget`].
#[derive(Clone)]
pub(super) struct DefInstallCursor {
    version_index: usize,
    receipt: Receipt,
    export_cursor: Option<ExportDecodeCursor>,
    record: Option<Box<ExportRecord>>,
    event_index: usize,
    durable: Option<DurableEventCursor>,
}

impl DefInstallCursor {
    pub(super) fn new(version_index: usize, receipt: Receipt) -> Self {
        Self {
            version_index,
            receipt,
            export_cursor: None,
            record: None,
            event_index: 0,
            durable: None,
        }
    }

    pub(super) fn step<E: SqlEngine>(
        &mut self,
        store: &CellStore<E>,
        reader: &StoreReader<'_, E>,
        versions: &[InstalledDefinitionSet],
        budget: &mut RowBudget,
    ) -> Result<bool, CellError> {
        let version = versions
            .get(self.version_index)
            .ok_or(CellError::OutOfRange("definition history index"))?;
        if self.record.is_none() {
            if self.export_cursor.is_none() {
                if budget.exhausted() {
                    return Ok(false);
                }
                self.export_cursor = Some(
                    ExportDecodeCursor::open(&store.conn, version.commit_seq, budget)?.ok_or_else(
                        || CellError::LedgerCorrupt {
                            seq: version.commit_seq,
                            reason: "installed definitions have no typed export record".to_owned(),
                        },
                    )?,
                );
            }
            let Some(cursor) = self.export_cursor.as_mut() else {
                return Err(CellError::OutOfRange("definition export cursor"));
            };
            let Some(record) = cursor.step(&store.conn, budget)? else {
                return Ok(false);
            };
            self.export_cursor = None;
            self.record = Some(Box::new(record));
        }
        let Some(record) = self.record.as_deref() else {
            return Err(CellError::OutOfRange("definition export record"));
        };
        while self.event_index < record.entries.len() {
            if self.durable.is_none() {
                let entry = record
                    .entries
                    .get(self.event_index)
                    .ok_or(CellError::OutOfRange("definition entry index"))?;
                if crate::history::durable_event_is_proven(&entry.event) {
                    self.durable = DurableEventCursor::new(
                        &entry.world,
                        entry.commit_seq,
                        &record.receipt,
                        &entry.event,
                    )?;
                }
            }
            if let Some(durable) = self.durable.as_mut() {
                if !durable.step(reader, versions, budget)? {
                    return Ok(false);
                }
                self.durable = None;
            }
            self.event_index = self.event_index.saturating_add(1);
        }
        if budget.remaining() < LEDGER_ENTRY_MAX_ROWS {
            return Ok(false);
        }
        let entries = reader.scrub_ledger_page(self.receipt.body.first_event_seq, 1, budget)?;
        CellStore::<E>::verify_definition_install(
            versions,
            self.version_index,
            &self.receipt,
            record,
            entries.as_slice(),
        )?;
        Ok(true)
    }
}

/// Bounded definition-digest folder for genesis-open seed-only runs (`!run.history`).
#[derive(Debug, Clone, Default)]
pub(super) struct SeedDefsCursor {
    after_version: u64,
    digest: String,
    count: u64,
    max_version: u64,
}

impl SeedDefsCursor {
    pub(super) fn step<E: SqlEngine>(
        &mut self,
        store: &CellStore<E>,
        target_commits: u64,
    ) -> Result<Option<VerifiedPins>, CellError> {
        if self.digest.is_empty() {
            self.digest = crate::codec::hash_json(&"norn.open-checkpoint.definitions/1")?;
        }
        let mut budget = RowBudget::new(WAKE_ROW_BUDGET);
        let cap = (budget.remaining().saturating_sub(3) / 4).max(1);
        let page = store.verifier().installed_definition_versions_after(
            self.after_version,
            cap,
            &mut budget,
        )?;
        let read = page.len();
        let mut reached_past = false;
        for version in page {
            self.after_version = version.version;
            if version.commit_seq > target_commits {
                reached_past = true;
                break;
            }
            self.digest = crate::codec::hash_json(&(
                self.digest.as_str(),
                crate::codec::hash_json(&version)?,
            ))?;
            self.count = self.count.saturating_add(1);
            self.max_version = self.max_version.max(version.version);
        }
        if reached_past || read < cap {
            return Ok(Some(VerifiedPins {
                export: crate::cell_identity::last_record_through(&store.conn, target_commits)?,
                definitions: self.digest.clone(),
                definitions_count: self.count,
                definitions_max_version: self.max_version,
            }));
        }
        Ok(None)
    }
}
