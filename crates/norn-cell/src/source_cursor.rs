//! SOURCE-CAPTURE-1: durable source-sync leases, atomic page cursors, and
//! post-capture pending-processing recovery records.
//!
//! Each source connection holds at most one active lease in `_norn_source_lease`
//! with a strictly monotonic fencing `generation`. Page capture
//! ([`crate::CellStore::capture_source_page`]) verifies the active lease, checks
//! the expected previous cursor in `_norn_source_cursor` (admitting exact
//! full-page idempotent duplicate deliveries bound to immutable row content),
//! captures every row or quarantine evidence, records any unconsumed pages in
//! `_norn_source_pending` for crash recovery, and advances `_norn_source_cursor`
//! in one SQLite transaction.

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
/// Durable post-capture inbox recovery table (deleted once processed).
pub(crate) const SOURCE_PENDING: &str = "_norn_source_pending";

/// Additive DDL for source-sync leases, cursors, and post-capture recovery.
/// Older engines ignore these tables, so no storage version bump is required.
pub(crate) const DDL: [&str; 3] = [
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
  PRIMARY KEY ("connection_id", "dataset", "page_digest")
)"#,
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
    /// Exclusive expiry deadline (milliseconds since epoch); active iff `now_ms < expires_at_ms`.
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
    /// Timestamp when this cursor was committed (milliseconds since epoch).
    pub updated_at_ms: u64,
}

/// One durably captured source page whose inbox rows have not yet completed
/// post-capture canonical processing.
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
    /// Logical/wall clock at capture time (ms); must satisfy `now_ms < lease.expires_at_ms`.
    pub now_ms: u64,
    /// Expected previously committed cursor JSON (`None` if never committed yet).
    pub expected_previous_cursor: Option<String>,
    /// Next cursor JSON to persist atomically with this page (`None` leaves cursor unchanged).
    pub next_cursor: Option<String>,
    /// Optional lease TTL extension in ms applied inside the same transaction.
    pub renew_ttl_ms: Option<u64>,
}

fn to_i64(value: u64, label: &'static str) -> Result<i64, CellError> {
    i64::try_from(value).map_err(|_| CellError::OutOfRange(label))
}

fn to_u64(value: i64, label: &str) -> Result<u64, CellError> {
    u64::try_from(value).map_err(|_| CellError::Engine(format!("negative {label}: {value}")))
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
    Ok(SourceCursorRecord {
        connection_id: row.get(0)?,
        dataset: row.get(1)?,
        cursor_json: row.get(2)?,
        generation: to_u64(row.get(3)?, "source cursor generation")?,
        page_digest: row.get(4)?,
        updated_at_ms: to_u64(row.get(5)?, "source cursor updated_at_ms")?,
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

/// Content-bound complete-page digest over cursor transition and every row's
/// immutable observation content (excluding wall-clock `extracted_at` /
/// `observed_at` so exact duplicate deliveries hash identically while any
/// payload or metadata mutation conflicts).
pub(crate) fn page_digest(
    advance: &CursorAdvanceSpec,
    raws: &[Observed<RuntimeValue>],
) -> Result<String, CellError> {
    #[derive(Serialize)]
    struct RowDigest<'a> {
        observation_id: &'a str,
        source: &'a str,
        source_id: &'a str,
        mapping_version: &'a str,
        input_source: Option<&'a RuntimeValue>,
        input_source_id: Option<&'a RuntimeValue>,
        event_id: Option<&'a RuntimeValue>,
        source_version: Option<&'a RuntimeValue>,
        pipeline_version: Option<&'a RuntimeValue>,
        kind: Option<&'a RuntimeValue>,
        payload_digest: Option<&'a RuntimeValue>,
        payload: Option<&'a RuntimeValue>,
    }
    #[derive(Serialize)]
    struct DigestInput<'a> {
        connection_id: &'a str,
        dataset: &'a str,
        rows: Vec<RowDigest<'a>>,
        expected_previous_cursor: Option<&'a str>,
        next_cursor: Option<&'a str>,
    }
    let rows = raws
        .iter()
        .map(|raw| RowDigest {
            observation_id: raw.id.0.as_str(),
            source: raw.provenance.source.as_str(),
            source_id: raw.provenance.source_id.as_str(),
            mapping_version: raw.provenance.mapping_version.as_str(),
            input_source: raw.input.get("source"),
            input_source_id: raw.input.get("source_id"),
            event_id: raw.input.get("event_id"),
            source_version: raw.input.get("source_version"),
            pipeline_version: raw.input.get("pipeline_version"),
            kind: raw.input.get("kind"),
            payload_digest: raw.input.get("payload_digest"),
            payload: raw.input.get("payload"),
        })
        .collect();
    let input = DigestInput {
        connection_id: &advance.connection_id,
        dataset: &advance.dataset,
        rows,
        expected_previous_cursor: advance.expected_previous_cursor.as_deref(),
        next_cursor: advance.next_cursor.as_deref(),
    };
    Ok(norn_ir::canonical::domain_hash_hex(
        b"norn.source.page.v2\0",
        &input,
    )?)
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
            "SELECT \"connection_id\", \"dataset\", \"cursor_json\", \"generation\", \"page_digest\", \"updated_at_ms\" \
             FROM \"{SOURCE_CURSOR}\" WHERE \"connection_id\" = ?1 AND \"dataset\" = ?2"
        ),
        params![connection_id, dataset],
        decode_cursor,
    )
}

