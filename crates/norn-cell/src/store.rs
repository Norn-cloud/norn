//! The normalized SQLite-format cell store over a host-provided [`SqlEngine`]
//! (T2-ST-1 #2540, T2-ST-2 #2541).
//!
//! Objects and links live in the tables the DDL projection derives from the
//! object model ([`Model`]); events, receipts, the command outbox, timers,
//! the observation inbox, `perform` runs, and version high-water marks live in the
//! system tables ([`crate::system`]). One [`CellStore::commit`] is one engine
//! transaction: the consumed delivery, object
//! writes and fact changes, link changes, ledger events, outbox and timer
//! effects, and the receipt either all land or none do. A crash anywhere
//! before the transaction commits leaves the store as it was; reopening verifies the ledger
//! hash chain and every receipt ([`StoreReader::verify_ledger`]).
//!
//! The store makes no durability claim; its host engine owns those guarantees.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
#[cfg(feature = "native")]
use std::path::Path;
#[cfg(feature = "native")]
use std::sync::Arc;

use norn_ir::{FactChange, RuntimeValue as Value};
use serde::{Deserialize, Serialize};
#[cfg(feature = "native")]
use tokio::sync::Notify;

use crate::codec::{canonical_json, hash_json};
#[cfg(feature = "native")]
use crate::engine::NativeEngine;
use crate::engine::{DefaultEngine, SqlEngine, params};
use crate::error::CellError;
use crate::export_journal;
use crate::model::{Model, ModelDefinitionSnapshot};
use crate::read::{
    LedgerHead, StoreReader, active_definition_version, definition_by_version, next,
    receipt_by_key, to_i64, to_u64, validate_active_definition_pointer,
};
use crate::system::{
    DEFINITION_ACTIVE, DEFINITION_ADMISSION_RECEIPT, DEFINITION_LAYOUT, DEFINITION_REGISTRY,
    DEFINITION_VERSION, INBOX, MARKING_CURRENT, MARKING_SNAPSHOT, MARKING_VERSION, META, OUTBOX,
    RUN, STORE_FORMAT, SYSTEM_DDL, ensure_marking_subject, marking_subject,
};
use crate::write::Writer;

/// The main world's id in the `_world` columns.
pub const MAIN_WORLD: &str = "main";

/// Do not partially update classification state when an install would need
/// to touch an unbounded number of retained fact versions. The transaction
/// rolls back and the caller may schedule a separately bounded backfill.
const MARKING_INSTALL_BACKFILL_LIMIT: usize = 4_096;

type SourceMarkingKey = (String, String, String);

fn source_field_markings(
    canonical_set: &str,
) -> Result<BTreeMap<SourceMarkingKey, BTreeSet<String>>, CellError> {
    let artifacts: Vec<serde_json::Value> =
        serde_json::from_str(canonical_set).map_err(CellError::Json)?;
    let mut markings = BTreeMap::<SourceMarkingKey, BTreeSet<String>>::new();
    for artifact in artifacts.iter().filter(|artifact| {
        artifact.get("kind").and_then(serde_json::Value::as_str) == Some("source_schema")
    }) {
        let schema_id = artifact
            .get("artifactId")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                CellError::DefinitionMigrationRefused(
                    "source schema has no artifact identity".to_owned(),
                )
            })?;
        let payload = artifact.get("payload").ok_or_else(|| {
            CellError::DefinitionMigrationRefused("source schema has no typed payload".to_owned())
        })?;
        let schema =
            norn_checker::source_schema::decode_source_schema(payload).map_err(|findings| {
                CellError::DefinitionMigrationRefused(format!(
                    "source schema classification contract is unavailable: {findings:?}"
                ))
            })?;
        for field in schema.fields {
            markings
                .entry((schema_id.to_owned(), field.dataset, field.name))
                .or_default()
                .extend(field.markings.into_iter().map(|marking| marking.0));
        }
    }
    Ok(markings)
}

fn preflight_mask_policy_changes(previous: Option<&str>, candidate: &str) -> Result<(), CellError> {
    let catalog = |encoded: &str| {
        let artifacts: Vec<norn_ir::ArtifactEnvelope> =
            serde_json::from_str(encoded).map_err(CellError::Json)?;
        Ok::<_, CellError>(norn_checker::marking_catalog_from_artifacts(&artifacts))
    };
    let Some(previous) = previous else {
        return Ok(());
    };
    let old = catalog(previous)?;
    let new = catalog(candidate)?;
    for (id, marking) in old {
        let weakens = match new.get(&id).map(|marking| &marking.mask) {
            None => true,
            Some(norn_ir::MaskPolicy::LastN { count }) => match marking.mask {
                norn_ir::MaskPolicy::LastN { count: old_count } => *count > old_count,
                norn_ir::MaskPolicy::Redacted {} | norn_ir::MaskPolicy::Null {} => true,
            },
            Some(norn_ir::MaskPolicy::Redacted {} | norn_ir::MaskPolicy::Null {}) => false,
        };
        if weakens {
            return Err(CellError::DefinitionMigrationRefused(format!(
                "install removes or weakens mask policy for `{}`; declassify the exact fact versions instead",
                id.as_str()
            )));
        }
    }
    Ok(())
}

fn preflight_source_marking_changes<E: SqlEngine>(
    engine: &E,
    previous: Option<&str>,
    candidate: &str,
) -> Result<(), CellError> {
    let old = match previous {
        Some(previous) => source_field_markings(previous)?,
        None => BTreeMap::new(),
    };
    let new = source_field_markings(candidate)?;
    for ((schema, dataset, field), labels) in &old {
        for marking in labels {
            if !new
                .get(&(schema.clone(), dataset.clone(), field.clone()))
                .is_some_and(|labels| labels.contains(marking))
            {
                return Err(CellError::DefinitionMigrationRefused(format!(
                    "install removes source-derived marking `{marking}` from `{schema}/{dataset}/{field}`; declassify the exact fact versions instead"
                )));
            }
        }
    }
    let addition = new.iter().find_map(|(key, labels)| {
        labels
            .iter()
            .find(|marking| !old.get(key).is_some_and(|old| old.contains(*marking)))
            .map(|marking| (key, marking))
    });
    if let Some(((schema, dataset, field), marking)) = addition {
        // Retained snapshots do not index source-field dependencies. Updating
        // only the next observation would leave earlier derived values public;
        // refuse until a bounded backfill can prove all affected versions.
        let retained: Option<i64> = engine.query_optional(
            &format!("SELECT 1 FROM \"{MARKING_SNAPSHOT}\" LIMIT 1"),
            params![],
            |row| row.get(0),
        )?;
        if retained.is_some() {
            return Err(CellError::DefinitionMigrationRefused(format!(
                "install adds source-derived marking `{marking}` to `{schema}/{dataset}/{field}` with retained fact versions; refusing an incomplete lineage backfill"
            )));
        }
    }
    Ok(())
}

fn preflight_marking_definition_changes<E: SqlEngine>(
    engine: &E,
    previous: &Model,
    candidate: &Model,
) -> Result<BTreeMap<(String, String), Vec<String>>, CellError> {
    let mut additions = BTreeMap::<(String, String), Vec<String>>::new();
    for old_type in previous.object_types() {
        let object_type = old_type.name.as_str().to_owned();
        if !candidate.has_type(&object_type) {
            continue;
        }
        let old = previous.definition_markings(&object_type)?;
        let new = candidate.definition_markings(&object_type)?;
        for (property, old_markings) in old {
            let new_markings = new.get(&property).cloned().unwrap_or_default();
            for marking in &old_markings {
                if !new_markings.contains(marking) {
                    return Err(CellError::DefinitionMigrationRefused(format!(
                        "install removes effective marking `{marking}` from `{object_type}.{property}`; declassify the exact fact versions instead"
                    )));
                }
            }
        }
    }
    for new_type in candidate.object_types() {
        let object_type = new_type.name.as_str().to_owned();
        let new = candidate.definition_markings(&object_type)?;
        let old = if previous.has_type(&object_type) {
            previous.definition_markings(&object_type)?
        } else {
            BTreeMap::new()
        };
        let old_computed = if previous.has_type(&object_type) {
            previous.computed_property_inputs(&object_type)?
        } else {
            BTreeMap::new()
        };
        let new_computed = candidate.computed_property_inputs(&object_type)?;
        for (property, new_markings) in new {
            let old_markings = old.get(&property).cloned().unwrap_or_default();
            let added = new_markings
                .into_iter()
                .filter(|marking| !old_markings.contains(marking))
                .collect::<Vec<_>>();
            if !added.is_empty()
                || !old.contains_key(&property)
                || new_computed
                    .get(&property)
                    .is_some_and(|inputs| old_computed.get(&property) != Some(inputs))
            {
                additions.insert((object_type.clone(), property), added);
            }
        }
    }

    for (object_type, property) in additions.keys() {
        for current in [true, false] {
            let count: i64 = engine.query_optional(
                &format!(
                    "SELECT COUNT(*) FROM (SELECT 1 FROM \"{MARKING_SNAPSHOT}\" WHERE \"object_type\" = ?1 AND \"is_current\" = ?2 LIMIT ?3)"
                ),
                params![object_type, i64::from(current), i64::try_from(MARKING_INSTALL_BACKFILL_LIMIT + 1).map_err(|_conversion_error| CellError::OutOfRange("marking backfill limit"))?],
                |row| row.get(0),
            )?.unwrap_or(0);
            if count
                > i64::try_from(MARKING_INSTALL_BACKFILL_LIMIT)
                    .map_err(|_conversion_error| CellError::OutOfRange("marking backfill limit"))?
            {
                return Err(CellError::DefinitionMigrationRefused(format!(
                    "adding a marking to `{object_type}.{property}` requires updating more than {MARKING_INSTALL_BACKFILL_LIMIT} retained snapshots; refusing an incomplete backfill"
                )));
            }
        }
    }
    Ok(additions)
}

fn apply_marking_definition_changes<E: SqlEngine>(
    engine: &E,
    previous: &Model,
    candidate: &Model,
) -> Result<(), CellError> {
    let additions = preflight_marking_definition_changes(engine, previous, candidate)?;
    let changed_types = additions
        .keys()
        .map(|(object_type, _)| object_type.clone())
        .collect::<BTreeSet<_>>();
    // New declarations classify all retained versions, not only the next
    // write. Otherwise a definition install could make an old raw value
    // readable under the new catalog. The bounded scan refuses the whole
    // install before publishing an incomplete classification update.
    for ((object_type, property), added) in additions {
        backfill_marking_table(
            engine,
            MARKING_SNAPSHOT,
            MARKING_CURRENT,
            &object_type,
            &property,
            &added,
            true,
        )?;
        backfill_marking_table(
            engine,
            MARKING_SNAPSHOT,
            MARKING_VERSION,
            &object_type,
            &property,
            &added,
            false,
        )?;
    }
    for object_type in changed_types {
        backfill_computed_markings(engine, candidate, &object_type)?;
    }
    Ok(())
}

