//! Durable indexed frontier of observed link claims (#1028) and governed
//! mapping dispute resolutions (#1549), introduced in storage version 13.
//!
//! `_norn_link_claim_frontier` materializes every committed
//! `norn.ObservedLinkClaim` object in `main`, tracking whether each claim is
//! currently active (`active = 1`, not yet superseded by any committed claim's
//! `supersedes` set) or already superseded (`active = 0`). Partial covering
//! indexes over `active = 1` serve O(active frontier) link planning, held-link
//! lookup, and source-delete withdrawal without re-reading or re-deriving
//! historical inbox bundles, removing the 1,000-bundle ceiling.
//!
//! Because every claim used to authorize an edge is still CAS-sealed through
//! `prepare_observed_link_claim_snapshot` -> `seal_observed_link_claim` against
//! the immutable, ledger-chained `norn.ObservedLinkClaim` fact table, this
//! table is strictly a rebuildable index and confers zero write authority on
//! its own.
//!
//! `_norn_dispute_resolution` indexes committed
//! `ObservationEvent::DisputeResolved` ledger events with their commit sequence
//! and receipt hash, rebuilt deterministically from `_norn_ledger` and
//! `_norn_receipt`.

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::mapping::ObservedLinkClaim;
use norn_ir::{RuntimeValue as Value, VersionKind};
use norn_kernel::Observed;
use serde::{Deserialize, Serialize};

use crate::engine::{SqlEngine, SqlRow, params};
use crate::error::CellError;
use crate::mapping_bundle::MappingBundle;
use crate::mapping_conflict::MappingConflict;
use crate::mapping_identity::IdentityEvidence;
use crate::read::{StoreReader, to_i64, to_u64};
use crate::system::{FACT_VALUES, INBOX, LEDGER, RECEIPT};

/// Storage version that introduced `_norn_link_claim_frontier` and
/// `_norn_dispute_resolution` (#1028, #1549).
pub(crate) const FRONTIER_STORAGE_VERSION: u32 = 13;
/// Durable link-claim frontier table (storage version 13).
pub(crate) const LINK_CLAIM_FRONTIER: &str = "_norn_link_claim_frontier";
/// Durable dispute-resolution audit index (storage version 13).
pub(crate) const DISPUTE_RESOLUTION: &str = "_norn_dispute_resolution";

/// DDL statements for storage version 13's frontier and resolution indexes.
pub(crate) const DDL: &[&str] = &[
    r#"CREATE TABLE IF NOT EXISTS "_norn_link_claim_frontier" (
  "claim_id" TEXT NOT NULL PRIMARY KEY,
  "contribution_id" TEXT NOT NULL,
  "source_scope" TEXT NOT NULL,
  "source_type" TEXT NOT NULL,
  "source_key" TEXT NOT NULL,
  "role" TEXT NOT NULL,
  "target_type" TEXT,
  "target_key" TEXT,
  "observation_id" TEXT NOT NULL,
  "commit_seq" INTEGER NOT NULL,
  "claim_ordinal" INTEGER NOT NULL,
  "source_version" TEXT NOT NULL,
  "version_kind" TEXT NOT NULL,
  "event_keys_json" TEXT NOT NULL,
  "active" INTEGER NOT NULL DEFAULT 1,
  "claim_json" TEXT NOT NULL
)"#,
    r#"CREATE INDEX IF NOT EXISTS "_norn_link_claim_frontier/contribution" ON "_norn_link_claim_frontier" ("contribution_id", "active", "commit_seq", "claim_ordinal")"#,
    r#"CREATE INDEX IF NOT EXISTS "_norn_link_claim_frontier/source_role" ON "_norn_link_claim_frontier" ("source_key", "role", "commit_seq", "claim_ordinal") WHERE "active" = 1"#,
    r#"CREATE INDEX IF NOT EXISTS "_norn_link_claim_frontier/target" ON "_norn_link_claim_frontier" ("target_key", "source_key", "role") WHERE "active" = 1 AND "target_key" IS NOT NULL"#,
    r#"CREATE INDEX IF NOT EXISTS "_norn_link_claim_frontier/observation" ON "_norn_link_claim_frontier" ("observation_id")"#,
    r#"CREATE TABLE IF NOT EXISTS "_norn_dispute_resolution" (
  "dispute_id" TEXT NOT NULL PRIMARY KEY,
  "decision" TEXT NOT NULL CHECK ("decision" IN ('accept-first', 'accept-second-as-correction', 'reject')),
  "actor" TEXT NOT NULL,
  "reason" TEXT NOT NULL,
  "commit_seq" INTEGER NOT NULL,
  "receipt_hash" TEXT NOT NULL
)"#,
    r#"CREATE INDEX IF NOT EXISTS "_norn_ledger/dispute_resolved" ON "_norn_ledger" ("world", json_extract("event", '$.disputeId'), "commit_seq") WHERE json_extract("event", '$.op') = 'dispute_resolved'"#,
    r#"CREATE INDEX IF NOT EXISTS "_norn_receipt/noop_observation" ON "_norn_receipt" ("world", "commit_seq") WHERE json_extract("receipt", '$.consumed.kind') = 'observation' AND (json_extract("receipt", '$.eventCount') = 0 OR json_extract("receipt", '$.consumed.observationId') LIKE 'mapping-page:%')"#,
];

