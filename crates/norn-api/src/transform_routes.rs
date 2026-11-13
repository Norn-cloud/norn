//! Typed transform reads (`ViewIR` v1, VIEW-READ-1, VIEW-ROWS-1): one
//! transform's rows, one row's lineage and its admitted plan.
//!
//! A transform is admitted against the cell's installed definitions on every
//! read, so the plan a reader sees is the one the checker admits now. Reading
//! rows needs the transform's collection grant and Population on every object
//! type it reads, decided before evaluation with one opaque refusal; each
//! input row then enters only with Population and Values, and a row the
//! reader may only count is excluded and counted. No physical table, SQL or
//! row locator crosses this boundary (#733).

use std::collections::{BTreeMap, BTreeSet};

use http::StatusCode;
use norn_cell::compute::materialize::{MaterializationState, Served};
use norn_cell::compute::transform::RowAccess;
use norn_cell::{CellStore, MAIN_WORLD, ObjectRecord, engine::SqlEngine};
use norn_checker::transform::{Column, Materialization, NodeSchema, Step, TransformPlan, admit};
use norn_ir::{ArtifactEnvelope, PropertyAdditivity, PropertyTypeName, RuntimeValue};
use norn_kernel::LogicalTime;
use serde::Serialize;
use utoipa::ToSchema;

use crate::authz::{
    ReadCapability::{Population, Values},
    ReadRequest, read_denied,
};
use crate::host::DevHost;
use crate::object_routes::{ObjectAccess, collection, mapped};
use crate::refusal::Refusal;
use crate::scope::ScopeQuery;

/// Rows on one page when `limit` is absent.
const DEFAULT_LIMIT: usize = 100;
/// The largest page.
const MAX_LIMIT: usize = 1_000;

/// The query string of `GET /v1/transforms/{transform}/rows`.
#[derive(Debug, Default, Clone, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct TransformRowsParams {
    /// Page size, 1 to 1,000; 100 when absent.
    #[param(value_type = Option<u32>, minimum = 1, maximum = 1000, nullable = false)]
    pub limit: Option<String>,
    /// The `nextAfter` of the previous page.
    #[param(nullable = false)]
    pub after: Option<String>,
}

/// Where a column's value comes from: one stored property of one input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornColumnOrigin {
    /// The input alias.
    pub input: String,
    /// The stored property, or `id`.
    pub property: String,
}

/// One typed column of a transform's row (VIEW-TYPE-1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornTransformColumn {
    /// The column name.
    pub name: String,
    /// The value type, as the checker names it (`decimal`, `text`, …).
    #[serde(rename = "type")]
    pub value_type: String,
    /// Whether a value may be null.
    pub nullable: bool,
    /// The aggregation contract: `undeclared`, `additive`, `semi_additive`,
    /// `non_additive` or `ratio`.
    pub additivity: String,
    /// The object type whose ids the column holds, if any.
    #[schema(required = true, nullable = true)]
    pub identity: Option<String>,
    /// Column lineage: every stored property the value derives from.
    pub origins: Vec<NornColumnOrigin>,
    /// Effective markings of the origins.
    pub markings: Vec<String>,
    /// The column of the same row holding this quantity's currency
    /// (VIEW-AGG-1): null when it has none or the row does not carry it.
    #[schema(required = true, nullable = true)]
    pub currency_from: Option<String>,
    /// The columns of the same row whose values together are this
    /// quantity's unit (METRIC-UNIT-PARTITION-1), in declaration order;
    /// empty when it has none or the row does not carry them.
    pub unit_from: Vec<String>,
    /// What readers call the column, as its definition labels it; null when
    /// the definition names it only by `name`.
    #[schema(required = true, nullable = true)]
    pub label: Option<String>,
}

/// One output row.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornTransformRow {
    /// The row's stable id: the content hash of its grain values.
    pub row_id: String,
    /// Every column value; decimals are exact JSON number tokens.
    #[schema(schema_with = row_values_schema)]
    pub values: BTreeMap<String, RuntimeValue>,
    /// How many input rows the row derives from (its lineage).
    pub member_count: u64,
}