fn backfill_computed_markings<E: SqlEngine>(
    engine: &E,
    candidate: &Model,
    object_type: &str,
) -> Result<(), CellError> {
    if candidate.computed_property_inputs(object_type)?.is_empty() {
        return Ok(());
    }
    let rows = engine.query(
        &format!("SELECT \"world\", \"id\", \"version\", \"is_current\" FROM \"{MARKING_SNAPSHOT}\" WHERE \"object_type\" = ?1 ORDER BY \"world\", \"id\", \"version\" LIMIT ?2"),
        params![object_type, i64::try_from(2 * MARKING_INSTALL_BACKFILL_LIMIT + 1).map_err(|_conversion_error| CellError::OutOfRange("marking backfill limit"))?],
    )?;
    if rows.len() > 2 * MARKING_INSTALL_BACKFILL_LIMIT {
        return Err(CellError::DefinitionMigrationRefused(
            "computed marking backfill exceeds retained snapshot budget".to_owned(),
        ));
    }
    let reader = StoreReader::new(engine, candidate);
    for raw in rows {
        let row = crate::engine::SqlRow(raw);
        let world: String = row.get(0)?;
        let id: String = row.get(1)?;
        let version: i64 = row.get(2)?;
        let current = row.get::<i64>(3)? == 1;
        let selected = if current {
            None
        } else {
            Some(to_u64(version, "marking snapshot version")?)
        };
        let Some(snapshot) = reader.effective_markings(&world, object_type, &id, selected)? else {
            // Existing incomplete snapshots continue to fail closed.
            continue;
        };
        let mut effective = snapshot
            .into_iter()
            .map(|(property, labels)| (property, labels.into_iter().collect::<BTreeSet<_>>()))
            .collect::<BTreeMap<_, _>>();
        let changed = candidate.close_computed_markings(object_type, &mut effective)?;
        let table = if current {
            MARKING_CURRENT
        } else {
            MARKING_VERSION
        };
        for property in changed {
            let labels = effective.get(&property).ok_or_else(|| {
                CellError::Engine("computed classification target is missing".to_owned())
            })?;
            let encoded = serde_json::to_string(labels).map_err(CellError::Json)?;
            engine.execute(
                &format!("UPDATE \"{table}\" SET \"markings\" = ?1 WHERE \"world\" = ?2 AND \"object_type\" = ?3 AND \"subject\" = ?4 AND \"property\" = ?5 AND \"version\" = ?6"),
                params![encoded, &world, object_type, marking_subject(engine, &world, object_type, &id)?, property, version],
            )?;
        }
    }
    Ok(())
}

fn backfill_marking_table<E: SqlEngine>(
    engine: &E,
    snapshots: &str,
    values: &str,
    object_type: &str,
    property: &str,
    additions: &[String],
    current: bool,
) -> Result<(), CellError> {
    let is_current = i64::from(current);
    let rows = engine.query(
        &format!(
            "SELECT \"world\", \"id\", \"version\" FROM \"{snapshots}\" WHERE \"object_type\" = ?1 AND \"is_current\" = ?2 ORDER BY \"world\", \"id\" LIMIT ?3"
        ),
        params![object_type, is_current, i64::try_from(MARKING_INSTALL_BACKFILL_LIMIT + 1).map_err(|_conversion_error| CellError::OutOfRange("marking backfill limit"))?],
    )?;
    if rows.len() > MARKING_INSTALL_BACKFILL_LIMIT {
        return Err(CellError::DefinitionMigrationRefused(format!(
            "adding a marking to `{object_type}.{property}` requires updating more than {MARKING_INSTALL_BACKFILL_LIMIT} retained snapshots; refusing an incomplete backfill"
        )));
    }
    for raw in rows {
        let row = crate::engine::SqlRow(raw);
        let world: String = row.get(0)?;
        let id: String = row.get(1)?;
        let version: i64 = row.get(2)?;
        let subject = ensure_marking_subject(engine, &world, object_type, &id)?;
        let existing = engine.query_optional(
            &format!(
                "SELECT \"markings\" FROM \"{values}\" WHERE \"world\" = ?1 AND \"object_type\" = ?2 AND \"subject\" = ?3 AND \"property\" = ?4 AND \"version\" = ?5"
            ),
            params![&world, object_type, subject, property, version],
            |row| row.get::<String>(0),
        )?;
        let mut markings = existing
            .map(|encoded| serde_json::from_str::<Vec<String>>(&encoded).map_err(CellError::Json))
            .transpose()?
            .unwrap_or_default();
        for marking in additions {
            if !markings.contains(marking) {
                markings.push(marking.clone());
            }
        }
        markings.sort();
        let encoded = serde_json::to_string(&markings).map_err(CellError::Json)?;
        let sql = if current {
            format!(
                "INSERT INTO \"{values}\" (\"world\", \"object_type\", \"subject\", \"property\", \"version\", \"markings\") VALUES (?1, ?2, ?3, ?4, ?5, ?6) ON CONFLICT (\"world\", \"object_type\", \"subject\", \"property\") DO UPDATE SET \"version\" = excluded.\"version\", \"markings\" = excluded.\"markings\""
            )
        } else {
            format!(
                "INSERT INTO \"{values}\" (\"world\", \"object_type\", \"subject\", \"property\", \"version\", \"markings\") VALUES (?1, ?2, ?3, ?4, ?5, ?6) ON CONFLICT (\"world\", \"object_type\", \"subject\", \"property\", \"version\") DO UPDATE SET \"markings\" = excluded.\"markings\""
            )
        };
        engine.execute(
            &sql,
            params![&world, object_type, subject, property, version, encoded],
        )?;
    }
    Ok(())
}

/// Largest canonical observation admitted to the inbox, including provenance.
/// Larger source payloads must be carried by an immutable blob reference.
pub const MAX_OBSERVATION_BYTES: usize = 2 * 1024 * 1024;

/// SQLite journal mode of a file-backed store.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum JournalMode {
    /// Rollback journal (`DELETE`), SQLite's default.
    #[default]
    Delete,
    /// Write-ahead log.
    Wal,
}

/// How a file-backed store is opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StoreOptions {
    /// The journal mode.
    pub journal_mode: JournalMode,
    /// The native page cache size in pages. A small cache makes
    /// SQLite spill uncommitted pages to the file mid-transaction, which the
    /// crash simulations use to exercise hot-journal and WAL recovery.
    pub cache_pages: Option<u32>,
}

/// One retained, checker-admitted definition set installed in the cell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstalledDefinitionSet {
    /// Monotonically increasing per-cell version.
    pub version: u64,
    /// Admission receipt digest supplied by the shared checker.
    pub receipt_sha256: String,
    /// Canonical full checker admission receipt, if this version was written
    /// by a host that retains receipt bodies.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admission_receipt: Option<String>,
    /// Canonical full artifact-list JSON, encoded as UTF-8 text.
    pub canonical_set: String,
    /// Exact package@version selections used to resolve this definition set.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub registry: Vec<String>,
    /// Projected-model hash, including defaults and lifecycle semantics.
    pub model_hash: String,
    /// Projected SQLite DDL hash.
    pub ddl_hash: String,
    /// Host logical timestamp in milliseconds.
    pub installed_at: u64,
    /// The commit sequence that installed this version.
    pub commit_seq: u64,
    /// Canonical versioned editor layout, retained independently of semantic
    /// artifacts. Missing on versions predating layout persistence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<String>,
    /// Monotonic editor-layout revision. It changes only when `layout` does.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub layout_revision: u64,
}

/// Input to one runtime definition-set installation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InstalledDefinitionInput {
    /// Canonical full artifact-list JSON, encoded as UTF-8 text.
    pub canonical_set: String,
    /// Exact package@version selections used to resolve this definition set.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub registry: Vec<String>,
    /// Admission receipt digest supplied by the shared checker.
    pub receipt_sha256: String,
    /// Canonical full checker admission receipt to retain with this version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admission_receipt: Option<String>,
    /// Present only when this is an admission-profile readmission of the
    /// unchanged definition set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readmission: Option<DefinitionReadmission>,
    /// Bounded added/removed/changed/narrowing summary for the ledger event.
    pub diff: serde_json::Value,
    /// Host logical timestamp in milliseconds.
    pub installed_at: u64,
    /// Canonical sidecar bytes. `None` inherits the exact previous snapshot;
    /// a present empty sidecar explicitly resets it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<String>,
}

/// Checker/profile transition recorded when unchanged definitions are
/// re-admitted after a checker or expression-profile upgrade.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DefinitionReadmission {
    /// Receipt digest retained by the preceding definitions version.
    pub previous_receipt_sha256: String,
    /// Checker version recorded by the preceding receipt.
    pub previous_checker_version: String,
    /// Expression profile recorded by the preceding receipt.
    pub previous_expr_profile: String,
    /// Checker version that admitted this readmission.
    pub checker_version: String,
    /// Expression profile that admitted this readmission.
    pub expr_profile: String,
    /// The only permitted canonical-set change during readmission: removal of
    /// editor-only fields that have been extracted into the layout sidecar.
    #[serde(default, skip_serializing_if = "is_false")]
    pub layout_migration: bool,
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde skip_serializing_if requires a borrowed field"
)]
fn is_false(value: &bool) -> bool {
    !value
}

/// The typed install data retained in a commit export and replayed on restore.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DefinitionInstallRecord {
    /// The immutable history row installed by this commit.
    pub installed: InstalledDefinitionSet,
    /// The entity projection admitted with the full artifact set.
    pub model: ModelDefinitionSnapshot,
    /// The bounded install diff in the corresponding ledger event.
    pub diff: serde_json::Value,
    /// Checker/profile transition, when this commit re-admitted an unchanged
    /// set under a newer checker or profile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readmission: Option<DefinitionReadmission>,
}

/// Construct the exact ledger event emitted for a typed definition install.
/// Export verification uses this same representation so ordinary business
/// events cannot masquerade as definition-install commits.
pub(crate) fn definition_installed_event(
    version: u64,
    previous_version: Option<u64>,
    receipt_sha256: &str,
    registry: &[String],
    diff: &serde_json::Value,
) -> Result<Value, CellError> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Event<'a> {
        #[serde(rename = "type")]
        kind: &'static str,
        version: u64,
        previous_version: Option<u64>,
        receipt_sha256: &'a str,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        registry: Vec<String>,
        diff: &'a serde_json::Value,
    }

    norn_ir::runtime::encode(&Event {
        kind: "definitions.installed",
        version,
        previous_version,
        receipt_sha256,
        registry: registry.to_vec(),
        diff,
    })
    .map_err(CellError::Encoding)
}

/// Construct the exact ledger event for either a new set or a readmission.
pub(crate) fn definition_event(
    version: u64,
    previous_version: Option<u64>,
    receipt_sha256: &str,
    registry: &[String],
    diff: &serde_json::Value,
    readmission: Option<&DefinitionReadmission>,
) -> Result<Value, CellError> {
    #[derive(Serialize)]
    #[serde(rename_all = "camelCase")]
    struct Event<'a> {
        #[serde(rename = "type")]
        kind: &'static str,
        version: u64,
        previous_version: Option<u64>,
        previous_receipt_sha256: &'a str,
        receipt_sha256: &'a str,
        previous_checker_version: &'a str,
        previous_expr_profile: &'a str,
        checker_version: &'a str,
        expr_profile: &'a str,
        diff: &'a serde_json::Value,
    }

    let Some(readmission) = readmission else {
        return definition_installed_event(
            version,
            previous_version,
            receipt_sha256,
            registry,
            diff,
        );
    };

    norn_ir::runtime::encode(&Event {
        kind: "definitions.readmitted",
        version,
        previous_version,
        previous_receipt_sha256: &readmission.previous_receipt_sha256,
        receipt_sha256,
        previous_checker_version: &readmission.previous_checker_version,
        previous_expr_profile: &readmission.previous_expr_profile,
        checker_version: &readmission.checker_version,
        expr_profile: &readmission.expr_profile,
        diff,
    })
    .map_err(CellError::Encoding)
}

pub(crate) fn verify_admission_receipt(
    receipt: &str,
    expected_sha256: &str,
) -> Result<(), CellError> {
    let found = norn_checker::admission_receipt_sha256(receipt)
        .map_err(|error| CellError::DefinitionReceiptInvalid(error.to_string()))?;
    if found != expected_sha256 {
        return Err(CellError::DefinitionReceiptInvalid(format!(
            "stored receipt digest {found} does not match recorded digest {expected_sha256}"
        )));
    }
    Ok(())
}

fn verify_receipt_profile(
    receipt: &str,
    checker_version: &str,
    expr_profile: &str,
) -> Result<(), CellError> {
    let value: serde_json::Value = serde_json::from_str(receipt)
        .map_err(|error| CellError::DefinitionReceiptInvalid(error.to_string()))?;
    let recorded_checker = value
        .get("checker")
        .and_then(|checker| checker.get("version"))
        .and_then(serde_json::Value::as_str);
    let recorded_profile = value.get("exprProfile").and_then(serde_json::Value::as_str);
    if recorded_checker != Some(checker_version) || recorded_profile != Some(expr_profile) {
        return Err(CellError::DefinitionReceiptInvalid(
            "receipt checker/profile does not match the readmission metadata".to_owned(),
        ));
    }
    Ok(())
}

