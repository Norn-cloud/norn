//! QUERY-ENGINE-1 on the adapter: the rows a read sees. A mirror source is
//! the versions current at its frontier, read from one catalog snapshot (its
//! model, schema, frontier and segments) and bound to the generation the cell
//! prepared; a dataset source is its stored load less the rows a rule holds
//! back. Filters are typed against the admitted schema before any row is
//! read, and evaluated exactly per row.

use std::collections::{BTreeMap, BTreeSet};

use super::super::backend::{ReadSnapshot, SnapshotProvenance};
use super::super::{
    AsOf, Column, ColumnKind, Filter, Literal, Source, TableSchema, TenantId, Test, TimeLink,
};
use super::dataset;
use super::mirror::{self, Version};
use super::values::{Cell, Dec};
use super::{Engine, Outcome, STALE, Tenant, UNSUPPORTED, engine_error, refusal};

/// One row a read sees.
#[derive(Debug, Clone)]
pub(super) struct Row {
    pub id: String,
    pub v: u64,
    pub cells: Vec<Cell>,
    pub scope: String,
    pub bad: bool,
    pub model: String,
}

impl From<Version> for Row {
    fn from(version: Version) -> Self {
        Self {
            id: version.id,
            v: version.v,
            cells: version.cells,
            scope: version.scope,
            bad: version.bad,
            model: version.model,
        }
    }
}

/// The rows a read sees: a mirror at a frontier, or a dataset's load.
pub(super) struct Scope {
    pub columns: Vec<Column>,
    /// Every row of the source, excluded identities included.
    pub all: Vec<Row>,
    /// Whether each row of `all` is read (not excluded, not held back).
    pub read: Vec<bool>,
    /// Every version known at the frontier, superseded ones included (mirror only).
    pub versions: Option<Vec<Row>>,
    pub excluded: Vec<String>,
    /// The mirror's world and the frontier read (mirror only).
    pub at: Option<(String, u64)>,
    /// At a knowledge cutoff, the model every version read must be of.
    pub model: Option<String>,
    snapshot: ReadSnapshot,
}

impl Scope {
    /// The rows of `source`, refusing a mirror behind its frontier or of
    /// another generation than `expected_model_pin` (when given).
    pub(super) fn new(
        engine: &Engine,
        tenant: &mut Tenant,
        source: &Source,
        expected_model_pin: Option<&str>,
    ) -> Outcome<Self> {
        match source {
            Source::Mirror {
                world,
                table,
                frontier,
                scope,
                excluded,
                model,
            } => {
                let reached = tenant
                    .catalog
                    .meta(&format!("frontier:{world}"))?
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(0);
                if reached < *frontier {
                    return Err(refusal(
                        "analytics.frontier_behind",
                        format!("the mirror is at {reached}"),
                    ));
                }
                let schema = mirror::schema_of(tenant, &format!("schema:{table}"))?;
                let model_pin = tenant
                    .catalog
                    .meta("model_pin")?
                    .filter(|pin| expected_model_pin.is_none_or(|expected| pin == expected))
                    .ok_or_else(|| refusal(STALE, "the mirror is of another model"))?;
                let versions = mirror::versions(engine, tenant, &schema, world)?;
                let all: Vec<Row> = versions
                    .iter()
                    .filter(|v| v.current_at(*frontier))
                    .cloned()
                    .map(Row::from)
                    .collect();
                let excluded_set: BTreeSet<&str> = excluded.iter().map(String::as_str).collect();
                let read = all
                    .iter()
                    .map(|row| !excluded_set.contains(row.id.as_str()))
                    .collect();
                let known: Vec<Row> = versions
                    .into_iter()
                    .filter(|v| v.from <= *frontier)
                    .map(Row::from)
                    .collect();
                Ok(Self {
                    columns: schema.columns,
                    all,
                    read,
                    versions: Some(known),
                    excluded: excluded.clone(),
                    at: Some((world.clone(), *frontier)),
                    model: model.clone(),
                    snapshot: ReadSnapshot::Mirror {
                        world: world.clone(),
                        table: table.clone(),
                        frontier: *frontier,
                        scope: scope.clone(),
                        model_pin,
                        schema_pin: schema.pin,
                    },
                })
            }
            Source::Dataset { table, load } => {
                let dataset = dataset::dataset(engine, tenant, table, load)?;
                Ok(Self {
                    columns: dataset.table.columns,
                    read: dataset
                        .rows
                        .iter()
                        .map(|row| !dataset.held.contains(&row.row))
                        .collect(),
                    all: dataset
                        .rows
                        .into_iter()
                        .map(|row| Row {
                            id: row.row.to_string(),
                            v: 1,
                            cells: row.cells,
                            scope: String::new(),
                            bad: false,
                            model: String::new(),
                        })
                        .collect(),
                    versions: None,
                    excluded: Vec::new(),
                    at: None,
                    model: None,
                    snapshot: ReadSnapshot::Dataset {
                        table: table.clone(),
                        load: load.clone(),
                    },
                })
            }
        }
    }

