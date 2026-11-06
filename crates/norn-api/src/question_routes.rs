//! HOST-READ-1 / QUERY-ENGINE-1: admitted questions through the shared read authority.
use crate::{
    authz::{
        ReadCapability::{self, Population, Sources, Values},
        ReadRequest, read_denied,
    },
    host::DevHost,
    refusal::Refusal,
    scope::{CellScope, ScopeQuery},
};
use norn_cell::{
    CellError, CellStore, KnowledgeCutoff, ObjectRecord,
    compute::{
        HistoricalQuestionResult, QueryContext, QueryResult,
        lineage::{LineageDisclosure, NumericTrace, keyed_lineage_handle, lineage_handle_matches},
    },
    engine::SqlEngine,
};
use norn_checker::EffectiveCatalog;
use norn_checker::metric::compute::{QuestionPlan, admit};
use norn_ir::{MetricDefinition, QuestionIR};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use utoipa::IntoParams;

/// Trusted per-request source frontier and authenticated history selection.
pub struct QuestionSnapshot {
    /// Current host population, source coverage, authorization epoch and observation clock.
    pub context: QueryContext,
    /// Server-authenticated receipt coordinate; never decoded from a request.
    pub cutoff: Option<KnowledgeCutoff>,
}

/// Server-owned admission/identity mapping. No client context, roles or SQL enters here.
pub trait QuestionRuntime<E: SqlEngine>: Send + Sync {
    /// Resolve an already admitted question after access is granted.
    /// # Errors
    /// Unknown or no-longer-admitted declarations refuse.
    fn plan(&self, question: &str) -> Result<QuestionPlan, Refusal>;
    /// Resolve the current frontier under the held definition epoch and cell lock.
    /// # Errors
    /// Unavailable or unauthenticated dependencies refuse rather than being fabricated.
    fn snapshot(
        &self,
        store: &CellStore<E>,
        plan: &QuestionPlan,
        actor: &str,
    ) -> Result<QuestionSnapshot, Refusal>;
    /// Map trusted principal/resource inputs without reading cell data. For a collection,
    /// resource ID is the question name and type is its grain; rows use their actual ID/type.
    /// # Errors
    /// Missing principal claims or an unavailable mapping deny the read.
    fn access(
        &self,
        scope: &CellScope,
        actor: &str,
        question: &str,
        capability: ReadCapability,
        row: Option<&ObjectRecord>,
    ) -> Result<norn_kernel::authz::AccessRequest, Refusal>;
    /// DATA-VALUES-1 (D53): the smallest bucket whose aggregate a reader who
    /// lacks Values on any of its members may see; smaller buckets come back
    /// `suppressed`. Declared server policy, not a request parameter; the
    /// default policy is 3. Values below 1 are read as 1.
    fn min_disclosed_bucket(&self) -> usize;
    /// D53: the cell's secret for keyed lineage handles, at least 32 bytes,
    /// held by the host (Durable Object storage or a Worker secret), never
    /// derived from cell data. A shorter key refuses every answer and trace.
    fn handle_key(&self) -> &[u8];
}

/// The declared default minimum disclosed bucket size (D53).
pub const DEFAULT_MIN_DISCLOSED_BUCKET: usize = 3;

fn handle_key<E: SqlEngine>(runtime: &dyn QuestionRuntime<E>) -> Result<&[u8], Refusal> {
    let key = runtime.handle_key();
    if key.len() < 32 {
        return Err(Refusal::host(
            "the lineage handle key is shorter than 32 bytes",
        ));
    }
    Ok(key)
}

/// Existing engine envelopes, serialized directly to preserve raw exact number tokens.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum QuestionAnswer {
    /// Current analytical answer, including its actual freshness label.
    Live(QueryResult),
    /// Answer at the authenticated knowledge cutoff.
    Historical(HistoricalQuestionResult),
}

/// Explicit scope and result coordinates; these select evidence and grant no access.
#[derive(Debug, Default, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[into_params(parameter_in = Query, rename_all = "camelCase")]
pub struct TraceQuery {
    /// Organization, matched against physical scope.
    #[param(required = true, nullable = false)]
    pub org_id: Option<String>,
    /// Environment, matched against physical scope.
    #[param(required = true, nullable = false)]
    pub environment_id: Option<String>,
    /// The `lineageRef` handle a complete exact answer returned (keyed, D53).
    #[param(required = true, nullable = false)]
    pub lineage_ref: Option<String>,
    /// Canonical bucket key from that result.
    #[param(required = true, nullable = false)]
    pub bucket: Option<String>,
}

