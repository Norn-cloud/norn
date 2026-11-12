//! RUN-ROUTES-1 and DECISIONS-1 on the DO host: start (`POST /v1/runs`) and
//! read (`GET /v1/runs/{id}`) runs through `norn_api::run_routes`, and list
//! (`GET /v1/decisions`) and decide (`POST /v1/decisions/{id}:decide`) the
//! decisions runs wait on through `norn_api::decisions`, scoped by the query
//! string. Starting is the host
//! operation `StartRun`, decided by the cell's policy; every run read is the
//! caller's ordinary Values read of the run's object type, mapped by
//! [`collection_access`]. A caller with no verified principal reads nothing.

use std::collections::BTreeMap;

use http::StatusCode;
use norn_api::authz::ReadCapability;
use norn_api::decisions::{DecideCall, DecideRequest, DecisionsQuery, decide, list_decisions};
use norn_api::host::DevHost;
use norn_api::question_routes::AdHocQuestions;
use norn_api::refusal::Refusal;
use norn_api::run_routes::{StartRunRequest, get_run, start_run};
use norn_api::scope::ScopeQuery;
use norn_cell::engine::SqlEngine;

use crate::policy::collection_access;
use crate::server::{Request, Response, document_json, parse};

/// Serve `request` when its path is a run or decision route; `None` for
/// every other path. `questions` is the cell's receipt-disclosure authority
/// (D53): a cell without one decides nothing.
pub(crate) fn dispatch<E: SqlEngine>(
    host: &DevHost<E>,
    questions: Option<&dyn AdHocQuestions<E>>,
    actor: Option<&str>,
    request: &Request<'_>,
) -> Option<Response> {
    let result = if let Some(route) = runs_route(request.path) {
        runs(host, actor, route, request)
    } else {
        let route = decisions_route(request.path)?;
        decisions(host, questions, actor, route, request)
    };
    Some(match result {
        Ok((status, body)) => Response {
            status: status.as_u16(),
            body,
        },
        Err(refused) => Response {
            status: refused.status,
            body: serde_json::to_string(&refused).unwrap_or_else(|_| String::from("{}")),
        },
    })
}

/// A run route: `/v1/runs` (start) or `/v1/runs/{id}` (read).
#[derive(Debug, Clone, Copy)]
enum RunsRoute<'p> {
    Collection,
    One(&'p str),
}

fn runs_route(path: &str) -> Option<RunsRoute<'_>> {
    if path == "/v1/runs" {
        return Some(RunsRoute::Collection);
    }
    path.strip_prefix("/v1/runs/")
        .filter(|id| !id.is_empty() && !id.contains('/'))
        .map(RunsRoute::One)
}

fn runs<E: SqlEngine>(
    host: &DevHost<E>,
    actor: Option<&str>,
    route: RunsRoute<'_>,
    request: &Request<'_>,
) -> Result<(StatusCode, String), Refusal> {
    let scope_of = |fields: &mut BTreeMap<&'static str, String>| ScopeQuery {
        org_id: fields.remove("orgId"),
        environment_id: fields.remove("environmentId"),
    };
    let reads = |subject: &str| {
        let subject = subject.to_owned();
        let scope = host.scope().clone();
        move |capability: ReadCapability, object_type: &str| -> norn_kernel::authz::AccessRequest {
            collection_access(&scope, &subject, capability, object_type)
        }
    };
    match (route, request.method) {
        (RunsRoute::Collection, "POST") => {
            let mut fields = parse(request.query, &["orgId", "environmentId"])?;
            let scope = scope_of(&mut fields);
            // A caller with no verified principal is refused by the start
            // itself, with the same document as any other denied start.
            let subject = actor.unwrap_or_default();
            let body: StartRunRequest = serde_json::from_str(request.body).map_err(|error| {
                Refusal::invalid(format!("the request body is not a run start: {error}"))
            })?;
            let now = norn_kernel::LogicalTime::from_millis(request.now.saturating_mul(1000));
            let (status, run) = start_run(host, &scope, actor, &body, &reads(subject), now)?;
            Ok((status, document_json(&run)?))
        }
        (RunsRoute::One(id), "GET") => {
            let mut fields = parse(request.query, &["orgId", "environmentId"])?;
            let scope = scope_of(&mut fields);
            let Some(subject) = actor else {
                return Err(norn_api::authz::read_denied());
            };
            let id = percent_encoding::percent_decode_str(id)
                .decode_utf8()
                .map_err(|_invalid| Refusal::invalid("the run path segment is not UTF-8"))?;
            Ok((
                StatusCode::OK,
                document_json(&get_run(host, &scope, subject, &id, &reads(subject))?)?,
            ))
        }
        (RunsRoute::Collection, _) => Err(not_allowed("a run is started with POST")),
        (RunsRoute::One(_), _) => Err(not_allowed("a run is read with GET")),
    }
}

