//! OPEN-CHECKPOINT-1 (#1533): an open verifies the history committed since
//! the cell's last verified checkpoint, not every commit from genesis, so
//! open time stays flat as the history grows.
//!
//! A checkpoint names a ledger head this engine verified (its commit count,
//! last event sequence and hash), the RFC 6962 root of the first `commits`
//! receipt digests, and digests of what live reads consume from the history
//! it covers: the quantity receipts, the last export record and the retained
//! definition versions. It is metadata, not evidence: it is never part of a
//! receipt, a ledger event or an export.
//!
//! # Claim and threat model (#1551 round 4)
//!
//! The adversary can rewrite the cell file (any row, its digests, and the
//! checkpoint row together) but does not hold the host's checkpoint key.
//!
//! - A checkpointed cell detects every edit a genesis open detects. Edits
//!   to live state are detected at open. Edits to history are detected
//!   within one history scrub ([`crate::scrub`]), and no read that depends
//!   on that history being complete is served before it ends.
//! - Forging a checkpoint needs the key. The row is MAC'd (HMAC-SHA256,
//!   [`CheckpointKey`]) over every column and the cell's identity, under a
//!   key the host keeps outside the file. A pin stored in the file cannot
//!   authenticate itself (round 3, S2); only the MAC makes the pins trusted.
//! - Missing key, a malformed row, a MAC that does not verify, no identity,
//!   or a checkpoint past the end of the file (a restore that trimmed
//!   history): the open is a genesis open, never a refusal.
//! - Not claimed: a rollback of the whole file to an older consistent state
//!   (a genesis open does not detect it either), an adversary holding the
//!   key, or completeness of history without reading it. A per-row
//!   authenticated index is a post-beta option.
//!
//! # Genesis parity
//!
//! Every check a genesis open runs has an equivalent from a checkpoint.
//! **(a)** re-run in full on every open; **(b)** pinned by a digest in the
//! MAC'd row and re-checked on every open and every advancement; **(c)**
//! checked on the suffix only, with the reason the prefix cannot affect what
//! is served. The scrub column says what the history scrub re-runs from
//! genesis over the prefix, and what waits for it.
//!
//! | Genesis open checks | From a checkpoint, at open | Scrub, from genesis |
//! |---|---|---|
//! | Ledger chain: sequence, `prev` links, entry hashes, every event in its receipt's commit and world, no orphan event | Suffix (a). Prefix (b): the head's sequence and hash, and the receipt root (which pins every commit's `ledgerHead`). | Full chain (S8). Ledger, finding and export reads wait for it. |
//! | Logical time never goes back | Suffix (a), from the anchor receipt's time (S1). | Full. |
//! | Receipts: body hash, lookup columns, commit tiling, quantity provenance | Suffix (a). Prefix (b) by the root; every receipt read checks body and columns (S4). | Every receipt, columns included, and the rebuilt root against the row's. An idempotency miss waits for it (S4). |
//! | Transparency log | (a) rebuilt; (b) the covered digests rebuild the root. | Rebuilt again from the bodies it verified; it seeds the next root. |
//! | Cell identity: every export record names the live binding | Suffix (a). Prefix (b): the last covered record, pinned by commit and digest, names the live binding (S2). | Every record. |
//! | Definition history: order, each install's typed proof, the active pointer | Suffix (a). Prefix: order (a); the active version's proof (a, S6); every covered version (b) by the `definitions` digest. The pointer (a). | Every install, superseded ones included (S9). |
//! | Value history: each version against its receipt's digest | Suffix (a). Prefix (c): exact-version reads prove their version. | Every version (S7). Historical frames, as-of reads and version lists wait for it. |
//! | Quantity projections and the cell clock | (a): replayed from every quantity receipt; (b) the `quantity_receipts` digest names them; a qualified load (S5). | The digest is re-folded from the verified receipts for the seed. |
//! | Quantity boundary | (b) recorded; (a) against the pointer and the suffix. | Re-derived from the markers; must equal the open's. |
//! | Object identities | (a) over current state. | Live: not repeated. |
//! | Schema, storage version, markings, fork pin | (a). | Live: not repeated. |
//!
//! # Advancement
//!
//! A checkpoint advances or is re-seeded only from scrub-verified state.
//! A commit advances it every [`EVERY`] commits, from the row this handle
//! validated, once its scrub has ended. It re-runs the open's pin checks on
//! that row (S3) and verifies at most [`MAX_ADVANCE`] commits, after
//! confirming the database tail inside the checkpoint transaction (S10). A
//! longer gap goes to the scrub, off the commit path (S10). The scrub seeds
//! the row at the head it verified.

