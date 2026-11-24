//! D137: Arrow inside the adapter alone. Cells become Arrow columns only to
//! be written as Parquet and are read back into cells as soon as a scan
//! answers, so no Arrow type (59.x here, 60.x under `DuckDB`) crosses the
//! adapter. The column layout is the one the `DuckDB` engine writes (each
//! value under its property id, an instant's key as `<id>:t` and `<id>:n`),
//! so either engine reads the other's data and held-set files.

use std::fs::File;
use std::path::Path;
use std::sync::Arc;

use datafusion::arrow::array::{
    Array, ArrayRef, BooleanArray, Decimal128Array, Int32Array, Int64Array, LargeStringArray,
    StringArray, StringViewArray, UInt64Array,
};
use datafusion::arrow::datatypes::{DataType, Field, Schema, SchemaRef};
use datafusion::arrow::record_batch::RecordBatch;
use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;

use super::super::{Column, ColumnKind, EngineRefusal, TableSchema};
use super::values::{Cell, Dec, STORAGE_DIGITS};
use super::{Outcome, engine_error};

/// Rows per written row group, as the `DuckDB` engine writes them.
const ROW_GROUP_ROWS: usize = 122_880;

fn derived(column: &str, suffix: &str) -> String {
    format!("{column}:{suffix}")
}

/// The storage fields of one column: its value and, for an instant, its key.
pub(super) fn column_fields(column: &Column) -> Vec<Field> {
    let value = match column.kind {
        ColumnKind::Integer => DataType::Decimal128(38, 0),
        ColumnKind::Decimal { scale } => {
            DataType::Decimal128(38, i8::try_from(scale).unwrap_or(i8::MAX))
        }
        ColumnKind::Boolean => DataType::Boolean,
        ColumnKind::Text | ColumnKind::Timestamp | ColumnKind::Date | ColumnKind::Opaque => {
            DataType::Utf8
        }
    };
    let mut fields = vec![Field::new(&column.id, value, true)];
    if matches!(column.kind, ColumnKind::Timestamp | ColumnKind::Date) {
        fields.push(Field::new(derived(&column.id, "t"), DataType::Int64, true));
        fields.push(Field::new(derived(&column.id, "n"), DataType::Int32, true));
    }
    fields
}

/// A schema of `system` fields followed by every column's storage fields.
pub(super) fn storage_schema(system: Vec<Field>, table: &TableSchema) -> SchemaRef {
    let mut fields = system;
    for column in &table.columns {
        fields.extend(column_fields(column));
    }
    Arc::new(Schema::new(fields))
}

/// A schema of `system` fields followed by every column as text.
pub(super) fn text_schema(system: Vec<Field>, table: &TableSchema) -> SchemaRef {
    let mut fields = system;
    fields.extend(
        table
            .columns
            .iter()
            .map(|column| Field::new(&column.id, DataType::Utf8, true)),
    );
    Arc::new(Schema::new(fields))
}

/// The storage arrays of one column's cells.
pub(super) fn column_arrays(column: &Column, cells: &[&Cell]) -> Outcome<Vec<ArrayRef>> {
    let mut arrays: Vec<ArrayRef> = Vec::new();
    match column.kind {
        ColumnKind::Integer | ColumnKind::Decimal { .. } => {
            let scale = match column.kind {
                ColumnKind::Decimal { scale } => scale,
                ColumnKind::Integer
                | ColumnKind::Text
                | ColumnKind::Boolean
                | ColumnKind::Timestamp
                | ColumnKind::Date
                | ColumnKind::Opaque => 0,
            };
            let values = cells
                .iter()
                .map(|cell| match cell {
                    Cell::Dec(value) => value.rescale(scale).map(|d| Some(d.m)).ok_or_else(|| {
                        engine_error(format!("{} holds a number off its scale", column.id))
                    }),
                    Cell::Null => Ok(None),
                    Cell::Text(_) | Cell::Bool(_) | Cell::Instant { .. } => Err(engine_error(
                        format!("{} holds a value of another kind", column.id),
                    )),
                })
                .collect::<Outcome<Vec<_>>>()?;
            let array = Decimal128Array::from(values)
                .with_precision_and_scale(38, i8::try_from(scale).unwrap_or(i8::MAX))
                .map_err(|error| engine_error(error.to_string()))?;
            arrays.push(Arc::new(array));
        }
        ColumnKind::Boolean => {
            let values: Vec<Option<bool>> = cells
                .iter()
                .map(|cell| match cell {
                    Cell::Bool(flag) => Some(*flag),
                    Cell::Null | Cell::Text(_) | Cell::Dec(_) | Cell::Instant { .. } => None,
                })
                .collect();
            arrays.push(Arc::new(BooleanArray::from(values)));
        }
        ColumnKind::Text | ColumnKind::Opaque | ColumnKind::Timestamp | ColumnKind::Date => {
            let texts: Vec<Option<String>> = cells.iter().map(|cell| cell.text()).collect();
            arrays.push(Arc::new(StringArray::from(texts)));
            if matches!(column.kind, ColumnKind::Timestamp | ColumnKind::Date) {
                let keys: Vec<Option<(i64, i32)>> = cells.iter().map(|c| c.instant()).collect();
                arrays.push(Arc::new(Int64Array::from(
                    keys.iter().map(|k| k.map(|(t, _)| t)).collect::<Vec<_>>(),
                )));
                arrays.push(Arc::new(Int32Array::from(
                    keys.iter().map(|k| k.map(|(_, n)| n)).collect::<Vec<_>>(),
                )));
            }
        }
    }
    Ok(arrays)
}

