//! MANIFEST-LAND-1 / LAW-02 on the adapter, B1: a landing staged on disk. The
//! source is read in bounded chunks; each chunk is typed, its rules checked,
//! and its typed rows, typing refusals and findings appended, each already in
//! key order, to the attempt's own Parquet parts. In memory a landing keeps
//! only its counts and its residue (bounded by the residue page contract).
//! The data file (typed rows no rule holds back, by row) and the held-set
//! file (by row, rule nulls first, reason) are then written by streaming the
//! pre-ordered parts ([`super::merge`]): one ordered pass and a k-way merge in
//! byte-bounded batches, never a buffer of the whole file, a sort or a join.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use datafusion::arrow::array::{ArrayRef, BooleanArray, StringArray, UInt64Array};
use datafusion::arrow::datatypes::{DataType, Field, SchemaRef};

use super::super::{
    ColumnKind, LoadRequest, LoadResponse, MAX_RESIDUE_PAGES, RESIDUE_EXCEEDED, RESIDUE_PAGE_ROWS,
    RuleCheck, RuleCount, TableSchema, ValidRange,
};
use super::columnar::{self, Sink};
use super::files::{self, Caps, SourceRow};
use super::laws::{self, HeldRow, TypedRow};
use super::values::Cell;
use super::{Engine, Outcome, Tenant, engine_error, refusal};

/// One residue row: its undecided rules and its wide values' bytes.
#[derive(Debug, Clone)]
pub(super) struct Residue {
    pub rules: BTreeSet<String>,
    pub bytes: usize,
}

/// A landing's staged state between its steps: files on disk, and in memory
/// only its counts and residue.
#[derive(Debug, Clone)]
pub(super) struct Staged {
    pub table: TableSchema,
    /// Every typed row, with its wide values' bytes (`:bytes`).
    pub typed: PathBuf,
    /// The rows that could not be typed.
    pub typing: PathBuf,
    /// The findings, one file per step.
    pub findings: Vec<PathBuf>,
    pub residue: BTreeMap<u64, Residue>,
    pub counts: BTreeMap<String, RuleCount>,
    /// Rows a decided rule holds back.
    pub held_by_rules: u64,
    /// Residue rows a decided rule already holds back.
    pub residue_held: BTreeSet<u64>,
    pub load: LoadResponse,
}

pub(super) fn data_system() -> Vec<Field> {
    vec![
        Field::new(":id", DataType::Utf8, true),
        Field::new(":v", DataType::UInt64, true),
        Field::new(":row", DataType::UInt64, true),
    ]
}

pub(super) fn held_system() -> Vec<Field> {
    vec![
        Field::new(":row", DataType::UInt64, true),
        Field::new(":reason", DataType::Utf8, true),
        Field::new(":rule", DataType::Utf8, true),
        Field::new(":holds", DataType::Boolean, true),
    ]
}

/// The data file's schema.
pub(super) fn data_schema(table: &TableSchema) -> SchemaRef {
    columnar::storage_schema(data_system(), table)
}

/// The staged typed file's schema: the data file's and `:bytes`.
fn typed_schema(table: &TableSchema) -> SchemaRef {
    let mut system = data_system();
    system.push(Field::new(":bytes", DataType::UInt64, true));
    columnar::storage_schema(system, table)
}

/// The held-set file's schema.
pub(super) fn held_schema(table: &TableSchema) -> SchemaRef {
    columnar::text_schema(held_system(), table)
}

/// A typed row's wide (text-like) values' bytes: a lower bound on its JSON.
pub(super) fn value_bytes(table: &TableSchema, row: &TypedRow) -> usize {
    table
        .columns
        .iter()
        .zip(&row.cells)
        .filter(|(column, _)| laws::is_wide(column.kind))
        .map(|(_, cell)| cell.as_str().map_or(0, str::len))
        .fold(0_usize, usize::saturating_add)
}

