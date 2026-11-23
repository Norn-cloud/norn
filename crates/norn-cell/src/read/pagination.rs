//! Binding-indexed / sorted pagination types and key-order object listing.

use std::collections::BTreeSet;

use norn_projections::sqlite_ddl::{ID_COLUMN, TYPE_COLUMN, WORLD_COLUMN, quote_identifier};
use serde::{Deserialize, Serialize};

use super::{ObjectRecord, StoreReader, placeholders};
use crate::engine::{MAX_PARAMETERS, MAX_SAFE_INTEGER, SqlEngine, SqlRow, SqlValue, params};
use crate::error::CellError;

/// One bounded root candidate for a `WidgetIR` key-order scan.
#[derive(Debug)]
pub struct BindingRootCandidate {
    /// Owned identity when it fits the fresh request's candidate budget.
    /// An intrinsically oversized identity is unavailable and omitted.
    pub identity: Option<(String, String)>,
    /// Persistent high-water locator; zero means the scan reached its end.
    pub locator: u64,
    /// Whether another raw candidate follows this position.
    pub has_more: bool,
}

/// A sort value retained in a sealed widget-list continuation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BindingSortValue {
    /// An integer or boolean column value.
    Integer(i64),
    /// A text, enum, or date column value.
    Text(String),
    /// A nullable sort property with no value.
    Null,
}

/// A stable keyset boundary for a sorted widget list.
///
/// The sort value and identity are copied into the sealed cursor so a deleted
/// boundary row cannot invalidate the next page's seek.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BindingSortedPosition {
    /// The property's SQLite sort value.
    pub value: BindingSortValue,
    /// The identity tie-breaker, always ascending.
    pub identity: String,
}

/// One bounded candidate from an indexed widget sort scan.
#[derive(Debug)]
pub struct BindingSortedCandidate {
    /// Owned identity when both identity and sort position fit the request.
    pub identity: Option<(String, String)>,
    /// Stable position to seal as the continuation boundary.
    pub position: Option<BindingSortedPosition>,
    /// Whether another raw candidate follows this position.
    pub has_more: bool,
}

impl<E: SqlEngine> StoreReader<'_, E> {
    /// Up to `limit` objects of `object_type` (or of a subtype) in `world`,
    /// ordered by id.
    ///
    /// # Errors
    ///
    /// Unknown type; SQL engine or decode errors.
    pub fn list(
        &self,
        world: &str,
        object_type: &str,
        limit: usize,
    ) -> Result<Vec<ObjectRecord>, CellError> {
        self.list_after(world, object_type, None, limit)
    }

    /// Up to `limit` objects after `after` of `object_type` (and its
    /// subtypes) in `world`, ordered by the root identity key. The root
    /// table's `(world, id)` primary key makes each identity unique across
    /// the hierarchy, so the key is also a stable tie-break for concrete
    /// object types.
    ///
    /// # Errors
    ///
    /// Unknown type; SQL engine or decode errors.
    pub fn list_after(
        &self,
        world: &str,
        object_type: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ObjectRecord>, CellError> {
        let ids = self.type_ids_after(world, object_type, after, limit)?;
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(record) = self.object(world, object_type, &id)? {
                out.push(record);
            }
        }
        Ok(out)
    }

    /// The first `limit` identity keys after `after` of `object_type` (and
    /// its subtypes) in `world`, in key order. Reads no object values.
    pub(crate) fn type_ids_after(
        &self,
        world: &str,
        object_type: &str,
        after: Option<&str>,
        limit: usize,
    ) -> Result<BTreeSet<String>, CellError> {
        let tables = self.model.tables(object_type)?;
        let Some(root) = tables.first() else {
            return Err(CellError::UnknownObjectType(object_type.to_owned()));
        };
        if limit == 0 {
            return Ok(BTreeSet::new());
        }
        let safe_max = usize::try_from(MAX_SAFE_INTEGER).unwrap_or(usize::MAX);
        let cap = limit.min(safe_max);
        let fetch = i64::try_from(cap)
            .unwrap_or(MAX_SAFE_INTEGER)
            .min(MAX_SAFE_INTEGER);
        let type_names: Vec<String> = self
            .model
            .object_types()
            .filter(|definition| self.model.is_a(definition.name.as_str(), object_type))
            .map(|definition| definition.name.as_str().to_owned())
            .collect();
        let after_clause = if after.is_some() {
            format!(" AND {} > ?", quote_identifier(ID_COLUMN))
        } else {
            String::new()
        };
        let fixed_parameters = if after.is_some() { 3 } else { 2 };
        let chunk_size = MAX_PARAMETERS.saturating_sub(fixed_parameters).max(1);
        let mut ids = BTreeSet::new();
        for type_chunk in type_names.chunks(chunk_size) {
            let placeholders = placeholders(type_chunk.len());
            let sql = format!(
                "SELECT {} FROM {} WHERE {} = ? AND {} IN ({placeholders}){after_clause} ORDER BY {} LIMIT ?",
                quote_identifier(ID_COLUMN),
                quote_identifier(&root.table),
                quote_identifier(WORLD_COLUMN),
                quote_identifier(TYPE_COLUMN),
                quote_identifier(ID_COLUMN),
            );
            let mut bindings =
                Vec::with_capacity(type_chunk.len().saturating_add(fixed_parameters));
            bindings.push(SqlValue::from(world));
            bindings.extend(type_chunk.iter().map(|name| SqlValue::from(name.as_str())));
            if let Some(after) = after {
                bindings.push(SqlValue::from(after));
            }
            bindings.push(SqlValue::from(fetch));
            for values in self.conn.query(&sql, &bindings)? {
                let id: String = SqlRow(values).get(0)?;
                ids.insert(id);
                if ids.len() > cap {
                    let _ignored = ids.pop_last();
                }
            }
        }
        Ok(ids)
    }

    /// The ids and concrete types of at most `max_rows` rows of the root
    /// table of `object_type`'s hierarchy in `world`, by id, and whether
    /// that was every row. Every examined row counts, whatever its type, so
    /// the work is bounded however many siblings share the table.
    ///
    /// # Errors
    ///
    /// Unknown type; SQL engine errors.
    pub fn root_rows(
        &self,
        world: &str,
        object_type: &str,
        max_rows: usize,
    ) -> Result<(Vec<(String, String)>, bool), CellError> {
        let root = self.root_type(object_type)?;
        let table = self.model.table(root)?;
        let sql = format!(
            "SELECT {}, {} FROM {} WHERE {} = ?1 ORDER BY {} LIMIT ?2",
            quote_identifier(ID_COLUMN),
            quote_identifier(TYPE_COLUMN),
            quote_identifier(&table.table),
            quote_identifier(WORLD_COLUMN),
            quote_identifier(ID_COLUMN),
        );
        let safe_max_rows = usize::try_from(MAX_SAFE_INTEGER)
            .unwrap_or(usize::MAX)
            .saturating_sub(1);
        let max_rows = max_rows.min(safe_max_rows);
        let fetch = i64::try_from(max_rows.saturating_add(1)).unwrap_or(MAX_SAFE_INTEGER);
        let rows = self.conn.query(&sql, params![world, fetch])?;
        let mut out = Vec::new();
        let mut exhausted = true;
        for values in rows {
            let row = SqlRow(values);
            if out.len() >= max_rows {
                exhausted = false;
                break;
            }
            out.push((row.get(0)?, row.get(1)?));
        }
        Ok((out, exhausted))
    }
}
