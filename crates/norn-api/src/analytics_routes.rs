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
    FileSource, FrontierResponse, LandRequest, LandResponse, LoadRequest, SettleRequest, Source,
    TenantId, hex_digest,
};
use norn_cell::compute::{ALL_MEMBERS, QueryContext, QueryResult};
use norn_cell::dataset_manifest::{
    DatasetManifest, MANIFEST_VERSION, ManifestCounts, ManifestFile, ManifestSource,
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
    /// `frontier`, `sync`, `evaluate`, `members`, `profile`, `land` or `export`.
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
    /// MANIFEST-LAWS-1: findings of each law and invariant, by rule.
    pub findings: BTreeMap<String, NornRuleFindings>,
    /// Admitted rows a law or invariant holds back; every other admitted row
    /// is trusted.
    pub held_by_rules: u64,
    /// Source file size in bytes.
    pub bytes: u64,
    /// Load time in milliseconds.
    pub millis: u64,
}

/// MANIFEST-LAWS-1: one law or invariant's findings over a dataset.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornRuleFindings {
    /// The law (`law:<id>`) or invariant (`invariant:<id>`).
    pub label: String,
    /// Rows that fail it.
    pub rows: u64,
    /// Whether a failing row is held back.
    pub holds: bool,
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
    let landed = land_upload(
        host,
        &request.object_type,
        &record,
        uploads,
        transport,
        now_ms,
    )?;
    let (manifest, hash, receipt) = commit_manifest(host, &request.object_type, &record, &landed)?;
    let loaded = &landed.load;
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
        findings: manifest
            .counts
            .findings
            .iter()
            .map(|(rule, count)| {
                (
                    rule.clone(),
                    NornRuleFindings {
                        label: count.label.clone(),
                        rows: count.rows,
                        holds: count.holds,
                    },
                )
            })
            .collect(),
        held_by_rules: manifest.counts.held_by_rules,
        bytes: loaded.bytes,
        millis: loaded.millis,
    })
}

/// The object keys a landing writes: `datasets/<scope>/<type>/<load>/{data,held}.parquet`.
fn landing_keys(scope_key: &str, object_type: &str, load: &str) -> [String; 2] {
    let prefix = format!("datasets/{scope_key}/{object_type}/{load}");
    [
        format!("{prefix}/data.parquet"),
        format!("{prefix}/held.parquet"),
    ]
}

/// A landing that is ready to be committed: its files are in object storage.
struct Landing {
    load: norn_cell::analytics::LoadResponse,
    land: LandResponse,
    table: norn_cell::analytics::TableSchema,
    format: FileFormat,
    uris: [String; 2],
}

