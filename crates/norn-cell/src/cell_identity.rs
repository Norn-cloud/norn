//! CELL-ID-1: fresh stores bind once; existing stores cannot adopt a new identity.

use crate::engine::{SqlEngine, params};
use crate::system::META;
use crate::{CellError, CellStore};
pub use norn_ir::cell_id::CellId;

pub(crate) const KEY: &str = "cell_id";

pub(crate) fn read(engine: &impl SqlEngine) -> Result<Option<CellId>, CellError> {
    let value: Option<String> = engine.query_optional(
        &format!("SELECT value FROM \"{META}\" WHERE key = ?1"),
        params!["binding:cell_id"],
        |row| row.get(0),
    )?;
    value
        .map(|value| {
            CellId::try_from(value).map_err(|reason| CellError::Encoding(reason.to_owned()))
        })
        .transpose()
}

pub(crate) fn verify_history(engine: &impl SqlEngine) -> Result<(), CellError> {
    let current = read(engine)?;
    if let Some(first) = crate::export_journal::read_record(engine, 1)? {
        let original = first.bindings.get(KEY);
        if original.map(String::as_str) != current.as_ref().map(CellId::as_str) {
            return Err(CellError::BindingMismatch {
                key: KEY.to_owned(),
                expected: original.cloned().unwrap_or_default(),
                found: current.map_or_else(String::new, String::from),
            });
        }
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
