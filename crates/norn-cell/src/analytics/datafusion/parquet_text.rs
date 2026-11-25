//! The text `DuckDB` reads a Parquet value as, so both engines type the same
//! text: integers and decimals exactly, floats in `DuckDB`'s shortest
//! round-trip form, dates and instants in its formats (a zoned one with its
//! `+00`), and every other type refused.

use datafusion::arrow::array::{
    Array, BooleanArray, Date32Array, Decimal128Array, Float32Array, Float64Array, Int8Array,
    Int16Array, Int32Array, Int64Array, TimestampMicrosecondArray, TimestampMillisecondArray,
    TimestampNanosecondArray, TimestampSecondArray, UInt8Array, UInt16Array, UInt32Array,
    UInt64Array,
};
use datafusion::arrow::datatypes::DataType;

use super::super::ColumnKind;
use super::columnar::text_at;
use super::values::{Dec, format_day, format_second};
use super::{Outcome, engine_error, refusal};

/// How a Parquet field's values become text.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Encoding {
    /// Its plain text cast.
    Plain,
    /// A `NANOS` instant, every nanosecond kept.
    Nanos,
    /// A microsecond (or coarser) instant.
    Micros,
}

pub(super) const fn is_temporal(kind: &DataType) -> bool {
    matches!(
        kind,
        DataType::Timestamp(..) | DataType::Time32(_) | DataType::Time64(_) | DataType::Duration(_)
    )
}

/// UTC second and nanosecond of a timestamp array's value.
fn instant_of(array: &dyn Array, row: usize) -> Option<(i64, i64)> {
    let any = array.as_any();
    let split = |value: i64, per: i64| (value.div_euclid(per), value.rem_euclid(per));
    if let Some(values) = any.downcast_ref::<TimestampNanosecondArray>() {
        return Some(split(values.value(row), 1_000_000_000));
    }
    if let Some(values) = any.downcast_ref::<TimestampMicrosecondArray>() {
        let (s, us) = split(values.value(row), 1_000_000);
        return Some((s, us.saturating_mul(1000)));
    }
    if let Some(values) = any.downcast_ref::<TimestampMillisecondArray>() {
        let (s, ms) = split(values.value(row), 1000);
        return Some((s, ms.saturating_mul(1_000_000)));
    }
    any.downcast_ref::<TimestampSecondArray>()
        .map(|values| (values.value(row), 0))
}

/// The text `DuckDB` reads a Parquet value as.
pub(super) fn parquet_text(
    array: &dyn Array,
    row: usize,
    encoding: Encoding,
    kind: ColumnKind,
) -> Outcome<Option<String>> {
    if array.is_null(row) {
        return Ok(None);
    }
    if encoding != Encoding::Plain {
        let (secs, nanos) = instant_of(array, row).ok_or_else(|| engine_error("an instant"))?;
        let fraction = if encoding == Encoding::Nanos {
            format!("{nanos:09}")
        } else {
            format!("{:06}", nanos.div_euclid(1000))
        };
        return Ok(Some(format!("{}.{fraction}Z", format_second(secs))));
    }
    plain_text(array, row, kind)
}

/// NB4 (#1765): whether a field of Arrow type `kind` has a plain text cast
/// this load reads, decided from the file's schema before anything is
/// decoded; [`plain_text`] refuses the same types if one is ever reached.
pub(super) const fn plain_readable(kind: &DataType) -> bool {
    match kind {
        DataType::Utf8
        | DataType::LargeUtf8
        | DataType::Utf8View
        | DataType::Boolean
        | DataType::Int8
        | DataType::Int16
        | DataType::Int32
        | DataType::Int64
        | DataType::UInt8
        | DataType::UInt16
        | DataType::UInt32
        | DataType::UInt64
        | DataType::Float32
        | DataType::Float64
        | DataType::Decimal128(..)
        | DataType::Date32
        | DataType::Timestamp(..) => true,
        DataType::Null
        | DataType::Float16
        | DataType::Date64
        | DataType::Time32(_)
        | DataType::Time64(_)
        | DataType::Duration(_)
        | DataType::Interval(_)
        | DataType::Binary
        | DataType::FixedSizeBinary(_)
        | DataType::LargeBinary
        | DataType::BinaryView
        | DataType::List(_)
        | DataType::ListView(_)
        | DataType::FixedSizeList(..)
        | DataType::LargeList(_)
        | DataType::LargeListView(_)
        | DataType::Struct(_)
        | DataType::Union(..)
        | DataType::Dictionary(..)
        | DataType::Decimal32(..)
        | DataType::Decimal64(..)
        | DataType::Decimal256(..)
        | DataType::Map(..)
        | DataType::RunEndEncoded(..) => false,
    }
}