    /// The rows a question reads.
    pub(super) fn population(&self) -> impl Iterator<Item = &Row> {
        self.all
            .iter()
            .zip(&self.read)
            .filter_map(|(row, read)| read.then_some(row))
    }

    /// The answer's provenance: this engine, exact, over this snapshot.
    pub(super) fn provenance(&self, tenant: &TenantId) -> SnapshotProvenance {
        SnapshotProvenance {
            engine: super::ENGINE_ID,
            tenant: tenant.clone(),
            snapshot: self.snapshot.clone(),
            freshness: crate::compute::Freshness::Exact,
        }
    }

    /// A column by id, and its index.
    pub(super) fn column(&self, id: &str) -> Outcome<(usize, &Column)> {
        self.columns
            .iter()
            .enumerate()
            .find(|(_, c)| c.id == id)
            .ok_or_else(|| refusal(UNSUPPORTED, format!("no column {id}")))
    }

    /// The excluded identities present at the frontier, in byte order.
    pub(super) fn excluded_present(&self) -> Vec<String> {
        if self.excluded.is_empty() {
            return Vec::new();
        }
        let excluded: BTreeSet<&str> = self.excluded.iter().map(String::as_str).collect();
        let mut present: Vec<String> = self
            .all
            .iter()
            .filter(|row| excluded.contains(row.id.as_str()))
            .map(|row| row.id.clone())
            .collect();
        present.sort_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
        present
    }

    /// Enforce a mirror's row scope, stored checks and knowledge-time model
    /// on every row of the source, as an evaluate does.
    pub(super) fn verify_rows(&self, source: &Source, bad_refuses: bool) -> Outcome<()> {
        let Source::Mirror { scope, .. } = source else {
            return Ok(());
        };
        if self.all.iter().any(|row| row.scope != *scope) {
            return Err(refusal(STALE, "a row of another scope"));
        }
        if bad_refuses && self.all.iter().any(|row| row.bad) {
            return Err(refusal(UNSUPPORTED, "a stored value failed its check"));
        }
        if let Some(model) = &self.model
            && self.all.iter().any(|row| row.model != *model)
        {
            return Err(refusal(UNSUPPORTED, "a version of another model"));
        }
        Ok(())
    }
}

/// A filter typed against its column: the literal already in the column's
/// storage domain.
pub(super) enum Typed {
    IsNull,
    Text(String),
    Bool(bool),
    Number(Dec),
    Instant(i64, i32),
    Never,
    RefusePresent,
}

/// One typed filter over a column index.
pub(super) struct TypedFilter {
    pub index: usize,
    pub negate: bool,
    pub test: Typed,
}

