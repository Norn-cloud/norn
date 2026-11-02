//! CELL-QUALIFY-1: exported commands and foreign inbox deduplication use envelopes.

use crate::{CellError, CellStore, EffectRow, OutboxCommand, engine::SqlEngine};
use norn_ir::{
    RuntimeValue,
    cell_reference::{CellReference, QualifiedCommit, ReferenceNamespace},
};
use serde::{Deserialize, Serialize};

/// A committed outgoing command with portable identity and foreign deduplication key.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExportedCommand {
    /// Qualified issuing commit; not a global position.
    pub origin: QualifiedCommit,
    /// Logical command reference.
    pub identity: CellReference,
    /// Key for an external receiver's deduplication, qualified by cell and world.
    pub deduplication: CellReference,
    /// Original local command bytes and local keys.
    pub command: OutboxCommand,
}

impl<E: SqlEngine> CellStore<E> {
    /// Qualify a local key for export without changing its local storage identity.
    ///
    /// # Errors
    /// The host has no durable cell identity, or storage failed.
    pub fn export_reference(
        &self,
        world: &str,
        namespace: ReferenceNamespace,
        local: &str,
    ) -> Result<CellReference, CellError> {
        let cell = self
            .cell_id()?
            .ok_or_else(|| CellError::Encoding("reference is local-only".to_owned()))?;
        Ok(CellReference::new(
            cell,
            world.to_owned(),
            namespace,
            local.to_owned(),
        ))
    }

    /// Export a command retained in a newly qualified commit. Use the envelope's
    /// deduplication digest at foreign boundaries; local keys remain unchanged.
    ///
    /// # Errors
    /// Missing command, local-only history, or invalid provenance.
    pub fn export_command(
        &self,
        commit: u64,
        command_id: &str,
    ) -> Result<ExportedCommand, CellError> {
        let portable = self.portable_receipt(commit)?;
        let record = self
            .export_record(commit)?
            .ok_or_else(|| CellError::Encoding("unknown commit".to_owned()))?;
        let command = record
            .batch
            .effects
            .iter()
            .find_map(|effect| match effect {
                EffectRow::Enqueue { command } => {
                    (command.command_id == command_id).then_some(command)
                }
                EffectRow::ClaimPerform { .. }
                | EffectRow::Schedule { .. }
                | EffectRow::CancelTimer { .. } => None,
            })
            .ok_or_else(|| CellError::Encoding("command not retained in commit".to_owned()))?;
        if let Some(invocation) = crate::portable_receipt::function_invocation(&command.command) {
            if invocation.provenance_commit.qualified().is_none() {
                return Err(CellError::Encoding(
                    "Function invocation is local-only".to_owned(),
                ));
            }
        }
        Ok(ExportedCommand {
            identity: self.export_reference(
                &portable.origin.world,
                ReferenceNamespace::Command,
                &command.command_id,
            )?,
            deduplication: self.export_reference(
                &portable.origin.world,
                ReferenceNamespace::Operation,
                &command.idempotency_key,
            )?,
            origin: portable.origin,
            command: command.clone(),
        })
    }

    /// Ingest an authenticated foreign delivery using its full originating
    /// cell/world identity. Transport authentication precedes this local call.
    /// Exact retries deduplicate; equal local ids from different origins do not.
    ///
    /// # Errors
    /// Wrong reference namespace, invalid payload, or storage failure.
    pub fn inject_foreign(
        &mut self,
        reference: &CellReference,
        observed: &RuntimeValue,
    ) -> Result<bool, CellError> {
        if reference.namespace != ReferenceNamespace::Inbox {
            return Err(CellError::Encoding(
                "foreign delivery requires an inbox reference".to_owned(),
            ));
        }
        self.inject(reference.content_digest()?.as_str(), observed)
    }
}
