//! LAW-02 on the adapter: a landing's final files from its staged parts,
//! streamed in small batches. Every part is written in key order (typed rows
//! by row; held rows by row, rule nulls first, reason), so the data file is
//! one ordered pass over the typed rows less the rows a rule holds back, and
//! the held-set file is a k-way merge of the held parts. Nothing is sorted
//! or joined in memory, whatever the rows' width.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use datafusion::arrow::array::BooleanArray;
use datafusion::arrow::compute::filter_record_batch;
use datafusion::arrow::record_batch::RecordBatch;
use parquet::arrow::arrow_reader::{ParquetRecordBatchReader, ParquetRecordBatchReaderBuilder};

use super::super::TableSchema;
use super::columnar::{self, MAX_BATCH_BYTES, Sink};
use super::files::Meter;
use super::laws::HeldRow;
use super::staging;
use super::{EXHAUSTED, Outcome, engine_error, refusal};

/// The most rows read from a staged part at a time.
const MERGE_ROWS: usize = 64;

/// A held row's order in the held-set file.
fn key(row: &HeldRow) -> (u64, Option<&[u8]>, &[u8]) {
    (
        row.row,
        row.rule.as_deref().map(str::as_bytes),
        row.reason.as_bytes(),
    )
}

/// Put held rows in the held-set file's order (a part is written so).
pub(super) fn sort_held(rows: &mut [HeldRow]) {
    rows.sort_by(|a, b| key(a).cmp(&key(b)));
}

/// A staged part's reader, its batches sized (B2) so that a batch of its
/// widest row group's average row stays within `budget` bytes. NB6: this is
/// an average, not a hard bound: a batch holding the part's widest rows can
/// decode up to [`MERGE_ROWS`] of them. A staged part is the engine's own
/// file, written without dictionaries (NB2) from rows each already within
/// the load's cell and [`MAX_BATCH_BYTES`] bounds, so its averages are
/// honest; an untrusted source file is bounded by its pages instead
/// ([`super::pages`]).
pub(super) fn reader(path: &Path, budget: usize) -> Outcome<ParquetRecordBatchReader> {
    let failed = |error: parquet::errors::ParquetError| engine_error(error.to_string());
    let file = std::fs::File::open(path).map_err(|error| engine_error(error.to_string()))?;
    let builder = ParquetRecordBatchReaderBuilder::try_new(file).map_err(failed)?;
    let row_bytes = builder
        .metadata()
        .row_groups()
        .iter()
        .map(|group| {
            let rows = group.num_rows().max(1);
            usize::try_from(group.total_byte_size().div_euclid(rows)).unwrap_or(usize::MAX)
        })
        .max()
        .unwrap_or(1)
        .max(1);
    let rows = budget
        .min(MAX_BATCH_BYTES)
        .div_euclid(row_bytes)
        .clamp(1, MERGE_ROWS);
    builder.with_batch_size(rows).build().map_err(failed)
}

fn next(reader: &mut ParquetRecordBatchReader) -> Outcome<Option<RecordBatch>> {
    reader
        .next()
        .transpose()
        .map_err(|error| engine_error(error.to_string()))
}

/// The held rows of one batch of a held part.
fn held_rows(batch: &RecordBatch, table: &TableSchema) -> Outcome<Vec<HeldRow>> {
    let rows = columnar::system(batch, ":row", columnar::u64_at)?;
    let reasons = columnar::system(batch, ":reason", columnar::text_at)?;
    let rules = columnar::system(batch, ":rule", columnar::text_at)?;
    let holds = columnar::system(batch, ":holds", columnar::bool_at)?;
    let values = table
        .columns
        .iter()
        .map(|column| columnar::system(batch, &column.id, columnar::text_at))
        .collect::<Outcome<Vec<_>>>()?;
    (0..batch.num_rows())
        .map(|at| {
            Ok(HeldRow {
                row: rows
                    .get(at)
                    .copied()
                    .flatten()
                    .ok_or_else(|| engine_error("a held row without its number"))?,
                reason: reasons.get(at).cloned().flatten().unwrap_or_default(),
                rule: rules.get(at).cloned().flatten(),
                holds: holds.get(at).copied().flatten().unwrap_or(false),
                values: values
                    .iter()
                    .map(|column| column.get(at).cloned().flatten())
                    .collect(),
            })
        })
        .collect()
}

