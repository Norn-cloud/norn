//! Ledger, receipt, definition-version, and verification reads.

use std::collections::BTreeSet;

use norn_ir::RuntimeValue as Value;
use serde::{Deserialize, Serialize};

use super::{StoreReader, decode_receipts, entry_hash, next, receipt_by_key, to_i64, to_u64};
use crate::codec::hash_json;
use crate::engine::{MAX_SAFE_INTEGER, SqlEngine, SqlRow, params};
use crate::error::CellError;
use crate::payload_storage::EVENT_BYTES;
use crate::reactive::DecodeBudget;
use crate::store::{InstalledDefinitionSet, Receipt, ReceiptBody};
use crate::system::{
    DEFINITION_ACTIVE, DEFINITION_ADMISSION_RECEIPT, DEFINITION_LAYOUT, DEFINITION_REGISTRY,
    DEFINITION_VERSION, GENESIS_HASH, LEDGER, RECEIPT,
};

/// One ledger entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LedgerEntry {
    /// 1-based, gapless sequence number.
    pub seq: u64,
    /// The commit that appended it.
    pub commit_seq: u64,
    /// The world.
    pub world: String,
    /// The event.
    pub event: Value,
    /// The previous entry's hash ([`GENESIS_HASH`] for the first).
    pub prev_hash: String,
    /// Norn1 hash of `{seq, commitSeq, world, event, prev}`.
    pub hash: String,
}

impl LedgerEntry {
    /// The business trigger evaluated by rules, without host causality.
    /// Observations stand for their native fact event at this boundary.
    pub fn rule_input(&self) -> Value {
        let mut event = match self.event.get("op").and_then(Value::as_str) {
            Some("observed_fact") => self.event.get("event").unwrap_or(&self.event).clone(),
            Some("observed_deleted") => norn_ir::runtime_object! {
                "op" => "deleted",
                "key" => self.event.get("key"),
            },
            Some("observed_transition") => norn_ir::runtime_object! {
                "op" => "transitioned",
                "key" => self.event.get("key"),
                "lifecycle" => self.event.get("property"),
                "transition" => self.event.get("transition"),
                "from" => self.event.get("from"),
                "to" => self.event.get("to"),
            },
            _ => self.event.clone(),
        };
        if let Some(object) = event.as_object_mut() {
            object.remove("causality");
        }
        event
    }
}

/// The verified ledger head.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LedgerHead {
    /// The last event sequence number (`0` when empty).
    pub seq: u64,
    /// The last event hash ([`GENESIS_HASH`] when empty).
    pub hash: String,
    /// How many commits.
    pub commits: u64,
}

impl LedgerHead {
    /// The head of an empty ledger: verification from it is a full one.
    #[must_use]
    pub fn genesis() -> Self {
        Self {
            seq: 0,
            hash: GENESIS_HASH.to_owned(),
            commits: 0,
        }
    }
}

pub(crate) fn active_definition_version(engine: &impl SqlEngine) -> Result<Option<u64>, CellError> {
    let version: Option<i64> = engine.query_optional(
        &format!("SELECT \"version\" FROM \"{DEFINITION_ACTIVE}\" WHERE \"id\" = 1"),
        params![],
        |row| row.get(0),
    )?;
    version
        .map(|version| to_u64(version, "definitions version"))
        .transpose()
}