/// One page of a transform's rows under the reader's visibility.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornTransformPage {
    /// The transform (its artifact id).
    pub transform: String,
    /// Its exact version.
    pub version: String,
    /// The evaluator: `cell` (exact) today.
    pub engine: String,
    /// Commits in the cell at the snapshot the rows were computed from: now,
    /// or a stored result's frontier.
    pub commits: u64,
    /// VIEW-MAT-1: the stored result these rows come from; null when they
    /// were evaluated live for this read.
    #[schema(required = true, nullable = true)]
    pub materialization: Option<NornMaterialization>,
    /// Output columns, in schema order.
    pub columns: Vec<NornTransformColumn>,
    /// The columns that identify a row.
    pub grain: Vec<String>,
    /// This page's rows, ordered by grain (nulls last).
    pub rows: Vec<NornTransformRow>,
    /// Rows across all pages.
    pub total_rows: u64,
    /// Input rows the reader may count but not read, excluded, by input alias.
    pub withheld: BTreeMap<String, u64>,
    /// Input rows excluded by open quarantining findings.
    pub excluded: u64,
    /// No input row was withheld or excluded.
    pub complete: bool,
    /// Rows each plan node produced for this read, by node id: the plan's
    /// volume under the reader's visibility.
    pub node_rows: BTreeMap<String, u64>,
    /// Pass as `after` for the next page; null on the last page.
    #[schema(required = true, nullable = true)]
    pub next_after: Option<String>,
}

/// One input row a transform row derives from (VIEW-LINEAGE-1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornTransformMember {
    /// The input alias.
    pub input: String,
    /// The object type.
    pub object_type: String,
    /// The object id.
    pub id: String,
    /// The version read.
    pub version: u64,
}

/// One row's lineage: the input rows it derives from.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornTransformLineage {
    /// The transform.
    pub transform: String,
    /// The row.
    pub row_id: String,
    /// Commits in the cell at the snapshot the row was computed from.
    pub commits: u64,
    /// VIEW-MAT-1: the stored result the row comes from; null when evaluated
    /// live for this read.
    #[schema(required = true, nullable = true)]
    pub materialization: Option<NornMaterialization>,
    /// Every column value of the row.
    #[schema(schema_with = row_values_schema)]
    pub values: BTreeMap<String, RuntimeValue>,
    /// The input rows, ordered by input, then id.
    pub members: Vec<NornTransformMember>,
}

/// One declared input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornTransformInput {
    /// The alias the plan reads.
    pub alias: String,
    /// The object type.
    pub object_type: String,
    /// The pinned artifact.
    pub artifact_id: String,
    /// Its exact version.
    pub version: String,
}

/// One admitted operator with its row schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornTransformNode {
    /// The node id.
    pub id: String,
    /// The operator: `scan`, `filter`, `project`, `join`, `aggregate`,
    /// `window`, `union` or `dedupe`.
    pub op: String,
    /// The nodes (or, for a scan, the input alias) it reads.
    pub inputs: Vec<String>,
    /// Its row identity.
    pub grain: Vec<String>,
    /// Its typed columns.
    pub columns: Vec<NornTransformColumn>,
}

/// A transform's admitted plan: the definition graph Studio draws.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornTransformPlan {
    /// The transform.
    pub transform: String,
    /// Its exact version.
    pub version: String,
    /// The view id.
    pub view_id: String,
    /// The declared inputs.
    pub inputs: Vec<NornTransformInput>,
    /// Every node, inputs first.
    pub nodes: Vec<NornTransformNode>,
    /// The output node.
    pub output: String,
    /// VIEW-MAT-1: how the stored result refreshes; null when the transform
    /// is evaluated on every read.
    #[schema(required = true, nullable = true)]
    pub refresh: Option<NornRefresh>,
}

/// VIEW-MAT-1: the stored result a read was served from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornMaterialization {
    /// Commits in the cell when the stored result was computed: its frontier.
    pub frontier: u64,
    /// When it was computed, in milliseconds since the Unix epoch.
    pub refreshed_at_ms: u64,
    /// The inputs changed since: the rows are the frontier's, not the
    /// current snapshot's.
    pub stale: bool,
}

