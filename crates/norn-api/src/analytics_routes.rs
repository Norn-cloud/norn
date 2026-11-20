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
    EngineRefusal, EvaluateRequest, EvaluateResponse, ExportFormat, ExportRequest, FileFormat,
    FileSource, FrontierResponse, LandRequest, LandResponse, LoadRequest, Source, StoreRequest,
    StoredObject, TenantId, dataset_cache_id, hex_digest,
};
use norn_cell::compute::{ALL_MEMBERS, QueryContext, QueryResult};
use norn_cell::dataset_manifest::{
    CommittedManifest, DatasetManifest, DatasetRequest, MANIFEST_VERSION, ManifestCounts,
    ManifestFile, ManifestSource,
};
use norn_cell::engine::SqlEngine;
use norn_cell::{CellError, CellStore, ObjectRecord};
use norn_checker::EffectiveCatalog;
use norn_checker::metric::compute::{QuestionPlan, UNSUPPORTED_ENGINE};
use norn_ir::{MetricDefinition, QuestionIR};
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
/// Lifetime of the presigned URLs a load reads and lands through, in seconds.
const LOAD_URL_SECONDS: u64 = 900;

/// One engine operation, as the host sends it: `POST /v1/<op>` with `body`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EngineCall {
    /// `frontier`, `sync`, `evaluate`, `members`, `profile`, `page`, `land`,
    /// `store` or `export`.
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
        let response = self.respond(store, &prepared, admitted)?;
        let mut result = store.analytics_answer(prepared, response)?;
        finish_answer(store, plan, &mut result, None, visible)?;
        Ok(result)
    }
    /// ASOF-KNOWLEDGE-1 on the engine: the answer as known at `cutoff`, from
    /// the mirror at the cutoff's frontier, in the exact engine's historical
    /// envelope; gated as a current answer is.
    fn evaluate_known_at(
        &self,
        store: &CellStore<E>,
        plan: &QuestionPlan,
        context: QueryContext,
        cutoff: norn_cell::KnowledgeCutoff,
        visible: &mut dyn FnMut(&ObjectRecord) -> Result<bool, CellError>,
    ) -> Result<norn_cell::compute::HistoricalQuestionResult, CellError> {
        let grain = plan.grain().as_str().to_owned();
        let admitted = visible(&gate_record(&grain, ALL_MEMBERS, &context.scope))?;
        let prepared = store.analytics_prepare_known_at(plan, context, &self.tenant, cutoff)?;
        let response = self.respond(store, &prepared, admitted)?;
        let mut answer = store.analytics_answer_known_at(prepared, response)?;
        finish_answer(store, plan, &mut answer.result, Some(cutoff), visible)?;
        Ok(answer)
    }
}

impl<E: SqlEngine> EngineQuestions<'_, E> {
    /// The engine's answer to `prepared`, or none when the caller may not
    /// count the grain's rows.
    fn respond(
        &self,
        store: &CellStore<E>,
        prepared: &norn_cell::analytics::Prepared,
        admitted: bool,
    ) -> Result<EvaluateResponse, CellError> {
        if admitted {
            evaluate_on(store, self.transport, &self.tenant, &prepared.request)
        } else {
            Ok(EvaluateResponse {
                population: 0,
                excluded: Vec::new(),
                buckets: Vec::new(),
                unlinked: 0,
            })
        }
    }
}

