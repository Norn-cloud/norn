//! D102: the routes of a host whose analytics run on the tenant's engine.
//!
//! Questions on such a host are answered by the engine, never by an
//! analytics scan of the cell's own store; the cell keeps admission,
//! authorization, the row gates and the answer's lineage, and verifies every
//! engine answer. Datasets load into the engine from the object store at bulk
//! speed, and an authorized reader exports a type's rows from it. No route
//! here accepts SQL, a table or a column expression (#733): the engine's
//! operations are typed, and the cell builds every one.
//!
//! An engine answer is a short exchange of operations: the mirror's frontier,
//! sync batches of committed versions until the mirror reaches the verified
//! head, then the question, export or load. The exchange is stateless: each
//! step is decided from the last answer alone, so a host that cannot block on
//! I/O (a Durable Object) ends the request with the call, performs it, and
//! re-enters with the answer; the request is then decided again from the
//! start, authorization included.

use std::collections::BTreeMap;

use norn_cell::analytics::{
    DatasetRecord, EngineRefusal, EvaluateRequest, EvaluateResponse, ExportFormat, ExportRequest,
    FileFormat, FileSource, FrontierResponse, LoadRequest, LoadResponse, Source, TenantId,
    hex_digest,
};
use norn_cell::compute::{ALL_MEMBERS, QueryContext, QueryResult};
use norn_cell::engine::SqlEngine;
use norn_cell::{CellError, CellStore, ObjectRecord};
use norn_checker::EffectiveCatalog;
use norn_checker::metric::compute::{QuestionPlan, UNSUPPORTED_ENGINE};
use norn_ir::MetricDefinition;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::authz::{Operation, ReadCapability};
use crate::host::DevHost;
use crate::object_routes::{ObjectAccess, uniform_gate};
use crate::question_routes::{AdHocQuestions, QuestionRuntime, QuestionSnapshot};
use crate::refusal::Refusal;
use crate::scope::{CellScope, ScopeQuery};

/// Committed versions sent per sync batch.
const SYNC_BATCH: usize = 5_000;
/// Engine steps one answer may take before it refuses.
pub const MAX_STEPS: usize = 64;
/// Lifetime of the presigned URL a load reads, in seconds.
const LOAD_URL_SECONDS: u64 = 900;

/// One engine operation, as the host sends it: `POST /v1/<op>` with `body`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EngineCall {
    /// `frontier`, `sync`, `evaluate`, `members`, `profile`, `load` or `export`.
    pub op: String,
    /// Identity of the call; the answer names it, so a stale answer is never used.
    pub digest: String,
    /// The typed request, as JSON.
    pub body: String,
    /// The host streams a successful answer to the caller with these headers
    /// instead of re-entering (an export).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<BTreeMap<String, String>>,
}

/// The engine's answer to an [`EngineCall`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineAnswer {
    /// The call's operation.
    pub op: String,
    /// The call's digest.
    pub digest: String,
    /// HTTP status.
    pub status: u16,
    /// The response body.
    pub body: String,
}

impl EngineCall {
    fn new(op: &str, request: &impl Serialize) -> Result<Self, CellError> {
        let body =
            serde_json::to_string(request).map_err(|e| CellError::Encoding(e.to_string()))?;
        Ok(Self {
            op: op.to_owned(),
            digest: hex_digest(body.as_bytes()),
            body,
            stream: None,
        })
    }
}

/// How the cell reaches its engine.
pub trait Transport: Send + Sync {
    /// The answer the host re-entered the cell with, once.
    fn resumed(&self) -> Option<EngineAnswer>;
    /// Perform `call` now, or `None` when the host must perform it and re-enter.
    fn call(&self, call: &EngineCall) -> Option<EngineAnswer>;
}

/// The marker error of a request that ended with an engine call; the host
/// performs the call and re-enters instead of answering.
fn pending() -> CellError {
    CellError::Engine("an analytics engine call is pending".to_owned())
}

fn decode<T: serde::de::DeserializeOwned>(body: &str) -> Result<T, CellError> {
    serde_json::from_str(body).map_err(|e| CellError::Engine(format!("engine answer: {e}")))
}