/// Evaluate using the native exact engine; analytical freshness labels are preserved.
/// # Errors
/// Denied scope/authority, unavailable trusted inputs, or typed compute refusal.
#[utoipa::path(get, path = "/v1/questions/{question}/answer", params(ScopeQuery, ("question" = String, Path)), responses((status = 200, body = crate::answers::NornQuestionAnswer), (status = 400, body = Refusal), (status = 403, body = Refusal), (status = 404, body = Refusal), (status = 413, body = Refusal), (status = 422, body = Refusal), (status = 500, body = Refusal)))]
pub fn get_answer<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: Option<&str>,
    question: &str,
    runtime: &dyn QuestionRuntime<E>,
) -> Result<QuestionAnswer, Refusal> {
    let actor = actor.ok_or_else(read_denied)?;
    let access = mapped(host, runtime, actor, question, Population, None)?;
    let key = handle_key(runtime)?;
    let _epoch = host.read_access(
        scope,
        &ReadRequest {
            capability: Population,
            access: &access,
        },
    )?;
    let plan = runtime.plan(question)?;
    if access.resource.object_type != plan.grain().as_str() {
        return Err(read_denied());
    }
    answer_plan(host, runtime, actor, question, &plan, key)
}

/// Evaluate an admitted `plan` for `actor` under the row gates and D53
/// redaction, after the caller's collection access was decided.
fn answer_plan<E: SqlEngine>(
    host: &DevHost<E>,
    runtime: &dyn QuestionRuntime<E>,
    actor: &str,
    question: &str,
    plan: &QuestionPlan,
    key: &[u8],
) -> Result<QuestionAnswer, Refusal> {
    let cell = host.cell()?;
    let snapshot = runtime.snapshot(cell.store(), plan, actor)?;
    if snapshot.context.scope != host.scope().key() {
        return Err(read_denied());
    }
    let permits = |row: &ObjectRecord, grant| allowed(host, runtime, actor, question, row, grant);
    let mut withheld = BTreeSet::new();
    let visible = |row: &ObjectRecord| visible_recording(row, &permits, &mut withheld);
    let mut answer = match snapshot.cutoff {
        Some(cutoff) => cell
            .store()
            .evaluate_question_known_at(plan, snapshot.context, cutoff, visible)
            .map(QuestionAnswer::Historical),
        None => cell
            .store()
            .evaluate_question(plan, snapshot.context, visible)
            .map(QuestionAnswer::Live),
    }
    .map_err(Refusal::from_compute)?;
    let result = match &mut answer {
        QuestionAnswer::Live(result) => result,
        QuestionAnswer::Historical(historical) => &mut historical.result,
    };
    withhold_values(result, &withheld, runtime.min_disclosed_bucket());
    result.lineage_ref = keyed_lineage_handle(key, &result.lineage_ref);
    Ok(answer)
}

/// Population decides membership; a member without Values is recorded so its
/// bucket's amounts are withheld (DATA-VALUES-1).
fn visible_recording(
    row: &ObjectRecord,
    permits: &dyn Fn(&ObjectRecord, ReadCapability) -> Result<bool, CellError>,
    withheld: &mut BTreeSet<String>,
) -> Result<bool, CellError> {
    let member = permits(row, Population)?;
    if member && !permits(row, Values)? {
        withheld.insert(row.id.clone());
    }
    Ok(member)
}

/// DATA-VALUES-1 (D52/D53): per-member amounts are row values. A bucket with
/// any member the reader may not see the values of withholds every amount, and
/// discloses its aggregate only at `min_disclosed` members or more.
fn withhold_values(result: &mut QueryResult, withheld: &BTreeSet<String>, min_disclosed: usize) {
    for row in result.rows.values_mut() {
        row.withhold(|id| withheld.contains(id), min_disclosed.max(1));
    }
}

/// LINEAGE-SUM-1: revalidate one exact result with separate value/source grants.
/// # Errors
/// Nonexact/stale results, historical tracing, changed pins or unavailable evidence refuse.
#[utoipa::path(get, path = "/v1/questions/{question}/trace", params(TraceQuery, ("question" = String, Path)), responses((status = 200, body = crate::trace_contract::NornNumericTrace), (status = 400, body = Refusal), (status = 403, body = Refusal), (status = 404, body = Refusal), (status = 413, body = Refusal), (status = 422, body = Refusal), (status = 500, body = Refusal)))]
pub fn get_numeric_trace<E: SqlEngine>(
    host: &DevHost<E>,
    query: &TraceQuery,
    actor: Option<&str>,
    question: &str,
    runtime: &dyn QuestionRuntime<E>,
) -> Result<NumericTrace, Refusal> {
    let actor = actor.ok_or_else(read_denied)?;
    let access = mapped(host, runtime, actor, question, Population, None)?;
    let key = handle_key(runtime)?;
    let scope = ScopeQuery {
        org_id: query.org_id.clone(),
        environment_id: query.environment_id.clone(),
    };
    let _epoch = host.read_access(
        &scope,
        &ReadRequest {
            capability: Population,
            access: &access,
        },
    )?;
    let plan = runtime.plan(question)?;
    if access.resource.object_type != plan.grain().as_str() {
        return Err(read_denied());
    }
    let lineage = query
        .lineage_ref
        .as_deref()
        .ok_or_else(|| Refusal::invalid("lineageRef is required"))?;
    let bucket = query
        .bucket
        .as_deref()
        .ok_or_else(|| Refusal::invalid("bucket is required"))?;
    trace_plan(host, runtime, actor, question, &plan, key, lineage, bucket)
}

