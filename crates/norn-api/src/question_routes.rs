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
        lineage::{
            LineageDisclosure, NumericTrace, keyed_finding_handle, keyed_lineage_handle,
            keyed_receipt_handle, keyed_revision_handle, lineage_handle_matches,
        },
    },
    engine::SqlEngine,
    system::GENESIS_HASH,
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
    /// The names [`Self::plan`] resolves, for catalogs that list them (an
    /// agent's tool schema). Naming a question grants nothing: every answer
    /// and trace is still authorized. The default lists none.
    fn question_names(&self) -> Vec<String> {
        Vec::new()
    }
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
    /// #1175 (D53): map `capability` over the whole collection `object_type`
    /// (resource ID and type are the type) without reading cell data. A
    /// reader granted Values on every object type of the cell sees raw ledger
    /// and receipt hashes; every other reader sees their keyed handles. The
    /// default maps nothing, so every reader sees handles.
    /// # Errors
    /// Missing principal claims or an unavailable mapping deny the read.
    fn collection_access(
        &self,
        _scope: &CellScope,
        _actor: &str,
        _capability: ReadCapability,
        _object_type: &str,
    ) -> Result<norn_kernel::authz::AccessRequest, Refusal> {
        Err(read_denied())
    }
    /// QUERY-ENGINE-1: evaluate an admitted plan's current answer. The default
    /// is the native exact engine. A host may choose another engine that
    /// returns the same rows; the result's `engine` names the one that
    /// answered. Answers at a knowledge cutoff always use the native engine.
    /// # Errors
    /// A typed compute refusal, or a host engine failure.
    fn evaluate(
        &self,
        store: &CellStore<E>,
        plan: &QuestionPlan,
        context: QueryContext,
        visible: &mut dyn FnMut(&ObjectRecord) -> Result<bool, CellError>,
    ) -> Result<QueryResult, CellError> {
        store.evaluate_question(plan, context, visible)
    }
}

/// The declared default minimum disclosed bucket size (D53).
pub const DEFAULT_MIN_DISCLOSED_BUCKET: usize = 3;

pub(crate) fn handle_key<E: SqlEngine>(runtime: &dyn QuestionRuntime<E>) -> Result<&[u8], Refusal> {
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
#[utoipa::path(get, path = "/v1/questions/{question}/answer", params(ScopeQuery, ("question" = String, Path)), responses((status = 200, description = "The exact answer.", body = crate::answers::NornQuestionAnswer), (status = 400, description = "The request was refused as invalid.", body = Refusal), (status = 403, description = "The request was refused: scope or authority denied.", body = Refusal), (status = 404, description = "The request was refused: not found.", body = Refusal), (status = 413, description = "The request was refused: over a resource bound.", body = Refusal), (status = 422, description = "The request was refused by a typed compute refusal.", body = Refusal), (status = 500, description = "The request was refused: host failure.", body = Refusal)))]
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
    let mut visible = |row: &ObjectRecord| visible_recording(row, &permits, &mut withheld);
    let mut answer = match snapshot.cutoff {
        Some(cutoff) => cell
            .store()
            .evaluate_question_known_at(plan, snapshot.context, cutoff, visible)
            .map(QuestionAnswer::Historical),
        None => runtime
            .evaluate(cell.store(), plan, snapshot.context, &mut visible)
            .map(QuestionAnswer::Live),
    }
    .map_err(Refusal::from_compute)?;
    let world = withheld.is_empty() && world_values(host, runtime, cell.store(), actor);
    let result = match &mut answer {
        QuestionAnswer::Live(result) => result,
        QuestionAnswer::Historical(historical) => {
            withhold_cutoff_receipt(historical, key, world)?;
            &mut historical.result
        }
    };
    withhold_ledger_head(result, key, world);
    withhold_exclusions(result, key, world);
    withhold_values(result, &withheld, runtime.min_disclosed_bucket());
    result.lineage_ref = keyed_lineage_handle(key, &result.lineage_ref);
    Ok(answer)
}

/// #1164 (D53): the receipt coordinate `actor` may see for a commit outside
/// any answer (a retained definition's install). The receipt hash commits to
/// the ledger head, which chains every earlier event's values, so only a
/// reader with whole-world Values sees it raw; every other reader sees its
/// keyed receipt handle. A cell without a question runtime holds no handle
/// key and decides no collection grant, so it discloses neither (`None`).
/// # Errors
/// A handle key shorter than 32 bytes.
pub(crate) fn disclosed_receipt<E: SqlEngine>(
    host: &DevHost<E>,
    runtime: Option<&dyn AdHocQuestions<E>>,
    store: &CellStore<E>,
    actor: &str,
    receipt_hash: &str,
) -> Result<Option<String>, Refusal> {
    let Some(runtime) = runtime else {
        return Ok(None);
    };
    let runtime: &dyn QuestionRuntime<E> = runtime;
    let key = handle_key(runtime)?;
    Ok(Some(if world_values(host, runtime, store, actor) {
        receipt_hash.to_owned()
    } else {
        keyed_receipt_handle(key, receipt_hash)
    }))
}

