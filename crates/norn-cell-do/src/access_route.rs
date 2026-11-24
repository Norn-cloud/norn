//! `GET /v1/access`: the request's verified principal and per-role PDP
//! decisions over the cell's collections. Extracted from `server.rs` to keep
//! each dispatch module compact.

use std::collections::BTreeMap;

use http::StatusCode;
use norn_api::authz::{ReadCapability, ReadRequest};
use norn_api::host::DevHost;
use norn_api::refusal::Refusal;
use norn_api::scope::ScopeQuery;
use norn_cell::engine::SqlEngine;

use crate::policy::{CallerSlot, DEMO_GRANTS, READ_POLICY_ID, collection_access};
use crate::principal::{PrincipalResolver, RequestScope};
use crate::server::{Request, Response, document_json, parse, respond};

/// The principal and the access decisions this cell makes for it.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccessDocument {
    /// The verified principal.
    pub principal: AccessPrincipal,
    /// The compiled `AccessIR` policy the PDP decides under.
    pub policy: &'static str,
    /// The declared grant table its roles were mapped through.
    pub grant_table: &'static str,
    /// One decision per read capability.
    pub capabilities: BTreeMap<&'static str, AccessDecision>,
    /// With `objectType`: the collection asked about.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub object_type: Option<String>,
    /// With `objectType`: what each role the table declares may do with that
    /// collection, each decided by the PDP for that role alone.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub roles: Option<Vec<RoleAccess>>,
}

/// What one declared role alone may read of a collection and which of its
/// actions it may call (`GET /v1/access?objectType=`).
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoleAccess {
    /// The declared role.
    pub role: &'static str,
    /// What every decision here is over: always [`COLLECTION_SCOPE`], the
    /// collection asked about, never one record of it.
    pub scope: &'static str,
    /// Whether the role sees the collection's records, as Studio shows it.
    pub see: SeeDecision,
    /// One decision per read capability.
    pub read: BTreeMap<&'static str, AccessDecision>,
    /// Each callable action on the collection, by identity.
    pub actions: Vec<crate::actions::RoleAction>,
}

/// The scope of every per-role answer of `GET /v1/access?objectType=`: the
/// whole collection. A role permitted its values still reads a marked fact
/// as the cell projects it for that reader (masked or withheld by its
/// markings), decided at each read of each record.
pub const COLLECTION_SCOPE: &str = "collection";

/// Whether one role alone sees a collection's records: the read PDP's
/// Population and Values decisions for it, read together by the cell.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SeeDecision {
    /// `permit`: the role reads the collection's values; `partial`: only
    /// which records exist (Population without Values); `deny`: neither.
    pub decision: &'static str,
    /// Always [`COLLECTION_SCOPE`]: a `permit` is the collection's, and a
    /// marked fact is still masked or withheld per record by its markings.
    pub scope: &'static str,
}

/// What `GET /v1/access?objectType=` asks: the read PDP and the action policy
/// installed for the cell's active definitions.
pub struct RoleExplainer<'a> {
    /// The read PDP, asked one role at a time.
    pub reads: &'a crate::policy::RoleReads,
    /// The action policy, when the definitions install one.
    pub actions: Option<&'a crate::actions::ActionPolicy>,
}

/// The verified principal, as the resolver recorded it.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccessPrincipal {
    /// Norn subject id.
    pub subject_id: String,
    /// `user` or `service`.
    pub actor_kind: &'static str,
    /// Norn roles.
    pub roles: Vec<String>,
    /// The declared role table the roles came from.
    pub role_table: &'static str,
}

/// One capability's decision over every collection of the cell.
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccessDecision {
    /// `permit` only when the PDP permits the capability on every collection.
    pub decision: &'static str,
    /// The grant-table role that grants the capability, when one does.
    pub rule: Option<String>,
}

/// `GET /v1/access`: the request's verified principal and, per read
/// capability, the cell's actual PDP decision on each of its collections
/// (`object_types`). An anonymous, out-of-scope or wholly denied caller gets
/// the shared read denial.
pub fn access<E: SqlEngine>(
    host: &DevHost<E>,
    principals: &dyn PrincipalResolver,
    caller: &CallerSlot,
    object_types: &[String],
    explainer: &RoleExplainer<'_>,
    request: &Request<'_>,
) -> Response {
    respond(access_document(
        Some(host),
        principals,
        caller,
        object_types,
        Some(explainer),
        request,
    ))
}

/// `GET /v1/access` on a scope that has no cell, for a caller nobody
/// verified: what [`access`] answers it on a provisioned cell.
#[must_use]
pub fn access_outside<E: SqlEngine>(
    principals: &dyn PrincipalResolver,
    request: &Request<'_>,
) -> Response {
    respond(access_document::<E>(
        None,
        principals,
        &CallerSlot::default(),
        &[],
        None,
        request,
    ))
}

const CAPABILITIES: [(ReadCapability, &str); 3] = [
    (ReadCapability::Population, "population"),
    (ReadCapability::Values, "values"),
    (ReadCapability::Sources, "sources"),
];