fn validate_readmission_transition(
    previous: &InstalledDefinitionSet,
    installed: &InstalledDefinitionSet,
    readmission: &DefinitionReadmission,
) -> Result<(), CellError> {
    if readmission.previous_checker_version.is_empty()
        || readmission.previous_expr_profile.is_empty()
        || readmission.checker_version.is_empty()
        || readmission.expr_profile.is_empty()
        || previous.receipt_sha256 != readmission.previous_receipt_sha256
        || !readmission_canonical_set_matches(
            &previous.canonical_set,
            &installed.canonical_set,
            previous.layout.as_deref(),
            installed.layout.as_deref(),
            previous.layout_revision,
            readmission.layout_migration,
        )?
        || previous.registry != installed.registry
        || previous.model_hash != installed.model_hash
        || previous.ddl_hash != installed.ddl_hash
    {
        return Err(CellError::DefinitionReceiptInvalid(
            "readmission must preserve the active definition set and model and name its previous receipt".to_owned(),
        ));
    }
    let Some(receipt) = &installed.admission_receipt else {
        return Err(CellError::DefinitionReceiptInvalid(
            "readmission must retain the new canonical admission receipt".to_owned(),
        ));
    };
    verify_admission_receipt(receipt, &installed.receipt_sha256)?;
    verify_receipt_profile(
        receipt,
        &readmission.checker_version,
        &readmission.expr_profile,
    )?;
    if let Some(previous_receipt) = &previous.admission_receipt {
        verify_admission_receipt(previous_receipt, &previous.receipt_sha256)?;
        verify_receipt_profile(
            previous_receipt,
            &readmission.previous_checker_version,
            &readmission.previous_expr_profile,
        )?;
    }
    Ok(())
}

fn readmission_canonical_set_matches(
    previous: &str,
    installed: &str,
    previous_layout: Option<&str>,
    installed_layout: Option<&str>,
    previous_layout_revision: u64,
    layout_migration: bool,
) -> Result<bool, CellError> {
    if !layout_migration {
        return Ok(previous == installed);
    }
    let mut original = norn_ir::RuntimeValue::from_json_text(previous).map_err(|error| {
        CellError::DefinitionReceiptInvalid(format!(
            "previous artifact set is invalid during layout migration: {error}"
        ))
    })?;
    let norn_ir::RuntimeValue::Array(artifacts) = &mut original else {
        return Err(CellError::DefinitionReceiptInvalid(
            "previous artifact set is not an array during layout migration".to_owned(),
        ));
    };
    let mut extracted_layout = norn_ir::LayoutSidecar::default();
    for artifact in artifacts {
        let extracted = norn_checker::extract_inline_layout(artifact).map_err(|diagnostics| {
            CellError::DefinitionReceiptInvalid(format!(
                "previous inline layout cannot be migrated: {}",
                norn_checker::diagnostics_to_json(&diagnostics)
            ))
        })?;
        for (artifact_id, layout) in extracted.artifacts {
            if extracted_layout
                .artifacts
                .insert(artifact_id, layout)
                .is_some()
            {
                return Ok(false);
            }
        }
    }
    let normalized = String::from_utf8(original.canonical_json().map_err(|error| {
        CellError::DefinitionReceiptInvalid(format!(
            "normalized artifact set cannot be encoded: {error}"
        ))
    })?)
    .map_err(|error| {
        CellError::DefinitionReceiptInvalid(format!(
            "normalized artifact set is not UTF-8: {error}"
        ))
    })?;
    if normalized == previous || normalized != installed {
        return Ok(false);
    }

    let previous_layout_value = previous_layout.map(decode_layout_snapshot).transpose()?;
    let installed_layout_value = installed_layout.map(decode_layout_snapshot).transpose()?;
    // A retained empty snapshot with an advanced layout revision is an
    // explicit clear. Its absence of artifact entries intentionally
    // suppresses the legacy geometry still present in historical bytes.
    if previous_layout_revision > 0
        && previous_layout_value.as_ref().is_some_and(|layout| {
            layout
                .get("artifacts")
                .and_then(Value::as_object)
                .is_some_and(BTreeMap::is_empty)
        })
    {
        return Ok(matches!(
            (&previous_layout_value, &installed_layout_value),
            (Some(previous), Some(installed)) if previous == installed
        ));
    }

    let extracted_value = norn_ir::runtime::encode(&extracted_layout).map_err(|error| {
        CellError::DefinitionReceiptInvalid(format!(
            "extracted layout cannot be encoded during migration: {error}"
        ))
    })?;
    let Some(installed_layout_value) = installed_layout_value else {
        return Ok(!layout_has_required_fields(&extracted_value));
    };
    Ok(layout_fields_match(
        &extracted_value,
        previous_layout_value.as_ref(),
        &installed_layout_value,
        &mut Vec::new(),
    ))
}

fn decode_layout_snapshot(layout: &str) -> Result<Value, CellError> {
    let value = Value::from_json_text(layout).map_err(|error| {
        CellError::DefinitionReceiptInvalid(format!("stored layout sidecar is invalid: {error}"))
    })?;
    norn_ir::runtime::decode::<norn_ir::LayoutSidecar>(&value).map_err(|error| {
        CellError::DefinitionReceiptInvalid(format!("stored layout sidecar is invalid: {error}"))
    })?;
    Ok(value)
}

fn layout_has_required_fields(layout: &Value) -> bool {
    layout
        .get("artifacts")
        .and_then(Value::as_object)
        .is_some_and(|artifacts| !artifacts.is_empty())
}

fn layout_fields_match(
    extracted: &Value,
    previous: Option<&Value>,
    installed: &Value,
    path: &mut Vec<String>,
) -> bool {
    match (extracted.as_object(), installed.as_object()) {
        (Some(extracted), Some(installed)) => {
            for (member, value) in extracted {
                let Some(installed_value) = installed.get(member) else {
                    return false;
                };
                let previous_value = previous.and_then(|value| value.get(member));
                path.push(member.clone());
                let matches = match layout_field_mode(path) {
                    Some(LayoutFieldMode::Settings) => {
                        settings_field_matches(value, previous_value, installed_value)
                    }
                    Some(LayoutFieldMode::Exact) => {
                        value == installed_value
                            || previous_value.is_some_and(|previous| previous == installed_value)
                    }
                    None => layout_fields_match(value, previous_value, installed_value, path),
                };
                path.pop();
                if !matches {
                    return false;
                }
            }
            true
        }
        _ => extracted == installed || previous.is_some_and(|value| value == installed),
    }
}

enum LayoutFieldMode {
    Exact,
    Settings,
}

fn layout_field_mode(path: &[String]) -> Option<LayoutFieldMode> {
    let member = path.last()?.as_str();
    match (path.len(), member) {
        (3, "settings") if path.first().is_some_and(|part| part == "artifacts") => {
            Some(LayoutFieldMode::Settings)
        }
        (3, "viewport") if path.first().is_some_and(|part| part == "artifacts") => {
            Some(LayoutFieldMode::Exact)
        }
        (5, "position" | "description" | "folded")
            if path.first().is_some_and(|part| part == "artifacts")
                && path.get(2).is_some_and(|part| part == "nodes") =>
        {
            Some(LayoutFieldMode::Exact)
        }
        (5, "name" | "targetHandle" | "type")
            if path.first().is_some_and(|part| part == "artifacts")
                && path.get(2).is_some_and(|part| part == "edges") =>
        {
            Some(LayoutFieldMode::Exact)
        }
        (7, "folded")
            if path.first().is_some_and(|part| part == "artifacts")
                && path.get(2).is_some_and(|part| part == "nodes")
                && path.get(4).is_some_and(|part| part == "rows") =>
        {
            Some(LayoutFieldMode::Exact)
        }
        (7, "width")
            if path.first().is_some_and(|part| part == "artifacts")
                && path.get(2).is_some_and(|part| part == "nodes")
                && path.get(4).is_some_and(|part| part == "columns") =>
        {
            Some(LayoutFieldMode::Exact)
        }
        _ => None,
    }
}

fn settings_field_matches(extracted: &Value, previous: Option<&Value>, installed: &Value) -> bool {
    if extracted == installed || previous.is_some_and(|value| value == installed) {
        return true;
    }
    match (
        extracted.as_object(),
        previous.and_then(Value::as_object),
        installed.as_object(),
    ) {
        (Some(extracted), previous, Some(installed)) => extracted.iter().all(|(member, value)| {
            let Some(installed_value) = installed.get(member) else {
                return false;
            };
            settings_field_matches(
                value,
                previous.and_then(|value| value.get(member)),
                installed_value,
            )
        }),
        _ => false,
    }
}

#[cfg(test)]
mod readmission_layout_tests {
    #![allow(
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        reason = "focused fixtures exercise the private readmission trust check"
    )]

    use super::readmission_canonical_set_matches;
    use norn_ir::RuntimeValue as Value;
    use serde_json::json;

    const LEGACY_SET: &str = r#"[{"artifactId":"rule.layout","kind":"rule","payload":{"ruleVersion":"2.0","graph":{"nodes":[{"id":"node-1","position":{"x":1,"y":2},"description":"inline"}],"edges":[],"settings":{"mode":"inline"}}}}]"#;

    fn canonical(value: &Value) -> String {
        String::from_utf8(value.canonical_json().expect("canonical JSON")).expect("UTF-8 JSON")
    }

    fn migration_pair() -> (String, String, String) {
        let mut legacy = Value::from_json_text(LEGACY_SET).expect("legacy artifact set");
        let previous = canonical(&legacy);
        let Value::Array(artifacts) = &mut legacy else {
            panic!("fixture is an array")
        };
        let extracted =
            norn_checker::extract_inline_layout(&mut artifacts[0]).expect("legacy layout extracts");
        let installed = canonical(&legacy);
        let layout = norn_ir::runtime::encode(&extracted).expect("sidecar encodes");
        (previous, installed, canonical(&layout))
    }

    fn layout(artifacts: &serde_json::Value) -> String {
        let value = Value::from_json(json!({
            "layoutVersion": "0.1",
            "artifacts": artifacts
        }))
        .expect("sidecar is exact JSON");
        canonical(&value)
    }

    #[test]
    fn migration_requires_extracted_geometry_and_accepts_existing_sidecar_precedence() {
        let (previous, installed, extracted) = migration_pair();
        assert!(
            readmission_canonical_set_matches(
                &previous,
                &installed,
                None,
                Some(&extracted),
                0,
                true,
            )
            .expect("valid initial migration")
        );
        assert!(
            !readmission_canonical_set_matches(&previous, &installed, None, None, 0, true,)
                .expect("missing sidecar is a mismatch")
        );

        let tampered = layout(&json!({
            "rule.layout": {
                "nodes": {
                    "node-1": {
                        "position": { "x": 4, "y": 5 },
                        "description": "inline"
                    }
                },
                "settings": { "mode": "inline" }
            }
        }));
        assert!(
            !readmission_canonical_set_matches(
                &previous,
                &installed,
                None,
                Some(&tampered),
                0,
                true,
            )
            .expect("tampered geometry is a mismatch")
        );

        let inherited = layout(&json!({
            "rule.layout": {
                "nodes": {
                    "node-1": {
                        "position": { "x": 9, "y": 8 },
                        "description": "saved"
                    }
                },
                "settings": { "mode": "saved", "extra": true },
                "viewport": { "x": 1, "y": 2, "zoom": 1 }
            },
            "orphan.artifact": {
                "nodes": { "orphan-node": { "position": { "x": 0, "y": 0 } } }
            }
        }));
        let retained_sidecar_wins = layout(&json!({
            "rule.layout": {
                "nodes": {
                    "node-1": {
                        "position": { "x": 9, "y": 8 },
                        "description": "saved"
                    }
                },
                "settings": { "mode": "saved", "extra": true }
            }
        }));
        assert!(
            readmission_canonical_set_matches(
                &previous,
                &installed,
                Some(&inherited),
                Some(&retained_sidecar_wins),
                1,
                true,
            )
            .expect("an inherited sidecar value takes precedence")
        );

        let inline_wins = layout(&json!({
            "rule.layout": {
                "nodes": {
                    "node-1": {
                        "position": { "x": 1, "y": 2 },
                        "description": "inline"
                    }
                },
                "settings": { "mode": "inline", "extra": true }
            }
        }));
        assert!(
            readmission_canonical_set_matches(
                &previous,
                &installed,
                Some(&inherited),
                Some(&inline_wins),
                1,
                true,
            )
            .expect("legacy inline fields are also retained by current composition")
        );
    }

    #[test]
    fn migration_keeps_explicit_nulls_and_retained_empty_clears() {
        let (previous, installed, _) = migration_pair();
        let cleared_field = layout(&json!({
            "rule.layout": {
                "nodes": { "node-1": { "position": null, "description": "inline" } },
                "settings": { "mode": "inline" }
            }
        }));
        let prior_clear = layout(&json!({
            "rule.layout": {
                "nodes": { "node-1": { "position": null } }
            }
        }));
        assert!(
            readmission_canonical_set_matches(
                &previous,
                &installed,
                Some(&prior_clear),
                Some(&cleared_field),
                2,
                true,
            )
            .expect("an explicit null clear is retained")
        );

        let empty = layout(&json!({}));
        assert!(
            readmission_canonical_set_matches(
                &previous,
                &installed,
                Some(&empty),
                Some(&empty),
                2,
                true,
            )
            .expect("a retained explicit empty snapshot suppresses inline migration")
        );
    }
}

