//! Fetch dispatch over `norn_api`'s shared, authorized services: the read
//! routes, F2b's ad-hoc questions, the retained-definition routes
//! (`/v1/definitions:admit`, `:install`, `:list`, `/v1/definitions/{id}` and
//! `/v1/definitions/{id}:retire`), and the one governed action route,
//! `POST /v1/actions/{actionId}`.
//!
//! The transport carries method, path, query, the `Authorization` header and
//! a bounded `POST` body only. Scope comes from the explicit query and is admitted against the
//! physical cell; the principal comes from [`PrincipalResolver`]; authority is
//! the host's [`norn_api::authz::Authorizer`]. No SQL, schema or table name is
//! ever accepted or returned.

use std::collections::BTreeMap;

use http::StatusCode;
use norn_api::actions::{ActionQuery, action_denied, call_action, decode_action_request};
use norn_api::authz::Operation;
use norn_api::definition_routes::{
    self, DefinitionKinds, GetDefinitionQuery, ListDefinitionsQuery,
};
use norn_api::host::DevHost;
use norn_api::object_links::{ObjectLinkParams, get_object_lineage, get_object_links};
use norn_api::object_query::{MAX_CURSOR_BYTES, ObjectQueryParams};
use norn_api::object_routes::{get_object_query, get_queried_object};
use norn_api::question_routes::{
    AdHocQuestions, AdHocTraceRequest, EvaluateRequest, QuestionRuntime, TraceQuery,
    evaluate_question, get_answer, get_numeric_trace, trace_ad_hoc,
};
use norn_api::refusal::Refusal;
use norn_api::routes::{DefinitionsQuery, get_defs};
use norn_api::scope::ScopeQuery;
use norn_api::transform_routes::{
    CompileTransformRequest, TransformRowsParams, compile_transform, get_transform_plan,
    get_transform_row_lineage, get_transform_rows,
};
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

