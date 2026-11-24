//! CELL-ID-1: fresh stores bind once; existing stores cannot adopt a new identity.

use crate::engine::{SqlEngine, params};
use crate::system::META;
use crate::{CellError, CellStore};
pub use norn_ir::cell_id::CellId;

pub(crate) const KEY: &str = "cell_id";
pub(crate) const FORMAT_KEY: &str = "cell_identity_format";
/// Version of the logical cell binding carried in portable export envelopes.
pub const FORMAT: &str = "norn.cell.identity/v1";

pub(crate) fn from_bindings(
    bindings: &std::collections::BTreeMap<String, String>,
) -> Result<Option<CellId>, CellError> {
    parse_binding(
        bindings.get(FORMAT_KEY).map(String::as_str),
        bindings.get(KEY).map(String::as_str),
    )
}

fn parse_binding(format: Option<&str>, value: Option<&str>) -> Result<Option<CellId>, CellError> {
    let Some(format) = format else {
        return Ok(None);
    };
    if format != FORMAT {
        return Err(CellError::BindingMismatch {
            key: FORMAT_KEY.to_owned(),
            expected: FORMAT.to_owned(),
            found: format.to_owned(),
        });
    }
    let value = value
        .ok_or_else(|| CellError::Encoding("versioned cell binding has no identity".to_owned()))?;
    CellId::try_from(value.to_owned())
        .map(Some)
        .map_err(|reason| CellError::Encoding(reason.to_owned()))
}

pub(crate) fn read(engine: &impl SqlEngine) -> Result<Option<CellId>, CellError> {
    let meta = |key: &str| {
        engine.query_optional(
            &format!("SELECT value FROM \"{META}\" WHERE key = ?1"),
            params![key],
            |row| row.get::<String>(0),
        )
    };
    let format = meta("binding:cell_identity_format")?;
    let value = meta("binding:cell_id")?;
    parse_binding(format.as_deref(), value.as_deref())
}

/// Every export record committed after `checkpoint` (or every record, with
/// none) names this cell's identity (OPEN-CHECKPOINT-1).
///
/// The current binding is live state, so a checkpoint never exempts it: the
/// last export record at or before the checkpoint must be the one it pinned
/// (same commit, same stored digest), hash-verify and name the current
/// identity too (S2, #1551 round 2). Changing the binding, even together
/// with that record and its stored digest, therefore refuses an open at a
/// head checkpoint, where no record follows it, as it refuses a genesis
/// open.
pub(crate) fn verify_history_after(
    engine: &impl SqlEngine,
    checkpoint: Option<&crate::open_checkpoint::OpenCheckpoint>,
) -> Result<(), CellError> {
    let commits = checkpoint.map_or(0, |checkpoint| checkpoint.head.commits);
    if let Some(checkpoint) = checkpoint {
        verify_export_pin(engine, checkpoint)?;
    }
    verify_records_between(engine, commits, u64::MAX)
}

/// The checkpoint's export pin (S2, #1551 round 2): the last record at or
/// before it is the one it pinned, hash-verifies and names the live
/// binding. Shared by the open, the scrub and every advancement (S3).
///
/// # Errors
/// A different, missing or foreign record; SQL failures.
pub(crate) fn verify_export_pin(
    engine: &impl SqlEngine,
    checkpoint: &crate::open_checkpoint::OpenCheckpoint,
) -> Result<(), CellError> {
    let commits = checkpoint.head.commits;
    if last_record_through(engine, commits)? != checkpoint.export {
        return Err(crate::open_checkpoint::corrupt(
            commits,
            "the last export record it covers is not the one it pinned",
        ));
    }
    if let Some(pin) = &checkpoint.export {
        verify_record_names(engine, pin.seq, read(engine)?.as_ref())?;
    }
    Ok(())
}

/// Every export record committed after `after` through `through` names
/// this cell's identity: one chunk of the history scrub, or the whole
/// suffix with `through = u64::MAX`.
///
/// # Errors
/// A record naming another identity; SQL failures.
pub(crate) fn verify_records_between(
    engine: &impl SqlEngine,
    after: u64,
    through: u64,
) -> Result<(), CellError> {
    let current = read(engine)?;
    let mut next = engine.query_optional(
        "SELECT commit_seq FROM _norn_export_journal WHERE commit_seq > ?1 ORDER BY commit_seq LIMIT 1",
        params![crate::read::to_i64(after, "commit sequence")?],
        |row| row.get::<i64>(0),
    )?;
    while let Some(sequence) = next {
        if crate::read::to_u64(sequence, "commit sequence")? > through {
            break;
        }
        verify_record_names(
            engine,
            crate::read::to_u64(sequence, "commit sequence")?,
            current.as_ref(),
        )?;
        next = engine.query_optional(
            "SELECT commit_seq FROM _norn_export_journal WHERE commit_seq > ?1 ORDER BY commit_seq LIMIT 1",
            params![sequence],
            |row| row.get::<i64>(0),
        )?;
    }
    Ok(())
}

