//! SOURCE-CAPTURE-1: durable source-sync leases, atomic page cursors, and
//! post-capture pending-processing recovery records.
//!
//! Each source connection holds at most one active lease in `_norn_source_lease`
//! with a strictly monotonic fencing `generation`. Page capture
//! ([`crate::CellStore::capture_source_page`]) verifies the active lease, checks
//! the expected previous cursor in `_norn_source_cursor` and recorded digests
//! in `_norn_source_pending` (admitting exact full-page idempotent duplicate
//! deliveries—including nonterminal cursor pages—without creating fresh inbox
//! rows), captures every row or quarantine evidence, records any unconsumed
//! pages in `_norn_source_pending` for crash recovery, and advances
//! `_norn_source_cursor` in one SQLite transaction.

use std::collections::BTreeMap;

use norn_ir::RuntimeValue;
use norn_kernel::Observed;
use serde::{Deserialize, Serialize};

use crate::engine::{SqlEngine, SqlRow, params};
use crate::error::CellError;
use crate::read::StoreReader;
use crate::store::CellStore;

/// Durable source-sync lease table (one row per connection; monotonic `generation`).
pub(crate) const SOURCE_LEASE: &str = "_norn_source_lease";
/// Durable source-sync cursor table (one row per `(connection_id, dataset)`).
pub(crate) const SOURCE_CURSOR: &str = "_norn_source_cursor";
/// Durable post-capture inbox recovery & nonterminal replay table.
pub(crate) const SOURCE_PENDING: &str = "_norn_source_pending";
/// Durable connection-to-mapping binding table for fail-closed rebind detection.
pub(crate) const SOURCE_BINDING: &str = "_norn_source_binding";
/// Durable per-connection evidence attribution and capture frontier table.
pub(crate) const SOURCE_EVIDENCE: &str = "_norn_source_evidence";

/// Additive DDL for source-sync leases, cursors, recovery, mapping bindings,
/// and capture-position evidence attribution (older engines ignore these).
pub(crate) const DDL: [&str; 6] = [
    r#"CREATE TABLE IF NOT EXISTS "_norn_source_lease" (
  "connection_id" TEXT NOT NULL PRIMARY KEY,
  "lease_id" TEXT NOT NULL,
  "generation" INTEGER NOT NULL,
  "acquired_at_ms" INTEGER NOT NULL,
  "expires_at_ms" INTEGER NOT NULL
)"#,
    r#"CREATE TABLE IF NOT EXISTS "_norn_source_cursor" (
  "connection_id" TEXT NOT NULL,
  "dataset" TEXT NOT NULL,
  "cursor_json" TEXT NOT NULL,
  "generation" INTEGER NOT NULL,
  "page_digest" TEXT NOT NULL,
  "pages_json" TEXT NOT NULL,
  "records_json" TEXT NOT NULL,
  "updated_at_ms" INTEGER NOT NULL,
  PRIMARY KEY ("connection_id", "dataset")
)"#,
    r#"CREATE TABLE IF NOT EXISTS "_norn_source_pending" (
  "connection_id" TEXT NOT NULL,
  "dataset" TEXT NOT NULL,
  "page_digest" TEXT NOT NULL,
  "pages_json" TEXT NOT NULL,
  "records_json" TEXT NOT NULL,
  "captured_at_ms" INTEGER NOT NULL,
  "processed" INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY ("connection_id", "dataset", "page_digest")
)"#,
    r#"CREATE TABLE IF NOT EXISTS "_norn_source_binding" (
  "connection_id" TEXT NOT NULL PRIMARY KEY,
  "mapping_id" TEXT NOT NULL,
  "generation" INTEGER NOT NULL,
  "updated_at_ms" INTEGER NOT NULL
)"#,
    r#"CREATE TABLE IF NOT EXISTS "_norn_source_evidence" (
  "observation_id" TEXT NOT NULL,
  "connection_id" TEXT NOT NULL,
  "captured_commit_seq" INTEGER NOT NULL,
  PRIMARY KEY ("observation_id", "connection_id")
)"#,
    r#"CREATE INDEX IF NOT EXISTS "_norn_fact_values/root_first_version" ON "_norn_fact_values" ("world", "root_type", "commit_seq", "id") WHERE "version" = 1"#,
];