/// MANIFEST-LAND-1: type `record` by `object_type` on the engine and land it
/// as the data and held-set Parquet files.
fn land_upload<E: SqlEngine>(
    host: &DevHost<E>,
    object_type: &str,
    record: &UploadRecord,
    uploads: &dyn Uploads,
    transport: &dyn Transport,
    now_ms: u64,
) -> Result<Landing, Refusal> {
    let guard = host.cell()?;
    let store = guard.store();
    let table = store
        .analytics_table(object_type)
        .map_err(|_unknown| dataset_not_found())?;
    // MANIFEST-LAWS-1: every law and invariant of the type, checked set-based.
    let checks = store
        .dataset_checks(object_type, &table)
        .map_err(dataset_refusal)?;
    let format = if record.content_type.contains("parquet") || record.uri.ends_with(".parquet") {
        FileFormat::Parquet
    } else {
        FileFormat::Csv
    };
    let pins = store
        .dataset_pins(object_type, &BTreeMap::new())
        .map_err(Refusal::host)?;
    let load = format!(
        "l{}",
        hex_digest(
            format!(
                "{}\n{}\n{}\n{}\n{}",
                record.uri, record.sha256, table.pin, pins.binding, pins.laws
            )
            .as_bytes()
        )
    );
    let [data_key, held_key] = landing_keys(&host.scope().key(), object_type, &load);
    let mut request = LandRequest {
        load: LoadRequest {
            tenant: TenantId::of_scope(&host.scope().key()),
            table: table.clone(),
            load,
            file: FileSource {
                url: String::new(),
                uri: record.uri.clone(),
                sha256: record.sha256.clone(),
                format,
            },
            headers: BTreeMap::new(),
        },
        data_url: String::new(),
        held_url: String::new(),
        checks,
        residue_page: RESIDUE_PAGE,
    };
    // The URLs are minted per request; the call's identity is the landing without them.
    let mut call = EngineCall::new("land", &request).map_err(engine_failure)?;
    let unconfigured = || Refusal::host("the object store is not configured");
    request.load.file.url = uploads
        .presigned_get(&record.uri, LOAD_URL_SECONDS, now_ms)
        .ok_or_else(unconfigured)?;
    let (data_put, data_object) = uploads
        .presigned_put(&data_key, LOAD_URL_SECONDS, now_ms)
        .ok_or_else(unconfigured)?;
    let (held_put, held_object) = uploads
        .presigned_put(&held_key, LOAD_URL_SECONDS, now_ms)
        .ok_or_else(unconfigured)?;
    request.data_url = data_put;
    request.held_url = held_put;
    call.body = serde_json::to_string(&request).map_err(Refusal::host)?;
    let land = land_steps(transport, &call, &request.load.load, |answer| {
        // MANIFEST-LAWS-1: the residue rows are decided by the per-row
        // evaluator, then settled on the engine.
        let findings = store
            .residue_findings(object_type, &table, &answer.residue)
            .map_err(dataset_refusal)?;
        let through = answer
            .residue
            .last()
            .map(|row| row.row)
            .ok_or_else(|| Refusal::host("the engine reported residue without rows"))?;
        let settle = SettleRequest {
            land: request.clone(),
            through,
            findings,
        };
        EngineCall::new("settle", &settle).map_err(engine_failure)
    })?;
    Ok(Landing {
        load: land.load.clone(),
        land,
        table,
        format,
        uris: [data_object, held_object],
    })
}

/// Residue rows one landing answer carries at most.
const RESIDUE_PAGE: u32 = 2_000;
/// Landing steps one load may take before it refuses.
const MAX_LAND_STEPS: usize = 100_000;

/// MANIFEST-LAWS-1: a landing's exchange, from its first call to the answer
/// that carries its files. Each answer decides the next call: a settle of
/// the residue rows it names, until none remains. The exchange is
/// stateless: a host that re-enters with an answer to a step of this
/// landing (its landing identity) continues from it.
fn land_steps(
    transport: &dyn Transport,
    first: &EngineCall,
    load: &str,
    mut settle: impl FnMut(&LandResponse) -> Result<EngineCall, Refusal>,
) -> Result<LandResponse, Refusal> {
    let ours = |answer: &EngineAnswer| {
        answer.digest == first.digest
            || (matches!(answer.op.as_str(), "land" | "settle")
                && decode::<LandResponse>(&answer.body).is_ok_and(|r| r.load.load == load))
    };
    let mut answer = transport.resumed().filter(ours);
    let mut call = first.clone();
    for _ in 0..MAX_LAND_STEPS {
        let done = match answer.take() {
            Some(done) => done,
            None => transport
                .call(&call)
                .ok_or_else(pending)
                .map_err(engine_failure)?,
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
        if land.residue_total == 0 {
            return Ok(land);
        }
        call = settle(&land)?;
    }
    Err(Refusal::host("the landing did not settle"))
}

fn dataset_refusal(error: CellError) -> Refusal {
    if let CellError::DatasetRefused { code, detail } = &error {
        return Refusal::new(
            http::StatusCode::UNPROCESSABLE_ENTITY,
            code,
            "the dataset was not loaded",
            detail,
        );
    }
    Refusal::host(error)
}

/// MANIFEST-COMMIT-1: commit the landing's manifest as the type's current dataset.
fn commit_manifest<E: SqlEngine>(
    host: &DevHost<E>,
    object_type: &str,
    record: &UploadRecord,
    landing: &Landing,
) -> Result<(DatasetManifest, String, norn_cell::Receipt), Refusal> {
    let mut guard = host.cell()?;
    let binding = BTreeMap::new();
    let pins = guard
        .store()
        .dataset_pins(object_type, &binding)
        .map_err(Refusal::host)?;
    let [data_uri, held_uri] = landing.uris.clone();
    let land = &landing.land;
    let unlanded = || Refusal::host("the engine answered a landing without its files");
    let (data, held) = (
        land.data.as_ref().ok_or_else(unlanded)?,
        land.held.as_ref().ok_or_else(unlanded)?,
    );
    let manifest = DatasetManifest {
        version: MANIFEST_VERSION.to_owned(),
        object_type: object_type.to_owned(),
        load: land.load.load.clone(),
        source: ManifestSource {
            uri: record.uri.clone(),
            sha256: record.sha256.clone(),
            format: landing.format,
            bytes: land.load.bytes,
        },
        binding,
        table: landing.table.clone(),
        pins,
        data: ManifestFile {
            uri: data_uri,
            sha256: data.sha256.clone(),
            rows: data.rows,
            bytes: data.bytes,
        },
        held: ManifestFile {
            uri: held_uri,
            sha256: held.sha256.clone(),
            rows: held.rows,
            bytes: held.bytes,
        },
        counts: ManifestCounts {
            source: land.load.rows.saturating_add(land.load.rejected),
            admitted: land.load.rows,
            held: land.load.reasons.clone(),
            findings: land.findings.clone(),
            held_by_rules: land.held_by_rules,
        },
        valid_time: land.valid_time.clone(),
        markings: guard
            .store()
            .dataset_markings(object_type, &landing.table)
            .map_err(Refusal::host)?,
    };
    let (hash, receipt) = guard
        .commit_dataset_manifest(&manifest)
        .map_err(manifest_refusal)?;
    Ok((manifest, hash, receipt))
}

/// One file a dataset row is traced through, and the row's offset in it.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornLineageFile {
    /// The object's durable URI.
    pub uri: String,
    /// Its SHA-256.
    pub sha256: String,
    /// The row's source offset (1-based, in source file order); the data
    /// file carries it as its `:row` column.
    pub row: u64,
}

