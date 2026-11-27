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
use crate::{CellError, CellStore, CommitOutcome, LedgerHead, Model, StoreOptions};

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
    /// The proven prefix is shorter than the cell's head, or the history has
    /// a refused entry, and the operator did not name the cut: a restore never
    /// drops commits unacknowledged.
    #[error(
        "restore would keep commits 1..={proven_through} of {head_commit}; pass the cut ({proven_through}) to accept it"
    )]
    PrefixNotAcknowledged {
        /// The last commit the diagnosis proves.
        proven_through: u64,
        /// The cell's head commit (see [`HistoryDiagnosis::head_commit`]).
        head_commit: u64,
    },
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
        let expected_id = crate::cell_identity::from_bindings(bindings)
            .map_err(|_invalid_identity| RestoreError::IdentityMismatch)?
            .ok_or(RestoreError::IdentityMismatch)?;
        let manifest = exporter.verify().await?;
        refuse_existing(path)?;
        if manifest.head_commit_seq == 0 {
            return Err(RestoreError::EmptyExport);
        }
        // Fetch again by the verified addresses: even a changing remote prefix
        // cannot substitute bytes beneath this trusted manifest during replay.
        let records = load_prefix(exporter, &manifest).await?;
        // ENGINE-UPGRADE-1: every record names the storage version that wrote
        // it, inside its content address; one newer than this engine refuses
        // before anything is staged or compared.
        for record in &records {
            crate::engine_release::admit_export(
                record.storage_version,
                crate::engine_release::THIS_ENGINE,
            )?;
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
        let mut store = Self::from_retained_engine(
            NativeEngine::open(file.path(), options)?,
            Model::from_retained_snapshot(initial)?,
            &expected_id,
        )?;
        store.require_cell_id(&expected_id)?;
        for record in &records {
            for (key, value) in &record.bindings {
                store.bind(key, value)?;
            }
            let seq = record.receipt.body.commit_seq;
            crate::export_journal::restore_late_source_evidence(
                &store.conn,
                seq,
                &record.source_evidence,
            )
            .map_err(|error| {
                if let CellError::HistoricalValueUnproven(_) = error {
                    RestoreError::InputMismatch(seq)
                } else {
                    RestoreError::Cell(error)
                }
            })?;
            if let Some(install) = &record.definition_install {
                let installed = store.install_retained_definitions(record)?;
                if installed != install.installed {
                    return Err(RestoreError::ReceiptMismatch(seq));
                }
            } else {
                if record.ddl_hash != store.model().ddl_hash()
                    || record.model_hash != store.model().model_hash()
                    || record.ddl_sql != store.model().ddl_sql()
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
                let outcome = store.commit_retained_batch(record)?;
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
        store.admit_reconstructed_model()?;
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

/// What a read-only check of a stored cell's history found (#1423): how far
/// its ledger is proven, and the first entry that is not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryDiagnosis {
    /// The cell's head: the highest commit its receipts, export journal,
    /// ledger or commit clock name, so losing the tail of any of them, or of
    /// all three logs together, reads as a cut.
    pub head_commit: u64,
    /// Consecutive export journal records from commit 1; a restore can
    /// rebuild no further than this.
    pub journal_commits: u64,
    /// The last commit through which every ledger entry is proven and the
    /// journal can rebuild it: never past `journal_commits`.
    pub proven_through: u64,
    /// The first unproven entry, when there is one.
    pub first_unproven: Option<UnprovenEntry>,
}

/// One ledger entry the store's own value proofs refuse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnprovenEntry {
    /// Its ledger sequence.
    pub seq: u64,
    /// The commit it belongs to.
    pub commit_seq: u64,
    /// The event's `op`, when it names one.
    pub op: Option<String>,
    /// The refusal.
    pub reason: String,
}

/// A proven-prefix restore: what was found, and what the new file holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrefixRestore {
    /// The source's diagnosis; commits after `proven_through` are not restored.
    pub diagnosis: HistoryDiagnosis,
    /// The restored file's verified boundary.
    pub report: RestoreReport,
}

impl CellStore<NativeEngine> {
    /// Diagnose a stored cell file its open refuses, without opening it as a
    /// store: the file is opened read-only (no schema step, no journal-mode
    /// change, no hot-journal rollback; the main file is never written, though
    /// SQLite may create `-wal`/`-shm` sidecars for a WAL file), and every ledger
    /// entry is checked against the store's own value proofs, under the models
    /// its export journal retains, in one read transaction. The first entry
    /// they refuse is named; any other failure is an error, never a verdict.
    ///
    /// # Errors
    /// An unreadable file, a journal record that does not verify, or an engine
    /// failure during the check.
    pub fn diagnose_history(source: &Path) -> Result<HistoryDiagnosis, CellError> {
        let engine = NativeEngine::open_read_only(source)?;
        diagnose(&engine).map(|(diagnosis, _)| diagnosis)
    }

