//! D137 (B2): mirror compaction. A table's segments grow by one per sync; once
//! more than [`COMPACT_AFTER`] are published, they are replaced by one
//! segment of resolved versions: every version they hold, each with the
//! commit interval the segments give it, tombstones dropped. The replacement
//! is installed first and published by one catalog commit that rechecks the
//! segments it replaces, so a reader sees the old segments or the new one,
//! and answers the same either way.

use super::catalog::{self, Segment};
use super::mirror::{self, Need, SegmentRow};
use super::{Engine, Outcome, Tenant, refusal};

/// The most segments a table keeps before they are compacted.
pub(super) const COMPACT_AFTER: usize = 8;

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
    let versions = mirror::versions(engine, tenant, &schema, &segments, Need::All)?;
    let rows: Vec<SegmentRow> = versions
        .into_iter()
        .enumerate()
        .map(|(ord, version)| SegmentRow {
            ord: u64::try_from(ord).unwrap_or(u64::MAX),
            tomb: false,
            resolved: true,
            version,
        })
        .collect();
    let staged = mirror::write_segment(
        tenant,
        &schema,
        last,
        &rows,
        &format!("compact-{last}.parquet"),
    )?;
    let (path, sha256, _bytes) = catalog::install(&staged, &tenant.objects())?;
    let replacement = Segment {
        seq: last,
        path,
        sha256,
    };
    tenant.catalog.write(|tx| {
        if catalog::segments(tx, table, world)? != segments {
            return Err(refusal("analytics.frontier_moved", "the segments changed"));
        }
        catalog::clear_segments(tx, table, world)?;
        catalog::add_segment(tx, table, world, &replacement)
    })?;
    // The replaced objects are named by nothing now (unless another world's
    // segment has the same bytes): remove them.
    for segment in &segments {
        if segment.path != replacement.path
            && !tenant
                .catalog
                .read(|tx| catalog::named(tx, &segment.path))?
        {
            let _removed = std::fs::remove_file(&segment.path);
        }
    }
    Ok(true)
}