/// Declares [`Route`], [`Route::ALL`] and [`Route::signature`] from one list,
/// so a route cannot exist without being in `ALL`.
macro_rules! routes {
    ($($(#[$doc:meta])* $name:ident => ($method:literal, $path:literal),)+) => {
        /// Every route this host serves, and nothing else: [`handle_all`]
        /// refuses a path that [`Route::of`] does not name. The generated
        /// `OpenAPI` document is checked against this list (`tests/server.rs`),
        /// so a served route is either documented or named there as
        /// undocumented, with the reason.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
        pub enum Route {
            $($(#[$doc])* $name,)+
        }

        impl Route {
            /// Every route, once, in declaration order.
            pub const ALL: &'static [Self] = &[$(Self::$name,)+];

            /// The method the route accepts and its path template, as the
            /// `OpenAPI` document writes them.
            #[must_use]
            pub const fn signature(self) -> (&'static str, &'static str) {
                match self {
                    $(Self::$name => ($method, $path),)+
                }
            }
        }
    };
}

routes! {
    /// `GET /v1/access`, dispatched by the published host's entry point.
    Access => ("GET", "/v1/access"),
    /// `POST /v1/cell:provision`, dispatched by the tenant before any cell
    /// route (HOST-PROVISION-1).
    Provision => ("POST", "/v1/cell:provision"),
    /// `POST /v1/actions/{actionId}`.
    Action => ("POST", "/v1/actions/{actionId}"),
    /// `GET /v1/definitions`.
    DefinitionsCatalog => ("GET", "/v1/definitions"),
    /// `POST /v1/definitions:admit`.
    DefinitionsAdmit => ("POST", "/v1/definitions:admit"),
    /// `POST /v1/definitions:install`.
    DefinitionsInstall => ("POST", "/v1/definitions:install"),
    /// `GET /v1/definitions:list`.
    DefinitionsList => ("GET", "/v1/definitions:list"),
    /// `GET /v1/definitions/{id}`.
    Definition => ("GET", "/v1/definitions/{id}"),
    /// `POST /v1/definitions/{id}:retire`.
    DefinitionRetire => ("POST", "/v1/definitions/{id}:retire"),
    /// `GET /v1/objects/{type}`.
    Objects => ("GET", "/v1/objects/{type}"),
    /// `GET /v1/objects/{type}/{id}`.
    Object => ("GET", "/v1/objects/{type}/{id}"),
    /// `GET /v1/objects/{type}/{id}/links`.
    ObjectLinks => ("GET", "/v1/objects/{type}/{id}/links"),
    /// `GET /v1/objects/{type}/{id}/lineage`.
    ObjectLineage => ("GET", "/v1/objects/{type}/{id}/lineage"),
    /// `GET /v1/questions/{question}/answer`.
    QuestionAnswer => ("GET", "/v1/questions/{question}/answer"),
    /// `GET /v1/questions/{question}/trace`.
    QuestionTrace => ("GET", "/v1/questions/{question}/trace"),
    /// `POST /v1/questions:evaluate`.
    QuestionsEvaluate => ("POST", "/v1/questions:evaluate"),
    /// `POST /v1/questions:trace`.
    QuestionsTrace => ("POST", "/v1/questions:trace"),
    /// `GET /v1/transforms/{transform}/rows`.
    TransformRows => ("GET", "/v1/transforms/{transform}/rows"),
    /// `GET /v1/transforms/{transform}/rows/{row}/lineage`.
    TransformRowLineage => ("GET", "/v1/transforms/{transform}/rows/{row}/lineage"),
    /// `GET /v1/transforms/{transform}/plan`.
    TransformPlan => ("GET", "/v1/transforms/{transform}/plan"),
    /// `POST /v1/transforms:compile`.
    TransformsCompile => ("POST", "/v1/transforms:compile"),
}

impl Route {
    /// The route a request path names, if any.
    #[must_use]
    pub fn of(path: &str) -> Option<Self> {
        match path {
            "/v1/access" => return Some(Self::Access),
            norn_api::provision::PROVISION_PATH => return Some(Self::Provision),
            "/v1/definitions" => return Some(Self::DefinitionsCatalog),
            "/v1/questions:evaluate" => return Some(Self::QuestionsEvaluate),
            "/v1/questions:trace" => return Some(Self::QuestionsTrace),
            "/v1/transforms:compile" => return Some(Self::TransformsCompile),
            _ => {}
        }
        if let Some(route) = definitions_route(path) {
            return Some(match route {
                DefinitionsRoute::Admit => Self::DefinitionsAdmit,
                DefinitionsRoute::Install => Self::DefinitionsInstall,
                DefinitionsRoute::List => Self::DefinitionsList,
                DefinitionsRoute::Retire(_) => Self::DefinitionRetire,
                DefinitionsRoute::Get(_) => Self::Definition,
            });
        }
        if let Some((_, leaf)) = question_path(path) {
            return match leaf {
                "answer" => Some(Self::QuestionAnswer),
                "trace" => Some(Self::QuestionTrace),
                _ => None,
            };
        }
        if let Some((_, id, leaf)) = path.strip_prefix("/v1/objects/").and_then(object_path) {
            return Some(match (id, leaf) {
                (None, _) => Self::Objects,
                (Some(_), None) => Self::Object,
                (Some(_), Some("links")) => Self::ObjectLinks,
                (Some(_), Some("lineage")) => Self::ObjectLineage,
                (Some(_), Some(_)) => return None,
            });
        }
        if let Some((_, leaf)) = transform_path(path) {
            return Some(match leaf {
                TransformLeaf::Rows => Self::TransformRows,
                TransformLeaf::Lineage(_) => Self::TransformRowLineage,
                TransformLeaf::Plan => Self::TransformPlan,
            });
        }
        path.strip_prefix("/v1/actions/")
            .filter(|id| !id.is_empty() && !id.contains('/'))
            .map(|_| Self::Action)
    }
}

/// What `/v1/transforms/{transform}/…` reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransformLeaf<'p> {
    Rows,
    Lineage(&'p str),
    Plan,
}

/// VIEW-SQL-1: `POST /v1/transforms:compile`, scoped by the query string and
/// carrying the query in a JSON body. Nothing is installed and no SQL runs.
fn compile<E: SqlEngine>(
    host: &DevHost<E>,
    actor: Option<&str>,
    method: &str,
    query: &str,
    body: &str,
) -> Result<String, Refusal> {
    if method != "POST" {
        return Err(Refusal::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "route.method_not_allowed",
            "the route does not accept this method",
            "transforms are compiled with POST",
        ));
    }
    let fields = parse(query, &["orgId", "environmentId"])?;
    let scope = ScopeQuery {
        org_id: fields.get("orgId").cloned(),
        environment_id: fields.get("environmentId").cloned(),
    };
    let request: CompileTransformRequest = serde_json::from_str(body)
        .map_err(|_invalid| Refusal::invalid("the body is not a transform compile request"))?;
    document_json(&compile_transform(host, &scope, actor, &request)?)
}

/// The transform id and leaf of a `/v1/transforms/...` path.
fn transform_path(path: &str) -> Option<(&str, TransformLeaf<'_>)> {
    let rest = path.strip_prefix("/v1/transforms/")?;
    let mut segments = rest.split('/');
    let transform = segments.next().filter(|id| !id.is_empty())?;
    let leaf = match (
        segments.next(),
        segments.next(),
        segments.next(),
        segments.next(),
    ) {
        (Some("rows"), None, None, None) => TransformLeaf::Rows,
        (Some("plan"), None, None, None) => TransformLeaf::Plan,
        (Some("rows"), Some(row), Some("lineage"), None) if !row.is_empty() => {
            TransformLeaf::Lineage(row)
        }
        _ => return None,
    };
    Some((transform, leaf))
}

/// `GET /v1/transforms/{transform}/rows`, `/rows/{row}/lineage` and `/plan`,
/// scoped by the query string (VIEW-READ-1).
fn transforms<E: SqlEngine>(
    host: &DevHost<E>,
    actor: Option<&str>,
    (transform, leaf): (&str, TransformLeaf<'_>),
    query: &str,
) -> Result<String, Refusal> {
    let decoded = |text: &str| {
        percent_encoding::percent_decode_str(text)
            .decode_utf8()
            .map(std::borrow::Cow::into_owned)
            .map_err(|_invalid| Refusal::invalid("a transform path segment is not UTF-8"))
    };
    let transform = decoded(transform)?;
    match leaf {
        TransformLeaf::Rows => {
            let mut fields = parse(query, &["orgId", "environmentId", "limit", "after"])?;
            let scope = ScopeQuery {
                org_id: fields.remove("orgId"),
                environment_id: fields.remove("environmentId"),
            };
            let params = TransformRowsParams {
                limit: fields.remove("limit"),
                after: fields.remove("after"),
            };
            document_json(&get_transform_rows(
                host,
                &scope,
                actor,
                &transform,
                &params,
                &CellObjectAccess,
            )?)
        }
        TransformLeaf::Lineage(row) => {
            let fields = parse(query, &["orgId", "environmentId"])?;
            let scope = ScopeQuery {
                org_id: fields.get("orgId").cloned(),
                environment_id: fields.get("environmentId").cloned(),
            };
            document_json(&get_transform_row_lineage(
                host,
                &scope,
                actor,
                &transform,
                &decoded(row)?,
                &CellObjectAccess,
            )?)
        }
        TransformLeaf::Plan => {
            let fields = parse(query, &["orgId", "environmentId"])?;
            let scope = ScopeQuery {
                org_id: fields.get("orgId").cloned(),
                environment_id: fields.get("environmentId").cloned(),
            };
            document_json(&get_transform_plan(
                host,
                &scope,
                actor,
                &transform,
                &CellObjectAccess,
            )?)
        }
    }
}

/// The question name and leaf of `/v1/questions/{question}/{leaf}`.
fn question_path(path: &str) -> Option<(&str, &str)> {
    path.strip_prefix("/v1/questions/")
        .and_then(|rest| rest.split_once('/'))
        .filter(|(name, _)| !name.is_empty() && !name.contains('/'))
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
    handle_all(
        host,
        principals,
        questions,
        &DefinitionKinds::default(),
        request,
    )
}

/// Dispatch one request; `kinds` validates the retained definitions this
/// cell admits (dashboards always; agents once a checker is registered).
pub fn handle_all<E: SqlEngine>(
    host: &DevHost<E>,
    principals: &dyn PrincipalResolver,
    questions: Option<&dyn AdHocQuestions<E>>,
    kinds: &DefinitionKinds,
    request: &Request<'_>,
) -> Response {
    // The verified principal lives exactly as long as this request.
    let _request = RequestScope::new(principals);
    let actor = principals.resolve(request.authorization, request.now);
    respond(route(
        Some(host),
        questions,
        kinds,
        actor.as_deref(),
        request,
    ))
}

/// What a caller nobody verified gets from a scope that has no cell: exactly
/// what [`handle_all`] answers the same request on a provisioned cell that
/// publishes no questions, so a refusal never tells a provisioned scope from
/// an unprovisioned one. The same routing code decides both; only the cell
/// is absent.
#[must_use]
pub fn outside<E: SqlEngine>(request: &Request<'_>) -> Response {
    respond(route::<E>(
        None,
        None,
        &DefinitionKinds::default(),
        None,
        request,
    ))
}

pub(crate) fn respond(result: Result<String, Refusal>) -> Response {
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

/// Route one request for `actor`. `host` is `None` only for a scope with no
/// cell, and then `actor` is `None`: every service is reached through
/// [`cell`], which answers the denial the service itself gives nobody.
fn route<E: SqlEngine>(
    host: Option<&DevHost<E>>,
    questions: Option<&dyn AdHocQuestions<E>>,
    kinds: &DefinitionKinds,
    actor: Option<&str>,
    request: &Request<'_>,
) -> Result<String, Refusal> {
    let Request {
        method,
        path,
        query,
        now,
        body,
        ..
    } = *request;
    let question = question_path(path);
    let ad_hoc = matches!(path, "/v1/questions:evaluate" | "/v1/questions:trace");
    let object = path.strip_prefix("/v1/objects/").and_then(object_path);
    let action = path
        .strip_prefix("/v1/actions/")
        .filter(|id| !id.is_empty() && !id.contains('/'));
    // `Route` is the one list of what this host serves; a path it does not
    // name is not found. `/v1/access` is dispatched by the published host's
    // entry point, never here.
    let route_of = Route::of(path);
    let served = route_of.is_some_and(|route| route != Route::Access);
    if !served {
        Err(Refusal::new(
            StatusCode::NOT_FOUND,
            "route.not_found",
            "no such route",
            "the host serves only its declared read routes",
        ))
    } else if route_of == Some(Route::Provision) {
        provisioned(host, actor, request)
    } else if let Some(route) = definitions_route(path) {
        definitions(host, questions, kinds, actor, route, request)
    } else if let Some(action) = action {
        governed_action(host, questions, actor, (method, action, query, body), now)
    } else if ad_hoc {
        ad_hoc_question(host, questions, actor, method, path, query, body)
    } else if path == "/v1/transforms:compile" {
        compile(cell(host)?, actor, method, query, body)
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
                |(kind, id, leaf)| objects(host, actor, (kind, id, leaf), query),
            ),
            (None, _) if transform_path(path).is_some() => transform_path(path).map_or_else(
                || Err(norn_api::authz::read_denied()),
                |target| transforms(cell(host)?, actor, target, query),
            ),
            (None, _) => definitions_query(query)
                .and_then(|query| get_defs(cell(host)?, &query, actor))
                .and_then(|document| document_json(&document)),
            (Some((name, "answer")), Some(runtime)) => parse(query, &["orgId", "environmentId"])
                .and_then(|fields| {
                    let scope = ScopeQuery {
                        org_id: fields.get("orgId").cloned(),
                        environment_id: fields.get("environmentId").cloned(),
                    };
                    get_answer(cell(host)?, &scope, actor, name, named(runtime))
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
                        get_numeric_trace(cell(host)?, &query, actor, name, named(runtime))
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
    }
}

/// The cell a read service runs on. A scope with no cell answers the one
/// opaque read denial, as every read service answers a caller nobody
/// verified.
fn cell<E: SqlEngine>(host: Option<&DevHost<E>>) -> Result<&DevHost<E>, Refusal> {
    host.ok_or_else(norn_api::authz::read_denied)
}

/// `POST /v1/cell:provision` on a cell this dispatch already serves: a
/// tenant host provisions a vacant scope before any cell route
/// ([`crate::tenant::Tenant`]); a cell served here exists, so a provisioner
/// is told so, and anyone else gets the opaque denial.
fn provisioned<E: SqlEngine>(
    host: Option<&DevHost<E>>,
    actor: Option<&str>,
    request: &Request<'_>,
) -> Result<String, Refusal> {
    if request.method != "POST" {
        return Err(Refusal::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "route.method_not_allowed",
            "the route does not accept this method",
            "a cell is provisioned with POST",
        ));
    }
    let mut fields = parse(request.query, &["orgId", "environmentId"])?;
    let scope = ScopeQuery {
        org_id: fields.remove("orgId"),
        environment_id: fields.remove("environmentId"),
    };
    definition_routes::authorize(cell(host)?, &scope, actor, Operation::ProvisionCell)?;
    Err(norn_api::provision::already_provisioned())
}

/// F2b: `POST /v1/questions:evaluate` and `:trace`, scoped by the query
/// string and carrying the `QuestionIR` in a JSON body.
fn ad_hoc_question<E: SqlEngine>(
    host: Option<&DevHost<E>>,
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
    let host = cell(host)?;
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

/// HOST-ACTION-1: `POST /v1/actions/{actionId}`, scoped by the query string
/// (`orgId`, `environmentId`, optional `version` pin) with the call in a JSON
/// body. The shared service authorizes, admits and commits; the cell's
/// question runtime is its receipt-disclosure authority (D53), so a cell
/// without one serves no action.
fn governed_action<E: SqlEngine>(
    host: Option<&DevHost<E>>,
    questions: Option<&dyn AdHocQuestions<E>>,
    actor: Option<&str>,
    (method, action, query, body): (&str, &str, &str, &str),
    now: u64,
) -> Result<String, Refusal> {
    if method != "POST" {
        return Err(Refusal::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "route.method_not_allowed",
            "the route does not accept this method",
            "an action is called with POST",
        ));
    }
    let mut fields = parse(query, &["orgId", "environmentId", "version"])?;
    let version = fields
        .remove("version")
        .map(|version| {
            version
                .parse::<u64>()
                .map_err(|error| Refusal::invalid(format!("version: {error}")))
        })
        .transpose()?;
    let query = ActionQuery {
        org_id: fields.remove("orgId"),
        environment_id: fields.remove("environmentId"),
        version,
    };
    // No runtime, no actor: one denial, before the body is read.
    let (Some(runtime), Some(_)) = (questions, actor) else {
        return Err(action_denied());
    };
    let host = host.ok_or_else(action_denied)?;
    let action = percent_encoding::percent_decode_str(action)
        .decode_utf8()
        .map_err(|_invalid| Refusal::invalid("the action path segment is not UTF-8"))?;
    let request = decode_action_request(body)?;
    let now = norn_kernel::LogicalTime::from_millis(now.saturating_mul(1000));
    document_json(&call_action(
        host,
        &query,
        actor,
        &action,
        &request,
        named(runtime),
        now,
    )?)
}

/// The segments of an object route after `/v1/objects/`: a type, an optional
/// id, and for an id an optional `links` or `lineage` leaf. No segment is empty.
fn object_path(rest: &str) -> Option<(&str, Option<&str>, Option<&str>)> {
    let mut segments = rest.split('/');
    let kind = segments.next().filter(|kind| !kind.is_empty())?;
    let id = segments.next();
    let leaf = segments.next();
    if segments.next().is_some()
        || id.is_some_and(str::is_empty)
        || leaf.is_some_and(|leaf| !matches!(leaf, "links" | "lineage"))
    {
        return None;
    }
    Some((kind, id, leaf))
}

/// HOST-READ-3/4: `GET /v1/objects/{type}` (a grid query),
/// `GET /v1/objects/{type}/{id}` and its `/links` and `/lineage`, scoped by
/// the query string.
fn objects<E: SqlEngine>(
    host: Option<&DevHost<E>>,
    actor: Option<&str>,
    (object_type, id, leaf): (&str, Option<&str>, Option<&str>),
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
    if let (Some(id), Some("links")) = (id, leaf) {
        let mut fields = parse(
            query,
            &[
                "orgId",
                "environmentId",
                "direction",
                "link",
                "limit",
                "after",
            ],
        )?;
        let scope = ScopeQuery {
            org_id: fields.remove("orgId"),
            environment_id: fields.remove("environmentId"),
        };
        let params = ObjectLinkParams {
            direction: fields.remove("direction"),
            link: fields.remove("link"),
            limit: fields.remove("limit"),
            after: fields.remove("after"),
        };
        let id = decoded(id)?;
        return document_json(&get_object_links(
            cell(host)?,
            &scope,
            actor,
            &object_type,
            &id,
            &params,
            &CellObjectAccess,
        )?);
    }
    if let (Some(id), Some(_lineage)) = (id, leaf) {
        let fields = parse(query, &["orgId", "environmentId"])?;
        let scope = ScopeQuery {
            org_id: fields.get("orgId").cloned(),
            environment_id: fields.get("environmentId").cloned(),
        };
        let id = decoded(id)?;
        return document_json(&get_object_lineage(
            cell(host)?,
            &scope,
            actor,
            &object_type,
            &id,
            &CellObjectAccess,
        )?);
    }
    if let Some(id) = id {
        let fields = parse(query, &["orgId", "environmentId"])?;
        let scope = ScopeQuery {
            org_id: fields.get("orgId").cloned(),
            environment_id: fields.get("environmentId").cloned(),
        };
        let id = decoded(id)?;
        let record = get_queried_object(
            cell(host)?,
            &scope,
            actor,
            &object_type,
            &id,
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
        cell(host)?,
        &scope,
        actor,
        &object_type,
        &params,
        &CellObjectAccess,
    )?)
}

/// A retained-definition route; `GET /v1/definitions` (the catalog) is not one.
#[derive(Debug, Clone, Copy)]
enum DefinitionsRoute<'p> {
    Admit,
    Install,
    List,
    Retire(&'p str),
    Get(&'p str),
}

fn definitions_route(path: &str) -> Option<DefinitionsRoute<'_>> {
    match path {
        "/v1/definitions:admit" => Some(DefinitionsRoute::Admit),
        "/v1/definitions:install" => Some(DefinitionsRoute::Install),
        "/v1/definitions:list" => Some(DefinitionsRoute::List),
        _ => {
            let rest = path.strip_prefix("/v1/definitions/")?;
            if rest.is_empty() || rest.contains('/') {
                return None;
            }
            Some(
                rest.strip_suffix(":retire")
                    .map_or(DefinitionsRoute::Get(rest), DefinitionsRoute::Retire),
            )
        }
    }
}

/// The retained-definition routes: each is authorized, with its explicit
/// scope, before its body or other query values are interpreted.
fn definitions<E: SqlEngine>(
    host: Option<&DevHost<E>>,
    questions: Option<&dyn AdHocQuestions<E>>,
    kinds: &DefinitionKinds,
    actor: Option<&str>,
    route: DefinitionsRoute<'_>,
    request: &Request<'_>,
) -> Result<String, Refusal> {
    let write = !matches!(route, DefinitionsRoute::List | DefinitionsRoute::Get(_));
    let expected = if write { "POST" } else { "GET" };
    if request.method != expected {
        return Err(Refusal::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "route.method_not_allowed",
            "the route does not accept this method",
            format!("this definitions route takes {expected}"),
        ));
    }
    let allowed: &[&'static str] = match route {
        DefinitionsRoute::List => &[
            "orgId",
            "environmentId",
            "kind",
            "include",
            "limit",
            "cursor",
            "uses",
        ],
        DefinitionsRoute::Get(_) => &["orgId", "environmentId", "version"],
        DefinitionsRoute::Admit | DefinitionsRoute::Install | DefinitionsRoute::Retire(_) => {
            &["orgId", "environmentId"]
        }
    };
    let mut fields = parse(request.query, allowed)?;
    let scope = ScopeQuery {
        org_id: fields.remove("orgId"),
        environment_id: fields.remove("environmentId"),
    };
    let operation = if write {
        Operation::WriteDefinitions
    } else {
        Operation::ReadDefinitions
    };
    let host = cell(host)?;
    definition_routes::authorize(host, &scope, actor, operation)?;
    let now_ms = request.now.saturating_mul(1000);
    let number = |name: &str, value: Option<String>| {
        value
            .map(|value| {
                value
                    .parse::<u64>()
                    .map_err(|error| Refusal::invalid(format!("{name}: {error}")))
            })
            .transpose()
    };
    match route {
        DefinitionsRoute::Admit => document_json(&definition_routes::admit_definition(
            host,
            &scope,
            actor,
            kinds,
            questions,
            &submission(request.body)?,
        )?),
        DefinitionsRoute::Install => document_json(&definition_routes::install_definition(
            host,
            &scope,
            actor,
            kinds,
            questions,
            &submission(request.body)?,
            now_ms,
        )?),
        DefinitionsRoute::Retire(id) => document_json(&definition_routes::retire_definition(
            host,
            &scope,
            actor,
            questions,
            id,
            &submission(request.body)?,
            now_ms,
        )?),
        DefinitionsRoute::List => {
            let limit = number("limit", fields.remove("limit"))?
                .map(|limit| u32::try_from(limit).unwrap_or(u32::MAX));
            let query = ListDefinitionsQuery {
                org_id: scope.org_id,
                environment_id: scope.environment_id,
                kind: fields.remove("kind"),
                include: fields.remove("include"),
                limit,
                cursor: fields.remove("cursor"),
                uses: fields.remove("uses"),
            };
            document_json(&definition_routes::list_definitions(host, &query, actor)?)
        }
        DefinitionsRoute::Get(id) => {
            let query = GetDefinitionQuery {
                org_id: scope.org_id,
                environment_id: scope.environment_id,
                version: number("version", fields.remove("version"))?,
            };
            document_json(&definition_routes::get_definition(
                host, &query, actor, kinds, questions, id,
            )?)
        }
    }
}

fn submission<T: serde::de::DeserializeOwned>(body: &str) -> Result<T, Refusal> {
    serde_json::from_str(body).map_err(|error| {
        Refusal::invalid(format!(
            "the request body is not a valid submission: {error}"
        ))
    })
}

/// The named-question view of the cell's question runtime.
fn named<E: SqlEngine>(runtime: &dyn AdHocQuestions<E>) -> &dyn QuestionRuntime<E> {
    runtime
}

pub(crate) fn document_json(document: &impl serde::Serialize) -> Result<String, Refusal> {
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
pub(crate) fn parse(
    query: &str,
    allowed: &[&'static str],
) -> Result<BTreeMap<&'static str, String>, Refusal> {
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
    respond(access_document(
        Some(host),
        principals,
        caller,
        object_types,
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
        request,
    ))
}

fn access_document<E: SqlEngine>(
    host: Option<&DevHost<E>>,
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
    let host = cell(host)?;
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