/// One row decoded from `_norn_link_claim_frontier`.
#[derive(Debug, Clone)]
pub(crate) struct FrontierRow {
    pub(crate) claim_id: String,
    pub(crate) contribution_id: String,
    pub(crate) source_version: String,
    pub(crate) version_kind: VersionKind,
    pub(crate) event_keys: [String; 5],
    pub(crate) active: bool,
    pub(crate) claim: ObservedLinkClaim,
}

/// Durable audit record of one governed `mapping.dispute.resolve` decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DisputeResolutionRecord {
    /// The resolved dispute's inbox observation id (`mapping-conflict:` or `mapping-identity:`).
    pub dispute_id: String,
    /// The decision applied (`accept-first`, `accept-second-as-correction`, or `reject`).
    pub decision: String,
    /// The actor who authorized the resolution.
    pub actor: String,
    /// The operator-supplied audit reason.
    pub reason: String,
    /// Commit sequence that recorded the resolution.
    pub commit_seq: u64,
    /// Hash of the commit receipt.
    pub receipt_hash: String,
}

/// Source-event metadata extracted for a committed claim's `observation_id`.
struct ClaimSourceMeta {
    source_version: String,
    version_kind: VersionKind,
    event_keys: [String; 5],
}

impl Default for ClaimSourceMeta {
    fn default() -> Self {
        Self {
            source_version: String::new(),
            version_kind: VersionKind::Lexicographical,
            event_keys: [
                String::new(),
                String::new(),
                String::new(),
                String::new(),
                String::new(),
            ],
        }
    }
}

/// Decode an [`ObservedLinkClaim`] from a stored `norn.ObservedLinkClaim`
/// object's property map and verify its content-addressed identity matches `key`.
pub(crate) fn parse_claim_from_values(
    key: &str,
    values: &BTreeMap<String, Value>,
) -> Result<ObservedLinkClaim, CellError> {
    let evidence = values
        .get("evidence")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            CellError::Encoding(format!(
                "observed link claim {key} is missing evidence text"
            ))
        })?;
    let parsed = Value::from_json_text(evidence).map_err(|error| {
        CellError::Encoding(format!(
            "observed link claim {key} has invalid evidence JSON: {error}"
        ))
    })?;
    let claim: ObservedLinkClaim = norn_ir::runtime::decode(&parsed).map_err(|error| {
        CellError::Encoding(format!(
            "observed link claim {key} failed to decode: {error}"
        ))
    })?;
    if claim.id()? != key {
        return Err(CellError::Encoding(format!(
            "observed link claim {key} identity does not match its evidence"
        )));
    }
    Ok(claim)
}

fn bundle_meta_with_reader<E: SqlEngine>(
    reader: Option<&StoreReader<'_, E>>,
    bundle: &MappingBundle,
) -> ClaimSourceMeta {
    let version_kind = if let Some(reader) = reader {
        reader
            .bundle_version_kind(bundle)
            .unwrap_or_else(|_| raw_version_kind(&bundle.raw))
    } else {
        raw_version_kind(&bundle.raw)
    };
    let source_version = bundle
        .raw
        .get("source_version")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let event_keys = crate::mapping_identity::keys(&bundle.raw, &bundle.input)
        .map(|keys| keys.map(|k| norn_ir::canonical::nfc(&k)))
        .unwrap_or_default();
    ClaimSourceMeta {
        source_version,
        version_kind,
        event_keys,
    }
}

fn raw_version_kind(raw: &Value) -> VersionKind {
    raw.get("version_kind")
        .and_then(Value::as_str)
        .and_then(VersionKind::parse_wire)
        .unwrap_or(VersionKind::Lexicographical)
}

fn claim_source_meta<E: SqlEngine>(
    conn: &E,
    reader: Option<&StoreReader<'_, E>>,
    commit_seq: u64,
    observation_id: &str,
) -> Result<ClaimSourceMeta, CellError> {
    let Some(observed_val) = crate::export_journal::authenticated_claim_observed(
        conn,
        reader,
        commit_seq,
        observation_id,
    )?
    else {
        return Ok(ClaimSourceMeta::default());
    };
    let Ok(observed) = norn_ir::runtime::decode::<Observed<Value>>(&observed_val) else {
        return Ok(ClaimSourceMeta::default());
    };
    if observation_id.starts_with("mapping-bundle:") {
        if let Ok(bundle) = norn_ir::runtime::decode::<MappingBundle>(&observed.input) {
            return Ok(bundle_meta_with_reader(reader, &bundle));
        }
    } else if observation_id.starts_with("mapping-conflict:") {
        if let Ok(conflict) = norn_ir::runtime::decode::<MappingConflict>(&observed.input) {
            return Ok(bundle_meta_with_reader(reader, &conflict.incoming));
        }
    } else if observation_id.starts_with("mapping-identity:")
        && let Ok(identity) = norn_ir::runtime::decode::<IdentityEvidence>(&observed.input)
    {
        return Ok(bundle_meta_with_reader(reader, &identity.bundle));
    }
    Ok(ClaimSourceMeta::default())
}