/// #1175 (D53): whether `actor` holds Values over the whole world: a
/// collection-level grant on every object type of the cell, decided by the
/// host policy without reading cell data. Raw ledger and receipt hashes chain
/// every event's values, so only such a reader sees them. An unmappable or
/// mismatched mapping is no grant.
pub(crate) fn world_values<E: SqlEngine>(
    host: &DevHost<E>,
    runtime: &dyn QuestionRuntime<E>,
    store: &CellStore<E>,
    actor: &str,
) -> bool {
    let mut types = store.model().object_types().peekable();
    types.peek().is_some()
        && types.all(|object_type| {
            let name = object_type.name.as_str();
            runtime
                .collection_access(host.scope(), actor, Values, name)
                .is_ok_and(|access| {
                    access.principal.subject_id == actor
                        && access.resource.object_type == name
                        && access.resource.id == name
                        && host.read_permitted(&ReadRequest {
                            capability: Values,
                            access: &access,
                        })
                })
        })
}

/// #1175 (DATA-VALUES-1, D53): `revision.hash` is the unkeyed ledger chain
/// hash, and the chained events carry record values: a reader who saw the
/// previous revision could confirm a guessed value of one new event offline.
/// A reader without whole-world Values sees the keyed revision handle; the
/// genesis head chains nothing and stays. Internal pins keep the raw head.
fn withhold_ledger_head(result: &mut QueryResult, key: &[u8], world: bool) {
    let genesis = result.revision.seq == 0 && result.revision.hash == GENESIS_HASH;
    if !(world || genesis) {
        result.revision.hash = keyed_revision_handle(key, &result.revision.hash);
    }
}

/// #1175 (DATA-VALUES-1, D53): an exclusion maps a quarantined record to its
/// finding ID, and the finding identity hashes that record's canonical values
/// without a key: Population alone shows the exclusion, so a reader without
/// Values could confirm a guessed value offline. A reader without whole-world
/// Values sees the keyed finding handle; the record IDs stay (Population).
fn withhold_exclusions(result: &mut QueryResult, key: &[u8], world: bool) {
    if !world {
        for finding in result.exclusions.values_mut() {
            *finding = keyed_finding_handle(key, finding);
        }
    }
}

/// #1175 (DATA-VALUES-1, D53): the historical world names its cutoff commit by
/// receipt hash, which commits to that commit's values and to the ledger head.
/// A reader without whole-world Values sees the keyed receipt handle instead,
/// so the coordinate never confirms a guess offline.
fn withhold_cutoff_receipt(
    historical: &mut HistoricalQuestionResult,
    key: &[u8],
    world: bool,
) -> Result<(), Refusal> {
    let norn_kernel::World::Historical { commit } = &mut historical.world else {
        return Err(Refusal::from_compute(CellError::ComputeRefused(
            "core.compute.unsupported_engine",
        )));
    };
    // Genesis has no receipt: its coordinate commits to nothing.
    if !world && historical.result.known_as_of != 0 {
        commit.0 = keyed_receipt_handle(key, &commit.0);
    }
    Ok(())
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
#[utoipa::path(get, path = "/v1/questions/{question}/trace", params(TraceQuery, ("question" = String, Path)), responses((status = 200, description = "The numeric trace.", body = crate::trace_contract::NornNumericTrace), (status = 400, description = "The request was refused as invalid.", body = Refusal), (status = 403, description = "The request was refused: scope or authority denied.", body = Refusal), (status = 404, description = "The request was refused: not found.", body = Refusal), (status = 413, description = "The request was refused: over a resource bound.", body = Refusal), (status = 422, description = "The request was refused by a typed compute refusal.", body = Refusal), (status = 500, description = "The request was refused: host failure.", body = Refusal)))]
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
    let world = withheld.is_empty() && world_values(host, runtime, cell.store(), actor);
    withhold_ledger_head(&mut trace.result, key, world);
    withhold_exclusions(&mut trace.result, key, world);
    if !world {
        for receipt in trace
            .contributions
            .iter_mut()
            .filter_map(|c| c.receipt_hash.as_mut())
        {
            *receipt = keyed_receipt_handle(key, receipt);
        }
    }
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
    #[schema(schema_with = evaluate_question_schema)]
    pub question: QuestionIR,
}

