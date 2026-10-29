//! Rebuild the exported committed prefix through the store's typed commit path.
//!
//! Exported SQL is compared with the admitted model, never executed. Ingress
//! outside the prefix is unavailable; pending external commands are fenced.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::engine::{NativeEngine, SqlEngine, params};
use crate::export::{ExportError, ExportManifest, ExportRecord, Exporter};
use crate::history::restore_input;
use crate::system::{GENESIS_HASH, META};
use crate::{
    CellError, CellStore, CommitOutcome, InstalledDefinitionInput, LedgerHead, Model, StoreOptions,
};

/// Restore refused or failed; a failed replay never publishes the target file.
#[derive(Debug, thiserror::Error)]
pub enum RestoreError {
    /// A destination file or SQLite sidecar already exists.
    #[error("restore target already exists: {0}")]
    TargetExists(PathBuf),
    /// Independent export verification failed (typed integrity/storage cause).
    #[error("restore export verification: {0}")]
    Export(#[from] ExportError),
    /// An absent or empty manifest has no restorable model/cell identity.
    #[error("restore requires a nonempty verified export")]
    EmptyExport,
    /// The exported model differs from the caller's admitted projection.
    #[error("restore model mismatch")]
    ModelMismatch,
    /// Replay definitions differ, including semantics absent from projected DDL.
    #[error("restore model definitions mismatch")]
    ModelDefinitionsMismatch,
    /// Scope or identity bindings differ from the caller's expected identity.
    #[error("restore cell identity or scope mismatch")]
    IdentityMismatch,
    /// A consumed preimage disagrees with already rebuilt state.
    #[error("restore consumed input mismatch at commit {0}")]
    InputMismatch(u64),
    /// The normal apply path returned a different receipt/version/hash.
    #[error("restore recomputed receipt mismatch at commit {0}")]
    ReceiptMismatch(u64),
    /// The rebuilt export journal differs from the original envelope.
    #[error("restore recomputed export journal mismatch at commit {0}")]
    JournalMismatch(u64),
    /// Full rebuilt ledger verification or manifest-head equality failed.
    #[error("restore rebuilt head mismatch")]
    HeadMismatch,
    /// Typed store apply, serialization, or engine failure.
    #[error("restore cell: {0}")]
    Cell(#[from] CellError),
    /// Staging, sync, or atomic no-clobber publication failed.
    #[error("restore file: {0}")]
    File(#[from] std::io::Error),
}

/// Verified recovery boundary of the newly published file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreReport {
    /// Original manifest, also the rebuilt export journal head.
    pub manifest: ExportManifest,
    /// Fully verified original ledger head.
    pub ledger_head: LedgerHead,
    /// Number of restored pending commands fenced against automatic execution.
    pub fenced_commands: usize,
}

impl CellStore<NativeEngine> {
    /// Restore to an absent destination using a matching admitted model and
    /// explicit expected scope/bindings. Returns only after no-clobber publication.
    ///
    /// No external responder runs. Unconsumed injections and unexported outcomes
    /// are not recovered; pending commands are durably fenced. The export journal
    /// is reproduced exactly so the same object-store prefix can continue.
    ///
    /// # Errors
    ///
    /// [`RestoreError`] identifies verification, identity, replay, or file failures.
    #[allow(
        clippy::too_many_lines,
        reason = "one verified replay followed by atomic no-clobber file publication"
    )]
    pub async fn restore_from(
        exporter: &mut Exporter,
        path: &Path,
        model: Model,
        scope: &str,
        bindings: &BTreeMap<String, String>,
        options: StoreOptions,
    ) -> Result<RestoreReport, RestoreError> {
        let manifest = exporter.verify().await?;
        refuse_existing(path)?;
        if manifest.head_commit_seq == 0 {
            return Err(RestoreError::EmptyExport);
        }
        if manifest.ddl_hash != model.ddl_hash() {
            return Err(RestoreError::ModelMismatch);
        }
        if manifest.model_hash != model.model_hash() {
            return Err(RestoreError::ModelDefinitionsMismatch);
        }
        if manifest.scope != scope
            || &manifest.bindings != bindings
            || bindings.get("scope").is_some_and(|bound| bound != scope)
        {
            return Err(RestoreError::IdentityMismatch);
        }
        // Fetch again by the verified addresses: even a changing remote prefix
        // cannot substitute bytes beneath this trusted manifest during replay.
        let records = load_prefix(exporter, &manifest).await?;
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let staging = tempfile::tempdir_in(parent)?;
        let file = tempfile::NamedTempFile::new_in(staging.path())?;
        let first = records.first().ok_or(RestoreError::EmptyExport)?;
        let initial = first
            .initial_model
            .as_ref()
            .or_else(|| {
                first
                    .definition_install
                    .as_ref()
                    .map(|install| &install.model)
            })
            .ok_or(RestoreError::ModelDefinitionsMismatch)?;
        let mut store = Self::open(file.path(), Model::from_snapshot(initial)?, options)?;
        for record in &records {
            for (key, value) in &record.bindings {
                store.bind(key, value)?;
            }
            let seq = record.receipt.body.commit_seq;
            if let Some(install) = &record.definition_install {
                let model = Model::from_snapshot(&install.model)?;
                let installed = store.install_definitions(
                    model,
                    InstalledDefinitionInput {
                        canonical_set: install.installed.canonical_set.clone(),
                        registry: install.installed.registry.clone(),
                        receipt_sha256: install.installed.receipt_sha256.clone(),
                        admission_receipt: install.installed.admission_receipt.clone(),
                        readmission: install.readmission.clone(),
                        installed_at: install.installed.installed_at,
                        diff: install.diff.clone(),
                        layout: install.installed.layout.clone(),
                    },
                    Some(install.installed.version.saturating_sub(1)),
                    scope,
                )?;
                if installed != install.installed {
                    return Err(RestoreError::ReceiptMismatch(seq));
                }
            } else {
                if record.ddl_hash != store.model().ddl_hash()
                    || record.model_hash != store.model().model_hash()
                    || record.ddl_sql != store.model().schema().to_sql()
                {
                    return Err(RestoreError::ModelDefinitionsMismatch);
                }
                restore_input(&mut store, record).map_err(|error| {
                    if let CellError::HistoricalValueUnproven(_) = error {
                        RestoreError::InputMismatch(seq)
                    } else {
                        RestoreError::Cell(error)
                    }
                })?;
                let ((), outcome) = store
                    .commit_guarded_submitted(
                        &record.batch,
                        record.quantity_input.as_ref(),
                        |_reader| Ok::<(), CellError>(()),
                        &mut |_point| Ok(()),
                    )
                    .map_err(|error| match error {
                        crate::store::GuardedError::Guard(error)
                        | crate::store::GuardedError::Store(error) => error,
                    })?;
                if !matches!(outcome, CommitOutcome::Committed(_))
                    || outcome.receipt() != &record.receipt
                {
                    return Err(RestoreError::ReceiptMismatch(seq));
                }
            }
            if store.export_record(seq)?.as_ref() != Some(record) {
                return Err(RestoreError::JournalMismatch(seq));
            }
        }
        let ledger_head = store.read().verify_ledger()?;
        let rebuilt_hash = store
            .export_record(ledger_head.commits)?
            .ok_or(RestoreError::HeadMismatch)?
            .hash()?;
        if ledger_head.commits != manifest.head_commit_seq
            || ledger_head.seq != manifest.head_ledger_seq
            || ledger_head.hash != manifest.head_ledger_hash
            || rebuilt_hash != manifest.head_hash
        {
            return Err(RestoreError::HeadMismatch);
        }
        let pending = store.read().pending_commands()?;
        let fenced_commands = pending.len();
        for command in pending {
            store.conn.execute(
                &format!("INSERT INTO \"{META}\" (\"key\", \"value\") VALUES (?1, ?2)"),
                params![
                    format!("restore:perform_fence:{}", command.command.command_id),
                    command.command.generation.to_string()
                ],
            )?;
        }
        // Close checkpoints any WAL before moving the database; staging owns
        // every sidecar and removes them on all failure paths.
        store.conn.close()?;
        file.as_file().sync_all()?;
        refuse_existing(path)?;
        file.persist_noclobber(path).map_err(|error| {
            if error.error.kind() == std::io::ErrorKind::AlreadyExists {
                RestoreError::TargetExists(path.to_owned())
            } else {
                RestoreError::File(error.error)
            }
        })?;
        Ok(RestoreReport {
            manifest,
            ledger_head,
            fenced_commands,
        })
    }
}

fn refuse_existing(path: &Path) -> Result<(), RestoreError> {
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let mut name = path.as_os_str().to_owned();
        name.push(suffix);
        let candidate = PathBuf::from(name);
        match std::fs::symlink_metadata(&candidate) {
            Ok(_) => return Err(RestoreError::TargetExists(candidate)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

async fn load_prefix(
    exporter: &Exporter,
    manifest: &ExportManifest,
) -> Result<Vec<ExportRecord>, RestoreError> {
    let mut address = manifest.head_hash.clone();
    let mut records = Vec::new();
    for seq in (1..=manifest.head_commit_seq).rev() {
        let record = exporter.load_record(&address).await?;
        if record.receipt.body.commit_seq != seq {
            return Err(ExportError::SequenceMismatch.into());
        }
        address.clone_from(&record.previous);
        records.push(record);
    }
    if address != GENESIS_HASH {
        return Err(ExportError::GenesisMismatch.into());
    }
    records.reverse();
    Ok(records)
}