/// A Core 03 refusal the engine reported, as the cell's own.
fn refused(body: &str) -> CellError {
    let code = decode::<EngineRefusal>(body)
        .map(|r| r.code)
        .unwrap_or_default();
    for known in [
        UNSUPPORTED_ENGINE,
        "core.compute.bound_exceeded",
        "core.compute.stale_input",
    ] {
        if code == known {
            return CellError::ComputeRefused(known);
        }
    }
    CellError::Engine(format!("the analytics engine refused: {code}"))
}

/// ANALYTICS-FRONTIER-1: bring `world`'s mirror to `head`, then perform
/// `last` and return its answer.
fn exchange<E: SqlEngine>(
    store: &CellStore<E>,
    transport: &dyn Transport,
    tenant: &TenantId,
    world: Option<(&str, u64)>,
    last: &EngineCall,
) -> Result<EngineAnswer, CellError> {
    let mut answer = transport.resumed();
    let Some((world, head)) = world else {
        return match answer.filter(|a| a.digest == last.digest) {
            Some(answer) => Ok(answer),
            None => transport.call(last).ok_or_else(pending),
        };
    };
    let frontier = EngineCall::new("frontier", &store.analytics_frontier(tenant, world)?)?;
    let mut next = frontier.clone();
    for _ in 0..MAX_STEPS {
        if let Some(done) = answer.take() {
            next = match (done.op.as_str(), done.status) {
                (_, _) if done.digest == last.digest => return Ok(done),
                ("frontier" | "sync", 200) => {
                    let reached = decode::<FrontierResponse>(&done.body)?.frontier;
                    if reached >= head {
                        last.clone()
                    } else {
                        let batch = store.analytics_delta(tenant, world, reached, SYNC_BATCH)?;
                        EngineCall::new("sync", &batch.request)?
                    }
                }
                // A moved mirror or an answer to a stale call: start from the frontier again.
                (_, 409 | 200) => frontier.clone(),
                (_, _) => return Err(refused(&done.body)),
            };
        }
        answer = Some(transport.call(&next).ok_or_else(pending)?);
    }
    Err(CellError::Engine(
        "the analytics exchange did not settle".to_owned(),
    ))
}

fn mirror_world(source: &Source) -> Option<(&str, u64)> {
    match source {
        Source::Mirror {
            world, frontier, ..
        } => Some((world.as_str(), *frontier)),
        Source::Dataset { .. } => None,
    }
}

/// QUERY-ENGINE-1 on the engine: the lowered question at the prepared head.
fn evaluate_on<E: SqlEngine>(
    store: &CellStore<E>,
    transport: &dyn Transport,
    tenant: &TenantId,
    request: &EvaluateRequest,
) -> Result<EvaluateResponse, CellError> {
    let call = EngineCall::new("evaluate", request)?;
    let answer = exchange(
        store,
        transport,
        tenant,
        mirror_world(&request.source),
        &call,
    )?;
    if answer.status == 200 {
        decode(&answer.body)
    } else {
        Err(refused(&answer.body))
    }
}

/// A question runtime whose answers come from the tenant's engine. Every
/// other decision is the wrapped runtime's.
pub struct EngineQuestions<'a, E: SqlEngine> {
    inner: &'a dyn AdHocQuestions<E>,
    transport: &'a dyn Transport,
    tenant: TenantId,
}

impl<'a, E: SqlEngine> EngineQuestions<'a, E> {
    /// Answer `inner`'s questions on the engine `transport` reaches, for the
    /// cell bound to `scope`.
    #[must_use]
    pub fn new(
        inner: &'a dyn AdHocQuestions<E>,
        transport: &'a dyn Transport,
        scope: &CellScope,
    ) -> Self {
        Self {
            inner,
            transport,
            tenant: TenantId::of_scope(&scope.key()),
        }
    }
}

fn gate_record(grain: &str, id: &str, scope: &str) -> ObjectRecord {
    ObjectRecord {
        object_type: grain.to_owned(),
        id: id.to_owned(),
        scope: scope.to_owned(),
        version: 0,
        values: BTreeMap::new(),
    }
}

