//! BULK-LOAD-1 on the adapter: a source file read once, in its physical
//! order, into each declared column's text, every row numbered from 1 before
//! anything is filtered; then typed exactly as the `DuckDB` engine types it.
//! A CSV is read strictly (RFC 4180 quoting, equal field counts) and a blank
//! field is null; a Parquet value is null only when the file says so, and
//! present text, empty or blank, is kept. An instant is imported with every
//! nanosecond and its instant or not at all (LAW-03): a nanosecond field the
//! reader would narrow (`INT96`) refuses the load.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use datafusion::arrow::array::{
    Array, BooleanArray, Date32Array, Decimal128Array, Float32Array, Float64Array, Int8Array,
    Int16Array, Int32Array, Int64Array, TimestampMicrosecondArray, TimestampMillisecondArray,
    TimestampNanosecondArray, TimestampSecondArray, UInt8Array, UInt16Array, UInt32Array,
    UInt64Array,
};
use datafusion::arrow::datatypes::{DataType, TimeUnit};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::basic::{LogicalType, Type as PhysicalType};

use super::super::{Column, ColumnKind, FileFormat, LoadRequest, PointLaw};
use super::columnar::text_at;
use super::values::{
    Cell, Dec, DecError, format_day, format_second, in_bulk_domain, parse_date, parse_timestamp,
};
use super::{Outcome, engine_error, refusal};

/// Rows decoded per Parquet batch.
const BATCH_ROWS: usize = 8192;
/// The most row groups a source file may declare.
const MAX_ROW_GROUPS: usize = 100_000;
/// The most top-level fields a source file may declare.
const MAX_FIELDS: usize = 4096;
/// A number the per-row decoder cannot hold exactly (S4).
const OUT_OF_RANGE: &str = "bulk.out_of_range";
const LAW: &str = "bulk.unsupported_law";

/// One source row's text per declared column, with its 1-based physical row.
pub(super) struct SourceRow {
    pub row: u64,
    pub texts: Vec<Option<String>>,
}

fn unreadable(error: impl std::fmt::Display) -> super::super::EngineRefusal {
    engine_error(error.to_string())
}

fn missing(name: &str) -> super::super::EngineRefusal {
    refusal(
        "bulk.missing_column",
        format!("the file has no column {name}"),
    )
}

/// The file field each column reads, by header or Parquet field name.
fn field_name<'r>(request: &'r LoadRequest, column: &'r Column) -> &'r str {
    request.headers.get(&column.id).unwrap_or(&column.id)
}

/// Every row of the source, each declared column's text.
pub(super) fn read_source(request: &LoadRequest, file: &Path) -> Outcome<Vec<SourceRow>> {
    match request.file.format {
        FileFormat::Csv => read_csv(request, file),
        FileFormat::Parquet => read_parquet(request, file),
    }
}

fn read_csv(request: &LoadRequest, file: &Path) -> Outcome<Vec<SourceRow>> {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(false)
        .quoting(true)
        .double_quote(true)
        .delimiter(b',')
        .quote(b'"')
        .from_path(file)
        .map_err(unreadable)?;
    let headers = reader.headers().map_err(unreadable)?.clone();
    let mut positions = Vec::new();
    for column in &request.table.columns {
        let name = field_name(request, column);
        positions.push(
            headers
                .iter()
                .position(|header| header == name)
                .ok_or_else(|| missing(name))?,
        );
        check_laws(column)?;
    }
    let mut rows = Vec::new();
    for (number, record) in reader.records().enumerate() {
        let record = record.map_err(unreadable)?;
        let texts = positions
            .iter()
            .map(|at| {
                record
                    .get(*at)
                    .filter(|text| !text.trim_matches(' ').is_empty())
                    .map(str::to_owned)
            })
            .collect();
        rows.push(SourceRow {
            row: u64::try_from(number).unwrap_or(u64::MAX).saturating_add(1),
            texts,
        });
    }
    Ok(rows)
}

/// How a Parquet field's values become text.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Encoding {
    /// Its plain text cast.
    Plain,
    /// A `NANOS` instant, every nanosecond kept.
    Nanos,
    /// A microsecond (or coarser) instant.
    Micros,
}

