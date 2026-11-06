//! RUN-ROUTES-1: start, read and list runs of installed flows.
//!
//! `POST /v1/runs` starts a run of an installed flow: the flow's `decide` rule
//! evaluates its start turn, and the cell commits the resulting run object
//! together with its run-ledger row (RUN-LEDGER-1) in one commit.
//! `GET /v1/runs/{id}` and `GET /v1/runs?flow=&state=&cursor=&limit=` read the
//! ledger. Every route admits explicit scope first. Starting is a host
//! operation the trusted [`crate::authz::Authorizer`] decides. Every run read
//! passes the ordinary `values` read gate on the run's object type. A run the
//! caller may not read is indistinguishable from an absent one.

use http::StatusCode;
use norn_cell::engine::SqlEngine;
use norn_cell::flow_run::{FlowRef, RunFilter, RunRow, RunStart};
use norn_ir::RuleRoot;
use norn_kernel::{DecisionTurn, EnvelopeLimits, FactKey, TypeCatalog, World};
use serde::{Deserialize, Serialize};

use crate::authz::{AccessRequest, Operation, ReadCapability, ReadRequest};
use crate::host::DevHost;
use crate::refusal::Refusal;
use crate::scope::ScopeQuery;

/// The most ledger rows one list request examines. A caller who can read
/// few of them gets a short page; filtering by flow keeps pages dense.
pub const MAX_RUN_SCAN: usize = 2000;

/// The longest run id, in bytes.
pub const MAX_RUN_ID_BYTES: usize = 128;

/// Builds the kernel PDP input for reading one capability of an object type,
/// from the trusted transport's verified principal and grant mapping.
pub type ReadAccess<'a> = &'a dyn Fn(ReadCapability, &str) -> norn_kernel::authz::AccessRequest;

/// `POST /v1/runs` body.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StartRunRequest {
    /// The installed flow's definition id (`flow.id`); its rule is
    /// `<flow>.flow.rule`.
    pub flow: String,
    /// The caller-chosen run id: `[A-Za-z0-9._:-]`, at most 128 bytes. A retry
    /// with the same id and flow returns the existing run.
    pub run_id: String,
}

/// One run, as the routes return it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunDocument {
    /// The run id.
    pub id: String,
    /// Its generation.
    pub generation: u64,
    /// The flow it runs.
    pub flow: FlowRef,
    /// The run's object type.
    pub object_type: String,
    /// Its lifecycle state; `null` once the run object is deleted.
    pub state: Option<String>,
    /// The run object's version.
    pub version: u64,
    /// Start time (logical milliseconds) and commit.
    pub created_at_ms: u64,
    /// The starting commit.
    pub created_commit_seq: u64,
    /// Last write time (logical milliseconds).
    pub updated_at_ms: u64,
    /// The last commit that wrote the run object.
    pub updated_commit_seq: u64,
}

impl From<RunRow> for RunDocument {
    fn from(row: RunRow) -> Self {
        Self {
            id: row.run_id,
            generation: row.generation,
            flow: row.flow,
            object_type: row.object_type,
            state: row.state,
            version: row.version,
            created_at_ms: row.created_at_ms,
            created_commit_seq: row.created_commit_seq,
            updated_at_ms: row.updated_at_ms,
            updated_commit_seq: row.updated_commit_seq,
        }
    }
}

/// One page of runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunPage {
    /// The runs the caller may read, in run-id order.
    pub runs: Vec<RunDocument>,
    /// Pass as `cursor` for the next page; absent on the last page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

/// `GET /v1/runs` query.
#[derive(Debug, Clone, Default)]
pub struct RunsQuery {
    /// Required scope.
    pub scope: ScopeQuery,
    /// Only runs of this flow.
    pub flow: Option<String>,
    /// Only runs in this state.
    pub state: Option<String>,
    /// The previous page's `nextCursor`.
    pub cursor: Option<String>,
    /// Page size, 1 to 200 (default 50).
    pub limit: Option<u32>,
}

fn run_exists() -> Refusal {
    Refusal::new(
        StatusCode::CONFLICT,
        "run.exists",
        "the run id is taken",
        "choose another runId",
    )
}

fn run_not_found() -> Refusal {
    Refusal::new(
        StatusCode::NOT_FOUND,
        "run.not_found",
        "no such run",
        "no run with this id is visible to the caller in this scope",
    )
}

fn valid_run_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_RUN_ID_BYTES
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b':' | b'-'))
}

fn readable<E: SqlEngine>(host: &DevHost<E>, access: ReadAccess<'_>, row: &RunRow) -> bool {
    host.read_permitted(&ReadRequest {
        capability: ReadCapability::Values,
        access: &access(ReadCapability::Values, &row.object_type),
    })
}

/// The installed flow: its rule, Run type, lifecycle property and version.
struct InstalledFlow {
    rule: RuleRoot,
    version: String,
    object_type: String,
    state_property: String,
    environment_hash: String,
    /// The flow's definition is retired: its live runs advance, new ones refuse.
    retired: bool,
}