impl<E: SqlEngine> QuestionRuntime<E> for EngineQuestions<'_, E> {
    fn plan(&self, question: &str) -> Result<QuestionPlan, Refusal> {
        self.inner.plan(question)
    }
    fn question_names(&self) -> Vec<String> {
        self.inner.question_names()
    }
    fn snapshot(
        &self,
        store: &CellStore<E>,
        plan: &QuestionPlan,
        actor: &str,
    ) -> Result<QuestionSnapshot, Refusal> {
        self.inner.snapshot(store, plan, actor)
    }
    fn access(
        &self,
        scope: &CellScope,
        actor: &str,
        question: &str,
        capability: ReadCapability,
        row: Option<&ObjectRecord>,
    ) -> Result<norn_kernel::authz::AccessRequest, Refusal> {
        self.inner.access(scope, actor, question, capability, row)
    }
    fn min_disclosed_bucket(&self) -> usize {
        self.inner.min_disclosed_bucket()
    }
    fn handle_key(&self) -> &[u8] {
        self.inner.handle_key()
    }
    fn collection_access(
        &self,
        scope: &CellScope,
        actor: &str,
        capability: ReadCapability,
        object_type: &str,
    ) -> Result<norn_kernel::authz::AccessRequest, Refusal> {
        self.inner
            .collection_access(scope, actor, capability, object_type)
    }
    /// D102: the engine answers. This host's read policy decides a row by its
    /// type alone, so the population gate is asked once for the whole grain
    /// ([`ALL_MEMBERS`]); listed members are then gated one by one, and any
    /// member the gate refuses refuses the answer.
    fn evaluate(
        &self,
        store: &CellStore<E>,
        plan: &QuestionPlan,
        context: QueryContext,
        visible: &mut dyn FnMut(&ObjectRecord) -> Result<bool, CellError>,
    ) -> Result<QueryResult, CellError> {
        let grain = plan.grain().as_str().to_owned();
        let scope = context.scope.clone();
        let admitted = visible(&gate_record(&grain, ALL_MEMBERS, &scope))?;
        let prepared = store.analytics_prepare(plan, context, &self.tenant)?;
        let response = if admitted {
            evaluate_on(store, self.transport, &self.tenant, &prepared.request)?
        } else {
            EvaluateResponse {
                population: 0,
                excluded: Vec::new(),
                buckets: Vec::new(),
            }
        };
        let result = store.analytics_answer(prepared, response)?;
        for row in result.rows.values() {
            for id in row.members.keys() {
                if !visible(&gate_record(&grain, id, &scope))? {
                    return Err(CellError::ComputeRefused(UNSUPPORTED_ENGINE));
                }
            }
        }
        Ok(result)
    }
}

impl<E: SqlEngine> AdHocQuestions<E> for EngineQuestions<'_, E> {
    fn definitions(&self) -> &[MetricDefinition] {
        self.inner.definitions()
    }
    fn catalog(&self) -> &EffectiveCatalog {
        self.inner.catalog()
    }
    fn max_rows(&self) -> u32 {
        self.inner.max_rows()
    }
    fn max_buckets(&self) -> usize {
        self.inner.max_buckets()
    }
}

fn engine_failure(error: CellError) -> Refusal {
    if matches!(error, CellError::ComputeRefused(_)) {
        Refusal::from_compute(error)
    } else {
        Refusal::host(error)
    }
}

/// `GET /v1/objects/{type}:export` parameters.
#[derive(Debug, Clone, Default, Deserialize, utoipa::IntoParams)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[into_params(parameter_in = Query, rename_all = "camelCase")]
pub struct ExportQuery {
    /// Organization scope.
    pub org_id: Option<String>,
    /// Environment scope.
    pub environment_id: Option<String>,
    /// Comma-separated property ids; each row also carries `id` and `version`.
    pub columns: String,
    /// `parquet` (the default) or `jsonl`.
    pub format: Option<String>,
}