/// `POST /v1/questions:trace`: one bucket of an ad-hoc question's answer.
#[derive(Debug, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdHocTraceRequest {
    /// The same `QuestionIR` the answer was evaluated from.
    #[schema(schema_with = trace_question_schema)]
    pub question: QuestionIR,
    /// The `lineageRef` handle that answer returned.
    pub lineage_ref: String,
    /// Canonical bucket key from that answer.
    pub bucket: String,
}

fn evaluate_question_schema() -> utoipa::openapi::schema::Object {
    crate::objects::free_form(
        "The `QuestionIR` to admit against the cell's declared metrics. It is evaluated once \
         and never installed.",
        false,
    )
}

fn trace_question_schema() -> utoipa::openapi::schema::Object {
    crate::objects::free_form(
        "The same `QuestionIR` the answer was evaluated from.",
        false,
    )
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
        let Some(definition) = definitions
            .iter()
            .find(|definition| definition.name == name)
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
    for span in findings
        .iter_mut()
        .filter_map(|finding| finding.span.as_mut())
    {
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
    match check_ad_hoc(host, scope, actor, question, runtime)? {
        Ok(plan) => Ok((plan, key)),
        Err(findings) => Err(Refusal::new(
            http::StatusCode::UNPROCESSABLE_ENTITY,
            "question.refused",
            "the question is not admissible",
            "each diagnostic names the rule and where in the question it applies",
        )
        .with_diagnostics(&findings, "/question")),
    }
}

/// F2b's question admission without evaluation: authorize every metric and
/// object type `question` reads for `actor` (one opaque 403 otherwise,
/// decided before admission), then admit it against the cell's declared
/// metrics. Returns the plan, or the checker's findings with pointers into
/// the question. Saved definitions (dashboard tiles) admit their questions
/// through this same path.
/// # Errors
/// An ungranted or undeclared metric or type, or a scope mismatch.
pub fn check_ad_hoc<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: &str,
    question: &QuestionIR,
    runtime: &dyn AdHocQuestions<E>,
) -> Result<Result<QuestionPlan, Vec<norn_checker::Diagnostic>>, Refusal> {
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
    let plan = match admit(
        runtime.definitions(),
        question,
        runtime.catalog(),
        runtime.max_rows(),
    ) {
        Ok(plan) => plan,
        // D62: pointers into the one submitted question, each finding once.
        Err(findings) => return Ok(Err(rebased(findings))),
    };
    if !types.contains(plan.grain().as_str()) {
        return Err(read_denied());
    }
    Ok(Ok(plan))
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
#[utoipa::path(post, path = "/v1/questions:evaluate", params(ScopeQuery), request_body = EvaluateRequest, responses((status = 200, description = "The exact answer.", body = crate::answers::NornQuestionAnswer), (status = 400, description = "The request was refused as invalid.", body = Refusal), (status = 403, description = "The request was refused: scope or authority denied.", body = Refusal), (status = 413, description = "The request was refused: over a resource bound.", body = Refusal), (status = 422, description = "The request was refused by a typed compute refusal.", body = Refusal), (status = 500, description = "The request was refused: host failure.", body = Refusal)))]
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
#[utoipa::path(post, path = "/v1/questions:trace", params(ScopeQuery), request_body = AdHocTraceRequest, responses((status = 200, description = "The numeric trace.", body = crate::trace_contract::NornNumericTrace), (status = 400, description = "The request was refused as invalid.", body = Refusal), (status = 403, description = "The request was refused: scope or authority denied.", body = Refusal), (status = 413, description = "The request was refused: over a resource bound.", body = Refusal), (status = 422, description = "The request was refused by a typed compute refusal.", body = Refusal), (status = 500, description = "The request was refused: host failure.", body = Refusal)))]
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

/// One metric an ad-hoc question may name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornMetricSummary {
    /// The metric's name, as a question's `aggregate` names it.
    pub name: String,
    /// Its label.
    pub label: String,
    /// `simple`, `ratio`, `cumulative`, `derived` or `conversion`.
    pub kind: String,
    /// The concept it aggregates, for a simple metric.
    #[schema(required = true, nullable = true)]
    pub object_type: Option<String>,
    /// The aggregated expression (a property), for a simple metric.
    #[schema(required = true, nullable = true)]
    pub expression: Option<String>,
    /// The time property its buckets use, for a simple metric.
    #[schema(required = true, nullable = true)]
    pub time_property: Option<String>,
    /// The numerator metric, for a ratio.
    #[schema(required = true, nullable = true)]
    pub numerator: Option<String>,
    /// The denominator metric, for a ratio.
    #[schema(required = true, nullable = true)]
    pub denominator: Option<String>,
}