fn verify_active_lease<E: SqlEngine>(
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
    /// Exact duplicate delivery of an already-committed page; inbox rows are
    /// re-checked idempotently and `_norn_source_cursor` stays at its committed row.
    IdempotentReplay,
}

/// Validate the active fencing lease and cursor precondition inside an open
/// capture transaction before writing any inbox row.
pub(crate) fn check_capture_precondition<E: SqlEngine>(
    conn: &E,
    advance: &CursorAdvanceSpec,
    digest: &str,
) -> Result<Precondition, CellError> {
    verify_active_lease(
        conn,
        &advance.connection_id,
        &advance.lease_id,
        advance.generation,
        advance.now_ms,
    )?;
    let current = read_cursor(conn, &advance.connection_id, &advance.dataset)?;
    let current_json = current.as_ref().map(|record| record.cursor_json.as_str());
    if current_json == advance.expected_previous_cursor.as_deref() {
        return Ok(Precondition::Advance);
    }
    if let Some(existing) = &current
        && advance.next_cursor.as_deref() == Some(existing.cursor_json.as_str())
        && existing.page_digest == digest
    {
        return Ok(Precondition::IdempotentReplay);
    }
    Err(CellError::SourceCursorConflict {
        connection_id: advance.connection_id.clone(),
        dataset: advance.dataset.clone(),
        expected: advance.expected_previous_cursor.clone(),
        actual: current.map(|record| record.cursor_json),
    })
}

/// Persist the advanced cursor, optional post-capture pending record, and
/// optional lease renewal inside the same SQLite transaction that captured the
/// page's inbox rows.
pub(crate) fn commit_cursor_in_tx<E: SqlEngine>(
    conn: &E,
    advance: &CursorAdvanceSpec,
    digest: &str,
    precondition: &Precondition,
    pages: &[String],
    records: &[String],
) -> Result<(), CellError> {
    if matches!(precondition, Precondition::Advance) {
        if !pages.is_empty() || !records.is_empty() {
            let pages_json = serde_json::to_string(pages)?;
            let records_json = serde_json::to_string(records)?;
            conn.execute(
                &format!(
                    "INSERT OR REPLACE INTO \"{SOURCE_PENDING}\" \
                     (\"connection_id\", \"dataset\", \"page_digest\", \"pages_json\", \"records_json\", \"captured_at_ms\") \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)"
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
        if let Some(next_cursor) = &advance.next_cursor {
            conn.execute(
                &format!(
                    "INSERT INTO \"{SOURCE_CURSOR}\" \
                     (\"connection_id\", \"dataset\", \"cursor_json\", \"generation\", \"page_digest\", \"updated_at_ms\") \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
                     ON CONFLICT(\"connection_id\", \"dataset\") DO UPDATE SET \
                     \"cursor_json\" = excluded.\"cursor_json\", \
                     \"generation\" = excluded.\"generation\", \
                     \"page_digest\" = excluded.\"page_digest\", \
                     \"updated_at_ms\" = excluded.\"updated_at_ms\""
                ),
                params![
                    advance.connection_id.as_str(),
                    advance.dataset.as_str(),
                    next_cursor.as_str(),
                    to_i64(advance.generation, "source cursor generation")?,
                    digest,
                    to_i64(advance.now_ms, "source cursor updated_at_ms")?,
                ],
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
                && current.lease_id != lease_id
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
        lease_id: &str,
        generation: u64,
        now_ms: u64,
        cursors: &BTreeMap<String, String>,
    ) -> Result<(), CellError> {
        if cursors.is_empty() {
            return Ok(());
        }
        self.write_tx(|| {
            verify_active_lease(&self.conn, connection_id, lease_id, generation, now_ms)?;
            for (dataset, cursor_json) in cursors {
                self.conn.execute(
                    &format!(
                        "INSERT OR IGNORE INTO \"{SOURCE_CURSOR}\" \
                         (\"connection_id\", \"dataset\", \"cursor_json\", \"generation\", \"page_digest\", \"updated_at_ms\") \
                         VALUES (?1, ?2, ?3, ?4, 'legacy-seed', ?5)"
                    ),
                    params![
                        connection_id,
                        dataset.as_str(),
                        cursor_json.as_str(),
                        to_i64(generation, "source cursor generation")?,
                        to_i64(now_ms, "source cursor updated_at_ms")?,
                    ],
                )?;
            }
            Ok(())
        })
    }

    /// Mark one post-capture pending source page complete once all of its inbox
    /// rows have been processed (landed, already landed, or quarantined).
    ///
    /// # Errors
    /// Store errors.
    pub fn complete_source_pending(
        &mut self,
        connection_id: &str,
        dataset: &str,
        page_digest: &str,
    ) -> Result<(), CellError> {
        self.write_tx(|| {
            self.conn.execute(
                &format!(
                    "DELETE FROM \"{SOURCE_PENDING}\" \
                     WHERE \"connection_id\" = ?1 AND \"dataset\" = ?2 AND \"page_digest\" = ?3"
                ),
                params![connection_id, dataset, page_digest],
            )?;
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
                "SELECT \"connection_id\", \"dataset\", \"cursor_json\", \"generation\", \"page_digest\", \"updated_at_ms\" \
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
    /// canonical processing has not yet completed, ordered by capture time.
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
                 FROM \"{SOURCE_PENDING}\" WHERE \"connection_id\" = ?1 \
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