fn validate_readmission(
    previous: &InstalledDefinitionSet,
    candidate_model: &Model,
    input: &InstalledDefinitionInput,
    expected_version: Option<u64>,
) -> Result<(), CellError> {
    let Some(readmission) = &input.readmission else {
        if let Some(receipt) = &input.admission_receipt {
            verify_admission_receipt(receipt, &input.receipt_sha256)?;
        }
        return Ok(());
    };

    if expected_version != Some(previous.version)
        || previous.receipt_sha256 != readmission.previous_receipt_sha256
        || !readmission_canonical_set_matches(
            &previous.canonical_set,
            &input.canonical_set,
            previous.layout.as_deref(),
            input.layout.as_deref().or(previous.layout.as_deref()),
            previous.layout_revision,
            readmission.layout_migration,
        )?
        || previous.registry != input.registry
        || previous.model_hash != candidate_model.model_hash()
        || previous.ddl_hash != candidate_model.ddl_hash()
        || readmission.previous_checker_version.is_empty()
        || readmission.previous_expr_profile.is_empty()
        || readmission.checker_version.is_empty()
        || readmission.expr_profile.is_empty()
    {
        return Err(CellError::DefinitionReceiptInvalid(
            "readmission must preserve the active definition set and model and name its prior and current checker profiles".to_owned(),
        ));
    }
    let Some(receipt) = &input.admission_receipt else {
        return Err(CellError::DefinitionReceiptInvalid(
            "readmission must retain the new canonical admission receipt".to_owned(),
        ));
    };
    verify_admission_receipt(receipt, &input.receipt_sha256)?;
    verify_receipt_profile(
        receipt,
        &readmission.checker_version,
        &readmission.expr_profile,
    )?;
    if let Some(previous_receipt) = &previous.admission_receipt {
        verify_admission_receipt(previous_receipt, &previous.receipt_sha256)?;
        verify_receipt_profile(
            previous_receipt,
            &readmission.previous_checker_version,
            &readmission.previous_expr_profile,
        )?;
    }
    Ok(())
}

/// A whole-object write: the `CellHost` commit's fact write, after the
/// kernel's compare-and-swap check (the new version is the current one plus
/// one; versions never repeat for an id within its type hierarchy).
///
/// Externally tagged (`{"put": {...}}`): an internally tagged decoder would
/// buffer the exact values through `f64` (#2770).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum FactWriteRow {
    /// Create the object, or replace all of its property values.
    Put {
        /// The object's (concrete) type.
        object_type: String,
        /// The object id.
        key: String,
        /// The complete property values; omitted ones take their declared
        /// default, or `null`.
        values: BTreeMap<String, Value>,
    },
    /// Delete the object (on an absent one this only bumps its version
    /// high-water mark).
    Delete {
        /// The object's type.
        object_type: String,
        /// The object id.
        key: String,
    },
}

impl FactWriteRow {
    /// The written object's type and id.
    #[must_use]
    pub fn target(&self) -> (&str, &str) {
        match self {
            Self::Put {
                object_type, key, ..
            }
            | Self::Delete { object_type, key } => (object_type, key),
        }
    }
}

/// A link change in a commit.
///
/// Externally tagged, like [`FactWriteRow`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum LinkChange {
    /// Link `source` to `target` with these link property values.
    Add {
        /// The source object's type (the declaring type or a subtype).
        object_type: String,
        /// The link id.
        link: String,
        /// The source object id.
        source: String,
        /// The target object id.
        target: String,
        /// Link property values by property id.
        #[serde(default)]
        values: BTreeMap<String, Value>,
        /// The target object's type as the caller names it (the kernel's
        /// `LinkKey`); the declared link target when absent.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_type: Option<String>,
    },
    /// Remove the link from `source` to `target` (a no-op when absent).
    Remove {
        /// The source object's type.
        object_type: String,
        /// The link id.
        link: String,
        /// The source object id.
        source: String,
        /// The target object id.
        target: String,
        /// As for [`LinkChange::Add`].
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_type: Option<String>,
    },
}

/// A link a commit added or removed, with its new version (#3013).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LinkedRow {
    /// The source object's type, as the change named it.
    pub object_type: String,
    /// The link id.
    pub link: String,
    /// The source object id.
    pub source: String,
    /// The target object's type.
    pub target_type: String,
    /// The target object id.
    pub target: String,
    /// The link's new version.
    pub version: u64,
}

/// A command to enqueue in the outbox for `perform`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutboxCommand {
    /// Stable command id (unique in the outbox).
    pub command_id: String,
    /// Request generation the result must match.
    pub generation: u64,
    /// The key the external system deduplicates on.
    pub idempotency_key: String,
    /// Logical deadline in milliseconds, if any.
    pub deadline: Option<u64>,
    /// The whole serialized command (freshness policy and payload included).
    pub command: Value,
}

/// A timer to schedule (replacing a pending timer with the same id).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimerEntry {
    /// Stable timer id.
    pub timer_id: String,
    /// Logical due time in milliseconds.
    pub due: u64,
    /// The whole serialized timer (its payload is the input delivered to
    /// `decide` when it fires).
    pub timer: Value,
}

/// One host effect of a commit, applied in order.
///
/// Externally tagged, like [`FactWriteRow`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum EffectRow {
    /// Enqueue a command for `perform`. A command id already enqueued at the
    /// same or a newer generation (pending or consumed) is a no-op, so a
    /// re-issue never re-executes; a newer generation of a pending command
    /// replaces it in place (dropping its recorded result: the new generation
    /// fences it); a newer generation of a consumed one is appended.
    Enqueue {
        /// The command.
        command: OutboxCommand,
    },
    /// Durably claim the current unperformed outbox head before dispatching
    /// it outside the cell. The command id and generation must still name
    /// that head, and may be claimed only once.
    ClaimPerform {
        /// The command id.
        command_id: String,
        /// The generation being dispatched.
        generation: u64,
    },
    /// Insert or replace a timer by id; its incarnation is this commit.
    Schedule {
        /// The timer.
        timer: TimerEntry,
    },
    /// Remove a timer (absent is a no-op).
    CancelTimer {
        /// The timer id.
        timer_id: String,
    },
}

/// The delivery a commit consumes (the `CellHost` consumption reference).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum Consumption {
    /// A timer, identified by id and incarnation: a timer rescheduled since
    /// delivery is a different timer.
    Timer {
        /// The timer id.
        timer_id: String,
        /// The commit that scheduled this incarnation.
        incarnation: u64,
    },
    /// The recorded result of the outbox head command.
    Result {
        /// The command id.
        command_id: String,
        /// The generation it was performed under.
        generation: u64,
    },
    /// An ingested observation.
    Observation {
        /// The observation id.
        observation_id: String,
    },
}

impl fmt::Display for Consumption {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Timer {
                timer_id,
                incarnation,
            } => write!(f, "timer {timer_id} scheduled at {incarnation}"),
            Self::Result {
                command_id,
                generation,
            } => write!(f, "result of {command_id} generation {generation}"),
            Self::Observation { observation_id } => write!(f, "observation {observation_id}"),
        }
    }
}

/// Everything one commit writes, atomically, in this order: consumption,
/// object writes, fact changes, link changes, events, effects, receipt.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommitBatch {
    /// The world (`main` or a fork id).
    pub world: String,
    /// The tenant scope recorded on created objects.
    pub scope: String,
    /// The host's logical time of the turn, in milliseconds.
    pub logical_time: u64,
    /// A caller idempotency key: a second commit with the same key and the
    /// same request returns the first receipt; with a different request it
    /// is refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    /// The delivery this commit consumes; it must be pending.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consumes: Option<Consumption>,
    /// Bounded quantity operation, atomically admitted with this batch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantity: Option<norn_ir::quantity_ledger::QuantityRequest>,
    /// Store-generated input fingerprint retained in committed batches.
    /// Caller-supplied values are ignored when deriving retry identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantity_intent: Option<String>,
    /// Whole-object writes, in order.
    #[serde(default)]
    pub writes: Vec<FactWriteRow>,
    /// Kernel-trusted optimistic reads used to derive the writes in this
    /// turn. The writer uses their immutable marking snapshots to propagate
    /// classifications into derived facts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub marking_reads: Vec<norn_kernel::FactExpectation>,
    /// Additive existence dependencies from kernel-proven reference reads.
    /// These resolve occurrence and identity labels; complete `marking_reads` always
    /// retain their full value lineage even if also listed here.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub marking_reference_reads: Vec<norn_kernel::FactExpectation>,
    /// Kernel-proven property operands of exact-version action inputs.
    /// Identity and occurrence labels always contribute too; unresolved
    /// properties remain unknown. Full `marking_reads` take precedence.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub marking_property_reads: Vec<(norn_kernel::FactExpectation, Vec<String>)>,
    /// Immutable Function input lineage, independent of freshness/CAS.
    /// Always resolves the exact historical snapshots: a later visibility
    /// declassification must not reclassify an already dispatched input.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub marking_historical_reads: Vec<norn_kernel::FactExpectation>,
    /// Trusted adapter outputs whose values were recomputed, even when their
    /// serialized values equal the previous version. This adds read lineage;
    /// it never removes existing labels or reclassifies historical versions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub marking_derived_properties: Vec<(norn_kernel::FactKey, Vec<String>)>,
    /// Additional destination CAS reads that conservatively protect changed
    /// values of that destination only. Each must name an exact written
    /// destination's current version. Sibling outputs do not read its values.
    /// These can add value lineage, never subtract declared occurrence inputs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub marking_value_reads: Vec<norn_kernel::FactExpectation>,
    /// Additional occurrence inputs. Unauthenticated overrides are additive:
    /// every complete `marking_reads` entry remains in occurrence lineage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub marking_occurrence_reads: Option<Vec<norn_kernel::FactExpectation>>,
    /// Immutable ledger inputs evaluated by this turn, separate from live
    /// optimistic reads. Zero denotes unresolved event attribution.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub marking_events: Vec<u64>,
    /// Canonical fact changes, in order.
    #[serde(default)]
    pub facts: Vec<FactChange>,
    /// Authorized removals from the current effective-marking set. The
    /// matching `marking.declassified` event is still required in `events`;
    /// both are hashed and committed with the same receipt.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub marking_declassifications: Vec<MarkingDeclassification>,
    /// Link changes, in order.
    #[serde(default)]
    pub links: Vec<LinkChange>,
    /// Ledger events, in order.
    #[serde(default)]
    pub events: Vec<Value>,
    /// Outbox and timer effects, in order.
    #[serde(default)]
    pub effects: Vec<EffectRow>,
}

