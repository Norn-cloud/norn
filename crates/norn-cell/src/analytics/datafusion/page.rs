//! HOST-READ-3 on the adapter: one records page (every clause typed against
//! the admitted schema, a missing value satisfying no comparison but
//! `empty`, `ne` included; absent values last in either direction, then
//! identity ascending; the count taken before the limit, the limit capped)
//! and exports in identity order, numbers as exact strings in JSONL.

use std::cmp::Ordering;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use datafusion::arrow::array::{ArrayRef, StringArray, UInt64Array};
use datafusion::arrow::datatypes::{DataType, Field};
use transparency_sha2::{Digest as _, Sha256};

use super::super::{
    Column, ColumnKind, ExportFormat, ExportRequest, PageClause, PageOp, PageOperand, PageRequest,
    PageResponse, PageRow, TableSchema, hex,
};
use super::columnar;
use super::query::{Row, Scope};
use super::values::{Cell, Dec};
use super::{
    EXPORT_RESERVED, Engine, MAX_PAGE_ROWS, Outcome, Tenant, UNSUPPORTED, engine_error, refusal,
};

/// One typed operand.
enum Operand {
    Text(String),
    Number(Dec),
    Bool(bool),
    Instant(i64, i64),
}

fn invalid(column: &Column) -> super::super::EngineRefusal {
    refusal(
        "analytics.invalid",
        format!("{} does not compare as asked", column.id),
    )
}

fn operand(column: &Column, operand: &PageOperand) -> Outcome<Operand> {
    match (column.kind, operand) {
        (ColumnKind::Text | ColumnKind::Opaque, PageOperand::Text(text)) => {
            Ok(Operand::Text(text.clone()))
        }
        (ColumnKind::Integer | ColumnKind::Decimal { .. }, PageOperand::Number(text)) => {
            let canonical = norn_ir::Decimal::parse(text)
                .map_err(|_unparsed| invalid(column))?
                .canonical();
            let scale = canonical.split_once('.').map_or(0, |(_, f)| f.len());
            let value = Dec::parse_at(&canonical, u32::try_from(scale).unwrap_or(u32::MAX))
                .map_err(|_range| invalid(column))?;
            Ok(Operand::Number(value))
        }
        (ColumnKind::Boolean, PageOperand::Bool(flag)) => Ok(Operand::Bool(*flag)),
        (ColumnKind::Timestamp | ColumnKind::Date, PageOperand::Instant { secs, nanos }) => {
            Ok(Operand::Instant(*secs, i64::from(*nanos)))
        }
        _ => Err(invalid(column)),
    }
}

/// The order of a present cell against an operand; `None` when they do not compare.
fn against(cell: &Cell, operand: &Operand) -> Option<Ordering> {
    match (cell, operand) {
        (Cell::Text(text), Operand::Text(other)) => Some(text.as_bytes().cmp(other.as_bytes())),
        (Cell::Dec(value), Operand::Number(other)) => Some(value.compare(*other)),
        (Cell::Bool(flag), Operand::Bool(other)) => Some(flag.cmp(other)),
        (Cell::Instant { t, n, .. }, Operand::Instant(secs, nanos)) => {
            Some((*t, i64::from(*n)).cmp(&(*secs, *nanos)))
        }
        _ => None,
    }
}

/// One clause, typed: a test over the cell of its column.
struct Clause {
    index: usize,
    op: PageOp,
    operands: Vec<Operand>,
}

fn type_clause(scope: &Scope, clause: &PageClause) -> Outcome<Clause> {
    let (index, column) = scope.column(&clause.column)?;
    let count = clause.operands.len();
    let fits = match clause.op {
        PageOp::Empty | PageOp::Present => true,
        PageOp::Between => count == 2,
        PageOp::In => count > 0,
        PageOp::Eq
        | PageOp::Ne
        | PageOp::Lt
        | PageOp::Lte
        | PageOp::Gt
        | PageOp::Gte
        | PageOp::Contains
        | PageOp::Prefix => count == 1,
    };
    if !fits {
        return Err(invalid(column));
    }
    if matches!(clause.op, PageOp::Contains | PageOp::Prefix)
        && !(column.kind == ColumnKind::Text
            && matches!(clause.operands.first(), Some(PageOperand::Text(_))))
    {
        return Err(invalid(column));
    }
    let operands = if matches!(clause.op, PageOp::Empty | PageOp::Present) {
        Vec::new()
    } else {
        clause
            .operands
            .iter()
            .map(|o| operand(column, o))
            .collect::<Outcome<_>>()?
    };
    Ok(Clause {
        index,
        op: clause.op,
        operands,
    })
}