/// VIEW-MAT-1: when a materialized transform's stored result is refreshed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornRefresh {
    /// `on_read`: a read that finds the inputs changed refreshes; `manual`:
    /// only `POST /v1/transforms/{transform}:refresh` does.
    pub policy: String,
    /// `on_read` only: a stored result younger than this is served,
    /// labelled stale, instead of refreshed.
    #[schema(required = true, nullable = true)]
    pub max_age_ms: Option<u64>,
}

fn materialization(state: &MaterializationState) -> NornMaterialization {
    NornMaterialization {
        frontier: state.frontier.commits,
        refreshed_at_ms: state.refreshed_at_ms,
        stale: state.stale,
    }
}

fn row_values_schema() -> utoipa::openapi::schema::Object {
    crate::objects::free_form(
        "Every column value by column name; decimals are exact JSON number tokens.",
        false,
    )
}

fn compiled_artifact_schema() -> utoipa::openapi::schema::Object {
    crate::objects::free_form(
        "The `derived` artifact the query declares, as an artifact envelope; null when refused.",
        true,
    )
}

fn not_found() -> Refusal {
    Refusal::new(
        StatusCode::NOT_FOUND,
        "transform.not_found",
        "no such transform",
        "the cell's installed definitions declare no relational transform with this id",
    )
}

fn row_not_found() -> Refusal {
    Refusal::new(
        StatusCode::NOT_FOUND,
        "transform.row_not_found",
        "no such row",
        "the transform has no row with this id for this reader at this revision",
    )
}

/// Admit `transform` against the cell's installed definitions.
fn installed_plan<E: SqlEngine>(
    store: &CellStore<E>,
    transform: &str,
) -> Result<TransformPlan, Refusal> {
    let installed = store
        .read()
        .installed_definitions(None)
        .map_err(Refusal::host)?
        .ok_or_else(not_found)?;
    let pool: Vec<ArtifactEnvelope> =
        serde_json::from_str(&installed.canonical_set).map_err(Refusal::host)?;
    let artifact = pool
        .iter()
        .find(|artifact| {
            artifact.artifact_id == transform && norn_checker::transform::is_relational(artifact)
        })
        .ok_or_else(not_found)?;
    let registry: BTreeSet<PropertyTypeName> = installed
        .registry
        .iter()
        .map(|name| PropertyTypeName(name.clone()))
        .collect();
    admit(artifact, &pool, &registry).map_err(|_findings| {
        // Installed definitions were admitted as a set; a transform that no
        // longer admits is stale, never partially evaluated.
        Refusal::from_compute(norn_cell::CellError::ComputeRefused(
            "core.compute.stale_input",
        ))
    })
}

/// VIEW-READ-1: Population on every object type the plan reads.
fn input_gates<E: SqlEngine>(
    host: &DevHost<E>,
    access: &dyn ObjectAccess,
    actor: &str,
    plan: &TransformPlan,
) -> Result<(), Refusal> {
    for input in plan.inputs.values() {
        let request = mapped(
            host.scope(),
            access,
            actor,
            Population,
            &input.object_type,
            None,
        )?;
        if !host.read_permitted(&ReadRequest {
            capability: Population,
            access: &request,
        }) {
            return Err(read_denied());
        }
    }
    Ok(())
}

/// HOST-READ-2 for one input row.
fn row_access<E: SqlEngine>(
    host: &DevHost<E>,
    access: &dyn ObjectAccess,
    actor: &str,
    row: &ObjectRecord,
) -> RowAccess {
    let permits = |capability| {
        mapped(
            host.scope(),
            access,
            actor,
            capability,
            &row.object_type,
            Some(row),
        )
        .is_ok_and(|request| {
            host.read_permitted(&ReadRequest {
                capability,
                access: &request,
            })
        })
    };
    if !permits(Population) {
        RowAccess::None
    } else if permits(Values) {
        RowAccess::Read
    } else {
        RowAccess::Count
    }
}