pub(crate) mod verify;

use crate::{
    CellError, LedgerHead,
    engine::{MAX_SAFE_INTEGER, SqlEngine, SqlRow, SqlValue, params},
    read::to_i64,
    system::{GENESIS_HASH, META},
};

/// The table holding the cell's one open checkpoint.
pub(crate) const TABLE: &str = "_norn_open_checkpoint";

/// `_norn_meta` key marking a cell durably fenced by the history scrub (S12).
/// Reopening or writing a cell carrying this marker fails closed with
/// [`CellError::Fenced`].
pub(crate) const FENCE_KEY: &str = "open_checkpoint:fenced";

/// Its schema; created by the schema statements of every open (storage
/// version 6), empty until the first checkpoint.
pub(crate) const DDL: &str = r#"CREATE TABLE IF NOT EXISTS "_norn_open_checkpoint" (
      "id" INTEGER NOT NULL PRIMARY KEY CHECK ("id" = 1),
      "commits" INTEGER NOT NULL, "seq" INTEGER NOT NULL, "hash" TEXT NOT NULL,
      "receipts_root" TEXT NOT NULL,
      "boundary_seq" INTEGER, "boundary_commit" INTEGER,
      "quantity_receipts" TEXT NOT NULL,
      "export_seq" INTEGER, "export_hash" TEXT,
      "definitions" TEXT NOT NULL,
      "mac" TEXT NOT NULL
    )"#;

/// How many commits apart checkpoints are written: an open verifies at most
/// this many commits of history before the latest one.
pub(crate) const EVERY: u64 = 128;

/// The most commits a commit's own checkpoint advancement verifies. A
/// longer gap (a checkpoint seeded behind a busy head) is closed by the
/// scrub, off the commit path, never inside a commit (S10, #1551 round 3).
pub(crate) const MAX_ADVANCE: u64 = 2 * EVERY;

/// OPEN-CHECKPOINT-1 (#1551 round 4): the key the host authenticates its
/// cells' open checkpoints with. It is held outside every cell file (the
/// host's secret store) and given to the engine when the host opens it
/// ([`crate::StoreOptions::checkpoint_key`],
/// [`crate::engine::SqlEngine::checkpoint_key`]). A checkpoint row is
/// trusted only when its HMAC-SHA256 under this key verifies, over the row
/// and the cell's identity; anyone who can write the file but does not hold
/// the key cannot forge one, nor copy one from another cell.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct CheckpointKey([u8; 32]);

impl std::fmt::Debug for CheckpointKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("CheckpointKey(<redacted>)")
    }
}

impl CheckpointKey {
    /// A key of 32 secret bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// A key given as 64 hexadecimal digits (how a secret store holds it).
    ///
    /// # Errors
    /// [`CellError::Encoding`] for anything but 64 hexadecimal digits.
    pub fn from_hex(text: &str) -> Result<Self, CellError> {
        let bytes = decode_hex(text.trim())
            .filter(|bytes| bytes.len() == 32)
            .ok_or_else(|| {
                CellError::Encoding("an open-checkpoint key is 64 hexadecimal digits".to_owned())
            })?;
        let mut key = [0_u8; 32];
        key.copy_from_slice(&bytes);
        Ok(Self(key))
    }
}

fn decode_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    text.as_bytes()
        .chunks(2)
        .map(|pair| {
            let digit = |byte: u8| char::from(byte).to_digit(16);
            let (high, low) = (digit(*pair.first()?)?, digit(*pair.get(1)?)?);
            u8::try_from(high.checked_mul(16)?.checked_add(low)?).ok()
        })
        .collect()
}

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

