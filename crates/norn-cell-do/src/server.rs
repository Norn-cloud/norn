//! Read-only fetch dispatch over `norn_api`'s shared, authorized services.
//!
//! The transport carries method, path, query and the `Authorization` header
//! only. Scope comes from the explicit query and is admitted against the
//! physical cell; the principal comes from [`PrincipalResolver`]; authority is
//! the host's [`norn_api::authz::Authorizer`]. No SQL, schema or table name is
//! ever accepted or returned.

use std::collections::BTreeMap;

use http::StatusCode;
use norn_api::host::DevHost;
use norn_api::object_query::{MAX_CURSOR_BYTES, ObjectQueryParams};
use norn_api::object_routes::{get_object_query, get_queried_object};
use norn_api::question_routes::{
    AdHocQuestions, AdHocTraceRequest, EvaluateRequest, QuestionRuntime, TraceQuery,
    evaluate_question, get_answer, get_numeric_trace, trace_ad_hoc,
};
use norn_api::refusal::Refusal;
use norn_api::routes::{DefinitionsQuery, get_defs};
use norn_api::scope::ScopeQuery;
use norn_cell::engine::SqlEngine;

use norn_api::authz::{ReadCapability, ReadRequest};

use crate::policy::{CallerSlot, CellObjectAccess, DEMO_GRANTS, READ_POLICY_ID, collection_access};
use crate::principal::{PrincipalResolver, RequestScope};

/// One finite HTTP response: a status and a JSON body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    /// HTTP status.
    pub status: u16,
    /// UTF-8 JSON body: the route's document or a shared [`Refusal`].
    pub body: String,
}

/// Longest accepted query string, in bytes.
pub const MAX_QUERY_BYTES: usize = 2048;

/// Longest accepted grid query string, in bytes: a filter of eight clauses,
/// each `in` up to fifty values, percent-encoded.
pub const MAX_OBJECT_QUERY_BYTES: usize = 8192;

/// One finite HTTP request as the transport delivers it.
#[derive(Debug, Clone, Copy)]
pub struct Request<'a> {
    /// HTTP method.
    pub method: &'a str,
    /// Path, for example `/v1/definitions`.
    pub path: &'a str,
    /// Raw query string, without `?`.
    pub query: &'a str,
    /// The `Authorization` header, if any.
    pub authorization: Option<&'a str>,
    /// The host's clock (Unix seconds), used to check token lifetimes.
    pub now: u64,
    /// The request body (`POST` routes), empty when there is none.
    pub body: &'a str,
}

/// Dispatch one request against the opened cell.
pub fn handle<E: SqlEngine>(
    host: &DevHost<E>,
    principals: &dyn PrincipalResolver,
    request: &Request<'_>,
) -> Response {
    handle_with(host, principals, None, request)
}

/// Dispatch one request; `questions` serves the cell's admitted questions
/// and ad-hoc questions over its declared metrics (F2b).
pub fn handle_with<E: SqlEngine>(
    host: &DevHost<E>,
    principals: &dyn PrincipalResolver,
    questions: Option<&dyn AdHocQuestions<E>>,
    request: &Request<'_>,
) -> Response {
    let Request {
        method,
        path,
        query,
        authorization,
        now,
        body,
    } = *request;
    // The verified principal lives exactly as long as this request.
    let _request = RequestScope::new(principals);
    let actor = principals.resolve(authorization, now);
    let actor = actor.as_deref();
    let question = path
        .strip_prefix("/v1/questions/")
        .and_then(|rest| rest.split_once('/'))
        .filter(|(name, _)| !name.is_empty() && !name.contains('/'));
    let ad_hoc = matches!(path, "/v1/questions:evaluate" | "/v1/questions:trace");
    let object = path
        .strip_prefix("/v1/objects/")
        .map(|rest| {
            rest.split_once('/')
                .map_or((rest, None), |(kind, id)| (kind, Some(id)))
        })
        .filter(|(kind, id)| {
            !kind.is_empty() && id.is_none_or(|id| !id.is_empty() && !id.contains('/'))
        });
    let known = path == "/v1/definitions"
        || ad_hoc
        || object.is_some()
        || question.is_some_and(|(_, leaf)| leaf == "answer" || leaf == "trace");
    let result = if !known {
        Err(Refusal::new(
            StatusCode::NOT_FOUND,
            "route.not_found",
            "no such route",
            "the host serves only its declared read routes",
        ))
    } else if ad_hoc {
        ad_hoc_question(host, questions, actor, method, path, query, body)
    } else if method != "GET" {
        Err(Refusal::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "route.method_not_allowed",
            "the route does not accept this method",
            "this host's read routes take GET",
        ))
    } else {
        match (question, questions) {
            (None, _) if object.is_some() => object.map_or_else(
                || Err(norn_api::authz::read_denied()),
                |(kind, id)| objects(host, actor, kind, id, query),
            ),
            (None, _) => definitions_query(query)
                .and_then(|query| get_defs(host, &query, actor))
                .and_then(|document| document_json(&document)),
            (Some((name, "answer")), Some(runtime)) => parse(query, &["orgId", "environmentId"])
                .and_then(|fields| {
                    let scope = ScopeQuery {
                        org_id: fields.get("orgId").cloned(),
                        environment_id: fields.get("environmentId").cloned(),
                    };
                    get_answer(host, &scope, actor, name, named(runtime))
                })
                .and_then(|document| document_json(&document)),
            (Some((name, _)), Some(runtime)) => {
                parse(query, &["orgId", "environmentId", "lineageRef", "bucket"])
                    .and_then(|mut fields| {
                        let query = TraceQuery {
                            org_id: fields.remove("orgId"),
                            environment_id: fields.remove("environmentId"),
                            lineage_ref: fields.remove("lineageRef"),
                            bucket: fields.remove("bucket"),
                        };
                        get_numeric_trace(host, &query, actor, name, named(runtime))
                    })
                    .and_then(|document| document_json(&document))
            }
            (Some(_), None) => Err(Refusal::new(
                StatusCode::NOT_FOUND,
                "question.not_found",
                "no such admitted question",
                "this cell admits only its declared questions",
            )),
        }
    };
    match result {
        Ok(body) => Response {
            status: StatusCode::OK.as_u16(),
            body,
        },
        Err(refused) => Response {
            status: refused.status,
            body: serde_json::to_string(&refused).unwrap_or_else(|_| String::from("{}")),
        },
    }
}

