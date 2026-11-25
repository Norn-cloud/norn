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
use super::columnar::{self, Sink};
use super::laws::HeldRow;
use super::staging;
use super::{EXHAUSTED, Outcome, engine_error, refusal};

/// Rows read from a staged part at a time.
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

fn reader(path: &Path) -> Outcome<ParquetRecordBatchReader> {
    let file = std::fs::File::open(path).map_err(|error| engine_error(error.to_string()))?;
    ParquetRecordBatchReaderBuilder::try_new(file)
        .and_then(|builder| builder.with_batch_size(MERGE_ROWS).build())
        .map_err(|error| engine_error(error.to_string()))
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
        let mut part = reader(part)?;
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
/// handing each written batch to `seen`.
pub(super) fn trusted(
    typed: &Path,
    back: &[u64],
    sink: &mut Sink,
    seen: &mut dyn FnMut(&RecordBatch) -> Outcome<()>,
) -> Outcome<()> {
    let mut part = reader(typed)?;
    while let Some(batch) = next(&mut part)? {
        let numbers = columnar::system(&batch, ":row", columnar::u64_at)?;
        let keep: BooleanArray = numbers
            .iter()
            .map(|row| Some(row.is_some_and(|row| back.binary_search(&row).is_err())))
            .collect();
        let kept = filter_record_batch(&batch, &keep).map_err(|e| engine_error(e.to_string()))?;
        if kept.num_rows() > 0 {
            seen(&kept)?;
            sink.write_named(&kept)?;
        }
    }
    Ok(())
}

/// A held row's order, owned.
type OwnedKey = (u64, Option<Vec<u8>>, Vec<u8>);

/// One held part's rows, read ahead a batch at a time.
struct Cursor {
    reader: ParquetRecordBatchReader,
    ahead: VecDeque<HeldRow>,
}

impl Cursor {
    fn front(&mut self, table: &TableSchema) -> Outcome<Option<&HeldRow>> {
        if self.ahead.is_empty()
            && let Some(batch) = next(&mut self.reader)?
        {
            self.ahead.extend(held_rows(&batch, table)?);
        }
        Ok(self.ahead.front())
    }
}

/// Merge the held parts (each in key order) into `sink` in key order.
pub(super) fn merge_held(table: &TableSchema, parts: &[PathBuf], sink: &mut Sink) -> Outcome<()> {
    let mut cursors = parts
        .iter()
        .map(|part| {
            reader(part).map(|reader| Cursor {
                reader,
                ahead: VecDeque::new(),
            })
        })
        .collect::<Outcome<Vec<_>>>()?;
    let mut out = Vec::with_capacity(MERGE_ROWS);
    loop {
        let mut least: Option<(usize, OwnedKey)> = None;
        for (at, cursor) in cursors.iter_mut().enumerate() {
            if let Some(row) = cursor.front(table)? {
                let (number, rule, reason) = key(row);
                let owned = (number, rule.map(<[u8]>::to_vec), reason.to_vec());
                if least.as_ref().is_none_or(|(_, best)| owned < *best) {
                    least = Some((at, owned));
                }
            }
        }
        let Some((at, _)) = least else { break };
        if let Some(row) = cursors
            .get_mut(at)
            .and_then(|cursor| cursor.ahead.pop_front())
        {
            out.push(row);
        }
        if out.len() >= MERGE_ROWS {
            staging::write_held(sink, table, &std::mem::take(&mut out))?;
        }
    }
    if !out.is_empty() {
        staging::write_held(sink, table, &out)?;
    }
    Ok(())
}