/// Trace one bucket of an admitted `plan`'s current exact result, named by
/// its keyed handle, after the caller's collection access was decided.
#[allow(clippy::too_many_arguments, reason = "one trace's fixed coordinates")]
fn trace_plan<E: SqlEngine>(
    host: &DevHost<E>,
    runtime: &dyn QuestionRuntime<E>,
    actor: &str,
    question: &str,
    plan: &QuestionPlan,
    key: &[u8],
    lineage: &str,
    bucket: &str,
) -> Result<NumericTrace, Refusal> {
    let cell = host.cell()?;
    let snapshot = runtime.snapshot(cell.store(), plan, actor)?;
    if snapshot.context.scope != host.scope().key() {
        return Err(read_denied());
    }
    if snapshot.cutoff.is_some() {
        return Err(Refusal::from_compute(CellError::ComputeRefused(
            "core.compute.unsupported_engine",
        )));
    }
    let permits = |row: &ObjectRecord, grant| allowed(host, runtime, actor, question, row, grant);
    let mut withheld = BTreeSet::new();
    let mut trace = cell
        .store()
        .trace_question_matching(
            plan,
            snapshot.context,
            &|internal| lineage_handle_matches(key, internal, lineage),
            bucket,
            |row| visible_recording(row, &permits, &mut withheld),
            LineageDisclosure {
                values: &mut |row| permits(row, Values),
                source: &mut |row| permits(row, Sources),
            },
        )
        .map_err(Refusal::from_compute)?;
    withhold_values(&mut trace.result, &withheld, runtime.min_disclosed_bucket());
    lineage.clone_into(&mut trace.result.lineage_ref);
    Ok(trace)
}

fn mapped<E: SqlEngine>(
    host: &DevHost<E>,
    runtime: &dyn QuestionRuntime<E>,
    actor: &str,
    question: &str,
    capability: ReadCapability,
    row: Option<&ObjectRecord>,
) -> Result<norn_kernel::authz::AccessRequest, Refusal> {
    let access = runtime
        .access(host.scope(), actor, question, capability, row)
        .map_err(|_denial| read_denied())?;
    let id = row.map_or(question, |row| row.id.as_str());
    if access.principal.subject_id != actor
        || access.resource.id != id
        || row.is_some_and(|row| {
            access.resource.object_type != row.object_type || row.scope != host.scope().key()
        })
    {
        return Err(read_denied());
    }
    Ok(access)
}
fn allowed<E: SqlEngine>(
    host: &DevHost<E>,
    runtime: &dyn QuestionRuntime<E>,
    actor: &str,
    question: &str,
    row: &ObjectRecord,
    capability: ReadCapability,
) -> Result<bool, CellError> {
    let access = mapped(host, runtime, actor, question, capability, Some(row))
        .map_err(|_mapping| CellError::ComputeRefused("core.compute.stale_input"))?;
    Ok(host.read_permitted(&ReadRequest {
        capability,
        access: &access,
    }))
}

/// F2b: a cell that also answers ad-hoc questions over its declared metrics.
/// A question submitted here has no resource of its own: it is authorized as
/// the union of every catalog metric and object type it reads, all of which
/// must be granted, decided before admission so that a refusal's diagnostics
/// never describe an ungranted resource. A question naming an undeclared
/// metric reads, for this purpose, every declared metric (D62).
pub trait AdHocQuestions<E: SqlEngine>: QuestionRuntime<E> {
    /// The cell's declared metric definitions; a question may name only these.
    fn definitions(&self) -> &[MetricDefinition];
    /// The effective catalog a question is admitted against.
    fn catalog(&self) -> &EffectiveCatalog;
    /// Rows read per evaluation (the admission budget). Above it the
    /// evaluation refuses `core.compute.bound_exceeded` (413); never a
    /// truncated answer.
    fn max_rows(&self) -> u32;
    /// Output buckets per answer. Above it the answer refuses
    /// `question.bucket_bound_exceeded` (413); never a truncated answer.
    fn max_buckets(&self) -> usize;
}

