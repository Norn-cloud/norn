//! D102/D136: routes of a host whose analytics run on the tenant's engine.
//!
//! Questions run on the engine rather than scanning the cell's store; the cell
//! keeps admission, authorization, row gates, lineage and answer verification.
//! Operations are typed with no caller-supplied SQL or expressions (#733).
//! The exchange (frontier -> sync -> op) is stateless so Durable Objects can
//! yield and re-enter with full re-authorization on every step.

use std::collections::BTreeMap;

use norn_cell::analytics::{
    EngineRefusal, EvaluateRequest, EvaluateResponse, ExportFormat, ExportRequest, FileFormat,
    FileSource, FrontierResponse, LandRequest, LandResponse, LoadRequest, RuleCheck, SettleRequest,
    Source, StoreRequest, StoredObject, TableSchema, TenantId, dataset_cache_id, hex_digest,
};
use norn_cell::compute::{ALL_MEMBERS, QueryContext, QueryResult};
use norn_cell::dataset_manifest::{
    CommittedManifest, DatasetManifest, DatasetRequest, MANIFEST_VERSION, ManifestCounts,
    ManifestFile, ManifestPins, ManifestSource,
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
use crate::object_routes::{ObjectAccess, uniform_gate, uniform_sources_gate};
use crate::question_routes::{AdHocQuestions, QuestionRuntime, QuestionSnapshot};
use crate::refusal::Refusal;
use crate::scope::{CellScope, ScopeQuery};

/// Committed versions sent per sync batch.
const SYNC_BATCH: usize = 5_000;
/// Engine steps one answer may take before it refuses.
pub const MAX_STEPS: u32 = norn_cell::analytics::MAX_ENGINE_STEPS;
/// Lifetime of the presigned URLs a load reads and lands through, in seconds.
const LOAD_URL_SECONDS: u64 = 900;

/// One engine operation, as the host sends it: `POST /v1/<op>` with `body`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EngineCall {
    /// `frontier`, `sync`, `evaluate`, `members`, `profile`, `page`, `land`,
    /// `store` or `export`.
    pub op: String,
    /// Call identity echoed in the answer so stale answers are ignored.
    pub digest: String,
    /// Typed JSON request body.
    pub body: String,
    /// Response headers when the host streams the answer directly (exports).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<BTreeMap<String, String>>,
    /// OLAP-CRED-1: cell-minted tenant-scoped bearer credential for this call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential: Option<String>,
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
            credential: None,
        })
    }
}

/// How the cell reaches its engine.
pub trait Transport: Send + Sync {
    /// What the engine this transport reaches answers exactly (D136).
    fn capabilities(&self) -> norn_cell::analytics::EngineCapabilities;
    /// The answer the host re-entered the cell with, once.
    fn resumed(&self) -> Option<EngineAnswer>;
    /// Perform `call` now, or `None` when the host must perform it and re-enter.
    fn call(&self, call: &EngineCall) -> Option<EngineAnswer>;
}

mod profile_view;
use profile_view::property_profile;

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
    let code = decode::<EngineRefusal>(body).map_or_default(|r| r.code);
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
    exchange_from(store, transport, tenant, world, last, transport.resumed())
}