/// The commit and stored digest of the last export record at or before
/// `commits`: what an open checkpoint at `commits` pins.
///
/// # Errors
/// SQL failures.
pub(crate) fn last_record_through(
    engine: &impl SqlEngine,
    commits: u64,
) -> Result<Option<crate::open_checkpoint::ExportPin>, CellError> {
    engine
        .query_optional(
            "SELECT commit_seq, hash FROM _norn_export_journal WHERE commit_seq <= ?1 ORDER BY commit_seq DESC LIMIT 1",
            params![crate::read::to_i64(commits, "commit sequence")?],
            |row| Ok((row.get::<i64>(0)?, row.get::<String>(1)?)),
        )?
        .map(|(seq, hash)| {
            Ok(crate::open_checkpoint::ExportPin {
                seq: crate::read::to_u64(seq, "commit sequence")?,
                hash,
            })
        })
        .transpose()
}

/// The hash-verified export record at `sequence` names `current`.
fn verify_record_names(
    engine: &impl SqlEngine,
    sequence: u64,
    current: Option<&CellId>,
) -> Result<(), CellError> {
    let record = crate::export_journal::read_record(engine, sequence)?.ok_or_else(|| {
        CellError::LedgerCorrupt {
            seq: sequence,
            reason: "retained journal record disappeared during admission".to_owned(),
        }
    })?;
    let retained = from_bindings(&record.bindings)?;
    if retained.as_ref() != current {
        return Err(CellError::BindingMismatch {
            key: KEY.to_owned(),
            expected: current.map_or_else(String::new, |id| id.as_str().to_owned()),
            found: retained.map_or_else(String::new, String::from),
        });
    }
    Ok(())
}

pub(crate) fn generate(engine: &impl SqlEngine) -> Result<CellId, CellError> {
    let value: String = engine
        .query_optional("SELECT lower(hex(randomblob(16)))", params![], |row| {
            row.get(0)
        })?
        .ok_or_else(|| CellError::Engine("cell identity source returned no value".to_owned()))?;
    CellId::try_from(value).map_err(|reason| CellError::Encoding(reason.to_owned()))
}

impl<E: SqlEngine> CellStore<E> {
    /// Open an engine under an explicit expected identity, refusing a different file.
    ///
    /// # Errors
    /// Store admission failure or an identity mismatch.
    pub fn from_engine_bound(
        engine: E,
        model: crate::Model,
        expected: &CellId,
    ) -> Result<Self, CellError> {
        // Checked inside the open transaction, so a different file is
        // refused before its open writes anything (ENGINE-UPGRADE-1).
        Self::open_admitted(engine, model, &mut |_| Ok(expected.clone()), Some(expected))
    }

    /// Persisted logical identity. Legacy files without a binding remain local-only.
    ///
    /// # Errors
    /// SQL failure or malformed persisted identity.
    pub fn cell_id(&self) -> Result<Option<CellId>, CellError> {
        self.read().check_live()?;
        read(&self.conn)
    }

    /// CELL-ID-1: validate an explicitly expected identity; never adopt an old file.
    ///
    /// # Errors
    /// Missing, malformed or different identity.
    pub fn require_cell_id(&self, expected: &CellId) -> Result<(), CellError> {
        let found = self.cell_id()?;
        if found.as_ref() == Some(expected) {
            Ok(())
        } else {
            Err(CellError::BindingMismatch {
                key: KEY.to_owned(),
                expected: expected.as_str().to_owned(),
                found: found.map_or_else(String::new, String::from),
            })
        }
    }
}

#[cfg(feature = "native")]
impl CellStore<crate::engine::NativeEngine> {
    /// Open or create a file under an explicitly supplied logical identity.
    ///
    /// # Errors
    /// Store admission failure or a different persisted identity.
    pub fn open_bound(
        path: &std::path::Path,
        model: crate::Model,
        options: crate::StoreOptions,
        expected: &CellId,
    ) -> Result<Self, CellError> {
        Self::from_engine_bound(
            crate::engine::NativeEngine::open(path, options)?,
            model,
            expected,
        )
    }
}
