//! OPEN-CHECKPOINT-1 (#1533): an open verifies the history committed since
//! the cell's last verified checkpoint, not every commit from genesis, so
//! open time stays flat as the history grows.
//!
//! A checkpoint names a ledger head this engine verified (its commit count,
//! last event sequence and hash) and the RFC 6962 root of the first
//! `commits` receipt digests. It is written every [`EVERY`] commits, after
//! [`crate::CellStore::verified_head`] has verified the chain up to it, and
//! at the end of an open that upgraded the cell. It is metadata, not
//! evidence: it is never part of a receipt, a ledger event or an export.
//!
//! An open from a checkpoint admits the file only if:
//! - the checkpoint still anchors the file: the ledger entry at its sequence
//!   carries its hash, and the receipt of its last commit names that head
//!   and verifies;
//! - the receipt digests of its first `commits` commits rebuild its
//!   recorded transparency root, so no receipt before it was replaced, even
//!   one rewritten together with its own hash;
//! - everything after it verifies as a genesis open verifies it: the hash
//!   chain and receipts, the definition history, the value history and the
//!   cell identity of each export record.
//!
//! What is not re-read at open is the body of the history before the
//! checkpoint. A consistent rewrite of it changes the anchored head or the
//! receipt root and refuses; an isolated edit of an old row that keeps its
//! stored hashes is refused when that row is read, because every historical
//! read verifies its own proof (a value version against its receipt's
//! digest, a receipt against its hash), and a verification from genesis
//! finds it (an operator's scrub opens a copy with no checkpoint row). A
//! checkpoint row that is missing or
//! malformed never weakens an open: with no checkpoint the open verifies
//! from genesis, and one that does not anchor the file refuses it.
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
      "receipts_root" TEXT NOT NULL
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
}

/// The cell's open checkpoint, if one was written.
///
/// # Errors
/// SQL failures, or a checkpoint row out of range.
pub(crate) fn load(engine: &impl SqlEngine) -> Result<Option<OpenCheckpoint>, CellError> {
    let Some(row) = engine
        .query(
            &format!(
                "SELECT \"commits\", \"seq\", \"hash\", \"receipts_root\" FROM \"{TABLE}\" WHERE \"id\" = 1"
            ),
            params![],
        )?
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
            "INSERT INTO \"{TABLE}\" (\"id\", \"commits\", \"seq\", \"hash\", \"receipts_root\") VALUES (1, ?1, ?2, ?3, ?4) \
             ON CONFLICT (\"id\") DO UPDATE SET \"commits\" = excluded.\"commits\", \"seq\" = excluded.\"seq\", \
             \"hash\" = excluded.\"hash\", \"receipts_root\" = excluded.\"receipts_root\""
        ),
        params![
            to_i64(checkpoint.head.commits, "open checkpoint commits")?,
            to_i64(checkpoint.head.seq, "open checkpoint sequence")?,
            checkpoint.head.hash.clone(),
            checkpoint.receipts_root.clone()
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