/// What an open finds in the checkpoint table (#1551 round 4). Only
/// [`Loaded::Authentic`] shortens an open; every other case is a genesis
/// open, never a refusal.
#[derive(Debug)]
pub(crate) enum Loaded {
    /// No checkpoint was written.
    Absent,
    /// The host gave no key, so no row can be trusted.
    Unkeyed,
    /// The cell has no identity binding to authenticate the row with.
    Unbound,
    /// The row is malformed or its MAC does not verify under the host's key
    /// and this cell's identity: a rewritten or corrupted row, one copied from
    /// another cell, or one written under another key.
    Unauthentic,
    /// The row this engine recorded for this cell.
    Authentic(OpenCheckpoint),
}

/// The HMAC-SHA256, under `key`, of `checkpoint` bound to the cell
/// identity `cell_id`: a domain tag, the identity and every column, in one
/// canonical JSON array (RFC 8785 via `norn_ir::canonical::canonical_json_of`).
fn mac(
    key: &CheckpointKey,
    cell_id: &str,
    checkpoint: &OpenCheckpoint,
) -> Result<hmac::Hmac<transparency_sha2::Sha256>, CellError> {
    use hmac::{KeyInit as _, Mac as _};
    let message = norn_ir::canonical::canonical_json_of(&(
        "norn.open-checkpoint.mac/1",
        cell_id,
        checkpoint.head.commits,
        checkpoint.head.seq,
        &checkpoint.head.hash,
        &checkpoint.receipts_root,
        checkpoint
            .boundary
            .map(|boundary| (boundary.seq, boundary.commit_seq)),
        &checkpoint.quantity_receipts,
        checkpoint.export.as_ref().map(|pin| (pin.seq, &pin.hash)),
        &checkpoint.definitions,
    ))?;
    let mut mac = hmac::Hmac::<transparency_sha2::Sha256>::new_from_slice(&key.0)
        .map_err(|_length| CellError::Encoding("open-checkpoint key length".to_owned()))?;
    mac.update(&message);
    Ok(mac)
}

/// The checkpoint row. Every column is qualified by the table alias, so a
/// column an older layout lacks is an error under either setting of
/// SQLite's double-quoted-string mode, never a string literal (S5, #1551).
///
/// S2 (#1551 round 5): every column is projected safely inside SQLite so
/// malformed values confined to `_norn_open_checkpoint` (integers outside
/// `0..=MAX_SAFE_INTEGER`, non-finite floats, invalid UTF-8 text, or wrong
/// storage classes) never abort row decoding at the Native or Workers SQL
/// boundary before `decode_with_tag` classifies the row as
/// [`Loaded::Unauthentic`]. Genuine SQL/schema errors still propagate.
pub(crate) const LOAD_SQL: &str = concat!(
    r#"SELECT "#,
    r#"CASE WHEN typeof(c."commits") = 'integer' AND c."commits" >= 0 AND c."commits" <= 9007199254740992 THEN c."commits" ELSE -1 END, "#,
    r#"CASE WHEN typeof(c."seq") = 'integer' AND c."seq" >= 0 AND c."seq" <= 9007199254740992 THEN c."seq" ELSE -1 END, "#,
    r#"CASE WHEN typeof(c."hash") = 'text' THEN CAST(c."hash" AS BLOB) ELSE NULL END, "#,
    r#"CASE WHEN typeof(c."receipts_root") = 'text' THEN CAST(c."receipts_root" AS BLOB) ELSE NULL END, "#,
    r#"CASE WHEN c."boundary_seq" IS NULL THEN NULL WHEN typeof(c."boundary_seq") = 'integer' AND c."boundary_seq" >= 0 AND c."boundary_seq" <= 9007199254740992 THEN c."boundary_seq" ELSE -1 END, "#,
    r#"CASE WHEN c."boundary_commit" IS NULL THEN NULL WHEN typeof(c."boundary_commit") = 'integer' AND c."boundary_commit" >= 0 AND c."boundary_commit" <= 9007199254740992 THEN c."boundary_commit" ELSE -1 END, "#,
    r#"CASE WHEN typeof(c."quantity_receipts") = 'text' THEN CAST(c."quantity_receipts" AS BLOB) ELSE NULL END, "#,
    r#"CASE WHEN c."export_seq" IS NULL THEN NULL WHEN typeof(c."export_seq") = 'integer' AND c."export_seq" >= 0 AND c."export_seq" <= 9007199254740992 THEN c."export_seq" ELSE -1 END, "#,
    r#"CASE WHEN c."export_hash" IS NULL THEN NULL WHEN typeof(c."export_hash") = 'text' THEN CAST(c."export_hash" AS BLOB) ELSE -1 END, "#,
    r#"CASE WHEN typeof(c."definitions") = 'text' THEN CAST(c."definitions" AS BLOB) ELSE NULL END, "#,
    r#"CASE WHEN typeof(c."mac") = 'text' THEN CAST(c."mac" AS BLOB) ELSE NULL END "#,
    r#"FROM "_norn_open_checkpoint" AS c WHERE c."id" = 1"#,
);

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
            |digest, version| crate::codec::hash_json(&(digest, crate::codec::hash_json(version)?)),
        )
}