/// Type `filters` against the scope (unknown columns and literals of
/// another kind refuse before any row is read).
pub(super) fn type_filters(scope: &Scope, filters: &[Filter]) -> Outcome<Vec<TypedFilter>> {
    filters
        .iter()
        .map(|filter| {
            let (index, column) = scope.column(&filter.column)?;
            let test = match &filter.test {
                Test::IsNull => Typed::IsNull,
                Test::Equals(literal) => match (literal, column.kind) {
                    (Literal::Text(text), ColumnKind::Text | ColumnKind::Date) => {
                        Typed::Text(text.clone())
                    }
                    (Literal::Bool(flag), ColumnKind::Boolean) => Typed::Bool(*flag),
                    (Literal::Number(text), ColumnKind::Integer) => Typed::Number(
                        Dec::cast(text, 0).ok_or_else(|| engine_error("a literal out of range"))?,
                    ),
                    (Literal::Number(text), ColumnKind::Decimal { scale }) => Typed::Number(
                        Dec::cast(text, scale)
                            .ok_or_else(|| engine_error("a literal out of range"))?,
                    ),
                    _ => return Err(refusal(UNSUPPORTED, "a literal of another kind")),
                },
                Test::Instant { secs, nanos } => {
                    Typed::Instant(*secs, i32::try_from(*nanos).unwrap_or(i32::MAX))
                }
                Test::NeverPresent => Typed::Never,
                Test::RefusePresent => Typed::RefusePresent,
            };
            Ok(TypedFilter {
                index,
                negate: filter.negate,
                test,
            })
        })
        .collect()
}

/// Sequential filters over a row: whether they all hold, and whether
/// reaching one refuses (a later filter is reached only when every earlier
/// one held).
pub(super) fn filters_hold(filters: &[TypedFilter], cells: &[Cell]) -> (bool, bool) {
    let mut holds = true;
    let mut refuse = false;
    for filter in filters {
        let cell = cells.get(filter.index).unwrap_or(&Cell::Null);
        let (test, refuses) = match &filter.test {
            Typed::IsNull => (!cell.is_present(), false),
            Typed::Text(text) => (cell.as_str() == Some(text.as_str()), false),
            Typed::Bool(flag) => (matches!(cell, Cell::Bool(b) if b == flag), false),
            Typed::Number(bound) => (
                matches!(cell, Cell::Dec(d) if d.compare(*bound).is_eq()),
                false,
            ),
            Typed::Instant(t, n) => (cell.instant() == Some((*t, *n)), false),
            Typed::Never => (false, false),
            Typed::RefusePresent => (false, cell.is_present()),
        };
        refuse = refuse || (holds && refuses);
        holds = holds && (test != filter.negate);
    }
    (holds, refuse)
}

/// The versions a snapshot picks, each with its period's label, and whether
/// a subject has a version without its valid time.
pub(super) type Picked = (Vec<(Option<String>, Row)>, bool);

/// METRIC-SNAPSHOT-ASOF-1: per period, each current subject's last version
/// known at the frontier whose valid time is before the period's end (ties
/// to the later version), labelled; and whether a subject has a version
/// without its valid time (which refuses the question).
pub(super) fn as_of_versions(scope: &Scope, as_of: &AsOf) -> Outcome<Picked> {
    let versions = scope
        .versions
        .as_ref()
        .ok_or_else(|| refusal(UNSUPPORTED, "a snapshot over a dataset"))?;
    let (index, column) = scope.column(&as_of.column)?;
    if !matches!(column.kind, ColumnKind::Timestamp | ColumnKind::Date) {
        return Err(refusal(UNSUPPORTED, "a snapshot's time is not an instant"));
    }
    if as_of.periods.is_empty() {
        return Err(refusal(UNSUPPORTED, "a snapshot without periods"));
    }
    let subjects: BTreeSet<&str> = scope.population().map(|row| row.id.as_str()).collect();
    let mut by_subject: BTreeMap<&str, Vec<&Row>> = BTreeMap::new();
    for version in versions.iter().filter(|v| subjects.contains(v.id.as_str())) {
        by_subject
            .entry(version.id.as_str())
            .or_default()
            .push(version);
    }
    let untimed = by_subject
        .values()
        .flatten()
        .any(|version| !version.cells.get(index).is_some_and(Cell::is_present));
    let mut picked = Vec::new();
    for (label, (secs, nanos)) in &as_of.periods {
        let end = (*secs, i64::from(*nanos));
        for candidates in by_subject.values() {
            let best = candidates
                .iter()
                .filter_map(|version| {
                    let (t, n) = version.cells.get(index)?.instant()?;
                    ((t, i64::from(n)) < end).then_some(((t, n, version.v), *version))
                })
                .max_by(|a, b| a.0.cmp(&b.0));
            if let Some((_, version)) = best {
                picked.push((label.clone(), version.clone()));
            }
        }
    }
    Ok((picked, untimed))
}