fn read_parquet(request: &LoadRequest, file: &Path) -> Outcome<Vec<SourceRow>> {
    let source =
        std::fs::File::open(file).map_err(|e| refusal("bulk.unreadable", e.to_string()))?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(source).map_err(unreadable)?;
    let metadata = builder.metadata();
    if metadata.num_row_groups() > MAX_ROW_GROUPS || builder.schema().fields().len() > MAX_FIELDS {
        return Err(refusal(
            "bulk.unsupported_encoding",
            "the file declares too much structure",
        ));
    }
    // LAW-03: the fields holding nanosecond instants, and the legacy ones a
    // reader narrows.
    let mut nanos = BTreeSet::new();
    let mut int96 = BTreeSet::new();
    for descriptor in builder.parquet_schema().columns() {
        let name = descriptor
            .path()
            .parts()
            .first()
            .cloned()
            .unwrap_or_default();
        if descriptor.physical_type() == PhysicalType::INT96 {
            int96.insert(name.clone());
        }
        if let Some(LogicalType::Timestamp(stamp)) = descriptor.logical_type_ref()
            && matches!(stamp.unit, parquet::basic::TimeUnit::NANOS)
        {
            nanos.insert(name);
        }
    }
    let schema = std::sync::Arc::clone(builder.schema());
    let mut plan = Vec::new();
    for column in &request.table.columns {
        let name = field_name(request, column);
        let (index, field) = schema
            .fields()
            .iter()
            .enumerate()
            .find(|(_, field)| field.name() == name)
            .ok_or_else(|| missing(name))?;
        let lossy = || {
            refusal(
                "bulk.unsupported_encoding",
                format!("{name} holds instants this load cannot import exactly"),
            )
        };
        let encoding = match (column.kind, field.data_type()) {
            (ColumnKind::Timestamp, _) if int96.contains(name) => return Err(lossy()),
            (ColumnKind::Timestamp, DataType::Timestamp(TimeUnit::Nanosecond, _)) => {
                if nanos.contains(name) {
                    Encoding::Nanos
                } else {
                    return Err(lossy());
                }
            }
            (ColumnKind::Timestamp, DataType::Timestamp(_, _)) => Encoding::Micros,
            (ColumnKind::Timestamp, kind) if is_temporal(kind) => return Err(lossy()),
            _ => Encoding::Plain,
        };
        check_laws(column)?;
        plan.push((index, encoding, column.kind));
    }
    let reader = builder
        .with_batch_size(BATCH_ROWS)
        .build()
        .map_err(unreadable)?;
    let mut rows = Vec::new();
    let mut number = 0_u64;
    for batch in reader {
        let batch = batch.map_err(unreadable)?;
        let columns = plan
            .iter()
            .map(|(index, encoding, kind)| {
                let array = batch.column(*index);
                (0..batch.num_rows())
                    .map(|row| parquet_text(array.as_ref(), row, *encoding, *kind))
                    .collect::<Outcome<Vec<_>>>()
            })
            .collect::<Outcome<Vec<_>>>()?;
        for row in 0..batch.num_rows() {
            number = number.saturating_add(1);
            rows.push(SourceRow {
                row: number,
                texts: columns
                    .iter()
                    .map(|texts| texts.get(row).cloned().flatten())
                    .collect(),
            });
        }
    }
    Ok(rows)
}