/// The metrics ad-hoc questions may name, among those the caller may read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornMetricList {
    /// In name order.
    pub metrics: Vec<NornMetricSummary>,
}

/// F2b: list the metrics an ad-hoc question may name. A metric is listed
/// only when the caller may read its concept's population (HOST-READ-1), so
/// the list never names a concept the caller cannot read.
/// # Errors
/// No actor, or a host failure.
#[utoipa::path(get, path = "/v1/questions:metrics", params(ScopeQuery), responses((status = 200, description = "The metrics ad-hoc questions may name.", body = NornMetricList), (status = 403, description = "The request was refused: scope or authority denied.", body = Refusal), (status = 500, description = "The request was refused: host failure.", body = Refusal)))]
pub fn list_metrics<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: Option<&str>,
    runtime: &dyn AdHocQuestions<E>,
) -> Result<NornMetricList, Refusal> {
    let actor = actor.ok_or_else(read_denied)?;
    let named: &dyn QuestionRuntime<E> = runtime;
    let readable = |object_type: &str| {
        mapped(host, named, actor, object_type, Population, None).is_ok_and(|access| {
            host.read_access(
                scope,
                &ReadRequest {
                    capability: Population,
                    access: &access,
                },
            )
            .is_ok()
        })
    };
    let mut metrics: Vec<NornMetricSummary> = runtime
        .definitions()
        .iter()
        .map(|definition| {
            let aggregation = definition.type_params.metric_aggregation_params.as_ref();
            NornMetricSummary {
                name: definition.name.clone(),
                label: definition.label.clone(),
                kind: serde_json::to_value(definition.metric_type)
                    .ok()
                    .and_then(|kind| kind.as_str().map(str::to_owned))
                    .unwrap_or_default(),
                object_type: aggregation.map(|params| params.semantic_model.0.clone()),
                expression: definition.type_params.expr.clone(),
                time_property: aggregation.map(|params| params.agg_time_dimension.clone()),
                numerator: definition.type_params.numerator.clone(),
                denominator: definition.type_params.denominator.clone(),
            }
        })
        .collect();
    // A ratio is listed when both of its operands are.
    let simple: BTreeSet<String> = metrics
        .iter()
        .filter(|metric| metric.object_type.as_deref().is_some_and(&readable))
        .map(|metric| metric.name.clone())
        .collect();
    metrics.retain(|metric| {
        simple.contains(&metric.name)
            || (metric.object_type.is_none()
                && metric
                    .numerator
                    .as_ref()
                    .is_some_and(|name| simple.contains(name))
                && metric
                    .denominator
                    .as_ref()
                    .is_some_and(|name| simple.contains(name)))
    });
    metrics.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(NornMetricList { metrics })
}

/// One object type an ad-hoc question may range over, with every effective
/// property and its resolved type (`null` for a registry or unresolved type).
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CatalogObjectType {
    /// The object type's name: a question's `objectSet`.
    pub name: String,
    /// Its effective properties, in declaration order.
    pub properties: Vec<CatalogProperty>,
}

/// One effective property: its id and its resolved type, as declared.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CatalogProperty {
    /// The property id: what a group-by or filter path names.
    pub id: String,
    /// The resolved type (kind, unit, scale, additivity), or `null`.
    #[serde(rename = "type")]
    #[schema(schema_with = property_type_schema)]
    pub property_type: Option<norn_ir::PropertyType>,
}

/// `GET /v1/questions:catalog`: what this caller may build an ad-hoc question
/// from, and what this cell's engine admits. Every metric and object type
/// listed is one the caller may read the population of (D67): a ratio only
/// when both its operands are, a type only when it is a readable metric's
/// population. Nothing listed is a promise: `:evaluate` still admits.
#[derive(Debug, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QuestionCatalog {
    /// The object types the listed metrics range over.
    pub object_types: Vec<CatalogObjectType>,
    /// The listed metrics, as declared, in name order.
    #[schema(schema_with = metrics_schema)]
    pub metrics: Vec<MetricDefinition>,
    /// Calendar grains a group-by may bucket a time property by; a group-by
    /// without a grain groups by the property's values.
    #[schema(value_type = Vec<String>)]
    pub grains: Vec<norn_ir::metric::TimeGrain>,
    /// Filter operators the engine admits.
    #[schema(value_type = Vec<String>)]
    pub operators: Vec<norn_ir::metric::FilterOperator>,
    /// Most group-bys one question may have.
    pub max_group_by: usize,
    /// Most buckets one answer may have (above it, 413).
    pub max_buckets: usize,
}