/// One batch of `schema` from its arrays.
pub(super) fn batch(schema: &SchemaRef, arrays: Vec<ArrayRef>) -> Outcome<RecordBatch> {
    RecordBatch::try_new(Arc::clone(schema), arrays).map_err(|e| engine_error(e.to_string()))
}

/// Write `batches` as one zstd Parquet file at `path`, created exclusively,
/// its footer finished and its bytes on disk before this returns.
pub(super) fn write_parquet(
    path: &Path,
    schema: &SchemaRef,
    batches: &[RecordBatch],
) -> Outcome<()> {
    let file = super::catalog::create_new(path)?;
    let properties = WriterProperties::builder()
        .set_compression(Compression::ZSTD(ZstdLevel::default()))
        .set_max_row_group_row_count(Some(ROW_GROUP_ROWS))
        .build();
    let written = (|| -> Result<File, parquet::errors::ParquetError> {
        let mut writer = ArrowWriter::try_new(file, Arc::clone(schema), Some(properties))?;
        for batch in batches {
            writer.write(batch)?;
        }
        writer.into_inner()
    })();
    let file = written.map_err(|error| engine_error(error.to_string()))?;
    file.sync_all()
        .map_err(|error| engine_error(error.to_string()))
}

/// A column of a scanned batch by name.
fn named<'b>(batch: &'b RecordBatch, name: &str) -> Outcome<&'b ArrayRef> {
    batch
        .column_by_name(name)
        .ok_or_else(|| engine_error(format!("a stored file has no column {name}")))
}

/// Text at `row` of a string column of any Arrow string encoding.
pub(super) fn text_at(array: &dyn Array, row: usize) -> Option<String> {
    if array.is_null(row) {
        return None;
    }
    let any = array.as_any();
    if let Some(strings) = any.downcast_ref::<StringArray>() {
        return Some(strings.value(row).to_owned());
    }
    if let Some(strings) = any.downcast_ref::<StringViewArray>() {
        return Some(strings.value(row).to_owned());
    }
    any.downcast_ref::<LargeStringArray>()
        .map(|strings| strings.value(row).to_owned())
}

/// An unsigned count at `row` (`UBIGINT`, or a signed 64-bit one).
pub(super) fn u64_at(array: &dyn Array, row: usize) -> Option<u64> {
    if array.is_null(row) {
        return None;
    }
    let any = array.as_any();
    if let Some(values) = any.downcast_ref::<UInt64Array>() {
        return Some(values.value(row));
    }
    any.downcast_ref::<Int64Array>()
        .and_then(|values| u64::try_from(values.value(row)).ok())
}

/// A Boolean at `row`.
pub(super) fn bool_at(array: &dyn Array, row: usize) -> Option<bool> {
    if array.is_null(row) {
        return None;
    }
    array
        .as_any()
        .downcast_ref::<BooleanArray>()
        .map(|values| values.value(row))
}

fn i64_at(array: &dyn Array, row: usize) -> Option<i64> {
    if array.is_null(row) {
        return None;
    }
    array
        .as_any()
        .downcast_ref::<Int64Array>()
        .map(|values| values.value(row))
}

fn i32_at(array: &dyn Array, row: usize) -> Option<i32> {
    if array.is_null(row) {
        return None;
    }
    array
        .as_any()
        .downcast_ref::<Int32Array>()
        .map(|values| values.value(row))
}

/// The system column `name` of a batch, as `read` decodes each row.
pub(super) fn system<T>(
    batch: &RecordBatch,
    name: &str,
    read: impl Fn(&dyn Array, usize) -> Option<T>,
) -> Outcome<Vec<Option<T>>> {
    let array = named(batch, name)?;
    Ok((0..batch.num_rows())
        .map(|row| read(array.as_ref(), row))
        .collect())
}

/// The cells of one stored column of a batch.
pub(super) fn column_cells(batch: &RecordBatch, column: &Column) -> Outcome<Vec<Cell>> {
    let array = named(batch, &column.id)?;
    let rows = batch.num_rows();
    let mismatch = || EngineRefusal {
        code: super::STALE.to_owned(),
        detail: format!("{} is not stored as its kind", column.id),
    };
    match column.kind {
        ColumnKind::Integer | ColumnKind::Decimal { .. } => {
            let values = array
                .as_any()
                .downcast_ref::<Decimal128Array>()
                .ok_or_else(mismatch)?;
            let scale = u32::try_from(values.scale()).map_err(|_negative| mismatch())?;
            if values.precision() > u8::try_from(STORAGE_DIGITS).unwrap_or(u8::MAX) {
                return Err(mismatch());
            }
            Ok((0..rows)
                .map(|row| {
                    if values.is_null(row) {
                        Cell::Null
                    } else {
                        Cell::Dec(Dec {
                            m: values.value(row),
                            s: scale,
                        })
                    }
                })
                .collect())
        }
        ColumnKind::Boolean => Ok((0..rows)
            .map(|row| bool_at(array.as_ref(), row).map_or(Cell::Null, Cell::Bool))
            .collect()),
        ColumnKind::Text | ColumnKind::Opaque => Ok((0..rows)
            .map(|row| text_at(array.as_ref(), row).map_or(Cell::Null, Cell::Text))
            .collect()),
        ColumnKind::Timestamp | ColumnKind::Date => {
            let t = named(batch, &derived(&column.id, "t"))?;
            let n = named(batch, &derived(&column.id, "n"))?;
            Ok((0..rows)
                .map(|row| {
                    match (
                        text_at(array.as_ref(), row),
                        i64_at(t.as_ref(), row),
                        i32_at(n.as_ref(), row),
                    ) {
                        (Some(text), Some(t), Some(n)) => Cell::Instant { text, t, n },
                        (Some(text), _, _) => Cell::Text(text),
                        (None, _, _) => Cell::Null,
                    }
                })
                .collect())
        }
    }
}
