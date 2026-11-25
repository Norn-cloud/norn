//! ANALYTICS-FRONTIER-1 on the adapter: the governed-fact mirror. Each sync
//! batch of a table is one immutable segment (every staged version, its
//! tombstones and its order within the batch); a version's commit interval
//! is reconstructed from the segments exactly as the `DuckDB` engine closes
//! it: within a batch by the next staged version of its identity (by
//! version, then batch order), across batches by the earliest commit of the
//! identity's next batch. The batch publishes with its frontier in one
//! catalog commit, after its segments are installed, so a refused or
//! crashed sync exposes nothing new.
//!
//! Segments are compacted ([`super::compact`]): once a table has more than a
//! few, they are replaced by one segment of resolved versions (each with its
//! interval written down), so a read decodes the versions it needs, pruned
//! by the frontier, not every batch ever synced.

use std::collections::BTreeMap;
use std::sync::Arc;

use datafusion::arrow::array::{ArrayRef, BooleanArray, StringArray, UInt64Array};
use datafusion::arrow::datatypes::{DataType, Field};
use datafusion::logical_expr::{Expr, lit};

use super::super::{FrontierRequest, FrontierResponse, MirrorRow, SyncRequest, TableSchema};
use super::catalog::{self, Edge, Segment};
use super::columnar::{self, bool_at, text_at, u64_at};
use super::scan::col;
use super::values::Cell;
use super::{Engine, Outcome, STALE, Tenant, UNSUPPORTED, engine_error, refusal};

/// One mirrored version with its commit interval.
#[derive(Debug, Clone)]
pub(super) struct Version {
    pub id: String,
    pub kind: String,
    pub v: u64,
    pub from: u64,
    pub to: Option<u64>,
    pub scope: String,
    pub bad: bool,
    pub model: String,
    pub cells: Vec<Cell>,
}

impl Version {
    /// Current at `frontier`: committed by it and not superseded by it.
    pub(super) fn current_at(&self, frontier: u64) -> bool {
        self.from <= frontier && self.to.is_none_or(|to| to > frontier)
    }
}

/// Which versions a read needs.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Need {
    /// Those current at the frontier.
    Current(u64),
    /// Every version known at the frontier, superseded ones included.
    History(u64),
    /// Every version (compaction).
    All,
}

fn schema_text(table: &TableSchema) -> Outcome<String> {
    serde_json::to_string(table).map_err(|e| engine_error(e.to_string()))
}

/// A recorded schema.
pub(super) fn parse_schema(text: Option<String>) -> Outcome<TableSchema> {
    let text = text.ok_or_else(|| refusal(UNSUPPORTED, "no such table"))?;
    serde_json::from_str(&text).map_err(|e| engine_error(e.to_string()))
}

/// The schema the catalog records under `key`.
pub(super) fn schema_of(tenant: &Tenant, key: &str) -> Outcome<TableSchema> {
    parse_schema(tenant.catalog.meta(key)?)
}

/// ANALYTICS-FRONTIER-1: the frontier, after a reset if the model, a schema
/// or the link set changed.
pub(super) fn frontier(
    tenant: &mut Tenant,
    request: &FrontierRequest,
) -> Outcome<FrontierResponse> {
    let links = request.links.join("\n");
    let mut stale = tenant.catalog.meta("links")?.unwrap_or_default() != links;
    stale |= tenant.catalog.meta("model_pin")?.as_deref() != Some(request.model_pin.as_str());
    for table in &request.tables {
        stale |= tenant
            .catalog
            .meta(&format!("pin:{}", table.name))?
            .as_deref()
            != Some(table.pin.as_str());
    }
    if stale {
        tenant.catalog.write(|tx| {
            catalog::clear_mirror(tx)?;
            for table in &request.tables {
                catalog::set(tx, &format!("pin:{}", table.name), &table.pin)?;
                catalog::set(tx, &format!("schema:{}", table.name), &schema_text(table)?)?;
            }
            catalog::set(tx, "links", &links)?;
            catalog::set(tx, "model_pin", &request.model_pin)
        })?;
    }
    let frontier = tenant
        .catalog
        .meta(&format!("frontier:{}", request.world))?
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    Ok(FrontierResponse { frontier })
}

