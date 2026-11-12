//! Fetch dispatch over `norn_api`'s shared, authorized services: the read
//! routes, F2b's ad-hoc questions, the retained-definition routes
//! (`/v1/definitions:admit`, `:install`, `:list`, `/v1/definitions/{id}` and
//! `/v1/definitions/{id}:retire`), the one governed action route,
//! `POST /v1/actions/{actionId}`, the run routes (`POST /v1/runs`,
//! `GET /v1/runs/{id}`), the decision routes (`GET /v1/decisions`,
//! `POST /v1/decisions/{id}:decide`); and `POST /v1/mcp`, whose tools call
//! these same routes ([`crate::mcp`]).
//!
//! The transport carries method, path, query, the `Authorization` header and
//! a bounded `POST` body only. Scope comes from the explicit query and is admitted against the
//! physical cell; the principal comes from [`PrincipalResolver`]; authority is
//! the host's [`norn_api::authz::Authorizer`]. No SQL, schema or table name is
//! ever accepted or returned.

use std::collections::BTreeMap;

use http::StatusCode;
use norn_api::action_catalog::{ActionCatalogQuery, list_actions};
use norn_api::actions::{ActionQuery, action_denied, call_action, decode_action_request};
use norn_api::authz::Operation;
use norn_api::definition_routes::{
    self, DefinitionKinds, GetDefinitionQuery, ListDefinitionsQuery,
};
use norn_api::host::DevHost;
use norn_api::object_links::{ObjectLinkParams, get_object_lineage, get_object_links};
use norn_api::object_profile::get_object_profile;
use norn_api::object_query::{MAX_CURSOR_BYTES, ObjectQueryParams};
use norn_api::object_review::{
    get_object_findings, get_object_history, get_object_lineage_at, get_quarantined_objects,
};
use norn_api::object_routes::{get_object_query, get_queried_object};
use norn_api::question_routes::{
    AdHocQuestions, AdHocTraceRequest, EvaluateRequest, QuestionRuntime, TraceQuery,
    evaluate_question, get_answer, get_numeric_trace, list_metrics, question_catalog, trace_ad_hoc,
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
    /// `GET /v1/actions`.
    ActionCatalog => ("GET", "/v1/actions"),
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
    /// `GET /v1/objects/{type}:profile`.
    ObjectProfile => ("GET", "/v1/objects/{type}:profile"),
    /// `GET /v1/objects/{type}/{id}`.
    Object => ("GET", "/v1/objects/{type}/{id}"),
    /// `GET /v1/objects/{type}/{id}/links`.
    ObjectLinks => ("GET", "/v1/objects/{type}/{id}/links"),
    /// `GET /v1/objects/{type}/{id}/lineage`.
    ObjectLineage => ("GET", "/v1/objects/{type}/{id}/lineage"),
    /// `GET /v1/objects/{type}/{id}/findings`.
    ObjectFindings => ("GET", "/v1/objects/{type}/{id}/findings"),
    /// `GET /v1/objects/{type}/{id}/history`.
    ObjectHistory => ("GET", "/v1/objects/{type}/{id}/history"),
    /// `GET /v1/objects/{type}:quarantined`.
    ObjectsQuarantined => ("GET", "/v1/objects/{type}:quarantined"),
    /// `GET /v1/questions/{question}/answer`.
    QuestionAnswer => ("GET", "/v1/questions/{question}/answer"),
    /// `GET /v1/questions/{question}/trace`.
    QuestionTrace => ("GET", "/v1/questions/{question}/trace"),
    /// `POST /v1/questions:evaluate`.
    QuestionsEvaluate => ("POST", "/v1/questions:evaluate"),
    /// `POST /v1/questions:trace`.
    QuestionsTrace => ("POST", "/v1/questions:trace"),
    /// `POST /v1/mcp`: MCP, whose tools call these same routes ([`crate::mcp`]).
    Mcp => ("POST", "/v1/mcp"),
    /// `GET /v1/questions:metrics`.
    QuestionsMetrics => ("GET", "/v1/questions:metrics"),
    /// `POST /v1/runs` (RUN-ROUTES-1).
    RunStart => ("POST", "/v1/runs"),
    /// `GET /v1/runs/{id}` (RUN-ROUTES-1).
    Run => ("GET", "/v1/runs/{id}"),
    /// `GET /v1/questions:catalog`.
    QuestionsCatalog => ("GET", "/v1/questions:catalog"),
    /// `GET /v1/decisions` (DECISIONS-1).
    Decisions => ("GET", "/v1/decisions"),
    /// `POST /v1/decisions/{id}:decide` (DECISIONS-1).
    Decide => ("POST", "/v1/decisions/{id}:decide"),
}