/// F2b: `POST /v1/questions:evaluate` and `:trace`, scoped by the query
/// string and carrying the `QuestionIR` in a JSON body.
fn ad_hoc_question<E: SqlEngine>(
    host: &DevHost<E>,
    questions: Option<&dyn AdHocQuestions<E>>,
    actor: Option<&str>,
    method: &str,
    path: &str,
    query: &str,
    body: &str,
) -> Result<String, Refusal> {
    if method != "POST" {
        return Err(Refusal::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "route.method_not_allowed",
            "the route does not accept this method",
            "ad-hoc questions are submitted with POST",
        ));
    }
    let fields = parse(query, &["orgId", "environmentId"])?;
    let scope = ScopeQuery {
        org_id: fields.get("orgId").cloned(),
        environment_id: fields.get("environmentId").cloned(),
    };
    // No runtime, no actor: one denial, before the body is read.
    let (Some(runtime), Some(_)) = (questions, actor) else {
        return Err(norn_api::authz::read_denied());
    };
    let malformed = |error: serde_json::Error| {
        Refusal::invalid(format!(
            "the request body is not a valid submission: {error}"
        ))
    };
    if path == "/v1/questions:evaluate" {
        let request: EvaluateRequest = serde_json::from_str(body).map_err(malformed)?;
        document_json(&evaluate_question(host, &scope, actor, &request, runtime)?)
    } else {
        let request: AdHocTraceRequest = serde_json::from_str(body).map_err(malformed)?;
        document_json(&trace_ad_hoc(host, &scope, actor, &request, runtime)?)
    }
}

/// HOST-READ-3: `GET /v1/objects/{type}` (a grid query) and
/// `GET /v1/objects/{type}/{id}`, scoped by the query string.
fn objects<E: SqlEngine>(
    host: &DevHost<E>,
    actor: Option<&str>,
    object_type: &str,
    id: Option<&str>,
    query: &str,
) -> Result<String, Refusal> {
    // Path segments decode percent escapes only: `&`, `=` and `+` are literal
    // in a path, never query syntax. A segment that is not UTF-8 once decoded
    // names nothing: it refuses rather than alias another id.
    let decoded = |text: &str| {
        percent_encoding::percent_decode_str(text)
            .decode_utf8()
            .map(std::borrow::Cow::into_owned)
            .map_err(|_invalid| Refusal::invalid("an object path segment is not UTF-8"))
    };
    let object_type = decoded(object_type)?;
    if let Some(id) = id {
        let fields = parse(query, &["orgId", "environmentId"])?;
        let scope = ScopeQuery {
            org_id: fields.get("orgId").cloned(),
            environment_id: fields.get("environmentId").cloned(),
        };
        let record = get_queried_object(
            host,
            &scope,
            actor,
            &object_type,
            &decoded(id)?,
            &CellObjectAccess,
        )?;
        return document_json(&record);
    }
    // The cursor has its own bound, so any query the host accepts on a first
    // page stays acceptable with every cursor the host issues for it.
    let mut fields = parse_within(
        query,
        &["orgId", "environmentId", "limit", "sort", "filter", "after"],
        MAX_OBJECT_QUERY_BYTES.saturating_add(MAX_CURSOR_BYTES),
    )?;
    let cursor_bytes = fields.get("after").map_or(0, String::len);
    if query.len().saturating_sub(cursor_bytes) > MAX_OBJECT_QUERY_BYTES {
        return Err(Refusal::invalid("the query string exceeds the host bound"));
    }
    let scope = ScopeQuery {
        org_id: fields.remove("orgId"),
        environment_id: fields.remove("environmentId"),
    };
    let params = ObjectQueryParams {
        limit: fields.remove("limit"),
        sort: fields.remove("sort"),
        filter: fields.remove("filter"),
        after: fields.remove("after"),
    };
    document_json(&get_object_query(
        host,
        &scope,
        actor,
        &object_type,
        &params,
        &CellObjectAccess,
    )?)
}

