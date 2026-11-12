//! BULK-LINEAGE-1: the cell's record of each loaded dataset. A dataset's
//! rows live in the tenant's engine; the cell keeps which load is current,
//! the file it came from (URI and SHA-256) and what was admitted, so every
//! answer over the dataset names its load and every row traces to
//! `(uri, sha256, row)`.

use serde::{Deserialize, Serialize};

use crate::engine::{SqlEngine, params};
use crate::system::META;
use crate::{CellError, CellStore};

/// One load of a dataset, as the engine reported it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DatasetRecord {
    /// The load identity.
    pub load: String,
    /// The file's durable URI.
    pub uri: String,
    /// The file's SHA-256, verified by the engine.
    pub sha256: String,
    /// Rows admitted.
    pub rows: u64,
    /// Rows quarantined.
    pub rejected: u64,
    /// Quarantined rows per reason code.
    pub reasons: std::collections::BTreeMap<String, u64>,
    /// The verified head the load was recorded at.
    pub recorded_at: u64,
}

fn key(object_type: &str) -> String {
    format!("analytics.dataset:{object_type}")
}

impl<E: SqlEngine> CellStore<E> {
    /// The current load of the dataset `object_type`, if it is one.
    ///
    /// # Errors
    /// A store error or an undecodable record.
    pub fn analytics_dataset(&self, object_type: &str) -> Result<Option<DatasetRecord>, CellError> {
        let value = self.conn.query_optional(
            &format!("SELECT \"value\" FROM \"{META}\" WHERE \"key\" = ?1"),
            params![key(object_type)],
            |row| row.get::<String>(0),
        )?;
        value
            .map(|text| serde_json::from_str(&text).map_err(|e| CellError::Encoding(e.to_string())))
            .transpose()
    }

    /// Record `record` as the current load of the dataset `object_type`.
    ///
    /// # Errors
    /// A store error.
    pub fn record_analytics_dataset(&self, object_type: &str, record: &DatasetRecord) -> Result<(), CellError> {
        let text = crate::codec::canonical_json(record)?;
        self.conn.transaction(|| {
            self.conn.execute(
                &format!("DELETE FROM \"{META}\" WHERE \"key\" = ?1"),
                params![key(object_type)],
            )?;
            self.conn.execute(
                &format!("INSERT INTO \"{META}\" (\"key\", \"value\") VALUES (?1, ?2)"),
                params![key(object_type), text],
            )
        })
    }
}