fn holds(clause: &Clause, row: &Row) -> bool {
    let cell = row.cells.get(clause.index).unwrap_or(&Cell::Null);
    // A temporal value's presence (its key) is checked by the caller.
    let present = cell.is_present();
    let first = clause.operands.first();
    let cmp = |o: Option<&Operand>| o.and_then(|o| against(cell, o));
    present
        && match clause.op {
            PageOp::Empty => false,
            PageOp::Present => true,
            PageOp::Eq => cmp(first).is_some_and(Ordering::is_eq),
            PageOp::Ne => cmp(first).is_some_and(Ordering::is_ne),
            PageOp::Lt => cmp(first).is_some_and(Ordering::is_lt),
            PageOp::Lte => cmp(first).is_some_and(Ordering::is_le),
            PageOp::Gt => cmp(first).is_some_and(Ordering::is_gt),
            PageOp::Gte => cmp(first).is_some_and(Ordering::is_ge),
            PageOp::Between => {
                cmp(first).is_some_and(Ordering::is_ge)
                    && cmp(clause.operands.get(1)).is_some_and(Ordering::is_le)
            }
            PageOp::In => clause
                .operands
                .iter()
                .any(|o| against(cell, o).is_some_and(Ordering::is_eq)),
            PageOp::Contains | PageOp::Prefix => match (cell.as_str(), first) {
                (Some(text), Some(Operand::Text(needle))) => {
                    if clause.op == PageOp::Contains {
                        text.contains(needle.as_str())
                    } else {
                        text.starts_with(needle.as_str())
                    }
                }
                _ => false,
            },
        }
}

fn matches_empty(clause: &Clause, row: &Row, temporal: bool) -> bool {
    let cell = row.cells.get(clause.index).unwrap_or(&Cell::Null);
    if temporal {
        cell.instant().is_none()
    } else {
        !cell.is_present()
    }
}

/// One records page of a source.
pub(super) fn page(
    engine: &Engine,
    tenant: &mut Tenant,
    request: &PageRequest,
) -> Outcome<PageResponse> {
    let scope = Scope::new(
        engine,
        tenant,
        &request.source,
        request.model_pin.as_deref(),
        false,
    )?;
    scope.verify_rows(&request.source, false)?;
    let clauses = request
        .filters
        .iter()
        .map(|clause| type_clause(&scope, clause))
        .collect::<Outcome<Vec<_>>>()?;
    let mut keys = Vec::new();
    for key in &request.sort {
        let (index, column) = scope.column(&key.column)?;
        if column.kind == ColumnKind::Opaque {
            return Err(refusal(UNSUPPORTED, format!("{} has no order", column.id)));
        }
        keys.push((index, key.descending));
    }
    let temporal: Vec<bool> = scope
        .columns
        .iter()
        .map(|c| matches!(c.kind, ColumnKind::Timestamp | ColumnKind::Date))
        .collect();
    let ids: std::collections::BTreeSet<&str> = request.ids.iter().map(String::as_str).collect();
    let mut rows: Vec<&Row> = scope
        .population()
        .filter(|row| ids.is_empty() || ids.contains(row.id.as_str()))
        .filter(|row| {
            clauses.iter().all(|clause| {
                let is_temporal = temporal.get(clause.index).copied().unwrap_or(false);
                if clause.op == PageOp::Empty {
                    matches_empty(clause, row, is_temporal)
                } else if is_temporal {
                    row.cells
                        .get(clause.index)
                        .is_some_and(|cell| cell.instant().is_some())
                        && holds(clause, row)
                } else {
                    holds(clause, row)
                }
            })
        })
        .collect();
    rows.sort_by(|a, b| {
        keys.iter()
            .map(|(index, descending)| {
                let (x, y) = (
                    a.cells.get(*index).unwrap_or(&Cell::Null),
                    b.cells.get(*index).unwrap_or(&Cell::Null),
                );
                let temporal = temporal.get(*index).copied().unwrap_or(false);
                let present = |c: &Cell| {
                    if temporal {
                        c.instant().is_some()
                    } else {
                        c.is_present()
                    }
                };
                match (present(x), present(y)) {
                    (false, false) => Ordering::Equal,
                    (false, true) => Ordering::Greater,
                    (true, false) => Ordering::Less,
                    (true, true) if *descending => y.order(x),
                    (true, true) => x.order(y),
                }
            })
            .find(|order| order.is_ne())
            .unwrap_or_else(|| a.id.as_bytes().cmp(b.id.as_bytes()))
    });
    let total = u64::try_from(rows.len()).unwrap_or(u64::MAX);
    let offset = usize::try_from(request.offset).unwrap_or(usize::MAX);
    let limit = usize::try_from(request.limit.min(MAX_PAGE_ROWS)).unwrap_or(0);
    let rows = rows
        .into_iter()
        .skip(offset)
        .take(limit)
        .map(|row| PageRow {
            id: row.id.clone(),
            version: row.v,
            values: scope
                .columns
                .iter()
                .zip(&row.cells)
                .filter_map(|(column, cell)| {
                    cell.present(column.kind)
                        .map(|value| (column.id.clone(), value))
                })
                .collect(),
        })
        .collect();
    Ok(PageResponse { total, rows })
}

