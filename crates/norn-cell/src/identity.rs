//! ID-UNIQUE-1: bounded normalized-key equality in the shared store transaction.
use std::collections::{BTreeMap, BTreeSet};

use norn_checker::{EffectiveKey, EffectiveKind, EffectiveObjectType};
use norn_ir::PropertyType;
use norn_projections::sqlite_ddl::{SCOPE_COLUMN, WORLD_COLUMN, quote_identifier};

use crate::codec::decode;
use crate::engine::{SqlEngine, SqlValue, params};
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
        columns.push((column, quote_identifier(physical)));
    }
    let selected = [
        quote_identifier(WORLD_COLUMN),
        quote_identifier(SCOPE_COLUMN),
    ]
    .into_iter()
    .chain(columns.iter().map(|(_, name)| name.clone()))
    .collect::<Vec<_>>();
    let size = selected
        .iter()
        .map(|name| format!("COALESCE(length(CAST({name} AS BLOB)), 0)"))
        .collect::<Vec<_>>()
        .join(" + ");
    let bound = rows_left
        .checked_add(1)
        .ok_or(CellError::OutOfRange("identity scan limit"))?;
    let table_name = quote_identifier(&table.table);
    let preflight = format!(
        "SELECT COUNT(*), COALESCE(SUM(n), 0) FROM (SELECT {size} AS n FROM {table_name} LIMIT {bound})"
    );
    let (count, bytes) = reader
        .conn
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
    let sql = format!(
        "SELECT {} FROM {table_name} LIMIT {count}",
        selected.join(", ")
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
            if let norn_checker::KeyComponent::Text(text) = value {
                *bytes_left = bytes_left
                    .checked_sub(text.len())
                    .ok_or(CellError::OutOfRange("normalized identity scan bytes"))?;
            }
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