/// An active durable lease for one connection's source sync.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceLease {
    /// Configured connection id.
    pub connection_id: String,
    /// Run or worker lease identifier.
    pub lease_id: String,
    /// Strictly monotonic fencing generation.
    pub generation: u64,
    /// When the lease was acquired (milliseconds since epoch).
    pub acquired_at_ms: u64,
    /// Exclusive expiry deadline (milliseconds since epoch).
    pub expires_at_ms: u64,
}

/// One durably committed dataset cursor for a source connection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceCursorRecord {
    /// Configured connection id.
    pub connection_id: String,
    /// Dataset name within the connection's pull profile.
    pub dataset: String,
    /// Canonical JSON encoding of the committed cursor / watermark.
    pub cursor_json: String,
    /// Lease generation that committed this cursor.
    pub generation: u64,
    /// Content digest of the complete page that committed this cursor.
    pub page_digest: String,
    /// Original `mapping-page:` inbox ids captured by this page.
    pub pages: Vec<String>,
    /// Original per-record inbox ids captured by this page.
    pub records: Vec<String>,
    /// Timestamp when this cursor was committed (milliseconds since epoch).
    pub updated_at_ms: u64,
}

/// One durably captured source page awaiting post-capture canonical processing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingSourceCapture {
    /// Configured connection id.
    pub connection_id: String,
    /// Dataset name within the connection's pull profile.
    pub dataset: String,
    /// Complete-page content digest matching `_norn_source_cursor.page_digest`.
    pub page_digest: String,
    /// `mapping-page:` inbox row ids captured for this page, in page order.
    pub pages: Vec<String>,
    /// Every row's inbox observation id captured for this page, in page order.
    pub records: Vec<String>,
    /// Capture timestamp (milliseconds since epoch).
    pub captured_at_ms: u64,
}

/// Precondition and atomic cursor advancement carried by one complete captured page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorAdvanceSpec {
    /// Configured connection id.
    pub connection_id: String,
    /// Dataset name within the connection's pull profile.
    pub dataset: String,
    /// Active lease id holding this connection.
    pub lease_id: String,
    /// Active lease fencing generation.
    pub generation: u64,
    /// Logical/wall clock at capture time (ms).
    pub now_ms: u64,
    /// Expected previously committed cursor JSON (`None` if never committed yet).
    pub expected_previous_cursor: Option<String>,
    /// Next cursor JSON to persist atomically with this page.
    pub next_cursor: Option<String>,
    /// Optional lease TTL extension in ms applied inside the same transaction.
    pub renew_ttl_ms: Option<u64>,
}

/// Cross-host write-transaction fence for source sync report capture and
/// canonical promotion: active durable lease plus active connector mapping
/// revision pin, both rechecked inside SQLite's write transaction.
#[derive(Debug, Clone, Copy, Default)]
pub struct SourceReportFence<'a> {
    /// Active lease `(connection_id, lease_id, generation, now_ms)`.
    pub lease: Option<(&'a str, &'a str, u64, u64)>,
    /// Active connector mapping revision `(mapping_id, expected_binding)`.
    pub expected_binding: Option<(&'a str, &'a str)>,
}

/// Payload persisted into `_norn_source_cursor` / `_norn_source_pending` in one tx.
pub(crate) struct CaptureCommitPayload<'a> {
    pub digest: &'a str,
    pub pages: &'a [String],
    pub records: &'a [String],
    pub record_pending: bool,
}

fn to_i64(value: u64, label: &'static str) -> Result<i64, CellError> {
    i64::try_from(value).map_err(|_overflow| CellError::OutOfRange(label))
}

fn to_u64(value: i64, label: &str) -> Result<u64, CellError> {
    u64::try_from(value)
        .map_err(|_negative| CellError::Engine(format!("negative {label}: {value}")))
}

fn decode_lease(row: &SqlRow) -> Result<SourceLease, CellError> {
    Ok(SourceLease {
        connection_id: row.get(0)?,
        lease_id: row.get(1)?,
        generation: to_u64(row.get(2)?, "source lease generation")?,
        acquired_at_ms: to_u64(row.get(3)?, "source lease acquired_at_ms")?,
        expires_at_ms: to_u64(row.get(4)?, "source lease expires_at_ms")?,
    })
}