/// The named-question view of the cell's question runtime.
fn named<E: SqlEngine>(runtime: &dyn AdHocQuestions<E>) -> &dyn QuestionRuntime<E> {
    runtime
}

fn document_json(document: &impl serde::Serialize) -> Result<String, Refusal> {
    serde_json::to_string(document).map_err(Refusal::host)
}

/// Exactly `orgId`, `environmentId` and optional `version`, each at most once.
fn definitions_query(query: &str) -> Result<DefinitionsQuery, Refusal> {
    let mut fields = parse(query, &["orgId", "environmentId", "version"])?;
    let version = fields
        .remove("version")
        .map(|version| {
            version
                .parse::<u64>()
                .map_err(|error| Refusal::invalid(format!("version: {error}")))
        })
        .transpose()?;
    Ok(DefinitionsQuery {
        org_id: fields.remove("orgId"),
        environment_id: fields.remove("environmentId"),
        version,
    })
}

/// A bounded query string with only `allowed` keys, each at most once.
fn parse(query: &str, allowed: &[&'static str]) -> Result<BTreeMap<&'static str, String>, Refusal> {
    parse_within(query, allowed, MAX_QUERY_BYTES)
}

fn parse_within(
    query: &str,
    allowed: &[&'static str],
    max: usize,
) -> Result<BTreeMap<&'static str, String>, Refusal> {
    if query.len() > max {
        return Err(Refusal::invalid("the query string exceeds the host bound"));
    }
    let mut fields = BTreeMap::new();
    for (key, value) in form_urlencoded::parse(query.as_bytes()) {
        let Some(name) = allowed.iter().copied().find(|name| *name == key) else {
            return Err(Refusal::invalid(format!("unknown query parameter {key}")));
        };
        if fields.insert(name, value.into_owned()).is_some() {
            return Err(Refusal::invalid(format!("repeated query parameter {key}")));
        }
    }
    Ok(fields)
}

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
    request: &Request<'_>,
) -> Response {
    match access_document(host, principals, caller, object_types, request) {
        Ok(body) => Response {
            status: StatusCode::OK.as_u16(),
            body,
        },
        Err(refused) => Response {
            status: refused.status,
            body: serde_json::to_string(&refused).unwrap_or_else(|_| String::from("{}")),
        },
    }
}

fn access_document<E: SqlEngine>(
    host: &DevHost<E>,
    principals: &dyn PrincipalResolver,
    caller: &CallerSlot,
    object_types: &[String],
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
    let fields = parse(request.query, &["orgId", "environmentId"])?;
    let scope = ScopeQuery {
        org_id: fields.get("orgId").cloned(),
        environment_id: fields.get("environmentId").cloned(),
    };
    // The verified principal lives exactly as long as this request.
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
    let decisions: Vec<(ReadCapability, Result<(), Refusal>)> = [
        ReadCapability::Population,
        ReadCapability::Values,
        ReadCapability::Sources,
    ]
    .into_iter()
    .map(|capability| (capability, decide(capability)))
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
    let capabilities = decisions
        .iter()
        .map(|(capability, decided)| {
            let name = match capability {
                ReadCapability::Population => "population",
                ReadCapability::Values => "values",
                ReadCapability::Sources => "sources",
            };
            let rule = DEMO_GRANTS
                .entries
                .iter()
                .find(|(role, granted)| {
                    caller.roles.iter().any(|held| held == role) && granted.contains(capability)
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
        .collect();
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
    })
}
