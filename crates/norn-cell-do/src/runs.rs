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
//! APPROVAL-1, APPROVAL-RECOVERY-1, APPROVAL-LIVE-1: an approved call (a
//! fresh decide, its replay, the requester's own retry, or the drive of a
//! step waiting on it) runs only under the requester's current authority,
//! never the roles recorded when the call parked. A host that looks
//! requesters up ([`PrincipalResolver::authority`]) runs every such attempt
//! under the organization's answer for exactly that approval, the
//! requester's own credential included; a host without one runs it only on
//! the requester's own verified credential. A decide is validated first and
//! asks for the answer only for an approval that would run a call: the host
//! looks the requester up and serves the decide again, and nothing is
//! recorded until it has one (`approval.authority_unavailable`, 503,
//! retryable). A drive that reaches an action after an approval asks for
//! the answer for that step and the host resumes only the drive, so nothing
//! the request wrote is replayed. A requester whose signed answer is "no
//! member", or whose current roles do not grant the call, gets it refused
//! for good (`approval.requester_unauthorized`); anything unverified runs
//! nothing and records nothing final.

use std::collections::BTreeMap;

use http::StatusCode;
use norn_api::actions::ActionAnswer;
use norn_api::authz::ReadCapability;
use norn_api::decisions::{DecideCall, DecideRequest, DecisionsQuery, decide, list_decisions};
use norn_api::host::DevHost;
use norn_api::question_routes::AdHocQuestions;
use norn_api::refusal::{Refusal, RefusalCategory};
use norn_api::run_routes::{StartRunRequest, get_run, start_run};
use norn_api::scope::ScopeQuery;
use norn_cell::engine::SqlEngine;
use norn_cell::flow_run::RunStarter;

use crate::policy::{Caller, CallerSlot, collection_access};
use crate::principal::{Authority, PrincipalResolver, Resume};
use crate::server::{Request, Response, document_json, parse};