/// `POST /v1/questions:evaluate`: one ad-hoc question to admit and answer.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EvaluateRequest {
    /// The `QuestionIR` to admit against the cell's declared metrics. It is
    /// evaluated once and never installed.
    #[schema(value_type = Object)]
    pub question: QuestionIR,
}

/// `POST /v1/questions:trace`: one bucket of an ad-hoc question's answer.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdHocTraceRequest {
    /// The same `QuestionIR` the answer was evaluated from.
    #[schema(value_type = Object)]
    pub question: QuestionIR,
    /// The `lineageRef` handle that answer returned.
    pub lineage_ref: String,
    /// Canonical bucket key from that answer.
    pub bucket: String,
}

fn bucket_bound_exceeded() -> Refusal {
    Refusal::new(
        http::StatusCode::PAYLOAD_TOO_LARGE,
        "question.bucket_bound_exceeded",
        "the answer has more buckets than this cell serves",
        "narrow the question: a shorter time window, a coarser grain or fewer groups",
    )
}

/// Every metric `question` reads: those it names and, transitively, a
/// ratio's numerator and denominator; and whether it names any metric the
/// cell does not declare.
fn read_metrics<'d>(
    question: &QuestionIR,
    definitions: &'d [MetricDefinition],
) -> (Vec<&'d MetricDefinition>, bool) {
    let mut seen = BTreeSet::new();
    let mut read = Vec::new();
    let mut undeclared = false;
    let mut pending: Vec<&str> = question.metrics.iter().map(|m| m.metric.as_str()).collect();
    while let Some(name) = pending.pop() {
        if !seen.insert(name.to_owned()) {
            continue;
        }
        let Some(definition) = definitions.iter().find(|definition| definition.name == name)
        else {
            undeclared = true;
            continue;
        };
        pending.extend(definition.type_params.numerator.as_deref());
        pending.extend(definition.type_params.denominator.as_deref());
        read.push(definition);
    }
    (read, undeclared)
}

/// The checker reports question findings within a list (`/questions/0/…`);
/// one submitted question is that list's only entry. Two checker passes can
/// report one finding twice; each code at each place is reported once, with
/// its first message.
fn rebased(mut findings: Vec<norn_checker::Diagnostic>) -> Vec<norn_checker::Diagnostic> {
    const ENTRY: &str = "/questions/0";
    for span in findings.iter_mut().filter_map(|finding| finding.span.as_mut()) {
        if span.pointer == ENTRY || span.pointer.starts_with("/questions/0/") {
            span.pointer.replace_range(..ENTRY.len(), "");
        }
    }
    let mut seen = BTreeSet::new();
    findings.retain(|finding| {
        let place = finding.span.as_ref().map(|span| span.pointer.clone());
        seen.insert((finding.code.as_str().to_owned(), place))
    });
    findings
}

/// Authorize every metric and object type `question` reads, then admit it.
fn admit_ad_hoc<'r, E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: &str,
    question: &QuestionIR,
    runtime: &'r dyn AdHocQuestions<E>,
) -> Result<(QuestionPlan, &'r [u8]), Refusal> {
    let key = handle_key(runtime)?;
    let named: &dyn QuestionRuntime<E> = runtime;
    let (named_metrics, undeclared) = read_metrics(question, runtime.definitions());
    // D62: that a named metric does not exist is disclosed (as the checker's
    // 422) only to a caller who may read every metric the cell declares, so
    // it says nothing they could not see. For anyone else an undeclared
    // metric is the same opaque denial as an ungranted one: no existence
    // oracle over the catalog.
    let metrics: Vec<&MetricDefinition> = if undeclared {
        runtime.definitions().iter().collect()
    } else {
        named_metrics
    };
    let mut types = BTreeSet::from([question.object_set.0.clone()]);
    types.extend(metrics.iter().filter_map(|metric| {
        metric
            .type_params
            .metric_aggregation_params
            .as_ref()
            .map(|params| params.semantic_model.0.clone())
    }));
    // The whole read set, before admission: each type as a collection, each
    // metric as a collection resource of a type the question reads.
    let resources = types
        .iter()
        .map(|object_type| (object_type.as_str(), None))
        .chain(metrics.iter().map(|metric| {
            let over = metric
                .type_params
                .metric_aggregation_params
                .as_ref()
                .map(|params| params.semantic_model.0.as_str());
            (metric.name.as_str(), Some(over))
        }));
    for (resource, over) in resources {
        let access = mapped(host, named, actor, resource, Population, None)?;
        let governed = access.resource.object_type.as_str();
        let expected = match over {
            None => governed == resource,
            Some(Some(semantic_model)) => governed == semantic_model,
            Some(None) => types.contains(governed),
        };
        if !expected {
            return Err(read_denied());
        }
        drop(host.read_access(
            scope,
            &ReadRequest {
                capability: Population,
                access: &access,
            },
        )?);
    }
    let plan = admit(
        runtime.definitions(),
        question,
        runtime.catalog(),
        runtime.max_rows(),
    )
    .map_err(|findings| {
        Refusal::new(
            http::StatusCode::UNPROCESSABLE_ENTITY,
            "question.refused",
            "the question is not admissible",
            "each diagnostic names the rule and where in the question it applies",
        )
        .with_diagnostics(&rebased(findings), "/question")
    })?;
    if !types.contains(plan.grain().as_str()) {
        return Err(read_denied());
    }
    Ok((plan, key))
}