/// D102: every row of `object_type` the caller may read, with the named
/// properties plus `id` and `version`, from the tenant's engine, exact at the
/// cell's verified head (or a dataset's current load). The caller needs
/// Population and Values on the type; quarantined rows are left out, as on a
/// page. The host streams the engine's file; the snapshot it is exact at is
/// the `Norn-Snapshot` header (`c<commit>` or `l<load>`).
///
/// # Errors
/// A denied gate (one opaque 403), an unknown type (404), an unknown column
/// or format (400), or an unreachable engine.
#[utoipa::path(get, path = "/v1/objects/{type}:export", params(("type" = String, Path, description = "The concept to export."), ExportQuery), responses((status = 200, description = "The rows, as Parquet (`application/vnd.apache.parquet`) or JSON lines (`application/x-ndjson`); `Norn-Snapshot` names the snapshot they are exact at.", content_type = "application/vnd.apache.parquet"), (status = 400, description = "The request was refused as invalid.", body = Refusal), (status = 403, description = "The request was refused: scope or authority denied.", body = Refusal), (status = 404, description = "The request was refused: not found.", body = Refusal), (status = 500, description = "The request was refused: host failure.", body = Refusal)))]
pub fn export_objects<E: SqlEngine>(
    host: &DevHost<E>,
    query: &ExportQuery,
    actor: Option<&str>,
    object_type: &str,
    access: &dyn ObjectAccess,
    transport: &dyn Transport,
) -> Result<(), Refusal> {
    let scope = ScopeQuery {
        org_id: query.org_id.clone(),
        environment_id: query.environment_id.clone(),
    };
    if !uniform_gate(host, &scope, actor, object_type, access)? {
        return Err(crate::authz::read_denied());
    }
    let format = match query.format.as_deref() {
        None | Some("parquet") => ExportFormat::Parquet,
        Some("jsonl") => ExportFormat::Jsonl,
        Some(_) => return Err(Refusal::invalid("format is parquet or jsonl")),
    };
    let columns: Vec<String> = query
        .columns
        .split(',')
        .filter(|c| !c.is_empty())
        .map(str::to_owned)
        .collect();
    let guard = host.cell()?;
    let store = guard.store();
    let tenant = TenantId::of_scope(&host.scope().key());
    let (source, snapshot) = store
        .analytics_source(norn_cell::MAIN_WORLD, &host.scope().key(), object_type)
        .map_err(engine_failure)?;
    let mut call = EngineCall::new(
        "export",
        &ExportRequest {
            tenant: tenant.clone(),
            source: source.clone(),
            columns,
            format,
        },
    )
    .map_err(engine_failure)?;
    call.stream = Some(BTreeMap::from([
        ("norn-snapshot".to_owned(), snapshot),
        ("norn-withheld-rows".to_owned(), "0".to_owned()),
    ]));
    let answer = exchange(store, transport, &tenant, mirror_world(&source), &call)
        .map_err(engine_failure)?;
    // A streamed export ends the request with the call; any answer that
    // re-enters is the engine's refusal.
    Err(engine_failure(refused(&answer.body)))
}

/// `POST /v1/datasets:load` body.
#[derive(Debug, Clone, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DatasetLoadRequest {
    /// A completed upload of this cell.
    pub upload_id: String,
    /// The admitted object type the rows are typed by.
    pub object_type: String,
}

/// A completed upload, as the cell recorded it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadRecord {
    /// The object's durable URI.
    pub uri: String,
    /// Its SHA-256, computed by the cell.
    pub sha256: String,
    /// Its declared content type.
    pub content_type: String,
    /// The file's name, as uploaded.
    pub file_name: String,
}

/// The cell's completed uploads and the object store they live in.
pub trait Uploads {
    /// The completed upload `id`, if any.
    fn record(&self, id: &str) -> Option<UploadRecord>;
    /// A GET URL for exactly `uri`, valid for `seconds` from `now_ms`.
    fn presigned_get(&self, uri: &str, seconds: u64, now_ms: u64) -> Option<String>;
}

/// The outcome of `POST /v1/datasets:load`.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornDatasetLoad {
    /// The dataset's object type.
    pub object_type: String,
    /// The `upload` connection the load is reported under (SYNC-REPORT-1).
    pub connection_id: String,
    /// The load identity; answers over the dataset name it.
    pub load: String,
    /// The file's durable URI.
    pub uri: String,
    /// The file's SHA-256, verified by the engine.
    pub sha256: String,
    /// Rows admitted.
    pub rows: u64,
    /// Rows quarantined.
    pub rejected: u64,
    /// Quarantined rows per reason code (`bulk.*`).
    pub reasons: BTreeMap<String, u64>,
    /// File size in bytes.
    pub bytes: u64,
    /// Load time in milliseconds.
    pub millis: u64,
}