impl Route {
    /// The route a request path names, if any.
    #[must_use]
    pub fn of(path: &str) -> Option<Self> {
        match path {
            "/v1/access" => return Some(Self::Access),
            "/v1/actions" => return Some(Self::ActionCatalog),
            norn_api::provision::PROVISION_PATH => return Some(Self::Provision),
            "/v1/definitions" => return Some(Self::DefinitionsCatalog),
            "/v1/questions:evaluate" => return Some(Self::QuestionsEvaluate),
            "/v1/questions:trace" => return Some(Self::QuestionsTrace),
            crate::mcp::MCP_PATH => return Some(Self::Mcp),
            "/v1/questions:metrics" => return Some(Self::QuestionsMetrics),
            "/v1/runs" => return Some(Self::RunStart),
            "/v1/questions:catalog" => return Some(Self::QuestionsCatalog),
            "/v1/decisions" => return Some(Self::Decisions),
            _ => {}
        }
        if path
            .strip_prefix("/v1/decisions/")
            .and_then(|rest| rest.strip_suffix(":decide"))
            .is_some_and(|id| !id.is_empty() && !id.contains('/'))
        {
            return Some(Self::Decide);
        }
        if path
            .strip_prefix("/v1/runs/")
            .is_some_and(|id| !id.is_empty() && !id.contains('/'))
        {
            return Some(Self::Run);
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
        if let Some((kind, id, leaf)) = path.strip_prefix("/v1/objects/").and_then(object_path) {
            return Some(match (id, leaf) {
                (None, _) if kind.ends_with(":profile") => Self::ObjectProfile,
                (None, _) if kind.ends_with(":quarantined") => Self::ObjectsQuarantined,
                (None, _) => Self::Objects,
                (Some(_), None) => Self::Object,
                (Some(_), Some("links")) => Self::ObjectLinks,
                (Some(_), Some("lineage")) => Self::ObjectLineage,
                (Some(_), Some("findings")) => Self::ObjectFindings,
                (Some(_), Some("history")) => Self::ObjectHistory,
                (Some(_), Some(_)) => return None,
            });
        }
        path.strip_prefix("/v1/actions/")
            .filter(|id| !id.is_empty() && !id.contains('/'))
            .map(|_| Self::Action)
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
    // MCP resolves its caller per tool call, through these same routes.
    if request.path == crate::mcp::MCP_PATH {
        return crate::mcp::serve(host, principals, questions, kinds, request);
    }
    // The verified principal lives exactly as long as this request.
    let _request = RequestScope::new(principals);
    let actor = principals.resolve(request.authorization, request.now);
    if let Some(response) = crate::runs::dispatch(host, questions, actor.as_deref(), request) {
        return response;
    }
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
    let ad_hoc = matches!(
        path,
        "/v1/questions:evaluate" | "/v1/questions:trace" | "/v1/questions:metrics"
    );
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
    } else if path == "/v1/actions" {
        action_catalog(host.ok_or_else(action_denied)?, actor, method, query)
    } else if let Some(action) = action {
        governed_action(host, questions, actor, (method, action, query, body), now)
    } else if ad_hoc {
        ad_hoc_question(host, questions, actor, method, path, query, body)
    } else if path == "/v1/questions:catalog" {
        ad_hoc_catalog(cell(host)?, questions, actor, method, query)
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
    let listing = path == "/v1/questions:metrics";
    if method != if listing { "GET" } else { "POST" } {
        return Err(Refusal::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "route.method_not_allowed",
            "the route does not accept this method",
            "ad-hoc questions are submitted with POST; their metrics are listed with GET",
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
    if listing {
        return document_json(&list_metrics(host, &scope, actor, runtime)?);
    }
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

/// `GET /v1/questions:catalog`, scoped by the query string: what this caller
/// may build an ad-hoc question from. A cell without a question runtime
/// answers the one denial.
fn ad_hoc_catalog<E: SqlEngine>(
    host: &DevHost<E>,
    questions: Option<&dyn AdHocQuestions<E>>,
    actor: Option<&str>,
    method: &str,
    query: &str,
) -> Result<String, Refusal> {
    if method != "GET" {
        return Err(Refusal::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "route.method_not_allowed",
            "the route does not accept this method",
            "the question catalog is read with GET",
        ));
    }
    let fields = parse(query, &["orgId", "environmentId"])?;
    let scope = ScopeQuery {
        org_id: fields.get("orgId").cloned(),
        environment_id: fields.get("environmentId").cloned(),
    };
    let Some(runtime) = questions else {
        return Err(norn_api::authz::read_denied());
    };
    document_json(&question_catalog(host, &scope, actor, runtime)?)
}

/// HOST-ACTION-1: `POST /v1/actions/{actionId}`, scoped by the query string
/// (`orgId`, `environmentId`, optional `version` pin) with the call in a JSON
/// body. The shared service authorizes, admits and commits; the cell's
/// question runtime is its receipt-disclosure authority (D53), so a cell
/// without one serves no action.
/// `GET /v1/actions`: the actions the caller may call, by the call's own gate.
fn action_catalog<E: SqlEngine>(
    host: &DevHost<E>,
    actor: Option<&str>,
    method: &str,
    query: &str,
) -> Result<String, Refusal> {
    if method != "GET" {
        return Err(Refusal::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "route.method_not_allowed",
            "the route does not accept this method",
            "the action catalog is read with GET",
        ));
    }
    let mut fields = parse(query, &["orgId", "environmentId", "objectKind"])?;
    let query = ActionCatalogQuery {
        org_id: fields.remove("orgId"),
        environment_id: fields.remove("environmentId"),
        object_kind: fields.remove("objectKind"),
    };
    document_json(&list_actions(host, &query, actor)?)
}

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
/// id, and for an id an optional `links`, `lineage`, `findings` or `history`
/// leaf. No segment is empty.
fn object_path(rest: &str) -> Option<(&str, Option<&str>, Option<&str>)> {
    let mut segments = rest.split('/');
    let kind = segments.next().filter(|kind| !kind.is_empty())?;
    let id = segments.next();
    let leaf = segments.next();
    if segments.next().is_some()
        || id.is_some_and(str::is_empty)
        || leaf.is_some_and(|leaf| !matches!(leaf, "links" | "lineage" | "findings" | "history"))
    {
        return None;
    }
    Some((kind, id, leaf))
}

/// HOST-READ-5: `GET /v1/objects/{type}:profile`, scoped by the query string.
fn profiled<E: SqlEngine>(
    host: &DevHost<E>,
    actor: Option<&str>,
    object_type: &str,
    query: &str,
) -> Result<String, Refusal> {
    let fields = parse(query, &["orgId", "environmentId"])?;
    let scope = ScopeQuery {
        org_id: fields.get("orgId").cloned(),
        environment_id: fields.get("environmentId").cloned(),
    };
    document_json(&get_object_profile(
        host,
        &scope,
        actor,
        object_type,
        &CellObjectAccess,
    )?)
}

/// HOST-READ-6: `GET /v1/objects/{type}:quarantined`, scoped by the query string.
fn quarantined<E: SqlEngine>(
    host: Option<&DevHost<E>>,
    actor: Option<&str>,
    object_type: &str,
    query: &str,
) -> Result<String, Refusal> {
    let fields = parse(query, &["orgId", "environmentId"])?;
    let scope = ScopeQuery {
        org_id: fields.get("orgId").cloned(),
        environment_id: fields.get("environmentId").cloned(),
    };
    document_json(&get_quarantined_objects(
        cell(host)?,
        &scope,
        actor,
        object_type,
        &CellObjectAccess,
    )?)
}

/// HOST-READ-4/6: the review reads, when the path names one: a type's
/// `:quarantined` ids, or an object's `/lineage` (optionally `?version=`),
/// `/findings` and `/history`.
fn review<E: SqlEngine>(
    host: Option<&DevHost<E>>,
    actor: Option<&str>,
    (object_type, id, leaf): (&str, Option<&str>, Option<&str>),
    query: &str,
) -> Option<Result<String, Refusal>> {
    match (object_type.strip_suffix(":quarantined"), id, leaf) {
        (Some(kind), None, _) => Some(quarantined(host, actor, kind, query)),
        (None, Some(id), Some(leaf @ ("findings" | "history" | "lineage"))) => Some(
            percent_encoding::percent_decode_str(id)
                .decode_utf8()
                .map_err(|_invalid| Refusal::invalid("an object path segment is not UTF-8"))
                .and_then(|id| reviewed(host, actor, (object_type, &id, leaf), query)),
        ),
        _ => None,
    }
}

/// One object's review read, scoped by the query string.
fn reviewed<E: SqlEngine>(
    host: Option<&DevHost<E>>,
    actor: Option<&str>,
    (object_type, id, leaf): (&str, &str, &str),
    query: &str,
) -> Result<String, Refusal> {
    let allowed: &[&str] = if leaf == "lineage" {
        &["orgId", "environmentId", "version"]
    } else {
        &["orgId", "environmentId"]
    };
    let fields = parse(query, allowed)?;
    let scope = ScopeQuery {
        org_id: fields.get("orgId").cloned(),
        environment_id: fields.get("environmentId").cloned(),
    };
    match (leaf, fields.get("version")) {
        ("findings", _) => document_json(&get_object_findings(
            cell(host)?,
            &scope,
            actor,
            object_type,
            id,
            &CellObjectAccess,
        )?),
        ("history", _) => document_json(&get_object_history(
            cell(host)?,
            &scope,
            actor,
            object_type,
            id,
            &CellObjectAccess,
        )?),
        (_, Some(version)) => {
            let version = version
                .parse::<u64>()
                .map_err(|_invalid| Refusal::invalid("version is a positive whole number"))?;
            document_json(&get_object_lineage_at(
                cell(host)?,
                &scope,
                actor,
                (object_type, id, version),
                &CellObjectAccess,
            )?)
        }
        (_, None) => document_json(&get_object_lineage(
            cell(host)?,
            &scope,
            actor,
            object_type,
            id,
            &CellObjectAccess,
        )?),
    }
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
    if let (None, Some(kind)) = (id, object_type.strip_suffix(":profile")) {
        return profiled(cell(host)?, actor, kind, query);
    }
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
    if let Some(reviewed) = review(host, actor, (&object_type, id, leaf), query) {
        return reviewed;
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
