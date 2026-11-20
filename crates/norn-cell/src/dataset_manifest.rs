//! MANIFEST-1 (#1381): a bulk dataset is governed as one manifest, not row by row.
//!
//! A load lands as columnar files in object storage (the data file and the
//! held-set file, MANIFEST-LAND-1). The cell commits one [`DatasetManifest`]
//! naming those files by content hash, together with what typed them:
//! the source upload, the column binding, the definitions version, model,
//! table and law-set pins, the counts and the valid-time range. That is one
//! governed command with one receipt per dataset. Per-row writes are
//! unchanged and stay the path for actions and corrections.
//!
//! - MANIFEST-COMMIT-1: a manifest commits as exactly one ledger event
//!   ([`MANIFEST_EVENT`]) under the idempotency key `dataset:<hash>`. A
//!   retry of the same manifest returns the first receipt, and nothing else
//!   is written.
//! - MANIFEST-PIN-1: a manifest whose pins are not the cell's current
//!   definitions version, model, table typing and law set is refused
//!   (`dataset.stale_pins`). A dataset is never admitted under definitions
//!   it was not typed and checked by.
//! - MANIFEST-CURRENT-1: the current dataset of a type in a world is the
//!   latest manifest event committed to that world. It is read from the
//!   ledger, so export, restore and replay reproduce it with no side table.
//! - MANIFEST-CACHE-1: the engine table that answers questions is a cache of
//!   the data file. It is keyed by the landing identity the manifest binds,
//!   and can be rebuilt from the files, which are checked by hash.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::analytics::{FileFormat, RuleCount, TableSchema, ValidRange};
use crate::engine::{SqlEngine, params};
use crate::system::LEDGER;
use crate::{CellError, CellStore, CommitBatch, GuardedError, Receipt};

/// The ledger event type of a committed manifest.
pub const MANIFEST_EVENT: &str = "dataset.manifest";
/// The manifest format.
pub const MANIFEST_VERSION: &str = "norn.dataset-manifest/1";
/// The commitment domain of a manifest's identity.
const DOMAIN: &[u8] = b"norn.dataset-manifest.v1\0";
/// The commitment domain of a type's law-set pin.
const LAWS_DOMAIN: &[u8] = b"norn.dataset-manifest.laws.v1\0";
/// The commitment domain of a column binding's pin.
const BINDING_DOMAIN: &[u8] = b"norn.dataset-manifest.binding.v1\0";

/// The source file a dataset was loaded from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManifestSource {
    /// The upload's durable URI.
    pub uri: String,
    /// Its SHA-256, as the cell recorded it and the engine verified it.
    pub sha256: String,
    /// Its format.
    pub format: FileFormat,
    /// Its size in bytes.
    pub bytes: u64,
}

/// One landed file, named by content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManifestFile {
    /// The object's durable URI.
    pub uri: String,
    /// Its SHA-256.
    pub sha256: String,
    /// Rows in it.
    pub rows: u64,
    /// Bytes in it.
    pub bytes: u64,
}

/// What a dataset was typed and checked by (MANIFEST-PIN-1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManifestPins {
    /// The admitted definitions version.
    pub definitions_version: u64,
    /// The model's Norn1 hash.
    pub model_hash: String,
    /// The column binding's pin (its mapping onto the type).
    pub binding: String,
    /// The pin of the laws and invariants that govern the type.
    pub laws: String,
}

/// Row counts of a dataset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManifestCounts {
    /// Rows the source file holds.
    pub source: u64,
    /// Rows admitted to the data file.
    pub admitted: u64,
    /// Rows held back before admission (not typed), by reason.
    pub held: BTreeMap<String, u64>,
    /// MANIFEST-LAWS-1: findings by rule, over the admitted rows.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub findings: BTreeMap<String, RuleCount>,
    /// Admitted rows a rule holds back; every other admitted row is trusted.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub held_by_rules: u64,
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if passes a reference"
)]
const fn is_zero(value: &u64) -> bool {
    *value == 0
}

