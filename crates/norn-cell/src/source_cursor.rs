//! SOURCE-CAPTURE-1: durable source-sync leases and atomic page cursors.
//!
//! Each source connection holds at most one active lease in `_norn_source_lease`
//! with a strictly monotonic fencing `generation`. Page capture
//! ([`crate::CellStore::capture_source_page`]) verifies the active lease, checks
//! the expected previous cursor in `_norn_source_cursor` (admitting exact
//! idempotent duplicate deliveries), captures every row or quarantine evidence,
//! and advances `_norn_source_cursor` in one SQLite transaction.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::engine::{SqlEngine, SqlRow, params};
use crate::error::CellError;
use crate::read::StoreReader;
use crate::store::CellStore;

/// Durable source-sync lease table (one row per connection; monotonic `generation`).
pub(crate) const SOURCE_LEASE: &str = "_norn_source_lease";
/// Durable source-sync cursor table (one row per `(connection_id, dataset)`).
pub(crate) const SOURCE_CURSOR: &str = "_norn_source_cursor";

/// Additive DDL for source-sync leases and cursors. Older engines ignore these
/// tables, so no storage version bump is required.
pub(crate) const DDL: [&str; 2] = [
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
    /// Content digest of the page that committed this cursor (for duplicate delivery).
    pub page_digest: String,
    /// Timestamp when this cursor was committed (milliseconds since epoch).
    pub updated_at_ms: u64,
}

/// Precondition and atomic cursor advancement carried by one captured page.
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

impl CursorAdvanceSpec {
    /// Clone this spec with `next_cursor = None` for non-final sub-pages while
    /// preserving lease fencing and cursor precondition checks.
    #[must_use]
    pub fn without_cursor_advance(&self) -> Self {
        Self {
            connection_id: self.connection_id.clone(),
            dataset: self.dataset.clone(),
            lease_id: self.lease_id.clone(),
            generation: self.generation,
            now_ms: self.now_ms,
            expected_previous_cursor: self.expected_previous_cursor.clone(),
            next_cursor: None,
            renew_ttl_ms: self.renew_ttl_ms,
        }
    }
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

pub(crate) fn page_digest(
    advance: &CursorAdvanceSpec,
    observation_ids: &[&str],
) -> Result<String, CellError> {
    #[derive(Serialize)]
    struct DigestInput<'a> {
        connection_id: &'a str,
        dataset: &'a str,
        observation_ids: &'a [&'a str],
        expected_previous_cursor: Option<&'a str>,
        next_cursor: Option<&'a str>,
    }
    let input = DigestInput {
        connection_id: &advance.connection_id,
        dataset: &advance.dataset,
        observation_ids,
        expected_previous_cursor: advance.expected_previous_cursor.as_deref(),
        next_cursor: advance.next_cursor.as_deref(),
    };
    Ok(norn_ir::canonical::domain_hash_hex(
        b"norn.source.page.v1\0",
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
    let fenced = || CellError::SourceLeaseFenced {
        connection_id: advance.connection_id.clone(),
        lease_id: advance.lease_id.clone(),
        generation: advance.generation,
    };
    let Some(lease) = read_lease(conn, &advance.connection_id)? else {
        return Err(fenced());
    };
    if lease.lease_id != advance.lease_id
        || lease.generation != advance.generation
        || advance.now_ms >= lease.expires_at_ms
    {
        return Err(fenced());
    }
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

/// Persist the advanced cursor and optional lease renewal inside the same SQLite
/// transaction that captured the page's inbox rows.
pub(crate) fn commit_cursor_in_tx<E: SqlEngine>(
    conn: &E,
    advance: &CursorAdvanceSpec,
    digest: &str,
    precondition: &Precondition,
) -> Result<(), CellError> {
    if matches!(precondition, Precondition::Advance)
        && let Some(next_cursor) = &advance.next_cursor
    {
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
}