/// BULK-LOAD-1: load a completed upload into the tenant's engine as the
/// dataset `objectType`, typed by that admitted definition. The engine reads
/// the object through a presigned URL, verifies its SHA-256, types every row,
/// and quarantines a row it cannot type (or that misses a required value)
/// with its reason. The load becomes the dataset's current one; each row
/// traces to `(uri, sha256, row)`. Decided like any upload load
/// (`SyncSource("upload")`); the load is reported as a run of the type's
/// `upload` connection, as the cell's own observations (SYNC-REPORT-1).
///
/// # Errors
/// A denied caller (one opaque 403), an unknown upload or type (404), a
/// changed file (`bulk.digest_mismatch`, 422), or an unreachable engine.
#[utoipa::path(post, path = "/v1/datasets:load", params(ScopeQuery), request_body = DatasetLoadRequest, responses((status = 200, description = "The load.", body = NornDatasetLoad), (status = 400, description = "The request was refused as invalid.", body = Refusal), (status = 403, description = "The request was refused: scope or authority denied.", body = Refusal), (status = 404, description = "The request was refused: not found.", body = Refusal), (status = 422, description = "The file was refused.", body = Refusal), (status = 500, description = "The request was refused: host failure.", body = Refusal)))]
pub fn load_dataset<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: Option<&str>,
    body: &str,
    uploads: &dyn Uploads,
    transport: &dyn Transport,
    now_ms: u64,
) -> Result<NornDatasetLoad, Refusal> {
    crate::definition_routes::authorize(
        host,
        scope,
        actor,
        Operation::SyncSource("upload".to_owned()),
    )?;
    let request: DatasetLoadRequest =
        serde_json::from_str(body).map_err(|e| Refusal::invalid(e.to_string()))?;
    // UPLOAD-RECORD-1: an upload this host instance never saw complete is
    // read from the record the cell kept at `:complete`.
    let record = match uploads.record(&request.upload_id) {
        Some(record) => record,
        None => crate::sources::kept_upload(host, &request.upload_id)
            .map_err(Refusal::host)?
            .map(|kept| UploadRecord {
                uri: kept.uri,
                sha256: kept.sha256,
                content_type: kept.content_type,
                file_name: kept.file_name,
            })
            .ok_or_else(dataset_not_found)?,
    };
    let loaded = load_upload(
        host,
        &request.object_type,
        &record,
        uploads,
        transport,
        now_ms,
    )?;
    // The type's upload connection, whichever path loads it: an `:define`d
    // type `upload.<Name>` reports where a cell load of it does.
    let named = request.object_type.strip_prefix("upload.");
    let connection_id = format!(
        "upload.{}",
        named.unwrap_or(&request.object_type).to_ascii_lowercase()
    );
    // Reported after the cell is released: the report commits through it.
    crate::sources::report_upload(
        host,
        &crate::sources::UploadRun {
            id: &connection_id,
            label: &record.file_name,
            uri: &record.uri,
            sha256: &record.sha256,
            mapping: "",
            actor: actor.unwrap_or_default(),
            started_ms: now_ms,
            ended_ms: now_ms.saturating_add(loaded.millis),
            counts: (
                loaded.rows.saturating_add(loaded.rejected),
                loaded.rows,
                loaded.rejected,
            ),
            reasons: loaded.reasons.clone(),
        },
    );
    Ok(NornDatasetLoad {
        object_type: request.object_type,
        connection_id,
        load: loaded.load,
        uri: record.uri,
        sha256: record.sha256,
        rows: loaded.rows,
        rejected: loaded.rejected,
        reasons: loaded.reasons,
        bytes: loaded.bytes,
        millis: loaded.millis,
    })
}

