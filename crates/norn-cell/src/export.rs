//! Versioned, content-addressed commit export over host-provided object storage.
//!
//! One exporter owns one dedicated cell prefix. Callers serialize publication;
//! two writers must never share a prefix. SQLite remains authority: a failed
//! upload leaves the transactional journal available for retry after restart.

use std::collections::BTreeMap;
#[cfg(feature = "native")]
use std::path::Path;
#[cfg(feature = "native")]
use std::sync::Arc;

use norn_ir::RuntimeValue as Value;
#[cfg(feature = "native")]
use object_store::local::LocalFileSystem;
#[cfg(feature = "native")]
use object_store::path::Path as ObjectPath;
#[cfg(feature = "native")]
use object_store::{ObjectStore, ObjectStoreExt as _, PutMode, PutOptions};
use serde::{Deserialize, Serialize};

use crate::codec::canonical_json;
use crate::codec::hash_json;
use crate::read::entry_hash;
use crate::system::{GENESIS_HASH, STORE_FORMAT};
use crate::{
    CellError, CommitBatch, DefinitionInstallRecord, InstalledDefinitionSet, LedgerEntry, Model,
    ModelDefinitionSnapshot, Receipt,
};

/// The version of the independently verifiable export contract.
pub const EXPORT_FORMAT: &str = "norn.cell.export/5";
/// The atomically replaced manifest object in a dedicated export prefix.
pub const MANIFEST_PATH: &str = "manifest.json";

/// Conservative logical byte bound for a retained export, including every
/// referenced payload occurrence. Hosts must check this before materialization.
///
/// # Errors
/// SQL or stored byte-length range errors.
pub fn record_decode_bytes(
    engine: &impl crate::engine::SqlEngine,
    commit_seq: u64,
) -> Result<Option<u64>, CellError> {
    let length: Option<i64> = engine.query_optional(
        &format!(
            "SELECT {} FROM \"_norn_export_journal\" WHERE \"commit_seq\"=?1",
            crate::payload_storage::EXPORT_BYTES
        ),
        &[crate::engine::SqlValue::Integer(crate::read::to_i64(
            commit_seq,
            "export commit seq",
        )?)],
        |row| row.get(0),
    )?;
    length
        .map(|length| crate::read::to_u64(length, "export decoded bytes"))
        .transpose()
}

/// One immutable object per committed batch, including batches with no events.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExportRecord {
    /// Export format version.
    pub format: String,
    /// SQLite store format version.
    pub store_format: String,
    /// Norn1 hash of the model's projected DDL string.
    pub ddl_hash: String,
    /// Norn1 hash of all replay-relevant object and property-type definitions.
    pub model_hash: String,
    /// Model DDL, retained for #2716's cell-file rebuild.
    pub ddl_sql: String,
    /// Identity bindings from the cell file (including tenant scope).
    pub bindings: BTreeMap<String, String>,
    /// Previous export object's Norn1 hash, or the genesis hash.
    pub previous: String,
    /// Norn1-canonicalized commit input: facts, whole-object writes, links and effects.
    /// Unicode spelling is NFC-equivalent; original bytes are not retained.
    pub batch: CommitBatch,
    /// Complete submitted quantity input before defaults and generated events.
    /// Retry identity is recomputed from this input and the pinned definition version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantity_input: Option<Value>,
    /// Original commit receipt, including its request hash and ledger head.
    pub receipt: Receipt,
    /// Original event chain links for this commit's ledger span.
    pub entries: Vec<LedgerEntry>,
    /// The consumed delivery's preimage, if any (observation, result, or timer).
    pub consumed_input: Option<Value>,
    /// Typed state installed by this commit. Only this record may change the
    /// projection or active definitions version during independent verification.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub definition_install: Option<DefinitionInstallRecord>,
    /// First model for a version-zero prefix, before any runtime install.
    /// Never executable SQL; restore derives DDL from these typed definitions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_model: Option<ModelDefinitionSnapshot>,
}

impl ExportRecord {
    /// Norn1 hash of the complete envelope: its content address.
    ///
    /// # Errors
    ///
    /// Serialization or canonicalization failed.
    pub fn hash(&self) -> Result<String, CellError> {
        hash_json(self)
    }

