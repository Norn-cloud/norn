//! OPEN-CHECKPOINT-1 (#1533): an open verifies the history committed since
//! the cell's last verified checkpoint, not every commit from genesis, so
//! open time stays flat as the history grows.
//!
//! A checkpoint names a ledger head this engine verified (its commit count,
//! last event sequence and hash), the RFC 6962 root of the first `commits`
//! receipt digests, and digests of what live reads consume from the history
//! it covers: the quantity receipts, the last export record and the retained
//! definition versions. It is written every [`EVERY`] commits and at the end
//! of an open that upgraded the cell. It is metadata, not evidence: it is
//! never part of a receipt, a ledger event or an export.
//!
//! # Genesis parity (#1551 round 3)
//!
//! Every check a genesis open runs has an equivalent from a checkpoint:
//! **(a)** re-run in full on every open, **(b)** pinned by a digest in the
//! checkpoint and re-checked on every open and every advancement, or **(c)**
//! checked on the suffix only, with the reason the prefix cannot affect it.
//! The writer advances a checkpoint only inside the transaction that
//! records it, after verifying from the previous durable checkpoint (from
//! genesis with none) everything it newly covers, never from a handle's
//! cached head (S3): so every row a checkpoint covers was verified once
//! against a pinned anchor, and the pins below keep it from changing since.
//!
//! | Genesis open checks | From a checkpoint |
//! |---|---|
//! | Ledger chain: sequence, `prev` links, entry hashes, every event in its receipt's commit and world, no orphan event | Suffix (a). Prefix (b): the head's sequence and hash are pinned, and the receipt root pins every commit's `ledgerHead`. Every ledger read also rehashes each entry it serves, so an event changed in place is never served. |
//! | Logical time never goes back | (a) on the suffix, from the anchor receipt's own time, in the verifier every open and every advancement share (S1). Prefix (b): receipt bodies are pinned by the root, and their order was checked when the checkpoint advanced over them. |
//! | Receipts: body hash, lookup columns, commit tiling, quantity provenance | Suffix (a). Prefix (b): the root pins every digest; every receipt read checks the body against its digest and its lookup columns against the body, including an idempotency lookup (S4). |
//! | Transparency log | (a) rebuilt from every digest, and (b) the covered digests must rebuild the recorded root. The writer builds that root only from digests equal to the ones on disk. |
//! | Cell identity: every export record names the live binding | Suffix (a). Prefix (b): the last covered record is pinned by commit and stored digest, and must hash-verify and name the live binding (S2). The earlier records are not live; restore and export verify the journal chain themselves. |
//! | Definition history: version order, each install's typed proof, the active pointer | Suffix (a). Prefix: order (a); the active-at-checkpoint version's typed proof (a, S6); every covered version row (b), by the `definitions` digest, since superseded versions are read live (historical compute, mapping bundles, run routes). The active pointer (a). |
//! | Value history: each version against its receipt's digest | Suffix (a). Prefix (c): history is not live state, and every historical read proves the version against its receipt (`object_at_version`), whose digest the root pins. |
//! | Quantity projections (balances, holds, postings, timers, history) and the cell clock | (a): replayed from every quantity receipt; the `quantity_receipts` digest (b) names which covered receipts carry quantity, and a qualified load (S5) refuses an older layout. The clock projection against the head receipt (a). |
//! | Quantity boundary | (b) recorded in the checkpoint, (a) checked against the cell's pointer and every quantity event after it. |
//! | Object identities (normalized keys) | (a) over current state. |
//! | Schema, storage version, markings, fork pin | (a): `init_schema` and the fork pin run before either verification. |
//!
//! Selectors that locate covered rows but are not re-derived on open are
//! cross-checked where they are read: an idempotency lookup against the
//! hashed body (S4); the quantity evidence lookups by ledger `world` against
//! the replayed projections, so a hidden or moved event refuses the write;
//! a payload's receipt `world` against the payload reference it locates.
//!
//! What is not re-read at open is the body of the history before the
//! checkpoint. A consistent rewrite of it changes the anchored head, the
//! receipt root or a pinned digest and refuses. A verification from genesis
//! still checks every row (an operator's scrub opens a copy with no
//! checkpoint row). A checkpoint row that is missing never weakens an open:
//! with none the open verifies from genesis, and one that does not anchor
//! the file, or has an older layout, refuses it. A restore that cuts the
//! history independently of the rest of the file must delete a checkpoint
//! past the cut (`docs/platform/engine-upgrades.md`, "Restores that trim
//! history").
use crate::{
    CellError, LedgerHead,
    engine::{SqlEngine, SqlRow, params},
    read::{to_i64, to_u64},
};