/// The cell's open checkpoint (#1551 round 4): authentic only when the row
/// decodes cleanly and its MAC verifies under the engine's key and the cell's
/// identity. Any malformed row (wrong SQLite column types, out-of-range
/// integers, half-null optional pairs, bad MAC length or encoding) is treated
/// as [`Loaded::Unauthentic`] so the open falls back to genesis instead of
/// panicking, refusing a valid cell, or trusting partial fields (S2).
///
/// # Errors
/// SQL failures, or a checkpoint table of an older layout (S5).
pub(crate) fn load(engine: &impl SqlEngine) -> Result<Loaded, CellError> {
    use hmac::Mac as _;
    let Some(row) = engine.query(LOAD_SQL, params![])?.into_iter().next() else {
        return Ok(Loaded::Absent);
    };
    let Some(key) = engine.checkpoint_key() else {
        return Ok(Loaded::Unkeyed);
    };
    let Some(cell_id) = crate::cell_identity::read(engine)? else {
        return Ok(Loaded::Unbound);
    };
    let row = SqlRow(row);
    let Some((checkpoint, tag)) = decode_with_tag(&row) else {
        return Ok(Loaded::Unauthentic);
    };
    let verified =
        mac(&key, cell_id.as_str(), &checkpoint).is_ok_and(|mac| mac.verify_slice(&tag).is_ok());
    Ok(if verified {
        Loaded::Authentic(checkpoint)
    } else {
        Loaded::Unauthentic
    })
}

fn unsigned_column(row: &SqlRow, index: usize) -> Option<u64> {
    let value = row.get::<i64>(index).ok()?;
    (0..=MAX_SAFE_INTEGER)
        .contains(&value)
        .then(|| u64::try_from(value).ok())
        .flatten()
}

enum OptUnsigned {
    Null,
    Value(u64),
    Malformed,
}

fn opt_unsigned_column(row: &SqlRow, index: usize) -> OptUnsigned {
    match row.get::<Option<i64>>(index) {
        Ok(None) => OptUnsigned::Null,
        Ok(Some(value)) if (0..=MAX_SAFE_INTEGER).contains(&value) => {
            u64::try_from(value).map_or(OptUnsigned::Malformed, OptUnsigned::Value)
        }
        Ok(Some(_)) | Err(_) => OptUnsigned::Malformed,
    }
}

fn text_column(row: &SqlRow, index: usize) -> Option<String> {
    match row.get_ref(index).ok()? {
        SqlValue::Blob(bytes) => String::from_utf8(bytes.clone()).ok(),
        SqlValue::Text(text) => Some(text.clone()),
        SqlValue::Null | SqlValue::Integer(_) | SqlValue::Real(_) => None,
    }
}

enum OptText {
    Null,
    Value(String),
    Malformed,
}

fn opt_text_column(row: &SqlRow, index: usize) -> OptText {
    match row.get_ref(index) {
        Ok(SqlValue::Null) => OptText::Null,
        Ok(SqlValue::Blob(bytes)) => {
            String::from_utf8(bytes.clone()).map_or(OptText::Malformed, OptText::Value)
        }
        Ok(SqlValue::Text(text)) => OptText::Value(text.clone()),
        Ok(SqlValue::Integer(_) | SqlValue::Real(_)) | Err(_) => OptText::Malformed,
    }
}