/// The rows a reader is served: live, or a materialized transform's stored
/// result when the reader may read every input row (VIEW-MAT-1).
fn evaluate<E: SqlEngine>(
    host: &DevHost<E>,
    (scope, actor): (&ScopeQuery, Option<&str>),
    transform: &str,
    access: &dyn ObjectAccess,
    now: LogicalTime,
) -> Result<(TransformPlan, Served), Refusal> {
    let (_epoch, actor) = collection(host, scope, actor, transform, access)?;
    let cell = host.cell()?;
    let store = cell.store();
    let plan = installed_plan(store, transform)?;
    input_gates(host, access, &actor, &plan)?;
    let served = store
        .serve_transform(
            &plan,
            (MAIN_WORLD, &host.scope().key()),
            |row| Ok(row_access(host, access, &actor, row)),
            now.as_millis(),
        )
        .map_err(Refusal::from_compute)?;
    Ok((plan, served))
}

fn additivity(value: &PropertyAdditivity) -> &'static str {
    match value {
        PropertyAdditivity::Undeclared {} => "undeclared",
        PropertyAdditivity::Additive {} => "additive",
        PropertyAdditivity::SemiAdditive {} => "semi_additive",
        PropertyAdditivity::NonAdditive {} => "non_additive",
        PropertyAdditivity::Ratio { .. } => "ratio",
    }
}

fn column(column: &Column, schema: &NodeSchema) -> NornTransformColumn {
    let holding = |source: &norn_checker::transform::Origin| {
        schema
            .columns
            .iter()
            .find(|candidate| candidate.exact.as_ref() == Some(source))
            .map(|candidate| candidate.name.clone())
    };
    let (nullable, inner) = column.describe();
    NornTransformColumn {
        name: column.name.clone(),
        value_type: inner,
        nullable,
        additivity: additivity(&column.additivity).to_owned(),
        identity: column.identity.clone(),
        origins: column
            .origins
            .iter()
            .map(|origin| NornColumnOrigin {
                input: origin.alias.clone(),
                property: origin.property.clone(),
            })
            .collect(),
        markings: column.markings.iter().cloned().collect(),
        currency_from: column.currency.as_ref().and_then(holding),
        unit_from: column.units.iter().filter_map(holding).collect(),
        label: column.label.clone(),
    }
}

/// Each output column with its lineage markings.
fn marked_columns(plan: &TransformPlan) -> Vec<(String, Vec<String>)> {
    columns(plan.output_schema())
        .into_iter()
        .map(|column| (column.name, column.markings))
        .collect()
}

fn columns(schema: &NodeSchema) -> Vec<NornTransformColumn> {
    schema
        .columns
        .iter()
        .map(|item| column(item, schema))
        .collect()
}

fn parse_limit(limit: Option<&str>) -> Result<usize, Refusal> {
    let Some(limit) = limit else {
        return Ok(DEFAULT_LIMIT);
    };
    match limit.parse::<usize>() {
        Ok(limit) if (1..=MAX_LIMIT).contains(&limit) => Ok(limit),
        Ok(_) | Err(_) => Err(Refusal::invalid("limit must be an integer from 1 to 1000")),
    }
}

