//! D137 (B2), #1765: mirror compaction. A table's segments grow by one per
//! sync; once more than [`COMPACT_AFTER`] are published, they are replaced
//! by one segment of resolved versions: every version they hold, each with
//! the commit interval the segments give it, tombstones dropped.
//!
//! The replacement is a streaming k-way merge, never the table's history in
//! memory: every segment is in identity order ([`mirror::identity_order`]),
//! each is read a bounded batch at a time, and one identity's rows across
//! the segments are resolved together ([`mirror::intervals`] holds over one
//! identity's rows alone) and written out before the next identity is read.
//! What the merge holds at once is bounded by the engine's memory limit:
//! an identity whose rows alone would pass a quarter of it refuses the
//! compaction (best effort: the segments stay as they are, exact), and so
//! does a segment out of identity order.
//!
//! The replacement is installed first and published by one catalog commit
//! that rechecks the segments it replaces, so a reader sees the old segments
//! or the new one, and answers the same either way. Only then are the
//! replaced objects removed; a refused publish removes the replacement.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use parquet::arrow::arrow_reader::ParquetRecordBatchReader;

use super::super::TableSchema;
use super::catalog::{self, Segment};
use super::columnar::Sink;
use super::mirror::{self, SegmentRow, Version};
use super::{EXHAUSTED, Engine, Outcome, Tenant, engine_error, refusal};

/// The most segments a table keeps before they are compacted.
pub(super) const COMPACT_AFTER: usize = 8;
/// The most rows written to the replacement at a time.
const WRITE_ROWS: usize = 64;

/// What one merge wrote and the most it held at once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Merged {
    /// Resolved versions written.
    pub rows: u64,
    /// The most row bytes held at once: read ahead, one identity's rows and
    /// the rows waiting to be written.
    pub peak: usize,
}

/// Compact `table`'s segments in `world` when there are more than
/// [`COMPACT_AFTER`]; `Ok(false)` when there was nothing to do.
pub(super) fn compact(
    engine: &Engine,
    tenant: &mut Tenant,
    table: &str,
    world: &str,
) -> Outcome<bool> {
    let (schema, segments) = tenant.catalog.read(|tx| {
        Ok((
            mirror::parse_schema(catalog::get(tx, &format!("schema:{table}"))?)?,
            catalog::segments(tx, table, world)?,
        ))
    })?;
    let Some(last) = segments.last().map(|segment| segment.seq) else {
        return Ok(false);
    };
    if segments.len() <= COMPACT_AFTER {
        return Ok(false);
    }
    for segment in &segments {
        tenant.verify(&segment.path, &segment.sha256)?;
    }
    let staged = tenant.staging().join(format!("compact-{last}.parquet"));
    let _stale = std::fs::remove_file(&staged);
    let paths: Vec<PathBuf> = segments.iter().map(|s| s.path.clone()).collect();
    if let Err(refused) = merge(engine.memory, &schema, last, &paths, &staged) {
        let _partial = std::fs::remove_file(&staged);
        return Err(refused);
    }
    let (path, sha256, _bytes) = catalog::install(&staged, &tenant.objects())?;
    let replacement = Segment {
        seq: last,
        path,
        sha256,
    };
    let published = tenant.catalog.write(|tx| {
        if catalog::segments(tx, table, world)? != segments {
            return Err(refusal("analytics.frontier_moved", "the segments changed"));
        }
        catalog::clear_segments(tx, table, world)?;
        catalog::add_segment(tx, table, world, &replacement)
    });
    if let Err(refused) = published {
        mirror::remove_unnamed(tenant, &replacement.path)?;
        return Err(refused);
    }
    // The replaced objects are named by nothing now (unless another record
    // names the same bytes): remove them.
    for segment in &segments {
        if segment.path != replacement.path {
            mirror::remove_unnamed(tenant, &segment.path)?;
        }
    }
    Ok(true)
}

/// The bytes a segment row holds in memory, roughly: its texts and cells.
fn row_bytes(row: &SegmentRow) -> usize {
    let version = &row.version;
    version.cells.iter().fold(
        version
            .id
            .len()
            .saturating_add(version.kind.len())
            .saturating_add(version.scope.len())
            .saturating_add(version.model.len())
            .saturating_add(64),
        |total, cell| total.saturating_add(cell.as_str().map_or(16, str::len)),
    )
}

/// One segment being merged: its reader, the rows read ahead, and the last
/// identity read (each segment must be in identity order).
struct Input {
    reader: ParquetRecordBatchReader,
    ahead: VecDeque<(u64, SegmentRow)>,
    bytes: usize,
    last: Option<String>,
    path: PathBuf,
}