/// Append typed rows to a typed sink.
fn write_typed(sink: &mut Sink, table: &TableSchema, rows: &[TypedRow]) -> Outcome<()> {
    let schema = typed_schema(table);
    let mut arrays: Vec<ArrayRef> = vec![
        Arc::new(StringArray::from_iter_values(
            rows.iter().map(|r| r.row.to_string()),
        )),
        Arc::new(UInt64Array::from(vec![1_u64; rows.len()])),
        Arc::new(UInt64Array::from_iter_values(rows.iter().map(|r| r.row))),
        Arc::new(UInt64Array::from_iter_values(rows.iter().map(|r| {
            u64::try_from(value_bytes(table, r)).unwrap_or(u64::MAX)
        }))),
    ];
    for (index, column) in table.columns.iter().enumerate() {
        let cells: Vec<&Cell> = rows
            .iter()
            .map(|row| row.cells.get(index).unwrap_or(&Cell::Null))
            .collect();
        arrays.extend(columnar::column_arrays(column, &cells)?);
    }
    sink.write(&columnar::batch(&schema, arrays)?)
}

/// A held row's text bytes.
pub(super) fn held_bytes(row: &HeldRow) -> usize {
    let base = row
        .reason
        .len()
        .saturating_add(row.rule.as_deref().map_or(0, str::len));
    row.values
        .iter()
        .flatten()
        .fold(base, |total, text| total.saturating_add(text.len()))
}

/// Append held rows to a held-set sink. NB3 (#1765): in slices of at most
/// [`columnar::MAX_BATCH_BYTES`] text bytes ([`held_slices`]), so many
/// rules' findings on wide rows never overflow a `Utf8` array's `i32`
/// offsets.
pub(super) fn write_held(sink: &mut Sink, table: &TableSchema, rows: &[HeldRow]) -> Outcome<()> {
    for slice in held_slices(rows, columnar::MAX_BATCH_BYTES)? {
        write_held_slice(sink, table, rows.get(slice).unwrap_or_default())?;
    }
    Ok(())
}

/// The consecutive slices of `rows` each holding at most `bound` text
/// bytes; one row past the bound alone refuses.
pub(super) fn held_slices(rows: &[HeldRow], bound: usize) -> Outcome<Vec<Range<usize>>> {
    let mut slices = Vec::new();
    let (mut start, mut bytes) = (0_usize, 0_usize);
    for (at, row) in rows.iter().enumerate() {
        let size = held_bytes(row);
        if size > bound {
            return Err(refusal(
                super::EXHAUSTED,
                "a held row is wider than the load's bound",
            ));
        }
        if at > start && bytes.saturating_add(size) > bound {
            slices.push(start..at);
            (start, bytes) = (at, 0);
        }
        bytes = bytes.saturating_add(size);
    }
    if start < rows.len() {
        slices.push(start..rows.len());
    }
    Ok(slices)
}

/// Append one slice of held rows, within the batch bound, as one batch.
fn write_held_slice(sink: &mut Sink, table: &TableSchema, rows: &[HeldRow]) -> Outcome<()> {
    let schema = held_schema(table);
    let mut arrays: Vec<ArrayRef> = vec![
        Arc::new(UInt64Array::from_iter_values(rows.iter().map(|r| r.row))),
        Arc::new(StringArray::from_iter_values(
            rows.iter().map(|r| r.reason.as_str()),
        )),
        Arc::new(StringArray::from(
            rows.iter().map(|r| r.rule.clone()).collect::<Vec<_>>(),
        )),
        Arc::new(BooleanArray::from(
            rows.iter().map(|r| r.holds).collect::<Vec<_>>(),
        )),
    ];
    for index in 0..table.columns.len() {
        arrays.push(Arc::new(StringArray::from(
            rows.iter()
                .map(|r| r.values.get(index).cloned().flatten())
                .collect::<Vec<_>>(),
        )));
    }
    sink.write(&columnar::batch(&schema, arrays)?)
}

