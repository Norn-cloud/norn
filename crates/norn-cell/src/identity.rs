//! ID-UNIQUE-1: bounded normalized-key equality in the shared store transaction.
use std::collections::{BTreeMap, BTreeSet};

use norn_checker::{EffectiveKey, EffectiveKind, EffectiveObjectType};
use norn_ir::PropertyType;
use norn_projections::sqlite_ddl::{SCOPE_COLUMN, ValueEncoding, WORLD_COLUMN, quote_identifier};

use crate::codec::decode;
use crate::engine::{SqlEngine, SqlValue, params};
use crate::read::read_projection;
use crate::{CellError, StoreReader};

const MAX_ROWS: usize = 4096;
const MAX_BYTES: usize = 8 * 1024 * 1024;
const MAX_STEPS: usize = 16;

pub(crate) fn validate_model(
    objects: &BTreeMap<String, EffectiveObjectType>,
    definitions: &[norn_ir::ObjectTypeDef],
) -> Result<(), CellError> {
    for definition in definitions {
        if definition.identity.iter().any(|key| {
            key.rule
                .as_ref()
                .is_some_and(|rule| !rule.normalize.is_empty())
        }) && !objects.contains_key(definition.name.as_str())
        {
            return Err(CellError::Encoding(
                "normalized identity has no resolved effective contract".into(),
            ));
        }
    }
    for object in objects.values() {
        for key in object.keys.iter().filter(|key| !key.normalize.is_empty()) {
            if key.match_policy.is_some() {
                return Err(CellError::Encoding(
                    "normalized identity does not admit entity-resolution policies".into(),
                ));
            }
            for component in &key.components {
                let supported = object.property(component.as_str()).is_some_and(|property| {
                    matches!(
                        property.kind,
                        EffectiveKind::Base(
                            PropertyType::Text { .. }
                                | PropertyType::Enum { .. }
                                | PropertyType::Integer {}
                                | PropertyType::Boolean {}
                        )
                    )
                });
                if !supported
                    || key
                        .normalize
                        .get(component)
                        .is_some_and(|steps| steps.len() > MAX_STEPS)
                {
                    return Err(CellError::Encoding("normalized identity requires text, enum, integer or boolean components and at most 16 steps per component".into()));
                }
            }
        }
    }
    Ok(())
}

/// A complete scan is required. Scope/world belong to stored rows, never business fields.
pub(crate) fn validate<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    changed: Option<&[crate::WrittenRow]>,
) -> Result<(), CellError> {
    let mut rows = MAX_ROWS;
    let mut keys_left = MAX_ROWS;
    let mut bytes = MAX_BYTES;
    for object in reader.model().identity_objects() {
        for key in object
            .keys
            .iter()
            .filter(|key| key.declared_by == object.name && !key.normalize.is_empty())
        {
            if changed.is_some_and(|writes| {
                !writes.iter().any(|row| {
                    reader
                        .model()
                        .is_a(&row.object_type, key.declared_by.as_str())
                })
            }) {
                continue;
            }
            keys_left = keys_left
                .checked_sub(1)
                .ok_or(CellError::OutOfRange("normalized identity scan keys"))?;
            check_key(reader, object, key, &mut rows, &mut bytes)?;
        }
    }
    Ok(())
}

/// Validate current rows under a candidate key contract before any DDL changes.
pub(crate) fn preflight<E: SqlEngine>(
    engine: &E,
    previous: &crate::Model,
    candidate: &crate::Model,
) -> Result<(), CellError> {
    let reader = StoreReader::new(engine, candidate);
    let mut rows = MAX_ROWS;
    let mut bytes = MAX_BYTES;
    let mut keys_left = MAX_ROWS;
    for object in candidate.identity_objects() {
        let Some(old) = previous.schema().object_table(object.name.as_str()) else {
            continue;
        };
        for key in object
            .keys
            .iter()
            .filter(|key| key.declared_by == object.name && !key.normalize.is_empty())
        {
            keys_left = keys_left
                .checked_sub(1)
                .ok_or(CellError::OutOfRange("normalized identity scan keys"))?;
            if key.components.iter().any(|component| {
                !old.columns
                    .iter()
                    .any(|column| column.property == *component)
            }) {
                let occupied = engine
                    .query_optional(
                        &format!("SELECT 1 FROM {} LIMIT 1", quote_identifier(&old.table)),
                        params![],
                        |row| row.get::<i64>(0),
                    )?
                    .is_some();
                if occupied {
                    return Err(CellError::DefinitionMigrationRefused("normalized keys over added live columns require backfill-aware identity admission".into()));
                }
                continue;
            }
            check_key(&reader, object, key, &mut rows, &mut bytes)?;
        }
    }
    Ok(())
}