/// Insert one committed `norn.ObservedLinkClaim` into `_norn_link_claim_frontier`
/// and mark every claim it lists in `supersedes` as inactive (`active = 0`).
pub(crate) fn record_claim_in_tx<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    commit_seq: u64,
    claim_ordinal: u64,
    key: &str,
    values: &BTreeMap<String, Value>,
) -> Result<(), CellError> {
    let claim = parse_claim_from_values(key, values)?;
    let meta = claim_source_meta(reader.conn, Some(reader), commit_seq, &claim.observation_id)?;
    insert_claim_row(
        reader.conn,
        commit_seq,
        claim_ordinal,
        key,
        &claim,
        &meta,
        true,
    )
}

fn insert_claim_row<E: SqlEngine>(
    conn: &E,
    commit_seq: u64,
    claim_ordinal: u64,
    claim_id: &str,
    claim: &ObservedLinkClaim,
    meta: &ClaimSourceMeta,
    active: bool,
) -> Result<(), CellError> {
    let contribution_id = claim.contribution_id()?;
    // Preserve exact Unicode spelling of immutable claim provenance (no NFC
    // rewriting) so resealing retained claims matches `_norn_fact_values`.
    let claim_json = serde_json::to_string(claim)?;
    let event_keys_json = serde_json::to_string(&meta.event_keys)?;
    let commit_seq_i64 = to_i64(commit_seq, "claim commit_seq")?;
    let ordinal_i64 = to_i64(claim_ordinal, "claim ordinal")?;
    for superseded in &claim.supersedes {
        conn.execute(
            &format!(
                "UPDATE \"{LINK_CLAIM_FRONTIER}\" SET \"active\" = 0 WHERE \"claim_id\" = ?1 AND \"active\" = 1"
            ),
            params![superseded.as_str()],
        )?;
    }
    conn.execute(
        &format!(
            "INSERT OR REPLACE INTO \"{LINK_CLAIM_FRONTIER}\" \
             (\"claim_id\", \"contribution_id\", \"source_scope\", \"source_type\", \"source_key\", \"role\", \
              \"target_type\", \"target_key\", \"observation_id\", \"commit_seq\", \"claim_ordinal\", \
              \"source_version\", \"version_kind\", \"event_keys_json\", \"active\", \"claim_json\") \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)"
        ),
        params![
            claim_id,
            contribution_id,
            claim.source_scope.as_str(),
            claim.source_type.as_str(),
            claim.source_key.as_str(),
            claim.role.as_str(),
            claim.target.as_ref().map(|t| t.object_type.as_str()),
            claim.target.as_ref().map(|t| t.key.as_str()),
            claim.observation_id.as_str(),
            commit_seq_i64,
            ordinal_i64,
            meta.source_version.as_str(),
            meta.version_kind.as_str(),
            event_keys_json,
            i64::from(active),
            claim_json,
        ],
    )?;
    Ok(())
}

/// Record any `ObservationEvent::DisputeResolved` ledger events from this
/// commit into `_norn_dispute_resolution` in the same SQLite transaction.
///
/// Enforces fail-closed audit integrity: every `dispute_resolved` event must
/// match the commit's consumed `Observation { observation_id }`, and uses
/// plain `INSERT` (never `INSERT OR REPLACE`) so a recorded resolution can
/// never be overwritten by a later commit.
pub(crate) fn record_dispute_resolutions_in_tx<E: SqlEngine>(
    conn: &E,
    commit_seq: u64,
    receipt_hash: &str,
    consumed: Option<&crate::store::Consumption>,
    allow_dispute_resolution: bool,
    entries: &[crate::read::LedgerEntry],
) -> Result<(), CellError> {
    let commit_seq_i64 = to_i64(commit_seq, "dispute resolution commit_seq")?;
    for entry in entries {
        let event = &entry.event;
        if event.get("op").and_then(Value::as_str) != Some("dispute_resolved") {
            continue;
        }
        if !allow_dispute_resolution {
            return Err(CellError::UnsupportedMappingProfile);
        }
        let dispute_id = event
            .get("disputeId")
            .and_then(Value::as_str)
            .ok_or_else(|| CellError::Encoding("dispute_resolved missing disputeId".to_owned()))?;
        if !(dispute_id.starts_with("mapping-conflict:")
            || dispute_id.starts_with("mapping-identity:"))
        {
            return Err(CellError::Encoding(format!(
                "dispute_resolved requires a mapping dispute id, got {dispute_id}"
            )));
        }
        if !dispute_consumption_valid(conn, commit_seq, dispute_id, consumed)? {
            return Err(CellError::Encoding(format!(
                "dispute_resolved event for {dispute_id} requires consuming that dispute (or resolving a previously consumed legacy dispute)"
            )));
        }
        let decision = event
            .get("decision")
            .and_then(Value::as_str)
            .ok_or_else(|| CellError::Encoding("dispute_resolved missing decision".to_owned()))?;
        let actor = event
            .get("actor")
            .and_then(Value::as_str)
            .ok_or_else(|| CellError::Encoding("dispute_resolved missing actor".to_owned()))?;
        let reason = event
            .get("reason")
            .and_then(Value::as_str)
            .ok_or_else(|| CellError::Encoding("dispute_resolved missing reason".to_owned()))?;
        conn.execute(
            &format!(
                "INSERT INTO \"{DISPUTE_RESOLUTION}\" \
                 (\"dispute_id\", \"decision\", \"actor\", \"reason\", \"commit_seq\", \"receipt_hash\") \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)"
            ),
            params![dispute_id, decision, actor, reason, commit_seq_i64, receipt_hash],
        )?;
        if dispute_id.starts_with("mapping-conflict:")
            && matches!(decision, "accept-second-as-correction" | "reject")
        {
            crate::export_journal::retire_superseded_conflict_bundle(
                conn,
                commit_seq_i64,
                dispute_id,
            )?;
        }
    }
    Ok(())
}

