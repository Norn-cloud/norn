//! BULK-LOAD-1 on the adapter: a source file read once, in its physical
//! order, in bounded chunks, into each declared column's text, every row
//! numbered from 1 before anything is filtered. A CSV is read strictly
//! (RFC 4180 quoting, equal field counts, a UTF-8 byte-order mark skipped,
//! blank lines skipped) and a blank field is null; a Parquet value is null
//! only when the file says so, and present text, empty or blank, is kept. An
//! instant is imported with every nanosecond and its instant or not at all
//! (LAW-03): a nanosecond field a reader would narrow or zone refuses.
//!
//! B1: nothing here holds a whole file. Rows reach the caller [`CHUNK_ROWS`]
//! at a time, and every cell, chunk, file and footer is checked against the
//! [`Caps`] the engine's memory bound gives before more is read.

use std::collections::BTreeSet;
use std::io::{BufRead as _, Read as _, Seek as _, SeekFrom};
use std::path::Path;
use std::time::{Duration, Instant};

use datafusion::arrow::datatypes::{DataType, TimeUnit};
use parquet::arrow::arrow_reader::{ArrowReaderOptions, ParquetRecordBatchReaderBuilder};
use parquet::basic::{LogicalType, Type as PhysicalType};

use super::super::{Column, ColumnKind, FileFormat, LoadRequest};
use super::parquet_text::{Encoding, is_temporal, parquet_text};
use super::{EXHAUSTED, Outcome, engine_error, refusal};

/// Rows handed on at a time.
pub(super) const CHUNK_ROWS: usize = 1024;
/// The most row groups a source file may declare.
const MAX_ROW_GROUPS: usize = 100_000;
/// The most top-level fields a source file may declare.
const MAX_FIELDS: usize = 4096;

/// The bounds of one load, from the engine's memory bound.
#[derive(Debug, Clone, Copy)]
pub(super) struct Caps {
    /// The widest cell, in bytes.
    pub cell: usize,
    /// The most bytes one chunk (or one decoded Parquet batch) may hold.
    pub chunk: usize,
    /// The most text bytes a whole load may decode.
    pub total: u64,
    /// The largest Parquet footer read before its metadata is parsed.
    pub footer: u64,
    /// The longest a load may run.
    pub deadline: Duration,
}

impl Caps {
    /// The bounds under `memory` bytes.
    pub(super) fn of(memory: usize) -> Self {
        let quarter = memory.div_euclid(4).max(1);
        Self {
            cell: quarter.min(64 << 20),
            chunk: quarter,
            total: u64::try_from(memory).unwrap_or(u64::MAX).saturating_mul(64),
            footer: u64::try_from(quarter.min(64 << 20)).unwrap_or(u64::MAX),
            deadline: Duration::from_mins(30),
        }
    }
}

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

/// The running bounds of one read.
struct Meter {
    caps: Caps,
    started: Instant,
    total: u64,
}

impl Meter {
    fn chunk(&mut self, bytes: usize) -> Outcome<()> {
        self.total = self
            .total
            .saturating_add(u64::try_from(bytes).unwrap_or(u64::MAX));
        if bytes > self.caps.chunk || self.total > self.caps.total {
            return Err(refusal(EXHAUSTED, "the load decodes more than its bound"));
        }
        if self.started.elapsed() > self.caps.deadline {
            return Err(refusal(EXHAUSTED, "the load ran past its deadline"));
        }
        Ok(())
    }

    fn cell(&self, text: &str) -> Outcome<()> {
        if text.len() > self.caps.cell {
            return Err(refusal(EXHAUSTED, "a value is wider than the load's bound"));
        }
        Ok(())
    }
}

/// Every row of the source, each declared column's text, handed to `each`
/// in chunks of at most [`CHUNK_ROWS`] rows.
pub(super) fn read_source(
    request: &LoadRequest,
    file: &Path,
    caps: Caps,
    each: &mut dyn FnMut(Vec<SourceRow>) -> Outcome<()>,
) -> Outcome<()> {
    for column in &request.table.columns {
        super::typing::check_laws(column)?;
    }
    let mut meter = Meter {
        caps,
        started: Instant::now(),
        total: 0,
    };
    match request.file.format {
        FileFormat::Csv => read_csv(request, file, &mut meter, each),
        FileFormat::Parquet => read_parquet(request, file, &mut meter, each),
    }
}