fn system_fields() -> Vec<Field> {
    let u64f = |name: &str| Field::new(name, DataType::UInt64, false);
    let text = |name: &str| Field::new(name, DataType::Utf8, false);
    vec![
        u64f(":seg"),
        u64f(":ord"),
        text(":id"),
        u64f(":v"),
        u64f(":from"),
        Field::new(":to", DataType::UInt64, true),
        Field::new(":resolved", DataType::Boolean, false),
        text(":scope"),
        text(":type"),
        Field::new(":bad", DataType::Boolean, false),
        Field::new(":tomb", DataType::Boolean, false),
        text(":model"),
    ]
}

/// One row of a segment: a staged version (or tombstone) of a sync batch,
/// or a resolved version of a compacted segment.
pub(super) struct SegmentRow {
    pub ord: u64,
    pub tomb: bool,
    pub resolved: bool,
    pub version: Version,
}

/// Write `rows` as a finished, synced segment file `name` in staging.
pub(super) fn write_segment(
    tenant: &Tenant,
    table: &TableSchema,
    seq: u64,
    rows: &[SegmentRow],
    name: &str,
) -> Outcome<std::path::PathBuf> {
    let schema = columnar::storage_schema(system_fields(), table);
    let count = rows.len();
    let texts = |f: fn(&SegmentRow) -> &str| -> ArrayRef {
        Arc::new(StringArray::from_iter_values(rows.iter().map(f)))
    };
    let mut arrays: Vec<ArrayRef> = vec![
        Arc::new(UInt64Array::from(vec![seq; count])),
        Arc::new(UInt64Array::from_iter_values(rows.iter().map(|r| r.ord))),
        texts(|r| r.version.id.as_str()),
        Arc::new(UInt64Array::from_iter_values(
            rows.iter().map(|r| r.version.v),
        )),
        Arc::new(UInt64Array::from_iter_values(
            rows.iter().map(|r| r.version.from),
        )),
        Arc::new(UInt64Array::from(
            rows.iter().map(|r| r.version.to).collect::<Vec<_>>(),
        )),
        Arc::new(BooleanArray::from(
            rows.iter().map(|r| r.resolved).collect::<Vec<_>>(),
        )),
        texts(|r| r.version.scope.as_str()),
        texts(|r| r.version.kind.as_str()),
        Arc::new(BooleanArray::from(
            rows.iter().map(|r| r.version.bad).collect::<Vec<_>>(),
        )),
        Arc::new(BooleanArray::from(
            rows.iter().map(|r| r.tomb).collect::<Vec<_>>(),
        )),
        texts(|r| r.version.model.as_str()),
    ];
    for (index, column) in table.columns.iter().enumerate() {
        let cells: Vec<&Cell> = rows
            .iter()
            .map(|row| row.version.cells.get(index).unwrap_or(&Cell::Null))
            .collect();
        arrays.extend(columnar::column_arrays(column, &cells)?);
    }
    let batch = columnar::batch(&schema, arrays)?;
    let staged = tenant.staging().join(name);
    let _stale = std::fs::remove_file(&staged);
    columnar::write_parquet(&staged, &schema, &[batch])?;
    Ok(staged)
}

/// A sync batch's rows of one table as segment rows; a number its column
/// cannot store refuses the batch (B5), as the storage cast does.
fn staged_rows(table: &TableSchema, rows: &[&MirrorRow]) -> Outcome<Vec<SegmentRow>> {
    rows.iter()
        .enumerate()
        .map(|(ord, row)| {
            let cells = match &row.values {
                None => vec![Cell::Null; table.columns.len()],
                Some(values) if values.len() == table.columns.len() => table
                    .columns
                    .iter()
                    .zip(values)
                    .map(|(column, value)| Cell::of_value(column.kind, value))
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(engine_error)?,
                Some(_) => return Err(engine_error("values do not match the table")),
            };
            Ok(SegmentRow {
                ord: u64::try_from(ord).unwrap_or(u64::MAX),
                tomb: row.values.is_none(),
                resolved: false,
                version: Version {
                    id: row.id.clone(),
                    kind: row.object_type.clone(),
                    v: row.version,
                    from: row.commit_seq,
                    to: None,
                    scope: row.scope.clone(),
                    bad: row.bad,
                    model: row.model.clone(),
                    cells,
                },
            })
        })
        .collect()
}

