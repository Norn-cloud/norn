//! ANALYTICS-FRONTIER-1 on the adapter: the governed-fact mirror. Each sync
//! batch of a table is one immutable segment (every staged version, its
//! tombstones and its order within the batch); a version's commit interval
//! is reconstructed from the segments exactly as the `DuckDB` engine closes
//! it: within a batch by the next staged version of its identity (by
//! version, then batch order), across batches by the earliest commit of the
//! identity's next batch. The batch publishes with its frontier in one
//! catalog commit, after its segments are installed, so a refused or
//! crashed sync exposes nothing new.

use std::collections::BTreeMap;
use std::sync::Arc;

use datafusion::arrow::array::{ArrayRef, BooleanArray, StringArray, UInt64Array};
use datafusion::arrow::datatypes::{DataType, Field};

use super::super::{
    ColumnKind, FrontierRequest, FrontierResponse, MirrorRow, SyncRequest, TableSchema,
};
use super::catalog::{self, Edge, Segment};
use super::columnar::{self, bool_at, text_at, u64_at};
use super::values::Cell;
use super::{Engine, Outcome, STALE, Tenant, UNSUPPORTED, engine_error, refusal};

/// One mirrored version with its commit interval.
#[derive(Debug, Clone)]
pub(super) struct Version {
    pub id: String,
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

fn schema_text(table: &TableSchema) -> Outcome<String> {
    serde_json::to_string(table).map_err(|e| engine_error(e.to_string()))
}

/// The schema the catalog records under `key`.
pub(super) fn schema_of(tenant: &Tenant, key: &str) -> Outcome<TableSchema> {
    let text = tenant
        .catalog
        .meta(key)?
        .ok_or_else(|| refusal(UNSUPPORTED, "no such table"))?;
    serde_json::from_str(&text).map_err(|e| engine_error(e.to_string()))
}

/// ANALYTICS-FRONTIER-1: the frontier, after a reset if the model, a schema
/// or the link set changed.
pub(super) fn frontier(tenant: &mut Tenant, request: &FrontierRequest) -> Outcome<FrontierResponse> {
    let links = request.links.join("\n");
    let mut stale = tenant.catalog.meta("links")?.unwrap_or_default() != links;
    stale |= tenant.catalog.meta("model_pin")?.as_deref() != Some(request.model_pin.as_str());
    for table in &request.tables {
        stale |= tenant.catalog.meta(&format!("pin:{}", table.name))?.as_deref()
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
        text(":scope"),
        text(":type"),
        Field::new(":bad", DataType::Boolean, false),
        Field::new(":tomb", DataType::Boolean, false),
        text(":model"),
    ]
}

/// Write one table's staged rows as a finished, synced segment file in the
/// tenant's staging directory.
fn write_segment(
    tenant: &Tenant,
    table: &TableSchema,
    seq: u64,
    rows: &[&MirrorRow],
) -> Outcome<std::path::PathBuf> {
    let schema = columnar::storage_schema(system_fields(), table);
    let count = rows.len();
    let ords = (0..count).map(|ord| u64::try_from(ord).unwrap_or(u64::MAX));
    let mut arrays: Vec<ArrayRef> = vec![
        Arc::new(UInt64Array::from(vec![seq; count])),
        Arc::new(UInt64Array::from_iter_values(ords)),
        Arc::new(StringArray::from_iter_values(rows.iter().map(|r| r.id.as_str()))),
        Arc::new(UInt64Array::from_iter_values(rows.iter().map(|r| r.version))),
        Arc::new(UInt64Array::from_iter_values(rows.iter().map(|r| r.commit_seq))),
        Arc::new(StringArray::from_iter_values(rows.iter().map(|r| r.scope.as_str()))),
        Arc::new(StringArray::from_iter_values(
            rows.iter().map(|r| r.object_type.as_str()),
        )),
        Arc::new(BooleanArray::from(rows.iter().map(|r| r.bad).collect::<Vec<_>>())),
        Arc::new(BooleanArray::from(
            rows.iter().map(|r| r.values.is_none()).collect::<Vec<_>>(),
        )),
        Arc::new(StringArray::from_iter_values(rows.iter().map(|r| r.model.as_str()))),
    ];
    for (index, column) in table.columns.iter().enumerate() {
        let cells: Vec<Cell> = rows
            .iter()
            .map(|row| {
                row.values
                    .as_ref()
                    .and_then(|values| values.get(index))
                    .map_or(Cell::Null, |value| stored(column.kind, value))
            })
            .collect();
        arrays.extend(columnar::column_arrays(column, &cells.iter().collect::<Vec<_>>())?);
    }
    let batch = columnar::batch(&schema, arrays)?;
    let staged = tenant
        .staging()
        .join(format!("segment-{seq}-{}.parquet", std::process::id()));
    let _stale = std::fs::remove_file(&staged);
    columnar::write_parquet(&staged, &schema, &[batch])?;
    Ok(staged)
}

/// A mirrored value as its column stores it (an instant column holding a
/// value without its key keeps the text alone, as the `DuckDB` mirror does).
fn stored(kind: ColumnKind, value: &super::super::Value) -> Cell {
    Cell::of_value(kind, value)
}

/// Check the batch against the catalog: it continues the frontier and is of
/// the mirror's generation (model, every table's schema, the link set).
fn check_batch(tenant: &Tenant, request: &SyncRequest) -> Outcome<()> {
    let key = format!("frontier:{}", request.world);
    let current: u64 = tenant
        .catalog
        .meta(&key)?
        .and_then(|value| value.parse().ok())
        .unwrap_or(0);
    if current != request.after || request.through < request.after {
        return Err(refusal(
            "analytics.frontier_moved",
            format!("the mirror is at {current}"),
        ));
    }
    let mut same = tenant.catalog.meta("model_pin")?.as_deref() == Some(request.model_pin.as_str())
        && request
            .rows
            .iter()
            .all(|row| request.schema_pins.contains_key(&row.table));
    for (table, pin) in &request.schema_pins {
        same &= tenant.catalog.meta(&format!("pin:{table}"))?.as_deref() == Some(pin.as_str());
    }
    if !same {
        return Err(refusal(STALE, "the batch is of another mirror generation"));
    }
    let known = tenant.catalog.meta("links")?.unwrap_or_default();
    for edge in &request.links {
        if !known.split('\n').any(|name| name == edge.table) {
            return Err(refusal(UNSUPPORTED, format!("no link table {}", edge.table)));
        }
        if edge.commit_seq <= request.after {
            return Err(refusal("analytics.frontier_moved", "an edge outside the batch"));
        }
    }
    Ok(())
}

/// Apply one sync batch: stage and install each table's segment, then
/// publish segments, edges and the frontier in one catalog commit that
/// rechecks the frontier and generation it was staged against.
pub(super) fn sync(engine: &Engine, tenant: &mut Tenant, request: &SyncRequest) -> Outcome<FrontierResponse> {
    let _engine = engine;
    check_batch(tenant, request)?;
    let mut by_table: BTreeMap<&str, Vec<&MirrorRow>> = BTreeMap::new();
    for row in &request.rows {
        if row.commit_seq <= request.after || row.commit_seq > request.through {
            return Err(refusal("analytics.frontier_moved", "a row outside the batch"));
        }
        by_table.entry(row.table.as_str()).or_default().push(row);
    }
    let mut installed = Vec::new();
    for (name, rows) in &by_table {
        let table = schema_of(tenant, &format!("schema:{name}"))?;
        if rows
            .iter()
            .any(|row| row.values.as_ref().is_some_and(|v| v.len() != table.columns.len()))
        {
            return Err(engine_error("values do not match the table"));
        }
        let seq = tenant
            .catalog
            .segments(name, &request.world)?
            .last()
            .map_or(1, |segment| segment.seq.saturating_add(1));
        let staged = write_segment(tenant, &table, seq, rows)?;
        let (path, sha256, _bytes) = catalog::install(&staged, &tenant.objects())?;
        installed.push(((*name).to_owned(), Segment { seq, path, sha256 }));
    }
    let key = format!("frontier:{}", request.world);
    let model = request.model_pin.clone();
    let pins = request.schema_pins.clone();
    tenant.catalog.write(|tx| {
        // Recheck inside the write transaction what the batch was staged
        // against: nothing else may have moved the mirror meanwhile.
        if catalog::frontier(tx, &request.world)? != request.after
            || catalog::get(tx, "model_pin")?.as_deref() != Some(model.as_str())
        {
            return Err(refusal("analytics.frontier_moved", "the mirror moved"));
        }
        for (table, pin) in &pins {
            if catalog::get(tx, &format!("pin:{table}"))?.as_deref() != Some(pin.as_str()) {
                return Err(refusal(STALE, "the batch is of another mirror generation"));
            }
        }
        for (table, segment) in &installed {
            if catalog::next_segment(tx, table, &request.world)? != segment.seq {
                return Err(refusal("analytics.frontier_moved", "the mirror moved"));
            }
            catalog::add_segment(tx, table, &request.world, segment)?;
        }
        for edge in &request.links {
            catalog::put_edge(
                tx,
                &edge.table,
                &request.world,
                &Edge {
                    source: edge.source.clone(),
                    target: edge.target.clone(),
                    live: edge.live,
                    commit: edge.commit_seq,
                },
            )?;
        }
        catalog::set(tx, &key, &request.through.to_string())
    })?;
    Ok(FrontierResponse {
        frontier: request.through,
    })
}

/// One staged row as read back from a segment.
struct Staged {
    seg: u64,
    ord: u64,
    tomb: bool,
    version: Version,
}

/// Every version of `table` in `world` with its interval, from the segments
/// the catalog names now.
pub(super) fn versions(
    engine: &Engine,
    tenant: &Tenant,
    table: &TableSchema,
    world: &str,
) -> Outcome<Vec<Version>> {
    let segments = tenant.catalog.segments(&table.name, world)?;
    let files: Vec<_> = segments.iter().map(|segment| segment.path.clone()).collect();
    let mut staged = Vec::new();
    for batch in engine.scan(tenant, &files, None)? {
        let seg = columnar::system(&batch, ":seg", u64_at)?;
        let ord = columnar::system(&batch, ":ord", u64_at)?;
        let id = columnar::system(&batch, ":id", text_at)?;
        let v = columnar::system(&batch, ":v", u64_at)?;
        let from = columnar::system(&batch, ":from", u64_at)?;
        let scope = columnar::system(&batch, ":scope", text_at)?;
        let bad = columnar::system(&batch, ":bad", bool_at)?;
        let tomb = columnar::system(&batch, ":tomb", bool_at)?;
        let model = columnar::system(&batch, ":model", text_at)?;
        let mut columns = Vec::new();
        for column in &table.columns {
            columns.push(columnar::column_cells(&batch, column)?);
        }
        for row in 0..batch.num_rows() {
            let read = || engine_error("a segment row is incomplete");
            staged.push(Staged {
                seg: seg.get(row).copied().flatten().ok_or_else(read)?,
                ord: ord.get(row).copied().flatten().ok_or_else(read)?,
                tomb: tomb.get(row).copied().flatten().ok_or_else(read)?,
                version: Version {
                    id: id.get(row).cloned().flatten().ok_or_else(read)?,
                    v: v.get(row).copied().flatten().ok_or_else(read)?,
                    from: from.get(row).copied().flatten().ok_or_else(read)?,
                    to: None,
                    scope: scope.get(row).cloned().flatten().ok_or_else(read)?,
                    bad: bad.get(row).copied().flatten().ok_or_else(read)?,
                    model: model.get(row).cloned().flatten().ok_or_else(read)?,
                    cells: columns
                        .iter()
                        .map(|cells| cells.get(row).cloned().unwrap_or(Cell::Null))
                        .collect(),
                },
            });
        }
    }
    Ok(intervals(staged))
}

/// The `DuckDB` engine's interval rules over staged rows in any order.
fn intervals(mut staged: Vec<Staged>) -> Vec<Version> {
    staged.sort_by(|a, b| {
        (a.seg, a.version.id.as_bytes(), a.version.v, a.ord).cmp(&(
            b.seg,
            b.version.id.as_bytes(),
            b.version.v,
            b.ord,
        ))
    });
    let mut kept: Vec<Version> = Vec::new();
    let mut open: BTreeMap<String, usize> = BTreeMap::new();
    let mut start = 0;
    while let Some(first) = staged.get(start) {
        let end = staged
            .iter()
            .skip(start)
            .position(|row| row.seg != first.seg || row.version.id != first.version.id)
            .map_or(staged.len(), |offset| start.saturating_add(offset));
        let group = staged.get(start..end).unwrap_or_default();
        // Close the identity's open version at the batch's earliest commit.
        let first_from = group.iter().map(|row| row.version.from).min();
        if let (Some(index), Some(from)) = (open.remove(&first.version.id), first_from)
            && let Some(version) = kept.get_mut(index)
        {
            version.to = Some(from);
        }
        for (offset, row) in group.iter().enumerate() {
            let next = group.get(offset.saturating_add(1)).map(|n| n.version.from);
            if row.tomb {
                continue;
            }
            let mut version = row.version.clone();
            version.to = next;
            if next.is_none() {
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