    /// Canonical bytes used by every host's immutable-object publisher.
    ///
    /// # Errors
    /// Serialization or canonicalization failed.
    pub fn bytes(&self) -> Result<Vec<u8>, CellError> {
        Ok(canonical_json(self)?.into_bytes())
    }

    /// Validate this committed envelope and derive the next manifest without IO.
    ///
    /// # Errors
    /// Envelope, identity, receipt, sequence, definitions or ledger mismatch.
    pub fn advance(&self, head: &ExportManifest) -> Result<ExportManifest, ExportError> {
        let body = &self.receipt.body;
        let invalid = |reason: &str| ExportError::Invalid(reason.to_owned());
        if self.format != EXPORT_FORMAT
            || self.store_format != STORE_FORMAT
            || head.store_format != STORE_FORMAT
        {
            return Err(ExportError::UnsupportedFormat);
        }
        if self.ddl_hash != hash_json(&self.ddl_sql)? {
            return Err(invalid("DDL hash mismatch"));
        }
        if body.commit_seq != increment(head.head_commit_seq)? || self.previous != head.head_hash {
            return Err(ExportError::SequenceMismatch);
        }
        if head.head_commit_seq != 0
            && (head.bindings != self.bindings || head.scope != self.batch.scope)
        {
            return Err(ExportError::IdentityMismatch);
        }
        self.verify_definition_state(head)?;
        if body.request_hash != hash_json(&self.batch)?
            || self.receipt.hash != hash_json(body)?
            || body.world != self.batch.world
            || body.logical_time != self.batch.logical_time
            || body.idempotency_key != self.batch.idempotency_key
            || body.quantity != self.batch.quantity
            || body.quantity_intent != self.batch.quantity_intent
            || body.quantity.is_some() != self.quantity_input.is_some()
            || body.quantity_intent
                != self
                    .quantity_input
                    .as_ref()
                    .map(|input| crate::quantity::retry_identity(input, body.definitions_version))
                    .transpose()?
            || body.quantity.is_some() != body.quantity_result.is_some()
            || body.consumed != self.batch.consumes
            || body.first_event_seq != increment(head.head_ledger_seq)?
            || body.event_count
                != u64::try_from(self.entries.len())
                    .map_err(|_overflow| invalid("event count overflow"))?
            || self.entries.len() != self.batch.events.len()
            || self.batch.consumes.is_some() != self.consumed_input.is_some()
        {
            return Err(ExportError::ReceiptMismatch);
        }
        let mut seq = head.head_ledger_seq;
        let mut hash = head.head_ledger_hash.clone();
        for (entry, event) in self.entries.iter().zip(&self.batch.events) {
            seq = increment(seq)?;
            if entry.seq != seq
                || entry.commit_seq != body.commit_seq
                || entry.world != body.world
                || entry.event != *event
                || entry.prev_hash != hash
                || entry.hash != entry_hash(seq, body.commit_seq, &body.world, event, &hash)?
            {
                return Err(ExportError::LedgerMismatch);
            }
            hash.clone_from(&entry.hash);
        }
        if hash != body.ledger_head {
            return Err(ExportError::ReceiptMismatch);
        }
        self.manifest_at(seq)
    }

    /// Match a recovered manifest to its exact originating journal envelope.
    ///
    /// # Errors
    /// The manifest disagrees with the journal's head or identity.
    pub fn validate_manifest(&self, manifest: &ExportManifest) -> Result<(), ExportError> {
        let ledger_seq = match self.entries.last() {
            Some(entry) => entry.seq,
            None => self
                .receipt
                .body
                .first_event_seq
                .checked_sub(1)
                .ok_or(ExportError::LedgerMismatch)?,
        };
        if self.manifest_at(ledger_seq)? != *manifest {
            return Err(ExportError::ManifestMismatch);
        }
        Ok(())
    }