/// METRIC-GRAIN-LINK-1: a linked time reads a mirrored link and an instant
/// of its target, at a frontier no edge of the link changed after.
pub(super) fn check_link(tenant: &Tenant, scope: &Scope, link: &TimeLink) -> Outcome<()> {
    let Some((world, frontier)) = &scope.at else {
        return Err(refusal(UNSUPPORTED, "a linked time over a dataset"));
    };
    let known = tenant.catalog.meta("links")?.unwrap_or_default();
    if !known.split('\n').any(|name| name == link.link) {
        return Err(refusal(UNSUPPORTED, format!("no link table {}", link.link)));
    }
    let target = mirror::schema_of(tenant, &format!("schema:{}", link.target))?;
    if !target.columns.iter().any(|column| {
        column.id == link.column && matches!(column.kind, ColumnKind::Timestamp | ColumnKind::Date)
    }) {
        return Err(refusal(UNSUPPORTED, "a linked time that is not an instant"));
    }
    if mirror::edges(tenant, &link.link, world)?
        .iter()
        .any(|edge| edge.commit > *frontier)
    {
        return Err(refusal(STALE, "a link changed after the answer's frontier"));
    }
    Ok(())
}

/// One row's linked time: exactly one live edge to an object current then
/// (`one`), more than one edge (`many`), and the target's instant.
#[derive(Debug, Clone, Default)]
pub(super) struct Linked {
    pub one: bool,
    pub many: bool,
    pub time: Option<Cell>,
}

/// The linked time of every source identity, from one catalog snapshot.
pub(super) fn linked_times(
    engine: &Engine,
    tenant: &Tenant,
    scope: &Scope,
    link: &TimeLink,
) -> Outcome<BTreeMap<String, Linked>> {
    let Some((world, frontier)) = &scope.at else {
        return Err(refusal(UNSUPPORTED, "a linked time over a dataset"));
    };
    let target: TableSchema = mirror::schema_of(tenant, &format!("schema:{}", link.target))?;
    let index = target
        .columns
        .iter()
        .position(|column| column.id == link.column)
        .ok_or_else(|| refusal(UNSUPPORTED, "a linked time that is not an instant"))?;
    let targets: BTreeMap<String, Cell> = mirror::versions(engine, tenant, &target, world)?
        .into_iter()
        .filter(|version| version.current_at(*frontier))
        .map(|version| {
            let cell = version.cells.get(index).cloned().unwrap_or(Cell::Null);
            (version.id, cell)
        })
        .collect();
    let mut edges: BTreeMap<String, (u64, String)> = BTreeMap::new();
    for edge in mirror::edges(tenant, &link.link, world)?
        .into_iter()
        .filter(|edge| edge.live)
    {
        let entry = edges
            .entry(edge.source)
            .or_insert_with(|| (0, edge.target.clone()));
        entry.0 = entry.0.saturating_add(1);
        if edge.target.as_bytes() < entry.1.as_bytes() {
            entry.1 = edge.target;
        }
    }
    Ok(edges
        .into_iter()
        .map(|(source, (count, first))| {
            let found = targets.get(&first);
            (
                source,
                Linked {
                    one: count == 1 && found.is_some(),
                    many: count > 1,
                    time: found.cloned(),
                },
            )
        })
        .collect())
}