/// A governed `dispute_resolved` commit either consumes `dispute_id` directly
/// (normal pending dispute) or consumes nothing when resolving a pre-v13
/// legacy dispute row that was already generically consumed at an earlier
/// commit sequence (`consumed_commit_seq < commit_seq`).
pub(crate) fn dispute_consumption_valid<E: SqlEngine>(
    conn: &E,
    commit_seq: u64,
    dispute_id: &str,
    consumed: Option<&crate::store::Consumption>,
) -> Result<bool, CellError> {
    match consumed {
        Some(crate::store::Consumption::Observation { observation_id }) => {
            Ok(observation_id == dispute_id)
        }
        None => {
            let seq_i64 = to_i64(commit_seq, "dispute resolution commit_seq")?;
            Ok(conn
                .query_optional(
                    &format!(
                        "SELECT 1 FROM \"{INBOX}\" \
                         WHERE \"observation_id\" = ?1 \
                           AND \"consumed_commit_seq\" IS NOT NULL \
                           AND \"consumed_commit_seq\" < ?2"
                    ),
                    params![dispute_id, seq_i64],
                    |_row| Ok(()),
                )?
                .is_some())
        }
        Some(
            crate::store::Consumption::Timer { .. } | crate::store::Consumption::Result { .. },
        ) => Ok(false),
    }
}

/// Whether `events` contains a governed `dispute_resolved` decision on
/// `observation_id` that supersedes (and atomically consumes) the conflict's
/// still-pending `acceptedBundleId` (`accept-second-as-correction` or `reject`).
pub(crate) fn dispute_supersedes_first(observation_id: &str, events: &[Value]) -> bool {
    observation_id.starts_with("mapping-conflict:")
        && events.iter().any(|event| {
            event.get("op").and_then(Value::as_str) == Some("dispute_resolved")
                && event.get("disputeId").and_then(Value::as_str) == Some(observation_id)
                && matches!(
                    event.get("decision").and_then(Value::as_str),
                    Some("accept-second-as-correction" | "reject")
                )
        })
}

/// When resolving a `mapping-conflict:` row with `accept-second-as-correction`
/// or `reject`, atomically consume any still-pending first capture
/// (`acceptedBundleId`) so it cannot later land and undo the resolution.
pub(crate) fn consume_superseded_conflict_bundle<E: SqlEngine>(
    conn: &E,
    commit_seq: i64,
    observation_id: &str,
    events: &[Value],
) -> Result<(), CellError> {
    if dispute_supersedes_first(observation_id, events) {
        crate::export_journal::retire_superseded_conflict_bundle(conn, commit_seq, observation_id)?;
    }
    Ok(())
}

/// Storage version 13 migration: create `_norn_link_claim_frontier` and
/// `_norn_dispute_resolution` and backfill them from immutable `_norn_fact_values`,
/// `_norn_ledger`, and `_norn_receipt`.
pub(crate) fn migrate_v13_link_claim_frontier<E: SqlEngine>(engine: &E) -> Result<(), CellError> {
    for stmt in DDL {
        engine.execute_batch(stmt)?;
    }
    let has_fact_values = engine
        .query_optional(
            &format!("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = '{FACT_VALUES}'"),
            params![],
            |row| row.get::<i64>(0),
        )?
        .is_some();
    if !has_fact_values {
        return Ok(());
    }
    rebuild_inner(engine, None)
}

/// Verify `_norn_link_claim_frontier` and `_norn_dispute_resolution` field-for-field
/// against immutable `_norn_fact_values`, `_norn_ledger`, and `_norn_receipt` on store
/// open, rebuilding deterministically if `force_rebuild` is set (upgrade from `< 13`)
/// or any missing, extra, or in-place modified row is detected.
pub(crate) fn verify_or_rebuild_on_open<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    force_rebuild: bool,
) -> Result<(), CellError> {
    if force_rebuild || frontier_has_drift(reader)? {
        rebuild_inner(reader.conn, Some(reader))?;
    }
    Ok(())
}