/// One page of a transform's rows, evaluated now under the reader's visibility.
///
/// # Errors
/// A denied gate (the opaque 403), an undeclared transform (404), an invalid
/// limit or cursor (400), a commit since the cursor (409
/// `transform.snapshot_moved`), a bound (413), a stale or failed evaluation
/// (422) or a host failure.
#[utoipa::path(get, path = "/v1/transforms/{transform}/rows", operation_id = "get_transform_rows", params(ScopeQuery, TransformRowsParams, ("transform" = String, Path, description = "The transform's artifact id.")), responses((status = 200, description = "One page of the transform's rows. Each input row enters only with Population and Values; a row the reader may only count is excluded, counted in `withheld`, and makes the page incomplete.", body = NornTransformPage), (status = 400, description = "The request was refused as invalid (`request.invalid`): an unknown or repeated parameter, a limit outside 1 to 1,000, or a cursor this transform did not issue.", body = Refusal), (status = 403, description = "The request was refused: scope or authority denied on the transform or on any object type it reads, with the same document whatever the reason.", body = Refusal), (status = 404, description = "The request was refused (`transform.not_found`): no installed relational transform has this id.", body = Refusal), (status = 409, description = "The request was refused (`transform.snapshot_moved`, retryable from the first page): the cell committed since the cursor was issued; a page is never assembled from two snapshots.", body = Refusal), (status = 413, description = "The request was refused (`core.compute.bound_exceeded`): the exact engine's row bound; no partial result is returned.", body = Refusal), (status = 422, description = "The request was refused (`core.compute.stale_input` or `core.compute.expression_failed`): the transform no longer admits against the installed definitions, or an admitted expression failed on a row.", body = Refusal), (status = 500, description = "The request was refused: host failure.", body = Refusal)))]
pub fn get_transform_rows<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: Option<&str>,
    transform: &str,
    params: &TransformRowsParams,
    access: &dyn ObjectAccess,
    now: LogicalTime,
) -> Result<NornTransformPage, Refusal> {
    let limit = parse_limit(params.limit.as_deref())?;
    let (plan, served) = evaluate(host, (scope, actor), transform, access, now)?;
    let result = &served.result;
    let commits = result.revision.commits;
    let start = match params.after.as_deref() {
        None => 0,
        Some(cursor) => {
            let (at, row) = cursor
                .split_once(':')
                .and_then(|(at, row)| at.parse::<u64>().ok().map(|at| (at, row)))
                .ok_or_else(|| Refusal::invalid("the cursor was not issued for this transform"))?;
            if at != commits {
                let mut moved = Refusal::new(
                    StatusCode::CONFLICT,
                    "transform.snapshot_moved",
                    "the cell committed since the cursor was issued",
                    "read the transform again from its first page",
                );
                moved.retryable = true;
                return Err(moved);
            }
            let position = result
                .rows
                .iter()
                .position(|candidate| candidate.id == row)
                .ok_or_else(|| Refusal::invalid("the cursor was not issued for this transform"))?;
            position.saturating_add(1)
        }
    };
    let total = result.rows.len();
    let mut page: Vec<NornTransformRow> = result
        .rows
        .iter()
        .skip(start)
        .take(limit)
        .map(|row| NornTransformRow {
            row_id: row.id.clone(),
            values: row.values.clone(),
            member_count: u64::try_from(row.members.len()).unwrap_or(u64::MAX),
        })
        .collect();
    // PRIVACY-1: a marked column leaves projected, as its source property does.
    let marked = marked_columns(&plan);
    crate::privacy::project_columns(
        host,
        &marked,
        &mut page.iter_mut().map(|row| &mut row.values).collect::<Vec<_>>(),
    )?;
    let end = start.saturating_add(page.len());
    let next_after = (end < total)
        .then(|| page.last().map(|row| format!("{commits}:{}", row.row_id)))
        .flatten();
    Ok(NornTransformPage {
        transform: result.transform.clone(),
        version: result.version.clone(),
        engine: "cell".to_owned(),
        commits,
        materialization: served.materialization.as_ref().map(materialization),
        columns: columns(plan.output_schema()),
        grain: result.grain.clone(),
        rows: page,
        total_rows: u64::try_from(total).unwrap_or(u64::MAX),
        withheld: result.withheld.clone(),
        excluded: u64::try_from(result.exclusions.len()).unwrap_or(u64::MAX),
        complete: result.complete,
        node_rows: result.node_rows.clone(),
        next_after,
    })
}

