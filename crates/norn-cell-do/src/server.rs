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
use norn_api::question_routes::{QuestionRuntime, TraceQuery, get_answer, get_numeric_trace};
use norn_api::refusal::Refusal;
use norn_api::routes::{DefinitionsQuery, get_defs};
use norn_api::scope::ScopeQuery;
use norn_cell::engine::SqlEngine;

use crate::principal::PrincipalResolver;

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

/// Dispatch one request against the opened cell.
pub fn handle<E: SqlEngine>(
    host: &DevHost<E>,
    principals: &dyn PrincipalResolver,
    method: &str,
    path: &str,
    query: &str,
    authorization: Option<&str>,
) -> Response {
    handle_with(host, principals, None, method, path, query, authorization)
}

/// Dispatch one request; `questions` serves the cell's admitted questions.
pub fn handle_with<E: SqlEngine>(
    host: &DevHost<E>,
    principals: &dyn PrincipalResolver,
    questions: Option<&dyn QuestionRuntime<E>>,
    method: &str,
    path: &str,
    query: &str,
    authorization: Option<&str>,
) -> Response {
    let actor = principals.resolve(authorization);
    let actor = actor.as_deref();
    let question = path
        .strip_prefix("/v1/questions/")
        .and_then(|rest| rest.split_once('/'))
        .filter(|(name, _)| !name.is_empty() && !name.contains('/'));
    let known = path == "/v1/definitions"
        || question.is_some_and(|(_, leaf)| leaf == "answer" || leaf == "trace");
    let result = if !known {
        Err(Refusal::new(
            StatusCode::NOT_FOUND,
            "route.not_found",
            "no such route",
            "the host serves only its declared read routes",
        ))
    } else if method != "GET" {
        Err(Refusal::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "route.method_not_allowed",
            "the route does not accept this method",
            "this host is read-only; use GET",
        ))
    } else {
        match (question, questions) {
            (None, _) => definitions_query(query)
                .and_then(|query| get_defs(host, &query, actor))
                .and_then(|document| document_json(&document)),
            (Some((name, "answer")), Some(runtime)) => parse(query, &["orgId", "environmentId"])
                .and_then(|fields| {
                    let scope = ScopeQuery {
                        org_id: fields.get("orgId").cloned(),
                        environment_id: fields.get("environmentId").cloned(),
                    };
                    get_answer(host, &scope, actor, name, runtime)
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
                        get_numeric_trace(host, &query, actor, name, runtime)
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
    if query.len() > MAX_QUERY_BYTES {
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
