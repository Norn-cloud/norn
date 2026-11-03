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

pub(crate) fn verify_history(engine: &impl SqlEngine) -> Result<(), CellError> {
    let current = read(engine)?;
    let mut next = engine.query_optional(
        "SELECT commit_seq FROM _norn_export_journal ORDER BY commit_seq LIMIT 1",
        params![],
        |row| row.get::<i64>(0),
    )?;
    while let Some(sequence) = next {
        let sequence_u64 = crate::read::to_u64(sequence, "commit sequence")?;
        let record =
            crate::export_journal::read_record(engine, sequence_u64)?.ok_or_else(|| {
                CellError::LedgerCorrupt {
                    seq: sequence_u64,
                    reason: "retained journal record disappeared during admission".to_owned(),
                }
            })?;
        let retained = from_bindings(&record.bindings)?;
        if retained != current {
            return Err(CellError::BindingMismatch {
                key: KEY.to_owned(),
                expected: current.map_or_else(String::new, String::from),
                found: retained.map_or_else(String::new, String::from),
            });
        }
        next = engine.query_optional(
            "SELECT commit_seq FROM _norn_export_journal WHERE commit_seq > ?1 ORDER BY commit_seq LIMIT 1",
            params![sequence],
            |row| row.get::<i64>(0),
        )?;
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
        let store =
            Self::from_engine_with_identity_source(engine, model, |_| Ok(expected.clone()))?;
        store.require_cell_id(expected)?;
        Ok(store)
    }

    /// Persisted logical identity. Legacy files without a binding remain local-only.
    ///
    /// # Errors
    /// SQL failure or malformed persisted identity.
    pub fn cell_id(&self) -> Result<Option<CellId>, CellError> {
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