/// Gate each listed member, then count disputed members (SOURCE-AGREE-1) with
/// the exact engine's own function, at the answer's cutoff when it has one.
fn finish_answer<E: SqlEngine>(
    store: &CellStore<E>,
    plan: &QuestionPlan,
    result: &mut QueryResult,
    cutoff: Option<norn_cell::KnowledgeCutoff>,
    visible: &mut dyn FnMut(&ObjectRecord) -> Result<bool, CellError>,
) -> Result<(), CellError> {
    let grain = plan.grain().as_str();
    let scope = result.context.scope.clone();
    for row in result.rows.values() {
        for id in row.members.keys() {
            if !visible(&gate_record(grain, id, &scope))? {
                return Err(CellError::ComputeRefused(UNSUPPORTED_ENGINE));
            }
        }
    }
    result.source_disagreements = store.answer_source_disagreements(
        plan,
        &result.context.clone(),
        (result.known_as_of, cutoff),
        visible,
    )?;
    Ok(())
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
    /// The wrapped runtime admits: a derived question keeps its default unit
    /// and currency grouping on the engine (METRIC-UNIT-PARTITION-1).
    fn admit(
        &self,
        question: &QuestionIR,
        max_rows: u32,
    ) -> Result<QuestionPlan, Vec<norn_checker::Diagnostic>> {
        self.inner.admit(question, max_rows)
    }
    fn unit_partition(&self, metric: &str) -> Vec<String> {
        self.inner.unit_partition(metric)
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
    /// Comma-separated stored property ids, each once; each row also carries
    /// its own `id` and `version`, so no property of those names is exported.
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
/// A denied gate (one opaque 403), an unknown type (404), an unknown,
/// repeated or reserved column or an unknown format (400), or an
/// unreachable engine.
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
    // PRIVACY-1: an export carries stored values, so a cell that declares
    // markings exports nothing; a marked value leaves only through a reveal.
    if !host.marking_catalog(&store.read())?.is_empty() {
        return Err(crate::authz::read_denied());
    }
    let table = store.analytics_table(object_type).map_err(engine_failure)?;
    let mut named = std::collections::BTreeSet::new();
    for column in &columns {
        if ["id", "version"].contains(&column.as_str()) {
            return Err(Refusal::invalid(format!(
                "`{column}` is each row's own column; a property of that name is not exported"
            )));
        }
        if !table.columns.iter().any(|c| &c.id == column) {
            return Err(Refusal::invalid(format!(
                "`{column}` is not a stored property of {object_type}"
            )));
        }
        if !named.insert(column) {
            return Err(Refusal::invalid(format!("`{column}` is named twice")));
        }
    }
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
    /// MANIFEST-03: the admitted mapping onto the type the load binds its
    /// columns by; needed only when more than one maps onto it.
    #[serde(default)]
    pub mapping: Option<String>,
    /// MANIFEST-04: the caller's key for this load. A retry with the same
    /// key and the same request answers with the first load's receipt; the
    /// same key with another request is refused (409
    /// `dataset.request_conflict`). A load without a key always commits.
    #[serde(default)]
    pub request_key: Option<String>,
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
    /// Whether `id` is an upload identity this host issued, readable from the
    /// cell scope `scope_key`, sealed to the object at exactly `uri`
    /// (UPLOAD-ID-1). A kept `source.Upload` record names a URI; only this
    /// answer makes it one the cell may sign (S5).
    fn issued(&self, id: &str, scope_key: &str, uri: &str) -> bool;
    /// A GET URL for exactly `uri`, valid for `seconds` from `now_ms`.
    fn presigned_get(&self, uri: &str, seconds: u64, now_ms: u64) -> Option<String>;
    /// MANIFEST-LAND-1: a PUT URL for the object `key` the cell lands a
    /// dataset file at, valid for `seconds` from `now_ms`, and its durable URI.
    fn presigned_put(&self, key: &str, seconds: u64, now_ms: u64) -> Option<(String, String)>;
}

/// The outcome of `POST /v1/datasets:load`.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornDatasetLoad {
    /// The dataset's object type.
    pub object_type: String,
    /// The `upload` connection the load is reported under (SYNC-REPORT-1).
    pub connection_id: String,
    /// The landing identity; answers over the dataset name it.
    pub load: String,
    /// MANIFEST-1: the committed manifest's identity.
    pub manifest: String,
    /// The receipt of the one commit that admitted the manifest.
    pub receipt: String,
    /// That commit's sequence number.
    pub commit_seq: u64,
    /// The source file's durable URI.
    pub uri: String,
    /// The source file's SHA-256, verified by the engine.
    pub sha256: String,
    /// The landed data file: every admitted row, columnar.
    pub data: NornDatasetFile,
    /// The landed held-set file: every held-back row and its reason.
    pub held: NornDatasetFile,
    /// Rows admitted.
    pub rows: u64,
    /// Rows quarantined.
    pub rejected: u64,
    /// Quarantined rows per reason code (`bulk.*`).
    pub reasons: BTreeMap<String, u64>,
    /// Source file size in bytes.
    pub bytes: u64,
    /// Load time in milliseconds.
    pub millis: u64,
}

/// One landed dataset file, named by content.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornDatasetFile {
    /// The object's durable URI.
    pub uri: String,
    /// Its SHA-256.
    pub sha256: String,
    /// Rows in it.
    pub rows: u64,
    /// Bytes in it.
    pub bytes: u64,
}

impl From<&ManifestFile> for NornDatasetFile {
    fn from(file: &ManifestFile) -> Self {
        Self {
            uri: file.uri.clone(),
            sha256: file.sha256.clone(),
            rows: file.rows,
            bytes: file.bytes,
        }
    }
}

/// BULK-LOAD-1: load a completed upload into the tenant's engine as the
/// dataset `objectType`, typed by that admitted definition. The engine reads
/// the object through a presigned URL, verifies its SHA-256, types every row,
/// and quarantines a row it cannot type (or that misses a required value)
/// with its reason. The load becomes the dataset's current one when its
/// manifest commits; each row traces to `(uri, sha256, row)`. The load is
/// reported as a run of the type's `upload` connection, as the cell's own
/// observations (SYNC-REPORT-1).
///
/// MANIFEST-03: a load is decided exactly as an observation load of the
/// same rows: `SyncSource("upload")` and the destination connection's own
/// grant (`SyncSource("upload.<type>")`), an admitted mapping onto the type
/// that the load binds its columns by, and the markings a write of those
/// rows would carry. A mapping or marking bulk ingest cannot enforce is
/// refused, never loaded unchecked.
///
/// # Errors
/// A denied caller (one opaque 403), an unknown upload or type (404), a
/// changed file (`bulk.digest_mismatch`, 422), a mapping or marking bulk
/// ingest cannot enforce (422), stale pins or a reused request key (409), or
/// an unreachable engine.
#[utoipa::path(post, path = "/v1/datasets:load", params(ScopeQuery), request_body = DatasetLoadRequest, responses((status = 200, description = "The load.", body = NornDatasetLoad), (status = 400, description = "The request was refused as invalid.", body = Refusal), (status = 403, description = "The request was refused: scope or authority denied.", body = Refusal), (status = 404, description = "The request was refused: not found.", body = Refusal), (status = 409, description = "The definitions changed while the dataset landed, or the request key names another request.", body = Refusal), (status = 422, description = "The file was refused.", body = Refusal), (status = 500, description = "The request was refused: host failure.", body = Refusal)))]
#[allow(clippy::too_many_lines, reason = "one load, in order: authorize, contract, land, commit, report")]
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
    // The type's upload connection, whichever path loads it: an `:define`d
    // type `upload.<Name>` reports where a cell load of it does.
    let named = request.object_type.strip_prefix("upload.");
    let connection_id = format!(
        "upload.{}",
        named.unwrap_or(&request.object_type).to_ascii_lowercase()
    );
    // MANIFEST-03: the destination's own grant, as a sync of it is decided.
    crate::definition_routes::authorize(
        host,
        scope,
        actor,
        Operation::SyncSource(connection_id.clone()),
    )?;
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
    // S5: a record is data in the cell, not the host's word. Before anything
    // is signed or the engine is called, the upload id must be one this host
    // issued for this scope, sealed to the very URI the record names; a
    // record pointing anywhere else answers as no such upload.
    if !uploads.issued(&request.upload_id, &host.scope().key(), &record.uri) {
        return Err(dataset_not_found());
    }
    let contract = bulk_contract(host, &request.object_type, request.mapping.as_deref())?;
    let keyed = request
        .request_key
        .as_deref()
        .map(|key| keyed_request(key, &request, &record, &contract))
        .transpose()?;
    // MANIFEST-04: a request already committed answers with its first receipt.
    if let Some(keyed) = &keyed
        && let Some(first) = committed_request(host, keyed)?
    {
        return Ok(replayed(
            &request.object_type,
            connection_id,
            &record,
            first,
        ));
    }
    let landed = land_upload(
        host,
        &request.object_type,
        &record,
        &contract,
        uploads,
        transport,
        now_ms,
    )?;
    let (manifest, hash, receipt) =
        match commit_manifest(host, &request.object_type, &record, &landed, keyed.as_ref()) {
            Ok(committed) => committed,
            // A concurrent commit of the same key: its request decides.
            Err(conflict) if conflict.code == "dataset.request_conflict" => {
                let Some(first) = keyed
                    .as_ref()
                    .map(|keyed| committed_request(host, keyed))
                    .transpose()?
                    .flatten()
                else {
                    return Err(conflict);
                };
                return Ok(replayed(
                    &request.object_type,
                    connection_id,
                    &record,
                    first,
                ));
            }
            Err(refusal) => return Err(refusal),
        };
    let loaded = &landed.land.load;
    // Reported after the cell is released: the report commits through it.
    crate::sources::report_upload(
        host,
        &crate::sources::UploadRun {
            id: &connection_id,
            label: &record.file_name,
            uri: &record.uri,
            sha256: &record.sha256,
            mapping: &contract.mapping_id,
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
        load: manifest.load.clone(),
        manifest: hash,
        receipt: receipt.hash,
        commit_seq: receipt.body.commit_seq,
        uri: record.uri,
        sha256: record.sha256,
        data: (&manifest.data).into(),
        held: (&manifest.held).into(),
        rows: loaded.rows,
        rejected: loaded.rejected,
        reasons: loaded.reasons.clone(),
        bytes: loaded.bytes,
        millis: loaded.millis,
    })
}

