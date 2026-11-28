//! Shared bounded cursors for `Phase::History` definition installs and
//! `Phase::Seed` genesis definition-pin folding (S10, D5).

use super::durable_event::DurableEventCursor;
use super::history::{LEDGER_ENTRY_MAX_ROWS, WAKE_ROW_BUDGET};
use super::{RowBudget, VerifiedPins};
use crate::engine::SqlEngine;
use crate::export::ExportRecord;
use crate::payload_storage::ExportDecodeCursor;
use crate::read::StoreReader;
use crate::store::InstalledDefinitionSet;
use crate::{CellError, CellStore, Receipt};

/// Closed phase state of [`DefInstallCursor`] (`D5`): eliminates parallel
/// `Option<ExportDecodeCursor>` / `Option<Box<ExportRecord>>` states.
#[derive(Clone)]
enum DefInstallStage {
    Unopened,
    DecodingExport(Box<ExportDecodeCursor>),
    VerifyingEvents {
        record: Box<ExportRecord>,
        event_index: usize,
        durable: Option<DurableEventCursor>,
    },
}

/// Resumable verifier for one definition-install commit under [`RowBudget`].
#[derive(Clone)]
pub(super) struct DefInstallCursor {
    version_index: usize,
    receipt: Receipt,
    stage: DefInstallStage,
}

impl DefInstallCursor {
    pub(super) fn new(version_index: usize, receipt: Receipt) -> Self {
        Self {
            version_index,
            receipt,
            stage: DefInstallStage::Unopened,
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
        if matches!(self.stage, DefInstallStage::Unopened) {
            if budget.exhausted() {
                return Ok(false);
            }
            let cursor = ExportDecodeCursor::open(&store.conn, version.commit_seq, budget)?
                .ok_or_else(|| CellError::LedgerCorrupt {
                    seq: version.commit_seq,
                    reason: "installed definitions have no typed export record".to_owned(),
                })?;
            self.stage = DefInstallStage::DecodingExport(Box::new(cursor));
        }
        if let DefInstallStage::DecodingExport(cursor) = &mut self.stage {
            let Some(record) = cursor.step(&store.conn, budget)? else {
                return Ok(false);
            };
            self.stage = DefInstallStage::VerifyingEvents {
                record: Box::new(record),
                event_index: 0,
                durable: None,
            };
        }
        let DefInstallStage::VerifyingEvents {
            record,
            event_index,
            durable,
        } = &mut self.stage
        else {
            return Err(CellError::OutOfRange("definition export record"));
        };
        while *event_index < record.entries.len() {
            if durable.is_none() {
                let entry = record
                    .entries
                    .get(*event_index)
                    .ok_or(CellError::OutOfRange("definition entry index"))?;
                if crate::history::durable_event_is_proven(&entry.event) {
                    *durable = DurableEventCursor::new(
                        &entry.world,
                        entry.commit_seq,
                        &record.receipt,
                        &entry.event,
                    )?;
                }
            }
            if let Some(active_durable) = durable.as_mut() {
                if !active_durable.step(reader, versions, budget)? {
                    return Ok(false);
                }
                *durable = None;
            }
            *event_index = event_index.saturating_add(1);
        }
        if budget.remaining() < LEDGER_ENTRY_MAX_ROWS {
            return Ok(false);
        }
        let entries = reader.scrub_ledger_page(
            self.receipt.body.first_event_seq,
            1,
            self.receipt.body.fork_pin.as_ref(),
            budget,
        )?;
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

/// Closed outcome of one bounded [`SeedDefsCursor::step`].
pub(super) enum SeedDefsStep {
    /// More retained definition versions remain to fold.
    Continue(SeedDefsCursor),
    /// Folding finished; verified pins are ready for checkpoint recording.
    Ready(VerifiedPins),
}

/// Bounded definition-digest folder for genesis-open seed-only runs.
#[derive(Debug, Clone, Default)]
pub(super) struct SeedDefsCursor {
    after_version: u64,
    digest: Option<String>,
    max_version: u64,
}

impl SeedDefsCursor {
    pub(super) fn step<E: SqlEngine>(
        mut self,
        store: &CellStore<E>,
        target_commits: u64,
    ) -> Result<SeedDefsStep, CellError> {
        let mut digest = match self.digest.take() {
            Some(existing) => existing,
            None => crate::codec::hash_json(&"norn.open-checkpoint.definitions/1")?,
        };
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
            digest =
                crate::codec::hash_json(&(digest.as_str(), crate::codec::hash_json(&version)?))?;
            self.max_version = self.max_version.max(version.version);
        }
        if reached_past || read < cap {
            return Ok(SeedDefsStep::Ready(VerifiedPins {
                export: crate::cell_identity::last_record_through(&store.conn, target_commits)?,
                definitions: digest,
                definitions_max_version: self.max_version,
            }));
        }
        self.digest = Some(digest);
        Ok(SeedDefsStep::Continue(self))
    }
}