// `schema_with` replaces the field's schema, doc comment included, so each
// helper restates its field's description.
fn property_type_schema() -> utoipa::openapi::schema::Object {
    crate::objects::free_form(
        "The resolved type (kind, unit, scale, additivity), or null for a registry or unresolved type.",
        true,
    )
}

fn metrics_schema() -> utoipa::openapi::schema::Array {
    let mut array = utoipa::openapi::schema::Array::new(crate::objects::free_form(
        "A declared metric definition: MetricFlow fields with Norn policy under `x-norn`.",
        false,
    ));
    array.description = Some("The listed metrics, as declared, in name order.".to_owned());
    array
}

/// `GET /v1/questions:catalog`: the metrics, object types and engine bounds
/// an ad-hoc question is built from, for this caller (D67: only what the
/// caller may read the population of).
/// # Errors
/// No actor or a scope that is not this cell's (one opaque 403), or a host
/// failure reading the model.
#[utoipa::path(get, path = "/v1/questions:catalog", params(ScopeQuery), responses((status = 200, description = "The question catalog for this caller.", body = QuestionCatalog), (status = 400, description = "The request was refused as invalid.", body = Refusal), (status = 403, description = "The request was refused: scope or authority denied.", body = Refusal), (status = 500, description = "The request was refused: host failure.", body = Refusal)))]
pub fn question_catalog<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    actor: Option<&str>,
    runtime: &dyn AdHocQuestions<E>,
) -> Result<QuestionCatalog, Refusal> {
    let actor = actor.ok_or_else(read_denied)?;
    host.scope()
        .admit(scope.org_id.as_deref(), scope.environment_id.as_deref())
        .map_err(|_scope| read_denied())?;
    let named: &dyn QuestionRuntime<E> = runtime;
    // Whether `actor` may read `resource`'s population, governed by `over`.
    let readable = |resource: &str, over: &dyn Fn(&str) -> bool| {
        mapped(host, named, actor, resource, Population, None).is_ok_and(|access| {
            over(access.resource.object_type.as_str())
                && host.read_permitted(&ReadRequest {
                    capability: Population,
                    access: &access,
                })
        })
    };
    let population = |metric: &MetricDefinition| {
        metric
            .type_params
            .metric_aggregation_params
            .as_ref()
            .map(|params| params.semantic_model.0.clone())
    };
    let definitions = runtime.definitions();
    let mut simple: BTreeSet<&str> = BTreeSet::new();
    let mut types: BTreeSet<String> = BTreeSet::new();
    for metric in definitions {
        if let Some(over) = population(metric)
            && readable(&over, &|governed| governed == over)
            && readable(&metric.name, &|governed| governed == over)
        {
            simple.insert(metric.name.as_str());
            types.insert(over);
        }
    }
    // A ratio is listed only when both its operands are.
    let operands_listed = |metric: &MetricDefinition| {
        [
            &metric.type_params.numerator,
            &metric.type_params.denominator,
        ]
        .into_iter()
        .all(|operand| operand.as_deref().is_some_and(|name| simple.contains(name)))
    };
    let mut metrics: Vec<MetricDefinition> = definitions
        .iter()
        .filter(|metric| {
            simple.contains(metric.name.as_str())
                || (population(metric).is_none()
                    && operands_listed(metric)
                    && readable(&metric.name, &|governed| types.contains(governed)))
        })
        .cloned()
        .collect();
    metrics.sort_by(|a, b| a.name.cmp(&b.name));
    let object_types = {
        let cell = host.cell()?;
        let reader = cell.store().read();
        types
            .into_iter()
            .filter(|name| reader.model().has_type(name))
            .map(|name| {
                let properties = reader
                    .model()
                    .effective_property_types(&name)
                    .map_err(Refusal::host)?
                    .into_iter()
                    .map(|(id, property_type)| CatalogProperty { id, property_type })
                    .collect();
                Ok(CatalogObjectType { name, properties })
            })
            .collect::<Result<Vec<_>, Refusal>>()?
    };
    Ok(QuestionCatalog {
        object_types,
        metrics,
        grains: norn_checker::metric::compute::GRAINS.to_vec(),
        operators: norn_checker::metric::compute::OPERATORS.to_vec(),
        max_group_by: norn_checker::metric::compute::MAX_GROUP_BY,
        max_buckets: runtime.max_buckets(),
    })
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