/// MANIFEST-03: what an observation load of the type would be decided by,
/// as bulk ingest enforces it: the admitted mapping onto the type and the
/// column binding it reads.
struct BulkContract {
    /// The mapping's artifact id.
    mapping_id: String,
    /// `<artifact id>@<sha256 of its canonical artifact>`.
    mapping_pin: String,
    /// The file column of each property read from a differently named field.
    binding: BTreeMap<String, String>,
}

fn unsupported(code: &str, detail: impl Into<String>) -> Refusal {
    Refusal::new(
        http::StatusCode::UNPROCESSABLE_ENTITY,
        code,
        "the dataset was not loaded",
        detail.into(),
    )
}

/// The source field a mapped property's value reads, if it reads one
/// field as itself: `input.<f>`, or `timestamp(input.<f>)` (with its null
/// guard), the conversion the engine's typing of a timestamp column is.
fn plain_read(value: &str) -> Option<&str> {
    fn field(text: &str) -> Option<&str> {
        text.strip_prefix("input.")
            .filter(|f| !f.is_empty() && f.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
    }
    fn converted(text: &str) -> Option<&str> {
        text.strip_prefix("timestamp(")
            .and_then(|rest| rest.strip_suffix(')'))
            .and_then(field)
    }
    if let Some(read) = field(value) {
        return Some(read);
    }
    if let Some(read) = converted(value) {
        return Some(read);
    }
    let (test, rest) = value.split_once(" == null ? null : ")?;
    let read = field(test)?;
    (converted(rest)? == read).then_some(read)
}

/// MANIFEST-03: the admitted mapping onto `object_type` a bulk load binds
/// its columns by (the one `named`, or the only one), its binding, and the
/// write-side markings check: a load of a type whose rows a write would
/// mark (a declared property marking, or a marked source field) is refused,
/// as bulk ingest attaches no per-row markings.
#[allow(clippy::too_many_lines, reason = "one contract, read top to bottom: the mapping, its reads, then the markings")]
fn bulk_contract<E: SqlEngine>(
    host: &DevHost<E>,
    object_type: &str,
    named: Option<&str>,
) -> Result<BulkContract, Refusal> {
    let set: Vec<serde_json::Value> = match host.active_definitions()? {
        Some(active) => serde_json::from_str(&active.canonical_set).map_err(Refusal::host)?,
        None => Vec::new(),
    };
    let kind = |artifact: &serde_json::Value| {
        artifact
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    };
    let id_of = |artifact: &serde_json::Value| {
        artifact
            .get("artifactId")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let entities: Vec<String> = set
        .iter()
        .filter(|artifact| {
            kind(artifact).as_deref() == Some("entity")
                && artifact
                    .pointer("/payload/objectType/name")
                    .and_then(serde_json::Value::as_str)
                    == Some(object_type)
        })
        .map(id_of)
        .collect();
    let maps_of = |artifact: &serde_json::Value| -> Vec<serde_json::Value> {
        artifact
            .pointer("/payload/maps")
            .and_then(serde_json::Value::as_array)
            .map(|maps| {
                maps.iter()
                    .filter(|map| {
                        map.pointer("/target/entity")
                            .and_then(serde_json::Value::as_str)
                            .is_some_and(|entity| entities.iter().any(|e| e == entity))
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    };
    let candidates: Vec<&serde_json::Value> = set
        .iter()
        .filter(|artifact| kind(artifact).as_deref() == Some("mapping"))
        .filter(|artifact| named.is_none_or(|named| id_of(artifact) == named))
        .filter(|artifact| !maps_of(artifact).is_empty())
        .collect();
    let mapping = match candidates.as_slice() {
        [one] => *one,
        [] => {
            return Err(unsupported(
                "bulk.unmapped",
                format!(
                    "no admitted mapping onto {object_type} binds the load; admit one, as an observation load needs"
                ),
            ));
        }
        _ => {
            return Err(Refusal::invalid(format!(
                "more than one admitted mapping maps onto {object_type}; name one as mapping"
            )));
        }
    };
    let maps = maps_of(mapping);
    let [map] = maps.as_slice() else {
        return Err(unsupported(
            "bulk.unsupported_mapping",
            "the mapping maps onto the type more than once; load it through observations",
        ));
    };
    // A map that filters, joins or computes beyond reading its fields is
    // not one bulk ingest can enforce.
    let plain = map.as_object().is_some_and(|fields| {
        fields.keys().all(|key| {
            matches!(
                key.as_str(),
                "id" | "source" | "target" | "subject" | "sourceRecord" | "properties"
            )
        })
    });
    if !plain {
        return Err(unsupported(
            "bulk.unsupported_mapping",
            "the mapping does more than read fields; load it through observations",
        ));
    }
    let guard = host.cell()?;
    let store = guard.store();
    let table = store
        .analytics_table(object_type)
        .map_err(|_unknown| dataset_not_found())?;
    let mut reads = BTreeMap::new();
    for entry in map
        .get("properties")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
    {
        let (Some(property), Some(value)) = (
            entry.get("property").and_then(serde_json::Value::as_str),
            entry.get("value").and_then(serde_json::Value::as_str),
        ) else {
            return Err(unsupported(
                "bulk.unsupported_mapping",
                "a mapped property is not a plain field read",
            ));
        };
        let field = plain_read(value).ok_or_else(|| {
            unsupported(
                "bulk.unsupported_mapping",
                format!("{property} is computed by the mapping; load it through observations"),
            )
        })?;
        reads.insert(property.to_owned(), field.to_owned());
    }
    let mut binding = BTreeMap::new();
    for column in &table.columns {
        let field = reads.get(&column.id).ok_or_else(|| {
            unsupported(
                "bulk.unsupported_mapping",
                format!("the mapping does not map {}", column.id),
            )
        })?;
        if *field != column.id {
            binding.insert(column.id.clone(), field.clone());
        }
    }
    // Write-side markings: a declared marking of a column, or a marked
    // source field the mapping reads, would mark every row a write lands.
    let declared = store
        .model()
        .effective_property_markings(object_type)
        .map_err(Refusal::host)?;
    if let Some((column, _)) = declared.iter().find(|(column, markings)| {
        !markings.is_empty() && table.columns.iter().any(|c| c.id == **column)
    }) {
        return Err(unsupported(
            "bulk.unsupported_marking",
            format!(
                "{column} is marked, and a dataset load attaches no markings; load it through observations"
            ),
        ));
    }
    drop(guard);
    let schema_id = mapping
        .pointer("/payload/source/schemaId")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    let read: Vec<&String> = reads.values().collect();
    let marked = set
        .iter()
        .filter(|artifact| id_of(artifact) == schema_id)
        .filter_map(|schema| {
            schema
                .pointer("/payload/fields")
                .and_then(serde_json::Value::as_array)
        })
        .flatten()
        .find(|field| {
            field
                .get("name")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|name| read.iter().any(|read| *read == name))
                && field
                    .get("markings")
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|markings| !markings.is_empty())
        });
    if let Some(field) = marked {
        return Err(unsupported(
            "bulk.unsupported_marking",
            format!(
                "the source field {} is marked, and a dataset load attaches no markings; load it through observations",
                field
                    .get("name")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
            ),
        ));
    }
    let mapping_id = id_of(mapping);
    let canonical = serde_json::to_vec(mapping).map_err(Refusal::host)?;
    Ok(BulkContract {
        mapping_pin: format!("{mapping_id}@{}", hex_digest(&canonical)),
        mapping_id,
        binding,
    })
}

/// MANIFEST-04: a request key and the fingerprint of what it asks: the
/// upload (by its recorded identity), the type and the mapping.
fn keyed_request(
    key: &str,
    request: &DatasetLoadRequest,
    record: &UploadRecord,
    contract: &BulkContract,
) -> Result<DatasetRequest, Refusal> {
    let valid = !key.is_empty()
        && key.len() <= 128
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'));
    if !valid {
        return Err(Refusal::invalid(
            "requestKey is 1 to 128 letters, digits, '-', '_', '.' or ':'",
        ));
    }
    let fingerprint = norn_ir::canonical::domain_hash_hex(
        b"norn.dataset-load.request.v1\0",
        &serde_json::json!({
            "objectType": request.object_type,
            "uploadId": request.upload_id,
            "uri": record.uri,
            "sha256": record.sha256,
            "mapping": contract.mapping_pin,
        }),
    )
    .map_err(Refusal::host)?;
    Ok(DatasetRequest {
        key: key.to_owned(),
        fingerprint,
    })
}

/// MANIFEST-04: the commit a request key already made, if it made one for
/// this request; the same key for another request is refused.
fn committed_request<E: SqlEngine>(
    host: &DevHost<E>,
    keyed: &DatasetRequest,
) -> Result<Option<(CommittedManifest, norn_cell::Receipt)>, Refusal> {
    let found = host
        .cell()?
        .store()
        .dataset_request(norn_cell::MAIN_WORLD, &keyed.key)
        .map_err(Refusal::host)?;
    match found {
        None => Ok(None),
        Some((committed, request, receipt)) if request.fingerprint == keyed.fingerprint => {
            Ok(Some((committed, receipt)))
        }
        Some(_) => Err(request_conflict()),
    }
}

fn request_conflict() -> Refusal {
    Refusal::new(
        http::StatusCode::CONFLICT,
        "dataset.request_conflict",
        "the dataset was not loaded",
        "the request key was used for another load request",
    )
}

/// The answer to a request already committed: its first manifest and receipt.
fn replayed(
    object_type: &str,
    connection_id: String,
    record: &UploadRecord,
    (committed, receipt): (CommittedManifest, norn_cell::Receipt),
) -> NornDatasetLoad {
    let manifest = committed.manifest;
    NornDatasetLoad {
        object_type: object_type.to_owned(),
        connection_id,
        load: manifest.load.clone(),
        manifest: committed.hash,
        receipt: receipt.hash,
        commit_seq: receipt.body.commit_seq,
        uri: record.uri.clone(),
        sha256: record.sha256.clone(),
        data: (&manifest.data).into(),
        held: (&manifest.held).into(),
        rows: manifest.counts.admitted,
        rejected: manifest.held.rows,
        reasons: manifest.counts.held.clone(),
        bytes: manifest.source.bytes,
        millis: 0,
    }
}

/// MANIFEST-CACHE-1: the content-addressed object key of a landed file:
/// `datasets/<scope>/<type>/<sha256>.parquet`. A file's object is named by
/// its bytes, so no landing overwrites another's.
fn object_key(scope_key: &str, object_type: &str, sha256: &str) -> String {
    format!("datasets/{scope_key}/{object_type}/{sha256}.parquet")
}

/// A landing whose files are stored: the request it was made by (with the
/// attempt it ran as) and the engine's answer.
struct Landing {
    request: LandRequest,
    land: LandResponse,
    uris: [String; 2],
}

/// Landing steps one load may take: the landing, then storing its files.
const MAX_LAND_STEPS: usize = 4;

/// MANIFEST-LAND-1: type `record` by `object_type` on the engine, land it
/// as the data and held-set Parquet files under a fresh attempt, and store
/// them at their content-addressed objects.
///
/// MANIFEST-01: the landing's identity is the digest of its whole request
/// without the attempt: the file, the table, the binding and the pins it is
/// typed and checked by. An answer continues this landing only if it names
/// that identity; the attempt is then the one the answer ran as, and its
/// pins are carried to the commit unchanged.
#[allow(clippy::too_many_lines, reason = "one landing, step by step: begin, land, settle, store")]
fn land_upload<E: SqlEngine>(
    host: &DevHost<E>,
    object_type: &str,
    record: &UploadRecord,
    contract: &BulkContract,
    uploads: &dyn Uploads,
    transport: &dyn Transport,
    now_ms: u64,
) -> Result<Landing, Refusal> {
    // The cell is held only while the landing is typed; the steps run
    // without it, so the definitions may change under them, and the commit
    // decides by the pins carried here.
    let (table, pins) = {
        let guard = host.cell()?;
        let store = guard.store();
        let table = store
            .analytics_table(object_type)
            .map_err(|_unknown| dataset_not_found())?;
        // MANIFEST-PIN-1: no law or invariant is checked on this path yet, so
        // a type that declares one is refused rather than admitted unchecked.
        if !store
            .model()
            .governing_laws(object_type)
            .map_err(Refusal::host)?
            .is_empty()
            || !store
                .model()
                .effective_invariants(object_type)
                .map_err(Refusal::host)?
                .is_empty()
        {
            return Err(Refusal::new(
                http::StatusCode::UNPROCESSABLE_ENTITY,
                "bulk.unsupported_law",
                "the dataset was not loaded",
                "the type declares laws or invariants, which a dataset load does not check yet; load it through observations",
            ));
        }
        let pins = store
            .dataset_pins(object_type, &contract.binding, &contract.mapping_pin)
            .map_err(Refusal::host)?;
        (table, pins)
    };
    let format = if record.content_type.contains("parquet") || record.uri.ends_with(".parquet") {
        FileFormat::Parquet
    } else {
        FileFormat::Csv
    };
    let scope_key = host.scope().key();
    let mut request = LandRequest {
        load: LoadRequest {
            tenant: TenantId::of_scope(&scope_key),
            table,
            load: String::new(),
            file: FileSource {
                url: String::new(),
                uri: record.uri.clone(),
                sha256: record.sha256.clone(),
                format,
            },
            headers: contract.binding.clone(),
        },
        pins,
        landing: String::new(),
        attempt: String::new(),
    };
    let identity = EngineCall::new("land", &request)
        .map_err(engine_failure)?
        .digest;
    request.load.load = format!("l{identity}");
    request.landing.clone_from(&identity);
    let store_digest = hex_digest(format!("store\n{identity}").as_bytes());
    let ours = |answer: &EngineAnswer| answer.digest == identity || answer.digest == store_digest;
    let mut answer = transport.resumed().filter(ours);
    // A fresh attempt is this request's own; an answer names the attempt it ran as.
    request.attempt = format!(
        "a{}",
        hex_digest(format!("{identity}\n{now_ms}").as_bytes())
            .get(..32)
            .unwrap_or_default()
    );
    let unconfigured = || Refusal::host("the object store is not configured");
    let mut call = EngineCall {
        op: "land".to_owned(),
        digest: identity.clone(),
        body: String::new(),
        stream: None,
    };
    for _ in 0..MAX_LAND_STEPS {
        let done = if let Some(done) = answer.take() {
            done
        } else {
            if call.op == "land" {
                let mut sent = request.clone();
                sent.load.file.url = uploads
                    .presigned_get(&record.uri, LOAD_URL_SECONDS, now_ms)
                    .ok_or_else(unconfigured)?;
                call.body = serde_json::to_string(&sent).map_err(Refusal::host)?;
            }
            transport
                .call(&call)
                .ok_or_else(pending)
                .map_err(engine_failure)?
        };
        if done.status != 200 {
            let refusal: EngineRefusal = decode(&done.body).map_err(engine_failure)?;
            return Err(Refusal::new(
                http::StatusCode::UNPROCESSABLE_ENTITY,
                &refusal.code,
                "the dataset was not loaded",
                &refusal.detail,
            ));
        }
        let land: LandResponse = decode(&done.body).map_err(engine_failure)?;
        if land.landing != identity {
            return Err(stale_landing());
        }
        request.attempt.clone_from(&land.attempt);
        let keys = [
            object_key(&scope_key, object_type, &land.data.sha256),
            object_key(&scope_key, object_type, &land.held.sha256),
        ];
        let mut objects = Vec::new();
        for key in &keys {
            objects.push(
                uploads
                    .presigned_put(key, LOAD_URL_SECONDS, now_ms)
                    .ok_or_else(unconfigured)?,
            );
        }
        let [(data_put, data_uri), (held_put, held_uri)] =
            <[(String, String); 2]>::try_from(objects).map_err(|_two| unconfigured())?;
        let uris = [data_uri.clone(), held_uri.clone()];
        if land.stored.is_some() {
            return Ok(Landing {
                request,
                land,
                uris,
            });
        }
        // MANIFEST-CACHE-1: the files go to objects named by their digests.
        let store_request = StoreRequest {
            land: request.clone(),
            landed: land,
            data: StoredObject {
                uri: data_uri,
                url: data_put,
            },
            held: StoredObject {
                uri: held_uri,
                url: held_put,
            },
        };
        call = EngineCall {
            op: "store".to_owned(),
            digest: store_digest.clone(),
            body: serde_json::to_string(&store_request).map_err(Refusal::host)?,
            stream: None,
        };
    }
    Err(Refusal::host("the landing did not settle"))
}

/// MANIFEST-01: an answer that is not this landing's own.
fn stale_landing() -> Refusal {
    Refusal::new(
        http::StatusCode::CONFLICT,
        "dataset.stale_landing",
        "the dataset was not loaded",
        "the engine answered for another landing; load the dataset again",
    )
}

/// MANIFEST-COMMIT-1: commit the landing's manifest as the type's current
/// dataset. Everything in it is the landing's own: the pins it was typed
/// and checked by (MANIFEST-01), its files' digests and the cache identity
/// made of them; nothing is recomputed, and pins that are no longer
/// current refuse (`dataset.stale_pins`).
fn commit_manifest<E: SqlEngine>(
    host: &DevHost<E>,
    object_type: &str,
    record: &UploadRecord,
    landing: &Landing,
    keyed: Option<&DatasetRequest>,
) -> Result<(DatasetManifest, String, norn_cell::Receipt), Refusal> {
    let land = &landing.land;
    let request = &landing.request;
    let table = &request.load.table;
    let cache = dataset_cache_id(table, &land.data.sha256, &land.held.sha256);
    if land.landing != request.landing
        || land.attempt != request.attempt
        || land.stored.as_deref() != Some(cache.as_str())
    {
        return Err(stale_landing());
    }
    let [data_uri, held_uri] = landing.uris.clone();
    let manifest = DatasetManifest {
        version: MANIFEST_VERSION.to_owned(),
        object_type: object_type.to_owned(),
        load: cache,
        source: ManifestSource {
            uri: record.uri.clone(),
            sha256: record.sha256.clone(),
            format: request.load.file.format,
            bytes: land.load.bytes,
        },
        binding: request.load.headers.clone(),
        table: table.clone(),
        pins: request.pins.clone(),
        data: ManifestFile {
            uri: data_uri,
            sha256: land.data.sha256.clone(),
            rows: land.data.rows,
            bytes: land.data.bytes,
        },
        held: ManifestFile {
            uri: held_uri,
            sha256: land.held.sha256.clone(),
            rows: land.held.rows,
            bytes: land.held.bytes,
        },
        counts: ManifestCounts {
            source: land.load.rows.saturating_add(land.load.rejected),
            admitted: land.load.rows,
            held: land.load.reasons.clone(),
        },
        valid_time: land.valid_time.clone(),
    };
    let (hash, receipt) = host
        .cell()?
        .commit_dataset_manifest(&manifest, keyed)
        .map_err(manifest_refusal)?;
    Ok((manifest, hash, receipt))
}

/// MANIFEST-CURRENT-1: the current dataset manifest of `object_type`, if it is one.
///
/// # Errors
/// A store failure.
pub fn current_manifest<E: SqlEngine>(
    host: &DevHost<E>,
    object_type: &str,
) -> Result<Option<CommittedManifest>, Refusal> {
    host.cell()?
        .store()
        .dataset_manifest(norn_cell::MAIN_WORLD, object_type)
        .map_err(Refusal::host)
}

fn manifest_refusal(error: CellError) -> Refusal {
    if let CellError::DatasetRefused { code, detail } = &error {
        return Refusal::new(
            http::StatusCode::CONFLICT,
            code,
            "the dataset manifest was not committed",
            detail,
        );
    }
    if matches!(error, CellError::IdempotencyConflict(_)) {
        return request_conflict();
    }
    Refusal::host(error)
}

fn dataset_not_found() -> Refusal {
    Refusal::new(
        http::StatusCode::NOT_FOUND,
        "dataset.not_found",
        "no such upload or type",
        "load a completed upload into an admitted object type",
    )
}

/// Whether `object_type`'s records are a loaded dataset's rows, which live
/// only in the tenant's engine.
///
/// # Errors
/// A store failure.
pub fn serves_records<E: SqlEngine>(host: &DevHost<E>, object_type: &str) -> Result<bool, Refusal> {
    let guard = host.cell()?;
    let has_type = guard.store().model().has_type(object_type);
    // PRIVACY-1: the engine mirror holds stored values, so a cell that
    // declares markings reads its records natively, where they are projected.
    if has_type && !host.marking_catalog(&guard.store().read())?.is_empty() {
        return Ok(false);
    }
    Ok(has_type
        && guard
            .store()
            .dataset_manifest(norn_cell::MAIN_WORLD, object_type)
            .map_err(Refusal::host)?
            .is_some())
}

/// HOST-READ-3 over a loaded dataset (D102): the grid query of
/// `GET /v1/objects/{type}` answered by the tenant's engine, with the native
/// grid's grammar, order and visibility: every clause holds (a withheld row
/// matches none), absent values sort last either way, then identity. The
/// dataset is immutable per load, so a cursor is an offset bound to the load,
/// the query, the scope and the caller, and a sort or filter reads every row.
///
/// # Errors
/// A denied gate (one opaque 403), an invalid query or cursor (400), a
/// comparison the engine cannot evaluate (422), a reload since the cursor
/// (409 `object.snapshot_moved`), or an unreachable engine.
pub fn page_objects<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: Option<&str>,
    object_type: &str,
    params: &crate::object_query::ObjectQueryParams,
    access: &dyn ObjectAccess,
    transport: &dyn Transport,
) -> Result<crate::objects::NornObjectQueryPage, Refusal> {
    use crate::object_query::{self, Cursor};
    use crate::objects::{NornAppliedQuery, NornObjectQueryPage, NornQueryTotal};
    use norn_cell::analytics::{PageRequest, PageResponse};
    let values = uniform_gate(host, scope, actor, object_type, access)?;
    let actor = actor.unwrap_or_default();
    let guard = host.cell()?;
    let store = guard.store();
    let properties = store
        .model()
        .effective_property_types(object_type)
        .map_err(|_unknown| crate::object_routes::not_found())?;
    let query = object_query::parse(params, &properties)?;
    let (source, snapshot) = store
        .analytics_source(norn_cell::MAIN_WORLD, &host.scope().key(), object_type)
        .map_err(engine_failure)?;
    let quarantined = store
        .dataset_manifest(norn_cell::MAIN_WORLD, object_type)
        .map_err(Refusal::host)?
        .map_or(0, |dataset| dataset.manifest.held.rows);
    let digest = norn_cell::compute::lineage::keyed_lineage_handle(
        object_query::CURSOR_KEY,
        &format!("{}\n{snapshot}", query.digest(object_type)),
    );
    let bound = [
        digest,
        Cursor::scope_digest(&host.scope().key()),
        Cursor::actor_digest(actor),
    ];
    let offset = match query.after.as_deref() {
        None => 0,
        Some(text) => Cursor::decode(text)
            .filter(|cursor| [&cursor.q, &cursor.s, &cursor.a] == [&bound[0], &bound[1], &bound[2]])
            .and_then(|cursor| cursor.p.parse::<u64>().ok())
            .ok_or_else(object_query::cursor_invalid)?,
    };
    // A caller without Values reads every row withheld: a withheld row
    // matches no clause and has no sort value, so only identity orders it.
    let (filters, sort) = if values {
        page_order(&query)?
    } else {
        (Vec::new(), Vec::new())
    };
    let page = if !values && !query.filter.is_empty() {
        PageResponse {
            total: 0,
            rows: Vec::new(),
        }
    } else {
        let request = PageRequest {
            tenant: TenantId::of_scope(&host.scope().key()),
            source,
            filters,
            sort,
            ids: Vec::new(),
            offset,
            limit: u32::try_from(query.limit).unwrap_or(u32::MAX),
        };
        engine_page(store, transport, &request, query.after.is_some())?
    };
    let returned = u64::try_from(page.rows.len()).unwrap_or(u64::MAX);
    let reached = offset.saturating_add(returned);
    let [q, s, a] = bound;
    let next = (returned > 0 && reached < page.total).then(|| {
        Cursor {
            v: 2,
            q,
            s,
            a,
            c: 0,
            r: None,
            p: reached.to_string(),
        }
        .encode()
    });
    let scope_key = host.scope().key();
    let records = page
        .rows
        .into_iter()
        .map(|row| queried(object_type, &scope_key, row, values))
        .collect::<Result<Vec<_>, Refusal>>()?;
    Ok(NornObjectQueryPage {
        object_type: object_type.to_owned(),
        records,
        quarantine_count: quarantined,
        query: NornAppliedQuery {
            sort: query.sort_document(),
            filter: query.filter_document(),
        },
        next,
        total: Some(NornQueryTotal {
            count: page.total,
            approximate: false,
        }),
    })
}

/// A parsed grid query's clauses and keys as the engine takes them.
fn page_order(
    query: &crate::object_query::ObjectQuery,
) -> Result<
    (
        Vec<norn_cell::analytics::PageClause>,
        Vec<norn_cell::analytics::PageKey>,
    ),
    Refusal,
> {
    Ok((
        query
            .filter
            .iter()
            .map(page_clause)
            .collect::<Result<Vec<_>, _>>()?,
        query
            .sort
            .iter()
            .map(|key| norn_cell::analytics::PageKey {
                column: key.property.clone(),
                descending: key.descending,
            })
            .collect(),
    ))
}

/// One records page from the engine. A cursor whose dataset was loaded
/// again since reads as `object.snapshot_moved`.
fn engine_page<E: SqlEngine>(
    store: &CellStore<E>,
    transport: &dyn Transport,
    request: &norn_cell::analytics::PageRequest,
    continued: bool,
) -> Result<norn_cell::analytics::PageResponse, Refusal> {
    let call = EngineCall::new("page", request).map_err(engine_failure)?;
    let answer = exchange(
        store,
        transport,
        &request.tenant,
        mirror_world(&request.source),
        &call,
    )
    .map_err(engine_failure)?;
    if answer.status != 200 {
        let error = refused(&answer.body);
        if continued
            && matches!(error, CellError::ComputeRefused(code) if code == "core.compute.stale_input")
        {
            return Err(crate::object_routes::snapshot_moved());
        }
        return Err(engine_failure(error));
    }
    decode(&answer.body).map_err(engine_failure)
}

/// One dataset row as the grid shows it: withheld (no values, no version)
/// for a caller without Values.
fn queried(
    object_type: &str,
    scope_key: &str,
    row: norn_cell::analytics::PageRow,
    values: bool,
) -> Result<crate::objects::NornQueriedRecord, Refusal> {
    let read = if values {
        Some(
            row.values
                .iter()
                .map(|(id, value)| Ok((id.clone(), norn_cell::analytics::runtime_value(value)?)))
                .collect::<Result<BTreeMap<_, _>, norn_ir::RuntimeValueError>>()
                .map_err(Refusal::host)?,
        )
    } else {
        None
    };
    Ok(crate::objects::NornQueriedRecord {
        object_type: object_type.to_owned(),
        id: row.id,
        scope: scope_key.to_owned(),
        version: read.as_ref().map(|_| row.version),
        withheld: read.is_none(),
        values: read,
        withheld_properties: Vec::new(),
        masked: BTreeMap::new(),
    })
}

/// HOST-READ-3 over a loaded dataset: one row by identity, as
/// `GET /v1/objects/{type}/{id}` shows it, from the tenant's engine.
///
/// # Errors
/// A denied gate (one opaque 403), an absent row (404, as an absent object),
/// or an unreachable engine.
pub fn record_object<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: Option<&str>,
    (object_type, id): (&str, &str),
    access: &dyn ObjectAccess,
    transport: &dyn Transport,
) -> Result<crate::objects::NornQueriedRecord, Refusal> {
    use norn_cell::analytics::{PageRequest, PageResponse};
    let values = uniform_gate(host, scope, actor, object_type, access)?;
    let guard = host.cell()?;
    let store = guard.store();
    let (source, _snapshot) = store
        .analytics_source(norn_cell::MAIN_WORLD, &host.scope().key(), object_type)
        .map_err(engine_failure)?;
    let tenant = TenantId::of_scope(&host.scope().key());
    let call = EngineCall::new(
        "page",
        &PageRequest {
            tenant: tenant.clone(),
            source: source.clone(),
            filters: Vec::new(),
            sort: Vec::new(),
            ids: vec![id.to_owned()],
            offset: 0,
            limit: 1,
        },
    )
    .map_err(engine_failure)?;
    let answer = exchange(store, transport, &tenant, mirror_world(&source), &call)
        .map_err(engine_failure)?;
    if answer.status != 200 {
        return Err(engine_failure(refused(&answer.body)));
    }
    let page: PageResponse = decode(&answer.body).map_err(engine_failure)?;
    let row = page
        .rows
        .into_iter()
        .next()
        .ok_or_else(crate::object_routes::not_found)?;
    queried(object_type, &host.scope().key(), row, values)
}

/// One parsed grid clause as the engine evaluates it: the same operator,
/// operands typed as the column compares (a date as its UTC midnight's key).
fn page_clause(
    clause: &crate::object_query::Clause,
) -> Result<norn_cell::analytics::PageClause, Refusal> {
    use crate::object_query::{Op, Operand, Scalar};
    use norn_cell::analytics::{PageClause, PageOp, PageOperand};
    let unsupported = || Refusal::from_compute(CellError::ComputeRefused(UNSUPPORTED_ENGINE));
    let operand = |scalar: &Scalar| -> Result<PageOperand, Refusal> {
        Ok(match scalar {
            Scalar::Text(text) => PageOperand::Text(text.clone()),
            Scalar::Number(number) => PageOperand::Number(number.canonical()),
            Scalar::Bool(flag) => PageOperand::Bool(*flag),
            Scalar::Date(days) => PageOperand::Instant {
                secs: days.checked_mul(86_400).ok_or_else(unsupported)?,
                nanos: 0,
            },
            Scalar::Instant(nanos) => PageOperand::Instant {
                secs: i64::try_from(nanos.div_euclid(1_000_000_000))
                    .map_err(|_overflow| unsupported())?,
                nanos: u32::try_from(nanos.rem_euclid(1_000_000_000))
                    .map_err(|_overflow| unsupported())?,
            },
            Scalar::Span(_) => return Err(unsupported()),
        })
    };
    let operands = match &clause.operand {
        Operand::None => Vec::new(),
        Operand::One(one) => vec![operand(one)?],
        Operand::Many(many) => many.iter().map(operand).collect::<Result<_, _>>()?,
        Operand::Range(low, high) => vec![operand(low)?, operand(high)?],
    };
    Ok(PageClause {
        column: clause.property.clone(),
        op: match clause.op {
            Op::Eq => PageOp::Eq,
            Op::Ne => PageOp::Ne,
            Op::In => PageOp::In,
            Op::Contains => PageOp::Contains,
            Op::Prefix => PageOp::Prefix,
            Op::Lt => PageOp::Lt,
            Op::Lte => PageOp::Lte,
            Op::Gt => PageOp::Gt,
            Op::Gte => PageOp::Gte,
            Op::Between => PageOp::Between,
            Op::Empty => PageOp::Empty,
            Op::Present => PageOp::Present,
        },
        operands,
    })
}

/// Whether every live row of `object_type` has a complete marking snapshot
/// with no label on any of its properties. An unknown type, an incomplete
/// snapshot or an unbounded summary counts as labeled: it fails closed to the
/// native profile.
fn unlabeled<E: SqlEngine>(reader: &norn_cell::StoreReader<'_, E>, object_type: &str) -> bool {
    let Ok(properties) = reader.model().effective_property_types(object_type) else {
        return false;
    };
    properties.iter().all(|(property, _)| {
        reader
            .property_marking_summary(norn_cell::MAIN_WORLD, object_type, property)
            .is_ok_and(|labels| labels.is_some_and(|labels| labels.is_empty()))
    })
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
    // PRIVACY-1: the engine mirror holds stored values; a cell that declares
    // markings profiles natively, over projected values, unless no live row
    // of this type carries a label on any of its properties. Then every value
    // projects as itself and the engine's profile is the native one.
    let marked = {
        let guard = host.cell()?;
        let reader = guard.store().read();
        !host.marking_catalog(&reader)?.is_empty() && !unlabeled(&reader, object_type)
    };
    if marked {
        return crate::object_profile::get_object_profile(host, scope, actor, object_type, access);
    }
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
