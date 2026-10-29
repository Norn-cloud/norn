//! Property values ↔ SQLite values, by [`ValueEncoding`].
//!
//! Instance values are exact runtime values (#2770): decimals are exact
//! [`norn_ir::Decimal`]s, stored as their canonical text and read back as numbers
//! (an exact decimal string is also accepted on write). Integers are
//! integral numbers or exact integer strings stored with SQLite's `INTEGER`
//! affinity; values outside the SQL seam's safe-integer range bind as decimal
//! text so SQLite stores the same signed 64-bit integer. Booleans are stored
//! as `0`/`1`; `json-ext` values are stored as Norn1 canonical JSON text and
//! read back exactly. Nothing passes through `f64`.

use norn_checker::property_codec::{self, EncodedValue, PropertyCodecError};
use norn_ir::canonical::HashProfile;
use norn_ir::{PropertyKind, RuntimeValue as Value};
use norn_projections::{PropertyColumn, ValueEncoding};
use serde::Serialize;

use crate::engine::{MAX_SAFE_INTEGER, SqlValue};
use crate::error::CellError;
use crate::model::Model;

/// The effective contract that owns a decoded property value.
#[derive(Debug, Clone, Copy)]
pub(crate) enum ValueOwner<'a> {
    /// A property stored on an object type.
    Object { object_type: &'a str },
    /// A property stored on a link declared by an object type.
    Link { source_type: &'a str, link: &'a str },
}

/// Model contract and owner for one committed-value read.
#[derive(Debug, Clone, Copy)]
pub(crate) struct DecodeContext<'m, 'o> {
    pub(crate) model: &'m Model,
    pub(crate) owner: ValueOwner<'o>,
}

/// Canonicalize one already-decoded stored value through its effective
/// property contract. This is shared by SQL-column, overflow, and selected
/// property reads so legacy temporal spellings do not escape through a read
/// path. Invalid source strings remain intact for quality reporting.
pub(crate) fn canonicalize_stored_value(
    context: DecodeContext<'_, '_>,
    column: &PropertyColumn,
    value: &Value,
) -> Value {
    match context.owner {
        ValueOwner::Object { object_type } => {
            context
                .model
                .canonicalize_object_value(object_type, column.property.as_str(), value)
        }
        ValueOwner::Link { source_type, link } => context.model.canonicalize_link_value(
            source_type,
            link,
            column.property.as_str(),
            value,
        ),
    }
}

/// Decode one SQL column and apply the same effective-contract canonicalizer
/// used by JSON overflow values.
pub(crate) fn decode_stored_value(
    context: DecodeContext<'_, '_>,
    table: &str,
    column: &PropertyColumn,
    value: &SqlValue,
) -> Result<Value, CellError> {
    Ok(canonicalize_stored_value(
        context,
        column,
        &decode(table, column, value)?,
    ))
}

/// Encode `value` for `column` (`owner` names the type for errors).
pub(crate) fn encode(
    owner: &str,
    column: &PropertyColumn,
    value: &Value,
) -> Result<SqlValue, CellError> {
    let mismatch = |expected: &'static str| CellError::ValueType {
        owner: owner.to_owned(),
        property: column.property.as_str().to_owned(),
        expected,
    };
    match property_codec::encode(property_kind(column.encoding), value) {
        Ok(EncodedValue::Null) if !column.nullable => Err(CellError::MissingValue {
            owner: owner.to_owned(),
            property: column.property.as_str().to_owned(),
        }),
        Ok(EncodedValue::Null) => Ok(SqlValue::Null),
        Ok(EncodedValue::Integer(value)) => Ok(sql_integer(value)),
        Ok(EncodedValue::Text(value)) => Ok(SqlValue::Text(value)),
        Err(PropertyCodecError::ValueType(expected)) => Err(mismatch(expected)),
        Err(PropertyCodecError::Runtime(error)) => Err(CellError::Value(error)),
        Err(PropertyCodecError::Corrupt) => Err(mismatch("an encodable value")),
    }
}

/// Decode a stored value of `column` (`table` names it for errors).
pub(crate) fn decode(
    table: &str,
    column: &PropertyColumn,
    value: &SqlValue,
) -> Result<Value, CellError> {
    let corrupt = || CellError::CorruptValue {
        table: table.to_owned(),
        column: column.property.as_str().to_owned(),
    };
    let encoded = match value {
        SqlValue::Null => EncodedValue::Null,
        SqlValue::Integer(value) => EncodedValue::Integer(*value),
        SqlValue::Text(value) => EncodedValue::Text(value.clone()),
        SqlValue::Real(_) | SqlValue::Blob(_) => return Err(corrupt()),
    };
    property_codec::decode(property_kind(column.encoding), &encoded)
        .map_err(|_codec_error| corrupt())
}

const fn property_kind(encoding: ValueEncoding) -> PropertyKind {
    match encoding {
        ValueEncoding::Text => PropertyKind::Text,
        ValueEncoding::Decimal => PropertyKind::Decimal,
        ValueEncoding::Integer => PropertyKind::Integer,
        ValueEncoding::Boolean => PropertyKind::Boolean,
        ValueEncoding::Json => PropertyKind::JsonExt,
    }
}

/// Norn1 canonical JSON text of `value` (exact: runtime values serialize their
/// decimals as exact number tokens).
pub(crate) fn canonical_json<T: Serialize + ?Sized>(value: &T) -> Result<String, CellError> {
    let bytes = norn_ir::canonical::canonical_json_of(value)?;
    // Norn1 output is UTF-8 JSON text by construction.
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Norn1 hash (lower-case hex SHA-256 with the Norn1 domain tag) of `value`.
pub(crate) fn hash_json<T: Serialize + ?Sized>(value: &T) -> Result<String, CellError> {
    let bytes = norn_ir::canonical::canonical_json_of(value)?;
    Ok(HashProfile::Norn1.hash_canonical_bytes(&bytes))
}

/// Keep every integer parameter in the host-safe range while allowing SQLite
/// INTEGER-affinity columns to retain exact native `i64` values.
fn sql_integer(value: i64) -> SqlValue {
    if (-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(&value) {
        SqlValue::Integer(value)
    } else {
        SqlValue::Text(value.to_string())
    }
}
