//! D102: a cell's link to its tenant analytics engine. Questions on a host
//! with an engine are answered there, never by an analytics scan of the
//! cell's own store; the cell keeps admission, authorization, the row gates
//! and the answer's lineage, and verifies every engine answer.
//!
//! An answer is a short exchange of typed engine operations: the mirror's
//! frontier, sync batches of committed versions until the mirror reaches the
//! verified head, then the lowered question. The exchange is stateless: each
//! step is decided from the last answer alone. Natively ([`InProcess`]) the
//! steps run in place. A Durable Object cannot block on I/O, so there
//! ([`Exchange`]) each step ends the request with the call the Worker makes,
//! and the Worker re-enters the cell with the answer; the cell then decides
//! the request again from the start, authorization included.

use std::cell::RefCell;
use std::collections::BTreeMap;

use norn_api::authz::ReadCapability;
use norn_api::question_routes::{AdHocQuestions, QuestionRuntime, QuestionSnapshot};
use norn_api::refusal::Refusal;
use norn_api::scope::CellScope;
use norn_cell::analytics::{
    EngineRefusal, EvaluateRequest, EvaluateResponse, FrontierResponse, Source, TenantId, hex_digest,
};
use norn_cell::compute::{ALL_MEMBERS, QueryContext, QueryResult};
use norn_cell::engine::SqlEngine;
use norn_cell::{CellError, CellStore, ObjectRecord};
use norn_checker::EffectiveCatalog;
use norn_checker::metric::compute::{QuestionPlan, UNSUPPORTED_ENGINE};
use norn_ir::MetricDefinition;
use serde::{Deserialize, Serialize};

/// Committed versions sent per sync batch.
const SYNC_BATCH: usize = 5_000;
/// Engine steps one answer may take before it refuses.
pub const MAX_STEPS: usize = 64;

/// One engine operation, as the host sends it: `POST /v1/<op>` with `body`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EngineCall {
    /// `frontier`, `sync`, `evaluate`, `members`, `profile`, `load` or `export`.
    pub op: String,
    /// SHA-256 of `body`; the answer names it, so a stale answer is never used.
    pub digest: String,
    /// The typed request, as JSON.
    pub body: String,
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
        let body = serde_json::to_string(request).map_err(|e| CellError::Encoding(e.to_string()))?;
        Ok(Self {
            op: op.to_owned(),
            digest: hex_digest(body.as_bytes()),
            body,
        })
    }
}

/// How the cell reaches its engine.
pub trait Transport {
    /// The answer the host re-entered the cell with, once.
    fn resumed(&self) -> Option<EngineAnswer>;
    /// Perform `call` now, or `None` when the host must perform it and re-enter.
    fn call(&self, call: &EngineCall) -> Option<EngineAnswer>;
}

/// The Durable Object transport: a call ends the request; the Worker performs
/// it and re-enters with the answer.
#[derive(Default)]
pub struct Exchange {
    answer: RefCell<Option<EngineAnswer>>,
    pending: RefCell<Option<EngineCall>>,
}

impl Exchange {
    /// Re-enter with the answer to the previous call.
    pub fn resume(&self, answer: EngineAnswer) {
        self.answer.replace(Some(answer));
        self.pending.replace(None);
    }

    /// The call this request ended with, if any; taken once.
    pub fn take_pending(&self) -> Option<EngineCall> {
        self.answer.replace(None);
        self.pending.take()
    }
}

impl Transport for Exchange {
    fn resumed(&self) -> Option<EngineAnswer> {
        self.answer.take()
    }
    fn call(&self, call: &EngineCall) -> Option<EngineAnswer> {
        self.pending.replace(Some(call.clone()));
        None
    }
}

/// The engine linked into a native host: each call runs in place.
#[cfg(all(feature = "duckdb", not(target_arch = "wasm32")))]
pub struct InProcess(pub norn_cell::analytics::engine::Engine);

#[cfg(all(feature = "duckdb", not(target_arch = "wasm32")))]
impl InProcess {
    fn run<T: serde::de::DeserializeOwned, R: Serialize>(
        body: &str,
        operation: impl FnOnce(&T) -> norn_cell::analytics::engine::Outcome<R>,
    ) -> (u16, String) {
        let outcome = serde_json::from_str::<T>(body)
            .map_err(|e| EngineRefusal {
                code: "core.analytics.invalid".to_owned(),
                detail: e.to_string(),
            })
            .and_then(|request| operation(&request));
        match outcome {
            Ok(response) => (200, serde_json::to_string(&response).unwrap_or_default()),
            Err(refused) => (
                refusal_status(&refused.code),
                serde_json::to_string(&refused).unwrap_or_default(),
            ),
        }
    }
}