/// [`exchange`], re-entered with `answer`.
fn exchange_from<E: SqlEngine>(
    store: &CellStore<E>,
    transport: &dyn Transport,
    tenant: &TenantId,
    world: Option<(&str, u64)>,
    last: &EngineCall,
    mut answer: Option<EngineAnswer>,
) -> Result<EngineAnswer, CellError> {
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
    if !transport
        .capabilities()
        .answers(&request.query, &request.source)
    {
        return Err(CellError::ComputeRefused(UNSUPPORTED_ENGINE));
    }
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
        let prepared = store
            .analytics_prepare(plan, context, &self.tenant)?
            .expecting(self.transport.capabilities().engine);
        let mut result = if admitted {
            let response = evaluate_on(store, self.transport, &self.tenant, prepared.request())?;
            store.analytics_answer(prepared, response)?
        } else {
            store.analytics_empty_authorized_answer(prepared)?
        };
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
        let mut answer = if admitted {
            let prepared = store
                .analytics_prepare_known_at(plan, context, &self.tenant, cutoff)?
                .expecting(self.transport.capabilities().engine);
            let response = evaluate_on(store, self.transport, &self.tenant, prepared.request())?;
            store.analytics_answer_known_at(prepared, response)?
        } else {
            store.analytics_empty_known_at(plan, context, &self.tenant, cutoff)?
        };
        finish_answer(store, plan, &mut answer.result, Some(cutoff), visible)?;
        Ok(answer)
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
#[utoipa::path(get, path = "/v1/objects/{type}:export", params(("type" = String, Path, description = "The concept to export."), ExportQuery), responses((status = 200, description = "The rows, as Parquet (`application/vnd.apache.parquet`) or JSON lines (`application/x-ndjson`); `Norn-Snapshot` names the snapshot they are exact at.", content(("application/vnd.apache.parquet"), ("application/x-ndjson"))), (status = 400, description = "The request was refused as invalid.", body = Refusal), (status = 403, description = "The request was refused: scope or authority denied.", body = Refusal), (status = 404, description = "The request was refused: not found.", body = Refusal), (status = 500, description = "The request was refused: host failure.", body = Refusal)))]
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
    // markings or holds dynamic `_norn_markings` exports nothing; a marked
    // value leaves only through a reveal. `admit_export` brackets the marking
    // decision and the exported source at one cell version (S3, #1547) and
    // issues a non-cloneable permit consumed at export dispatch.
    let permit = crate::privacy::disclosure::admit_export(host, store, object_type)?;
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
    dispatch_export(
        store,
        transport,
        &host.scope().key(),
        permit,
        columns,
        format,
    )
}

fn dispatch_export<E: SqlEngine>(
    store: &CellStore<E>,
    transport: &dyn Transport,
    scope_key: &str,
    permit: crate::privacy::disclosure::DisclosedExportPermit,
    columns: Vec<String>,
    format: ExportFormat,
) -> Result<(), Refusal> {
    let (source, snapshot) = permit.into_parts();
    let tenant = TenantId::of_scope(scope_key);
    let mut call = EngineCall::new(
        "export",
        &ExportRequest {
            tenant: tenant.clone(),
            source: source.clone(),
            columns,
            format,
            model_pin: Some(store.model().model_hash().to_owned()),
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
    /// Rows admitted as trusted: typed, and held back by no rule (LAW-02).
    pub rows: u64,
    /// Rows quarantined because they could not be typed.
    pub rejected: u64,
    /// Quarantined rows per reason code (`bulk.*`).
    pub reasons: BTreeMap<String, u64>,
    /// MANIFEST-LAWS-1: findings of each law and invariant, by rule.
    pub findings: BTreeMap<String, NornRuleFindings>,
    /// Typed rows a law or invariant holds back: in the held-set file, not
    /// among `rows`.
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
#[allow(
    clippy::too_many_lines,
    reason = "one load, in order: authorize, contract, land, commit, report"
)]
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
    // MANIFEST-04: a request is fingerprinted by what it asks (the upload,
    // the type and the mapping it names), never by what the definitions
    // resolve it to now: a retry after a mapping edit keeps its first
    // receipt. A request already committed answers with that receipt.
    let keyed = request
        .request_key
        .as_deref()
        .map(|key| keyed_request(key, &request, &record))
        .transpose()?;
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
    let contract = bulk_contract(host, &request.object_type, request.mapping.as_deref())?;
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
            // LAW-02: rows a rule holds back are not landed as trusted.
            counts: (
                manifest.counts.source,
                manifest.counts.admitted,
                manifest
                    .counts
                    .source
                    .saturating_sub(manifest.counts.admitted),
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
        rows: manifest.counts.admitted,
        rejected: loaded.rejected,
        reasons: loaded.reasons.clone(),
        findings: rule_findings(&manifest.counts),
        held_by_rules: manifest.counts.held_by_rules,
        bytes: loaded.bytes,
        millis: loaded.millis,
    })
}