/// Type one chunk: typed rows and the rows refused at typing (first reason
/// in declared column order).
fn type_chunk(table: &TableSchema, chunk: Vec<SourceRow>) -> (Vec<TypedRow>, Vec<HeldRow>) {
    let mut typed = Vec::new();
    let mut held = Vec::new();
    for row in chunk {
        let mut cells = Vec::with_capacity(table.columns.len());
        let mut refused = None;
        for (column, text) in table.columns.iter().zip(&row.texts) {
            match super::typing::type_text(column, text.as_deref()) {
                Ok(cell) => cells.push(cell),
                Err(reason) => {
                    refused = Some(reason);
                    break;
                }
            }
        }
        match refused {
            Some(reason) => held.push(HeldRow {
                row: row.row,
                reason,
                rule: None,
                holds: true,
                values: row.texts,
            }),
            None => typed.push(TypedRow {
                row: row.row,
                cells,
            }),
        }
    }
    (typed, held)
}

impl Staged {
    /// Count findings as a landing answers them.
    pub(super) fn record(&mut self, findings: &[HeldRow]) {
        let mut held = BTreeSet::new();
        for finding in findings {
            let Some(rule) = &finding.rule else { continue };
            let count = self
                .counts
                .entry(rule.clone())
                .or_insert_with(|| RuleCount {
                    label: finding.reason.clone(),
                    rows: 0,
                    holds: false,
                });
            if finding.reason.as_bytes() < count.label.as_bytes() {
                count.label.clone_from(&finding.reason);
            }
            count.rows = count.rows.saturating_add(1);
            count.holds |= finding.holds;
            if finding.holds {
                held.insert(finding.row);
            }
        }
        for row in held {
            let residue = self.residue.contains_key(&row);
            if !residue || self.residue_held.insert(row) {
                self.held_by_rules = self.held_by_rules.saturating_add(1);
            }
        }
    }
}

/// Stage `request`'s source `file` in `dir`, checking `checks` chunk by chunk.
pub(super) fn stage(
    request: &LoadRequest,
    checks: &[RuleCheck],
    file: &Path,
    dir: &Path,
    bytes: u64,
    caps: Caps,
) -> Outcome<Staged> {
    let started = Instant::now();
    let table = &request.table;
    let mut staged = Staged {
        table: table.clone(),
        typed: dir.join("typed.parquet"),
        typing: dir.join("typing.parquet"),
        findings: vec![dir.join("findings-0.parquet")],
        residue: BTreeMap::new(),
        counts: BTreeMap::new(),
        held_by_rules: 0,
        residue_held: BTreeSet::new(),
        load: LoadResponse {
            load: request.load.clone(),
            rows: 0,
            rejected: 0,
            reasons: BTreeMap::new(),
            bytes,
            millis: 0,
        },
    };
    let mut typed_sink = Sink::staged(&staged.typed, &typed_schema(table))?;
    let mut typing_sink = Sink::staged(&staged.typing, &held_schema(table))?;
    let findings_path = staged.findings.first().cloned().unwrap_or_default();
    let mut findings_sink = Sink::staged(&findings_path, &held_schema(table))?;
    let bound =
        usize::try_from(MAX_RESIDUE_PAGES.saturating_mul(RESIDUE_PAGE_ROWS)).unwrap_or(usize::MAX);
    files::read_source(request, file, caps, &mut |chunk| {
        let (typed, refused) = type_chunk(table, chunk);
        let checked = laws::check_rules(table, checks, &typed)?;
        for row in &typed {
            if let Some(rules) = checked.residue.get(&row.row) {
                let entry = Residue {
                    rules: rules.clone(),
                    bytes: value_bytes(table, row),
                };
                staged.residue.insert(row.row, entry);
            }
        }
        if staged.residue.len() > bound {
            return Err(refusal(
                RESIDUE_EXCEEDED,
                format!("the landing leaves more residue than {MAX_RESIDUE_PAGES} pages"),
            ));
        }
        staged.record(&checked.findings);
        for row in &refused {
            let count = staged.load.reasons.entry(row.reason.clone()).or_insert(0);
            *count = count.saturating_add(1);
        }
        staged.load.rows = staged
            .load
            .rows
            .saturating_add(u64::try_from(typed.len()).unwrap_or(u64::MAX));
        staged.load.rejected = staged
            .load
            .rejected
            .saturating_add(u64::try_from(refused.len()).unwrap_or(u64::MAX));
        write_typed(&mut typed_sink, table, &typed)?;
        write_held(&mut typing_sink, table, &refused)?;
        let mut findings = checked.findings;
        super::merge::sort_held(&mut findings);
        write_held(&mut findings_sink, table, &findings)
    })?;
    typed_sink.finish()?;
    typing_sink.finish()?;
    findings_sink.finish()?;
    staged.load.millis = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    Ok(staged)
}

