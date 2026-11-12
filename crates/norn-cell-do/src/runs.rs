//! RUN-ROUTES-1 on the DO host: start (`POST /v1/runs`), list
//! (`GET /v1/runs`) and read (`GET /v1/runs/{id}`) runs through
//! `norn_api::run_routes`, scoped by the query string. Starting is the host
//! operation `StartRun`, decided by the cell's policy; every run read is the
//! caller's ordinary Values read of the run's object type, mapped by
//! [`collection_access`]. A caller with no verified principal reads nothing.

use std::collections::BTreeMap;

use http::StatusCode;
use norn_api::authz::ReadCapability;
use norn_api::host::DevHost;
use norn_api::refusal::Refusal;
use norn_api::run_routes::{RunsQuery, StartRunRequest, get_run, list_runs, start_run};
use norn_api::scope::ScopeQuery;
use norn_cell::engine::SqlEngine;

use crate::policy::collection_access;
use crate::server::{Request, Response, document_json, parse};

/// Serve `request` when its path is a run route; `None` for every other path.
pub(crate) fn dispatch<E: SqlEngine>(
    host: &DevHost<E>,
    actor: Option<&str>,
    request: &Request<'_>,
) -> Option<Response> {
    let route = runs_route(request.path)?;
    Some(match runs(host, actor, route, request) {
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

/// A run route: `/v1/runs` (start or list) or `/v1/runs/{id}`.
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
    let not_allowed = |hint: &str| {
        Refusal::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "route.method_not_allowed",
            "the route does not accept this method",
            hint.to_owned(),
        )
    };
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
        (RunsRoute::Collection, "GET") => {
            let mut fields = parse(
                request.query,
                &["orgId", "environmentId", "flow", "state", "cursor", "limit"],
            )?;
            let Some(subject) = actor else {
                return Err(norn_api::authz::read_denied());
            };
            let limit = fields
                .remove("limit")
                .map(|limit| {
                    limit
                        .parse::<u32>()
                        .map_err(|error| Refusal::invalid(format!("limit: {error}")))
                })
                .transpose()?;
            let query = RunsQuery {
                org_id: fields.remove("orgId"),
                environment_id: fields.remove("environmentId"),
                flow: fields.remove("flow"),
                state: fields.remove("state"),
                cursor: fields.remove("cursor"),
                limit,
            };
            Ok((
                StatusCode::OK,
                document_json(&list_runs(host, &query, &reads(subject))?)?,
            ))
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
                document_json(&get_run(host, &scope, &id, &reads(subject))?)?,
            ))
        }
        (RunsRoute::Collection, _) => Err(not_allowed(
            "runs are started with POST and listed with GET",
        )),
        (RunsRoute::One(_), _) => Err(not_allowed("a run is read with GET")),
    }
}