/// Full field-for-field drift check comparing `_norn_link_claim_frontier` against
/// immutable `_norn_fact_values` and `_norn_dispute_resolution` against `_norn_ledger`
/// joined with `_norn_receipt`.
pub(crate) fn frontier_has_drift<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
) -> Result<bool, CellError> {
    let (expected_claims, expected_superseded) =
        decode_claims_from_fact_values(reader.conn, Some(reader))?;
    let stored_claims = reader.conn.query(
        &format!(
            "SELECT \"claim_id\", \"contribution_id\", \"source_scope\", \"source_type\", \
                    \"source_key\", \"role\", \"target_type\", \"target_key\", \"observation_id\", \
                    \"commit_seq\", \"claim_ordinal\", \"source_version\", \"version_kind\", \
                    \"event_keys_json\", \"active\", \"claim_json\" \
             FROM \"{LINK_CLAIM_FRONTIER}\" ORDER BY \"claim_id\""
        ),
        params![],
    )?;
    if stored_claims.len() != expected_claims.len() {
        return Ok(true);
    }
    let mut expected_by_id = BTreeMap::new();
    for (id, commit_seq, ordinal, claim) in expected_claims {
        expected_by_id.insert(id, (commit_seq, ordinal, claim));
    }
    let mut meta_cache: BTreeMap<(u64, String), ClaimSourceMeta> = BTreeMap::new();
    for raw in stored_claims {
        let row = SqlRow(raw);
        let id: String = row.get(0)?;
        let Some((exp_seq, exp_ordinal, exp_claim)) = expected_by_id.get(&id) else {
            return Ok(true);
        };
        let meta_key = (*exp_seq, exp_claim.observation_id.clone());
        if !meta_cache.contains_key(&meta_key) {
            let meta = claim_source_meta(
                reader.conn,
                Some(reader),
                *exp_seq,
                &exp_claim.observation_id,
            )?;
            meta_cache.insert(meta_key.clone(), meta);
        }
        let exp_meta = meta_cache
            .get(&meta_key)
            .ok_or_else(|| CellError::Engine("missing cached claim metadata".to_owned()))?;
        let exp_contrib = exp_claim.contribution_id()?;
        let exp_active = i64::from(!expected_superseded.contains(&id));
        let exp_event_keys_json = serde_json::to_string(&exp_meta.event_keys)?;
        let exp_json = serde_json::to_string(exp_claim)?;
        let matches = row.get::<String>(1)? == exp_contrib
            && row.get::<String>(2)? == exp_claim.source_scope
            && row.get::<String>(3)? == exp_claim.source_type.as_str()
            && row.get::<String>(4)? == exp_claim.source_key
            && row.get::<String>(5)? == exp_claim.role.as_str()
            && row.get::<Option<String>>(6)?.as_deref()
                == exp_claim.target.as_ref().map(|t| t.object_type.as_str())
            && row.get::<Option<String>>(7)?.as_deref()
                == exp_claim.target.as_ref().map(|t| t.key.as_str())
            && row.get::<String>(8)? == exp_claim.observation_id
            && u64::try_from(row.get::<i64>(9)?).ok() == Some(*exp_seq)
            && u64::try_from(row.get::<i64>(10)?).ok() == Some(*exp_ordinal)
            && row.get::<String>(11)? == exp_meta.source_version
            && row.get::<String>(12)? == exp_meta.version_kind.as_str()
            && row.get::<String>(13)? == exp_event_keys_json
            && row.get::<i64>(14)? == exp_active
            && row.get::<String>(15)? == exp_json;
        if !matches {
            return Ok(true);
        }
    }
    let expected_resolutions = expected_dispute_resolutions(reader.conn)?;
    let Ok(stored_resolutions) = reader.dispute_resolutions() else {
        return Ok(true);
    };
    Ok(stored_resolutions != expected_resolutions)
}

/// Decode every committed `norn.ObservedLinkClaim` from immutable
/// `_norn_fact_values` along with the exact set of superseded claim ids.
#[allow(
    clippy::type_complexity,
    reason = "internal frontier verification tuple"
)]
pub(crate) fn decode_claims_from_fact_values<E: SqlEngine>(
    conn: &E,
    reader: Option<&StoreReader<'_, E>>,
) -> Result<(Vec<(String, u64, u64, ObservedLinkClaim)>, BTreeSet<String>), CellError> {
    let rows = conn.query(
        &format!(
            "SELECT \"id\", \"commit_seq\", \"snapshot\" FROM \"{FACT_VALUES}\" \
             WHERE \"world\" = ?1 AND \"object_type\" = ?2 AND \"version\" = 1 \
             ORDER BY \"commit_seq\", \"rowid\""
        ),
        params![
            crate::MAIN_WORLD,
            norn_ir::mapping::OBSERVED_LINK_CLAIM_TYPE
        ],
    )?;
    let mut decoded = Vec::with_capacity(rows.len());
    let mut superseded_ids = BTreeSet::new();
    let mut last_commit_seq: Option<u64> = None;
    let mut ordinal: u64 = 0;
    for raw in rows {
        let row = SqlRow(raw);
        let id: String = row.get(0)?;
        let commit_seq = to_u64(row.get(1)?, "claim commit_seq")?;
        let snapshot_text: String = row.get(2)?;
        if last_commit_seq == Some(commit_seq) {
            ordinal = ordinal.saturating_add(1);
        } else {
            last_commit_seq = Some(commit_seq);
            ordinal = 0;
        }
        let snapshot_val = Value::from_json_text(&snapshot_text).map_err(|error| {
            CellError::Encoding(format!("invalid claim snapshot JSON for {id}: {error}"))
        })?;
        let snapshot: crate::history::FactSnapshot = norn_ir::runtime::decode(&snapshot_val)
            .map_err(|error| {
                CellError::Encoding(format!("invalid claim FactSnapshot for {id}: {error}"))
            })?;
        let Some(values) = snapshot.values else {
            continue;
        };
        // Verify canonical equality against immutable `_norn_fact_values` and
        // decode live exact Unicode spelling (for resealing) when `reader` is
        // available; fail closed if the immutable live claim is missing or tampered.
        let claim_values = if let Some(reader) = reader {
            let Some(live) = reader.object(
                crate::MAIN_WORLD,
                norn_ir::mapping::OBSERVED_LINK_CLAIM_TYPE,
                &id,
            )?
            else {
                return Err(CellError::LedgerCorrupt {
                    seq: commit_seq,
                    reason: format!("missing live claim {id} referenced by _norn_fact_values"),
                });
            };
            if crate::codec::canonical_json(&live.values)? != crate::codec::canonical_json(&values)?
            {
                return Err(CellError::LedgerCorrupt {
                    seq: commit_seq,
                    reason: format!("live claim {id} disagrees with _norn_fact_values"),
                });
            }
            live.values
        } else {
            values
        };
        let claim = parse_claim_from_values(&id, &claim_values)?;
        superseded_ids.extend(claim.supersedes.iter().cloned());
        decoded.push((id, commit_seq, ordinal, claim));
    }
    Ok((decoded, superseded_ids))
}