fn check_key<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    object: &EffectiveObjectType,
    key: &EffectiveKey,
    rows_left: &mut usize,
    bytes_left: &mut usize,
) -> Result<(), CellError> {
    let table = reader.model().table(key.declared_by.as_str())?;
    let mut columns = Vec::new();
    for component in &key.components {
        let column = table
            .columns
            .iter()
            .find(|column| column.property == *component)
            .ok_or_else(|| {
                CellError::Encoding("identity component has no physical column".into())
            })?;
        let physical = column.column.as_deref().ok_or_else(|| {
            CellError::Encoding("identity component is in overflow storage".into())
        })?;
        columns.push((column, physical));
    }
    let selected = [
        quote_identifier(WORLD_COLUMN),
        quote_identifier(SCOPE_COLUMN),
    ]
    .into_iter()
    .chain(columns.iter().map(|(_, name)| quote_identifier(name)))
    .collect::<Vec<_>>();
    let table_name = quote_identifier(&table.table);
    let count = scan_count(reader.conn, &table_name, &selected, rows_left, bytes_left)?;
    let projection = read_projection(
        &[(WORLD_COLUMN, false), (SCOPE_COLUMN, false)]
            .into_iter()
            .chain(
                columns
                    .iter()
                    .map(|(column, name)| (*name, column.encoding == ValueEncoding::Integer)),
            )
            .collect::<Vec<_>>(),
    );
    let sql = format!(
        "SELECT {} FROM {table_name} LIMIT {count}",
        projection.join(", ")
    );
    let mut seen = BTreeSet::new();
    for raw in reader.conn.query(&sql, params![])? {
        let (Some(SqlValue::Text(world)), Some(SqlValue::Text(scope))) = (raw.first(), raw.get(1))
        else {
            return Err(CellError::Encoding(
                "invalid identity world or scope".into(),
            ));
        };
        let mut payload = serde_json::Map::new();
        for (index, (column, _)) in columns.iter().enumerate() {
            let offset = index
                .checked_add(2)
                .ok_or(CellError::OutOfRange("identity columns"))?;
            let value = raw
                .get(offset)
                .ok_or_else(|| CellError::Encoding("missing identity column".into()))?;
            payload.insert(
                column.property.as_str().to_owned(),
                serde_json::to_value(decode(&table.table, column, value)?)?,
            );
        }
        let tuple = key
            .value(object, &payload)
            .ok_or_else(|| CellError::Encoding("unsupported normalized identity value".into()))?;
        for value in &tuple {
            let size = match value {
                norn_checker::KeyComponent::Text(text) => text.len(),
                norn_checker::KeyComponent::Integer(_) => std::mem::size_of::<i128>(),
                norn_checker::KeyComponent::Boolean(_) => 1,
            };
            *bytes_left = bytes_left
                .checked_sub(size)
                .ok_or(CellError::OutOfRange("normalized identity scan bytes"))?;
        }
        if !seen.insert((world.clone(), scope.clone(), tuple)) {
            return Err(CellError::IdentityCollision {
                owner: key.declared_by.as_str().to_owned(),
                key: key.name.as_str().to_owned(),
            });
        }
    }
    Ok(())
}

fn scan_count<E: SqlEngine>(
    engine: &E,
    table_name: &str,
    columns: &[String],
    rows_left: &mut usize,
    bytes_left: &mut usize,
) -> Result<usize, CellError> {
    let size = columns
        .iter()
        .map(|name| format!("COALESCE(length(CAST({name} AS BLOB)), 0)"))
        .collect::<Vec<_>>()
        .join(" + ");
    let bound = rows_left
        .checked_add(1)
        .ok_or(CellError::OutOfRange("identity scan limit"))?;
    let preflight = format!(
        "SELECT COUNT(*), COALESCE(SUM(n), 0) FROM (SELECT {size} AS n FROM {table_name} LIMIT {bound})"
    );
    let (count, bytes) = engine
        .query_optional(&preflight, params![], |row| {
            Ok((row.get::<i64>(0)?, row.get::<i64>(1)?))
        })?
        .ok_or_else(|| CellError::Encoding("identity preflight returned no count".into()))?;
    let count =
        usize::try_from(count).map_err(|_negative| CellError::OutOfRange("identity row count"))?;
    let bytes =
        usize::try_from(bytes).map_err(|_negative| CellError::OutOfRange("identity byte count"))?;
    *rows_left = rows_left
        .checked_sub(count)
        .ok_or(CellError::OutOfRange("normalized identity scan rows"))?;
    *bytes_left = bytes_left
        .checked_sub(bytes)
        .ok_or(CellError::OutOfRange("normalized identity scan bytes"))?;
    Ok(count)
}