/// One exact-version, authorized declassification applied by the cell writer.
///
/// The API supplies this only after checking `norn.declassify` for each
/// marking. The cell repeats the object-version compare-and-swap inside the
/// commit transaction and requires the corresponding canonical ledger event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MarkingDeclassification {
    /// The concrete object type.
    pub object_type: String,
    /// The object's opaque store id.
    pub key: String,
    /// The property whose current markings are changed.
    pub property: String,
    /// The exact current fact version the request authorized.
    pub expected_version: u64,
    /// Markings removed from the current snapshot only.
    pub markings: Vec<String>,
    /// The authenticated actor that authorized the operation.
    pub actor: String,
    /// Human-entered reason retained in the hash-chained ledger.
    pub reason: String,
}

impl MarkingDeclassification {
    /// The canonical audit event that must accompany this change.
    ///
    /// # Errors
    /// Returns a cell encoding error if the event cannot be represented.
    pub fn ledger_event(&self) -> Result<Value, CellError> {
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Event<'a> {
            #[serde(rename = "type")]
            kind: &'static str,
            object_type: &'a str,
            key: &'a str,
            property: &'a str,
            expected_version: u64,
            markings: &'a [String],
            actor: &'a str,
            reason: &'a str,
        }

        norn_ir::runtime::encode(&Event {
            kind: "marking.declassified",
            object_type: &self.object_type,
            key: &self.key,
            property: &self.property,
            expected_version: self.expected_version,
            markings: &self.markings,
            actor: &self.actor,
            reason: &self.reason,
        })
        .map_err(CellError::Encoding)
    }
}

/// An object's version after a commit wrote it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WrittenRow {
    /// The object type.
    pub object_type: String,
    /// The object id.
    pub key: String,
    /// The new version.
    pub version: u64,
}

/// The hashed body of a receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReceiptBody {
    /// The commit's sequence number (1-based, gapless): its ledger position.
    pub commit_seq: u64,
    /// The admitted definitions version used for this commit (`0` for a
    /// legacy/unseeded store). Omitted from legacy receipt hashes.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub definitions_version: u64,
    /// The world committed to.
    pub world: String,
    /// The turn's logical time.
    pub logical_time: u64,
    /// The caller idempotency key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
    /// Norn1 hash of the [`CommitBatch`].
    pub request_hash: String,
    /// Typed quantity provenance, bound into the receipt hash. Legacy ordinary
    /// receipts omit this field regardless of their event type names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantity: Option<norn_ir::quantity_ledger::QuantityRequest>,
    /// Exact quantity result, derived under the transaction and hash bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantity_result: Option<norn_kernel::quantity::QuantityResult>,
    /// Store-computed fingerprint of complete original inputs and the pinned
    /// definitions version, before materialization.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantity_intent: Option<String>,
    /// The delivery consumed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consumed: Option<Consumption>,
    /// All fact versions emitted by whole-object and canonical writes.
    pub written: Vec<WrittenRow>,
    /// The links the commit added or removed, with their new versions
    /// (#3013). Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub linked: Vec<LinkedRow>,
    /// Ledger sequence number of the first event (the next one when there are
    /// none).
    pub first_event_seq: u64,
    /// How many events the commit appended.
    pub event_count: u64,
    /// The ledger head hash after the commit.
    pub ledger_head: String,
    /// Command ids enqueued.
    pub enqueued: Vec<String>,
    /// Timer ids scheduled.
    pub scheduled: Vec<String>,
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde skip_serializing_if requires a reference predicate"
)]
const fn is_zero(value: &u64) -> bool {
    *value == 0
}

/// A commit receipt: its body and the body's Norn1 hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Receipt {
    /// The hashed body.
    pub body: ReceiptBody,
    /// Norn1 hash of `body`.
    pub hash: String,
}

/// What a commit did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommitOutcome {
    /// The batch was committed.
    Committed(Receipt),
    /// The idempotency key was already committed with this exact request:
    /// nothing was written and the recorded receipt is returned.
    Replayed(Receipt),
}

impl CommitOutcome {
    /// The receipt.
    #[must_use]
    pub const fn receipt(&self) -> &Receipt {
        match self {
            Self::Committed(receipt) | Self::Replayed(receipt) => receipt,
        }
    }
}

/// Where a commit is when a fault probe runs ([`CellStore::commit_probed`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum CommitPoint {
    /// The transaction began; nothing written.
    Begun,
    /// The delivery consumed, object writes and fact changes applied.
    FactsApplied,
    /// Link changes applied.
    LinksApplied,
    /// Events appended to the ledger.
    EventsAppended,
    /// Outbox and timer effects written.
    EffectsEnqueued,
    /// The receipt written; committing the engine transaction is next.
    ReceiptWritten,
    /// The engine transaction committed; the caller has not yet been told.
    Committed,
}

impl CommitPoint {
    /// Every point, in commit order.
    pub const ALL: [Self; 7] = [
        Self::Begun,
        Self::FactsApplied,
        Self::LinksApplied,
        Self::EventsAppended,
        Self::EffectsEnqueued,
        Self::ReceiptWritten,
        Self::Committed,
    ];
}

/// A guarded commit's failure: the guard refused, or the store failed.
#[derive(Debug)]
pub enum GuardedError<X> {
    /// The guard refused the commit; nothing was written.
    Guard(X),
    /// The store failed; nothing was written (unless the probe failed at
    /// [`CommitPoint::Committed`]).
    Store(CellError),
}

impl<X> From<CellError> for GuardedError<X> {
    fn from(error: CellError) -> Self {
        Self::Store(error)
    }
}

#[cfg(feature = "native")]
impl<X> From<rusqlite::Error> for GuardedError<X> {
    fn from(error: rusqlite::Error) -> Self {
        Self::Store(CellError::Sqlite(error))
    }
}

type CandidateValidator<E> = fn(&StoreReader<'_, E>, &CommitBatch, bool) -> Result<(), CellError>;

/// The normalized store of one tenant cell over a SQLite-format database.
#[derive(Debug)]
pub struct CellStore<E: SqlEngine = DefaultEngine> {
    pub(crate) conn: E,
    model: Model,
    candidate_validator: Cell<Option<CandidateValidator<E>>>,
    #[cfg(feature = "native")]
    pub(crate) export_notify: Arc<Notify>,
    /// The last head this store verified (#2975): verification at open,
    /// then [`Self::verified_head`] extends it over later commits only.
    verified: RefCell<LedgerHead>,
    /// Immutable quantity boundary found at open or established by a committed
    /// upgrade. Mutable metadata is checked against it, never used as authority.
    quantity_boundary: Cell<Option<crate::quantity::FormatBoundary>>,
}

#[derive(Clone, Copy)]
struct DefinitionInstallContext<'a> {
    candidate_model: &'a Model,
    input: &'a InstalledDefinitionInput,
    expected_version: Option<u64>,
}

struct DefinitionPreflight {
    installed: InstalledDefinitionSet,
    migration: crate::migration::MigrationPlan,
}

fn preflight_definition_install<E: SqlEngine>(
    engine: &E,
    previous_model: &Model,
    install: &DefinitionInstallContext<'_>,
    actual_version: Option<u64>,
) -> Result<DefinitionPreflight, CellError> {
    if install
        .expected_version
        .is_some_and(|expected| expected != actual_version.unwrap_or(0))
    {
        return Err(CellError::DefinitionVersionConflict {
            expected: install.expected_version,
            actual: actual_version,
        });
    }
    let previous = actual_version
        .map(|version| {
            definition_by_version(engine, version)?.ok_or_else(|| CellError::LedgerCorrupt {
                seq: version,
                reason: "active definitions version has no retained set".to_owned(),
            })
        })
        .transpose()?;
    let (layout, layout_revision) =
        resolve_layout_snapshot(previous.as_ref(), install.input.layout.as_deref())?;
    if install.input.readmission.is_some() {
        let previous = previous.as_ref().ok_or_else(|| {
            CellError::DefinitionReceiptInvalid(
                "readmission requires an active definitions version".to_owned(),
            )
        })?;
        validate_readmission(
            previous,
            install.candidate_model,
            install.input,
            install.expected_version,
        )?;
    } else if let Some(receipt) = &install.input.admission_receipt {
        verify_admission_receipt(receipt, &install.input.receipt_sha256)?;
    }
    let migration = crate::migration::preflight_model_migration(
        engine,
        previous_model,
        install.candidate_model,
        previous
            .as_ref()
            .map(|installed| installed.canonical_set.as_str()),
        &install.input.canonical_set,
    )?;
    preflight_marking_definition_changes(engine, previous_model, install.candidate_model)?;
    preflight_mask_policy_changes(
        previous
            .as_ref()
            .map(|installed| installed.canonical_set.as_str()),
        &install.input.canonical_set,
    )?;
    preflight_source_marking_changes(
        engine,
        previous
            .as_ref()
            .map(|installed| installed.canonical_set.as_str()),
        &install.input.canonical_set,
    )?;
    let version = actual_version
        .unwrap_or(0)
        .checked_add(1)
        .ok_or(CellError::OutOfRange("definitions version"))?;
    let commit_seq = StoreReader::new(engine, previous_model).next_commit_seq()?;
    Ok(DefinitionPreflight {
        installed: InstalledDefinitionSet {
            version,
            receipt_sha256: install.input.receipt_sha256.clone(),
            admission_receipt: install.input.admission_receipt.clone(),
            canonical_set: install.input.canonical_set.clone(),
            registry: install.input.registry.clone(),
            model_hash: install.candidate_model.model_hash().to_owned(),
            ddl_hash: install.candidate_model.ddl_hash().to_owned(),
            installed_at: install.input.installed_at,
            commit_seq,
            layout,
            layout_revision,
        },
        migration,
    })
}

fn resolve_layout_snapshot(
    previous: Option<&InstalledDefinitionSet>,
    supplied: Option<&str>,
) -> Result<(Option<String>, u64), CellError> {
    if let Some(layout) = supplied {
        let value = norn_ir::RuntimeValue::from_json_text(layout).map_err(|error| {
            CellError::DefinitionReceiptInvalid(format!("layout sidecar is invalid: {error}"))
        })?;
        let canonical = value.canonical_json().map_err(|error| {
            CellError::DefinitionReceiptInvalid(format!("layout sidecar is invalid: {error}"))
        })?;
        norn_ir::runtime::decode::<norn_ir::LayoutSidecar>(&value).map_err(|error| {
            CellError::DefinitionReceiptInvalid(format!("layout sidecar is invalid: {error}"))
        })?;
        if canonical.as_slice() != layout.as_bytes() {
            return Err(CellError::DefinitionReceiptInvalid(
                "layout sidecar is not in canonical JSON form".to_owned(),
            ));
        }
    }
    let old_layout = previous.and_then(|installed| installed.layout.as_deref());
    let old_revision = previous.map_or(0, |installed| installed.layout_revision);
    let layout = supplied
        .map(str::to_owned)
        .or_else(|| previous.and_then(|installed| installed.layout.clone()));
    let changed = supplied.is_some_and(|layout| Some(layout) != old_layout);
    let layout_revision = if changed {
        old_revision
            .checked_add(1)
            .ok_or(CellError::OutOfRange("layout revision"))?
    } else {
        old_revision
    };
    Ok((layout, layout_revision))
}