/// Load `record` into the engine as the dataset `object_type` and record it
/// as the type's current load.
fn load_upload<E: SqlEngine>(
    host: &DevHost<E>,
    object_type: &str,
    record: &UploadRecord,
    uploads: &dyn Uploads,
    transport: &dyn Transport,
    now_ms: u64,
) -> Result<LoadResponse, Refusal> {
    let guard = host.cell()?;
    let store = guard.store();
    let table = store
        .analytics_table(object_type)
        .map_err(|_unknown| dataset_not_found())?;
    let format = if record.content_type.contains("parquet") || record.uri.ends_with(".parquet") {
        FileFormat::Parquet
    } else {
        FileFormat::Csv
    };
    let load = format!(
        "l{}",
        hex_digest(format!("{}\n{}\n{}", record.uri, record.sha256, table.pin).as_bytes())
    );
    let mut load_request = LoadRequest {
        tenant: TenantId::of_scope(&host.scope().key()),
        table,
        load,
        file: FileSource {
            url: String::new(),
            uri: record.uri.clone(),
            sha256: record.sha256.clone(),
            format,
        },
        headers: BTreeMap::new(),
    };
    // The URL is minted per request; the call's identity is the load without it.
    let mut call = EngineCall::new("load", &load_request).map_err(engine_failure)?;
    load_request.file.url = uploads
        .presigned_get(&record.uri, LOAD_URL_SECONDS, now_ms)
        .ok_or_else(|| Refusal::host("the object store is not configured"))?;
    call.body = serde_json::to_string(&load_request).map_err(Refusal::host)?;
    let answer =
        exchange(store, transport, &load_request.tenant, None, &call).map_err(engine_failure)?;
    if answer.status != 200 {
        let refusal: EngineRefusal = decode(&answer.body).map_err(engine_failure)?;
        return Err(Refusal::new(
            http::StatusCode::UNPROCESSABLE_ENTITY,
            &refusal.code,
            "the dataset was not loaded",
            &refusal.detail,
        ));
    }
    let loaded: LoadResponse = decode(&answer.body).map_err(engine_failure)?;
    let recorded_at = store.verified_head().map_err(Refusal::host)?.commits;
    store
        .record_analytics_dataset(
            object_type,
            &DatasetRecord {
                load: loaded.load.clone(),
                uri: record.uri.clone(),
                sha256: record.sha256.clone(),
                rows: loaded.rows,
                rejected: loaded.rejected,
                reasons: loaded.reasons.clone(),
                recorded_at,
            },
        )
        .map_err(Refusal::host)?;
    Ok(loaded)
}

fn dataset_not_found() -> Refusal {
    Refusal::new(
        http::StatusCode::NOT_FOUND,
        "dataset.not_found",
        "no such upload or type",
        "load a completed upload into an admitted object type",
    )
}

/// HOST-READ-5 on the engine (D102): the profile of every row of
/// `object_type` the caller may read, in the exact profile's shape and
/// semantics (nearest-rank quantiles, equal-population bins, calendar labels,
/// Tukey far-out values), computed by the tenant's engine over the same
/// snapshot an export reads. A caller without Values on the type sees the
/// rows counted as withheld and no value statistic.
///
/// # Errors
/// A denied gate (one opaque 403), an unknown type (404), or an unreachable engine.
pub fn profile_objects<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: Option<&str>,
    object_type: &str,
    access: &dyn ObjectAccess,
    transport: &dyn Transport,
) -> Result<crate::object_profile::NornObjectProfile, Refusal> {
    use norn_cell::analytics::{ProfileRequest, ProfileResponse};
    let values = uniform_gate(host, scope, actor, object_type, access)?;
    let guard = host.cell()?;
    let store = guard.store();
    let declared = store
        .model()
        .effective_property_types(object_type)
        .map_err(|_unknown| crate::object_routes::not_found())?;
    let table = store.analytics_table(object_type).map_err(engine_failure)?;
    let tenant = TenantId::of_scope(&host.scope().key());
    let (source, _snapshot) = store
        .analytics_source(norn_cell::MAIN_WORLD, &host.scope().key(), object_type)
        .map_err(engine_failure)?;
    let call = EngineCall::new(
        "profile",
        &ProfileRequest {
            tenant: tenant.clone(),
            source: source.clone(),
            columns: table.columns.iter().map(|c| c.id.clone()).collect(),
        },
    )
    .map_err(engine_failure)?;
    let answer = exchange(store, transport, &tenant, mirror_world(&source), &call)
        .map_err(engine_failure)?;
    if answer.status != 200 {
        return Err(engine_failure(refused(&answer.body)));
    }
    let profile: ProfileResponse = decode(&answer.body).map_err(engine_failure)?;
    let properties = declared
        .iter()
        .map(|(property, declared_type)| {
            let column = profile
                .columns
                .iter()
                .find(|c| &c.column == property)
                .filter(|_| values);
            let rows = if values { profile.rows } else { 0 };
            property_profile(property, declared_type.as_ref(), column, rows)
        })
        .collect::<Result<_, _>>()?;
    Ok(crate::object_profile::NornObjectProfile {
        object_type: object_type.to_owned(),
        rows: if values { profile.rows } else { 0 },
        withheld_rows: if values { 0 } else { profile.rows },
        quarantined_rows: profile.quarantined,
        approximate: false,
        scan_limit: profile.rows,
        engine: "duckdb".into(),
        properties,
    })
}