/// Decode all 11 checkpoint columns and the 32-byte MAC tag, returning `None`
/// for any column count, type, range, UTF-8, structural or MAC-encoding defect (S2).
fn decode_with_tag(row: &SqlRow) -> Option<(OpenCheckpoint, [u8; 32])> {
    if row.0.len() != 11 {
        return None;
    }
    let commits = unsigned_column(row, 0)?;
    let seq = unsigned_column(row, 1)?;
    let hash = text_column(row, 2)?;
    let receipts_root = text_column(row, 3)?;
    if commits == 0 && (seq != 0 || hash != GENESIS_HASH) {
        return None;
    }
    if commits > 0 && seq == 0 {
        return None;
    }
    let boundary = match (opt_unsigned_column(row, 4), opt_unsigned_column(row, 5)) {
        (OptUnsigned::Null, OptUnsigned::Null) => None,
        (OptUnsigned::Value(b_seq), OptUnsigned::Value(commit_seq)) => {
            Some(crate::quantity::FormatBoundary {
                seq: b_seq,
                commit_seq,
            })
        }
        _ => return None,
    };
    let quantity_receipts = text_column(row, 6)?;
    let export = match (opt_unsigned_column(row, 7), opt_text_column(row, 8)) {
        (OptUnsigned::Null, OptText::Null) => None,
        (OptUnsigned::Value(export_seq), OptText::Value(export_hash)) => Some(ExportPin {
            seq: export_seq,
            hash: export_hash,
        }),
        _ => return None,
    };
    let definitions = text_column(row, 9)?;
    let mac_text = text_column(row, 10)?;
    let tag_bytes = decode_hex(&mac_text)?;
    let tag: [u8; 32] = tag_bytes.try_into().ok()?;
    Some((
        OpenCheckpoint {
            head: LedgerHead { seq, hash, commits },
            receipts_root,
            boundary,
            quantity_receipts,
            export,
            definitions,
        },
        tag,
    ))
}

/// Record `checkpoint` as the cell's open checkpoint, replacing the last,
/// MAC'd under the engine's key and bound to the cell's identity. Returns
/// whether it was recorded: a host with no key, or a cell with no identity,
/// records none, and its opens verify from genesis.
///
/// # Errors
/// SQL failures.
pub(crate) fn record(
    engine: &impl SqlEngine,
    checkpoint: &OpenCheckpoint,
) -> Result<bool, CellError> {
    use hmac::Mac as _;
    let (Some(key), Some(cell_id)) = (engine.checkpoint_key(), crate::cell_identity::read(engine)?)
    else {
        return Ok(false);
    };
    let tag = root_hex(
        &mac(&key, cell_id.as_str(), checkpoint)?
            .finalize()
            .into_bytes(),
    );
    engine.execute(
        &format!(
            "INSERT INTO \"{TABLE}\" (\"id\", \"commits\", \"seq\", \"hash\", \"receipts_root\", \"boundary_seq\", \"boundary_commit\", \"quantity_receipts\", \"export_seq\", \"export_hash\", \"definitions\", \"mac\") \
             VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11) \
             ON CONFLICT (\"id\") DO UPDATE SET \"commits\" = excluded.\"commits\", \"seq\" = excluded.\"seq\", \
             \"hash\" = excluded.\"hash\", \"receipts_root\" = excluded.\"receipts_root\", \
             \"boundary_seq\" = excluded.\"boundary_seq\", \"boundary_commit\" = excluded.\"boundary_commit\", \
             \"quantity_receipts\" = excluded.\"quantity_receipts\", \
             \"export_seq\" = excluded.\"export_seq\", \"export_hash\" = excluded.\"export_hash\", \
             \"definitions\" = excluded.\"definitions\", \"mac\" = excluded.\"mac\""
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
            checkpoint.definitions.clone(),
            tag
        ],
    )?;
    Ok(true)
}

/// Drop the cell's checkpoint, so its next open verifies from genesis.
///
/// # Errors
/// SQL failures.
pub(crate) fn drop_row(engine: &impl SqlEngine) -> Result<(), CellError> {
    engine.execute(&format!("DELETE FROM \"{TABLE}\""), params![])
}