fn expected_dispute_resolutions<E: SqlEngine>(
    conn: &E,
) -> Result<Vec<DisputeResolutionRecord>, CellError> {
    let has_ledger = conn
        .query_optional(
            &format!("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = '{LEDGER}'"),
            params![],
            |row| row.get::<i64>(0),
        )?
        .is_some();
    if !has_ledger {
        return Ok(Vec::new());
    }
    let has_export_journal = conn
        .query_optional(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = '_norn_export_journal'",
            params![],
            |row| row.get::<i64>(0),
        )?
        .is_some();
    let rows = conn.query(
        &format!(
            "SELECT l.\"commit_seq\", l.\"event\", r.\"receipt_hash\", r.\"receipt\" \
             FROM \"{LEDGER}\" l \
             JOIN \"{RECEIPT}\" r ON r.\"commit_seq\" = l.\"commit_seq\" \
             WHERE l.\"world\" = ?1 AND json_extract(l.\"event\", '$.op') = 'dispute_resolved' \
             ORDER BY l.\"commit_seq\", json_extract(l.\"event\", '$.disputeId')"
        ),
        params![crate::MAIN_WORLD],
    )?;
    let mut out = Vec::with_capacity(rows.len());
    for raw in rows {
        let row = SqlRow(raw);
        let commit_seq = to_u64(row.get(0)?, "dispute resolution commit_seq")?;
        let event_text: String = row.get(1)?;
        let receipt_hash: String = row.get(2)?;
        let receipt_text: String = row.get(3)?;
        // Enforce the exact same retained storage-version gate as replay using
        // the hash-verified packed-format journal decoder: pre-v13 records
        // never populate `_norn_dispute_resolution`.
        if has_export_journal
            && let Some(record) = crate::export_journal::read_record(conn, commit_seq)?
            && record.storage_version.unwrap_or(1) < FRONTIER_STORAGE_VERSION
        {
            continue;
        }
        let event = Value::from_json_text(&event_text).map_err(|error| {
            CellError::Encoding(format!("invalid dispute_resolved ledger event: {error}"))
        })?;
        let body: crate::store::ReceiptBody = serde_json::from_str(&receipt_text)?;
        // Pre-v13 cells could contain generic application events whose `op`
        // happened to be `"dispute_resolved"`. Only index genuine governed
        // mapping dispute resolutions whose payload and receipt consumption
        // match a `mapping-conflict:` or `mapping-identity:` dispute row.
        let Some(dispute_id) = event.get("disputeId").and_then(Value::as_str) else {
            continue;
        };
        if !(dispute_id.starts_with("mapping-conflict:")
            || dispute_id.starts_with("mapping-identity:"))
        {
            continue;
        }
        if !dispute_consumption_valid(conn, commit_seq, dispute_id, body.consumed.as_ref())? {
            continue;
        }
        let Some(decision) = event.get("decision").and_then(Value::as_str).filter(|d| {
            matches!(
                *d,
                "accept-first" | "accept-second-as-correction" | "reject"
            )
        }) else {
            continue;
        };
        let Some(actor) = event.get("actor").and_then(Value::as_str) else {
            continue;
        };
        let Some(reason) = event.get("reason").and_then(Value::as_str) else {
            continue;
        };
        out.push(DisputeResolutionRecord {
            dispute_id: dispute_id.to_owned(),
            decision: decision.to_owned(),
            actor: actor.to_owned(),
            reason: reason.to_owned(),
            commit_seq,
            receipt_hash,
        });
    }
    Ok(out)
}

fn rebuild_inner<E: SqlEngine>(
    conn: &E,
    reader: Option<&StoreReader<'_, E>>,
) -> Result<(), CellError> {
    conn.execute(&format!("DELETE FROM \"{LINK_CLAIM_FRONTIER}\""), params![])?;
    conn.execute(&format!("DELETE FROM \"{DISPUTE_RESOLUTION}\""), params![])?;

    let (decoded, superseded_ids) = decode_claims_from_fact_values(conn, reader)?;
    let mut meta_cache: BTreeMap<(u64, String), ClaimSourceMeta> = BTreeMap::new();
    for (id, commit_seq, ord, claim) in &decoded {
        let meta_key = (*commit_seq, claim.observation_id.clone());
        if !meta_cache.contains_key(&meta_key) {
            let meta = claim_source_meta(conn, reader, *commit_seq, &claim.observation_id)?;
            meta_cache.insert(meta_key.clone(), meta);
        }
        let meta = meta_cache
            .get(&meta_key)
            .ok_or_else(|| CellError::Engine("missing cached claim metadata".to_owned()))?;
        let active = !superseded_ids.contains(id);
        insert_claim_row(conn, *commit_seq, *ord, id, claim, meta, active)?;
    }
    for rec in expected_dispute_resolutions(conn)? {
        conn.execute(
            &format!(
                "INSERT INTO \"{DISPUTE_RESOLUTION}\" \
                 (\"dispute_id\", \"decision\", \"actor\", \"reason\", \"commit_seq\", \"receipt_hash\") \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)"
            ),
            params![
                rec.dispute_id,
                rec.decision,
                rec.actor,
                rec.reason,
                to_i64(rec.commit_seq, "dispute resolution commit_seq")?,
                rec.receipt_hash,
            ],
        )?;
    }
    Ok(())
}