/// A decision route: `/v1/decisions` or `/v1/decisions/{id}:decide`.
#[derive(Debug, Clone, Copy)]
enum DecisionsRoute<'p> {
    Collection,
    Decide(&'p str),
}

fn decisions_route(path: &str) -> Option<DecisionsRoute<'_>> {
    if path == "/v1/decisions" {
        return Some(DecisionsRoute::Collection);
    }
    path.strip_prefix("/v1/decisions/")
        .and_then(|rest| rest.strip_suffix(":decide"))
        .filter(|id| !id.is_empty() && !id.contains('/'))
        .map(DecisionsRoute::Decide)
}

fn decisions<E: SqlEngine>(
    host: &DevHost<E>,
    questions: Option<&dyn AdHocQuestions<E>>,
    actor: Option<&str>,
    route: DecisionsRoute<'_>,
    request: &Request<'_>,
) -> Result<(StatusCode, String), Refusal> {
    let now = norn_kernel::LogicalTime::from_millis(request.now.saturating_mul(1000));
    match (route, request.method) {
        (DecisionsRoute::Collection, "GET") => {
            let mut fields = parse(
                request.query,
                &["orgId", "environmentId", "state", "flow", "cursor", "limit"],
            )?;
            let limit = fields
                .remove("limit")
                .map(|limit| {
                    limit
                        .parse::<u32>()
                        .map_err(|error| Refusal::invalid(format!("limit: {error}")))
                })
                .transpose()?;
            let query = DecisionsQuery {
                org_id: fields.remove("orgId"),
                environment_id: fields.remove("environmentId"),
                state: fields.remove("state"),
                flow: fields.remove("flow"),
                cursor: fields.remove("cursor"),
                limit,
            };
            Ok((
                StatusCode::OK,
                document_json(&list_decisions(host, &query, actor, now)?)?,
            ))
        }
        (DecisionsRoute::Decide(id), "POST") => {
            let fields = parse(request.query, &["orgId", "environmentId"])?;
            let scope = ScopeQuery {
                org_id: fields.get("orgId").cloned(),
                environment_id: fields.get("environmentId").cloned(),
            };
            // No disclosure authority or no caller: the decision is absent.
            let (Some(runtime), Some(subject)) = (questions, actor) else {
                return Err(norn_api::decisions::not_found());
            };
            let id = percent_encoding::percent_decode_str(id)
                .decode_utf8()
                .map_err(|_invalid| norn_api::decisions::not_found())?;
            let body: DecideRequest = serde_json::from_str(request.body).map_err(|error| {
                Refusal::invalid(format!("the request body is not a decision: {error}"))
            })?;
            let reads = {
                let subject = subject.to_owned();
                let scope = host.scope().clone();
                move |capability: ReadCapability,
                      object_type: &str|
                      -> norn_kernel::authz::AccessRequest {
                    collection_access(&scope, &subject, capability, object_type)
                }
            };
            let call = DecideCall {
                scope: &scope,
                actor,
                id: &id,
                body: &body,
                now,
            };
            match decide(host, &call, &reads, runtime) {
                Ok(decided) => Ok((StatusCode::OK, document_json(&decided)?)),
                // A refusal carries who resolved the decision first, when
                // someone did.
                Err(refused) => Ok((
                    StatusCode::from_u16(refused.refusal.status)
                        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                    document_json(&*refused)?,
                )),
            }
        }
        (DecisionsRoute::Collection, _) => Err(not_allowed("decisions are listed with GET")),
        (DecisionsRoute::Decide(_), _) => Err(not_allowed("a decision is decided with POST")),
    }
}

fn not_allowed(hint: &str) -> Refusal {
    Refusal::new(
        StatusCode::METHOD_NOT_ALLOWED,
        "route.method_not_allowed",
        "the route does not accept this method",
        hint.to_owned(),
    )
}