/// One law or invariant a dataset row fails.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornRowFinding {
    /// The rule identity (canonical JSON of its reference).
    pub rule: String,
    /// The law (`law:<id>`) or invariant (`invariant:<id>`).
    pub label: String,
    /// Whether it holds the row back.
    pub holds: bool,
}

/// The outcome of `GET /v1/datasets/{type}/rows/{row}:lineage`.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornDatasetLineage {
    /// The dataset's object type.
    pub object_type: String,
    /// The source row offset; dataset answers name rows by it.
    pub row: u64,
    /// The manifest that admitted the dataset.
    pub manifest: String,
    /// The commit that admitted it.
    pub commit_seq: u64,
    /// The source upload, at the row.
    pub source: NornLineageFile,
    /// The data file, at the row; absent when the row was not typed.
    pub data: Option<NornLineageFile>,
    /// Why the row was not typed (`bulk.*`), if it was not.
    pub refused: Option<String>,
    /// Every rule the row fails.
    pub findings: Vec<NornRowFinding>,
    /// Whether the row is held back (not typed, or failing a rule that holds).
    pub held: bool,
}

/// MANIFEST-LINEAGE-1: trace one dataset row, as a question answer or the
/// records grid names it, to the manifest that admitted it, the source
/// upload and the data file, each by content hash and row offset, with what
/// held it back. Held-back rows are traced too. Read like the type's
/// records; no value is disclosed.
///
/// # Errors
/// A denied gate (one opaque 403), no dataset or no such row (404), or an
/// unreachable engine.
#[utoipa::path(get, path = "/v1/datasets/{type}/rows/{row}:lineage", params(("type" = String, Path, description = "The dataset's object type."), ("row" = u64, Path, description = "The source row offset."), ScopeQuery), responses((status = 200, description = "The row's lineage.", body = NornDatasetLineage), (status = 403, description = "The request was refused: scope or authority denied.", body = Refusal), (status = 404, description = "The request was refused: not found.", body = Refusal), (status = 500, description = "The request was refused: host failure.", body = Refusal)))]
pub fn dataset_lineage<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: Option<&str>,
    (object_type, row): (&str, &str),
    access: &dyn ObjectAccess,
    transport: &dyn Transport,
) -> Result<NornDatasetLineage, Refusal> {
    use norn_cell::analytics::{LineageRequest, LineageResponse};
    uniform_gate(host, scope, actor, object_type, access)?;
    let row: u64 = row
        .parse()
        .ok()
        .filter(|row| *row > 0)
        .ok_or_else(crate::object_routes::not_found)?;
    let guard = host.cell()?;
    let store = guard.store();
    let current = store
        .dataset_manifest(norn_cell::MAIN_WORLD, object_type)
        .map_err(Refusal::host)?
        .ok_or_else(crate::object_routes::not_found)?;
    let manifest = &current.manifest;
    if row > manifest.counts.source {
        return Err(crate::object_routes::not_found());
    }
    let tenant = TenantId::of_scope(&host.scope().key());
    let call = EngineCall::new(
        "lineage",
        &LineageRequest {
            tenant: tenant.clone(),
            table: object_type.to_owned(),
            load: manifest.load.clone(),
            row,
        },
    )
    .map_err(engine_failure)?;
    let answer = exchange(store, transport, &tenant, None, &call).map_err(engine_failure)?;
    if answer.status != 200 {
        return Err(engine_failure(refused(&answer.body)));
    }
    let found: LineageResponse = decode(&answer.body).map_err(engine_failure)?;
    let file = |file: &ManifestFile| NornLineageFile {
        uri: file.uri.clone(),
        sha256: file.sha256.clone(),
        row,
    };
    let held = !found.admitted || found.findings.iter().any(|finding| finding.holds);
    Ok(NornDatasetLineage {
        object_type: object_type.to_owned(),
        row,
        manifest: current.hash.clone(),
        commit_seq: current.commit_seq,
        source: NornLineageFile {
            uri: manifest.source.uri.clone(),
            sha256: manifest.source.sha256.clone(),
            row,
        },
        data: found.admitted.then(|| file(&manifest.data)),
        refused: found.refused,
        findings: found
            .findings
            .into_iter()
            .map(|finding| NornRowFinding {
                rule: finding.rule,
                label: finding.label,
                holds: finding.holds,
            })
            .collect(),
        held,
    })
}