/// One record of `line` (its line ending removed), as `DuckDB`'s strict
/// reader splits it: fields at commas; a field that starts with a quote is
/// quoted (doubled quotes undone, and anything but a comma after its closing
/// quote refuses); a quote inside an unquoted field is a literal character.
/// `None` when the line ends inside a quoted field (the record goes on).
fn fields(line: &[u8]) -> Outcome<Option<Vec<Vec<u8>>>> {
    let mut out = Vec::new();
    let mut at = 0;
    loop {
        let mut field = Vec::new();
        if line.get(at) == Some(&b'"') {
            at = at.saturating_add(1);
            loop {
                match (line.get(at), line.get(at.saturating_add(1))) {
                    (Some(b'"'), Some(b'"')) => {
                        field.push(b'"');
                        at = at.saturating_add(2);
                    }
                    (Some(b'"'), next) => {
                        at = at.saturating_add(1);
                        if !matches!(next, None | Some(b',')) {
                            return Err(engine_error("the CSV has text after a closing quote"));
                        }
                        break;
                    }
                    (Some(byte), _) => {
                        field.push(*byte);
                        at = at.saturating_add(1);
                    }
                    (None, _) => return Ok(None),
                }
            }
        } else {
            while let Some(byte) = line.get(at).filter(|b| **b != b',') {
                field.push(*byte);
                at = at.saturating_add(1);
            }
        }
        out.push(field);
        if line.get(at) == Some(&b',') {
            at = at.saturating_add(1);
        } else {
            return Ok(Some(out));
        }
    }
}

/// The next record's fields (lines joined while a quoted field is open), or
/// `None` at the end; blank lines are skipped.
fn next_record(
    reader: &mut impl std::io::BufRead,
    meter: &Meter,
) -> Outcome<Option<(usize, Vec<Vec<u8>>)>> {
    let mut record = Vec::new();
    loop {
        let read = reader.read_until(b'\n', &mut record).map_err(unreadable)?;
        if record.len() > meter.caps.chunk {
            return Err(refusal(
                EXHAUSTED,
                "a CSV record is wider than the load's bound",
            ));
        }
        let mut line = record.clone();
        while matches!(line.last(), Some(b'\n' | b'\r')) {
            line.pop();
        }
        if read == 0 && line.is_empty() {
            return Ok(None);
        }
        if line.is_empty() && read > 0 && !record.contains(&b'"') {
            record.clear();
            continue;
        }
        match fields(&line)? {
            Some(found) => return Ok(Some((line.len(), found))),
            None if read == 0 => return Err(engine_error("the CSV ends inside a quoted field")),
            None => {}
        }
    }
}

fn read_csv(
    request: &LoadRequest,
    file: &Path,
    meter: &mut Meter,
    each: &mut dyn FnMut(Vec<SourceRow>) -> Outcome<()>,
) -> Outcome<()> {
    let source =
        std::fs::File::open(file).map_err(|e| refusal("bulk.unreadable", e.to_string()))?;
    let mut reader = std::io::BufReader::new(source);
    if reader
        .fill_buf()
        .map_err(unreadable)?
        .starts_with(b"\xEF\xBB\xBF")
    {
        reader.consume(3);
    }
    let text = |bytes: Vec<u8>| String::from_utf8(bytes).map_err(unreadable);
    let headers = next_record(&mut reader, meter)?
        .map_or_else(Vec::new, |(_, found)| found)
        .into_iter()
        .map(text)
        .collect::<Outcome<Vec<_>>>()?;
    let mut positions = Vec::new();
    for column in &request.table.columns {
        let name = field_name(request, column);
        positions.push(
            headers
                .iter()
                .position(|header| header == name)
                .ok_or_else(|| missing(name))?,
        );
    }
    let mut chunk = Vec::new();
    let mut bytes = 0_usize;
    let mut number = 0_u64;
    while let Some((width, record)) = next_record(&mut reader, meter)? {
        if record.len() != headers.len() {
            return Err(engine_error("a CSV row has another number of fields"));
        }
        number = number.saturating_add(1);
        bytes = bytes.saturating_add(width);
        let mut texts = Vec::with_capacity(positions.len());
        for at in &positions {
            let value = text(record.get(*at).cloned().unwrap_or_default())?;
            meter.cell(&value)?;
            texts.push((!value.trim_matches(' ').is_empty()).then_some(value));
        }
        chunk.push(SourceRow { row: number, texts });
        if chunk.len() >= CHUNK_ROWS {
            meter.chunk(std::mem::take(&mut bytes))?;
            each(std::mem::take(&mut chunk))?;
        }
    }
    meter.chunk(bytes)?;
    if chunk.is_empty() {
        Ok(())
    } else {
        each(chunk)
    }
}