fn decode_cursor(row: &SqlRow) -> Result<SourceCursorRecord, CellError> {
    let pages_json: String = row.get(5)?;
    let records_json: String = row.get(6)?;
    Ok(SourceCursorRecord {
        connection_id: row.get(0)?,
        dataset: row.get(1)?,
        cursor_json: row.get(2)?,
        generation: to_u64(row.get(3)?, "source cursor generation")?,
        page_digest: row.get(4)?,
        pages: serde_json::from_str(&pages_json)?,
        records: serde_json::from_str(&records_json)?,
        updated_at_ms: to_u64(row.get(7)?, "source cursor updated_at_ms")?,
    })
}

fn decode_pending(row: &SqlRow) -> Result<PendingSourceCapture, CellError> {
    let pages_json: String = row.get(3)?;
    let records_json: String = row.get(4)?;
    Ok(PendingSourceCapture {
        connection_id: row.get(0)?,
        dataset: row.get(1)?,
        page_digest: row.get(2)?,
        pages: serde_json::from_str(&pages_json)?,
        records: serde_json::from_str(&records_json)?,
        captured_at_ms: to_u64(row.get(5)?, "source pending captured_at_ms")?,
    })
}

/// Content-bound complete-page digest over cursor transition, mapping selection,
/// and every row's immutable observation content (excluding wall-clock timestamps).
///
/// # Errors
/// Canonical encoding error.
pub fn page_digest(
    advance: &CursorAdvanceSpec,
    input: Option<&crate::mapping_bundle::MappingInput>,
    observations: &[Observed<RuntimeValue>],
) -> Result<String, CellError> {
    crate::mapping_process::source_page_digest_v3(advance, input, observations)
}

/// Legacy B3 `norn.source.page.v2` digest for pre-upgrade idempotent replay.
///
/// # Errors
/// Canonical encoding error.
pub fn page_digest_v2(
    advance: &CursorAdvanceSpec,
    observations: &[Observed<RuntimeValue>],
) -> Result<String, CellError> {
    crate::mapping_process::source_page_digest_v2(advance, observations)
}

fn read_lease<E: SqlEngine>(
    conn: &E,
    connection_id: &str,
) -> Result<Option<SourceLease>, CellError> {
    conn.query_optional(
        &format!(
            "SELECT \"connection_id\", \"lease_id\", \"generation\", \"acquired_at_ms\", \"expires_at_ms\" \
             FROM \"{SOURCE_LEASE}\" WHERE \"connection_id\" = ?1"
        ),
        params![connection_id],
        decode_lease,
    )
}

fn read_cursor<E: SqlEngine>(
    conn: &E,
    connection_id: &str,
    dataset: &str,
) -> Result<Option<SourceCursorRecord>, CellError> {
    conn.query_optional(
        &format!(
            "SELECT \"connection_id\", \"dataset\", \"cursor_json\", \"generation\", \"page_digest\", \
             \"pages_json\", \"records_json\", \"updated_at_ms\" \
             FROM \"{SOURCE_CURSOR}\" WHERE \"connection_id\" = ?1 AND \"dataset\" = ?2"
        ),
        params![connection_id, dataset],
        decode_cursor,
    )
}

fn read_pending_by_digest<E: SqlEngine>(
    conn: &E,
    connection_id: &str,
    dataset: &str,
    digest: &str,
) -> Result<Option<PendingSourceCapture>, CellError> {
    conn.query_optional(
        &format!(
            "SELECT \"connection_id\", \"dataset\", \"page_digest\", \"pages_json\", \"records_json\", \"captured_at_ms\" \
             FROM \"{SOURCE_PENDING}\" WHERE \"connection_id\" = ?1 AND \"dataset\" = ?2 AND \"page_digest\" = ?3"
        ),
        params![connection_id, dataset, digest],
        decode_pending,
    )
}

pub(crate) fn verify_active_lease<E: SqlEngine>(
    conn: &E,
    connection_id: &str,
    lease_id: &str,
    generation: u64,
    now_ms: u64,
) -> Result<(), CellError> {
    let fenced = || CellError::SourceLeaseFenced {
        connection_id: connection_id.to_owned(),
        lease_id: lease_id.to_owned(),
        generation,
    };
    let Some(lease) = read_lease(conn, connection_id)? else {
        return Err(fenced());
    };
    if lease.lease_id != lease_id || lease.generation != generation || now_ms >= lease.expires_at_ms
    {
        return Err(fenced());
    }
    Ok(())
}