/// One row's lineage: the input rows it derives from (VIEW-LINEAGE-1).
///
/// # Errors
/// As [`get_transform_rows`], and a row this reader does not see now (404
/// `transform.row_not_found`).
#[utoipa::path(get, path = "/v1/transforms/{transform}/rows/{row}/lineage", operation_id = "get_transform_row_lineage", params(ScopeQuery, ("transform" = String, Path, description = "The transform's artifact id."), ("row" = String, Path, description = "A `rowId` from a page of the transform's rows.")), responses((status = 200, description = "The row and every input row it derives from, under the same visibility as its page.", body = NornTransformLineage), (status = 403, description = "The request was refused: scope or authority denied, with the same document whatever the reason.", body = Refusal), (status = 404, description = "The request was refused (`transform.not_found` or `transform.row_not_found`).", body = Refusal), (status = 413, description = "The request was refused (`core.compute.bound_exceeded`).", body = Refusal), (status = 422, description = "The request was refused (`core.compute.stale_input` or `core.compute.expression_failed`).", body = Refusal), (status = 500, description = "The request was refused: host failure.", body = Refusal)))]
pub fn get_transform_row_lineage<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: Option<&str>,
    transform: &str,
    row: &str,
    access: &dyn ObjectAccess,
    now: LogicalTime,
) -> Result<NornTransformLineage, Refusal> {
    let (plan, served) = evaluate(host, (scope, actor), transform, access, now)?;
    let result = &served.result;
    let found = result
        .rows
        .iter()
        .find(|candidate| candidate.id == row)
        .ok_or_else(row_not_found)?;
    let mut values = found.values.clone();
    crate::privacy::project_columns(host, &marked_columns(&plan), &mut [&mut values])?;
    Ok(NornTransformLineage {
        transform: result.transform.clone(),
        row_id: found.id.clone(),
        commits: result.revision.commits,
        materialization: served.materialization.as_ref().map(materialization),
        values,
        members: found
            .members
            .iter()
            .map(|member| NornTransformMember {
                input: member.alias.clone(),
                object_type: member.object_type.clone(),
                id: member.id.clone(),
                version: member.version,
            })
            .collect(),
    })
}

fn op(step: &Step) -> (&'static str, Vec<String>) {
    match step {
        Step::Scan { alias } => ("scan", vec![alias.clone()]),
        Step::Filter { input, .. } => ("filter", vec![input.clone()]),
        Step::Project { input, .. } => ("project", vec![input.clone()]),
        Step::Join { left, right, .. } => ("join", vec![left.clone(), right.clone()]),
        Step::Aggregate { input, .. } => ("aggregate", vec![input.clone()]),
        Step::Window { input, .. } => ("window", vec![input.clone()]),
        Step::Union { inputs, .. } => ("union", inputs.clone()),
        Step::Dedupe { input, .. } => ("dedupe", vec![input.clone()]),
    }
}

/// A transform's admitted plan: inputs, typed nodes and column lineage.
///
/// # Errors
/// A denied gate (the opaque 403), an undeclared transform (404), a plan that
/// no longer admits (422) or a host failure.
#[utoipa::path(get, path = "/v1/transforms/{transform}/plan", operation_id = "get_transform_plan", params(ScopeQuery, ("transform" = String, Path, description = "The transform's artifact id.")), responses((status = 200, description = "The plan the checker admits against the installed definitions now: every node's operator, inputs, grain and typed columns with their lineage.", body = NornTransformPlan), (status = 403, description = "The request was refused: scope or authority denied, with the same document whatever the reason.", body = Refusal), (status = 404, description = "The request was refused (`transform.not_found`).", body = Refusal), (status = 422, description = "The request was refused (`core.compute.stale_input`): the transform no longer admits.", body = Refusal), (status = 500, description = "The request was refused: host failure.", body = Refusal)))]
pub fn get_transform_plan<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: Option<&str>,
    transform: &str,
    access: &dyn ObjectAccess,
) -> Result<NornTransformPlan, Refusal> {
    let (_epoch, _actor) = collection(host, scope, actor, transform, access)?;
    let cell = host.cell()?;
    let plan = installed_plan(cell.store(), transform)?;
    Ok(plan_document(&plan))
}

/// The definition graph of an admitted plan.
fn plan_document(plan: &TransformPlan) -> NornTransformPlan {
    let nodes = plan
        .order
        .iter()
        .filter_map(|id| {
            plan.nodes.get(id).map(|node| {
                let (op, inputs) = op(&node.step);
                NornTransformNode {
                    id: id.clone(),
                    op: op.to_owned(),
                    inputs,
                    grain: node.schema.grain.clone(),
                    columns: columns(&node.schema),
                }
            })
        })
        .collect();
    NornTransformPlan {
        transform: plan.artifact_id.clone(),
        version: plan.version.clone(),
        view_id: plan.view_id.clone(),
        inputs: plan
            .inputs
            .values()
            .map(|input| NornTransformInput {
                alias: input.alias.clone(),
                object_type: input.object_type.clone(),
                artifact_id: input.artifact_id.clone(),
                version: input.version.clone(),
            })
            .collect(),
        nodes,
        output: plan.output.clone(),
        refresh: plan.materialization.map(|policy| match policy {
            Materialization::OnRead { max_age_ms } => NornRefresh {
                policy: "on_read".to_owned(),
                max_age_ms,
            },
            Materialization::Manual => NornRefresh {
                policy: "manual".to_owned(),
                max_age_ms: None,
            },
        }),
    }
}