    /// Verify a remote immutable object's canonical bytes against this journal record.
    ///
    /// # Errors
    /// Noncanonical or different object bytes, or serialization failure.
    pub fn verify_bytes(&self, bytes: &[u8]) -> Result<(), ExportError> {
        if self.bytes()?.as_slice() != bytes {
            return Err(ExportError::ContentAddressMismatch);
        }
        Ok(())
    }

    fn manifest_at(&self, ledger_seq: u64) -> Result<ExportManifest, ExportError> {
        let body = &self.receipt.body;
        Ok(ExportManifest {
            format: EXPORT_FORMAT.to_owned(),
            store_format: self.store_format.clone(),
            head_commit_seq: body.commit_seq,
            head_hash: self.hash()?,
            head_ledger_seq: ledger_seq,
            head_ledger_hash: body.ledger_head.clone(),
            ddl_hash: self.ddl_hash.clone(),
            model_hash: self.model_hash.clone(),
            definitions_version: body.definitions_version,
            bindings: self.bindings.clone(),
            scope: self.batch.scope.clone(),
        })
    }

    fn verify_definition_state(&self, head: &ExportManifest) -> Result<(), ExportError> {
        let body = &self.receipt.body;
        let invalid = |reason: &str| ExportError::Invalid(reason.to_owned());
        if head.head_commit_seq != 0 && self.initial_model.is_some() {
            return Err(invalid("initial model appears after export genesis"));
        }
        if let Some(install) = &self.definition_install {
            if self.initial_model.is_some() {
                return Err(invalid("an install may not also supply an initial model"));
            }
            let installed = &install.installed;
            let model = Model::from_snapshot(&install.model)?;
            if installed.version != increment(head.definitions_version)?
                || body.definitions_version != installed.version
                || installed.commit_seq != body.commit_seq
                || installed.installed_at != body.logical_time
                || installed.model_hash != model.model_hash()
                || installed.ddl_hash != model.ddl_hash()
                || self.model_hash != model.model_hash()
                || self.ddl_hash != model.ddl_hash()
                || self.ddl_sql != model.schema().to_sql()
            {
                return Err(invalid(
                    "typed definitions installation disagrees with its model or receipt",
                ));
            }
            let canonical = Value::from_json_text(&installed.canonical_set)
                .map_err(|error| invalid(&format!("installed artifact set is invalid: {error}")))?;
            if !matches!(canonical, Value::Array(_))
                || canonical
                    .canonical_json()
                    .map_err(|error| invalid(&error.to_string()))?
                    != installed.canonical_set.as_bytes()
                || installed.receipt_sha256.len() != 64
                || !installed
                    .receipt_sha256
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            {
                return Err(invalid(
                    "installed artifact set or checker digest is not canonical",
                ));
            }
            if let Some(receipt) = &installed.admission_receipt {
                crate::store::verify_admission_receipt(receipt, &installed.receipt_sha256)?;
            }
            if install.readmission.is_some() && installed.admission_receipt.is_none() {
                return Err(invalid(
                    "definition readmission lacks its canonical admission receipt",
                ));
            }
            let previous = (head.definitions_version != 0).then_some(head.definitions_version);
            let event = crate::store::definition_event(
                installed.version,
                previous,
                &installed.receipt_sha256,
                &installed.registry,
                &install.diff,
                install.readmission.as_ref(),
            )?;
            let expected = CommitBatch {
                world: crate::MAIN_WORLD.to_owned(),
                scope: self.batch.scope.clone(),
                logical_time: installed.installed_at,
                events: vec![event],
                ..CommitBatch::default()
            };
            if self.batch != expected || self.consumed_input.is_some() {
                return Err(invalid(
                    "typed installation is not the exclusive install commit",
                ));
            }
        } else {
            if body.definitions_version != head.definitions_version {
                return Err(ExportError::ReceiptMismatch);
            }
            if head.head_commit_seq == 0 {
                let snapshot = self
                    .initial_model
                    .as_ref()
                    .ok_or_else(|| invalid("version-zero genesis lacks its typed initial model"))?;
                let model = Model::from_snapshot(snapshot)?;
                if self.model_hash != model.model_hash()
                    || self.ddl_hash != model.ddl_hash()
                    || self.ddl_sql != model.schema().to_sql()
                {
                    return Err(ExportError::IdentityMismatch);
                }
            } else if head.ddl_hash != self.ddl_hash || head.model_hash != self.model_hash {
                return Err(ExportError::IdentityMismatch);
            }
        }
        Ok(())
    }
}