/// The rows a rule holds back, in order, within `memory` bytes.
pub(super) fn held_back(findings: &[PathBuf], memory: usize) -> Outcome<Vec<u64>> {
    let mut rows = Vec::new();
    for part in findings {
        let mut part = reader(part, memory.div_euclid(8).max(1))?;
        while let Some(batch) = next(&mut part)? {
            let numbers = columnar::system(&batch, ":row", columnar::u64_at)?;
            let rules = columnar::system(&batch, ":rule", columnar::text_at)?;
            let holds = columnar::system(&batch, ":holds", columnar::bool_at)?;
            for ((row, rule), hold) in numbers.into_iter().zip(rules).zip(holds) {
                if let (Some(row), Some(_), Some(true)) = (row, rule, hold) {
                    rows.push(row);
                }
            }
            if rows.len().saturating_mul(8) > memory {
                return Err(refusal(
                    EXHAUSTED,
                    "a landing holds back more rows than its bound",
                ));
            }
        }
    }
    rows.sort_unstable();
    rows.dedup();
    Ok(rows)
}

/// Stream the typed rows not in `back` (sorted) into `sink`, in row order,
/// handing each written batch to `seen`; batches are sized to half a chunk
/// of the part's average row (an average bound, see [`reader`]). A typed
/// row without its `:row` number refuses.
pub(super) fn trusted(
    typed: &Path,
    back: &[u64],
    meter: &Meter,
    sink: &mut Sink,
    seen: &mut dyn FnMut(&RecordBatch) -> Outcome<()>,
) -> Outcome<()> {
    let mut part = reader(typed, meter.caps.chunk.div_euclid(2))?;
    while let Some(batch) = next(&mut part)? {
        meter.deadline()?;
        let numbers = columnar::system(&batch, ":row", columnar::u64_at)?;
        let keep = numbers
            .iter()
            .map(|row| {
                row.map(|row| Some(back.binary_search(&row).is_err()))
                    .ok_or_else(|| engine_error("a typed row without its number"))
            })
            .collect::<Outcome<BooleanArray>>()?;
        let kept = filter_record_batch(&batch, &keep).map_err(|e| engine_error(e.to_string()))?;
        if kept.num_rows() > 0 {
            seen(&kept)?;
            sink.write_named(&kept)?;
        }
    }
    Ok(())
}

/// One held part's rows, read ahead a batch at a time.
struct Cursor {
    reader: ParquetRecordBatchReader,
    ahead: VecDeque<HeldRow>,
}

impl Cursor {
    /// Read the next batch when nothing is ahead.
    fn fill(&mut self, table: &TableSchema) -> Outcome<()> {
        if self.ahead.is_empty()
            && let Some(batch) = next(&mut self.reader)?
        {
            self.ahead.extend(held_rows(&batch, table)?);
        }
        Ok(())
    }
}

/// Merge the held parts (each in key order) into `sink` in key order, each
/// part read within its share of half a chunk, the output flushed by rows or
/// bytes, whichever comes first.
pub(super) fn merge_held(
    table: &TableSchema,
    parts: &[PathBuf],
    meter: &Meter,
    sink: &mut Sink,
) -> Outcome<()> {
    let budget = meter.caps.chunk.div_euclid(2).min(MAX_BATCH_BYTES);
    let share = budget.div_euclid(parts.len().max(1)).max(1);
    let mut cursors = parts
        .iter()
        .map(|part| {
            reader(part, share).map(|reader| Cursor {
                reader,
                ahead: VecDeque::new(),
            })
        })
        .collect::<Outcome<Vec<_>>>()?;
    let mut out = Vec::with_capacity(MERGE_ROWS);
    let mut bytes = 0_usize;
    loop {
        meter.deadline()?;
        for cursor in &mut cursors {
            cursor.fill(table)?;
        }
        let least = cursors
            .iter()
            .enumerate()
            .filter_map(|(at, cursor)| cursor.ahead.front().map(|row| (at, row)))
            .min_by(|(_, a), (_, b)| key(a).cmp(&key(b)))
            .map(|(at, _)| at);
        let Some(at) = least else { break };
        if let Some(row) = cursors
            .get_mut(at)
            .and_then(|cursor| cursor.ahead.pop_front())
        {
            bytes = bytes.saturating_add(staging::held_bytes(&row));
            out.push(row);
        }
        if out.len() >= MERGE_ROWS || bytes >= budget {
            staging::write_held(sink, table, &std::mem::take(&mut out))?;
            bytes = 0;
        }
    }
    if !out.is_empty() {
        staging::write_held(sink, table, &out)?;
    }
    Ok(())
}