const fn is_temporal(kind: &DataType) -> bool {
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
fn parquet_text(
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
            .map(|a| float_text(f64::from(a.value(row)))),
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
        DataType::Timestamp(..) if kind != ColumnKind::Timestamp => {
            instant_of(array, row).map(|(secs, nanos)| {
                let micros = nanos.div_euclid(1000);
                let base = format_second(secs).replacen('T', " ", 1);
                if micros == 0 {
                    base
                } else {
                    format!("{base}.{}", format!("{micros:06}").trim_end_matches('0'))
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

/// A double as `DuckDB` casts it to text: its shortest round-trip digits.
fn float_text(value: f64) -> String {
    if value.is_finite() && value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{value:.1}")
    } else {
        value.to_string()
    }
}

/// A column's declared laws are all checkable (refused before any row).
fn check_laws(column: &Column) -> Outcome<()> {
    for law in &column.laws {
        match law {
            PointLaw::Min(bound) | PointLaw::Max(bound) => {
                let canonical = norn_ir::Decimal::parse(bound)
                    .ok()
                    .filter(|d| d.canonical() == *bound);
                if canonical.is_none() {
                    return Err(refusal(LAW, "a bound that is not a canonical decimal"));
                }
            }
            PointLaw::Unchecked => {
                return Err(refusal(
                    LAW,
                    format!(
                        "{} declares a constraint the engine does not check",
                        column.id
                    ),
                ));
            }
            PointLaw::OneOf(_) | PointLaw::MinLength(_) | PointLaw::MaxLength(_) => {}
        }
    }
    Ok(())
}

/// One column's text typed: its cell, or the reason it is refused.
pub(super) fn type_text(column: &Column, text: Option<&str>) -> Result<Cell, String> {
    let Some(text) = text else {
        return if column.nullable {
            Ok(Cell::Null)
        } else {
            Err("bulk.required".to_owned())
        };
    };
    let cell = parse(column.kind, text).map_err(str::to_owned)?;
    match first_law(column, &cell) {
        Some(reason) => Err(reason.to_owned()),
        None => Ok(cell),
    }
}

fn parse(kind: ColumnKind, text: &str) -> Result<Cell, &'static str> {
    match kind {
        ColumnKind::Text | ColumnKind::Opaque => Ok(Cell::Text(text.to_owned())),
        ColumnKind::Integer => {
            if !text
                .bytes()
                .all(|b| b.is_ascii_digit() || b == b'-' || b == b'+')
                || text.bytes().filter(|b| !b.is_ascii_digit()).count() > 1
            {
                return Err("bulk.not_integer");
            }
            let value = Dec::parse_at(text, 0).map_err(|_e| "bulk.not_integer")?;
            if i64::try_from(value.m).is_err() {
                return Err(OUT_OF_RANGE);
            }
            Ok(Cell::Dec(value))
        }
        ColumnKind::Decimal { scale } => {
            let others: String = text.chars().filter(|c| !c.is_ascii_digit()).collect();
            let shaped = ["", ".", "-", "+", "-.", "+."].contains(&others.as_str())
                && !text.trim_start_matches(['+', '-']).starts_with('.')
                && !text.ends_with('.');
            if !shaped {
                return Err("bulk.not_decimal");
            }
            let value = match Dec::parse_at(text, scale) {
                Ok(value) => value,
                Err(DecError::Scale) => return Err("bulk.scale_exceeded"),
                Err(DecError::Shape | DecError::Range) => return Err("bulk.not_decimal"),
            };
            if in_bulk_domain(value) {
                Ok(Cell::Dec(value))
            } else {
                Err(OUT_OF_RANGE)
            }
        }
        ColumnKind::Boolean => match text.to_lowercase().as_str() {
            "true" => Ok(Cell::Bool(true)),
            "false" => Ok(Cell::Bool(false)),
            _ => Err("bulk.not_boolean"),
        },
        ColumnKind::Date => parse_date(text)
            .map(|t| Cell::Instant {
                text: text.to_owned(),
                t,
                n: 0,
            })
            .ok_or("bulk.not_date"),
        ColumnKind::Timestamp => parse_timestamp(text)
            .map(|(t, n)| Cell::Instant {
                text: text.to_owned(),
                t,
                n,
            })
            .ok_or("bulk.not_timestamp"),
    }
}

/// The first declared pointwise law a typed value breaks.
fn first_law(column: &Column, cell: &Cell) -> Option<&'static str> {
    let text = cell.text()?;
    column.laws.iter().find_map(|law| match law {
        PointLaw::OneOf(values) => {
            let found = match cell {
                Cell::Dec(value) => values.iter().any(|v| {
                    norn_ir::Decimal::parse(v).is_ok_and(|d| d.canonical() == value.canonical())
                }),
                Cell::Null | Cell::Text(_) | Cell::Bool(_) | Cell::Instant { .. } => {
                    values.contains(&text)
                }
            };
            (!found).then_some("bulk.not_in_enum")
        }
        PointLaw::Min(bound) | PointLaw::Max(bound) => {
            let bound = Dec::parse_at(bound, decimal_scale(bound)).ok()?;
            let value = match cell {
                Cell::Dec(value) => *value,
                Cell::Null | Cell::Text(_) | Cell::Bool(_) | Cell::Instant { .. } => {
                    Dec::parse_at(&text, decimal_scale(&text)).ok()?
                }
            };
            let order = value.compare(bound);
            match law {
                PointLaw::Min(_) => order.is_lt().then_some("bulk.below_min"),
                PointLaw::Max(_)
                | PointLaw::OneOf(_)
                | PointLaw::MinLength(_)
                | PointLaw::MaxLength(_)
                | PointLaw::Unchecked => order.is_gt().then_some("bulk.above_max"),
            }
        }
        PointLaw::MinLength(n) => (u64::try_from(text.chars().count()).unwrap_or(u64::MAX) < *n)
            .then_some("bulk.too_short"),
        PointLaw::MaxLength(n) => (u64::try_from(text.chars().count()).unwrap_or(u64::MAX) > *n)
            .then_some("bulk.too_long"),
        PointLaw::Unchecked => None,
    })
}

fn decimal_scale(text: &str) -> u32 {
    u32::try_from(text.split_once('.').map_or(0, |(_, f)| f.len())).unwrap_or(0)
}

/// Every refusal reason and how many rows it holds, as a load answers it.
pub(super) fn count_reasons<'r>(reasons: impl Iterator<Item = &'r str>) -> BTreeMap<String, u64> {
    let mut counts = BTreeMap::new();
    for reason in reasons {
        let count = counts.entry(reason.to_owned()).or_insert(0_u64);
        *count = count.saturating_add(1);
    }
    counts
}