#[cfg(all(feature = "duckdb", not(target_arch = "wasm32")))]
impl Transport for InProcess {
    fn resumed(&self) -> Option<EngineAnswer> {
        None
    }
    fn call(&self, call: &EngineCall) -> Option<EngineAnswer> {
        let engine = &self.0;
        let (status, body) = match call.op.as_str() {
            "frontier" => Self::run(&call.body, |r| engine.frontier(r)),
            "sync" => Self::run(&call.body, |r| engine.sync(r)),
            "evaluate" => Self::run(&call.body, |r| engine.evaluate(r)),
            "members" => Self::run(&call.body, |r| engine.members(r)),
            "profile" => Self::run(&call.body, |r| engine.profile(r)),
            _ => (404, String::new()),
        };
        Some(EngineAnswer {
            op: call.op.clone(),
            digest: call.digest.clone(),
            status,
            body,
        })
    }
}

/// The status an engine refusal answers with (the sidecar uses the same).
#[must_use]
pub fn refusal_status(code: &str) -> u16 {
    match code {
        "core.analytics.frontier_moved" | "core.analytics.frontier_behind" | "core.compute.stale_input" => 409,
        "core.analytics.engine" => 500,
        code if code.starts_with("core.compute.") || code.starts_with("core.bulk.") => 422,
        _ => 400,
    }
}

/// The marker error of a request that ended with an engine call.
fn pending() -> CellError {
    CellError::Engine("an analytics engine call is pending".to_owned())
}

fn decode<T: serde::de::DeserializeOwned>(body: &str) -> Result<T, CellError> {
    serde_json::from_str(body).map_err(|e| CellError::Engine(format!("engine answer: {e}")))
}

/// A Core 03 refusal the engine reported, as the cell's own.
fn refused(body: &str) -> CellError {
    let code = decode::<EngineRefusal>(body).map(|r| r.code).unwrap_or_default();
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

/// ANALYTICS-FRONTIER-1 then QUERY-ENGINE-1: bring the mirror to the prepared
/// head and evaluate there.
fn exchange<E: SqlEngine>(
    store: &CellStore<E>,
    transport: &dyn Transport,
    tenant: &TenantId,
    request: &EvaluateRequest,
) -> Result<EvaluateResponse, CellError> {
    let evaluate = EngineCall::new("evaluate", request)?;
    let (world, head) = match &request.source {
        Source::Mirror { world, frontier, .. } => (world.clone(), *frontier),
        Source::Dataset { .. } => {
            let answer = match transport.resumed().filter(|a| a.digest == evaluate.digest) {
                Some(answer) => answer,
                None => transport.call(&evaluate).ok_or_else(pending)?,
            };
            return if answer.status == 200 {
                decode(&answer.body)
            } else {
                Err(refused(&answer.body))
            };
        }
    };
    let frontier = EngineCall::new("frontier", &store.analytics_frontier(tenant, &world)?)?;
    let mut answer = transport.resumed();
    let mut next = frontier.clone();
    for _ in 0..MAX_STEPS {
        if let Some(last) = answer.take() {
            next = match (last.op.as_str(), last.status) {
                ("evaluate", 200) if last.digest == evaluate.digest => return decode(&last.body),
                ("frontier" | "sync", 200) => {
                    let reached = decode::<FrontierResponse>(&last.body)?.frontier;
                    if reached >= head {
                        evaluate.clone()
                    } else {
                        let batch = store.analytics_delta(tenant, &world, reached, SYNC_BATCH)?;
                        EngineCall::new("sync", &batch.request)?
                    }
                }
                // A moved mirror or a stale answer: start from its frontier again.
                ("evaluate", 200) | (_, 409) => frontier.clone(),
                (_, _) => return Err(refused(&last.body)),
            };
        }
        answer = Some(transport.call(&next).ok_or_else(pending)?);
    }
    Err(CellError::Engine("the analytics exchange did not settle".to_owned()))
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
    pub fn new(inner: &'a dyn AdHocQuestions<E>, transport: &'a dyn Transport, scope: &CellScope) -> Self {
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
    fn snapshot(&self, store: &CellStore<E>, plan: &QuestionPlan, actor: &str) -> Result<QuestionSnapshot, Refusal> {
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
        self.inner.collection_access(scope, actor, capability, object_type)
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
            exchange(store, self.transport, &self.tenant, &prepared.request)?
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