/// Outcome of validating lease + cursor precondition inside the capture tx.
pub(crate) enum Precondition {
    /// Normal forward capture (persist `next_cursor` if `Some`).
    Advance,
    /// Exact duplicate delivery of an already-committed page (including
    /// nonterminal cursor pages recorded in `_norn_source_pending`); return
    /// the original captured observation ids without writing new inbox rows.
    IdempotentReplay {
        matched_digest: String,
        pages: Vec<String>,
        records: Vec<String>,
    },
}

/// Legacy `norn.source.page.v2` replay context: `(v2_digest, input, active_binding, rows)`.
pub(crate) type LegacyReplay<'a> = (
    &'a str,
    Option<&'a crate::mapping_bundle::MappingInput>,
    Option<&'a str>,
    &'a [Observed<RuntimeValue>],
);

/// Validate lease and cursor precondition inside an open capture transaction.
pub(crate) fn check_capture_precondition<E: SqlEngine>(
    conn: &E,
    advance: &CursorAdvanceSpec,
    digest: &str,
    legacy_v2: Option<LegacyReplay<'_>>,
) -> Result<Precondition, CellError> {
    verify_active_lease(
        conn,
        &advance.connection_id,
        &advance.lease_id,
        advance.generation,
        advance.now_ms,
    )?;
    if let Some(existing_pending) =
        read_pending_by_digest(conn, &advance.connection_id, &advance.dataset, digest)?
    {
        return Ok(Precondition::IdempotentReplay {
            matched_digest: digest.to_owned(),
            pages: existing_pending.pages,
            records: existing_pending.records,
        });
    }
    if let Some((v2, input, active_binding, delivered)) = legacy_v2
        && let Some(existing_pending) =
            read_pending_by_digest(conn, &advance.connection_id, &advance.dataset, v2)?
    {
        let check = crate::mapping_process::LegacyReplayCheck {
            mapping: None,
            input,
            active_binding,
            delivered: Some(delivered),
        };
        if crate::mapping_process::legacy_source_replay_allowed(
            conn,
            &existing_pending.records,
            check,
        )? {
            return Ok(Precondition::IdempotentReplay {
                matched_digest: v2.to_owned(),
                pages: existing_pending.pages,
                records: existing_pending.records,
            });
        }
        return Err(CellError::SourceCursorConflict {
            connection_id: advance.connection_id.clone(),
            dataset: advance.dataset.clone(),
            expected: advance.expected_previous_cursor.clone(),
            actual: Some(v2.to_owned()),
        });
    }
    let current = read_cursor(conn, &advance.connection_id, &advance.dataset)?;
    let current_json = current.as_ref().map(|record| record.cursor_json.as_str());
    if current_json == advance.expected_previous_cursor.as_deref() {
        return Ok(Precondition::Advance);
    }
    if let Some(existing) = &current
        && advance.next_cursor.as_deref() == Some(existing.cursor_json.as_str())
    {
        if existing.page_digest == digest {
            return Ok(Precondition::IdempotentReplay {
                matched_digest: digest.to_owned(),
                pages: existing.pages.clone(),
                records: existing.records.clone(),
            });
        }
        if let Some((v2, input, active_binding, delivered)) = legacy_v2
            && existing.page_digest == v2
            && crate::mapping_process::legacy_source_replay_allowed(
                conn,
                &existing.records,
                crate::mapping_process::LegacyReplayCheck {
                    mapping: None,
                    input,
                    active_binding,
                    delivered: Some(delivered),
                },
            )?
        {
            return Ok(Precondition::IdempotentReplay {
                matched_digest: v2.to_owned(),
                pages: existing.pages.clone(),
                records: existing.records.clone(),
            });
        }
    }
    Err(CellError::SourceCursorConflict {
        connection_id: advance.connection_id.clone(),
        dataset: advance.dataset.clone(),
        expected: advance.expected_previous_cursor.clone(),
        actual: current.map(|record| record.cursor_json),
    })
}