/// Read any durable history-scrub fence recorded in `_norn_meta` (S12).
/// A non-text value in the fence row fails closed as a fence finding.
///
/// # Errors
/// SQL failures.
pub(crate) fn read_fence(engine: &impl SqlEngine) -> Result<Option<String>, CellError> {
    let has_meta = engine
        .query_optional(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            params![META],
            |row| row.get::<i64>(0),
        )?
        .is_some();
    if !has_meta {
        return Ok(None);
    }
    let value = engine.query_optional(
        &format!("SELECT \"value\" FROM \"{META}\" WHERE \"key\" = ?1"),
        params![FENCE_KEY],
        |row| row.get_ref(0).cloned(),
    )?;
    Ok(value.map(|val| match val {
        SqlValue::Text(finding) if !finding.is_empty() => finding,
        SqlValue::Null
        | SqlValue::Integer(_)
        | SqlValue::Real(_)
        | SqlValue::Text(_)
        | SqlValue::Blob(_) => "corrupt durable history-scrub fence marker".to_owned(),
    }))
}

/// Read any durable fence in `_norn_meta` on an already-open cell, in one
/// primary-key lookup (S12).
///
/// # Errors
/// SQL failures.
pub(crate) fn read_fence_open(engine: &impl SqlEngine) -> Result<Option<String>, CellError> {
    let value = engine.query_optional(
        &format!("SELECT \"value\" FROM \"{META}\" WHERE \"key\" = ?1"),
        params![FENCE_KEY],
        |row| row.get_ref(0).cloned(),
    )?;
    Ok(value.map(|val| match val {
        SqlValue::Text(finding) if !finding.is_empty() => finding,
        SqlValue::Null
        | SqlValue::Integer(_)
        | SqlValue::Real(_)
        | SqlValue::Text(_)
        | SqlValue::Blob(_) => "corrupt durable history-scrub fence marker".to_owned(),
    }))
}

/// Refuse with [`CellError::Fenced`] if the cell carries a durable fence (S12).
///
/// # Errors
/// [`CellError::Fenced`] or SQL failures.
pub(crate) fn check_fence(engine: &impl SqlEngine) -> Result<(), CellError> {
    if let Some(finding) = read_fence(engine)? {
        return Err(CellError::Fenced(finding));
    }
    Ok(())
}