fn increment(seq: u64) -> Result<u64, ExportError> {
    seq.checked_add(1)
        .ok_or_else(|| ExportError::Invalid("export sequence overflow".to_owned()))
}

/// Publication checkpoint; objects before this head form a verified prefix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExportManifest {
    /// Store interpretation at this head; it cannot regress after quantity adoption.
    pub store_format: String,
    /// Export format version.
    pub format: String,
    /// Last committed batch sequence (zero for an empty export).
    pub head_commit_seq: u64,
    /// Content address of the last export object.
    pub head_hash: String,
    /// Last event sequence, independent of the commit sequence.
    pub head_ledger_seq: u64,
    /// Original ledger hash at the last event.
    pub head_ledger_hash: String,
    /// Projected model DDL identity.
    pub ddl_hash: String,
    /// Replay-definition identity, required even when the projected DDL matches.
    pub model_hash: String,
    /// Authoritative definitions version at the head (zero before an install).
    pub definitions_version: u64,
    /// Cell-file identity bindings.
    pub bindings: BTreeMap<String, String>,
    /// Tenant scope of the commit batches.
    pub scope: String,
}

impl Default for ExportManifest {
    fn default() -> Self {
        Self {
            format: EXPORT_FORMAT.to_owned(),
            store_format: STORE_FORMAT.to_owned(),
            head_commit_seq: 0,
            head_hash: GENESIS_HASH.to_owned(),
            head_ledger_seq: 0,
            head_ledger_hash: GENESIS_HASH.to_owned(),
            ddl_hash: String::new(),
            model_hash: String::new(),
            definitions_version: 0,
            bindings: BTreeMap::new(),
            scope: String::new(),
        }
    }
}

impl ExportManifest {
    /// Canonical bytes for the manifest object and durable host checkpoints.
    ///
    /// # Errors
    /// Serialization or canonicalization failed.
    pub fn bytes(&self) -> Result<Vec<u8>, CellError> {
        Ok(canonical_json(self)?.into_bytes())
    }

    /// Decode a remote manifest, treating an absent object as the genesis head.
    ///
    /// # Errors
    /// Invalid JSON, unsupported format, or a noncanonical genesis identity.
    pub fn from_bytes(bytes: Option<&[u8]>) -> Result<Self, ExportError> {
        let manifest = bytes
            .map(serde_json::from_slice::<Self>)
            .transpose()?
            .unwrap_or_default();
        if manifest.format != EXPORT_FORMAT {
            return Err(ExportError::UnsupportedFormat);
        }
        if manifest.head_commit_seq == 0 && manifest != Self::default() {
            return Err(ExportError::ManifestMismatch);
        }
        Ok(manifest)
    }
}

/// Export or independent verification failed.
#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    /// The export or cell-store version is unsupported; no implicit migration.
    #[error("unsupported export or cell-store format")]
    UnsupportedFormat,
    /// commit sequence gap or export hash-chain mismatch.
    #[error("commit sequence gap or export hash-chain mismatch")]
    SequenceMismatch,
    /// export belongs to another model or cell scope.
    #[error("export belongs to another model or cell scope")]
    IdentityMismatch,
    /// commit batch, receipt, or event span mismatch.
    #[error("commit batch, receipt, or event span mismatch")]
    ReceiptMismatch,
    /// ledger event hash-chain mismatch.
    #[error("ledger event hash-chain mismatch")]
    LedgerMismatch,
    /// export genesis mismatch.
    #[error("export genesis mismatch")]
    GenesisMismatch,
    /// manifest head or identity mismatch.
    #[error("manifest head or identity mismatch")]
    ManifestMismatch,
    /// content address mismatch.
    #[error("content address mismatch")]
    ContentAddressMismatch,
    /// Object storage failed; the cell journal remains retryable.
    #[error("object storage: {0}")]
    Storage(String),
    /// Local export directory creation failed.
    #[cfg(feature = "native")]
    #[error("export directory: {0}")]
    Directory(#[from] std::io::Error),
    /// Cell record serialization or reading failed.
    #[error("cell export record: {0}")]
    Cell(#[from] CellError),
    /// Export data could not be decoded.
    #[error("export JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// A hash, sequence, identity, or format failed verification.
    #[error("invalid export: {0}")]
    Invalid(String),
}

/// Host-provided storage for the shared export protocol.
///
/// Keys are relative to one dedicated cell prefix. For create-only writes,
/// return false when the immutable object already exists; replacement writes
/// return true after publishing the new bytes.
#[allow(
    async_fn_in_trait,
    reason = "host storage futures may be isolate-local on wasm; native adapters retain Send futures"
)]
pub trait ExportStorage {
    /// Read an object, returning None when the key is absent.
    ///
    /// # Errors
    /// The host could not read the object or its body.
    #[cfg(target_arch = "wasm32")]
    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, String>;