/// Serve `request` when its path is a run or decision route; `None` for
/// every other path. `questions` is the cell's receipt-disclosure authority
/// (D53): a cell without one decides nothing. `principals` verified the
/// request's caller into the slot the cell's policy reads, when it has one,
/// and answers an approved call's requester's current authority.
pub(crate) fn dispatch<E: SqlEngine>(
    host: &DevHost<E>,
    questions: Option<&dyn AdHocQuestions<E>>,
    principals: &dyn PrincipalResolver,
    actor: Option<&str>,
    request: &Request<'_>,
) -> Option<Response> {
    let steps = Steps {
        host,
        questions,
        caller: principals.caller(),
        principals,
        now: request.now,
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
        Err(refused) => crate::incident::respond(refused),
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
            // A start, or its exact retry (APPROVAL-RECOVERY-1: the starter's
            // authenticated retry runs a step's approved call), drives the run.
            let run = if (status == StatusCode::CREATED || status == StatusCode::OK)
                && steps.drive(&run.id, now, Drive::Started)
            {
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
            // A cell with no disclosure authority decides nothing, so it lists
            // nothing either: the list shows exactly what :decide accepts.
            if questions.is_none() {
                return Err(norn_api::authz::read_denied());
            }
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
                 binding: &str,
                 call: &mut dyn FnMut() -> Result<ActionAnswer, Refusal>| {
                    steps.as_present(principal, binding, call, None)
                };
            // R1-06: asked only once the decide is valid and would run the
            // call; nothing is recorded until the host has the answer.
            let ready = |requester: &RunStarter, binding: &str| steps.ready(requester, binding);
            match decide(host, &call, &reads, runtime, &as_requester, &ready) {
                Ok(mut decided) => {
                    // APPROVAL-REVISION-1: an approved call refused because its
                    // action or gate changed drives nothing: the step waiting on
                    // it never runs under the changed definition.
                    let stale = decided.call.as_ref().is_some_and(|call| {
                        call.code.as_deref()
                            == Some(norn_api::approvals::approval_codes::DEFINITION_CHANGED)
                    });
                    // The decision may have moved the run onto action steps.
                    if !stale
                        && steps.drive(&decided.run.id, now, Drive::Live)
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
    /// Answers a requester's current authority (APPROVAL-LIVE-1).
    principals: &'a dyn PrincipalResolver,
    /// The request's clock (Unix seconds).
    now: u64,
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
            previous: Some(slot.snapshot()),
        };
        slot.record(Some(acting));
        Some(call())
    }

    /// Whether the request's own verified caller is `requester` (the same
    /// subject, actor kind and role table), as that caller is verified now.
    fn is_requester(&self, requester: &RunStarter) -> bool {
        self.caller
            .and_then(CallerSlot::current)
            .is_some_and(|current| {
                current.subject_id == requester.subject
                    && current.actor_kind == requester.actor_kind
                    && current.role_table == requester.role_table
            })
    }

    /// APPROVAL-LIVE-1 (R1-06): the decide's check before it records or
    /// replays an approval into running its call: the host must have the
    /// organization's answer for exactly that approval. Without one it is
    /// asked for (the host serves the decide again with it) and nothing is
    /// recorded; an answer that did not verify records nothing either. A
    /// host without a lookup admits.
    fn ready(&self, requester: &RunStarter, binding: &str) -> Result<(), Refusal> {
        match self.principals.authority(requester, binding, self.now) {
            Authority::Pending => {
                self.principals
                    .ask_authority(requester, binding, Resume::Request, self.now);
                Err(norn_api::approvals::authority_unavailable())
            }
            Authority::Unavailable => Err(norn_api::approvals::authority_unavailable()),
            Authority::Current(_) | Authority::Revoked | Authority::Unknown => Ok(()),
        }
    }

    /// Run one attempt that can produce an effect under `principal`'s
    /// current authority only (APPROVAL-RECOVERY-1, APPROVAL-LIVE-1), for the
    /// one attempt `binding` names:
    /// - a host that looks principals up runs it only under the
    ///   organization's answer for exactly that attempt, whoever's request
    ///   it is (the principal's own included): a member now runs it as the
    ///   slot's caller with its current roles, restored on every path, and a
    ///   denial of that authority is `approval.requester_unauthorized`; a
    ///   signed "no member" is `approval.requester_unauthorized` and runs
    ///   nothing; no answer yet asks for one (resuming the drive of `drive`,
    ///   when the attempt is a drive's) and, like an answer that did not
    ///   verify, runs nothing (`approval.authority_unavailable`);
    /// - a host without a lookup runs it only when the request's own
    ///   verified caller is the principal, as verified now.
    ///
    /// `None` when neither holds (or there is no slot): nothing runs.
    fn as_present(
        &self,
        principal: &RunStarter,
        binding: &str,
        call: &mut dyn FnMut() -> Result<ActionAnswer, Refusal>,
        drive: Option<&str>,
    ) -> Option<Result<ActionAnswer, Refusal>> {
        let slot = self.caller?;
        match self.principals.authority(principal, binding, self.now) {
            // No lookup: the request's own credential, as #1483.
            Authority::Unknown => self.is_requester(principal).then(call),
            Authority::Current(member)
                if member.subject_id == principal.subject
                    && member.actor_kind == principal.actor_kind
                    && member.role_table == principal.role_table =>
            {
                let _restore = Restore {
                    slot,
                    previous: Some(slot.snapshot()),
                };
                slot.record_live(member, binding);
                Some(call().map_err(|refusal| {
                    if matches!(
                        refusal.category,
                        RefusalCategory::Permission | RefusalCategory::Visibility
                    ) {
                        norn_api::approvals::requester_unauthorized()
                    } else {
                        refusal
                    }
                }))
            }
            // Another role table than the one the call parked under: no
            // verified statement about that principal, so nothing final.
            Authority::Current(_) | Authority::Unavailable => {
                Some(Err(norn_api::approvals::authority_unavailable()))
            }
            Authority::Revoked => Some(Err(norn_api::approvals::requester_unauthorized())),
            Authority::Pending => {
                if let Some(run) = drive {
                    self.principals.ask_authority(
                        principal,
                        binding,
                        Resume::Drive(run.to_owned()),
                        self.now,
                    );
                }
                Some(Err(norn_api::approvals::authority_unavailable()))
            }
        }
    }

    /// Run `run_id`'s pending action steps; `true` when it could try. A
    /// failure leaves the step pending for the next drive. A step waiting on
    /// an approved call runs under its requester's current authority
    /// ([`Self::as_present`]). The run's other steps run under its recorded
    /// starter for a drive the starter's own start makes
    /// ([`Drive::Started`]); a drive an approval moved ([`Drive::Live`])
    /// runs them, too, under the starter's current authority when the host
    /// looks principals up (R1-03), and under the recorded starter
    /// otherwise.
    fn drive(&self, run_id: &str, now: norn_kernel::LogicalTime, mode: Drive) -> bool {
        let (Some(runtime), Some(_)) = (self.questions, self.caller) else {
            return false;
        };
        let as_starter = |principal: &RunStarter,
                          binding: &str,
                          call: &mut dyn FnMut() -> Result<ActionAnswer, Refusal>| {
            match mode {
                Drive::Started => self.as_principal(principal, call),
                Drive::Live => self
                    .as_present(principal, binding, &mut *call, Some(run_id))
                    .or_else(|| {
                        (self.principals.authority(principal, binding, self.now)
                            == Authority::Unknown)
                            .then(|| self.as_principal(principal, &mut *call))
                            .flatten()
                    }),
            }
        };
        let as_requester = |principal: &RunStarter,
                            binding: &str,
                            call: &mut dyn FnMut() -> Result<ActionAnswer, Refusal>| {
            self.as_present(principal, binding, call, Some(run_id))
        };
        let driven: Result<(), Refusal> = norn_api::flow_driver::drive_run(
            self.host,
            run_id,
            &as_starter,
            &as_requester,
            runtime,
            now,
        );
        driven.is_ok()
    }
}

/// Whose authority a drive runs a run's ordinary action steps under
/// (APPROVAL-LIVE-1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Drive {
    /// The drive of the starter's own start: its recorded starter.
    Started,
    /// A drive an approval moved, or one resumed with the organization's
    /// answer: the starter's current authority where the host looks it up.
    Live,
}

/// APPROVAL-LIVE-1 (R1-07): resume the drive of `run_id` that asked the
/// organization's authority for a step's principal, now that the host has
/// the answer: only the drive runs again, so nothing the request that asked
/// wrote is replayed. It runs with no request caller, every step under the
/// answered (current) authority.
pub(crate) fn resume_drive<E: SqlEngine>(
    host: &DevHost<E>,
    questions: Option<&dyn AdHocQuestions<E>>,
    principals: &dyn PrincipalResolver,
    run_id: &str,
    now: u64,
) {
    let steps = Steps {
        host,
        questions,
        caller: principals.caller(),
        principals,
        now,
    };
    let at = norn_kernel::LogicalTime::from_millis(now.saturating_mul(1000));
    // A drive that cannot run now leaves its step pending for the next.
    let _driven = steps.drive(run_id, at, Drive::Live);
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
    previous: Option<crate::policy::Snapshot>,
}

impl Drop for Restore<'_> {
    fn drop(&mut self) {
        if let Some(previous) = self.previous.take() {
            self.slot.put_back(previous);
        }
    }
}