/// Check the batch against the catalog: it continues the frontier and is of
/// the mirror's generation (model, every table's schema, the link set).
fn check_batch(tx: &rusqlite::Connection, request: &SyncRequest) -> Outcome<()> {
    let current = catalog::frontier(tx, &request.world)?;
    if current != request.after || request.through < request.after {
        return Err(refusal(
            "analytics.frontier_moved",
            format!("the mirror is at {current}"),
        ));
    }
    let mut same = catalog::get(tx, "model_pin")?.as_deref() == Some(request.model_pin.as_str())
        && request
            .rows
            .iter()
            .all(|row| request.schema_pins.contains_key(&row.table));
    for (table, pin) in &request.schema_pins {
        same &= catalog::get(tx, &format!("pin:{table}"))?.as_deref() == Some(pin.as_str());
    }
    if !same {
        return Err(refusal(STALE, "the batch is of another mirror generation"));
    }
    let known = catalog::get(tx, "links")?.unwrap_or_default();
    for edge in &request.links {
        if !known.split('\n').any(|name| name == edge.table) {
            return Err(refusal(
                UNSUPPORTED,
                format!("no link table {}", edge.table),
            ));
        }
        if edge.commit_seq <= request.after {
            return Err(refusal(
                "analytics.frontier_moved",
                "an edge outside the batch",
            ));
        }
    }
    Ok(())
}

/// Apply one sync batch: stage and install each table's segment, then
/// publish segments, edges and the frontier in one catalog commit that
/// rechecks everything the batch was staged against (N7), then compact.
pub(super) fn sync(
    engine: &Engine,
    tenant: &mut Tenant,
    request: &SyncRequest,
) -> Outcome<FrontierResponse> {
    tenant.catalog.read(|tx| check_batch(tx, request))?;
    let mut by_table: BTreeMap<&str, Vec<&MirrorRow>> = BTreeMap::new();
    for row in &request.rows {
        if row.commit_seq <= request.after || row.commit_seq > request.through {
            return Err(refusal(
                "analytics.frontier_moved",
                "a row outside the batch",
            ));
        }
        by_table.entry(row.table.as_str()).or_default().push(row);
    }
    let mut installed = Vec::new();
    for (name, rows) in &by_table {
        let table = schema_of(tenant, &format!("schema:{name}"))?;
        let rows = staged_rows(&table, rows)?;
        let seq = tenant
            .catalog
            .segments(name, &request.world)?
            .last()
            .map_or(1, |segment| segment.seq.saturating_add(1));
        let staged = write_segment(
            tenant,
            &table,
            seq,
            &rows,
            &format!("segment-{seq}.parquet"),
        )?;
        let (path, sha256, _bytes) = catalog::install(&staged, &tenant.objects())?;
        installed.push(((*name).to_owned(), Segment { seq, path, sha256 }));
    }
    let key = format!("frontier:{}", request.world);
    tenant.catalog.write(|tx| {
        // Nothing may have moved the mirror since the batch was checked.
        check_batch(tx, request)?;
        for (table, segment) in &installed {
            if catalog::next_segment(tx, table, &request.world)? != segment.seq {
                return Err(refusal("analytics.frontier_moved", "the mirror moved"));
            }
            catalog::add_segment(tx, table, &request.world, segment)?;
        }
        for edge in &request.links {
            let state = Edge {
                source: edge.source.clone(),
                target: edge.target.clone(),
                live: edge.live,
                commit: edge.commit_seq,
            };
            catalog::put_edge(tx, &edge.table, &request.world, &state)?;
        }
        catalog::set(tx, &key, &request.through.to_string())
    })?;
    for name in by_table.keys() {
        // Best effort: a compaction that cannot run leaves the mirror exact,
        // and the next sync tries again.
        let _compacted = super::compact::compact(engine, tenant, name, &request.world);
    }
    Ok(FrontierResponse {
        frontier: request.through,
    })
}

/// The scan filter of `need`: a resolved version outside it is never decoded.
fn pruned(need: Need) -> Option<Expr> {
    let unresolved = col(":resolved").eq(lit(false));
    match need {
        Need::All => None,
        Need::History(frontier) => Some(unresolved.or(col(":from").lt_eq(lit(frontier)))),
        Need::Current(frontier) => Some(
            unresolved.or(col(":from")
                .lt_eq(lit(frontier))
                .and(col(":to").is_null().or(col(":to").gt(lit(frontier))))),
        ),
    }
}

/// The versions of `table` that `need` reads, with their intervals, from
/// `segments` (one catalog snapshot), each verified against its digest.
pub(super) fn versions(
    engine: &Engine,
    tenant: &Tenant,
    table: &TableSchema,
    segments: &[Segment],
    need: Need,
) -> Outcome<Vec<Version>> {
    for segment in segments {
        tenant.verify(&segment.path, &segment.sha256)?;
    }
    let files: Vec<_> = segments
        .iter()
        .map(|segment| segment.path.clone())
        .collect();
    let mut staged = Vec::new();
    for batch in engine.scan(tenant, &files, pruned(need))? {
        staged.extend(read_rows(&batch, table)?);
    }
    let versions = intervals(staged);
    Ok(match need {
        Need::All => versions,
        Need::History(frontier) => versions
            .into_iter()
            .filter(|v| v.from <= frontier)
            .collect(),
        Need::Current(frontier) => versions
            .into_iter()
            .filter(|v| v.current_at(frontier))
            .collect(),
    })
}

