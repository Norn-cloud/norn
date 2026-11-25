//! VIEW-ENGINE-1 on `DataFusion`: how each admitted column type is held,
//! how a stored value enters a typed array exactly, and how an answer's
//! value leaves it. Text, dates and timestamps are text; a timestamp also
//! carries its exact instant key (whole nanoseconds since the epoch) for
//! ordering, grouping and equality; numbers are 38-digit decimals at their
//! column's scale. A value that does not fit its column exactly refuses.

use std::sync::Arc;

use datafusion::arrow::array::{
    Array, ArrayRef, BooleanArray, Decimal128Array, StringArray, UInt64Array,
};
use datafusion::arrow::datatypes::{DataType, Field};
use norn_checker::transform::{Column, NodeSchema, Type};
use norn_ir::{Decimal, RuntimeValue};

use crate::CellError;
use crate::compute::{UNSUPPORTED_ENGINE, refused};

/// The most decimal digits a 128-bit decimal holds.
pub(super) const PRECISION: u8 = 38;
/// The scale an unscaled `number` column is held at.
const NUMBER_SCALE: u32 = 18;
/// The widest literal or column scale the lowering takes.
pub(super) const MAX_SCALE: u32 = 18;

/// The row pointer every materialized node carries: its row index.
pub(super) const ROW: &str = "__r";

pub(super) fn unsupported<T>() -> Result<T, CellError> {
    Err(refused(UNSUPPORTED_ENGINE))
}

/// How a column is held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Kind {
    Text,
    Bool,
    Integer,
    Decimal(u32),
    Number,
    Date,
    Timestamp,
}

fn strip(ty: &Type) -> &Type {
    if let Type::Optional(inner) = ty {
        strip(inner)
    } else {
        ty
    }
}

/// The kind of an admitted type; anything else refuses the lowering.
pub(super) fn kind(ty: &Type) -> Result<Kind, CellError> {
    let ty = strip(ty);
    if matches!(ty, Type::Text { .. } | Type::Enum { .. }) {
        Ok(Kind::Text)
    } else if matches!(ty, Type::Bool) {
        Ok(Kind::Bool)
    } else if matches!(ty, Type::Integer) {
        Ok(Kind::Integer)
    } else if matches!(ty, Type::Number) {
        Ok(Kind::Number)
    } else if matches!(ty, Type::Date) {
        Ok(Kind::Date)
    } else if matches!(ty, Type::Timestamp { .. }) {
        Ok(Kind::Timestamp)
    } else if let Type::Decimal { scale, .. } = ty {
        match u32::try_from(*scale) {
            Ok(scale) if scale <= MAX_SCALE => Ok(Kind::Decimal(scale)),
            _ => unsupported(),
        }
    } else {
        unsupported()
    }
}

impl Kind {
    /// The scale a numeric kind is held at.
    const fn scale(self) -> Option<u32> {
        match self {
            Self::Integer => Some(0),
            Self::Decimal(scale) => Some(scale),
            Self::Number => Some(NUMBER_SCALE),
            Self::Text | Self::Bool | Self::Date | Self::Timestamp => None,
        }
    }

    /// The Arrow type a column of this kind is held as.
    pub(super) fn arrow(self) -> DataType {
        match self.scale() {
            Some(scale) => decimal(scale),
            None if self == Self::Bool => DataType::Boolean,
            None => DataType::Utf8,
        }
    }
}

/// A 38-digit decimal type at `scale`.
pub(super) fn decimal(scale: u32) -> DataType {
    DataType::Decimal128(PRECISION, i8::try_from(scale).unwrap_or(i8::MAX))
}

/// The admitted column `name` of `schema`.
pub(super) fn column<'s>(schema: &'s NodeSchema, name: &str) -> Result<&'s Column, CellError> {
    schema
        .column(name)
        .ok_or_else(|| refused(UNSUPPORTED_ENGINE))
}

/// The kind of column `name` of `schema`.
pub(super) fn kind_of(schema: &NodeSchema, name: &str) -> Result<Kind, CellError> {
    kind(&column(schema, name)?.ty)
}

/// The instant key column of timestamp column `name`.
pub(super) fn key_name(name: &str) -> String {
    format!("__k_{name}")
}

/// The name a column is ordered, grouped and compared by: its instant key
/// for a timestamp, itself otherwise.
pub(super) fn ordered(schema: &NodeSchema, name: &str) -> Result<String, CellError> {
    Ok(if kind_of(schema, name)? == Kind::Timestamp {
        key_name(name)
    } else {
        name.to_owned()
    })
}

/// Every column `schema` carries, with the instant key after each timestamp.
pub(super) fn carried(schema: &NodeSchema) -> Result<Vec<String>, CellError> {
    let mut names = Vec::new();
    for column in &schema.columns {
        names.push(column.name.clone());
        if kind(&column.ty)? == Kind::Timestamp {
            names.push(key_name(&column.name));
        }
    }
    Ok(names)
}

/// `text` (`-?digits(.digits)?`) as an integer at `scale`, exactly: more
/// fractional digits than `scale`, or more than 38 digits, is `None`.
pub(super) fn scaled(text: &str, scale: u32) -> Option<i128> {
    let (negative, digits) = text
        .strip_prefix('-')
        .map_or((false, text), |rest| (true, rest));
    let (whole, fraction) = digits.split_once('.').unwrap_or((digits, ""));
    let fraction_len = u32::try_from(fraction.len()).ok()?;
    let valid = |part: &str| part.bytes().all(|b| b.is_ascii_digit());
    if whole.is_empty() || !valid(whole) || !valid(fraction) || fraction_len > scale {
        return None;
    }
    let mut value = 0_i128;
    for byte in whole.bytes().chain(fraction.bytes()) {
        value = value
            .checked_mul(10)?
            .checked_add(i128::from(byte.checked_sub(b'0')?))?;
    }
    let value = value.checked_mul(10_i128.checked_pow(scale.checked_sub(fraction_len)?)?)?;
    if value.unsigned_abs() >= 10_u128.checked_pow(u32::from(PRECISION))? {
        return None;
    }
    Some(if negative {
        value.checked_neg()?
    } else {
        value
    })
}

