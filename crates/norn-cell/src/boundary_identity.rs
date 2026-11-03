//! CELL-QUALIFY-1: exported commands and foreign inbox deduplication use envelopes.

use crate::{
    CellError, CellStore, OutboxCommand,
    engine::{SqlEngine, params},
};
use norn_ir::cell_reference::{CellReference, QualifiedCommit, ReferenceNamespace};
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
        let command = self.conn.query_optional(
            &format!(
                "SELECT \"generation\", \"idempotency_key\", \"deadline\", \"command\" FROM \"{}\" WHERE \"command_id\" = ?1 AND \"commit_seq\" = ?2",
                crate::system::OUTBOX,
            ),
            params![command_id, crate::read::to_i64(commit, "commit sequence")?],
            |row| {
                let deadline: Option<i64> = row.get(2)?;
                let payload: String = row.get(3)?;
                Ok(OutboxCommand {
                    command_id: command_id.to_owned(),
                    generation: crate::read::to_u64(row.get(0)?, "generation")?,
                    idempotency_key: row.get(1)?,
                    deadline: deadline.map(|v| crate::read::to_u64(v, "deadline")).transpose()?,
                    command: serde_json::from_str(&payload)?,
                })
            },
        )?.ok_or_else(|| CellError::Encoding("command not retained from this commit".to_owned()))?;
        if let Some(invocation) = crate::portable_receipt::function_invocation(&command.command)?
            && invocation.provenance_commit.qualified().is_none()
        {
            return Err(CellError::Encoding(
                "Function invocation is local-only".to_owned(),
            ));
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
            command,
        })
    }

    /// Ingest an authenticated foreign delivery using its full originating
    /// cell/world identity. Transport authentication precedes this local call.
    /// Exact retries deduplicate; equal local ids from different origins do not.
    ///
    /// # Errors
    /// Wrong reference namespace, invalid payload, or storage failure.
    pub fn inject_foreign<I: Serialize>(
        &mut self,
        reference: &CellReference,
        observed: &norn_kernel::Observed<I>,
    ) -> Result<bool, CellError> {
        if reference.namespace != ReferenceNamespace::Inbox || reference.local != observed.id.0 {
            return Err(CellError::Encoding(
                "foreign delivery requires its matching inbox reference".to_owned(),
            ));
        }
        if self.cell_id()?.is_none() {
            return Err(CellError::Encoding(
                "foreign delivery requires a bound target cell".to_owned(),
            ));
        }
        let identity = reference.content_digest()?;
        let qualified = norn_kernel::Observed {
            id: norn_kernel::ObservationId(identity.0.clone()),
            provenance: observed.provenance.clone(),
            input: &observed.input,
        };
        let payload = norn_ir::runtime::encode(&qualified).map_err(CellError::Encoding)?;
        self.inject(identity.as_str(), &payload)
    }
}