fn caller_decisions(
    roles: &[String],
    decisions: &[(ReadCapability, Result<(), Refusal>)],
) -> BTreeMap<&'static str, AccessDecision> {
    decisions
        .iter()
        .zip(CAPABILITIES)
        .map(|((capability, decided), (_, name))| {
            let rule = DEMO_GRANTS
                .entries
                .iter()
                .find(|(role, granted)| {
                    roles.iter().any(|held| held == role) && granted.contains(capability)
                })
                .map(|(role, _)| format!("{}:{role}", DEMO_GRANTS.version));
            let decision = if decided.is_ok() { "permit" } else { "deny" };
            (
                name,
                AccessDecision {
                    decision,
                    rule: decided.is_ok().then_some(rule).flatten(),
                },
            )
        })
        .collect()
}

fn role_access(explainer: &RoleExplainer<'_>, object_type: &str) -> Vec<RoleAccess> {
    explainer
        .reads
        .roles()
        .into_iter()
        .map(|role| {
            let permits =
                |capability: ReadCapability| explainer.reads.permits(role, capability, object_type);
            let see = match (
                permits(ReadCapability::Population),
                permits(ReadCapability::Values),
            ) {
                (true, true) => "permit",
                (true, false) => "partial",
                (false, _) => "deny",
            };
            RoleAccess {
                role,
                scope: COLLECTION_SCOPE,
                see: SeeDecision {
                    decision: see,
                    scope: COLLECTION_SCOPE,
                },
                read: CAPABILITIES
                    .iter()
                    .map(|(capability, name)| {
                        let permitted = permits(*capability);
                        (
                            *name,
                            AccessDecision {
                                decision: if permitted { "permit" } else { "deny" },
                                rule: permitted
                                    .then(|| format!("{}:{role}", explainer.reads.table())),
                            },
                        )
                    })
                    .collect(),
                actions: explainer
                    .actions
                    .map_or_default(|policy| policy.role_decisions(role, object_type)),
            }
        })
        .collect()
}

fn access_document<E: SqlEngine>(
    host: Option<&DevHost<E>>,
    principals: &dyn PrincipalResolver,
    caller: &CallerSlot,
    object_types: &[String],
    explainer: Option<&RoleExplainer<'_>>,
    request: &Request<'_>,
) -> Result<String, Refusal> {
    if request.method != "GET" {
        return Err(Refusal::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "route.method_not_allowed",
            "the route does not accept this method",
            "this host is read-only; use GET",
        ));
    }
    let fields = parse(request.query, &["orgId", "environmentId", "objectType"])?;
    let scope = ScopeQuery {
        org_id: fields.get("orgId").cloned(),
        environment_id: fields.get("environmentId").cloned(),
    };
    let host = host.ok_or_else(norn_api::authz::read_denied)?;
    let asked = fields.get("objectType").cloned();
    let object_types = match &asked {
        Some(object_type) if !object_types.contains(object_type) => {
            return Err(norn_api::authz::read_denied());
        }
        Some(object_type) => std::slice::from_ref(object_type),
        None => object_types,
    };
    let _request = RequestScope::new(principals);
    let subject = principals.resolve(request.authorization, request.now);
    let decide = |capability| {
        let subject = subject.as_deref().unwrap_or_default();
        let mut first_denial = None;
        for object_type in object_types {
            let access = collection_access(host.scope(), subject, capability, object_type);
            if let Err(denied) = host.read_access(
                &scope,
                &ReadRequest {
                    capability,
                    access: &access,
                },
            ) {
                first_denial.get_or_insert(denied);
            }
        }
        first_denial.map_or(Ok(()), Err)
    };
    let decisions: Vec<(ReadCapability, Result<(), Refusal>)> = CAPABILITIES
        .iter()
        .map(|(capability, _)| (*capability, decide(*capability)))
        .collect();
    let caller = caller
        .current()
        .filter(|caller| Some(&caller.subject_id) == subject.as_ref());
    let (Some(caller), true) = (caller, decisions.iter().any(|(_, decided)| decided.is_ok()))
    else {
        return Err(decisions
            .into_iter()
            .find_map(|(_, decided)| decided.err())
            .unwrap_or_else(|| Refusal::invalid("the cell publishes no collection")));
    };
    let capabilities = caller_decisions(&caller.roles, &decisions);
    let roles = match asked.as_deref() {
        None => None,
        Some(object_type) => {
            let Some(explainer) = explainer.filter(|_| {
                decisions.iter().all(|(capability, decided)| {
                    *capability != ReadCapability::Population || decided.is_ok()
                })
            }) else {
                return Err(norn_api::authz::read_denied());
            };
            Some(role_access(explainer, object_type))
        }
    };
    document_json(&AccessDocument {
        principal: AccessPrincipal {
            subject_id: caller.subject_id,
            actor_kind: caller.actor_kind,
            roles: caller.roles,
            role_table: caller.role_table,
        },
        policy: READ_POLICY_ID,
        grant_table: DEMO_GRANTS.version,
        capabilities,
        object_type: asked,
        roles,
    })
}