/// One property's profile from the engine's sorted values: a property
/// without a column (computed) counts every row as null.
fn property_profile(
    property: &str,
    declared: Option<&norn_ir::PropertyType>,
    column: Option<&norn_cell::analytics::ColumnProfile>,
    rows: u64,
) -> Result<crate::object_profile::NornPropertyProfile, Refusal> {
    use crate::object_profile::{
        NornBin, NornHistogram, NornPropertyProfile, NornQuantile, kind_of,
    };
    use norn_cell::analytics::{PROFILE_QUANTILES, bin_ranks, nearest_rank, runtime_value};
    let kind = kind_of(declared);
    let mut out = NornPropertyProfile {
        property: property.to_owned(),
        kind: kind.to_owned(),
        count: column.map_or(0, |c| c.count),
        nulls: column.map_or(rows, |c| c.nulls),
        distinct: column.map_or(0, |c| c.distinct),
        min: None,
        max: None,
        quantiles: Vec::new(),
        histogram: None,
        outliers: None,
    };
    let Some(column) = column.filter(|c| c.count > 0 && (kind == "numeric" || kind == "temporal"))
    else {
        return Ok(out);
    };
    let n = column.count;
    let at = |rank: u64| {
        column
            .ranks
            .get(&rank)
            .map(|v| runtime_value(v).map_err(Refusal::host))
            .transpose()
    };
    out.min = at(1)?;
    out.max = at(n)?;
    for percent in PROFILE_QUANTILES {
        if let Some(value) = at(nearest_rank(percent, n))? {
            out.quantiles.push(NornQuantile { percent, value });
        }
    }
    if kind == "numeric" {
        let mut bins = Vec::new();
        for (first, last) in bin_ranks(n) {
            if let (Some(lower), Some(upper)) = (at(first)?, at(last)?) {
                bins.push(NornBin {
                    lower,
                    upper,
                    count: last.saturating_sub(first).saturating_add(1),
                });
            }
        }
        out.histogram = Some(NornHistogram {
            kind: "equal_population".into(),
            bins,
        });
        if let (Some(found), Some(q1), Some(q3)) = (
            &column.outliers,
            at(nearest_rank(25, n))?,
            at(nearest_rank(75, n))?,
        ) {
            out.outliers = Some(outliers_of(found, q1, q3)?);
        }
    } else if let Some((kind, labels)) = &column.labels {
        out.histogram = Some(NornHistogram {
            kind: kind.clone(),
            bins: labels
                .iter()
                .map(|(label, rows)| NornBin {
                    lower: norn_ir::RuntimeValue::String(label.clone()),
                    upper: norn_ir::RuntimeValue::String(label.clone()),
                    count: *rows,
                })
                .collect(),
        });
    }
    Ok(out)
}

fn outliers_of(
    found: &norn_cell::analytics::OutlierSummary,
    q1: norn_ir::RuntimeValue,
    q3: norn_ir::RuntimeValue,
) -> Result<crate::object_profile::NornOutliers, Refusal> {
    use crate::object_profile::{NornOutlier, NornOutliers};
    let number = |text: &str| {
        norn_ir::Decimal::parse(text)
            .map(norn_ir::RuntimeValue::Number)
            .map_err(Refusal::host)
    };
    Ok(NornOutliers {
        method: "tukey_far_out".into(),
        k: norn_cell::analytics::PROFILE_OUTLIER_K.to_string(),
        q1,
        q3,
        lower_fence: number(&found.lower)?,
        upper_fence: number(&found.upper)?,
        count: found.count,
        examples: found
            .examples
            .iter()
            .map(|(id, value)| {
                Ok(NornOutlier {
                    id: id.clone(),
                    value: number(value)?,
                })
            })
            .collect::<Result<_, Refusal>>()?,
    })
}