#[cfg(feature = "native")]
impl CellStore<NativeEngine> {
    /// Open (or create) the store file at `path` for `model`.
    ///
    /// Creates the system tables and the model's tables if absent, checks
    /// that an existing file has this store format and was created for the
    /// same model (DDL hash), and verifies its ledger and receipts before
    /// returning a writable store.
    ///
    /// # Errors
    ///
    /// [`CellError::SchemaMismatch`] for a file of another format or model;
    /// [`CellError::LedgerCorrupt`] for a ledger that does not verify; SQLite
    /// errors otherwise.
    pub fn open(path: &Path, model: Model, options: StoreOptions) -> Result<Self, CellError> {
        Self::from_engine(NativeEngine::open(path, options)?, model)
    }

    /// An in-memory store for `model` (tests and simulation).
    ///
    /// # Errors
    ///
    /// SQL engine errors.
    pub fn open_in_memory(model: Model) -> Result<Self, CellError> {
        Self::from_engine(NativeEngine::open_in_memory()?, model)
    }
}

// Check before DDL, definition decoding, or host model selection touches old data.
fn validate_store_format<E: SqlEngine>(engine: &E) -> Result<(), CellError> {
    let exists = engine
        .query_optional(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            params![META],
            |row| row.get::<i64>(0),
        )?
        .is_some();
    if exists
        && let Some(found) = engine.query_optional(
            &format!("SELECT value FROM \"{META}\" WHERE key = ?1"),
            params!["format"],
            |row| row.get::<String>(0),
        )?
        && found != STORE_FORMAT
    {
        return Err(CellError::StoreFormat {
            found,
            expected: STORE_FORMAT.to_owned(),
        });
    }
    Ok(())
}

impl<E: SqlEngine> CellStore<E> {
    /// Initialize this store over an already-open engine.
    ///
    /// The engine supplies transaction and parameter/result handling; the
    /// store owns the schema, value encoding, and ledger contracts.
    ///
    /// # Errors
    ///
    /// [`CellError::StoreFormat`] or [`CellError::SchemaMismatch`] when the
    /// engine already contains a store for another format or model;
    /// [`CellError::LedgerCorrupt`] when its ledger does not verify; engine
    /// errors otherwise.
    pub fn from_engine(conn: E, model: Model) -> Result<Self, CellError> {
        Self::init(conn, model)
    }

    /// Read the installed active set before constructing the model used to
    /// open this cell. A missing system table means this is a fresh or
    /// pre-install file and the host may use its seed.
    ///
    /// # Errors
    ///
    /// SQL, decoding, or a corrupt/dangling definitions pointer.
    pub fn read_active_definitions(
        engine: &E,
    ) -> Result<Option<InstalledDefinitionSet>, CellError> {
        validate_store_format(engine)?;
        let has_versions = engine
            .query_optional(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
                params![DEFINITION_VERSION],
                |row| row.get::<i64>(0),
            )?
            .is_some();
        let has_active = engine
            .query_optional(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
                params![DEFINITION_ACTIVE],
                |row| row.get::<i64>(0),
            )?
            .is_some();
        match (has_versions, has_active) {
            (false, false) => Ok(None),
            (true, true) => {
                if let Some(version) = validate_active_definition_pointer(engine)? {
                    definition_by_version(engine, version)?.map_or_else(
                        || {
                            Err(CellError::LedgerCorrupt {
                                seq: version,
                                reason: "active definitions pointer has no retained version"
                                    .to_owned(),
                            })
                        },
                        |installed| Ok(Some(installed)),
                    )
                } else {
                    let has_history = engine
                        .query_optional(
                            &format!("SELECT 1 FROM \"{DEFINITION_VERSION}\" LIMIT 1"),
                            params![],
                            |row| row.get::<i64>(0),
                        )?
                        .is_some();
                    if has_history {
                        Err(CellError::LedgerCorrupt {
                            seq: 0,
                            reason: "retained definitions have no active pointer".to_owned(),
                        })
                    } else {
                        Ok(None)
                    }
                }
            }
            _ => Err(CellError::LedgerCorrupt {
                seq: 0,
                reason: "definitions system tables are incomplete".to_owned(),
            }),
        }
    }