/// The table holding the cell's one open checkpoint.
pub(crate) const TABLE: &str = "_norn_open_checkpoint";

/// Its schema; created by the schema statements of every open (storage
/// version 5), empty until the first checkpoint.
pub(crate) const DDL: &str = r#"CREATE TABLE IF NOT EXISTS "_norn_open_checkpoint" (
      "id" INTEGER NOT NULL PRIMARY KEY CHECK ("id" = 1),
      "commits" INTEGER NOT NULL, "seq" INTEGER NOT NULL, "hash" TEXT NOT NULL,
      "receipts_root" TEXT NOT NULL,
      "boundary_seq" INTEGER, "boundary_commit" INTEGER,
      "quantity_receipts" TEXT NOT NULL,
      "export_seq" INTEGER, "export_hash" TEXT,
      "definitions" TEXT NOT NULL
    )"#;

/// How many commits apart checkpoints are written: an open verifies at most
/// this many commits of history before the latest one.
pub(crate) const EVERY: u64 = 128;

/// One verified ledger head and the transparency root of its receipts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OpenCheckpoint {
    /// The verified head.
    pub(crate) head: LedgerHead,
    /// Lowercase hex of the RFC 6962 root over the first `head.commits`
    /// receipt digests.
    pub(crate) receipts_root: String,
    /// The quantity boundary in force at the head, if the cell has one, so
    /// an open never scans the receipts before it to find it.
    pub(crate) boundary: Option<crate::quantity::FormatBoundary>,
    /// [`quantity_extend`] folded over the `(commit, receipt digest)` of every
    /// quantity receipt among the first `head.commits`, from
    /// [`quantity_seed`]. It names which covered receipts carry quantity, so
    /// an open replays the quantity projections from those receipts alone
    /// and a quantity receipt hidden from that lookup refuses (S1, #1551).
    pub(crate) quantity_receipts: String,
    /// The last export record at or before the head, if any: its commit and
    /// stored digest (S2, #1551 round 2). An open checks the current cell
    /// identity against this record, so the record cannot be rewritten
    /// together with the identity.
    pub(crate) export: Option<ExportPin>,
    /// [`definitions_digest`] over every retained definition version it
    /// covers. Superseded versions are read live too (historical compute,
    /// mapping bundles pinned to a version), so an open re-derives this
    /// digest from the retained rows and refuses a changed one.
    pub(crate) definitions: String,
}

/// One export record named by its commit and stored digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExportPin {
    /// Its commit.
    pub(crate) seq: u64,
    /// Its stored Norn1 digest.
    pub(crate) hash: String,
}

/// The checkpoint row. Every column is qualified by the table alias, so a
/// column an older layout lacks is an error under either setting of
/// SQLite's double-quoted-string mode, never a string literal (S5, #1551).
pub(crate) const LOAD_SQL: &str = r#"SELECT c."commits", c."seq", c."hash", c."receipts_root", c."boundary_seq", c."boundary_commit", c."quantity_receipts", c."export_seq", c."export_hash", c."definitions" FROM "_norn_open_checkpoint" AS c WHERE c."id" = 1"#;

