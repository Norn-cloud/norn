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
use norn_checker::metric::compute::QuestionPlan;
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
    let cell = host.cell()?;
    let snapshot = runtime.snapshot(cell.store(), &plan, actor)?;
    if snapshot.context.scope != host.scope().key() {
        return Err(read_denied());
    }
    let permits = |row: &ObjectRecord, grant| allowed(host, runtime, actor, question, row, grant);
    let mut withheld = BTreeSet::new();
    let visible = |row: &ObjectRecord| visible_recording(row, &permits, &mut withheld);
    let mut answer = match snapshot.cutoff {
        Some(cutoff) => cell
            .store()
            .evaluate_question_known_at(&plan, snapshot.context, cutoff, visible)
            .map(QuestionAnswer::Historical),
        None => cell
            .store()
            .evaluate_question(&plan, snapshot.context, visible)
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
    let cell = host.cell()?;
    let snapshot = runtime.snapshot(cell.store(), &plan, actor)?;
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
            &plan,
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