/// Insert one pending capture entry inside an already-open `write_tx`.
pub(crate) fn insert_pending_in_tx<E: SqlEngine>(
    conn: &E,
    connection_id: &str,
    dataset: &str,
    digest: &str,
    pages: &[String],
    records: &[String],
    captured_at_ms: u64,
) -> Result<(), CellError> {
    if pages.is_empty() && records.is_empty() {
        return Ok(());
    }
    let pages_json = serde_json::to_string(pages)?;
    let records_json = serde_json::to_string(records)?;
    conn.execute(
        &format!(
            "INSERT OR IGNORE INTO \"{SOURCE_PENDING}\" \
             (\"connection_id\", \"dataset\", \"page_digest\", \"pages_json\", \"records_json\", \"captured_at_ms\", \"processed\") \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0)"
        ),
        params![
            connection_id,
            dataset,
            digest,
            pages_json.as_str(),
            records_json.as_str(),
            to_i64(captured_at_ms, "source pending captured_at_ms")?,
        ],
    )?;
    Ok(())
}

/// Persist the advanced cursor, optional post-capture pending record, and
/// optional lease renewal inside the same SQLite transaction that captured the
/// page's inbox rows.
pub(crate) fn commit_cursor_in_tx<E: SqlEngine>(
    conn: &E,
    advance: &CursorAdvanceSpec,
    precondition: &Precondition,
    payload: &CaptureCommitPayload<'_>,
) -> Result<(), CellError> {
    if matches!(precondition, Precondition::Advance) {
        if payload.record_pending {
            insert_pending_in_tx(
                conn,
                &advance.connection_id,
                &advance.dataset,
                payload.digest,
                payload.pages,
                payload.records,
                advance.now_ms,
            )?;
        }
        if let Some(next_cursor) = &advance.next_cursor {
            let pages_json = serde_json::to_string(payload.pages)?;
            let records_json = serde_json::to_string(payload.records)?;
            conn.execute(
                &format!(
                    "INSERT INTO \"{SOURCE_CURSOR}\" \
                     (\"connection_id\", \"dataset\", \"cursor_json\", \"generation\", \"page_digest\", \
                      \"pages_json\", \"records_json\", \"updated_at_ms\") \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
                     ON CONFLICT(\"connection_id\", \"dataset\") DO UPDATE SET \
                     \"cursor_json\" = excluded.\"cursor_json\", \
                     \"generation\" = excluded.\"generation\", \
                     \"page_digest\" = excluded.\"page_digest\", \
                     \"pages_json\" = excluded.\"pages_json\", \
                     \"records_json\" = excluded.\"records_json\", \
                     \"updated_at_ms\" = excluded.\"updated_at_ms\""
                ),
                params![
                    advance.connection_id.as_str(),
                    advance.dataset.as_str(),
                    next_cursor.as_str(),
                    to_i64(advance.generation, "source cursor generation")?,
                    payload.digest,
                    pages_json.as_str(),
                    records_json.as_str(),
                    to_i64(advance.now_ms, "source cursor updated_at_ms")?,
                ],
            )?;
            // Reap processed nonterminal cursor-page replay records once the
            // dataset's durable cursor advances past them.
            conn.execute(
                &format!(
                    "DELETE FROM \"{SOURCE_PENDING}\" \
                     WHERE \"connection_id\" = ?1 AND \"dataset\" = ?2 AND \"processed\" = 1"
                ),
                params![advance.connection_id.as_str(), advance.dataset.as_str()],
            )?;
        }
    }
    if let Some(ttl_ms) = advance.renew_ttl_ms {
        let expires_at = advance.now_ms.saturating_add(ttl_ms);
        conn.execute(
            &format!(
                "UPDATE \"{SOURCE_LEASE}\" SET \"expires_at_ms\" = ?1 \
                 WHERE \"connection_id\" = ?2 AND \"lease_id\" = ?3 AND \"generation\" = ?4"
            ),
            params![
                to_i64(expires_at, "source lease expires_at_ms")?,
                advance.connection_id.as_str(),
                advance.lease_id.as_str(),
                to_i64(advance.generation, "source lease generation")?,
            ],
        )?;
    }
    Ok(())
}

impl<E: SqlEngine> CellStore<E> {
    /// Acquire or take over an expired durable sync lease for `connection_id`
    /// with a strictly monotonic fencing `generation`.
    ///
    /// # Errors
    /// [`CellError::SourceLeaseFenced`] if another active lease holds
    /// `connection_id` at `now_ms`, [`CellError::OutOfRange`] on zero TTL or
    /// timestamp overflow, or store errors.
    pub fn acquire_source_lease(
        &mut self,
        connection_id: &str,
        lease_id: &str,
        now_ms: u64,
        ttl_ms: u64,
    ) -> Result<SourceLease, CellError> {
        if connection_id.is_empty() || lease_id.is_empty() || ttl_ms == 0 {
            return Err(CellError::OutOfRange("source lease"));
        }
        let expires_at_ms = now_ms.saturating_add(ttl_ms);
        self.write_tx(|| {
            let existing = read_lease(&self.conn, connection_id)?;
            if let Some(current) = &existing
                && now_ms < current.expires_at_ms
            {
                return Err(CellError::SourceLeaseFenced {
                    connection_id: connection_id.to_owned(),
                    lease_id: lease_id.to_owned(),
                    generation: current.generation,
                });
            }
            let generation = existing
                .as_ref()
                .map_or(1, |current| current.generation.saturating_add(1));
            self.conn.execute(
                &format!(
                    "INSERT INTO \"{SOURCE_LEASE}\" \
                     (\"connection_id\", \"lease_id\", \"generation\", \"acquired_at_ms\", \"expires_at_ms\") \
                     VALUES (?1, ?2, ?3, ?4, ?5) \
                     ON CONFLICT(\"connection_id\") DO UPDATE SET \
                     \"lease_id\" = excluded.\"lease_id\", \
                     \"generation\" = excluded.\"generation\", \
                     \"acquired_at_ms\" = excluded.\"acquired_at_ms\", \
                     \"expires_at_ms\" = excluded.\"expires_at_ms\""
                ),
                params![
                    connection_id,
                    lease_id,
                    to_i64(generation, "source lease generation")?,
                    to_i64(now_ms, "source lease acquired_at_ms")?,
                    to_i64(expires_at_ms, "source lease expires_at_ms")?,
                ],
            )?;
            Ok(SourceLease {
                connection_id: connection_id.to_owned(),
                lease_id: lease_id.to_owned(),
                generation,
                acquired_at_ms: now_ms,
                expires_at_ms,
            })
        })
    }

    /// Expire the durable lease for `connection_id` only if `(lease_id, generation)`
    /// still owns it. Preserves the row so `generation` stays strictly monotonic.
    ///
    /// # Errors
    /// Store errors only; a stale or absent lease returns `Ok(false)`.
    pub fn release_source_lease(
        &mut self,
        connection_id: &str,
        lease_id: &str,
        generation: u64,
    ) -> Result<bool, CellError> {
        self.write_tx(|| {
            let Some(current) = read_lease(&self.conn, connection_id)? else {
                return Ok(false);
            };
            if current.lease_id != lease_id || current.generation != generation {
                return Ok(false);
            }
            self.conn.execute(
                &format!(
                    "UPDATE \"{SOURCE_LEASE}\" SET \"expires_at_ms\" = 0 \
                     WHERE \"connection_id\" = ?1 AND \"lease_id\" = ?2 AND \"generation\" = ?3"
                ),
                params![
                    connection_id,
                    lease_id,
                    to_i64(generation, "source lease generation")?,
                ],
            )?;
            Ok(true)
        })
    }

    /// Seed legacy dataset cursors (upgraded from `source.Connection.watermark`)
    /// into `_norn_source_cursor` under the active fencing lease using
    /// `INSERT OR IGNORE` so existing durable cursors are never overwritten.
    ///
    /// # Errors
    /// [`CellError::SourceLeaseFenced`] if the lease is no longer active, or
    /// store errors.
    pub fn seed_source_cursors(
        &mut self,
        connection_id: &str,
        lease: &SourceLease,
        now_ms: u64,
        cursors: &BTreeMap<String, String>,
        expected_binding: Option<(&str, &str)>,
    ) -> Result<(), CellError> {
        if cursors.is_empty() && expected_binding.is_none() {
            return Ok(());
        }
        self.write_tx(|| {
            verify_active_lease(
                &self.conn,
                connection_id,
                &lease.lease_id,
                lease.generation,
                now_ms,
            )?;
            crate::mapping_conflict::verify_mapping_binding_in_tx(
                &self.read(),
                expected_binding,
                Some((connection_id, &lease.lease_id, lease.generation)),
            )?;
            for (dataset, cursor_json) in cursors {
                self.conn.execute(
                    &format!(
                        "INSERT OR IGNORE INTO \"{SOURCE_CURSOR}\" \
                         (\"connection_id\", \"dataset\", \"cursor_json\", \"generation\", \"page_digest\", \
                          \"pages_json\", \"records_json\", \"updated_at_ms\") \
                         VALUES (?1, ?2, ?3, ?4, 'legacy-seed', '[]', '[]', ?5)"
                    ),
                    params![
                        connection_id,
                        dataset.as_str(),
                        cursor_json.as_str(),
                        to_i64(lease.generation, "source cursor generation")?,
                        to_i64(now_ms, "source cursor updated_at_ms")?,
                    ],
                )?;
            }
            Ok(())
        })
    }

    /// Persist `_norn_source_cursor` for a complete page whose rows were
    /// already captured and processed one-by-one under `OBS-LINK-DISPUTE-1`
    /// fallback, binding the full-page `digest`, `pages`, and `records` for
    /// exact future idempotent replay.
    ///
    /// # Errors
    /// [`CellError::SourceLeaseFenced`], [`CellError::SourceCursorConflict`],
    /// or store errors.
    pub fn commit_source_page_cursor(
        &mut self,
        advance: &CursorAdvanceSpec,
        digest: &str,
        pages: &[String],
        records: &[String],
        expected_binding: Option<(&str, &str)>,
    ) -> Result<(), CellError> {
        self.write_tx(|| {
            crate::mapping_conflict::verify_mapping_binding_in_tx(
                &self.read(),
                expected_binding,
                Some((
                    advance.connection_id.as_str(),
                    advance.lease_id.as_str(),
                    advance.generation,
                )),
            )?;
            let precondition = check_capture_precondition(&self.conn, advance, digest, None)?;
            let payload = CaptureCommitPayload {
                digest,
                pages,
                records,
                record_pending: false,
            };
            commit_cursor_in_tx(&self.conn, advance, &precondition, &payload)?;
            // For a nonterminal cursor page (`next_cursor = None`), retain a
            // processed (`processed = 1`) full-page replay record so exact
            // redelivery hits `IdempotentReplay` without regrouping or creating
            // a fresh `mapping-page:` row; the terminal cursor commit reaps it.
            if matches!(precondition, Precondition::Advance)
                && advance.next_cursor.is_none()
                && (!pages.is_empty() || !records.is_empty())
            {
                let pages_json = serde_json::to_string(pages)?;
                let records_json = serde_json::to_string(records)?;
                self.conn.execute(
                    &format!(
                        "INSERT OR IGNORE INTO \"{SOURCE_PENDING}\" \
                         (\"connection_id\", \"dataset\", \"page_digest\", \"pages_json\", \"records_json\", \"captured_at_ms\", \"processed\") \
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1)"
                    ),
                    params![
                        advance.connection_id.as_str(),
                        advance.dataset.as_str(),
                        digest,
                        pages_json.as_str(),
                        records_json.as_str(),
                        to_i64(advance.now_ms, "source pending captured_at_ms")?,
                    ],
                )?;
            }
            Ok(())
        })
    }

    /// Mark one post-capture pending source page complete once all of its inbox
    /// rows have been processed (landed, already landed, or quarantined).
    ///
    /// If `_norn_source_cursor` already committed this `page_digest`, the row
    /// is deleted; otherwise (e.g., a nonterminal cursor page with `next_cursor
    /// = None`), it is marked `processed = 1` so exact replays return the
    /// original observation ids without creating fresh inbox rows until the
    /// terminal page advances the cursor.
    ///
    /// # Errors
    /// Store errors.
    pub fn complete_source_pending(
        &mut self,
        connection_id: &str,
        dataset: &str,
        page_digest: &str,
    ) -> Result<(), CellError> {
        self.complete_source_pending_checked(connection_id, dataset, page_digest, None)
    }

    /// Like [`Self::complete_source_pending`], additionally verifying an active
    /// fencing lease inside the same SQLite transaction.
    ///
    /// # Errors
    /// [`CellError::SourceLeaseFenced`] or store errors.
    pub fn complete_source_pending_checked(
        &mut self,
        connection_id: &str,
        dataset: &str,
        page_digest: &str,
        lease: Option<(&str, &str, u64, u64)>,
    ) -> Result<(), CellError> {
        self.write_tx(|| {
            if let Some((conn, lid, generation, now_ms)) = lease {
                verify_active_lease(&self.conn, conn, lid, generation, now_ms)?;
            }
            let cursor_matches = read_cursor(&self.conn, connection_id, dataset)?
                .is_some_and(|record| record.page_digest == page_digest);
            if cursor_matches || page_digest.contains(":row:") {
                self.conn.execute(
                    &format!(
                        "DELETE FROM \"{SOURCE_PENDING}\" \
                         WHERE \"connection_id\" = ?1 AND \"dataset\" = ?2 AND \"page_digest\" = ?3"
                    ),
                    params![connection_id, dataset, page_digest],
                )?;
            } else {
                self.conn.execute(
                    &format!(
                        "UPDATE \"{SOURCE_PENDING}\" SET \"processed\" = 1 \
                         WHERE \"connection_id\" = ?1 AND \"dataset\" = ?2 AND \"page_digest\" = ?3"
                    ),
                    params![connection_id, dataset, page_digest],
                )?;
            }
            Ok(())
        })
    }
}

impl<E: SqlEngine> StoreReader<'_, E> {
    /// Current durable lease row for `connection_id`, if any.
    ///
    /// # Errors
    /// Store errors.
    pub fn source_lease(&self, connection_id: &str) -> Result<Option<SourceLease>, CellError> {
        read_lease(self.conn, connection_id)
    }

    /// All durably committed dataset cursors for `connection_id`, keyed by dataset.
    ///
    /// # Errors
    /// Store errors.
    pub fn source_cursors(
        &self,
        connection_id: &str,
    ) -> Result<BTreeMap<String, SourceCursorRecord>, CellError> {
        let rows = self.conn.query(
            &format!(
                "SELECT \"connection_id\", \"dataset\", \"cursor_json\", \"generation\", \"page_digest\", \
                 \"pages_json\", \"records_json\", \"updated_at_ms\" \
                 FROM \"{SOURCE_CURSOR}\" WHERE \"connection_id\" = ?1 ORDER BY \"dataset\""
            ),
            params![connection_id],
        )?;
        let mut out = BTreeMap::new();
        for values in rows {
            let record = decode_cursor(&SqlRow(values))?;
            out.insert(record.dataset.clone(), record);
        }
        Ok(out)
    }

    /// All durably captured source pages for `connection_id` whose post-capture
    /// canonical processing has not yet completed (`processed = 0`), ordered by
    /// capture time.
    ///
    /// # Errors
    /// Store errors.
    pub fn pending_source_captures(
        &self,
        connection_id: &str,
    ) -> Result<Vec<PendingSourceCapture>, CellError> {
        let rows = self.conn.query(
            &format!(
                "SELECT \"connection_id\", \"dataset\", \"page_digest\", \"pages_json\", \"records_json\", \"captured_at_ms\" \
                 FROM \"{SOURCE_PENDING}\" WHERE \"connection_id\" = ?1 AND \"processed\" = 0 \
                 ORDER BY \"captured_at_ms\", \"dataset\", \"page_digest\""
            ),
            params![connection_id],
        )?;
        let mut out = Vec::with_capacity(rows.len());
        for values in rows {
            out.push(decode_pending(&SqlRow(values))?);
        }
        Ok(out)
    }

    /// Distinct datasets for `connection_id` that still have unprocessed
    /// (`processed = 0`) captured source pages, without decoding page payloads.
    ///
    /// # Errors
    /// Store errors.
    pub fn pending_source_datasets(
        &self,
        connection_id: &str,
    ) -> Result<std::collections::BTreeSet<String>, CellError> {
        let rows = self.conn.query(
            &format!(
                "SELECT DISTINCT \"dataset\" FROM \"{SOURCE_PENDING}\" \
                 WHERE \"connection_id\" = ?1 AND \"processed\" = 0"
            ),
            params![connection_id],
        )?;
        rows.into_iter()
            .map(|values| SqlRow(values).get::<String>(0))
            .collect()
    }

    /// Whether `_norn_inbox` holds `observation_id` (pending or already consumed).
    ///
    /// # Errors
    /// Store errors.
    pub fn inbox_observation_exists(&self, observation_id: &str) -> Result<bool, CellError> {
        let found = self.conn.query_optional(
            &format!(
                "SELECT 1 FROM \"{}\" WHERE \"observation_id\" = ?1",
                crate::system::INBOX
            ),
            params![observation_id],
            |_| Ok(()),
        )?;
        Ok(found.is_some())
    }
}