/// A SQL transform to compile against the installed definitions (VIEW-SQL-1).
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompileTransformRequest {
    /// The transform id to give the artifact (`shop.revenue_by_region`).
    pub id: String,
    /// Its title.
    pub title: String,
    /// Its version.
    pub version: String,
    /// The query in the transform dialect. It is lowered, never run.
    pub sql: String,
}

/// What a SQL transform lowers to, and whether the checker admits it.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornCompiledTransform {
    /// Installing the lowered transform over the installed definitions would
    /// be admitted.
    pub admitted: bool,
    /// The `derived` artifact the query declares; null when refused.
    #[schema(required = true, schema_with = compiled_artifact_schema)]
    pub artifact: Option<serde_json::Value>,
    /// The admitted plan; null when refused.
    #[schema(required = true, nullable = true)]
    pub plan: Option<NornTransformPlan>,
    /// Every refusal, with its stable code; empty when admitted.
    pub diagnostics: Vec<crate::refusal::RefusalDiagnostic>,
}

/// VIEW-SQL-1: lower a SQL query to a transform and admit it as installing
/// it would: the installed set re-admitted whole with it added, on the base
/// the installed receipt records. Nothing is installed and no SQL runs.
///
/// # Errors
/// A denied definitions read (the opaque 403), no installed definitions
/// (404 `defs.version_not_found`) or a host failure. A query the dialect or
/// the checker refuses is a 200 with `admitted: false` and its diagnostics.
#[utoipa::path(post, path = "/v1/transforms:compile", operation_id = "compile_transform", params(ScopeQuery), request_body = CompileTransformRequest, responses((status = 200, description = "The lowered artifact and plan when the checker admits it; otherwise its diagnostics, among them `derived.validation.sql_unsupported` for anything without a typed operator.", body = NornCompiledTransform), (status = 400, description = "The request was refused as invalid (`request.invalid`).", body = Refusal), (status = 403, description = "The request was refused: scope or authority denied, with the same document whatever the reason.", body = Refusal), (status = 404, description = "The request was refused (`defs.version_not_found`): nothing is installed yet.", body = Refusal), (status = 500, description = "The request was refused: host failure.", body = Refusal)))]
pub fn compile_transform<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: Option<&str>,
    request: &CompileTransformRequest,
) -> Result<NornCompiledTransform, Refusal> {
    let epoch = host.definition_read();
    let authorized = host.authorizer.authorize(&crate::authz::AccessRequest {
        scope: host.scope(),
        actor,
        operation: crate::authz::Operation::ReadDefinitions,
    });
    let scoped = host
        .scope()
        .admit(scope.org_id.as_deref(), scope.environment_id.as_deref());
    if authorized.is_err() || scoped.is_err() {
        return Err(read_denied());
    }
    let _epoch = epoch?;
    let cell = host.cell()?;
    let installed = cell
        .store()
        .read()
        .installed_definitions(None)
        .map_err(Refusal::host)?
        .ok_or_else(|| {
            Refusal::new(
                StatusCode::NOT_FOUND,
                "defs.version_not_found",
                "no definitions are installed",
                "install definitions before compiling a transform against them",
            )
        })?;
    let pool: Vec<ArtifactEnvelope> =
        serde_json::from_str(&installed.canonical_set).map_err(Refusal::host)?;
    let registry: BTreeSet<PropertyTypeName> = installed
        .registry
        .iter()
        .map(|name| PropertyTypeName(name.clone()))
        .collect();
    let meta = norn_checker::transform::sql::SqlMeta {
        id: &request.id,
        title: &request.title,
        version: &request.version,
        scope: serde_json::json!({"orgId": host.scope().org_id, "environmentId": host.scope().environment_id}),
    };
    let refused = |findings: &[norn_checker::Diagnostic]| NornCompiledTransform {
        admitted: false,
        artifact: None,
        plan: None,
        diagnostics: crate::refusal::RefusalDiagnostic::from_checker(findings, ""),
    };
    let artifact =
        match norn_checker::transform::sql::sql_artifact(&request.sql, &meta, &pool, &registry) {
            Ok(artifact) => artifact,
            Err(findings) => return Ok(refused(&findings)),
        };
    let envelope: ArtifactEnvelope =
        serde_json::from_value(artifact.clone()).map_err(Refusal::host)?;
    // The verdict is an install's: the whole installed set re-admitted with
    // this transform added, through the path installs take.
    match crate::kernel_set::admit_over(
        &installed.canonical_set,
        installed.admission_receipt.as_deref(),
        std::slice::from_ref(&envelope),
    ) {
        Ok(_) => {}
        Err(crate::kernel_set::KernelRefusal::Refused(findings)) => return Ok(refused(&findings)),
        Err(crate::kernel_set::KernelRefusal::Invalid(detail)) => {
            return Err(Refusal::host(detail));
        }
    }
    let mut all = pool;
    all.retain(|artifact| artifact.artifact_id != envelope.artifact_id);
    all.push(envelope.clone());
    match admit(&envelope, &all, &registry) {
        Ok(plan) => Ok(NornCompiledTransform {
            admitted: true,
            artifact: Some(artifact),
            plan: Some(plan_document(&plan)),
            diagnostics: Vec::new(),
        }),
        Err(findings) => Ok(refused(&findings)),
    }
}