/// The written files' row counts and the data's valid time.
pub(super) struct Written {
    pub data_rows: u64,
    pub held_rows: u64,
    pub valid_time: Vec<ValidRange>,
}

/// Each temporal column's earliest and latest trusted value, by instant
/// (the first in row order on a tie).
#[derive(Default)]
struct Ranges(BTreeMap<usize, (Extreme, Extreme)>);

/// An instant's ordering key and its text.
type Extreme = ((i64, i32), String);

impl Ranges {
    fn absorb(&mut self, index: usize, cells: &[Cell]) {
        for cell in cells {
            let (Some(key), Some(text)) = (cell.instant(), cell.as_str()) else {
                continue;
            };
            let entry = self
                .0
                .entry(index)
                .or_insert_with(|| ((key, text.to_owned()), (key, text.to_owned())));
            if key < entry.0.0 {
                entry.0 = (key, text.to_owned());
            }
            if key > entry.1.0 {
                entry.1 = (key, text.to_owned());
            }
        }
    }
}

/// Write `staged`'s data file to `data` and its held-set file to `held`.
pub(super) fn finish(
    engine: &Engine,
    staged: &Staged,
    data: &Path,
    held: &Path,
) -> Outcome<Written> {
    let table = &staged.table;
    let schema = data_schema(table);
    let mut sink = Sink::create(data, &schema)?;
    let mut ranges = Ranges::default();
    let meter = files::Meter::new(Caps::of(engine.memory));
    let back = super::merge::held_back(&staged.findings, engine.memory)?;
    super::merge::trusted(&staged.typed, &back, &meter, &mut sink, &mut |batch| {
        for (index, column) in table.columns.iter().enumerate() {
            if matches!(column.kind, ColumnKind::Timestamp | ColumnKind::Date) {
                ranges.absorb(index, &columnar::column_cells(batch, column)?);
            }
        }
        Ok(())
    })?;
    let data_rows = sink.finish()?;
    let mut sink = Sink::create(held, &held_schema(table))?;
    let mut parts = vec![staged.typing.clone()];
    parts.extend(staged.findings.iter().cloned());
    super::merge::merge_held(table, &parts, &meter, &mut sink)?;
    let held_rows = sink.finish()?;
    let valid_time = table
        .columns
        .iter()
        .enumerate()
        .filter_map(|(index, column)| {
            ranges.0.get(&index).map(|((_, from), (_, to))| ValidRange {
                column: column.id.clone(),
                from: from.clone(),
                to: to.clone(),
            })
        })
        .collect();
    Ok(Written {
        data_rows,
        held_rows,
        valid_time,
    })
}

/// The typed rows of `staged` numbered `rows`, from its typed file.
pub(super) fn typed_rows(
    engine: &Engine,
    tenant: &Tenant,
    staged: &Staged,
    rows: &[u64],
) -> Outcome<BTreeMap<u64, TypedRow>> {
    if rows.is_empty() {
        return Ok(BTreeMap::new());
    }
    let filter = super::scan::row_in(":row", rows);
    let mut out = BTreeMap::new();
    for batch in engine.scan(tenant, std::slice::from_ref(&staged.typed), Some(filter))? {
        let numbers = columnar::system(&batch, ":row", columnar::u64_at)?;
        let cells = staged
            .table
            .columns
            .iter()
            .map(|column| columnar::column_cells(&batch, column))
            .collect::<Outcome<Vec<_>>>()?;
        for (at, number) in numbers.into_iter().enumerate() {
            let row = number.ok_or_else(|| engine_error("a typed row without its number"))?;
            let row_cells = cells
                .iter()
                .map(|column| column.get(at).cloned().unwrap_or(Cell::Null))
                .collect();
            out.insert(
                row,
                TypedRow {
                    row,
                    cells: row_cells,
                },
            );
        }
    }
    Ok(out)
}