/// The `(commit, receipt digest)` of every quantity receipt, in commit
/// order, read through the receipt table's two quantity indexes (a single
/// request and a group), never by scanning receipt bodies.
pub(crate) const QUANTITY_RECEIPTS_SQL: &str = r#"SELECT "commit_seq", "receipt_hash" FROM "_norn_receipt" INDEXED BY "_norn_receipt/quantity_boundary"
      WHERE json_extract("receipt", '$.quantity.version')='norn.quantity.commit/2'
    UNION
    SELECT "commit_seq", "receipt_hash" FROM "_norn_receipt" INDEXED BY "_norn_receipt/quantity_group_boundary"
      WHERE json_type("receipt", '$.quantity')='array'
    ORDER BY 1"#;

/// The quantity digest of a history with no quantity receipt.
///
/// # Errors
/// Encoding failures.
pub(crate) fn quantity_seed() -> Result<String, CellError> {
    crate::codec::hash_json(&"norn.open-checkpoint.quantity/1")
}

/// `digest` extended by one quantity receipt.
///
/// # Errors
/// Encoding failures.
pub(crate) fn quantity_extend(
    digest: &str,
    commit_seq: u64,
    receipt_hash: &str,
) -> Result<String, CellError> {
    crate::codec::hash_json(&(digest, commit_seq, receipt_hash))
}

/// The digest of the retained definition versions installed at or before
/// commit `commits`, in version order: each row (its canonical set,
/// registry, admission receipt, model and DDL hashes and layout, none of
/// which changes after its install) hashed in turn.
///
/// # Errors
/// Encoding failures.
pub(crate) fn definitions_digest(
    versions: &[crate::store::InstalledDefinitionSet],
    commits: u64,
) -> Result<String, CellError> {
    versions
        .iter()
        .take_while(|version| version.commit_seq <= commits)
        .try_fold(
            crate::codec::hash_json(&"norn.open-checkpoint.definitions/1")?,
            |digest, version| {
                crate::codec::hash_json(&(digest, crate::codec::hash_json(version)?))
            },
        )
}

/// The cell's open checkpoint, if one was written.
///
/// # Errors
/// SQL failures, or a checkpoint row out of range.
pub(crate) fn load(engine: &impl SqlEngine) -> Result<Option<OpenCheckpoint>, CellError> {
    let Some(row) = engine
        .query(LOAD_SQL, params![])?
        .into_iter()
        .next()
    else {
        return Ok(None);
    };
    let row = SqlRow(row);
    Ok(Some(OpenCheckpoint {
        head: LedgerHead {
            commits: to_u64(row.get(0)?, "open checkpoint commits")?,
            seq: to_u64(row.get(1)?, "open checkpoint sequence")?,
            hash: row.get(2)?,
        },
        receipts_root: row.get(3)?,
        boundary: match (row.get::<Option<i64>>(4)?, row.get::<Option<i64>>(5)?) {
            (None, None) => None,
            (Some(seq), Some(commit)) => Some(crate::quantity::FormatBoundary {
                seq: to_u64(seq, "open checkpoint boundary")?,
                commit_seq: to_u64(commit, "open checkpoint boundary commit")?,
            }),
            _ => return Err(corrupt(0, "a half-recorded quantity boundary")),
        },
        quantity_receipts: row.get(6)?,
        export: match (row.get::<Option<i64>>(7)?, row.get::<Option<String>>(8)?) {
            (None, None) => None,
            (Some(seq), Some(hash)) => Some(ExportPin {
                seq: to_u64(seq, "open checkpoint export record")?,
                hash,
            }),
            _ => return Err(corrupt(0, "a half-recorded export record")),
        },
        definitions: row.get(9)?,
    }))
}