/// One stored value as text, or `None` for null; refuses a non-scalar.
pub(super) fn bound(value: Option<&RuntimeValue>) -> Result<Option<String>, CellError> {
    Ok(match value {
        None | Some(RuntimeValue::Null) => None,
        Some(RuntimeValue::Bool(flag)) => Some(flag.to_string()),
        Some(RuntimeValue::Number(number)) => Some(number.canonical()),
        Some(RuntimeValue::String(text)) => Some(text.clone()),
        Some(RuntimeValue::Array(_) | RuntimeValue::Object(_)) => return unsupported(),
    })
}

/// A timestamp's instant key, whole nanoseconds since the epoch.
pub(super) fn instant_key(text: &str) -> Result<i128, CellError> {
    let (seconds, nanos) = norn_checker::transform::instant(text)
        .ok_or_else(|| refused("core.compute.stale_input"))?;
    i128::from(seconds)
        .checked_mul(1_000_000_000)
        .and_then(|whole| whole.checked_add(i128::from(nanos)))
        .ok_or_else(|| refused(UNSUPPORTED_ENGINE))
}

/// The typed array of one column of `kind` from its values' texts.
pub(super) fn array(kind: Kind, texts: &[Option<String>]) -> Result<ArrayRef, CellError> {
    Ok(match kind.scale() {
        Some(scale) => {
            let values = texts
                .iter()
                .map(|text| {
                    text.as_deref()
                        .map(|text| scaled(text, scale).ok_or_else(|| refused(UNSUPPORTED_ENGINE)))
                        .transpose()
                })
                .collect::<Result<Vec<_>, _>>()?;
            Arc::new(Decimal128Array::from(values).with_data_type(decimal(scale)))
        }
        None if kind == Kind::Bool => {
            let values = texts
                .iter()
                .map(|text| match text.as_deref() {
                    None => Ok(None),
                    Some("true") => Ok(Some(true)),
                    Some("false") => Ok(Some(false)),
                    Some(other) => Err(CellError::Engine(format!(
                        "invalid boolean value {other:?}"
                    ))),
                })
                .collect::<Result<Vec<_>, _>>()?;
            Arc::new(BooleanArray::from(values))
        }
        None => Arc::new(StringArray::from(texts.to_vec())),
    })
}

/// The instant keys of a timestamp column's texts.
pub(super) fn keys(texts: &[Option<String>]) -> Result<ArrayRef, CellError> {
    let keys = texts
        .iter()
        .map(|text| text.as_deref().map(instant_key).transpose())
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Arc::new(
        Decimal128Array::from(keys).with_data_type(decimal(0)),
    ))
}

/// The row pointer column of `rows` rows.
pub(super) fn pointers(rows: usize) -> ArrayRef {
    Arc::new(UInt64Array::from_iter_values(
        (0..rows).map(|row| u64::try_from(row).unwrap_or(u64::MAX)),
    ))
}

/// The field of a column of `kind`.
pub(super) fn field(name: &str, kind: Kind) -> Field {
    Field::new(name, kind.arrow(), true)
}

/// One answer value of `kind` from its text, or null.
pub(super) fn value(text: Option<&str>, kind: Kind) -> Result<RuntimeValue, CellError> {
    let Some(text) = text else {
        return Ok(RuntimeValue::Null);
    };
    Ok(match kind {
        Kind::Text | Kind::Date | Kind::Timestamp => RuntimeValue::String(text.to_owned()),
        Kind::Bool => RuntimeValue::Bool(text == "true"),
        Kind::Integer | Kind::Decimal(_) | Kind::Number => {
            RuntimeValue::Number(Decimal::parse(text).map_err(CellError::Value)?)
        }
    })
}

/// Every value of an answer column as text, through Arrow's own cast.
pub(super) fn texts(array: &dyn Array) -> Result<Vec<Option<String>>, CellError> {
    let cast = datafusion::arrow::compute::cast(array, &DataType::Utf8)
        .map_err(|error| CellError::Engine(error.to_string()))?;
    let strings = cast
        .as_any()
        .downcast_ref::<StringArray>()
        .ok_or_else(|| CellError::Engine("an answer column is not text".to_owned()))?;
    Ok((0..strings.len())
        .map(|row| (!strings.is_null(row)).then(|| strings.value(row).to_owned()))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::scaled;

    #[test]
    fn a_value_is_scaled_exactly_or_not_at_all() {
        assert_eq!(scaled("12.5", 2), Some(1250));
        assert_eq!(scaled("-0.05", 2), Some(-5));
        assert_eq!(scaled("7", 0), Some(7));
        assert_eq!(scaled("1.005", 2), None, "a digit would be lost");
        assert_eq!(scaled("1e5", 2), None);
        assert_eq!(scaled("", 0), None);
        assert_eq!(scaled(".5", 1), None);
        assert!(scaled(&"9".repeat(38), 0).is_some());
        assert_eq!(scaled(&"9".repeat(39), 0), None);
        assert_eq!(scaled(&"9".repeat(21), 18), None, "39 digits at scale 18");
    }
}