    /// The operator restore of a cell its open refuses (#1423): rebuild, into
    /// the absent `target`, the longest prefix of the source's own export
    /// journal whose history is proven, through the same typed replay as
    /// [`Self::restore_from`]. `source` is opened read-only; `staging` (an
    /// empty directory the operator owns) receives the prefix's export
    /// objects.
    ///
    /// A prefix shorter than the cell's head drops commits, so it is restored
    /// only when `through` names exactly the diagnosed `proven_through`: the
    /// operator acknowledges the cut they read from [`Self::diagnose_history`].
    /// A history proven through its head, with no refused entry, restores
    /// whole with `through` absent or equal to its head.
    ///
    /// # Errors
    /// [`RestoreError`] for an unacknowledged cut, or a refused, unverifiable
    /// or unpublishable restore.
    pub async fn restore_proven_prefix(
        source: &Path,
        target: &Path,
        staging: &Path,
        through: Option<u64>,
        options: StoreOptions,
    ) -> Result<PrefixRestore, RestoreError> {
        let engine = NativeEngine::open_read_only(source)?;
        let (diagnosis, records) = diagnose(&engine)?;
        drop(engine);
        let acknowledged = match through {
            None => {
                diagnosis.proven_through == diagnosis.head_commit
                    && diagnosis.first_unproven.is_none()
            }
            Some(cut) => cut == diagnosis.proven_through,
        };
        if !acknowledged {
            return Err(RestoreError::PrefixNotAcknowledged {
                proven_through: diagnosis.proven_through,
                head_commit: diagnosis.head_commit,
            });
        }
        if diagnosis.proven_through == 0 {
            return Err(RestoreError::EmptyExport);
        }
        let prefix = records
            .get(..usize::try_from(diagnosis.proven_through).unwrap_or(usize::MAX))
            .ok_or(RestoreError::EmptyExport)?;
        let mut exporter = Exporter::local(staging)?;
        for record in prefix {
            exporter.append(record).await?;
        }
        let manifest = exporter.verify().await?;
        let model = latest_model(prefix)?;
        if model.model_hash() != manifest.model_hash {
            return Err(RestoreError::ModelDefinitionsMismatch);
        }
        let report = Self::restore_from(
            &mut exporter,
            target,
            model,
            &manifest.scope,
            &manifest.bindings,
            options,
        )
        .await?;
        Ok(PrefixRestore { diagnosis, report })
    }
}

/// The diagnosis of an engine opened read-only, and its journal records, read
/// once, inside one read transaction (one snapshot, one pass-scoped proof
/// cache).
fn diagnose(engine: &NativeEngine) -> Result<(HistoryDiagnosis, Vec<ExportRecord>), CellError> {
    engine.read_transaction(|| {
        // ENGINE-UPGRADE-1: an older tool never diagnoses, and so never
        // restores a prefix of, a cell a newer engine wrote.
        crate::engine_release::admission(engine)?;
        let records = journal(engine)?;
        let model = latest_model(&records)?;
        let pass = std::cell::RefCell::default();
        let reader =
            crate::read::StoreReader::new(engine, &model).with_transaction_verified_receipts(&pass);
        // The head is the highest commit any record names: a tail lost from
        // any of them must read as a cut, never as a fully proven history.
        let mut head_commit = 0_u64;
        for table in [
            crate::system::RECEIPT,
            crate::system::LEDGER,
            "_norn_export_journal",
            // The single clock row, updated in place by every commit: it still
            // names the head when all three tails above are lost together.
            "_norn_clock",
        ] {
            let highest: Option<i64> = engine.query_optional(
                &format!("SELECT COALESCE(MAX(\"commit_seq\"), 0) FROM \"{table}\""),
                params![],
                |row| row.get(0),
            )?;
            head_commit = head_commit.max(crate::read::to_u64(highest.unwrap_or(0), "commit_seq")?);
        }
        let journal_commits: u64 = records.len().try_into().unwrap_or(u64::MAX);
        let diagnosis = match reader.first_unproven_entry()? {
            None => HistoryDiagnosis {
                head_commit,
                journal_commits,
                proven_through: head_commit.min(journal_commits),
                first_unproven: None,
            },
            Some((entry, error)) => HistoryDiagnosis {
                head_commit,
                journal_commits,
                proven_through: entry.commit_seq.saturating_sub(1).min(journal_commits),
                first_unproven: Some(UnprovenEntry {
                    seq: entry.seq,
                    commit_seq: entry.commit_seq,
                    op: entry
                        .event
                        .get("op")
                        .and_then(norn_ir::RuntimeValue::as_str)
                        .map(str::to_owned),
                    reason: error.to_string(),
                }),
            },
        };
        Ok((diagnosis, records))
    })
}

/// Every verified export journal record, in commit order. The provable
/// journal ends before commit `k` when its row is lost, and also when it is a
/// residue (EXPORT-RESIDUE-1) its context, receipt or ledger rows no longer
/// rebuild to its stored hash: the head and the ledger diagnosis still name
/// `k`. Any other journal row that does not verify still refuses.
fn journal(engine: &NativeEngine) -> Result<Vec<ExportRecord>, CellError> {
    let mut records = Vec::new();
    loop {
        let commit_seq = u64::try_from(records.len())
            .unwrap_or(u64::MAX)
            .saturating_add(1);
        let residue = crate::export_journal_residue::is_residue(engine, commit_seq)?;
        match crate::export_journal::read_record(engine, commit_seq) {
            Ok(Some(record)) => records.push(record),
            Ok(None) => break,
            Err(
                CellError::LedgerCorrupt { .. }
                | CellError::Json(_)
                | CellError::Encoding(_)
                | CellError::HistoricalValueUnproven(_),
            ) if residue => break,
            Err(error) => return Err(error),
        }
    }
    Ok(records)
}

/// The model the last retained install (or the initial snapshot) pins.
fn latest_model(records: &[ExportRecord]) -> Result<Model, CellError> {
    let snapshot = records
        .iter()
        .rev()
        .find_map(|record| {
            record
                .definition_install
                .as_ref()
                .map(|install| &install.model)
                .or(record.initial_model.as_ref())
        })
        .ok_or_else(|| {
            CellError::HistoricalValueUnproven("the journal retains no model".to_owned())
        })?;
    Model::from_retained_snapshot(snapshot)
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
