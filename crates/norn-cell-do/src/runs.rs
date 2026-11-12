//! RUN-ROUTES-1 and DECISIONS-1 on the DO host: start (`POST /v1/runs`) and
//! read (`GET /v1/runs/{id}`) runs through `norn_api::run_routes`, and list
//! (`GET /v1/decisions`) and decide (`POST /v1/decisions/{id}:decide`) the
//! decisions runs wait on through `norn_api::decisions`, scoped by the query
//! string. Starting is the host
//! operation `StartRun`, decided by the cell's policy; every run read is the
//! caller's ordinary Values read of the run's object type, mapped by
//! [`collection_access`]. A caller with no verified principal reads nothing.
//!
//! ACTION-STEP-1: a run's `norn.action@1` steps act under the verified
//! principal who started the run, for the whole run. The dispatch records
//! that principal from the resolver's caller slot when a run starts, and after
//! a start or a decision runs the run's pending action steps with the starter
//! made the slot's caller for each one call, restored on every path. A
//! resolver without a caller slot records no starter, and its runs' action
//! steps are denied.
//!
//! APPROVAL-1: approving a parked action call commits it through the same
//! dispatch with its requester made the slot's caller for that one call, the
//! same way.

use std::collections::BTreeMap;

use http::StatusCode;
use norn_api::actions::ActionAnswer;
use norn_api::authz::ReadCapability;
use norn_api::decisions::{DecideCall, DecideRequest, DecisionsQuery, decide, list_decisions};
use norn_api::host::DevHost;
use norn_api::question_routes::AdHocQuestions;
use norn_api::refusal::Refusal;
use norn_api::run_routes::{StartRunRequest, get_run, start_run};
use norn_api::scope::ScopeQuery;
use norn_cell::engine::SqlEngine;
use norn_cell::flow_run::RunStarter;

use crate::policy::{Caller, CallerSlot, collection_access};
use crate::server::{Request, Response, document_json, parse};

/// Serve `request` when its path is a run or decision route; `None` for
/// every other path. `questions` is the cell's receipt-disclosure authority
/// (D53): a cell without one decides nothing. `caller` is the slot the
/// cell's policy reads the verified caller from, when the host has one.
pub(crate) fn dispatch<E: SqlEngine>(
    host: &DevHost<E>,
    questions: Option<&dyn AdHocQuestions<E>>,
    caller: Option<&CallerSlot>,
    actor: Option<&str>,
    request: &Request<'_>,
) -> Option<Response> {
    let steps = Steps {
        host,
        questions,
        caller,
    };
    let result = if let Some(route) = runs_route(request.path) {
        runs(&steps, actor, route, request)
    } else {
        let route = decisions_route(request.path)?;
        decisions(&steps, actor, route, request)
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
    steps: &Steps<'_, E>,
    actor: Option<&str>,
    route: RunsRoute<'_>,
    request: &Request<'_>,
) -> Result<(StatusCode, String), Refusal> {
    let host = steps.host;
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
            let starter = steps.starter(subject);
            let (status, run) =
                start_run(host, &scope, actor, &body, &reads(subject), now, starter)?;
            let run = if status == StatusCode::CREATED && steps.drive(&run.id, now) {
                get_run(host, &scope, subject, &run.id, &reads(subject)).unwrap_or(run)
            } else {
                run
            };
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
    steps: &Steps<'_, E>,
    actor: Option<&str>,
    route: DecisionsRoute<'_>,
    request: &Request<'_>,
) -> Result<(StatusCode, String), Refusal> {
    let (host, questions) = (steps.host, steps.questions);
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
            let as_requester =
                |principal: &RunStarter,
                 call: &mut dyn FnMut() -> Result<ActionAnswer, Refusal>| {
                    steps.as_principal(principal, call)
                };
            match decide(host, &call, &reads, runtime, &as_requester) {
                Ok(mut decided) => {
                    // The decision may have moved the run onto action steps.
                    if steps.drive(&decided.run.id, now)
                        && let Ok(run) = get_run(host, &scope, subject, &decided.run.id, &reads)
                    {
                        decided.run.state = run.state;
                    }
                    Ok((StatusCode::OK, document_json(&decided)?))
                }
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

/// The host side of a run's action steps: the cell, its disclosure runtime,
/// and the slot its policy reads the verified caller from.
struct Steps<'a, E: SqlEngine> {
    host: &'a DevHost<E>,
    questions: Option<&'a dyn AdHocQuestions<E>>,
    caller: Option<&'a CallerSlot>,
}

impl<E: SqlEngine> Steps<'_, E> {
    /// The verified principal of this request, as a run's starter: exactly
    /// the caller the resolver recorded for `actor`, never rebuilt from
    /// anything the request carries.
    fn starter(&self, actor: &str) -> Option<RunStarter> {
        self.caller?
            .current()
            .filter(|caller| caller.subject_id == actor)
            .map(principal_of)
    }

    /// Run one call as a recorded principal (a run's starter, an approved
    /// call's requester): it is the slot's verified caller for exactly that
    /// call, and the request's own caller is restored on every path. `None`
    /// when this cell cannot act as it, or has no slot.
    fn as_principal(
        &self,
        principal: &RunStarter,
        call: &mut dyn FnMut() -> Result<ActionAnswer, Refusal>,
    ) -> Option<Result<ActionAnswer, Refusal>> {
        let slot = self.caller?;
        let current = slot.current();
        let acting = acting_caller(principal, current.as_ref())?;
        let _restore = Restore {
            slot,
            previous: current,
        };
        slot.record(Some(acting));
        Some(call())
    }

    /// Run `run_id`'s pending action steps under its recorded starter;
    /// `true` when it could try. A failure leaves the step pending for the
    /// next drive.
    fn drive(&self, run_id: &str, now: norn_kernel::LogicalTime) -> bool {
        let (Some(runtime), Some(_)) = (self.questions, self.caller) else {
            return false;
        };
        let as_starter =
            |principal: &RunStarter, call: &mut dyn FnMut() -> Result<ActionAnswer, Refusal>| {
                self.as_principal(principal, call)
            };
        let driven: Result<(), Refusal> =
            norn_api::flow_driver::drive_run(self.host, run_id, &as_starter, runtime, now);
        driven.is_ok()
    }
}

/// A verified caller as a recorded principal: exactly what the resolver
/// verified, never rebuilt from the request.
pub(crate) fn principal_of(caller: Caller) -> RunStarter {
    RunStarter {
        subject: caller.subject_id,
        actor_kind: caller.actor_kind.to_owned(),
        roles: caller.roles,
        role_table: caller.role_table.to_owned(),
    }
}

/// The recorded starter as this cell's verified caller: only an actor kind
/// this cell issues, and the role table of the request it runs in (the same
/// deployment's table); anything else cannot be acted as, and the step is
/// denied.
fn acting_caller(starter: &RunStarter, current: Option<&Caller>) -> Option<Caller> {
    let actor_kind = match starter.actor_kind.as_str() {
        "user" => "user",
        "service" => "service",
        _other => return None,
    };
    let role_table = current
        .map(|caller| caller.role_table)
        .filter(|table| *table == starter.role_table)?;
    Some(Caller {
        subject_id: starter.subject.clone(),
        roles: starter.roles.clone(),
        actor_kind,
        role_table,
    })
}

/// Puts the request's own verified caller back into the slot when dropped,
/// on every path, including unwinding.
struct Restore<'a> {
    slot: &'a CallerSlot,
    previous: Option<Caller>,
}

impl Drop for Restore<'_> {
    fn drop(&mut self) {
        self.slot.record(self.previous.take());
    }
}