/// MANIFEST-03: what an observation load of the type would be decided by,
/// as bulk ingest enforces it: the admitted mapping onto the type, the
/// column binding it reads, and the table and pins the landing is typed
/// and checked by. MANIFEST-01: all of it is read from one definitions
/// revision, so a landing never pairs one revision's mapping with
/// another's pins.
struct BulkContract {
    /// The mapping's artifact id.
    mapping_id: String,
    /// The file column of each property read from a differently named field.
    binding: BTreeMap<String, String>,
    /// The type's table.
    table: TableSchema,
    /// The pins, the resolved mapping's (`<artifact id>@<sha256>`) among them.
    pins: ManifestPins,
    /// MANIFEST-LAWS-1: the type's laws and invariants, as the landing checks them.
    checks: Vec<RuleCheck>,
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
///
/// MANIFEST-01: the definitions epoch and the cell are held from reading
/// the set to capturing the pins, so the mapping, the markings, the table
/// and the pins are one revision's; the commit then refuses any later one.
#[allow(
    clippy::too_many_lines,
    reason = "one contract, read top to bottom: the mapping, its reads, the markings, then the pins"
)]
fn bulk_contract<E: SqlEngine>(
    host: &DevHost<E>,
    object_type: &str,
    named: Option<&str>,
) -> Result<BulkContract, Refusal> {
    let _epoch = host.definition_read()?;
    let guard = host.cell()?;
    let store = guard.store();
    let set: Vec<serde_json::Value> = match store
        .read()
        .installed_definitions(None)
        .map_err(Refusal::host)?
    {
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
            .map_or_default(|maps| {
                maps.iter()
                    .filter(|map| {
                        map.pointer("/target/entity")
                            .and_then(serde_json::Value::as_str)
                            .is_some_and(|entity| entities.iter().any(|e| e == entity))
                    })
                    .cloned()
                    .collect()
            })
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
    // MANIFEST-LAWS-1: every law and invariant of the type, checked
    // set-based; the checks are part of the landing's identity.
    let checks = store
        .dataset_checks(object_type, &table)
        .map_err(dataset_refusal)?;
    let mapping_id = id_of(mapping);
    let mapping_pin = dataset_mapping_pin(&mapping_id, mapping).map_err(Refusal::host)?;
    let pins = store
        .dataset_pins(object_type, &binding, &mapping_pin)
        .map_err(Refusal::host)?;
    Ok(BulkContract {
        mapping_id,
        binding,
        table,
        pins,
        checks,
    })
}

/// MANIFEST-04: a request key and the fingerprint of what it asks: the
/// upload (by its recorded identity), the type and the mapping it names
/// (`null` for the type's only one). The mapping it resolved to is the
/// manifest's own pin, not the request's.
fn keyed_request(
    key: &str,
    request: &DatasetLoadRequest,
    record: &UploadRecord,
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
            "mapping": request.mapping,
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
        rejected: manifest.counts.held.values().sum(),
        reasons: manifest.counts.held.clone(),
        findings: rule_findings(&manifest.counts),
        held_by_rules: manifest.counts.held_by_rules,
        bytes: manifest.source.bytes,
        millis: 0,
    }
}