/// Export columns of every row of a source into a scratch file.
pub(super) fn export(
    engine: &Engine,
    tenant: &mut Tenant,
    request: &ExportRequest,
    scratch: &Path,
) -> Outcome<PathBuf> {
    let scope = Scope::new(
        engine,
        tenant,
        &request.source,
        request.model_pin.as_deref(),
        false,
    )?;
    scope.verify_rows(&request.source, false)?;
    let mut columns = Vec::new();
    for id in &request.columns {
        if EXPORT_RESERVED.contains(&id.as_str()) {
            return Err(refusal(
                "analytics.invalid",
                format!("{id} names the row's own column"),
            ));
        }
        let (index, column) = scope.column(id)?;
        columns.push((index, column.clone()));
    }
    let mut rows: Vec<&Row> = scope.population().collect();
    rows.sort_by(|a, b| a.id.as_bytes().cmp(b.id.as_bytes()));
    let name = format!(
        "export-{}.{}",
        hex(&Sha256::digest(
            format!("{request:?}{:?}", std::time::Instant::now()).as_bytes()
        )),
        match request.format {
            ExportFormat::Parquet => "parquet",
            ExportFormat::Jsonl => "jsonl",
        }
    );
    let path = scratch.join(name);
    match request.format {
        ExportFormat::Jsonl => export_jsonl(&path, &rows, &columns)?,
        ExportFormat::Parquet => export_parquet(&path, &rows, &columns)?,
    }
    Ok(path)
}

fn export_jsonl(path: &Path, rows: &[&Row], columns: &[(usize, Column)]) -> Outcome<()> {
    let file = super::catalog::create_new(path)?;
    let mut out = std::io::BufWriter::new(file);
    for row in rows {
        let mut object = serde_json::Map::new();
        object.insert("id".to_owned(), serde_json::Value::String(row.id.clone()));
        object.insert("version".to_owned(), serde_json::Value::from(row.v));
        for (index, column) in columns {
            let cell = row.cells.get(*index).unwrap_or(&Cell::Null);
            let value = match cell {
                Cell::Null => serde_json::Value::Null,
                Cell::Bool(flag) => serde_json::Value::Bool(*flag),
                Cell::Dec(_) | Cell::Text(_) | Cell::Instant { .. } => {
                    serde_json::Value::String(cell.text().unwrap_or_default())
                }
            };
            object.insert(column.id.clone(), value);
        }
        let line = serde_json::to_string(&serde_json::Value::Object(object))
            .map_err(|e| engine_error(e.to_string()))?;
        writeln!(out, "{line}").map_err(|e| engine_error(e.to_string()))?;
    }
    let file = out.into_inner().map_err(|e| engine_error(e.to_string()))?;
    file.sync_all().map_err(|e| engine_error(e.to_string()))
}

fn export_parquet(path: &Path, rows: &[&Row], columns: &[(usize, Column)]) -> Outcome<()> {
    let table = TableSchema {
        name: String::new(),
        columns: columns.iter().map(|(_, column)| column.clone()).collect(),
        pin: String::new(),
    };
    let mut fields = vec![
        Field::new("id", DataType::Utf8, false),
        Field::new("version", DataType::UInt64, false),
    ];
    for column in &table.columns {
        fields.extend(columnar::column_fields(column).into_iter().take(1));
    }
    let schema = Arc::new(datafusion::arrow::datatypes::Schema::new(fields));
    let mut arrays: Vec<ArrayRef> = vec![
        Arc::new(StringArray::from_iter_values(
            rows.iter().map(|r| r.id.as_str()),
        )),
        Arc::new(UInt64Array::from_iter_values(rows.iter().map(|r| r.v))),
    ];
    for (index, column) in columns {
        let cells: Vec<&Cell> = rows
            .iter()
            .map(|row| row.cells.get(*index).unwrap_or(&Cell::Null))
            .collect();
        arrays.extend(columnar::column_arrays(column, &cells)?.into_iter().take(1));
    }
    let batch = columnar::batch(&schema, arrays)?;
    columnar::write_parquet(path, &schema, &[batch])
}