fn text<'v>(value: &'v serde_json::Value, pointer: &str) -> Option<&'v str> {
    value.pointer(pointer).and_then(serde_json::Value::as_str)
}

fn flow_not_found() -> Refusal {
    Refusal::new(
        StatusCode::NOT_FOUND,
        "run.flow_not_found",
        "no such installed flow",
        "name the id of a flow installed in the cell's active definitions",
    )
}

fn installed_flow<E: SqlEngine>(
    store: &norn_cell::CellStore<E>,
    flow: &str,
) -> Result<InstalledFlow, Refusal> {
    let active = store
        .read()
        .installed_definitions(None)
        .map_err(Refusal::host)?
        .ok_or_else(flow_not_found)?;
    let artifacts: Vec<serde_json::Value> =
        serde_json::from_str(&active.canonical_set).map_err(Refusal::host)?;
    let find = |id: &str, kind: &str| {
        artifacts.iter().find(|artifact| {
            text(artifact, "/artifactId") == Some(id) && text(artifact, "/kind") == Some(kind)
        })
    };
    let rule = find(&format!("{flow}.flow.rule"), "rule").ok_or_else(flow_not_found)?;
    let entity = rule
        .pointer("/dependencies")
        .and_then(serde_json::Value::as_array)
        .and_then(|dependencies| {
            let mut entities = dependencies
                .iter()
                .filter(|dependency| text(dependency, "/kind") == Some("entity"));
            let only = entities.next();
            only.filter(|_| entities.next().is_none())
        })
        .and_then(|dependency| text(dependency, "/artifactId"))
        .and_then(|id| find(id, "entity"))
        .ok_or_else(flow_not_found)?;
    let lifecycles = entity
        .pointer("/payload/objectType/lifecycles")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(flow_not_found)?;
    let (Some(object_type), [lifecycle], Some(version), Some("decide"), Some(payload)) = (
        text(entity, "/payload/objectType/name"),
        lifecycles.as_slice(),
        text(rule, "/version"),
        text(rule, "/payload/contract"),
        rule.pointer("/payload"),
    ) else {
        return Err(flow_not_found());
    };
    Ok(InstalledFlow {
        rule: serde_json::from_value(payload.clone()).map_err(|_not_a_rule| flow_not_found())?,
        version: version.to_owned(),
        object_type: object_type.to_owned(),
        state_property: text(lifecycle, "/property")
            .ok_or_else(flow_not_found)?
            .to_owned(),
        environment_hash: active.receipt_sha256,
        retired: text(rule, "/lifecycle") == Some("retired"),
    })
}

/// `POST /v1/runs`: start a run of an installed flow.
///
/// # Errors
/// Missing or foreign scope, a denied caller, an invalid body, an unknown
/// flow, a run id already started for another flow (`run.exists`), a refused
/// start turn (`run.start_refused`, or the kernel's own refusal code), or a
/// host failure.
pub fn start_run<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: Option<&str>,
    body: &StartRunRequest,
    access: ReadAccess<'_>,
) -> Result<(StatusCode, RunDocument), Refusal> {
    let _epoch = host.definition_read()?;
    host.scope()
        .admit(scope.org_id.as_deref(), scope.environment_id.as_deref())?;
    host.authorizer.authorize(&AccessRequest {
        scope: host.scope(),
        actor,
        operation: Operation::StartRun(body.flow.clone()),
    })?;
    if !valid_run_id(&body.run_id) {
        return Err(Refusal::invalid(
            "runId must be 1-128 bytes of [A-Za-z0-9._:-]",
        ));
    }
    let mut cell = host.cell()?;
    let world = norn_cell::MAIN_WORLD;
    if let Some(row) = cell
        .store()
        .read()
        .flow_run(world, &body.run_id)
        .map_err(Refusal::host)?
    {
        // An exact retry returns the run; another flow's id never leaks.
        return if row.flow.id == body.flow && readable(host, access, &row) {
            Ok((StatusCode::OK, row.into()))
        } else {
            Err(run_exists())
        };
    }
    commit_start(&mut cell, body)?;
    let row = cell
        .store()
        .read()
        .flow_run(world, &body.run_id)
        .map_err(Refusal::host)?
        .ok_or_else(|| Refusal::host("a committed run start left no ledger row"))?;
    Ok((StatusCode::CREATED, row.into()))
}