    /// Read an object with a future suitable for a native export worker.
    ///
    /// # Errors
    /// The host could not read the object or its body.
    #[cfg(not(target_arch = "wasm32"))]
    fn get(
        &self,
        key: &str,
    ) -> impl std::future::Future<Output = Result<Option<Vec<u8>>, String>> + Send;

    /// Publish bytes, optionally requiring that the key does not exist.
    ///
    /// # Errors
    /// The host could not publish the object atomically.
    #[cfg(target_arch = "wasm32")]
    async fn put(&self, key: &str, bytes: Vec<u8>, create_only: bool) -> Result<bool, String>;

    /// Publish bytes with a future suitable for a native export worker.
    ///
    /// # Errors
    /// The host could not publish the object atomically.
    #[cfg(not(target_arch = "wasm32"))]
    fn put(
        &self,
        key: &str,
        bytes: Vec<u8>,
        create_only: bool,
    ) -> impl std::future::Future<Output = Result<bool, String>> + Send;
}

/// Shared content-addressed publisher and verifier over host storage.
///
/// One instance owns one dedicated cell prefix. Callers serialize publication;
/// two writers must never share a prefix. SQLite remains authority: a failed
/// upload leaves the transactional journal available for retry after restart.
pub struct StorageExporter<S> {
    storage: S,
    head: Option<ExportManifest>,
}

impl<S> std::fmt::Debug for StorageExporter<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StorageExporter")
            .field("head", &self.head)
            .finish_non_exhaustive()
    }
}

impl<S> StorageExporter<S> {
    /// Create a publisher for one dedicated cell prefix.
    #[must_use]
    pub fn from_storage(storage: S) -> Self {
        Self {
            storage,
            head: None,
        }
    }

    /// The last checkpoint verified or published by this instance.
    #[must_use]
    pub fn checkpoint(&self) -> Option<&ExportManifest> {
        self.head.as_ref()
    }
}

impl<S: ExportStorage> StorageExporter<S> {
    async fn read_manifest(&self) -> Result<ExportManifest, ExportError> {
        let bytes = self
            .storage
            .get(MANIFEST_PATH)
            .await
            .map_err(ExportError::Storage)?;
        ExportManifest::from_bytes(bytes.as_deref())
    }

    /// Resume publication against the originating cell's trusted commit journal.
    ///
    /// Checks the complete remote manifest and canonical head object against
    /// the exact local committed envelope. This bounds cold host recovery to
    /// two reads. Independent artifact verification and restore must use
    /// [`Self::verify`], which walks the entire exported prefix.
    ///
    /// # Errors
    /// Missing journal records, mismatched identities/heads, or storage failure.
    pub async fn resume_from_journal(
        &mut self,
        record_at: impl FnOnce(u64) -> Result<Option<ExportRecord>, CellError>,
    ) -> Result<ExportManifest, ExportError> {
        self.head = None;
        let manifest = self.read_manifest().await?;
        if manifest.head_commit_seq != 0 {
            let record =
                record_at(manifest.head_commit_seq)?.ok_or(ExportError::SequenceMismatch)?;
            record.validate_manifest(&manifest)?;
            if self.load_record(&manifest.head_hash).await? != record {
                return Err(ExportError::IdentityMismatch);
            }
        }
        self.head = Some(manifest.clone());
        Ok(manifest)
    }