/// F2b: admit an ad-hoc question against the cell's declared metrics and
/// answer it once, under the same row gates and D53 redaction as a named
/// question. Nothing is installed.
/// # Errors
/// Any ungranted metric or type, or an undeclared metric from a caller who
/// cannot read every declared metric (one opaque 403, before admission); an
/// inadmissible question, including an undeclared metric from a caller who
/// can (422 `question.refused` with diagnostics); a bound (413); or a
/// compute refusal.
#[utoipa::path(post, path = "/v1/questions:evaluate", params(ScopeQuery), request_body = EvaluateRequest, responses((status = 200, body = crate::answers::NornQuestionAnswer), (status = 400, body = Refusal), (status = 403, body = Refusal), (status = 413, body = Refusal), (status = 422, body = Refusal), (status = 500, body = Refusal)))]
pub fn evaluate_question<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: Option<&str>,
    request: &EvaluateRequest,
    runtime: &dyn AdHocQuestions<E>,
) -> Result<QuestionAnswer, Refusal> {
    let actor = actor.ok_or_else(read_denied)?;
    let (plan, key) = admit_ad_hoc(host, scope, actor, &request.question, runtime)?;
    let _epoch = collection_epoch(host, scope, runtime, actor, &request.question)?;
    let answer = answer_plan(host, runtime, actor, &request.question.name, &plan, key)?;
    let buckets = match &answer {
        QuestionAnswer::Live(result) => result.rows.len(),
        QuestionAnswer::Historical(historical) => historical.result.rows.len(),
    };
    if buckets > runtime.max_buckets() {
        return Err(bucket_bound_exceeded());
    }
    Ok(answer)
}

/// F2b: trace one bucket of an ad-hoc question's current exact answer, by the
/// keyed handle that answer returned. Authorized and admitted as `:evaluate`.
/// # Errors
/// As [`evaluate_question`], and as [`get_numeric_trace`] for the trace.
#[utoipa::path(post, path = "/v1/questions:trace", params(ScopeQuery), request_body = AdHocTraceRequest, responses((status = 200, body = crate::trace_contract::NornNumericTrace), (status = 400, body = Refusal), (status = 403, body = Refusal), (status = 413, body = Refusal), (status = 422, body = Refusal), (status = 500, body = Refusal)))]
pub fn trace_ad_hoc<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: Option<&str>,
    request: &AdHocTraceRequest,
    runtime: &dyn AdHocQuestions<E>,
) -> Result<NumericTrace, Refusal> {
    let actor = actor.ok_or_else(read_denied)?;
    let (plan, key) = admit_ad_hoc(host, scope, actor, &request.question, runtime)?;
    let _epoch = collection_epoch(host, scope, runtime, actor, &request.question)?;
    trace_plan(
        host,
        runtime,
        actor,
        &request.question.name,
        &plan,
        key,
        &request.lineage_ref,
        &request.bucket,
    )
}

/// Hold the definition epoch across evaluation, as a named question does.
fn collection_epoch<'h, E: SqlEngine>(
    host: &'h DevHost<E>,
    scope: &ScopeQuery,
    runtime: &dyn AdHocQuestions<E>,
    actor: &str,
    question: &QuestionIR,
) -> Result<std::sync::RwLockReadGuard<'h, ()>, Refusal> {
    let named: &dyn QuestionRuntime<E> = runtime;
    let access = mapped(host, named, actor, &question.object_set.0, Population, None)?;
    host.read_access(
        scope,
        &ReadRequest {
            capability: Population,
            access: &access,
        },
    )
}