/// MANIFEST-1: one dataset, governed as a whole.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DatasetManifest {
    /// [`MANIFEST_VERSION`].
    pub version: String,
    /// The admitted object type the rows are typed by.
    pub object_type: String,
    /// The landing identity: the engine cache key (MANIFEST-CACHE-1).
    pub load: String,
    /// The source upload.
    pub source: ManifestSource,
    /// The file column for each property named differently; any other
    /// property is read by its id.
    pub binding: BTreeMap<String, String>,
    /// The typing: the type's columns and their pointwise constraints.
    pub table: TableSchema,
    /// The pins it was typed and checked by.
    pub pins: ManifestPins,
    /// The data file: every admitted row and its source row offset.
    pub data: ManifestFile,
    /// The held-set file: each held-back source row and its reason.
    pub held: ManifestFile,
    /// Row counts.
    pub counts: ManifestCounts,
    /// Each temporal column's range over the admitted rows.
    pub valid_time: Vec<ValidRange>,
}

impl DatasetManifest {
    /// The manifest's identity: SHA-256 over its canonical JSON.
    ///
    /// # Errors
    /// The manifest cannot be canonicalized.
    pub fn hash(&self) -> Result<String, CellError> {
        norn_ir::canonical::domain_hash_hex(DOMAIN, self)
            .map_err(|error| CellError::Encoding(error.to_string()))
    }
}

/// The ledger event of a committed manifest.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ManifestEvent<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    object_type: &'a str,
    hash: &'a str,
    manifest: &'a DatasetManifest,
}

/// A manifest as committed: its identity and where it sits in the ledger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommittedManifest {
    /// The manifest.
    pub manifest: DatasetManifest,
    /// Its identity.
    pub hash: String,
    /// The commit that admitted it.
    pub commit_seq: u64,
}

/// The pin of a column binding.
///
/// # Errors
/// The binding cannot be canonicalized.
pub fn binding_pin(binding: &BTreeMap<String, String>) -> Result<String, CellError> {
    norn_ir::canonical::domain_hash_hex(BINDING_DOMAIN, binding)
        .map_err(|error| CellError::Encoding(error.to_string()))
}

fn refused(code: &'static str, detail: impl Into<String>) -> CellError {
    CellError::DatasetRefused {
        code,
        detail: detail.into(),
    }
}

impl<E: SqlEngine> CellStore<E> {
    /// MANIFEST-PIN-1: the pin of every law and invariant that governs
    /// `object_type` under the current definitions.
    ///
    /// # Errors
    /// An unknown type, or a law whose scope cannot be resolved.
    pub fn dataset_laws_pin(&self, object_type: &str) -> Result<String, CellError> {
        let laws = self.model().governing_laws(object_type)?;
        let invariants = self.model().effective_invariants(object_type)?;
        norn_ir::canonical::domain_hash_hex(LAWS_DOMAIN, &(laws, invariants))
            .map_err(|error| CellError::Encoding(error.to_string()))
    }

    /// The pins a dataset of `object_type` is typed and checked by now.
    ///
    /// # Errors
    /// As [`Self::dataset_laws_pin`], or a store error.
    pub fn dataset_pins(
        &self,
        object_type: &str,
        binding: &BTreeMap<String, String>,
    ) -> Result<ManifestPins, CellError> {
        Ok(ManifestPins {
            definitions_version: self.read().active_definitions_version()?.unwrap_or(0),
            model_hash: self.model().model_hash().to_owned(),
            binding: binding_pin(binding)?,
            laws: self.dataset_laws_pin(object_type)?,
        })
    }