/// MANIFEST-CURRENT-1: the current dataset manifest of `object_type`, if it is one.
///
/// # Errors
/// A store failure.
pub fn current_manifest<E: SqlEngine>(
    host: &DevHost<E>,
    object_type: &str,
) -> Result<Option<norn_cell::dataset_manifest::CommittedManifest>, Refusal> {
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
    // MANIFEST-MARK-1: a dataset's rows live only in the engine; its marked
    // columns are projected on every read (`dataset_marks`).
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
    let marks = dataset_marks(host, store, object_type)?;
    marks.refuse_ordering(&filters, &sort)?;
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
        .map(|row| queried(object_type, &scope_key, row, values, &marks))
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
/// MANIFEST-MARK-1: how a dataset's marked columns are projected for a reader.
pub(crate) struct DatasetMarks {
    catalog: crate::privacy::MarkingCatalog,
    columns: BTreeMap<String, Vec<String>>,
    key: Option<crate::privacy::PseudonymKey>,
}

impl DatasetMarks {
    /// A filter or sort over a marked column would order or select rows by
    /// its stored values, outside any projection: it refuses.
    fn refuse_ordering(
        &self,
        filters: &[norn_cell::analytics::PageClause],
        sort: &[norn_cell::analytics::PageKey],
    ) -> Result<(), Refusal> {
        match filters
            .iter()
            .map(|clause| clause.column.as_str())
            .chain(sort.iter().map(|key| key.column.as_str()))
            .find(|column| self.columns.contains_key(*column))
        {
            Some(column) => Err(marked_column(column)),
            None => Ok(()),
        }
    }
}

/// The marks of the current dataset of `object_type`: its manifest's marked
/// columns, under the cell's marking catalog.
fn dataset_marks<E: SqlEngine>(
    host: &DevHost<E>,
    store: &CellStore<E>,
    object_type: &str,
) -> Result<DatasetMarks, Refusal> {
    let columns = store
        .dataset_manifest(norn_cell::MAIN_WORLD, object_type)
        .map_err(Refusal::host)?
        .map(|current| current.manifest.markings)
        .unwrap_or_default();
    Ok(DatasetMarks {
        catalog: host.marking_catalog(&store.read())?,
        columns,
        key: host.pseudonym_key().cloned(),
    })
}

/// MANIFEST-MARK-1: a marked column on a path that would read its stored
/// values outside a projection (a filter, a sort, a profile, an export or a
/// question) refuses.
fn marked_column(column: &str) -> Refusal {
    Refusal::new(
        http::StatusCode::FORBIDDEN,
        "privacy.marked_column",
        "the column is marked",
        format!("{column} is marked; read it through the records grid, where it is masked"),
    )
}

fn queried(
    object_type: &str,
    scope_key: &str,
    row: norn_cell::analytics::PageRow,
    values: bool,
    marks: &DatasetMarks,
) -> Result<crate::objects::NornQueriedRecord, Refusal> {
    let mut masked = BTreeMap::new();
    let mut withheld_properties = Vec::new();
    let read = if values {
        let mut read = row
            .values
            .iter()
            .map(|(id, value)| Ok((id.clone(), norn_cell::analytics::runtime_value(value)?)))
            .collect::<Result<BTreeMap<_, _>, norn_ir::RuntimeValueError>>()
            .map_err(Refusal::host)?;
        for (column, ids) in &marks.columns {
            let mask = crate::privacy::strongest(&marks.catalog, ids)
                .unwrap_or(norn_ir::MaskPolicy::Refuse {});
            let Some(value) = read.get_mut(column) else {
                continue;
            };
            let mode = crate::privacy::apply(&mask, marks.key.as_ref(), value);
            if mode == crate::privacy::NornMaskMode::Refuse {
                read.remove(column);
                withheld_properties.push(column.clone());
            }
            masked.insert(
                column.clone(),
                crate::privacy::NornMaskedProperty {
                    mode,
                    markings: ids.clone(),
                    revealable: false,
                },
            );
        }
        Some(read)
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
        withheld_properties,
        masked,
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
    let marks = dataset_marks(host, store, object_type)?;
    queried(object_type, &host.scope().key(), row, values, &marks)
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

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Arc;

    use norn_cell::analytics::{PageRow, Value};
    use norn_ir::{MarkingId, MarkingIr, MarkingSeverity, MaskPolicy, RuntimeValue};

    use super::{DatasetMarks, queried};
    use crate::privacy::{NornMaskMode, PseudonymKey};
    use crate::scope::CellScope;

    fn marks(mask: MaskPolicy, key: Option<PseudonymKey>) -> DatasetMarks {
        let marking = MarkingIr {
            id: MarkingId("pii.name".into()),
            label: "Name".into(),
            severity: MarkingSeverity::High,
            category: "pii".into(),
            mask,
        };
        DatasetMarks {
            catalog: Arc::new(BTreeMap::from([("pii.name".to_owned(), marking)])),
            columns: BTreeMap::from([("customer".to_owned(), vec!["pii.name".to_owned()])]),
            key,
        }
    }

    fn row() -> PageRow {
        PageRow {
            id: "1".into(),
            version: 1,
            values: BTreeMap::from([
                ("customer".to_owned(), Value::Text("Ann Lee".into())),
                ("amount".to_owned(), Value::Number("12.5".into())),
            ]),
        }
    }

    /// MANIFEST-MARK-1: a marked dataset column reads as its mask says (an
    /// analyst sees a pseudonym); an unmarked one reads as stored.
    #[test]
    fn a_marked_dataset_column_reads_as_its_mask() {
        let scope = CellScope {
            org_id: "org_demo".into(),
            environment_id: "dev".into(),
        };
        let key = PseudonymKey::derive(&[7_u8; 32], &scope).expect("key");
        let pseudonym = key.pseudonym("customer", &RuntimeValue::String("Ann Lee".into()));
        let read = queried(
            "bench.Line",
            "org_demo/dev",
            row(),
            true,
            &marks(
                MaskPolicy::Pseudonym {
                    subject: "customer".into(),
                },
                Some(key),
            ),
        )
        .expect("read");
        let values = read.values.expect("values");
        assert_eq!(values["customer"], RuntimeValue::String(pseudonym));
        assert_eq!(values["amount"].canonical_json().expect("json"), b"12.5");
        assert_eq!(read.masked["customer"].mode, NornMaskMode::Pseudonym);
        assert!(!read.masked.contains_key("amount"));
        // A refused mask withholds the column.
        let refused = queried(
            "bench.Line",
            "org_demo/dev",
            row(),
            true,
            &marks(MaskPolicy::Refuse {}, None),
        )
        .expect("read");
        assert!(!refused.values.expect("values").contains_key("customer"));
        assert_eq!(refused.withheld_properties, vec!["customer".to_owned()]);
    }
}