/// VIEW-MAT-1: recompute and store a materialized transform's result. Only
/// a reader who may read every input row refreshes; the stored result is
/// then served to such readers until the inputs change.
///
/// # Errors
/// A denied gate, or a reader who may not read every input row (the same
/// opaque 403), an undeclared transform (404), a transform that is not
/// materialized (400), a bound (413), a stale or failed evaluation (422) or
/// a host failure.
#[utoipa::path(post, path = "/v1/transforms/{transform}:refresh", operation_id = "refresh_transform", params(ScopeQuery, ("transform" = String, Path, description = "The transform's artifact id.")), responses((status = 200, description = "The stored result now: its frontier and refresh time.", body = NornMaterialization), (status = 400, description = "The request was refused as invalid (`request.invalid`): the transform is evaluated on every read, not materialized.", body = Refusal), (status = 403, description = "The request was refused: scope or authority denied on the transform or any object type it reads, or an input row this reader may not read, with the same document whatever the reason.", body = Refusal), (status = 404, description = "The request was refused (`transform.not_found`).", body = Refusal), (status = 413, description = "The request was refused (`core.compute.bound_exceeded`).", body = Refusal), (status = 422, description = "The request was refused (`core.compute.stale_input` or `core.compute.expression_failed`).", body = Refusal), (status = 500, description = "The request was refused: host failure.", body = Refusal)))]
pub fn refresh_transform<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: Option<&str>,
    transform: &str,
    access: &dyn ObjectAccess,
    now: LogicalTime,
) -> Result<NornMaterialization, Refusal> {
    let (_epoch, actor) = collection(host, scope, actor, transform, access)?;
    let cell = host.cell()?;
    let store = cell.store();
    let plan = installed_plan(store, transform)?;
    input_gates(host, access, &actor, &plan)?;
    if plan.materialization.is_none() {
        return Err(Refusal::invalid(
            "the transform is evaluated on every read; only a materialized transform refreshes",
        ));
    }
    store
        .refresh_transform(
            &plan,
            (MAIN_WORLD, &host.scope().key()),
            |row| Ok(row_access(host, access, &actor, row)),
            now.as_millis(),
        )
        .map_err(Refusal::from_compute)?
        .map(|state| materialization(&state))
        .ok_or_else(read_denied)
}