    /// Verify an export without access to the originating cell file.
    ///
    /// Follows content addresses backwards, then verifies batches and the
    /// original ledger forwards from genesis, including zero-event commits.
    /// An unreferenced object from an interrupted upload is safely ignored.
    ///
    /// # Errors
    ///
    /// Missing objects, tampering, gaps, invalid versions, or storage failures.
    pub async fn verify(&mut self) -> Result<ExportManifest, ExportError> {
        self.head = None;
        let manifest = self.read_manifest().await?;
        let mut address = manifest.head_hash.clone();
        let mut expected_seq = manifest.head_commit_seq;
        let mut records = Vec::new();
        while expected_seq != 0 {
            let record = self.load_record(&address).await?;
            if record.receipt.body.commit_seq != expected_seq {
                return Err(ExportError::SequenceMismatch);
            }
            address.clone_from(&record.previous);
            expected_seq = expected_seq
                .checked_sub(1)
                .ok_or_else(|| ExportError::Invalid("export sequence underflow".to_owned()))?;
            records.push(record);
        }
        if address != GENESIS_HASH {
            return Err(ExportError::GenesisMismatch);
        }
        let mut verified = ExportManifest::default();
        for record in records.iter().rev() {
            verified = record.advance(&verified)?;
        }
        if verified != manifest {
            return Err(ExportError::ManifestMismatch);
        }
        self.head = Some(verified.clone());
        Ok(verified)
    }

    /// The active retained set from an independently verified export prefix.
    /// None is a version-zero prefix without a definitions installation.
    ///
    /// # Errors
    /// Integrity, typed installation, sequence or object-storage failure.
    pub async fn active_definitions(
        &mut self,
    ) -> Result<Option<InstalledDefinitionSet>, ExportError> {
        let manifest = self.verify().await?;
        if manifest.definitions_version == 0 {
            return Ok(None);
        }
        let mut address = manifest.head_hash;
        for _ in 0..manifest.head_commit_seq {
            let record = self.load_record(&address).await?;
            if let Some(install) = record.definition_install {
                if install.installed.version != manifest.definitions_version {
                    return Err(ExportError::Invalid(
                        "active exported definitions disagree with the manifest".to_owned(),
                    ));
                }
                return Ok(Some(install.installed));
            }
            address = record.previous;
        }
        Err(ExportError::Invalid(
            "active exported definitions have no installation record".to_owned(),
        ))
    }

    pub(crate) async fn load_record(&self, hash: &str) -> Result<ExportRecord, ExportError> {
        let key = object_key(hash)?;
        let bytes = self
            .storage
            .get(&key)
            .await
            .map_err(ExportError::Storage)?
            .ok_or_else(|| ExportError::Storage(format!("missing object {key}")))?;
        let record: ExportRecord = serde_json::from_slice(&bytes)?;
        if record.hash()? != hash || record.bytes()?.as_slice() != bytes.as_slice() {
            return Err(ExportError::ContentAddressMismatch);
        }
        Ok(record)
    }