    /// MANIFEST-COMMIT-1: commit `manifest` to `world` as one governed
    /// command and return its identity and receipt. The pins must be the
    /// current ones (MANIFEST-PIN-1). A retry of the same manifest returns
    /// the first receipt.
    ///
    /// # Errors
    /// `dataset.invalid` for a manifest that is not well formed,
    /// `dataset.stale_pins` for pins that are not current, or a store error.
    pub fn commit_dataset_manifest(
        &mut self,
        world: &str,
        scope: &str,
        logical_time: u64,
        manifest: &DatasetManifest,
    ) -> Result<(String, Receipt), CellError> {
        if manifest.version != MANIFEST_VERSION
            || manifest.table.name != manifest.object_type
            || manifest.counts.admitted != manifest.data.rows
            || manifest
                .counts
                .held
                .values()
                .chain(manifest.counts.findings.values().map(|count| &count.rows))
                .sum::<u64>()
                != manifest.held.rows
            || manifest.counts.held_by_rules > manifest.counts.admitted
        {
            return Err(refused(
                "dataset.invalid",
                "the manifest is not well formed",
            ));
        }
        let current_table = self.analytics_table(&manifest.object_type)?;
        let current = self.dataset_pins(&manifest.object_type, &manifest.binding)?;
        if current_table != manifest.table
            || current.model_hash != manifest.pins.model_hash
            || current.binding != manifest.pins.binding
            || current.laws != manifest.pins.laws
        {
            return Err(refused(
                "dataset.stale_pins",
                "the definitions changed since the dataset was typed",
            ));
        }
        let hash = manifest.hash()?;
        let key = format!("dataset:{hash}");
        // A retry of the same manifest is the same request: it keeps the
        // first commit's logical time, so it replays rather than conflicts.
        let logical_time = self
            .read()
            .receipt_by_idempotency_key(world, &key)?
            .map_or(logical_time, |receipt| receipt.body.logical_time);
        let event = norn_ir::runtime::encode(&ManifestEvent {
            kind: MANIFEST_EVENT,
            object_type: &manifest.object_type,
            hash: &hash,
            manifest,
        })
        .map_err(CellError::Encoding)?;
        let batch = CommitBatch {
            world: world.to_owned(),
            scope: scope.to_owned(),
            logical_time,
            idempotency_key: Some(key),
            events: vec![event],
            ..CommitBatch::default()
        };
        let pinned = manifest.pins.definitions_version;
        // The definitions version is re-read inside the commit transaction:
        // an install between the check above and this commit refuses.
        let guarded = self.commit_guarded(
            &batch,
            |reader| {
                if reader.active_definitions_version()?.unwrap_or(0) == pinned {
                    Ok(())
                } else {
                    Err(refused(
                        "dataset.stale_pins",
                        "the definitions changed since the dataset was typed",
                    ))
                }
            },
            &mut |_point| Ok(()),
        );
        match guarded {
            Ok(((), outcome)) => Ok((hash, outcome.receipt().clone())),
            Err(GuardedError::Guard(error) | GuardedError::Store(error)) => Err(error),
        }
    }

    /// MANIFEST-CURRENT-1: the current dataset of `object_type` in `world`.
    ///
    /// # Errors
    /// A store error or an undecodable manifest event.
    pub fn dataset_manifest(
        &self,
        world: &str,
        object_type: &str,
    ) -> Result<Option<CommittedManifest>, CellError> {
        let found = self.conn.query_optional(
            &format!(
                "SELECT \"event\", \"commit_seq\" FROM \"{LEDGER}\" \
                 WHERE \"world\" = ?1 AND json_extract(\"event\", '$.type') = ?2 \
                 AND json_extract(\"event\", '$.objectType') = ?3 \
                 ORDER BY \"seq\" DESC LIMIT 1"
            ),
            params![world, MANIFEST_EVENT, object_type],
            |row| Ok((row.get::<String>(0)?, row.get::<i64>(1)?)),
        )?;
        let Some((text, commit_seq)) = found else {
            return Ok(None);
        };
        let commit_seq =
            u64::try_from(commit_seq).map_err(|_negative| CellError::LedgerCorrupt {
                seq: 0,
                reason: "a negative commit sequence".to_owned(),
            })?;
        let event: Value = serde_json::from_str(&text)?;
        let manifest: DatasetManifest =
            serde_json::from_value(event.get("manifest").cloned().unwrap_or(Value::Null))?;
        let hash = manifest.hash()?;
        if event.get("hash").and_then(Value::as_str) != Some(hash.as_str()) {
            return Err(CellError::LedgerCorrupt {
                seq: commit_seq,
                reason: "a dataset manifest event does not match its hash".to_owned(),
            });
        }
        Ok(Some(CommittedManifest {
            manifest,
            hash,
            commit_seq,
        }))
    }
}