/// Record `checkpoint` as the cell's open checkpoint, replacing the last.
///
/// # Errors
/// SQL failures.
pub(crate) fn record(
    engine: &impl SqlEngine,
    checkpoint: &OpenCheckpoint,
) -> Result<(), CellError> {
    engine.execute(
        &format!(
            "INSERT INTO \"{TABLE}\" (\"id\", \"commits\", \"seq\", \"hash\", \"receipts_root\", \"boundary_seq\", \"boundary_commit\", \"quantity_receipts\", \"export_seq\", \"export_hash\", \"definitions\") \
             VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10) \
             ON CONFLICT (\"id\") DO UPDATE SET \"commits\" = excluded.\"commits\", \"seq\" = excluded.\"seq\", \
             \"hash\" = excluded.\"hash\", \"receipts_root\" = excluded.\"receipts_root\", \
             \"boundary_seq\" = excluded.\"boundary_seq\", \"boundary_commit\" = excluded.\"boundary_commit\", \
             \"quantity_receipts\" = excluded.\"quantity_receipts\", \
             \"export_seq\" = excluded.\"export_seq\", \"export_hash\" = excluded.\"export_hash\", \
             \"definitions\" = excluded.\"definitions\""
        ),
        params![
            to_i64(checkpoint.head.commits, "open checkpoint commits")?,
            to_i64(checkpoint.head.seq, "open checkpoint sequence")?,
            checkpoint.head.hash.clone(),
            checkpoint.receipts_root.clone(),
            checkpoint
                .boundary
                .map(|boundary| to_i64(boundary.seq, "open checkpoint boundary"))
                .transpose()?,
            checkpoint
                .boundary
                .map(|boundary| to_i64(boundary.commit_seq, "open checkpoint boundary commit"))
                .transpose()?,
            checkpoint.quantity_receipts.clone(),
            checkpoint
                .export
                .as_ref()
                .map(|pin| to_i64(pin.seq, "open checkpoint export record"))
                .transpose()?,
            checkpoint.export.as_ref().map(|pin| pin.hash.clone()),
            checkpoint.definitions.clone()
        ],
    )
}

/// Lowercase hex of a transparency root.
pub(crate) fn root_hex(root: &[u8]) -> String {
    use std::fmt::Write as _;
    root.iter()
        .fold(String::with_capacity(64), |mut hex, byte| {
            let _infallible = write!(hex, "{byte:02x}");
            hex
        })
}

/// A checkpoint that no longer anchors the file.
pub(crate) fn corrupt(commits: u64, reason: &str) -> CellError {
    CellError::LedgerCorrupt {
        seq: commits,
        reason: format!("open checkpoint: {reason}"),
    }
}

#[cfg(all(test, feature = "native"))]
#[allow(clippy::expect_used, reason = "test assertions")]
mod tests {
    /// S5 (#1551 round 2): a checkpoint table of an older layout, with no
    /// `quantity_receipts` column, refuses the load under either setting of
    /// SQLite's double-quoted-string mode. The unqualified form the load
    /// used before reads the missing column back as its own name when the
    /// mode is on.
    #[test]
    fn an_older_checkpoint_layout_refuses_under_either_quoted_string_mode() {
        use rusqlite::config::DbConfig;
        const UNQUALIFIED: &str = r#"SELECT "commits", "quantity_receipts" FROM "_norn_open_checkpoint" WHERE "id" = 1"#;
        for dqs in [true, false] {
            let conn = rusqlite::Connection::open_in_memory().expect("database");
            conn.set_db_config(DbConfig::SQLITE_DBCONFIG_DQS_DML, dqs)
                .expect("quoted-string mode");
            conn.execute_batch(
                r#"CREATE TABLE "_norn_open_checkpoint" ("id" INTEGER PRIMARY KEY, "commits" INTEGER NOT NULL,
                     "seq" INTEGER NOT NULL, "hash" TEXT NOT NULL, "receipts_root" TEXT NOT NULL,
                     "boundary_seq" INTEGER, "boundary_commit" INTEGER);
                   INSERT INTO "_norn_open_checkpoint" VALUES (1, 128, 300, 'h', 'r', NULL, NULL);"#,
            )
            .expect("older layout");
            if dqs {
                let read: String = conn
                    .query_row(UNQUALIFIED, [], |row| row.get(1))
                    .expect("the unqualified form reads a literal");
                assert_eq!(read, "quantity_receipts");
            }
            assert!(
                conn.prepare(super::LOAD_SQL).is_err(),
                "the load admitted an older layout (quoted-string mode {dqs})"
            );
        }
    }
}