    /// Publish one batch, then advance the manifest.
    ///
    /// A retry of a blob already uploaded succeeds only when its bytes match.
    /// Manifest failure never advances the in-memory checkpoint. Restart
    /// verifies the published prefix and reuses any interrupted upload.
    ///
    /// # Errors
    ///
    /// Hash/identity/sequence mismatches or storage failure.
    pub async fn append(&mut self, record: &ExportRecord) -> Result<ExportManifest, ExportError> {
        let head = match &self.head {
            Some(head) => head.clone(),
            None => self.verify().await?,
        };
        let hash = record.hash()?;
        if record.receipt.body.commit_seq == head.head_commit_seq && hash == head.head_hash {
            // Revalidate the stored bytes on a retried acknowledgement.
            self.load_record(&hash).await?;
            return Ok(head);
        }
        let next = record.advance(&head)?;
        let bytes = record.bytes()?;
        let key = object_key(&hash)?;
        let created = self
            .storage
            .put(&key, bytes.clone(), true)
            .await
            .map_err(ExportError::Storage)?;
        if !created {
            let existing = self
                .storage
                .get(&key)
                .await
                .map_err(ExportError::Storage)?
                .ok_or_else(|| ExportError::Storage(format!("missing existing object {key}")))?;
            if existing.as_slice() != bytes.as_slice() {
                return Err(ExportError::Invalid(
                    "immutable export object differs".to_owned(),
                ));
            }
        }
        let manifest = next.bytes()?;
        let published = self
            .storage
            .put(MANIFEST_PATH, manifest, false)
            .await
            .map_err(ExportError::Storage)?;
        if !published {
            return Err(ExportError::Storage(
                "manifest was not published".to_owned(),
            ));
        }
        tracing::debug!(commit_seq = next.head_commit_seq, hash = %next.head_hash, "cell commit exported");
        self.head = Some(next.clone());
        Ok(next)
    }
}

/// Adapter that preserves `object_store` support for native development hosts.
#[cfg(feature = "native")]
#[derive(Debug)]
pub struct NativeExportStorage {
    storage: Arc<dyn ObjectStore>,
}

#[cfg(feature = "native")]
impl ExportStorage for NativeExportStorage {
    async fn get(&self, key: &str) -> Result<Option<Vec<u8>>, String> {
        match self.storage.get(&ObjectPath::from(key)).await {
            Ok(result) => result
                .bytes()
                .await
                .map(|bytes| Some(bytes.to_vec()))
                .map_err(|error| error.to_string()),
            Err(object_store::Error::NotFound { .. }) => Ok(None),
            Err(error) => Err(error.to_string()),
        }
    }

    async fn put(&self, key: &str, bytes: Vec<u8>, create_only: bool) -> Result<bool, String> {
        let path = ObjectPath::from(key);
        if create_only {
            let options = PutOptions {
                mode: PutMode::Create,
                ..PutOptions::default()
            };
            match self.storage.put_opts(&path, bytes.into(), options).await {
                Ok(_) => Ok(true),
                Err(object_store::Error::AlreadyExists { .. }) => Ok(false),
                Err(error) => Err(error.to_string()),
            }
        } else {
            self.storage
                .put(&path, bytes.into())
                .await
                .map(|_| true)
                .map_err(|error| error.to_string())
        }
    }
}

/// Native development/test publisher backed by `object_store`.
#[cfg(feature = "native")]
pub type Exporter = StorageExporter<NativeExportStorage>;

#[cfg(feature = "native")]
impl StorageExporter<NativeExportStorage> {
    /// Use a preconfigured backend in a dedicated, single-writer cell prefix.
    #[must_use]
    pub fn new(storage: Arc<dyn ObjectStore>) -> Self {
        Self::from_storage(NativeExportStorage { storage })
    }

    /// Local-filesystem backend, with atomic publication and file sync enabled.
    ///
    /// # Errors
    ///
    /// Directory creation or object-store initialization failed.
    pub fn local(path: &Path) -> Result<Self, ExportError> {
        std::fs::create_dir_all(path)?;
        let storage = LocalFileSystem::new_with_prefix(path)
            .map_err(|error| ExportError::Storage(error.to_string()))?
            .with_fsync(true);
        Ok(Self::new(Arc::new(storage)))
    }
}

/// Object key for a validated lower-case Norn1 SHA-256 content address.
///
/// # Errors
///
/// The address is not exactly 64 lower-case hexadecimal characters.
pub fn object_key(hash: &str) -> Result<String, ExportError> {
    if hash.len() != 64
        || !hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(ExportError::Invalid("invalid content address".to_owned()));
    }
    Ok(format!("commits/{hash}.json"))
}

/// Native `object_store` path compatibility helper.
///
/// # Errors
///
/// The address is not exactly 64 lower-case hexadecimal characters.
#[cfg(feature = "native")]
pub fn object_path(hash: &str) -> Result<ObjectPath, ExportError> {
    object_key(hash).map(ObjectPath::from)
}
