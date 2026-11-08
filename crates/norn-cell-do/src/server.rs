//! Read-only fetch dispatch over `norn_api`'s shared, authorized services.
//!
//! The transport carries method, path, query and the `Authorization` header
//! only. Scope comes from the explicit query and is admitted against the
//! physical cell; the principal comes from [`PrincipalResolver`]; authority is
//! the host's [`norn_api::authz::Authorizer`]. No SQL, schema or table name is
//! ever accepted or returned.

use http::StatusCode;
use norn_api::host::DevHost;
use norn_api::refusal::Refusal;
use norn_api::routes::{DefinitionsQuery, get_defs};
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
    let actor = principals.resolve(authorization);
    let result = match path {
        "/v1/definitions" if method == "GET" => definitions_query(query)
            .and_then(|query| get_defs(host, &query, actor.as_deref()))
            .and_then(|document| document_json(&document)),
        "/v1/definitions" => Err(Refusal::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "route.method_not_allowed",
            "the route does not accept this method",
            "this host is read-only; use GET",
        )),
        _ => Err(Refusal::new(
            StatusCode::NOT_FOUND,
            "route.not_found",
            "no such route",
            "the host serves only its declared read routes",
        )),
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
    if query.len() > MAX_QUERY_BYTES {
        return Err(Refusal::invalid("the query string exceeds the host bound"));
    }
    let mut parsed = DefinitionsQuery::default();
    for (key, value) in form_urlencoded::parse(query.as_bytes()) {
        let duplicate = match key.as_ref() {
            "orgId" => parsed.org_id.replace(value.into_owned()).is_some(),
            "environmentId" => parsed.environment_id.replace(value.into_owned()).is_some(),
            "version" => {
                let version = value
                    .parse::<u64>()
                    .map_err(|error| Refusal::invalid(format!("version: {error}")))?;
                parsed.version.replace(version).is_some()
            }
            other => return Err(Refusal::invalid(format!("unknown query parameter {other}"))),
        };
        if duplicate {
            return Err(Refusal::invalid(format!("repeated query parameter {key}")));
        }
    }
    Ok(parsed)
}