/// Evaluate the installed flow's start turn and commit it with its run.
fn commit_start<E: SqlEngine>(
    cell: &mut crate::host::ActionCell<E>,
    body: &StartRunRequest,
) -> Result<(), Refusal> {
    let flow = installed_flow(cell.store(), &body.flow)?;
    if flow.retired {
        return Err(Refusal::new(
            StatusCode::CONFLICT,
            "run.flow_retired",
            "the flow is retired",
            "a retired flow starts no new runs; its live runs still finish",
        ));
    }
    let output =
        norn_kernel::flow_start::flow_start(&flow.rule, &flow.environment_hash, &body.run_id)
            .map_err(|error| {
                Refusal::new(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    error.code(),
                    "the flow refused to start",
                    error.to_string(),
                )
            })?;
    let model = cell.store().model();
    let catalog = TypeCatalog::new(model.object_types())
        .with_interfaces(model.interfaces())
        .with_governing_laws(model.law_artifacts())
        .with_property_types(model.property_types());
    let run_key = FactKey::new(flow.object_type.clone(), body.run_id.clone());
    let slice = norn_kernel::load_slice(&*cell, [run_key]).map_err(Refusal::host)?;
    let turn = DecisionTurn {
        id: format!("norn.run.start/{}", body.run_id),
        now: cell.now(),
        world: World::Main,
        consumes: None,
    };
    let commit = norn_kernel::decision_cell_commit::<_, _, _, norn_cell::CellError>(
        &catalog,
        &slice,
        &turn,
        &output,
        &EnvelopeLimits::V1,
    )
    .map_err(|refusal| {
        Refusal::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            refusal.code(),
            "the flow's start turn was refused",
            refusal.to_string(),
        )
    })?;
    let start = RunStart {
        run_id: body.run_id.clone(),
        flow: FlowRef {
            id: body.flow.clone(),
            version: flow.version,
        },
        object_type: flow.object_type,
        state_property: flow.state_property,
    };
    cell.commit_run_start(commit, &start).map_err(|error| {
        let norn_kernel::CommitError::Host(host_error) = error else {
            return Refusal::new(
                StatusCode::CONFLICT,
                "run.start_conflict",
                "the run start was refused",
                "the cell changed while the run started; retry with the same runId",
            );
        };
        if let norn_cell::CellError::RunRefused(code) = host_error {
            if code == "run.exists" {
                return run_exists();
            }
            return Refusal::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                code,
                "the run start was refused",
                "the flow's start turn must create its run object",
            );
        }
        // A storage failure is the host's, never the caller's conflict.
        Refusal::host(host_error)
    })?;
    Ok(())
}

/// `GET /v1/runs/{id}`.
///
/// # Errors
/// Missing or foreign scope; `run.not_found` for an absent or unreadable run.
pub fn get_run<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    id: &str,
    access: ReadAccess<'_>,
) -> Result<RunDocument, Refusal> {
    let _epoch = host.definition_read()?;
    host.scope()
        .admit(scope.org_id.as_deref(), scope.environment_id.as_deref())?;
    if !valid_run_id(id) {
        return Err(run_not_found());
    }
    let cell = host.cell()?;
    let row = cell
        .store()
        .read()
        .flow_run(norn_cell::MAIN_WORLD, id)
        .map_err(Refusal::host)?
        .filter(|row| readable(host, access, row))
        .ok_or_else(run_not_found)?;
    Ok(row.into())
}

/// `GET /v1/runs`: one page in run-id order, holding only runs the caller may
/// read. The cursor advances past every examined row, readable or not.
///
/// # Errors
/// Missing or foreign scope, an invalid limit, or a host failure.
pub fn list_runs<E: SqlEngine>(
    host: &DevHost<E>,
    query: &RunsQuery,
    access: ReadAccess<'_>,
) -> Result<RunPage, Refusal> {
    let _epoch = host.definition_read()?;
    host.scope().admit(
        query.scope.org_id.as_deref(),
        query.scope.environment_id.as_deref(),
    )?;
    let limit = query.limit.unwrap_or(50);
    if !(1..=norn_cell::flow_run::MAX_RUN_PAGE).contains(&limit) {
        return Err(Refusal::invalid("limit must be 1-200"));
    }
    let cell = host.cell()?;
    let reader = cell.store().read();
    let wanted = usize::try_from(limit).unwrap_or(usize::MAX);
    let mut runs = Vec::new();
    let mut after = query.cursor.clone();
    let mut scanned = 0_usize;
    let mut exhausted = false;
    // Scan whole ledger pages until the page holds `limit` readable runs, the
    // ledger ends, or the scan bound is reached. The cursor names only a run
    // the caller may read, so it never reveals an invisible one.
    while runs.len() < wanted && scanned < MAX_RUN_SCAN {
        let rows = reader
            .flow_runs(
                norn_cell::MAIN_WORLD,
                &RunFilter {
                    flow: query.flow.clone(),
                    state: query.state.clone(),
                    after: after.clone(),
                    limit: norn_cell::flow_run::MAX_RUN_PAGE,
                },
            )
            .map_err(Refusal::host)?;
        exhausted = rows.len() < usize::try_from(norn_cell::flow_run::MAX_RUN_PAGE).unwrap_or(0);
        for row in rows {
            scanned = scanned.saturating_add(1);
            after = Some(row.run_id.clone());
            if readable(host, access, &row) {
                runs.push(RunDocument::from(row));
                if runs.len() == wanted {
                    exhausted = false;
                    break;
                }
            }
        }
        if exhausted {
            break;
        }
    }
    let next_cursor = if exhausted {
        None
    } else {
        runs.last().map(|run| run.id.clone())
    };
    Ok(RunPage { runs, next_cursor })
}