pub(crate) fn validate_active_definition_pointer(
    engine: &impl SqlEngine,
) -> Result<Option<u64>, CellError> {
    // One statement pins both values to the same SQLite snapshot, including
    // when another CellStore installs definitions on a separate connection.
    let (active, latest): (Option<i64>, Option<i64>) = engine.query_row(
        &format!(
            "SELECT (SELECT \"version\" FROM \"{DEFINITION_ACTIVE}\" WHERE \"id\" = 1), \
             (SELECT \"version\" FROM \"{DEFINITION_VERSION}\" ORDER BY \"version\" DESC LIMIT 1)"
        ),
        params![],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let active = active
        .map(|version| to_u64(version, "definitions version"))
        .transpose()?;
    let latest = latest
        .map(|version| to_u64(version, "definitions version"))
        .transpose()?;
    match (active, latest) {
        (None, None) => Ok(None),
        (Some(active), Some(latest)) if active == latest && active > 0 => Ok(Some(active)),
        (active, latest) => Err(CellError::LedgerCorrupt {
            seq: active.or(latest).unwrap_or(0),
            reason: "active definitions pointer does not name the latest retained version"
                .to_owned(),
        }),
    }
}

pub(crate) fn definition_by_version(
    engine: &impl SqlEngine,
    version: u64,
) -> Result<Option<InstalledDefinitionSet>, CellError> {
    let version_sql = to_i64(version, "definitions version")?;
    let row = engine.query_optional(
        &format!(
            "SELECT \"version\", \"receipt_sha256\", \"canonical_set\", \"model_hash\", \"ddl_hash\", \"installed_at\", \"commit_seq\" FROM \"{DEFINITION_VERSION}\" WHERE \"version\" = ?1"
        ),
        params![version_sql],
        |row| {
            Ok(InstalledDefinitionSet {
                version: to_u64(row.get(0)?, "definitions version")?,
                receipt_sha256: row.get(1)?,
                admission_receipt: None,
                canonical_set: row.get(2)?,
                registry: Vec::new(),
                model_hash: row.get(3)?,
                ddl_hash: row.get(4)?,
                installed_at: to_u64(row.get(5)?, "definitions installed_at")?,
                commit_seq: to_u64(row.get(6)?, "definitions commit_seq")?,
                layout: None,
                layout_revision: 0,
            })
        },
    )?;
    let Some(mut installed) = row else {
        return Ok(None);
    };
    if definition_registry_table_exists(engine)? {
        installed.registry = definition_registry_for_version(engine, version)?;
    }
    if definition_admission_receipt_table_exists(engine)? {
        installed.admission_receipt = definition_admission_receipt_for_version(engine, version)?;
    }
    (installed.layout, installed.layout_revision) = definition_layout_for_version(engine, version)?;
    Ok(Some(installed))
}

fn definition_layout_for_version(
    engine: &impl SqlEngine,
    version: u64,
) -> Result<(Option<String>, u64), CellError> {
    if !definition_layout_table_exists(engine)? {
        return Ok((None, 0));
    }
    let row = engine.query_optional(
        &format!(
            "SELECT \"layout_revision\", \"sidecar\" FROM \"{DEFINITION_LAYOUT}\" WHERE \"version\" = ?1"
        ),
        params![to_i64(version, "definitions version")?],
        |row| {
            Ok((
                to_u64(row.get(0)?, "layout revision")?,
                row.get::<String>(1)?,
            ))
        },
    )?;
    Ok(row.map_or((None, 0), |(revision, sidecar)| (Some(sidecar), revision)))
}

fn definition_layout_table_exists(engine: &impl SqlEngine) -> Result<bool, CellError> {
    let exists = engine
        .query_optional(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            params![DEFINITION_LAYOUT],
            |row| row.get::<i64>(0),
        )?
        .is_some();
    Ok(exists)
}

fn definition_registry_table_exists(engine: &impl SqlEngine) -> Result<bool, CellError> {
    let exists = engine
        .query_optional(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            params![DEFINITION_REGISTRY],
            |row| row.get::<i64>(0),
        )?
        .is_some();
    Ok(exists)
}

fn definition_admission_receipt_table_exists(engine: &impl SqlEngine) -> Result<bool, CellError> {
    let exists = engine
        .query_optional(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            params![DEFINITION_ADMISSION_RECEIPT],
            |row| row.get::<i64>(0),
        )?
        .is_some();
    Ok(exists)
}

fn definition_admission_receipt_for_version(
    engine: &impl SqlEngine,
    version: u64,
) -> Result<Option<String>, CellError> {
    engine.query_optional(
        &format!(
            "SELECT \"receipt\" FROM \"{DEFINITION_ADMISSION_RECEIPT}\" WHERE \"version\" = ?1"
        ),
        params![to_i64(version, "definitions version")?],
        |row| row.get::<String>(0),
    )
}

fn definition_registry_for_version(
    engine: &impl SqlEngine,
    version: u64,
) -> Result<Vec<String>, CellError> {
    let registry = engine.query_optional(
        &format!("SELECT \"registry\" FROM \"{DEFINITION_REGISTRY}\" WHERE \"version\" = ?1"),
        params![to_i64(version, "definitions version")?],
        |row| row.get::<String>(0),
    )?;
    let Some(registry) = registry else {
        return Ok(Vec::new());
    };
    Ok(serde_json::from_str(&registry)?)
}

fn definition_from_row(row: &SqlRow) -> Result<InstalledDefinitionSet, CellError> {
    Ok(InstalledDefinitionSet {
        version: to_u64(row.get(0)?, "definitions version")?,
        receipt_sha256: row.get(1)?,
        admission_receipt: None,
        canonical_set: row.get(2)?,
        registry: Vec::new(),
        model_hash: row.get(3)?,
        ddl_hash: row.get(4)?,
        installed_at: to_u64(row.get(5)?, "definitions installed_at")?,
        commit_seq: to_u64(row.get(6)?, "definitions commit_seq")?,
        layout: None,
        layout_revision: 0,
    })
}

impl<E: SqlEngine> StoreReader<'_, E> {
    /// The next commit sequence number.
    pub(crate) fn next_commit_seq(&self) -> Result<u64, CellError> {
        let last: i64 = self.conn.query_row(
            &format!("SELECT COALESCE(MAX(\"commit_seq\"), 0) FROM \"{RECEIPT}\""),
            params![],
            |row| row.get(0),
        )?;
        next(to_u64(last, "commit_seq")?, "commit_seq")
    }

    /// The ledger head: last sequence number and hash.
    pub(crate) fn ledger_tail(&self) -> Result<(u64, String), CellError> {
        let head: Option<(i64, String)> = self.conn.query_optional(
            &format!("SELECT \"seq\", \"hash\" FROM \"{LEDGER}\" ORDER BY \"seq\" DESC LIMIT 1"),
            params![],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        match head {
            Some((seq, hash)) => Ok((to_u64(seq, "seq")?, hash)),
            None => Ok((0, GENESIS_HASH.to_owned())),
        }
    }

    /// Ledger entries with `seq >= from`, in order.
    ///
    /// # Errors
    ///
    /// SQL engine or decode errors.
    pub fn ledger(&self, from: u64) -> Result<Vec<LedgerEntry>, CellError> {
        self.ledger_range(from, None)
    }

    /// At most `limit` ledger entries with `seq > after`, in order: one page,
    /// read with a bounded query (#2970), so a caller never loads the whole
    /// backlog to take a batch.
    ///
    /// # Errors
    ///
    /// SQL engine or decode errors.
    pub fn ledger_page(&self, after: u64, limit: usize) -> Result<Vec<LedgerEntry>, CellError> {
        // No stored sequence exceeds the host's exact-integer bound, so a cursor at or past it
        // has nothing after it.
        match i64::try_from(after) {
            Ok(after) if after < MAX_SAFE_INTEGER => {}
            _ => return Ok(Vec::new()),
        }
        self.ledger_range(after.saturating_add(1), Some(limit))
    }

    /// Read a ledger page for projection, masking every unproven payload.
    /// The stored hashes still refer to the original durability records.
    ///
    /// # Errors
    /// Storage failures or inconsistent value commitments.
    pub fn verified_ledger_page(
        &self,
        after: u64,
        limit: usize,
    ) -> Result<Vec<LedgerEntry>, CellError> {
        self.project_history_entries(self.ledger_page(after, limit)?)
    }

    /// Budgeted counterpart of `verified_ledger_page`; values pass the same accessor.
    ///
    /// # Errors
    /// Storage failures or inconsistent value commitments.
    pub fn verified_ledger_page_with_budget(
        &self,
        after: u64,
        limit: usize,
        budget: &mut crate::reactive::DecodeBudget,
    ) -> Result<Option<Vec<LedgerEntry>>, CellError> {
        self.ledger_page_with_budget(after, limit, budget)?
            .map(|entries| self.project_history_entries(entries))
            .transpose()
    }

    fn project_history_entries(
        &self,
        entries: Vec<LedgerEntry>,
    ) -> Result<Vec<LedgerEntry>, CellError> {
        entries
            .into_iter()
            .map(|mut entry| {
                entry.event = self
                    .verified_history_event(&entry)?
                    .unwrap_or_else(|| norn_ir::runtime_object! { "masked" => true });
                Ok(entry)
            })
            .collect()
    }

    /// Resolve the complete immutable classifications of an event's receipt.
    /// Missing attribution or any incomplete fact snapshot returns `None`.
    /// Current visibility and declassification never rewrite this lineage.
    ///
    /// # Errors
    /// Storage or classification decode errors.
    pub fn ledger_markings(
        &self,
        entry: &LedgerEntry,
    ) -> Result<Option<BTreeSet<String>>, CellError> {
        if self.verified_history_event(entry)?.is_none() {
            return Ok(None);
        }
        let Some(receipt) = self.receipt_at(entry.commit_seq)? else {
            return Ok(None);
        };
        let body = receipt.body;
        if body.world != entry.world
            || entry.seq < body.first_event_seq
            || entry.seq.saturating_sub(body.first_event_seq) >= body.event_count
        {
            return Ok(None);
        }
        // Definition-install metadata is attributed to retained declarative
        // definitions, rather than to a business fact. An ordinary event with
        // the same name cannot borrow an earlier install's public lineage.
        if entry.event.get("type").and_then(Value::as_str) == Some("definitions.installed")
            && body.definitions_version != 0
            && let Some(installed) = self.installed_definitions(Some(body.definitions_version))?
            && installed.commit_seq == entry.commit_seq
            && entry
                .event
                .get("version")
                .and_then(Value::as_i64)
                .and_then(|version| u64::try_from(version).ok())
                == Some(installed.version)
            && entry.event.get("receiptSha256").and_then(Value::as_str)
                == Some(installed.receipt_sha256.as_str())
        {
            return Ok(Some(BTreeSet::new()));
        }
        let mut written = body.written;
        // Visibility-only changes emit no fact version. Their audit payload
        // is still bound to the exact historical subject they authorized.
        if entry.event.get("type").and_then(Value::as_str) == Some("marking.declassified") {
            let Some(object_type) = entry.event.get("objectType").and_then(Value::as_str) else {
                return Ok(None);
            };
            let Some(key) = entry.event.get("key").and_then(Value::as_str) else {
                return Ok(None);
            };
            let Some(version) = entry
                .event
                .get("expectedVersion")
                .and_then(Value::as_i64)
                .and_then(|v| u64::try_from(v).ok())
            else {
                return Ok(None);
            };
            written.push(crate::store::WrittenRow {
                object_type: object_type.to_owned(),
                key: key.to_owned(),
                version,
                snapshot_sha256: None,
            });
        }
        if written.is_empty() {
            return Ok(None);
        }
        let mut labels = BTreeSet::new();
        for row in written {
            let Some(snapshot) = self.effective_markings(
                &entry.world,
                &row.object_type,
                &row.key,
                Some(row.version),
            )?
            else {
                return Ok(None);
            };
            labels.extend(snapshot.into_values().flatten());
            let Some(occurrence) = self.fact_occurrence_markings(
                &entry.world,
                &row.object_type,
                &row.key,
                Some(row.version),
            )?
            else {
                return Ok(None);
            };
            labels.extend(occurrence);
        }
        Ok(Some(labels))
    }

    /// Immutable source classifications of a typed fact trigger. Other writes
    /// in the receipt (such as raw staging records) were not evaluated as the
    /// fact event's values and do not classify those values. Unattributed event
    /// kinds fall back to the complete receipt's conservative lineage.
    ///
    /// # Errors
    /// Storage or classification decode errors.
    pub fn fact_event_markings(
        &self,
        entry: &LedgerEntry,
    ) -> Result<Option<BTreeSet<String>>, CellError> {
        let Some(verified) = self.verified_history_event(entry)? else {
            return Ok(None);
        };
        let mut projected = entry.clone();
        projected.event = verified;
        let event = entry.rule_input();
        // A projection can omit unproven envelope text. Raw rule evaluation
        // must not inherit public labels for text absent from that proof.
        if projected.rule_input() != event {
            return Ok(None);
        }
        if !matches!(
            event.get("op").and_then(Value::as_str),
            Some("created" | "set" | "modified" | "deleted" | "transitioned")
        ) {
            return self.ledger_markings(entry);
        }
        let Some(raw_key) = event.get("key") else {
            return Ok(None);
        };
        let Ok(key) = norn_ir::runtime::decode::<norn_kernel::FactKey>(raw_key) else {
            return Ok(None);
        };
        let object_type = key.object_type.as_str();
        let Some(receipt) = self.receipt_at(entry.commit_seq)? else {
            return Ok(None);
        };
        let body = receipt.body;
        if body.world != entry.world
            || entry.seq < body.first_event_seq
            || entry.seq.saturating_sub(body.first_event_seq) >= body.event_count
        {
            return Ok(None);
        }
        let mut labels = BTreeSet::new();
        let mut attributed = false;
        for row in body
            .written
            .iter()
            .filter(|row| row.key == key.key && self.model.is_a(&row.object_type, object_type))
        {
            let Some(snapshot) = self.effective_markings(
                &entry.world,
                &row.object_type,
                &row.key,
                Some(row.version),
            )?
            else {
                return Ok(None);
            };
            attributed = true;
            labels.extend(snapshot.into_values().flatten());
            let Some(occurrence) = self.fact_occurrence_markings(
                &entry.world,
                &row.object_type,
                &row.key,
                Some(row.version),
            )?
            else {
                return Ok(None);
            };
            labels.extend(occurrence);
        }
        Ok(attributed.then_some(labels))
    }

    /// Read one ledger page only when its entries fit the caller's remaining
    /// row and byte budget. Event lengths are checked before the JSON event
    /// bodies are fetched and decoded. `None` means the next page would exceed
    /// the budget; an empty vector means the ledger is exhausted.
    ///
    /// # Errors
    ///
    /// SQL engine, sequence-range, or event decode errors.
    pub fn ledger_page_with_budget(
        &self,
        after: u64,
        limit: usize,
        budget: &mut DecodeBudget,
    ) -> Result<Option<Vec<LedgerEntry>>, CellError> {
        if limit == 0 {
            return Ok(Some(Vec::new()));
        }
        if i64::try_from(after).is_err() || after >= u64::try_from(MAX_SAFE_INTEGER).unwrap_or(0) {
            return Ok(Some(Vec::new()));
        }
        if limit > budget.remaining_rows() {
            return Ok(None);
        }
        let page_limit_sql = i64::try_from(limit).unwrap_or(MAX_SAFE_INTEGER);
        let length_rows = self.conn.query(
            &format!(
                "SELECT \"seq\", COALESCE(octet_length(\"world\"), 0), COALESCE({EVENT_BYTES}, 0), COALESCE(octet_length(\"prev_hash\"), 0), COALESCE(octet_length(\"hash\"), 0) FROM \"{LEDGER}\" WHERE \"seq\" > ?1 ORDER BY \"seq\" LIMIT ?2"
            ),
            params![to_i64(after, "seq")?, page_limit_sql],
        )?;
        let mut bytes = 0_u64;
        for values in &length_rows {
            let row = SqlRow(values.clone());
            bytes = bytes.saturating_add(64);
            for index in 1..5 {
                bytes =
                    bytes.saturating_add(to_u64(row.get::<i64>(index)?, "ledger field length")?);
            }
        }
        if length_rows.len() > budget.remaining_rows() || !budget.charge(bytes) {
            return Ok(None);
        }
        budget.rows = budget.rows.saturating_sub(length_rows.len());
        self.ledger_page(after, length_rows.len()).map(Some)
    }

    /// The active retained definition version without loading its payload.
    ///
    /// # Errors
    /// SQL errors or a dangling active-version pointer.
    pub fn active_definitions_version(&self) -> Result<Option<u64>, CellError> {
        validate_active_definition_pointer(self.conn)
    }

    /// ADMIT-CACHE-1 (#1533): the active definitions' identity, its version
    /// and admission receipt digest, without loading its payload: what a host
    /// keys the work it does once per definitions version by.
    ///
    /// # Errors
    /// SQL errors or a dangling active-version pointer.
    pub fn active_definitions_identity(&self) -> Result<Option<(u64, String)>, CellError> {
        let Some(version) = validate_active_definition_pointer(self.conn)? else {
            return Ok(None);
        };
        let digest: Option<String> = self.conn.query_optional(
            &format!(
                "SELECT \"receipt_sha256\" FROM \"{DEFINITION_VERSION}\" WHERE \"version\" = ?1"
            ),
            params![to_i64(version, "definitions version")?],
            |row| row.get(0),
        )?;
        let Some(digest) = digest else {
            return Err(CellError::LedgerCorrupt {
                seq: version,
                reason: "active definitions pointer has no retained version".to_owned(),
            });
        };
        Ok(Some((version, digest)))
    }

    /// Whether a retained definition set is installed at `version`, without
    /// loading it: exactly `installed_definitions(Some(version)).is_some()`
    /// for any version at or below the active one.
    ///
    /// # Errors
    /// SQL errors.
    pub(crate) fn definitions_version_retained(&self, version: u64) -> Result<bool, CellError> {
        Ok(self
            .conn
            .query_optional(
                &format!("SELECT 1 FROM \"{DEFINITION_VERSION}\" WHERE \"version\" = ?1"),
                params![to_i64(version, "definitions version")?],
                |row| row.get::<i64>(0),
            )?
            .is_some())
    }

    /// The retained definition set at `version`, or the active set when
    /// `version` is `None`.
    ///
    /// # Errors
    ///
    /// SQL, decoding, or a dangling active-version pointer.
    pub fn installed_definitions(
        &self,
        version: Option<u64>,
    ) -> Result<Option<InstalledDefinitionSet>, CellError> {
        let version = if let Some(version) = version {
            version
        } else {
            let Some(version) = validate_active_definition_pointer(self.conn)? else {
                return Ok(None);
            };
            version
        };
        let installed = definition_by_version(self.conn, version)?;
        if installed.is_none()
            && version == validate_active_definition_pointer(self.conn)?.unwrap_or(0)
        {
            return Err(CellError::LedgerCorrupt {
                seq: version,
                reason: "active definitions pointer has no retained version".to_owned(),
            });
        }
        Ok(installed)
    }

    /// The active definition version in force at `commit_seq`, derived from
    /// the retained version rows rather than from user-emittable ledger data.
    ///
    /// # Errors
    ///
    /// SQL, decoding, sequence-range, or receipt-integrity errors.
    pub fn definitions_version_at(&self, commit_seq: u64) -> Result<Option<u64>, CellError> {
        if commit_seq == 0 {
            return Ok(None);
        }
        let row: Option<(i64, String, String)> = self.conn.query_optional(
            &format!(
                "SELECT \"commit_seq\", \"receipt\", \"receipt_hash\" FROM \"{RECEIPT}\" WHERE \"commit_seq\" <= ?1 ORDER BY \"commit_seq\" DESC LIMIT 1"
            ),
            params![to_i64(commit_seq, "commit_seq")?],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        let Some((stored_seq, serialized, stored_hash)) = row else {
            return Ok(None);
        };
        let stored_seq = to_u64(stored_seq, "commit_seq")?;
        let body: ReceiptBody = serde_json::from_str(&serialized)?;
        if body.commit_seq != stored_seq || stored_hash != hash_json(&body)? {
            return Err(CellError::LedgerCorrupt {
                seq: stored_seq,
                reason: "definitions-version receipt does not verify".to_owned(),
            });
        }
        Ok((body.definitions_version != 0).then_some(body.definitions_version))
    }

    /// All retained definition versions in ascending order.
    ///
    /// # Errors
    ///
    /// SQL or decoding errors.
    pub fn installed_definition_versions(&self) -> Result<Vec<InstalledDefinitionSet>, CellError> {
        let rows = self.conn.query(
            &format!(
                "SELECT \"version\", \"receipt_sha256\", \"canonical_set\", \"model_hash\", \"ddl_hash\", \"installed_at\", \"commit_seq\" FROM \"{DEFINITION_VERSION}\" ORDER BY \"version\""
            ),
            params![],
        )?;
        let has_registry_table = definition_registry_table_exists(self.conn)?;
        let has_admission_receipt_table = definition_admission_receipt_table_exists(self.conn)?;
        rows.into_iter()
            .map(|values| {
                let mut installed = definition_from_row(&SqlRow(values))?;
                if has_registry_table {
                    installed.registry =
                        definition_registry_for_version(self.conn, installed.version)?;
                }
                if has_admission_receipt_table {
                    installed.admission_receipt =
                        definition_admission_receipt_for_version(self.conn, installed.version)?;
                }
                (installed.layout, installed.layout_revision) =
                    definition_layout_for_version(self.conn, installed.version)?;
                Ok(installed)
            })
            .collect()
    }

    fn ledger_range(&self, from: u64, limit: Option<usize>) -> Result<Vec<LedgerEntry>, CellError> {
        self.ledger_range_with(from, limit, true)
    }

    fn ledger_range_with(
        &self,
        from: u64,
        limit: Option<usize>,
        verify: bool,
    ) -> Result<Vec<LedgerEntry>, CellError> {
        // SQLite: a negative LIMIT is no limit.
        let limit = match limit {
            Some(limit) => i64::try_from(limit)
                .unwrap_or(MAX_SAFE_INTEGER)
                .min(MAX_SAFE_INTEGER),
            None => -1,
        };
        let rows = self.conn.query(
            &format!(
                "SELECT \"seq\", \"commit_seq\", \"world\", \"event\", \"prev_hash\", \"hash\" FROM \"{LEDGER}\" WHERE \"seq\" >= ?1 ORDER BY \"seq\" LIMIT ?2"
            ),
            params![to_i64(from, "seq")?, limit],
        )?;
        let mut out = Vec::new();
        for values in rows {
            let row = SqlRow(values);
            let event: String = row.get(3)?;
            let entry = LedgerEntry {
                seq: to_u64(row.get(0)?, "seq")?,
                commit_seq: to_u64(row.get(1)?, "commit_seq")?,
                world: row.get(2)?,
                event: crate::payload_storage::decode_event(
                    self.conn,
                    to_u64(row.get(0)?, "seq")?,
                    &row.get::<String>(2)?,
                    &event,
                )?,
                prev_hash: row.get(4)?,
                hash: row.get(5)?,
            };
            // Result placeholders must never escape when their encoding index
            // is missing or changed. Valid legacy lookalikes retain their own hash.
            if entry.event.get("type").and_then(Value::as_str) == Some("command.result")
                && entry_hash(
                    entry.seq,
                    entry.commit_seq,
                    &entry.world,
                    &entry.event,
                    &entry.prev_hash,
                )? != entry.hash
            {
                return Err(CellError::LedgerCorrupt {
                    seq: entry.seq,
                    reason: "command result payload does not verify".to_owned(),
                });
            }
            if verify {
                self.verify_durable_event(&entry.world, entry.commit_seq, &entry.event)
                    .map_err(|error| unproven_at(&entry, error))?;
            }
            out.push(entry);
        }
        Ok(out)
    }

    /// #1423: the first ledger entry, in sequence order, whose committed value
    /// proof the store refuses ([`CellError::HistoricalValueUnproven`] or
    /// [`CellError::LedgerCorrupt`]), with its refusal; `None` when every entry
    /// is proven. Read-only: an operator's diagnosis of a cell its open refuses.
    ///
    /// # Errors
    /// SQL engine or decode errors, which are never read as a refusal.
    #[cfg(feature = "native")]
    pub(crate) fn first_unproven_entry(
        &self,
    ) -> Result<Option<(LedgerEntry, CellError)>, CellError> {
        const PAGE: usize = 512;
        let mut from = 1_u64;
        loop {
            let page = self.ledger_range_with(from, Some(PAGE), false)?;
            let Some(last) = page.last().map(|entry| entry.seq) else {
                return Ok(None);
            };
            for entry in page {
                if let Err(error) =
                    self.verify_durable_event(&entry.world, entry.commit_seq, &entry.event)
                {
                    // Only a refused proof marks the entry; an engine or
                    // decode failure is no verdict and propagates.
                    return if matches!(
                        error,
                        CellError::HistoricalValueUnproven(_) | CellError::LedgerCorrupt { .. }
                    ) {
                        Ok(Some((entry, error)))
                    } else {
                        Err(error)
                    };
                }
            }
            from = last.saturating_add(1);
        }
    }

    /// Every receipt, in commit order.
    ///
    /// # Errors
    ///
    /// SQL engine or decode errors; [`CellError::LedgerCorrupt`] when a
    /// receipt's lookup columns disagree with its hashed body.
    pub fn receipts(&self) -> Result<Vec<Receipt>, CellError> {
        self.receipts_from(0)
    }

    /// Find one commit receipt by its world-scoped idempotency key.
    ///
    /// # Errors
    ///
    /// SQL engine or receipt decode errors.
    pub fn receipt_by_idempotency_key(
        &self,
        world: &str,
        key: &str,
    ) -> Result<Option<Receipt>, CellError> {
        receipt_by_key(self.conn, world, key)
    }

    /// Receipts with `commit_seq >= from`, in commit order.
    fn receipts_from(&self, from: u64) -> Result<Vec<Receipt>, CellError> {
        // The full scan deliberately has no lower bound: filtering by the
        // host's integer range could hide a corrupt out-of-range receipt.
        let rows = if from == 0 {
            self.conn.query(
                &format!(
                    "SELECT \"commit_seq\", \"world\", \"idempotency_key\", \"request_hash\", \"receipt\", \"receipt_hash\" FROM \"{RECEIPT}\" ORDER BY \"commit_seq\""
                ),
                params![],
            )?
        } else {
            self.conn.query(
                &format!(
                    "SELECT \"commit_seq\", \"world\", \"idempotency_key\", \"request_hash\", \"receipt\", \"receipt_hash\" FROM \"{RECEIPT}\" WHERE \"commit_seq\" >= ?1 ORDER BY \"commit_seq\""
                ),
                params![to_i64(from, "commit_seq")?],
            )?
        };
        decode_receipts(rows)
    }

    /// At most `limit` receipts with `commit_seq > after`, in commit order:
    /// the bounded cursor log that reactive subscriptions publish and resume
    /// from (#2694). Each receipt's lookup columns are checked against its
    /// hashed body, as in [`Self::receipts`].
    ///
    /// # Errors
    ///
    /// SQL engine or decode errors; [`CellError::LedgerCorrupt`] when a
    /// receipt's lookup columns disagree with its hashed body.
    pub fn receipts_after(&self, after: u64, limit: usize) -> Result<Vec<Receipt>, CellError> {
        let limit = i64::try_from(limit)
            .unwrap_or(MAX_SAFE_INTEGER)
            .min(MAX_SAFE_INTEGER);
        let rows = self.conn.query(
            &format!(
                "SELECT \"commit_seq\", \"world\", \"idempotency_key\", \"request_hash\", \"receipt\", \"receipt_hash\" FROM \"{RECEIPT}\" WHERE \"commit_seq\" > ?1 ORDER BY \"commit_seq\" LIMIT ?2"
            ),
            params![to_i64(after, "commit_seq")?, limit],
        )?;
        decode_receipts(rows)
    }

    /// Iterate retained receipts in bounded pages, including an unfiltered first
    /// page so malformed nonpositive sequence numbers cannot be hidden.
    pub(crate) fn receipt_stream(&self) -> impl Iterator<Item = Result<Receipt, CellError>> + '_ {
        self.receipt_stream_from(0)
    }

    fn receipt_stream_from(
        &self,
        checkpoint: u64,
    ) -> impl Iterator<Item = Result<Receipt, CellError>> + '_ {
        let mut page = std::collections::VecDeque::new();
        let mut after = (checkpoint != 0).then_some(checkpoint);
        let mut done = false;
        std::iter::from_fn(move || {
            if let Some(receipt) = page.pop_front() {
                return Some(Ok(receipt));
            }
            if done {
                return None;
            }
            let fetched = match after {
                Some(after) => self.receipts_after(after, 32),
                None => self.conn.query(
                    &format!("SELECT \"commit_seq\", \"world\", \"idempotency_key\", \"request_hash\", \"receipt\", \"receipt_hash\" FROM \"{RECEIPT}\" ORDER BY \"commit_seq\" LIMIT 32"), params![]
                ).and_then(decode_receipts),
            };
            match fetched {
                Ok(receipts) => {
                    after = receipts.last().map(|receipt| receipt.body.commit_seq);
                    done = receipts.is_empty();
                    page = receipts.into();
                    page.pop_front().map(Ok)
                }
                Err(error) => {
                    done = true;
                    Some(Err(error))
                }
            }
        })
    }

    /// Read a bounded receipt page only when all receipt bodies fit the
    /// caller's remaining candidate byte budget. Length metadata is fetched
    /// first, so an over-budget page is never materialized or decoded.
    /// Returns `None` on exhaustion without charging the budget, leaving it
    /// available for conservative slice re-evaluation.
    ///
    /// # Errors
    /// SQL engine or decode errors; [`CellError::LedgerCorrupt`] when a
    /// receipt's lookup columns disagree with its hashed body.
    pub fn receipts_after_with_budget(
        &self,
        after: u64,
        limit: usize,
        budget: &mut DecodeBudget,
    ) -> Result<Option<Vec<Receipt>>, CellError> {
        let limit = i64::try_from(limit)
            .unwrap_or(MAX_SAFE_INTEGER)
            .min(MAX_SAFE_INTEGER);
        let length_rows = self.conn.query(
            &format!(
                "SELECT COALESCE(octet_length(\"world\"), 0), COALESCE(octet_length(\"idempotency_key\"), 0), COALESCE(octet_length(\"request_hash\"), 0), COALESCE(octet_length(\"receipt\"), 0), COALESCE(octet_length(\"receipt_hash\"), 0) FROM \"{RECEIPT}\" WHERE \"commit_seq\" > ?1 ORDER BY \"commit_seq\" LIMIT ?2"
            ),
            params![to_i64(after, "commit_seq")?, limit],
        )?;
        let mut bytes = 0_u64;
        for values in &length_rows {
            let row = SqlRow(values.clone());
            bytes = bytes.saturating_add(64);
            for index in 0..5 {
                bytes =
                    bytes.saturating_add(to_u64(row.get::<i64>(index)?, "receipt field length")?);
            }
        }
        if !budget.charge(bytes) {
            return Ok(None);
        }
        self.receipts_after(after, usize::try_from(limit).unwrap_or(usize::MAX))
            .map(Some)
    }

    /// The receipt of commit `commit_seq`, when it is still in the log.
    ///
    /// # Errors
    ///
    /// As [`Self::receipts_after`].
    pub fn receipt_at(&self, commit_seq: u64) -> Result<Option<Receipt>, CellError> {
        if commit_seq == 0 {
            return Ok(None);
        }
        Ok(self
            .receipts_after(commit_seq.saturating_sub(1), 1)?
            .into_iter()
            .find(|receipt| receipt.body.commit_seq == commit_seq))
    }

    /// The stored hash column of receipt `commit_seq`, without decoding its
    /// body. Subscription cursors need only this digest to bind to the log.
    ///
    /// # Errors
    /// SQL engine errors or an out-of-range commit sequence.
    pub fn receipt_hash_at(&self, commit_seq: u64) -> Result<Option<String>, CellError> {
        if commit_seq == 0 {
            return Ok(None);
        }
        self.conn.query_optional(
            &format!("SELECT \"receipt_hash\" FROM \"{RECEIPT}\" WHERE \"commit_seq\" = ?1"),
            params![to_i64(commit_seq, "commit_seq")?],
            |row| row.get(0),
        )
    }

    /// Recompute the ledger hash chain and every receipt hash, and check that
    /// the receipts tile the ledger (contiguous spans from the first event,
    /// every event in its receipt's commit and world, no orphan events) and
    /// that each receipt's head is the chain's hash at its last event.
    ///
    /// # Errors
    ///
    /// [`CellError::LedgerCorrupt`] at the first mismatch.
    pub fn verify_ledger(&self) -> Result<LedgerHead, CellError> {
        self.verify_ledger_from(&LedgerHead::genesis())
    }

    /// [`Self::verify_ledger`] for the suffix after `checkpoint`, a head an
    /// earlier verification of this file returned (#2975): reads and checks
    /// only the events and receipts after it, so a caller that keeps the
    /// last verified head pays for new commits, not for the whole ledger.
    ///
    /// The prefix through the checkpoint is not re-verified. What is checked
    /// is that the checkpoint still anchors the file: the event at its
    /// sequence number still has its hash, and its last receipt still ends
    /// at that event with that head. Starting from [`LedgerHead::genesis`] is
    /// a full verification.
    ///
    /// # Errors
    ///
    /// [`CellError::LedgerCorrupt`] when the checkpoint no longer anchors
    /// the file or at the first mismatch after it.
    pub fn verify_ledger_from(&self, checkpoint: &LedgerHead) -> Result<LedgerHead, CellError> {
        self.verify_ledger_with_boundary(checkpoint)
            .map(|(head, _)| head)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one pass: the anchor, the hash chain, receipt tiling and quantity boundary"
    )]
    pub(crate) fn verify_ledger_with_boundary(
        &self,
        checkpoint: &LedgerHead,
    ) -> Result<(LedgerHead, Option<crate::quantity::FormatBoundary>), CellError> {
        let base = checkpoint.seq;
        let anchor_corrupt = || CellError::LedgerCorrupt {
            seq: base,
            reason: "the verified checkpoint no longer anchors the ledger".to_owned(),
        };
        if base == 0 {
            if checkpoint.hash != GENESIS_HASH {
                return Err(anchor_corrupt());
            }
        } else {
            let anchor = self.ledger_range(base, Some(1))?;
            if !anchor
                .first()
                .is_some_and(|entry| entry.seq == base && entry.hash == checkpoint.hash)
            {
                return Err(anchor_corrupt());
            }
        }
        if checkpoint.commits > 0 {
            let anchor = self.receipts_after(checkpoint.commits.saturating_sub(1), 1)?;
            if !anchor.first().is_some_and(|receipt| {
                receipt.body.commit_seq == checkpoint.commits
                    && receipt.body.ledger_head == checkpoint.hash
                    && receipt
                        .body
                        .first_event_seq
                        .checked_add(receipt.body.event_count)
                        == base.checked_add(1)
                    && hash_json(&receipt.body).is_ok_and(|hash| hash == receipt.hash)
            }) {
                return Err(anchor_corrupt());
            }
        } else if base != 0 {
            return Err(anchor_corrupt());
        }

        let mut boundary = self
            .quantity_boundary
            .filter(|boundary| boundary.seq <= base);
        if base > 0 && self.quantity_boundary.is_none() {
            boundary = crate::quantity::format_boundary(self.conn)?
                .filter(|boundary| boundary.seq <= base);
        }
        let mut expected_seq = base;
        let mut expected_commit = checkpoint.commits;
        let mut prev = checkpoint.hash.clone();
        // Consume one receipt and one event at a time. Memory is bounded by the
        // receipt page plus the largest individual retained event, not history.
        let receipts = self.receipt_stream_from(checkpoint.commits);
        for receipt in receipts {
            let receipt = receipt?;
            expected_commit = next(expected_commit, "commit_seq")?;
            let corrupt = |reason: &str| CellError::LedgerCorrupt {
                seq: receipt.body.commit_seq,
                reason: reason.to_owned(),
            };
            if receipt.body.commit_seq != expected_commit {
                return Err(corrupt("commit sequence gap"));
            }
            if receipt.body.quantity.is_some() != receipt.body.quantity_result.is_some()
                || (receipt.body.quantity_intent.is_some() && receipt.body.quantity.is_none())
                || receipt.body.quantity.as_ref().is_some_and(|requests| {
                    requests.is_empty()
                        || requests.len() > 100
                        || receipt
                            .body
                            .quantity_result
                            .as_ref()
                            .is_none_or(|results| results.len() != requests.len())
                })
            {
                return Err(corrupt("quantity receipt provenance is incomplete"));
            }
            if hash_json(&receipt.body)? != receipt.hash {
                return Err(corrupt("receipt hash does not verify"));
            }
            if receipt.body.first_event_seq != next(expected_seq, "seq")? {
                return Err(corrupt("receipt span does not continue the ledger"));
            }
            let end = receipt
                .body
                .first_event_seq
                .checked_add(receipt.body.event_count)
                .ok_or_else(|| corrupt("event span out of range"))?;
            for seq in receipt.body.first_event_seq..end {
                let entry = self
                    .ledger_range(
                        if expected_seq == 0 {
                            0
                        } else {
                            next(expected_seq, "seq")?
                        },
                        Some(1),
                    )?
                    .into_iter()
                    .next()
                    .ok_or_else(|| corrupt("receipt names events past the ledger head"))?;
                let event_corrupt = |reason: &str| CellError::LedgerCorrupt {
                    seq: entry.seq,
                    reason: reason.to_owned(),
                };
                if entry.seq != seq {
                    return Err(event_corrupt("sequence gap"));
                }
                if entry.prev_hash != prev {
                    return Err(event_corrupt("previous hash does not chain"));
                }
                if entry.commit_seq != receipt.body.commit_seq || entry.world != receipt.body.world
                {
                    return Err(corrupt("an event does not belong to its receipt"));
                }
                let hash = entry_hash(
                    entry.seq,
                    entry.commit_seq,
                    &entry.world,
                    &entry.event,
                    &prev,
                )?;
                if hash != entry.hash {
                    return Err(event_corrupt("entry hash does not verify"));
                }
                if receipt.body.quantity.is_some() {
                    if let Some(marker) = crate::quantity::boundary_marker(&entry, &receipt.body)?
                        && boundary.replace(marker).is_some()
                    {
                        return Err(norn_kernel::quantity::QuantityError::Corrupt.into());
                    }
                    let requests = receipt
                        .body
                        .quantity
                        .as_ref()
                        .ok_or(norn_kernel::quantity::QuantityError::Corrupt)?;
                    let start = end
                        .checked_sub(
                            u64::try_from(requests.len())
                                .map_err(|_range| norn_kernel::quantity::QuantityError::Corrupt)?,
                        )
                        .ok_or(norn_kernel::quantity::QuantityError::Corrupt)?;
                    if seq >= start {
                        let request =
                            requests
                                .get(usize::try_from(seq.saturating_sub(start)).map_err(
                                    |_range| norn_kernel::quantity::QuantityError::Corrupt,
                                )?)
                                .ok_or(norn_kernel::quantity::QuantityError::Corrupt)?;
                        if boundary
                            .is_none_or(|boundary| receipt.body.first_event_seq < boundary.seq)
                            || entry.event != crate::quantity::transition_event(request)?
                        {
                            return Err(norn_kernel::quantity::QuantityError::Corrupt.into());
                        }
                    } else if entry.event.get("type").and_then(Value::as_str)
                        == Some("quantity.transition")
                    {
                        return Err(norn_kernel::quantity::QuantityError::Corrupt.into());
                    }
                }
                prev = hash;
                expected_seq = seq;
            }
            if receipt.body.ledger_head != prev {
                return Err(corrupt("receipt head does not match the ledger"));
            }
            if let Some(requests) = &receipt.body.quantity {
                if receipt.body.event_count
                    < u64::try_from(requests.len())
                        .map_err(|_range| norn_kernel::quantity::QuantityError::Corrupt)?
                {
                    return Err(norn_kernel::quantity::QuantityError::Corrupt.into());
                }
                if base > 0 {
                    for request in requests.iter() {
                        crate::quantity::verify_transition_projection(
                            self.conn,
                            &receipt.body.world,
                            receipt.body.commit_seq,
                            request,
                        )?;
                    }
                }
            }
        }
        if let Some(entry) = self
            .ledger_range(
                if expected_seq == 0 {
                    0
                } else {
                    next(expected_seq, "seq")?
                },
                Some(1),
            )?
            .first()
        {
            return Err(CellError::LedgerCorrupt {
                seq: entry.seq,
                reason: "ledger events without a receipt".to_owned(),
            });
        }
        crate::quantity::validate_boundary(self.conn, boundary)?;
        if base == 0 {
            crate::quantity::verify_projections(self.conn, self.receipt_stream())?;
        }
        Ok((
            LedgerHead {
                seq: expected_seq,
                hash: prev,
                commits: expected_commit,
            },
            boundary,
        ))
    }
}

/// A history refusal names the ledger entry it stopped at (#1423), so the
/// operator the `cell.unopenable` reason reaches can find it. The fact key
/// is not repeated: the sequence and commit locate it.
fn unproven_at(entry: &LedgerEntry, error: CellError) -> CellError {
    let CellError::HistoricalValueUnproven(reason) = error else {
        return error;
    };
    CellError::HistoricalValueUnproven(format!(
        "{reason} (ledger seq {}, commit {}, op {})",
        entry.seq,
        entry.commit_seq,
        entry.event.get("op").and_then(Value::as_str).unwrap_or("-")
    ))
}
