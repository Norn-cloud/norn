//! Exact scalar property values at the SQLite storage boundary.
//!
//! `norn-cell` adapts these portable encodings to SQLite values. Keeping the
//! integer, decimal, boolean, text, and canonical JSON conversions here also
//! lets the kernel prepare the exact values a cell will persist.

use norn_ir::{PropertyKind, RuntimeValue, RuntimeValueError};

/// One value encoded for a scalar property column.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncodedValue {
    /// SQL `NULL`.
    Null,
    /// An integer or boolean (`false` is zero, `true` is one).
    Integer(i64),
    /// Text, canonical decimal text, or Norn1 canonical JSON text.
    Text(String),
}

/// Why a property value could not be encoded or decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PropertyCodecError {
    /// A runtime value does not have the kind's scalar encoding.
    ValueType(&'static str),
    /// A stored value does not match the kind's encoding.
    Corrupt,
    /// A runtime value could not be represented as exact canonical JSON.
    Runtime(RuntimeValueError),
}

/// Materialize the persisted representation before checking its constraints.
pub(crate) fn stored_value(
    kind: PropertyKind,
    value: &RuntimeValue,
) -> Result<RuntimeValue, PropertyCodecError> {
    decode(kind, &encode(kind, value)?)
}

/// Encode one runtime value using the cell's scalar property encoding.
///
/// `Null` is retained for every property kind; the cell adapter applies the
/// property's nullability rule.
///
/// # Errors
///
/// Returns [`PropertyCodecError::ValueType`] when the value does not match
/// the kind, or [`PropertyCodecError::Runtime`] when canonical JSON cannot be
/// represented exactly.
pub fn encode(
    kind: PropertyKind,
    value: &RuntimeValue,
) -> Result<EncodedValue, PropertyCodecError> {
    if value.is_null() {
        return Ok(EncodedValue::Null);
    }
    match kind {
        PropertyKind::Text
        | PropertyKind::Enum
        | PropertyKind::Date
        | PropertyKind::Timestamp
        | PropertyKind::Duration => value
            .as_str()
            .map(|text| EncodedValue::Text(text.to_owned()))
            .ok_or(PropertyCodecError::ValueType("a string")),
        PropertyKind::Decimal => {
            let decimal = match value {
                RuntimeValue::Number(number) => Some(*number),
                RuntimeValue::String(text) if is_decimal(text) => text.parse().ok(),
                RuntimeValue::Null
                | RuntimeValue::Bool(_)
                | RuntimeValue::String(_)
                | RuntimeValue::Array(_)
                | RuntimeValue::Object(_) => None,
            };
            decimal
                .map(|decimal| EncodedValue::Text(decimal.canonical()))
                .ok_or(PropertyCodecError::ValueType("an exact decimal"))
        }
        PropertyKind::Integer => integer(value)
            .map(EncodedValue::Integer)
            .ok_or(PropertyCodecError::ValueType("a signed 64-bit integer")),
        PropertyKind::Boolean => value
            .as_bool()
            .map(|flag| EncodedValue::Integer(i64::from(flag)))
            .ok_or(PropertyCodecError::ValueType("a boolean")),
        PropertyKind::Record if !matches!(value, RuntimeValue::Object(_)) => {
            Err(PropertyCodecError::ValueType("a record object"))
        }
        PropertyKind::Record | PropertyKind::JsonExt => {
            let bytes = value
                .canonical_json()
                .map_err(PropertyCodecError::Runtime)?;
            Ok(EncodedValue::Text(
                String::from_utf8_lossy(&bytes).into_owned(),
            ))
        }
        PropertyKind::Ref => Err(PropertyCodecError::ValueType("a resolved property kind")),
    }
}

/// Decode one stored scalar value as an exact runtime value.
///
/// # Errors
///
/// Returns [`PropertyCodecError::Corrupt`] when the stored scalar does not
/// match the kind, or [`PropertyCodecError::Runtime`] for invalid or inexact
/// canonical JSON.
pub fn decode(
    kind: PropertyKind,
    value: &EncodedValue,
) -> Result<RuntimeValue, PropertyCodecError> {
    match (kind, value) {
        (_, EncodedValue::Null) => Ok(RuntimeValue::Null),
        (
            PropertyKind::Text
            | PropertyKind::Enum
            | PropertyKind::Date
            | PropertyKind::Timestamp
            | PropertyKind::Duration,
            EncodedValue::Text(text),
        ) => Ok(RuntimeValue::String(text.clone())),
        (PropertyKind::Decimal, EncodedValue::Text(text)) => text
            .parse::<norn_ir::Decimal>()
            .ok()
            .map(RuntimeValue::Number)
            .ok_or(PropertyCodecError::Corrupt),
        (PropertyKind::Integer, EncodedValue::Integer(number)) => Ok(RuntimeValue::from(*number)),
        (PropertyKind::Integer, EncodedValue::Text(text)) => text
            .parse::<i64>()
            .ok()
            .map(RuntimeValue::from)
            .ok_or(PropertyCodecError::Corrupt),
        (PropertyKind::Boolean, EncodedValue::Integer(0)) => Ok(RuntimeValue::Bool(false)),
        (PropertyKind::Boolean, EncodedValue::Integer(1)) => Ok(RuntimeValue::Bool(true)),
        (PropertyKind::Record, EncodedValue::Text(text)) => {
            let value = RuntimeValue::from_json_text(text).map_err(PropertyCodecError::Runtime)?;
            if matches!(value, RuntimeValue::Object(_)) {
                Ok(value)
            } else {
                Err(PropertyCodecError::Corrupt)
            }
        }
        (PropertyKind::JsonExt, EncodedValue::Text(text)) => {
            RuntimeValue::from_json_text(text).map_err(PropertyCodecError::Runtime)
        }
        _ => Err(PropertyCodecError::Corrupt),
    }
}

/// An integral number or exact integer string in the signed 64-bit range.
fn integer(value: &RuntimeValue) -> Option<i64> {
    match value {
        RuntimeValue::Number(number) => number.to_i64(),
        RuntimeValue::String(text) => text.parse().ok(),
        RuntimeValue::Null
        | RuntimeValue::Bool(_)
        | RuntimeValue::Array(_)
        | RuntimeValue::Object(_) => None,
    }
}

/// The exact decimal grammar: `-?(0|[1-9][0-9]*)(\.[0-9]+)?`.
fn is_decimal(text: &str) -> bool {
    let unsigned = text.strip_prefix('-').unwrap_or(text);
    let (whole, fraction) = match unsigned.split_once('.') {
        Some((whole, fraction)) => (whole, Some(fraction)),
        None => (unsigned, None),
    };
    let digits = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    digits(whole) && (whole == "0" || !whole.starts_with('0')) && fraction.is_none_or(digits)
}

#[cfg(test)]
mod tests {
    use super::is_decimal;

    #[test]
    fn decimal_grammar() {
        for good in ["0", "-12", "19.990", "0.001", "10"] {
            assert!(is_decimal(good), "{good}");
        }
        for bad in ["", "-", "01", "1.", ".5", "1e3", "+1", "1.2.3", "NaN"] {
            assert!(!is_decimal(bad), "{bad}");
        }
    }
}