/// Refuse a Parquet file whose footer is past the bound, before any of its
/// metadata is parsed.
fn check_footer(file: &mut std::fs::File, caps: Caps) -> Outcome<()> {
    let length = file.metadata().map_err(unreadable)?.len();
    if length < 12 {
        return Err(engine_error("not a Parquet file"));
    }
    let mut tail = [0_u8; 8];
    file.seek(SeekFrom::End(-8)).map_err(unreadable)?;
    file.read_exact(&mut tail).map_err(unreadable)?;
    let (size, magic) = tail.split_at(4);
    let footer = u32::from_le_bytes(size.try_into().map_err(|_short| engine_error("a footer"))?);
    if magic != b"PAR1" {
        return Err(engine_error("not a Parquet file"));
    }
    if u64::from(footer) > caps.footer || u64::from(footer).saturating_add(8) > length {
        return Err(refusal(
            EXHAUSTED,
            "the Parquet footer is larger than the load's bound",
        ));
    }
    file.seek(SeekFrom::Start(0)).map_err(unreadable)?;
    Ok(())
}

/// LAW-03: how each declared column's field becomes text, refusing fields
/// that hold instants this load cannot import exactly.
fn parquet_plan(
    request: &LoadRequest,
    builder: &ParquetRecordBatchReaderBuilder<std::fs::File>,
) -> Outcome<Vec<(usize, Encoding, ColumnKind)>> {
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
    let schema = builder.schema();
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
            // N6.1: a UTC-adjusted nanosecond field is read zoned, at
            // microseconds, by `DuckDB`: lossy, so refused.
            (ColumnKind::Timestamp, DataType::Timestamp(TimeUnit::Nanosecond, None))
                if nanos.contains(name) =>
            {
                Encoding::Nanos
            }
            (ColumnKind::Timestamp, DataType::Timestamp(TimeUnit::Nanosecond, _)) => {
                return Err(lossy());
            }
            (ColumnKind::Timestamp, DataType::Timestamp(_, _)) => Encoding::Micros,
            (ColumnKind::Timestamp, kind) if is_temporal(kind) => return Err(lossy()),
            _ => Encoding::Plain,
        };
        plan.push((index, encoding, column.kind));
    }
    Ok(plan)
}

fn read_parquet(
    request: &LoadRequest,
    file: &Path,
    meter: &mut Meter,
    each: &mut dyn FnMut(Vec<SourceRow>) -> Outcome<()>,
) -> Outcome<()> {
    let mut source =
        std::fs::File::open(file).map_err(|e| refusal("bulk.unreadable", e.to_string()))?;
    check_footer(&mut source, meter.caps)?;
    // N6.4: the file's own Arrow schema hint is ignored; values are read as
    // their Parquet types say, as `DuckDB` reads them.
    let options = ArrowReaderOptions::new().with_skip_arrow_metadata(true);
    let builder = ParquetRecordBatchReaderBuilder::try_new_with_options(source, options)
        .map_err(unreadable)?;
    if builder.metadata().num_row_groups() > MAX_ROW_GROUPS
        || builder.schema().fields().len() > MAX_FIELDS
    {
        return Err(refusal(
            "bulk.unsupported_encoding",
            "the file declares too much structure",
        ));
    }
    let plan = parquet_plan(request, &builder)?;
    let reader = builder
        .with_batch_size(CHUNK_ROWS)
        .build()
        .map_err(unreadable)?;
    let mut number = 0_u64;
    for batch in reader {
        let batch = batch.map_err(unreadable)?;
        meter.chunk(batch.get_array_memory_size())?;
        let mut chunk = Vec::with_capacity(batch.num_rows());
        for row in 0..batch.num_rows() {
            number = number.saturating_add(1);
            let mut texts = Vec::with_capacity(plan.len());
            for (index, encoding, kind) in &plan {
                let array = batch
                    .columns()
                    .get(*index)
                    .ok_or_else(|| engine_error("a Parquet column"))?;
                let text = parquet_text(array.as_ref(), row, *encoding, *kind)?;
                if let Some(text) = &text {
                    meter.cell(text)?;
                }
                texts.push(text);
            }
            chunk.push(SourceRow { row: number, texts });
        }
        each(chunk)?;
    }
    Ok(())
}