fn decode_frontier_row(raw: Vec<crate::engine::SqlValue>) -> Result<FrontierRow, CellError> {
    let row = SqlRow(raw);
    let claim_id: String = row.get(0)?;
    let contribution_id: String = row.get(1)?;
    let source_version: String = row.get(2)?;
    let version_kind_text: String = row.get(3)?;
    let version_kind =
        VersionKind::parse_wire(&version_kind_text).unwrap_or(VersionKind::Lexicographical);
    let event_keys_json: String = row.get(4)?;
    let event_keys: [String; 5] = serde_json::from_str(&event_keys_json)?;
    let active = row.get::<i64>(5)? != 0;
    let claim_json: String = row.get(6)?;
    let claim_val = Value::from_json_text(&claim_json).map_err(|error| {
        CellError::Encoding(format!("frontier claim JSON for {claim_id}: {error}"))
    })?;
    let claim: ObservedLinkClaim = norn_ir::runtime::decode(&claim_val).map_err(|error| {
        CellError::Encoding(format!("frontier claim decode for {claim_id}: {error}"))
    })?;
    Ok(FrontierRow {
        claim_id,
        contribution_id,
        source_version,
        version_kind,
        event_keys,
        active,
        claim,
    })
}

const SELECT_FRONTIER_COLS: &str = "\"claim_id\", \"contribution_id\", \
     \"source_version\", \"version_kind\", \"event_keys_json\", \"active\", \"claim_json\"";