/// Persist the durable fence and drop the checkpoint row in one transaction
/// so subsequent calls, sibling handles and reopens all fail closed (S12).
///
/// # Errors
/// SQL failures.
pub(crate) fn fence_durably(engine: &impl SqlEngine, finding: &str) -> Result<(), CellError> {
    engine.transaction(|| {
        drop_row(engine)?;
        engine.execute(
            &format!(
                "INSERT INTO \"{META}\" (\"key\", \"value\") VALUES (?1, ?2) \
                 ON CONFLICT (\"key\") DO UPDATE SET \"value\" = excluded.\"value\""
            ),
            params![FENCE_KEY, finding],
        )
    })
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
mod tests {
    use super::{CheckpointKey, Loaded, OpenCheckpoint, load, mac};
    use crate::CellError;
    use crate::engine::{NativeEngine, SqlEngine as _};
    use hmac::Mac as _;

    /// 32 random bytes from SQLite's `randomblob`: a key for this test only.
    fn random_bytes() -> Result<[u8; 32], CellError> {
        let conn = rusqlite::Connection::open_in_memory()?;
        let bytes: Vec<u8> = conn.query_row("SELECT randomblob(32)", [], |row| row.get(0))?;
        bytes
            .try_into()
            .map_err(|_bytes| CellError::Encoding("32 random bytes".to_owned()))
    }

    fn checkpoint() -> OpenCheckpoint {
        OpenCheckpoint {
            head: crate::LedgerHead {
                commits: 128,
                seq: 300,
                hash: "h".to_owned(),
            },
            receipts_root: "r".to_owned(),
            boundary: None,
            quantity_receipts: "q".to_owned(),
            export: None,
            definitions: "d".to_owned(),
        }
    }

    fn verifies(
        key: &CheckpointKey,
        cell: &str,
        row: &OpenCheckpoint,
        tag: &[u8],
    ) -> Result<bool, CellError> {
        Ok(mac(key, cell, row)?.verify_slice(tag).is_ok())
    }

    /// #1551 round 4: the MAC binds every column, the cell's identity and
    /// the key. A row MAC'd for one cell does not verify for another, and
    /// one changed column, or another key, fails it.
    #[test]
    fn the_mac_binds_the_row_the_cell_and_the_key() -> Result<(), CellError> {
        let key = CheckpointKey::from_bytes(random_bytes()?);
        let row = checkpoint();
        let tag = mac(&key, "cell-a", &row)?.finalize().into_bytes();
        assert!(verifies(&key, "cell-a", &row, &tag)?);
        assert!(
            !verifies(&key, "cell-b", &row, &tag)?,
            "copied across cells"
        );
        let mut changed = row.clone();
        changed.receipts_root = "r2".to_owned();
        assert!(!verifies(&key, "cell-a", &changed, &tag)?, "a changed root");
        let other = CheckpointKey::from_bytes(random_bytes()?);
        assert!(!verifies(&other, "cell-a", &row, &tag)?, "another key");
        Ok(())
    }

    #[test]
    fn a_key_is_64_hexadecimal_digits() -> Result<(), CellError> {
        let bytes = random_bytes()?;
        let hex = super::root_hex(&bytes);
        let key = CheckpointKey::from_hex(&hex)?;
        assert_eq!(key, CheckpointKey::from_bytes(bytes));
        assert!(CheckpointKey::from_hex("07").is_err());
        assert!(CheckpointKey::from_hex(&"zz".repeat(32)).is_err());
        assert_eq!(format!("{key:?}"), "CheckpointKey(<redacted>)");
        Ok(())
    }

    /// S2 (#1551): malformed checkpoint rows (wrong SQLite column types,
    /// negative or out-of-range integers, floats, non-finite floats, invalid
    /// UTF-8 text, BLOBs, half-null boundary/export pairs, short or non-hex
    /// MAC) decode as `Loaded::Unauthentic` rather than erroring or panicking.
    #[test]
    fn malformed_checkpoint_rows_are_unauthentic() -> Result<(), CellError> {
        let key = CheckpointKey::from_bytes(random_bytes()?);
        let cases = [
            // Non-integer commits (SQLite allows text in non-STRICT tables).
            "INSERT INTO _norn_open_checkpoint VALUES (1, 'bad', 1, 'h', 'r', NULL, NULL, 'q', NULL, NULL, 'd', '00')",
            // Negative commits.
            "INSERT INTO _norn_open_checkpoint VALUES (1, -5, 1, 'h', 'r', NULL, NULL, 'q', NULL, NULL, 'd', '00')",
            // Out-of-range positive integer (> MAX_SAFE_INTEGER / i64::MAX).
            "INSERT INTO _norn_open_checkpoint VALUES (1, 9223372036854775807, 1, 'h', 'r', NULL, NULL, 'q', NULL, NULL, 'd', '00')",
            "INSERT INTO _norn_open_checkpoint VALUES (1, 9007199254740993, 1, 'h', 'r', NULL, NULL, 'q', NULL, NULL, 'd', '00')",
            // Out-of-range negative integer (i64::MIN).
            "INSERT INTO _norn_open_checkpoint VALUES (1, -9223372036854775808, 1, 'h', 'r', NULL, NULL, 'q', NULL, NULL, 'd', '00')",
            // Float and non-finite float (+Inf, -Inf) in integer and text columns.
            "INSERT INTO _norn_open_checkpoint VALUES (1, 1.5, 1, 'h', 'r', NULL, NULL, 'q', NULL, NULL, 'd', '00')",
            "INSERT INTO _norn_open_checkpoint VALUES (1, 9e999, 1, 'h', 'r', NULL, NULL, 'q', NULL, NULL, 'd', '00')",
            "INSERT INTO _norn_open_checkpoint VALUES (1, 1, 1, -9e999, 'r', NULL, NULL, 'q', NULL, NULL, 'd', '00')",
            // Invalid UTF-8 bytes stored with TEXT storage class.
            "INSERT INTO _norn_open_checkpoint VALUES (1, 1, 1, CAST(X'fffe' AS TEXT), 'r', NULL, NULL, 'q', NULL, NULL, 'd', '00')",
            "INSERT INTO _norn_open_checkpoint VALUES (1, 1, 1, 'h', 'r', NULL, NULL, 'q', 1, CAST(X'fffe' AS TEXT), 'd', '00')",
            "INSERT INTO _norn_open_checkpoint VALUES (1, 1, 1, 'h', 'r', NULL, NULL, 'q', NULL, NULL, 'd', CAST(X'fffe' AS TEXT))",
            // BLOB in integer and text columns.
            "INSERT INTO _norn_open_checkpoint VALUES (1, X'deadbeef', 1, 'h', 'r', NULL, NULL, 'q', NULL, NULL, 'd', '00')",
            "INSERT INTO _norn_open_checkpoint VALUES (1, 1, 1, 'h', 'r', NULL, NULL, 'q', NULL, NULL, 'd', X'0102')",
            // Short MAC hex.
            "INSERT INTO _norn_open_checkpoint VALUES (1, 1, 1, 'h', 'r', NULL, NULL, 'q', NULL, NULL, 'd', 'abcd')",
            // Non-hex MAC.
            "INSERT INTO _norn_open_checkpoint VALUES (1, 1, 1, 'h', 'r', NULL, NULL, 'q', NULL, NULL, 'd', 'zzzz')",
            // Half-null boundary.
            "INSERT INTO _norn_open_checkpoint VALUES (1, 1, 1, 'h', 'r', 5, NULL, 'q', NULL, NULL, 'd', '0000000000000000000000000000000000000000000000000000000000000000')",
            // Half-null export pin.
            "INSERT INTO _norn_open_checkpoint VALUES (1, 1, 1, 'h', 'r', NULL, NULL, 'q', 1, NULL, 'd', '0000000000000000000000000000000000000000000000000000000000000000')",
            // Non-text hash column.
            "INSERT INTO _norn_open_checkpoint VALUES (1, 1, 1, 42, 'r', NULL, NULL, 'q', NULL, NULL, 'd', '0000000000000000000000000000000000000000000000000000000000000000')",
            // commits > 0 with seq == 0.
            "INSERT INTO _norn_open_checkpoint VALUES (1, 5, 0, 'h', 'r', NULL, NULL, 'q', NULL, NULL, 'd', '0000000000000000000000000000000000000000000000000000000000000000')",
        ];
        for sql in cases {
            let engine = NativeEngine::open_in_memory()?.with_checkpoint_key(Some(key));
            engine.execute_batch(&format!(
                r#"CREATE TABLE "_norn_meta" ("key" TEXT PRIMARY KEY, "value" TEXT NOT NULL);
                   INSERT INTO "_norn_meta" VALUES ('binding:cell_id', '0123456789abcdef0123456789abcdef');
                   INSERT INTO "_norn_meta" VALUES ('binding:cell_identity_format', '{}');"#,
                crate::cell_identity::FORMAT
            ))?;
            engine.execute_batch(super::DDL)?;
            engine.execute_batch(sql)?;
            let loaded = load(&engine)?;
            assert!(
                matches!(loaded, Loaded::Unauthentic),
                "expected Unauthentic for `{sql}`, got {loaded:?}"
            );
        }
        Ok(())
    }

    /// S5 (#1551 round 2): a checkpoint table of an older layout, with no
    /// `quantity_receipts` column, refuses the load under either setting of
    /// SQLite's double-quoted-string mode. The unqualified form the load
    /// used before reads the missing column back as its own name when the
    /// mode is on.
    #[test]
    fn an_older_checkpoint_layout_refuses_under_either_quoted_string_mode() -> Result<(), CellError>
    {
        use rusqlite::config::DbConfig;
        const UNQUALIFIED: &str =
            r#"SELECT "commits", "quantity_receipts" FROM "_norn_open_checkpoint" WHERE "id" = 1"#;
        for dqs in [true, false] {
            let conn = rusqlite::Connection::open_in_memory()?;
            conn.set_db_config(DbConfig::SQLITE_DBCONFIG_DQS_DML, dqs)?;
            conn.execute_batch(
                r#"CREATE TABLE "_norn_open_checkpoint" ("id" INTEGER PRIMARY KEY, "commits" INTEGER NOT NULL,
                     "seq" INTEGER NOT NULL, "hash" TEXT NOT NULL, "receipts_root" TEXT NOT NULL,
                     "boundary_seq" INTEGER, "boundary_commit" INTEGER);
                   INSERT INTO "_norn_open_checkpoint" VALUES (1, 128, 300, 'h', 'r', NULL, NULL);"#,
            )?;
            if dqs {
                let read: String = conn.query_row(UNQUALIFIED, [], |row| row.get(1))?;
                assert_eq!(read, "quantity_receipts");
            }
            assert!(
                conn.prepare(super::LOAD_SQL).is_err(),
                "the load admitted an older layout (quoted-string mode {dqs})"
            );
        }
        Ok(())
    }
}