impl Input {
    /// Read the next batch when nothing is ahead.
    fn fill(&mut self, table: &TableSchema) -> Outcome<()> {
        if !self.ahead.is_empty() {
            return Ok(());
        }
        let Some(batch) = self
            .reader
            .next()
            .transpose()
            .map_err(|error| engine_error(error.to_string()))?
        else {
            return Ok(());
        };
        for (seq, row) in mirror::read_rows(&batch, table)? {
            if self
                .last
                .as_deref()
                .is_some_and(|last| last.as_bytes() > row.version.id.as_bytes())
            {
                return Err(engine_error(format!(
                    "the segment {} is not in identity order",
                    self.path.display()
                )));
            }
            self.last = Some(row.version.id.clone());
            self.bytes = self.bytes.saturating_add(row_bytes(&row));
            self.ahead.push_back((seq, row));
        }
        Ok(())
    }

    /// The identity of the next row, reading ahead as needed.
    fn front(&mut self, table: &TableSchema) -> Outcome<Option<&str>> {
        self.fill(table)?;
        Ok(self.ahead.front().map(|(_, row)| row.version.id.as_str()))
    }

    fn pop(&mut self) -> Option<(u64, SegmentRow)> {
        let popped = self.ahead.pop_front()?;
        self.bytes = self.bytes.saturating_sub(row_bytes(&popped.1));
        Some(popped)
    }
}

/// #1765: merge the segments at `paths` (in segment order, each in identity
/// order) into the resolved segment `out`, numbered `seq`, holding at most
/// a bounded share of `memory` at once.
pub(super) fn merge(
    memory: usize,
    table: &TableSchema,
    seq: u64,
    paths: &[PathBuf],
    out: &Path,
) -> Outcome<Merged> {
    let identity_bound = memory.div_euclid(4).max(1);
    let share = memory.div_euclid(8).div_euclid(paths.len().max(1)).max(1);
    let mut inputs = paths
        .iter()
        .map(|path| {
            Ok(Input {
                reader: super::merge::reader(path, share)?,
                ahead: VecDeque::new(),
                bytes: 0,
                last: None,
                path: path.clone(),
            })
        })
        .collect::<Outcome<Vec<_>>>()?;
    let schema = mirror::segment_schema(table);
    let mut sink = Sink::bounded(out, &schema, memory.div_euclid(8).max(1))?;
    let mut merged = Merged { rows: 0, peak: 0 };
    let mut pending: Vec<SegmentRow> = Vec::with_capacity(WRITE_ROWS);
    let mut pending_bytes = 0_usize;
    loop {
        let mut least: Option<String> = None;
        for input in &mut inputs {
            if let Some(id) = input.front(table)?
                && least
                    .as_deref()
                    .is_none_or(|least| id.as_bytes() < least.as_bytes())
            {
                least = Some(id.to_owned());
            }
        }
        let Some(id) = least else { break };
        // Every row of this identity, segment by segment.
        let mut rows = Vec::new();
        let mut held = 0_usize;
        for input in &mut inputs {
            while input.front(table)? == Some(id.as_str()) {
                if let Some(row) = input.pop() {
                    held = held.saturating_add(row_bytes(&row.1));
                    rows.push(row);
                }
                if held > identity_bound {
                    return Err(refusal(
                        EXHAUSTED,
                        "an identity's history is larger than the compaction's bound",
                    ));
                }
            }
        }
        let ahead = inputs
            .iter()
            .fold(0_usize, |total, input| total.saturating_add(input.bytes));
        merged.peak = merged
            .peak
            .max(ahead.saturating_add(held).saturating_add(pending_bytes));
        for version in mirror::intervals(rows) {
            let row = resolved(version, merged.rows);
            pending_bytes = pending_bytes.saturating_add(row_bytes(&row));
            pending.push(row);
            merged.rows = merged.rows.saturating_add(1);
            if pending.len() >= WRITE_ROWS || pending_bytes >= share {
                sink.write(&mirror::segment_batch(&schema, table, seq, &pending)?)?;
                pending.clear();
                pending_bytes = 0;
            }
        }
    }
    if !pending.is_empty() {
        sink.write(&mirror::segment_batch(&schema, table, seq, &pending)?)?;
    }
    sink.finish()?;
    Ok(merged)
}

/// A resolved version as the `ord`-th row of a compacted segment.
fn resolved(version: Version, ord: u64) -> SegmentRow {
    SegmentRow {
        ord,
        tomb: false,
        resolved: true,
        version,
    }
}