/// A non-instant Parquet value as its text cast.
fn plain_text(array: &dyn Array, row: usize, kind: ColumnKind) -> Outcome<Option<String>> {
    let any = array.as_any();
    let text = match array.data_type() {
        DataType::Utf8 | DataType::LargeUtf8 | DataType::Utf8View => text_at(array, row),
        DataType::Boolean => any
            .downcast_ref::<BooleanArray>()
            .map(|a| a.value(row).to_string()),
        DataType::Int8 => any
            .downcast_ref::<Int8Array>()
            .map(|a| a.value(row).to_string()),
        DataType::Int16 => any
            .downcast_ref::<Int16Array>()
            .map(|a| a.value(row).to_string()),
        DataType::Int32 => any
            .downcast_ref::<Int32Array>()
            .map(|a| a.value(row).to_string()),
        DataType::Int64 => any
            .downcast_ref::<Int64Array>()
            .map(|a| a.value(row).to_string()),
        DataType::UInt8 => any
            .downcast_ref::<UInt8Array>()
            .map(|a| a.value(row).to_string()),
        DataType::UInt16 => any
            .downcast_ref::<UInt16Array>()
            .map(|a| a.value(row).to_string()),
        DataType::UInt32 => any
            .downcast_ref::<UInt32Array>()
            .map(|a| a.value(row).to_string()),
        DataType::UInt64 => any
            .downcast_ref::<UInt64Array>()
            .map(|a| a.value(row).to_string()),
        DataType::Float32 => any
            .downcast_ref::<Float32Array>()
            .map(|a| float_text(a.value(row))),
        DataType::Float64 => any
            .downcast_ref::<Float64Array>()
            .map(|a| float_text(a.value(row))),
        DataType::Decimal128(_, scale) => any.downcast_ref::<Decimal128Array>().map(|a| {
            Dec {
                m: a.value(row),
                s: u32::try_from(*scale).unwrap_or(0),
            }
            .text()
        }),
        DataType::Date32 => any
            .downcast_ref::<Date32Array>()
            .map(|a| format_day(i64::from(a.value(row)))),
        DataType::Timestamp(_, zone) if kind != ColumnKind::Timestamp => {
            // N6.2: a zoned value keeps the UTC offset `DuckDB` prints.
            let offset = if zone.is_some() { "+00" } else { "" };
            instant_of(array, row).map(|(secs, nanos)| {
                let micros = nanos.div_euclid(1000);
                let base = format_second(secs).replacen('T', " ", 1);
                if micros == 0 {
                    format!("{base}{offset}")
                } else {
                    let fraction = format!("{micros:06}");
                    format!("{base}.{}{offset}", fraction.trim_end_matches('0'))
                }
            })
        }
        DataType::Null
        | DataType::Float16
        | DataType::Timestamp(..)
        | DataType::Date64
        | DataType::Time32(_)
        | DataType::Time64(_)
        | DataType::Duration(_)
        | DataType::Interval(_)
        | DataType::Binary
        | DataType::FixedSizeBinary(_)
        | DataType::LargeBinary
        | DataType::BinaryView
        | DataType::List(_)
        | DataType::ListView(_)
        | DataType::FixedSizeList(..)
        | DataType::LargeList(_)
        | DataType::LargeListView(_)
        | DataType::Struct(_)
        | DataType::Union(..)
        | DataType::Dictionary(..)
        | DataType::Decimal32(..)
        | DataType::Decimal64(..)
        | DataType::Decimal256(..)
        | DataType::Map(..)
        | DataType::RunEndEncoded(..) => {
            return Err(refusal(
                "bulk.unsupported_encoding",
                "a field of a type this load does not read",
            ));
        }
    };
    text.map(Some)
        .ok_or_else(|| engine_error("an unreadable value"))
}

/// A float in the form `DuckDB` casts it to text (B4): its shortest
/// round-trip digits at its own width (a `FLOAT` is never widened), fixed
/// notation for magnitudes in `[1e-4, 1e16)` with `.0` on integral values,
/// exponent notation `d.ddde±XX` outside it, and `nan`, `inf`, `-inf`.
pub(super) fn float_text<F: Float>(value: F) -> String {
    if value.nan() {
        return "nan".to_owned();
    }
    if let Some(negative) = value.infinite() {
        return if negative { "-inf" } else { "inf" }.to_owned();
    }
    let exponent = value.exponent_text();
    let (mantissa, power) = exponent.split_once('e').unwrap_or((&exponent, "0"));
    let power: i32 = power.parse().unwrap_or(0);
    if value.zero() || (-4..16).contains(&power) {
        let fixed = value.fixed_text();
        return if fixed.contains('.') {
            fixed
        } else {
            format!("{fixed}.0")
        };
    }
    let sign = if power < 0 { '-' } else { '+' };
    format!("{mantissa}e{sign}{:02}", power.unsigned_abs())
}

/// The float widths a Parquet file holds.
pub(super) trait Float: Copy {
    fn nan(self) -> bool;
    fn infinite(self) -> Option<bool>;
    fn zero(self) -> bool;
    fn exponent_text(self) -> String;
    fn fixed_text(self) -> String;
}

impl Float for f32 {
    fn nan(self) -> bool {
        self.is_nan()
    }
    fn infinite(self) -> Option<bool> {
        self.is_infinite().then(|| self.is_sign_negative())
    }
    fn zero(self) -> bool {
        self == 0.0
    }
    fn exponent_text(self) -> String {
        format!("{self:e}")
    }
    fn fixed_text(self) -> String {
        self.to_string()
    }
}

impl Float for f64 {
    fn nan(self) -> bool {
        self.is_nan()
    }
    fn infinite(self) -> Option<bool> {
        self.is_infinite().then(|| self.is_sign_negative())
    }
    fn zero(self) -> bool {
        self == 0.0
    }
    fn exponent_text(self) -> String {
        format!("{self:e}")
    }
    fn fixed_text(self) -> String {
        self.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::float_text;

    #[test]
    fn floats_print_as_duckdb_prints_them() {
        assert_eq!(float_text(0.1_f32), "0.1");
        assert_eq!(float_text(0.1_f64), "0.1");
        assert_eq!(float_text(1e-5_f64), "1e-05");
        assert_eq!(float_text(1e16_f64), "1e+16");
        assert_eq!(float_text(1e15_f64), "1000000000000000.0");
        assert_eq!(float_text(0.0_f64), "0.0");
        assert_eq!(float_text(-0.0_f64), "-0.0");
        assert_eq!(float_text(f64::NAN), "nan");
        assert_eq!(float_text(f64::INFINITY), "inf");
        assert_eq!(float_text(f64::NEG_INFINITY), "-inf");
        assert_eq!(float_text(1.5e-7_f32), "1.5e-07");
    }
}