impl<E: SqlEngine> StoreReader<'_, E> {
    /// All committed link claims from `_norn_link_claim_frontier`.
    pub(crate) fn all_frontier_claims(&self) -> Result<Vec<FrontierRow>, CellError> {
        let rows = self.conn.query(
            &format!(
                "SELECT {SELECT_FRONTIER_COLS} FROM \"{LINK_CLAIM_FRONTIER}\" \
                 ORDER BY \"commit_seq\", \"claim_ordinal\", \"claim_id\""
            ),
            params![],
        )?;
        rows.into_iter().map(decode_frontier_row).collect()
    }

    /// All committed claims touching a specific canonical link endpoint pair
    /// (both active and superseded, for `observed_link_provenance`).
    pub(crate) fn frontier_claims_for_endpoints(
        &self,
        source_key: &str,
        role: &str,
        target_key: &str,
    ) -> Result<Vec<FrontierRow>, CellError> {
        let rows = self.conn.query(
            &format!(
                "SELECT {SELECT_FRONTIER_COLS} FROM \"{LINK_CLAIM_FRONTIER}\" \
                 WHERE \"source_key\" = ?1 AND \"role\" = ?2 AND \"target_key\" = ?3 \
                 ORDER BY \"commit_seq\", \"claim_ordinal\", \"claim_id\""
            ),
            params![source_key, role, target_key],
        )?;
        rows.into_iter().map(decode_frontier_row).collect()
    }

    /// Active (`active = 1`) frontier claims touching `sources` or `targets`,
    /// closed over every `(source_key, role)` group touched by a target match.
    pub(crate) fn active_frontier_for_scope(
        &self,
        sources: &BTreeSet<String>,
        targets: &BTreeSet<String>,
    ) -> Result<Vec<FrontierRow>, CellError> {
        let mut closed_sources = sources.clone();
        if !targets.is_empty() {
            let target_json = serde_json::to_string(targets)?;
            let matched = self.conn.query(
                &format!(
                    "SELECT DISTINCT \"source_key\" FROM \"{LINK_CLAIM_FRONTIER}\" \
                     WHERE \"active\" = 1 AND \"target_key\" IS NOT NULL \
                       AND \"target_key\" IN (SELECT value FROM json_each(?1))"
                ),
                params![target_json],
            )?;
            for raw in matched {
                let row = SqlRow(raw);
                closed_sources.insert(row.get::<String>(0)?);
            }
        }
        if closed_sources.is_empty() {
            return Ok(Vec::new());
        }
        let source_json = serde_json::to_string(&closed_sources)?;
        let rows = self.conn.query(
            &format!(
                "SELECT {SELECT_FRONTIER_COLS} FROM \"{LINK_CLAIM_FRONTIER}\" \
                 WHERE \"active\" = 1 AND \"source_key\" IN (SELECT value FROM json_each(?1)) \
                 ORDER BY \"commit_seq\", \"claim_ordinal\", \"claim_id\""
            ),
            params![source_json],
        )?;
        rows.into_iter().map(decode_frontier_row).collect()
    }

    /// Active (`active = 1`) frontier claims for one `contribution_id`.
    pub(crate) fn active_claims_for_contribution(
        &self,
        contribution_id: &str,
    ) -> Result<Vec<FrontierRow>, CellError> {
        let rows = self.conn.query(
            &format!(
                "SELECT {SELECT_FRONTIER_COLS} FROM \"{LINK_CLAIM_FRONTIER}\" \
                 WHERE \"contribution_id\" = ?1 AND \"active\" = 1 \
                 ORDER BY \"commit_seq\", \"claim_ordinal\", \"claim_id\""
            ),
            params![contribution_id],
        )?;
        rows.into_iter().map(decode_frontier_row).collect()
    }

    /// Inactive (`active = 0`) frontier claims for one `contribution_id`.
    /// Queried only on out-of-order late arrivals (`new_version <= some active
    /// claim`) so the late claim records its maximal earlier predecessor in
    /// `supersedes` without touching history on monotonic feeds.
    pub(crate) fn inactive_claims_for_contribution(
        &self,
        contribution_id: &str,
    ) -> Result<Vec<FrontierRow>, CellError> {
        let rows = self.conn.query(
            &format!(
                "SELECT {SELECT_FRONTIER_COLS} FROM \"{LINK_CLAIM_FRONTIER}\" \
                 WHERE \"contribution_id\" = ?1 AND \"active\" = 0 \
                 ORDER BY \"commit_seq\", \"claim_ordinal\", \"claim_id\""
            ),
            params![contribution_id],
        )?;
        rows.into_iter().map(decode_frontier_row).collect()
    }

    /// Active (`active = 1`) frontier claims owned by `source_scope` for
    /// `source_key` with a live target (`target_key IS NOT NULL`), used when a
    /// source `kind == "delete"` withdraws that source's link contributions.
    pub(crate) fn active_claims_for_source_delete(
        &self,
        source_scope: &str,
        source_type: &str,
        source_key: &str,
    ) -> Result<Vec<FrontierRow>, CellError> {
        let rows = self.conn.query(
            &format!(
                "SELECT {SELECT_FRONTIER_COLS} FROM \"{LINK_CLAIM_FRONTIER}\" \
                 WHERE \"active\" = 1 AND \"source_key\" = ?1 \
                   AND \"source_scope\" = ?2 AND \"source_type\" = ?3 \
                   AND \"target_key\" IS NOT NULL \
                 ORDER BY \"commit_seq\", \"claim_ordinal\", \"claim_id\""
            ),
            params![source_key, source_scope, source_type],
        )?;
        rows.into_iter().map(decode_frontier_row).collect()
    }

    /// Active (`active = 1`) frontier claims whose NFC `event_keys` match
    /// `event_keys`, used when `mapping.dispute.resolve` (`reject`) withdraws
    /// the disputed event's link contributions.
    pub(crate) fn active_claims_for_event_keys(
        &self,
        event_keys: &[String; 5],
    ) -> Result<Vec<FrontierRow>, CellError> {
        let keys_json = serde_json::to_string(event_keys)?;
        let rows = self.conn.query(
            &format!(
                "SELECT {SELECT_FRONTIER_COLS} FROM \"{LINK_CLAIM_FRONTIER}\" \
                 WHERE \"active\" = 1 AND \"event_keys_json\" = ?1 \
                 ORDER BY \"commit_seq\", \"claim_ordinal\", \"claim_id\""
            ),
            params![keys_json],
        )?;
        rows.into_iter().map(decode_frontier_row).collect()
    }

    /// Every recorded `mapping.dispute.resolve` decision, ordered by commit
    /// sequence then dispute id.
    ///
    /// # Errors
    /// Store query failures or unverified resolution index rows.
    pub fn dispute_resolutions(&self) -> Result<Vec<DisputeResolutionRecord>, CellError> {
        self.check_live()?;
        let rows = self.conn.query(
            &format!(
                "SELECT \"dispute_id\", \"decision\", \"actor\", \"reason\", \"commit_seq\", \"receipt_hash\" \
                 FROM \"{DISPUTE_RESOLUTION}\" ORDER BY \"commit_seq\", \"dispute_id\""
            ),
            params![],
        )?;
        rows.into_iter()
            .map(|raw| {
                let rec =
                    crate::mapping_conflict_scoped::decode_dispute_resolution_row(&SqlRow(raw))?;
                self.verify_dispute_resolution_record(&rec)?;
                Ok(rec)
            })
            .collect()
    }

    /// Look up the recorded resolution for one `dispute_id`, if resolved.
    ///
    /// # Errors
    /// Store query failures or an unverified resolution index row.
    pub fn dispute_resolution(
        &self,
        dispute_id: &str,
    ) -> Result<Option<DisputeResolutionRecord>, CellError> {
        self.check_live()?;
        let rec = self.conn.query_optional(
            &format!(
                "SELECT \"dispute_id\", \"decision\", \"actor\", \"reason\", \"commit_seq\", \"receipt_hash\" \
                 FROM \"{DISPUTE_RESOLUTION}\" WHERE \"dispute_id\" = ?1"
            ),
            params![dispute_id],
            crate::mapping_conflict_scoped::decode_dispute_resolution_row,
        )?;
        if let Some(record) = &rec {
            self.verify_dispute_resolution_record(record)?;
        } else {
            self.ensure_no_missing_dispute_resolution(dispute_id)?;
        }
        Ok(rec)
    }
}
