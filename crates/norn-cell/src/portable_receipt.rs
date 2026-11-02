//! CELL-QUALIFY-1: portable evidence is issued atomically with a new commit.

use crate::engine::SqlEngine;
use crate::{
    CellError, CellStore, CommitBatch, CommitOutcome, InstalledDefinitionInput,
    InstalledDefinitionSet, Model, Receipt,
};
use norn_ir::{cell_reference::QualifiedCommit, function::Digest};
use serde::{Deserialize, Serialize};

/// Portable receipt envelope version; the inner local receipt is not rehashed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReceiptFormat {
    /// First portable envelope format.
    #[serde(rename = "norn.cell.receipt/v1")]
    V1,
}

/// Durable portable evidence, absent from unqualified historical records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PortableReceipt {
    /// Explicit envelope version.
    pub format: ReceiptFormat,
    /// Issuing cell/world and local commit.
    pub origin: QualifiedCommit,
    /// Exact original local receipt hash.
    pub receipt_hash: String,
}

impl PortableReceipt {
    /// Identity of this complete portable envelope.
    ///
    /// # Errors
    /// Canonical serialization failed.
    pub fn content_digest(&self) -> Result<Digest, CellError> {
        norn_ir::canonical::domain_hash_hex(b"norn.cell.receipt/v1\0", self)
            .map(|hex| Digest::from_hex(&hex))
            .map_err(CellError::from)
    }

    pub(crate) fn validates(&self, cell: &norn_ir::cell_id::CellId, receipt: &Receipt) -> bool {
        self.origin.belongs_to(cell, &receipt.body.world)
            && self.origin.commit == receipt.body.commit_seq
            && self.receipt_hash == receipt.hash
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum ReceiptMode {
    Current,
    LegacyReplay,
}

#[derive(Clone, Copy)]
pub(crate) struct JournalInput<'a> {
    pub submitted: Option<&'a norn_ir::RuntimeValue>,
    pub mode: ReceiptMode,
}

impl<'a> JournalInput<'a> {
    pub fn current(submitted: Option<&'a norn_ir::RuntimeValue>) -> Self {
        Self {
            submitted,
            mode: ReceiptMode::Current,
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) struct JournalReceipt<'a> {
    pub receipt: &'a Receipt,
    pub mode: ReceiptMode,
}
impl JournalReceipt<'_> {
    pub fn envelope(&self, cell: Option<norn_ir::cell_id::CellId>) -> Option<PortableReceipt> {
        match (self.mode, cell) {
            (ReceiptMode::Current, Some(cell)) => Some(PortableReceipt {
                format: ReceiptFormat::V1,
                origin: QualifiedCommit::new(
                    cell,
                    self.receipt.body.world.clone(),
                    self.receipt.body.commit_seq,
                ),
                receipt_hash: self.receipt.hash.clone(),
            }),
            (ReceiptMode::LegacyReplay, _) | (ReceiptMode::Current, None) => None,
        }
    }
}

impl<E: SqlEngine> CellStore<E> {
    /// Install an admitted definition set under the current receipt format.
    ///
    /// # Errors
    /// Version conflict, unsafe migration, or storage failure.
    pub fn install_definitions(
        &mut self,
        model: Model,
        input: InstalledDefinitionInput,
        expected: Option<u64>,
        scope: &str,
    ) -> Result<InstalledDefinitionSet, CellError> {
        self.install_definitions_with_receipt_mode(
            model,
            input,
            expected,
            scope,
            ReceiptMode::Current,
        )
    }

    /// Retrieve only evidence that was retained when its commit was made.
    /// CELL-QUALIFY-LEGACY: reading old history never manufactures an envelope.
    ///
    /// # Errors
    /// Unqualified history, mismatched origin, or storage failure.
    pub fn portable_receipt(&self, commit: u64) -> Result<PortableReceipt, CellError> {
        let record = self
            .export_record(commit)?
            .ok_or_else(|| CellError::Encoding("unknown receipt".to_owned()))?;
        let portable = record
            .portable_receipt
            .ok_or_else(|| CellError::Encoding("receipt is local-only".to_owned()))?;
        let cell = self
            .cell_id()?
            .ok_or_else(|| CellError::Encoding("store is local-only".to_owned()))?;
        if !portable.validates(&cell, &record.receipt) {
            return Err(CellError::Encoding(
                "portable receipt origin mismatch".to_owned(),
            ));
        }
        Ok(portable)
    }

    pub(crate) fn replay_commit(
        &mut self,
        record: &crate::export::ExportRecord,
    ) -> Result<CommitOutcome, CellError> {
        let input = JournalInput {
            submitted: record.quantity_input.as_ref(),
            mode: record.receipt_mode(),
        };
        self.commit_guarded_inner(
            &record.batch,
            None,
            input,
            |_reader| Ok::<(), CellError>(()),
            &mut |_point| Ok(()),
        )
        .map(|((), outcome)| outcome)
        .map_err(|error| match error {
            crate::GuardedError::Guard(e) | crate::GuardedError::Store(e) => e,
        })
    }
}

impl crate::export::ExportRecord {
    pub(crate) fn receipt_mode(&self) -> ReceiptMode {
        if self.portable_receipt.is_some() {
            ReceiptMode::Current
        } else {
            ReceiptMode::LegacyReplay
        }
    }
}

pub(crate) fn validate_dispatches(
    engine: &impl SqlEngine,
    batch: &CommitBatch,
    commit: u64,
    mode: ReceiptMode,
) -> Result<(), CellError> {
    if matches!(mode, ReceiptMode::LegacyReplay) {
        return Ok(());
    }
    for effect in &batch.effects {
        let crate::EffectRow::Enqueue { command } = effect else {
            continue;
        };
        let Some(invocation) = function_invocation(&command.command) else {
            continue;
        };
        if let Some(origin) = invocation.provenance_commit.qualified() {
            let cell = crate::cell_identity::read(engine)?;
            if invocation.world != batch.world
                || origin.commit != commit
                || cell
                    .as_ref()
                    .is_none_or(|cell| !origin.belongs_to(cell, &batch.world))
            {
                return Err(CellError::Encoding("Function dispatch provenance does not match its committing cell/world/position".to_owned()));
            }
        }
    }
    Ok(())
}

pub(crate) fn function_invocation(
    value: &norn_ir::RuntimeValue,
) -> Option<norn_ir::function::FunctionInvocation> {
    let payload = match value {
        norn_ir::RuntimeValue::Object(fields) => fields.get("invocation").unwrap_or(value),
        value => value,
    };
    norn_ir::runtime::decode(payload).ok()
}