/// The segment rows of one scanned batch, with their segment number.
fn read_rows(
    batch: &datafusion::arrow::record_batch::RecordBatch,
    table: &TableSchema,
) -> Outcome<Vec<(u64, SegmentRow)>> {
    let u = |name| columnar::system(batch, name, u64_at);
    let t = |name| columnar::system(batch, name, text_at);
    let b = |name| columnar::system(batch, name, bool_at);
    let (seg, ord, v, from, to) = (u(":seg")?, u(":ord")?, u(":v")?, u(":from")?, u(":to")?);
    let (id, kind, scope, model) = (t(":id")?, t(":type")?, t(":scope")?, t(":model")?);
    let (resolved, bad, tomb) = (b(":resolved")?, b(":bad")?, b(":tomb")?);
    let columns = table
        .columns
        .iter()
        .map(|column| columnar::column_cells(batch, column))
        .collect::<Outcome<Vec<_>>>()?;
    let read = || engine_error("a segment row is incomplete");
    (0..batch.num_rows())
        .map(|row| {
            let at = |values: &[Option<u64>]| values.get(row).copied().flatten().ok_or_else(read);
            let text =
                |values: &[Option<String>]| values.get(row).cloned().flatten().ok_or_else(read);
            let flag =
                |values: &[Option<bool>]| values.get(row).copied().flatten().ok_or_else(read);
            Ok((
                at(&seg)?,
                SegmentRow {
                    ord: at(&ord)?,
                    tomb: flag(&tomb)?,
                    resolved: flag(&resolved)?,
                    version: Version {
                        id: text(&id)?,
                        kind: text(&kind)?,
                        v: at(&v)?,
                        from: at(&from)?,
                        to: to.get(row).copied().flatten(),
                        scope: text(&scope)?,
                        bad: flag(&bad)?,
                        model: text(&model)?,
                        cells: columns
                            .iter()
                            .map(|cells| cells.get(row).cloned().unwrap_or(Cell::Null))
                            .collect(),
                    },
                },
            ))
        })
        .collect()
}

/// The `DuckDB` engine's interval rules over segment rows in any order: a
/// resolved row keeps the interval written down; a staged row is closed by
/// the next staged version of its identity in its batch, or by the earliest
/// commit of the identity's next batch.
fn intervals(mut staged: Vec<(u64, SegmentRow)>) -> Vec<Version> {
    staged.sort_by(|(a_seg, a), (b_seg, b)| {
        (a_seg, a.version.id.as_bytes(), a.version.v, a.ord).cmp(&(
            b_seg,
            b.version.id.as_bytes(),
            b.version.v,
            b.ord,
        ))
    });
    let mut kept: Vec<Version> = Vec::new();
    let mut open: BTreeMap<String, usize> = BTreeMap::new();
    let mut start = 0;
    while let Some((first_seg, first)) = staged.get(start) {
        let end = staged
            .iter()
            .skip(start)
            .position(|(seg, row)| seg != first_seg || row.version.id != first.version.id)
            .map_or(staged.len(), |offset| start.saturating_add(offset));
        let group = staged.get(start..end).unwrap_or_default();
        let first_from = group.iter().map(|(_, row)| row.version.from).min();
        if let (Some(index), Some(from)) = (open.remove(&first.version.id), first_from)
            && let Some(version) = kept.get_mut(index)
        {
            version.to = Some(from);
        }
        for (offset, (_, row)) in group.iter().enumerate() {
            if row.tomb {
                continue;
            }
            let mut version = row.version.clone();
            if !row.resolved {
                version.to = group
                    .get(offset.saturating_add(1))
                    .map(|(_, next)| next.version.from);
            }
            if version.to.is_none() {
                open.insert(version.id.clone(), kept.len());
            }
            kept.push(version);
        }
        start = end;
    }
    kept
}

/// The edges of `link` in `world` the catalog holds now.
pub(super) fn edges(tenant: &Tenant, link: &str, world: &str) -> Outcome<Vec<Edge>> {
    tenant.catalog.edges(link, world)
}