    fn init(conn: E, model: Model) -> Result<Self, CellError> {
        validate_store_format(&conn)?;
        conn.transaction(|| -> Result<(), CellError> {
            for statement in SYSTEM_DDL {
                conn.execute_batch(statement)?;
            }
            crate::system::intern_marking_subjects(&conn)?;
            export_journal::init(&conn)?;
            crate::payload_storage::init(&conn)?;
            for statement in model.schema().statements() {
                conn.execute_batch(statement)?;
            }
            let meta = |key: &str| {
                conn.query_optional(
                    &format!("SELECT \"value\" FROM \"{META}\" WHERE \"key\" = ?1"),
                    params![key],
                    |row| row.get::<String>(0),
                )
            };
            match (meta("format")?, meta("ddl_hash")?) {
                (None, None) => {
                    let insert =
                        format!("INSERT INTO \"{META}\" (\"key\", \"value\") VALUES (?1, ?2)");
                    conn.execute(&insert, params!["format", STORE_FORMAT])?;
                    conn.execute(&insert, params!["ddl_hash", model.ddl_hash()])?;
                }
                (Some(format), Some(found))
                    if format == STORE_FORMAT && found == model.ddl_hash() => {}
                // Older formats lack exact encoding or durable marking coverage.
                // Refuse atomically rather than serve partially classified facts.
                (Some(format), _) if format != STORE_FORMAT => {
                    return Err(CellError::StoreFormat {
                        found: format,
                        expected: STORE_FORMAT.to_owned(),
                    });
                }
                (format, found) => {
                    return Err(CellError::SchemaMismatch {
                        expected: format!("{STORE_FORMAT} {}", model.ddl_hash()),
                        found: format!(
                            "{} {}",
                            format.unwrap_or_default(),
                            found.unwrap_or_default()
                        ),
                    });
                }
            }
            if let Some(active) = Self::read_active_definitions(&conn)?
                && (active.model_hash != model.model_hash() || active.ddl_hash != model.ddl_hash())
            {
                return Err(CellError::SchemaMismatch {
                    expected: format!("{} {}", model.ddl_hash(), model.model_hash()),
                    found: format!("{} {}", active.ddl_hash, active.model_hash),
                });
            }
            Ok(())
        })?;
        let store = Self {
            conn,
            model,
            candidate_validator: Cell::new(None),
            #[cfg(feature = "native")]
            export_notify: Arc::new(Notify::new()),
            verified: RefCell::new(LedgerHead::genesis()),
            quantity_boundary: Cell::new(None),
        };
        // An existing file is admitted only if its ledger and receipts verify.
        // Chain, receipts and all quantity caches share one open snapshot.
        // Successful verification is retained by this store's head/boundary
        // caches; ordinary admission does not repeat the startup fold.
        let (head, boundary) = store.conn.transaction(|| {
            let verified = store
                .read()
                .verify_ledger_with_boundary(&LedgerHead::genesis())?;
            store.verify_definition_history()?;
            store.read().verify_value_history()?;
            Ok::<_, CellError>(verified)
        })?;
        store.verified.replace(head);
        store.quantity_boundary.set(boundary);
        Ok(store)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "startup validates every retained install before serving the cell"
    )]
    fn verify_definition_history(&self) -> Result<(), CellError> {
        let reader = self.read();
        let versions = reader.installed_definition_versions()?;
        let receipts = reader.receipt_stream();
        let mut next_version = 1_u64;
        let mut version_index = 0_usize;
        let mut active_version = 0_u64;
        let mut previous_install_seq = 0_u64;

        for receipt in receipts {
            let receipt = receipt?;
            if let Some(version) = versions.get(version_index)
                && version.commit_seq == receipt.body.commit_seq
            {
                if version.version != next_version
                    || version.commit_seq <= previous_install_seq
                    || receipt.body.definitions_version != version.version
                {
                    return Err(CellError::LedgerCorrupt {
                        seq: receipt.body.commit_seq,
                        reason: "definition history version or receipt order is inconsistent"
                            .to_owned(),
                    });
                }
                let record = self.export_record(version.commit_seq)?.ok_or_else(|| {
                    CellError::LedgerCorrupt {
                        seq: version.commit_seq,
                        reason: "installed definitions have no typed export record".to_owned(),
                    }
                })?;
                let installation =
                    record
                        .definition_install
                        .as_ref()
                        .ok_or_else(|| CellError::LedgerCorrupt {
                            seq: version.commit_seq,
                            reason: "installed definitions have no typed export record".to_owned(),
                        })?;
                if let Some(receipt_text) = &version.admission_receipt {
                    verify_admission_receipt(receipt_text, &version.receipt_sha256)?;
                }
                if let Some(readmission) = &installation.readmission {
                    let previous = version_index
                        .checked_sub(1)
                        .and_then(|index| versions.get(index))
                        .ok_or_else(|| CellError::LedgerCorrupt {
                            seq: version.commit_seq,
                            reason: "definition readmission has no prior version".to_owned(),
                        })?;
                    if readmission.previous_receipt_sha256 != previous.receipt_sha256
                        || !readmission_canonical_set_matches(
                            &previous.canonical_set,
                            &version.canonical_set,
                            previous.layout.as_deref(),
                            version.layout.as_deref(),
                            previous.layout_revision,
                            readmission.layout_migration,
                        )?
                        || previous.registry != version.registry
                        || previous.model_hash != version.model_hash
                        || previous.ddl_hash != version.ddl_hash
                        || version.admission_receipt.is_none()
                    {
                        return Err(CellError::LedgerCorrupt {
                            seq: version.commit_seq,
                            reason: "definition readmission changed its definitions or lost receipt evidence"
                                .to_owned(),
                        });
                    }
                    validate_readmission_transition(previous, version, readmission)?;
                }
                let event = definition_event(
                    version.version,
                    (version.version > 1).then_some(version.version.saturating_sub(1)),
                    &version.receipt_sha256,
                    &version.registry,
                    &installation.diff,
                    installation.readmission.as_ref(),
                )?;
                let installed_model = Model::from_snapshot(&installation.model)?;
                let entries =
                    reader.ledger_page(receipt.body.first_event_seq.saturating_sub(1), 1)?;
                if installation.installed != *version
                    || record.receipt != receipt
                    || installed_model.model_hash() != version.model_hash
                    || installed_model.ddl_hash() != version.ddl_hash
                    || record.model_hash != version.model_hash
                    || record.ddl_hash != version.ddl_hash
                    || receipt.body.event_count != 1
                    || record.batch.events.len() != 1
                    || record.batch.events.first() != Some(&event)
                    || record.entries != entries
                {
                    return Err(CellError::LedgerCorrupt {
                        seq: version.commit_seq,
                        reason: "definition history disagrees with its typed export record"
                            .to_owned(),
                    });
                }
                active_version = version.version;
                previous_install_seq = version.commit_seq;
                next_version = next_version
                    .checked_add(1)
                    .ok_or(CellError::OutOfRange("definitions version"))?;
                version_index = version_index
                    .checked_add(1)
                    .ok_or(CellError::OutOfRange("definition history index"))?;
            } else if receipt.body.definitions_version != active_version {
                return Err(CellError::LedgerCorrupt {
                    seq: receipt.body.commit_seq,
                    reason: "receipt names a definitions version not in force".to_owned(),
                });
            }
        }
        if version_index != versions.len() {
            return Err(CellError::LedgerCorrupt {
                seq: versions
                    .get(version_index)
                    .map_or(0, |version| version.commit_seq),
                reason: "definition history extends beyond the receipt ledger".to_owned(),
            });
        }
        let active = active_definition_version(&self.conn)?;
        if active != (active_version != 0).then_some(active_version) {
            return Err(CellError::LedgerCorrupt {
                seq: active_version,
                reason: "active definitions pointer disagrees with retained commit history"
                    .to_owned(),
            });
        }
        Ok(())
    }

    /// Bind the file to `value` under the identity key `key` (for example
    /// the tenant scope it serves): recorded on first use, and checked on
    /// every later open, so a file is never served under another identity.
    ///
    /// # Errors
    ///
    /// [`CellError::BindingMismatch`] when the file is bound to another value.
    pub fn bind(&mut self, key: &str, value: &str) -> Result<(), CellError> {
        let meta_key = format!("binding:{key}");
        self.conn.transaction(|| -> Result<(), CellError> {
            let found: Option<String> = self.conn.query_optional(
                &format!("SELECT \"value\" FROM \"{META}\" WHERE \"key\" = ?1"),
                params![meta_key],
                |row| row.get::<String>(0),
            )?;
            match found {
                Some(found) if found == value => {}
                Some(found) => {
                    return Err(CellError::BindingMismatch {
                        key: key.to_owned(),
                        expected: value.to_owned(),
                        found,
                    });
                }
                None => {
                    self.conn.execute(
                        &format!("INSERT INTO \"{META}\" (\"key\", \"value\") VALUES (?1, ?2)"),
                        params![meta_key, value],
                    )?;
                }
            }
            Ok(())
        })?;
        Ok(())
    }

    /// The object model.
    #[must_use]
    pub const fn model(&self) -> &Model {
        &self.model
    }

    /// Committed-state reads.
    #[must_use]
    pub fn read(&self) -> StoreReader<'_, E> {
        StoreReader::new(&self.conn, &self.model)
            .with_quantity_boundary(self.quantity_boundary.get())
    }

    /// Configure a host-owned, read-only check of the candidate transaction.
    /// It runs after all facts and links are applied and before ledger writes.
    pub fn set_candidate_validator(&self, validator: Option<CandidateValidator<E>>) {
        self.candidate_validator.set(validator);
    }

    /// The verified ledger head, verifying only what was committed since the
    /// last verification (#2975): the whole file is verified at open, and
    /// each call extends the remembered head over the new suffix with
    /// [`StoreReader::verify_ledger_from`], so a reader that reports the head
    /// on every page does not re-read the whole ledger.
    ///
    /// When the remembered head no longer anchors the file (for example a
    /// transaction that was observed and then rolled back), the whole ledger
    /// is verified again, so a stale cache can cost time but never pass a
    /// ledger a full verification would refuse.
    ///
    /// # Errors
    ///
    /// [`CellError::LedgerCorrupt`] when the ledger does not verify.
    pub fn verified_head(&self) -> Result<LedgerHead, CellError> {
        let checkpoint = self.verified.borrow().clone();
        let (head, boundary) = match self.read().verify_ledger_with_boundary(&checkpoint) {
            Ok(verified) => verified,
            Err(_) => self
                .read()
                .verify_ledger_with_boundary(&LedgerHead::genesis())?,
        };
        self.verified.replace(head.clone());
        self.quantity_boundary.set(boundary);
        Ok(head)
    }

    /// Commit `batch` atomically.
    ///
    /// # Errors
    ///
    /// Any [`CellError`]; on error nothing was written.
    pub fn commit(&mut self, batch: &CommitBatch) -> Result<CommitOutcome, CellError> {
        self.commit_probed(batch, &mut |_point| Ok(()))
    }

    /// [`Self::commit`] with a fault probe called at every [`CommitPoint`]
    /// (deterministic crash simulation). A probe error before
    /// [`CommitPoint::Committed`] rolls the transaction back; at
    /// [`CommitPoint::Committed`] the commit is already durable and the error
    /// models a crash before the caller is acknowledged.
    ///
    /// # Errors
    ///
    /// Any [`CellError`], including the probe's.
    pub fn commit_probed(
        &mut self,
        batch: &CommitBatch,
        probe: &mut dyn FnMut(CommitPoint) -> Result<(), CellError>,
    ) -> Result<CommitOutcome, CellError> {
        match self.commit_guarded(batch, |_reader| Ok::<(), CellError>(()), probe) {
            Ok(((), outcome)) => Ok(outcome),
            Err(GuardedError::Guard(error) | GuardedError::Store(error)) => Err(error),
        }
    }

    /// Commit `batch` after `guard` admits it, inside the same transaction:
    /// the guard reads exactly the state the commit applies to (the native
    /// `CellHost` driver runs the kernel's `check_commit` here), and nothing
    /// can commit in between. An idempotent replay writes nothing and is not
    /// guarded.
    ///
    /// # Errors
    ///
    /// [`GuardedError::Guard`] when the guard refuses; otherwise as
    /// [`Self::commit_probed`].
    pub fn commit_guarded<T: Default, X>(
        &mut self,
        batch: &CommitBatch,
        guard: impl FnOnce(&StoreReader<'_, E>) -> Result<T, X>,
        probe: &mut dyn FnMut(CommitPoint) -> Result<(), CellError>,
    ) -> Result<(T, CommitOutcome), GuardedError<X>> {
        self.commit_guarded_inner(batch, None, None, guard, probe)
    }

    // Only the driver and verified restore can supply pre-materialization input.
    // Public/deserialized batches always derive identity from their own contents.
    pub(crate) fn commit_guarded_submitted<T: Default, X>(
        &mut self,
        batch: &CommitBatch,
        submitted: Option<&Value>,
        guard: impl FnOnce(&StoreReader<'_, E>) -> Result<T, X>,
        probe: &mut dyn FnMut(CommitPoint) -> Result<(), CellError>,
    ) -> Result<(T, CommitOutcome), GuardedError<X>> {
        self.commit_guarded_inner(batch, None, submitted, guard, probe)
    }

    /// Install an already-admitted projected model and its canonical full
    /// definition set. The route performs checker admission; this method owns
    /// the version CAS, orphan checks, DDL, active pointer, ledger event,
    /// receipt, and export journal entry in one transaction.
    ///
    /// # Errors
    ///
    /// A version conflict, unsafe live-data migration, or store error.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "the owned input is retained by the atomic definition-install transaction"
    )]
    pub fn install_definitions(
        &mut self,
        candidate_model: Model,
        input: InstalledDefinitionInput,
        expected_version: Option<u64>,
        scope: &str,
    ) -> Result<InstalledDefinitionSet, CellError> {
        let batch = CommitBatch {
            world: MAIN_WORLD.to_owned(),
            scope: scope.to_owned(),
            logical_time: input.installed_at,
            ..CommitBatch::default()
        };
        let install = DefinitionInstallContext {
            candidate_model: &candidate_model,
            input: &input,
            expected_version,
        };
        let result = self.commit_guarded_inner(
            &batch,
            Some(install),
            None,
            |_reader| Ok::<(), CellError>(()),
            &mut |_point| Ok(()),
        );
        match result {
            Ok(((), CommitOutcome::Committed(receipt))) => {
                self.model = candidate_model;
                self.read()
                    .installed_definitions(Some(receipt.body.definitions_version))?
                    .ok_or_else(|| CellError::LedgerCorrupt {
                        seq: receipt.body.definitions_version,
                        reason: "committed definitions version has no retained set".to_owned(),
                    })
            }
            Ok(((), CommitOutcome::Replayed(_))) => Err(CellError::Engine(
                "definition installation unexpectedly replayed".to_owned(),
            )),
            Err(GuardedError::Guard(error) | GuardedError::Store(error)) => Err(error),
        }
    }

    /// Check a definition candidate against the current version and live
    /// values, returning the exact version/layout preview installation would
    /// retain. This performs SQL reads only and does not enter a transaction.
    ///
    /// # Errors
    /// A version conflict, unsafe live-data migration, or store error.
    pub fn check_definitions(
        &self,
        candidate_model: &Model,
        input: &InstalledDefinitionInput,
        expected_version: Option<u64>,
    ) -> Result<InstalledDefinitionSet, CellError> {
        let install = DefinitionInstallContext {
            candidate_model,
            input,
            expected_version,
        };
        let actual_version = active_definition_version(&self.conn)?;
        Ok(
            preflight_definition_install(&self.conn, &self.model, &install, actual_version)?
                .installed,
        )
    }

    #[allow(
        clippy::too_many_lines,
        reason = "the shared atomic commit transaction keeps receipt, ledger and install writes together"
    )]
    fn commit_guarded_inner<T: Default, X>(
        &mut self,
        batch: &CommitBatch,
        installation: Option<DefinitionInstallContext<'_>>,
        submitted: Option<&Value>,
        guard: impl FnOnce(&StoreReader<'_, E>) -> Result<T, X>,
        probe: &mut dyn FnMut(CommitPoint) -> Result<(), CellError>,
    ) -> Result<(T, CommitOutcome), GuardedError<X>> {
        let admitted_boundary = Cell::new(self.quantity_boundary.get());
        let result = self.conn.transaction(|| -> Result<_, GuardedError<X>> {
            probe(CommitPoint::Begun)?;
            let actual_version = active_definition_version(&self.conn)?;
            let (version, install_record, synthetic_batch) = if let Some(install) = &installation {
                let preflight = preflight_definition_install(
                    &self.conn,
                    &self.model,
                    install,
                    actual_version,
                )?;
                crate::migration::apply_model_migration(
                    &self.conn,
                    &self.model,
                    install.candidate_model,
                    &preflight.migration,
                )?;
                apply_marking_definition_changes(
                    &self.conn,
                    &self.model,
                    install.candidate_model,
                )?;
                let version = preflight.installed.version;
                let mut event_batch = batch.clone();
                event_batch.events = vec![definition_event(
                    version,
                    actual_version,
                    &install.input.receipt_sha256,
                    &install.input.registry,
                    &install.input.diff,
                    install.input.readmission.as_ref(),
                )?];
                let record = DefinitionInstallRecord {
                    installed: preflight.installed,
                    model: install.candidate_model.definitions_snapshot(),
                    diff: install.input.diff.clone(),
                    readmission: install.input.readmission.clone(),
                };
                (version, Some(record), Some(event_batch))
            } else {
                (actual_version.unwrap_or(0), None, None)
            };
            let effective_batch = synthetic_batch.as_ref().unwrap_or(batch);
            let prior_receipt = effective_batch.idempotency_key.as_ref()
                .map(|key| receipt_by_key(&self.conn, &effective_batch.world, key))
                .transpose()?.flatten();
            crate::quantity::clock_floor(&self.conn)?;
            // Reconcile host time only after the transaction-local retry lookup.
            let mut reconciled = effective_batch.clone();
            if let Some(prior) = &prior_receipt
                && prior.body.quantity.is_some() != reconciled.quantity.is_some()
            {
                return Err(CellError::IdempotencyConflict(reconciled.idempotency_key.unwrap_or_default()).into());
            }
            if let Some(prior) = &prior_receipt && let Some(request) = &reconciled.quantity {
                reconciled.logical_time = prior.body.logical_time;
                crate::quantity::check_request(&self.conn, &reconciled, request)?;
            }
            let quantity_input = if reconciled.quantity.is_some() {
                Some(match submitted {
                    Some(input) => input.clone(),
                    None => crate::quantity::retry_input(effective_batch)?,
                })
            } else {
                None
            };
            if let Some(input) = &quantity_input {
                let pinned_version = prior_receipt.as_ref()
                    .map_or(version, |prior| prior.body.definitions_version);
                reconciled.quantity_intent = Some(crate::quantity::retry_identity(input, pinned_version)?);
            }
            if let Some(prior) = &prior_receipt && reconciled.quantity.is_some() {
                reconciled.logical_time = prior.body.logical_time;
                if reconciled.quantity_intent != prior.body.quantity_intent || reconciled.quantity != prior.body.quantity {
                    return Err(CellError::IdempotencyConflict(reconciled.idempotency_key.unwrap_or_default()).into());
                }
                let request = reconciled.quantity.as_ref().ok_or(CellError::Quantity(norn_kernel::quantity::QuantityError::Corrupt))?;
                crate::quantity::verify_replay(&self.conn, &reconciled, request)?;
                return Ok((T::default(), CommitOutcome::Replayed(prior.clone())));
            }
            let effective_batch = &reconciled;
            let boundary = crate::quantity::admission_boundary(&self.conn, self.quantity_boundary.get())?;
            let (quantity_batch, new_boundary) = crate::quantity::admit_events(
                &self.conn, &self.model, effective_batch, boundary, prior_receipt.as_ref(),
            )?;
            admitted_boundary.set(boundary.or(new_boundary));
            let effective_batch = &quantity_batch;
            let request_hash = hash_json(effective_batch)?;
            if let Some(key) = &batch.idempotency_key
                && let Some(receipt) = prior_receipt
            {
                if receipt.body.request_hash == request_hash {
                    if let Some(request) = &effective_batch.quantity {
                        crate::quantity::verify_replay(&self.conn, effective_batch, request)?;
                    }
                    return Ok((T::default(), CommitOutcome::Replayed(receipt)));
                }
                return Err(CellError::IdempotencyConflict(key.clone()).into());
            }
            let reader = StoreReader::new(&self.conn, &self.model);
            if effective_batch.logical_time < reader.clock_floor()? {
                return Err(CellError::Quantity(norn_kernel::quantity::QuantityError::ClockRegression).into());
            }
            let admitted = guard(&reader).map_err(GuardedError::Guard)?;
            let consumed_input = export_journal::consumed_input(&reader, effective_batch.consumes.as_ref())?;
            let commit_seq = reader.next_commit_seq()?;
            let commit_model = installation
                .as_ref()
                .map_or(&self.model, |install| install.candidate_model);
            let mut writer = Writer::new(&self.conn, commit_model, effective_batch, commit_seq);

            let install_record = if let (Some(install), Some(mut record)) =
                (installation.as_ref(), install_record)
            {
                record.installed.commit_seq = commit_seq;
                self.conn.execute(
                    &format!(
                        "INSERT INTO \"{DEFINITION_VERSION}\" (\"version\", \"receipt_sha256\", \"canonical_set\", \"model_hash\", \"ddl_hash\", \"installed_at\", \"commit_seq\") VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"
                    ),
                    params![
                        to_i64(version, "definitions version")?,
                        record.installed.receipt_sha256,
                        record.installed.canonical_set,
                        record.installed.model_hash,
                        record.installed.ddl_hash,
                        to_i64(record.installed.installed_at, "definitions installed_at")?,
                        to_i64(commit_seq, "commit_seq")?,
                    ],
                )?;
                let registry_json = serde_json::to_string(&record.installed.registry)
                    .map_err(CellError::Json)?;
                self.conn.execute(
                    &format!(
                        "INSERT INTO \"{DEFINITION_REGISTRY}\" (\"version\", \"registry\") VALUES (?1, ?2)"
                    ),
                    params![to_i64(version, "definitions version")?, registry_json],
                )?;
                if let Some(receipt) = &record.installed.admission_receipt {
                    self.conn.execute(
                        &format!(
                            "INSERT INTO \"{DEFINITION_ADMISSION_RECEIPT}\" (\"version\", \"receipt\") VALUES (?1, ?2)"
                        ),
                        params![to_i64(version, "definitions version")?, receipt],
                    )?;
                }
                if let Some(layout) = &record.installed.layout {
                    self.conn.execute(
                        &format!(
                            "INSERT INTO \"{DEFINITION_LAYOUT}\" (\"version\", \"layout_revision\", \"sidecar\") VALUES (?1, ?2, ?3)"
                        ),
                        params![
                            to_i64(version, "definitions version")?,
                            to_i64(record.installed.layout_revision, "layout revision")?,
                            layout,
                        ],
                    )?;
                }
                self.conn.execute(
                    &format!("DELETE FROM \"{DEFINITION_ACTIVE}\" WHERE \"id\" = 1"),
                    params![],
                )?;
                self.conn.execute(
                    &format!(
                        "INSERT INTO \"{DEFINITION_ACTIVE}\" (\"id\", \"version\") VALUES (1, ?1)"
                    ),
                    params![to_i64(version, "definitions version")?],
                )?;
                self.conn.execute(
                    &format!("UPDATE \"{META}\" SET \"value\" = ?1 WHERE \"key\" = 'ddl_hash'"),
                    params![install.candidate_model.ddl_hash()],
                )?;
                Some(record)
            } else {
                None
            };

            let written = writer.apply_writes()?;
            let quantity_result = writer.apply_quantity()?;
            probe(CommitPoint::FactsApplied)?;
            let linked = writer.apply_links()?;
            probe(CommitPoint::LinksApplied)?;
            if let Some(validate) = self.candidate_validator.get() {
                validate(
                    &StoreReader::new(&self.conn, commit_model),
                    effective_batch,
                    installation.is_some(),
                )?;
            }
            let (first_event_seq, ledger_head) = writer.append_events()?;
            if let Some(boundary) = new_boundary {
                crate::quantity::establish_boundary(&self.conn, boundary)?;
            }
            probe(CommitPoint::EventsAppended)?;
            writer.apply_effects()?;
            probe(CommitPoint::EffectsEnqueued)?;

            let mut body = ReceiptBody {
                commit_seq,
                definitions_version: version,
                world: effective_batch.world.clone(),
                logical_time: effective_batch.logical_time,
                idempotency_key: effective_batch.idempotency_key.clone(),
                request_hash,
                quantity: effective_batch.quantity.clone(),
                quantity_result,
                quantity_intent: effective_batch.quantity_intent.clone(),
                consumed: effective_batch.consumes.clone(),
                written,
                linked,
                first_event_seq,
                event_count: u64::try_from(effective_batch.events.len())
                    .map_err(|_overflow| CellError::OutOfRange("event count"))?,
                ledger_head,
                enqueued: effective_batch
                    .effects
                    .iter()
                    .filter_map(|effect| match effect {
                        EffectRow::Enqueue { command } => Some(command.command_id.clone()),
                        EffectRow::ClaimPerform { .. }
                        | EffectRow::Schedule { .. }
                        | EffectRow::CancelTimer { .. } => None,
                    })
                    .collect(),
                scheduled: effective_batch
                    .effects
                    .iter()
                    .filter_map(|effect| match effect {
                        EffectRow::Schedule { timer } => Some(timer.timer_id.clone()),
                        EffectRow::Enqueue { .. }
                        | EffectRow::ClaimPerform { .. }
                        | EffectRow::CancelTimer { .. } => None,
                    })
                    .collect(),
            };
            if let Some((id, reservation)) = body.quantity_result.as_ref().and_then(|result| result.reservation.as_ref())
                && reservation.status == "open" {
                let request = effective_batch.quantity.as_ref().ok_or(CellError::Quantity(norn_kernel::quantity::QuantityError::Corrupt))?;
                body.scheduled.push(crate::quantity::expiry_timer_id(&effective_batch.world, &request.ledger_id, id)?);
            }
            let receipt = writer.write_receipt(body)?;
            self.conn.execute(r#"UPDATE "_norn_clock" SET logical_time=?1,commit_seq=?2,receipt_hash=?3 WHERE id=1"#,
                params![to_i64(receipt.body.logical_time, "cell clock")?, to_i64(receipt.body.commit_seq, "clock commit")?, receipt.hash])?;
            export_journal::record(
                &self.conn,
                commit_model,
                effective_batch,
                quantity_input,
                &receipt,
                consumed_input,
                install_record,
            )?;
            probe(CommitPoint::ReceiptWritten)?;
            Ok((admitted, CommitOutcome::Committed(receipt)))
        })?;
        self.quantity_boundary.set(admitted_boundary.get());
        match result {
            (admitted, CommitOutcome::Replayed(receipt)) => {
                Ok((admitted, CommitOutcome::Replayed(receipt)))
            }
            (admitted, CommitOutcome::Committed(receipt)) => {
                #[cfg(feature = "native")]
                self.export_notify.notify_one();
                tracing::debug!(
                    commit_seq = receipt.body.commit_seq,
                    events = receipt.body.event_count,
                    writes = batch.writes.len(),
                    facts = batch.facts.len(),
                    "cell store commit"
                );
                probe(CommitPoint::Committed).map_err(GuardedError::Store)?;
                Ok((admitted, CommitOutcome::Committed(receipt)))
            }
        }
    }

    /// Record the result of performing the outbox head command `command_id`
    /// at `generation`, in its own transaction, so a crash before the result
    /// is consumed never executes the command again. Returns `false` (and
    /// writes nothing) when that command is not the head or already has a
    /// recorded result.
    ///
    /// # Errors
    ///
    /// SQL engine errors.
    pub fn record_performed(
        &mut self,
        command_id: &str,
        generation: u64,
        outcome: &Value,
    ) -> Result<bool, CellError> {
        self.conn.transaction(|| -> Result<bool, CellError> {
            let head = StoreReader::new(&self.conn, &self.model).head_command()?;
            let is_head = head.as_ref().is_some_and(|head| {
                head.command.command_id == command_id
                    && head.command.generation == generation
                    && head.result.is_none()
            });
            if !is_head {
                return Ok(false);
            }
            let head = head.ok_or_else(|| CellError::Encoding("pending result command disappeared".to_owned()))?;
            crate::payload_storage::remember_command(&self.conn, command_id, generation, head.commit_seq)?;
            let outcome = match self.export_record(head.commit_seq)? {
                Some(record) => crate::payload_storage::encode_result(&self.conn, &record.batch.scope, &record.batch.world, outcome)?,
                None => canonical_json(outcome)?,
            };
            let generation = to_i64(generation, "generation")?;
            self.conn.execute(
                &format!(
                    "UPDATE \"{OUTBOX}\" SET \"result\" = ?1 WHERE \"command_id\" = ?2 AND \"generation\" = ?3"
                ),
                params![outcome, command_id, generation],
            )?;
            let attempts: i64 = self.conn.query_row(
                &format!(
                    "SELECT COUNT(*) FROM \"{RUN}\" WHERE \"command_id\" = ?1 AND \"generation\" = ?2"
                ),
                params![command_id, generation],
                |row| row.get::<i64>(0),
            )?;
            let attempt = to_u64(attempts, "attempt")?
                .checked_add(1)
                .ok_or(CellError::OutOfRange("attempt"))?;
            self.conn.execute(
                &format!(
                    "INSERT INTO \"{RUN}\" (\"command_id\", \"generation\", \"attempt\", \"outcome\", \"consumed_commit_seq\") VALUES (?1, ?2, ?3, ?4, NULL)"
                ),
                params![command_id, generation, to_i64(attempt, "attempt")?, outcome],
            )?;
            Ok(true)
        })
    }

    /// Ingest one external observation into the inbox, in its own
    /// transaction. An id ingested before (pending or consumed) is ignored;
    /// returns whether it was accepted. The canonical payload, including
    /// provenance, is bounded by [`MAX_OBSERVATION_BYTES`].
    ///
    /// # Errors
    ///
    /// Serialization, observation-size or queue-position limits, or SQL errors.
    pub fn inject(&mut self, observation_id: &str, observed: &Value) -> Result<bool, CellError> {
        self.conn.transaction(|| -> Result<bool, CellError> {
            let seen: Option<i64> = self.conn.query_optional(
                &format!("SELECT 1 FROM \"{INBOX}\" WHERE \"observation_id\" = ?1"),
                params![observation_id],
                |row| row.get::<i64>(0),
            )?;
            if seen.is_some() {
                return Ok(false);
            }
            let serialized = canonical_json(observed)?;
            if serialized.len() > MAX_OBSERVATION_BYTES {
                return Err(CellError::OutOfRange("observation bytes"));
            }
            let maximum: i64 = self.conn.query_row(
                &format!("SELECT COALESCE(MAX(\"position\"), 0) FROM \"{INBOX}\""),
                params![],
                |row| row.get::<i64>(0),
            )?;
            let position = to_i64(
                next(to_u64(maximum, "inbox position")?, "inbox position")?,
                "inbox position",
            )?;
            self.conn.execute(
                &format!(
                    "INSERT INTO \"{INBOX}\" (\"position\", \"observation_id\", \"observed\", \"consumed_commit_seq\") VALUES (?1, ?2, ?3, NULL)"
                ),
                params![position, observation_id, serialized],
            )?;
            Ok(true)
        })
    }
}