/// MANIFEST-LAWS-1: the findings of each law and invariant, by rule.
fn rule_findings(counts: &ManifestCounts) -> BTreeMap<String, NornRuleFindings> {
    counts
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
        .collect()
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

/// The pin a dataset manifest names for its mapping: `<artifact id>@<sha256>`
/// of the mapping's Norn1 canonical bytes (#2882), so the key order or number
/// spelling of the stored artifact never changes it.
///
/// # Errors
/// The mapping has no Norn1 canonical form (for example a non-finite number).
pub fn dataset_mapping_pin(
    mapping_id: &str,
    mapping: &serde_json::Value,
) -> Result<String, norn_ir::canonical::CanonicalJsonError> {
    let canonical = norn_ir::canonical::canonical_json_of(mapping)?;
    Ok(format!("{mapping_id}@{}", hex_digest(&canonical)))
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

/// Landing steps one load may take: the landing, its residue settles, then
/// storing its files. MANIFEST-LAWS-1 (S1): the bound every host shares
/// (the engine refuses a landing whose residue takes more pages).
const MAX_LAND_STEPS: u32 = norn_cell::analytics::MAX_ENGINE_STEPS;
/// Residue rows one landing answer carries at most.
const RESIDUE_PAGE: u32 = norn_cell::analytics::RESIDUE_PAGE_ROWS;

/// MANIFEST-LAND-1: type `record` by `object_type` on the engine, land it
/// as the data and held-set Parquet files under an attempt the engine
/// mints, and store them at their content-addressed objects.
///
/// MANIFEST-01: the landing's identity is the digest of its whole request
/// without the attempt: the file, the table, the binding and the pins it is
/// typed and checked by. An answer continues this landing only if it names
/// that identity; the attempt is then the one the answer ran as, and its
/// pins are carried to the commit unchanged.
#[allow(
    clippy::too_many_lines,
    reason = "one landing, step by step: begin, land, settle, store"
)]
fn land_upload<E: SqlEngine>(
    host: &DevHost<E>,
    object_type: &str,
    record: &UploadRecord,
    contract: &BulkContract,
    uploads: &dyn Uploads,
    transport: &dyn Transport,
    now_ms: u64,
) -> Result<Landing, Refusal> {
    // The steps run without the cell, so the definitions may change under
    // them; the commit decides by the contract's pins, carried here.
    let table = contract.table.clone();
    let pins = contract.pins.clone();
    let checks = contract.checks.clone();
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
        checks,
        residue_page: RESIDUE_PAGE,
    };
    let identity = EngineCall::new("land", &request)
        .map_err(engine_failure)?
        .digest;
    request.load.load = format!("l{identity}");
    request.landing.clone_from(&identity);
    let store_digest = hex_digest(format!("store\n{identity}").as_bytes());
    let settle_digest = hex_digest(format!("settle\n{identity}").as_bytes());
    let ours = |answer: &EngineAnswer| {
        [&identity, &settle_digest, &store_digest].contains(&&answer.digest)
    };
    let mut answer = transport.resumed().filter(ours);
    // OLAP-02: the engine mints the attempt (a landing names none); an
    // answer names the attempt it ran as, and the store takes exactly that.
    let unconfigured = || Refusal::host("the object store is not configured");
    let mut call = EngineCall {
        op: "land".to_owned(),
        digest: identity.clone(),
        body: String::new(),
        stream: None,
        credential: None,
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
        if land.residue_total > 0 {
            // MANIFEST-LAWS-1: the residue rows are decided by the per-row
            // evaluator, for the rules the engine left them (LAW-04), then
            // settled on the engine.
            let findings = host
                .cell()?
                .store()
                .residue_findings(object_type, &request.load.table, &land.residue)
                .map_err(dataset_refusal)?;
            let through = land
                .residue
                .last()
                .map(|row| row.row)
                .ok_or_else(|| Refusal::host("the engine reported residue without rows"))?;
            let settle = SettleRequest {
                land: request.clone(),
                through,
                findings,
            };
            call = EngineCall {
                op: "settle".to_owned(),
                digest: settle_digest.clone(),
                body: serde_json::to_string(&settle).map_err(Refusal::host)?,
                stream: None,
                credential: None,
            };
            continue;
        }
        let (Some(data), Some(held)) = (&land.data, &land.held) else {
            return Err(Refusal::host(
                "the engine answered a landing without its files",
            ));
        };
        let keys = [
            object_key(&scope_key, object_type, &data.sha256),
            object_key(&scope_key, object_type, &held.sha256),
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
            credential: None,
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
    let (Some(data), Some(held)) = (&land.data, &land.held) else {
        return Err(stale_landing());
    };
    let cache = dataset_cache_id(table, &data.sha256, &held.sha256);
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
        // LAW-02: the data file holds the typed rows no rule holds back.
        counts: ManifestCounts {
            source: land.load.rows.saturating_add(land.load.rejected),
            admitted: data.rows,
            held: land.load.reasons.clone(),
            findings: land.findings.clone(),
            held_by_rules: land.held_by_rules,
        },
        valid_time: land.valid_time.clone(),
        markings: host
            .cell()?
            .store()
            .dataset_markings(object_type, table)
            .map_err(Refusal::host)?,
    };
    let (hash, receipt) = host
        .cell()?
        .commit_dataset_manifest(&manifest, keyed)
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
/// held it back. Held-back rows are traced too. Requires Population, Values
/// and Sources on the type; no value is disclosed.
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
    uniform_sources_gate(host, scope, actor, object_type, access)?;
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
) -> Result<crate::privacy::DisclosedObjectQueryPage, Refusal> {
    use crate::object_query::{self, Cursor};
    use crate::objects::{NornAppliedQuery, NornQueryTotal};
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
            model_pin: Some(store.model().model_hash().to_owned()),
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
    Ok(crate::privacy::disclosure::project_object_query_page(
        object_type.to_owned(),
        records,
        quarantined,
        NornAppliedQuery {
            sort: query.sort_document(),
            filter: query.filter_document(),
        },
        next,
        Some(NornQueryTotal {
            count: page.total,
            approximate: false,
        }),
    ))
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

/// The marks of the current dataset of `object_type`: the monotone union of
/// its manifest's marked columns and the type's currently declared property
/// markings, under the cell's marking catalog.
fn dataset_marks<E: SqlEngine>(
    host: &DevHost<E>,
    store: &CellStore<E>,
    object_type: &str,
) -> Result<DatasetMarks, Refusal> {
    let reader = store.read();
    let columns = match reader
        .dataset_manifest(norn_cell::MAIN_WORLD, object_type)
        .map_err(Refusal::host)?
    {
        Some(current) => reader
            .effective_dataset_markings(&current.manifest)
            .map_err(Refusal::host)?,
        None => reader
            .model()
            .effective_property_markings(object_type)
            .map_err(Refusal::host)?
            .into_iter()
            .filter(|(_, markings)| !markings.is_empty())
            .collect(),
    };
    Ok(DatasetMarks {
        catalog: host.marking_catalog(&reader)?,
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
) -> Result<crate::privacy::DisclosedQueriedObject, Refusal> {
    crate::privacy::disclosure::project_dataset_queried_object(
        object_type,
        scope_key,
        row,
        values,
        &marks.catalog,
        &marks.columns,
        marks.key.as_ref(),
    )
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
) -> Result<crate::privacy::DisclosedQueriedObject, Refusal> {
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
            model_pin: Some(store.model().model_hash().to_owned()),
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

/// A cell version: its head and foreign-write revision.
type CellVersion = (u64, Option<(u64, u64)>);

/// S3 (#1547): bracket `decide` and `analytics_source` at one cell version (retries 3x).
fn decided_source<E: SqlEngine, T>(
    host: &DevHost<E>,
    store: &CellStore<E>,
    object_type: &str,
    decide: impl Fn(&norn_cell::StoreReader<'_, E>) -> Result<T, Refusal>,
) -> Result<(T, norn_cell::analytics::Source, String, CellVersion), Refusal> {
    for _attempt in 0..3 {
        let reader = store.read();
        let start = crate::object_routes::version_of(&reader)?;
        let decision = decide(&reader)?;
        let (source, snapshot) = store
            .analytics_source(norn_cell::MAIN_WORLD, &host.scope().key(), object_type)
            .map_err(engine_failure)?;
        if crate::object_routes::version_of(&reader)? == start {
            return Ok((decision, source, snapshot, start));
        }
    }
    Err(crate::object_routes::snapshot_moved())
}

fn mirror_unlabeled<E: SqlEngine>(r: &norn_cell::StoreReader<'_, E>, object_type: &str) -> bool {
    crate::privacy::disclosure::mirror_unlabeled(r, object_type)
}

/// How `GET /v1/objects/{type}:profile` handles `object_type` under PRIVACY-1
/// and MANIFEST-MARK-1.
enum ProfilePlan {
    /// Unmarked mirror or unmarked loaded dataset: profile on the engine.
    Engine,
    /// Marked cell-stored type: profile natively over projected values.
    Native,
    /// Loaded dataset with at least one effective marked column: refuse before
    /// touching the engine.
    RefusedColumn(String),
}

fn decide_profile<E: SqlEngine>(
    host: &DevHost<E>,
    reader: &norn_cell::StoreReader<'_, E>,
    object_type: &str,
) -> Result<ProfilePlan, Refusal> {
    if let Some(current) = reader
        .dataset_manifest(norn_cell::MAIN_WORLD, object_type)
        .map_err(Refusal::host)?
    {
        let markings = reader
            .effective_dataset_markings(&current.manifest)
            .map_err(Refusal::host)?;
        if let Some((column, _)) = markings.into_iter().next() {
            return Ok(ProfilePlan::RefusedColumn(column));
        }
        return Ok(ProfilePlan::Engine);
    }
    if host.marking_catalog(reader)?.is_empty() || mirror_unlabeled(reader, object_type) {
        Ok(ProfilePlan::Engine)
    } else {
        Ok(ProfilePlan::Native)
    }
}

fn profile_exchange<E: SqlEngine>(
    store: &CellStore<E>,
    transport: &dyn Transport,
    tenant: &TenantId,
    source: &Source,
    call: &EngineCall,
    resumed: Option<EngineAnswer>,
) -> Result<norn_cell::analytics::ProfileResponse, Refusal> {
    use crate::object_routes::snapshot_moved;
    if matches!(&resumed, Some(a) if a.op == call.op && a.digest != call.digest) {
        return Err(snapshot_moved());
    }
    let world = mirror_world(source);
    let answer =
        exchange_from(store, transport, tenant, world, call, resumed).map_err(engine_failure)?;
    if answer.status != 200 {
        let err = refused(&answer.body);
        if matches!(err, CellError::ComputeRefused("core.compute.stale_input")) {
            return Err(snapshot_moved());
        }
        return Err(engine_failure(err));
    }
    decode(&answer.body).map_err(engine_failure)
}

/// HOST-READ-5 on the engine (D102/D136): authorized column profile.
///
/// # Errors
/// Denied (403), marked column (403), unknown type (404), stale (409), or engine error.
pub fn profile_objects<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: Option<&str>,
    object_type: &str,
    access: &dyn ObjectAccess,
    transport: &dyn Transport,
) -> Result<crate::object_profile::NornObjectProfile, Refusal> {
    use crate::object_routes::{not_found, snapshot_moved, version_of};
    use norn_cell::analytics::ProfileRequest;
    let resumed = transport.resumed();
    let resumed_profile = resumed.as_ref().is_some_and(|a| a.op == "profile");
    let native = || {
        if resumed_profile {
            return Err(snapshot_moved());
        }
        crate::object_profile::get_object_profile(host, scope, actor, object_type, access)
    };
    let native_only = {
        let guard = host.cell()?;
        let r = guard.store().read();
        r.dataset_manifest(norn_cell::MAIN_WORLD, object_type)
            .map_err(Refusal::host)?
            .is_none()
            && !host.marking_catalog(&r)?.is_empty()
            && !mirror_unlabeled(&r, object_type)
    };
    if native_only {
        return native();
    }
    let values = uniform_gate(host, scope, actor, object_type, access)?;
    let guard = host.cell()?;
    let store = guard.store();
    let (plan, source, _snapshot, start_version) = decided_source(host, store, object_type, |r| {
        decide_profile(host, r, object_type)
    })?;
    match plan {
        ProfilePlan::Native => {
            drop(guard);
            return native();
        }
        ProfilePlan::RefusedColumn(column) => return Err(marked_column(&column)),
        ProfilePlan::Engine => {}
    }
    let declared = store
        .model()
        .effective_property_types(object_type)
        .map_err(|_unknown| not_found())?;
    let model_pin = store.model().model_hash().to_owned();
    let table = store.analytics_table(object_type).map_err(engine_failure)?;
    let tenant = TenantId::of_scope(&host.scope().key());
    let call = EngineCall::new(
        "profile",
        &ProfileRequest {
            tenant: tenant.clone(),
            source: source.clone(),
            columns: table.columns.iter().map(|c| c.id.clone()).collect(),
            model_pin: model_pin.clone(),
            cell_revision: format!("{start_version:?}"),
        },
    )
    .map_err(engine_failure)?;
    let profile = profile_exchange(store, transport, &tenant, &source, &call, resumed)?;
    let schema_pin = matches!(source, Source::Mirror { .. }).then_some(table.pin.as_str());
    if store.model().model_hash() != model_pin
        || version_of(&store.read())? != start_version
        || profile
            .provenance
            .verify(
                transport.capabilities().engine,
                &tenant,
                &source,
                &model_pin,
                schema_pin,
            )
            .is_err()
    {
        return Err(snapshot_moved());
    }
    let rows = if values { profile.rows } else { 0 };
    let properties = declared
        .iter()
        .map(|(property, declared_type)| {
            let column = profile
                .columns
                .iter()
                .find(|c| &c.column == property)
                .filter(|_| values);
            property_profile(property, declared_type.as_ref(), column, rows)
        })
        .collect::<Result<_, _>>()?;
    Ok(crate::object_profile::NornObjectProfile {
        object_type: object_type.to_owned(),
        rows,
        withheld_rows: if values { 0 } else { profile.rows },
        quarantined_rows: profile.quarantined,
        approximate: false,
        scan_limit: profile.rows,
        engine: profile.provenance.engine.as_str().to_owned(),
        properties,
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
        let values = read.values.as_ref().expect("values");
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
        assert!(
            !refused
                .values
                .as_ref()
                .expect("values")
                .contains_key("customer")
        );
        assert_eq!(refused.withheld_properties, vec!["customer".to_owned()]);
    }
}
