//! `norn dev`: loopback-only HTTP/1.1 local development cell over `LocalCell`.
//!
//! Refuses non-loopback bind addresses fail-closed, bounds request head and body
//! sizes (`MAX_BODY_BYTES = 2 MiB`), enforces a whole-request deadline, requires
//! a per-server local session secret (`NORN_DEV_TOKEN` or cryptographically
//! random ephemeral token) on every HTTP request, rejects duplicate or
//! conflicting authority/scope headers fail-closed (`400`), and dispatches
//! directly to the real `norn-api` services.

use std::collections::BTreeMap;
use std::io::{BufReader, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use clap::Args;
use norn_api::actions::{ActionAnswer, ActionQuery};
use norn_api::cell_definitions::{APPLY_PATH, PLAN_PATH};
use norn_api::local_cell::LocalCell;
use norn_api::object_query::ObjectQueryParams;
use norn_api::refusal::Refusal;
use norn_api::routes::DefinitionsQuery;
use norn_api::scope::ScopeQuery;
use serde_json::{Map, Value};

use crate::authoring::CommandOutput;

use super::boot_local_cell;
use super::dev_auth::{
    DEV_TOKEN_ENV, DevAuth, authenticate_dev_session, extract_actor, single_header,
    validate_request_headers,
};
use super::http_wire::{
    HttpRequest, HttpResponse, disclosed_ok, encoded_json_response, method_not_allowed,
    parse_query, percent_decode_path, read_http_request, refusal_response, synthetic_refusal,
    write_http_response,
};
use super::pack::PackInput;

const IO_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_THREADS: usize = 64;

/// Arguments for `norn dev`.
#[derive(Debug, Args)]
pub(crate) struct DevArgs {
    /// Authoring source (`.ts`/`.py` file or directory) to compile and install on startup.
    #[arg(long, conflicts_with = "artifacts")]
    pub(crate) source: Option<PathBuf>,
    /// Compiled JSON artifact file or directory to install on startup.
    #[arg(long, conflicts_with = "source")]
    pub(crate) artifacts: Option<PathBuf>,
    /// Base pack(s) (JSON or `TypeScript`/Python) installed before the app pack.
    #[arg(long = "base")]
    pub(crate) base: Vec<PathBuf>,
    /// Explicit organization scope (`scope.orgId`).
    #[arg(long)]
    pub(crate) org: String,
    /// Explicit environment scope (`scope.environmentId`).
    #[arg(long)]
    pub(crate) env: String,
    /// Optional `SQLite` database file path (in-memory when omitted).
    #[arg(long)]
    pub(crate) db: Option<PathBuf>,
    /// Loopback bind address (`127.0.0.1:<port>` or `[::1]:<port>`).
    #[arg(long, default_value = "127.0.0.1:4040")]
    pub(crate) bind: String,
}

/// Parse `--bind` and enforce loopback-only binding fail-closed.
pub(super) fn parse_loopback_bind(bind: &str) -> Result<SocketAddr, String> {
    let addr: SocketAddr = bind
        .parse()
        .map_err(|err| format!("invalid --bind address `{bind}`: {err}"))?;
    if !addr.ip().is_loopback() {
        return Err(format!(
            "refusing non-loopback bind `{bind}`: norn dev only listens on loopback addresses (127.0.0.1 or [::1])"
        ));
    }
    Ok(addr)
}

/// Execute `norn dev`: verify loopback bind, generate/load session token, boot
/// the local cell, and serve HTTP/1.1.
pub(crate) fn execute_dev(args: &DevArgs) -> CommandOutput {
    let addr = match parse_loopback_bind(&args.bind) {
        Ok(addr) => addr,
        Err(err) => return CommandOutput::usage_or_io_error(&err),
    };
    let auth = match DevAuth::from_env_or_generate() {
        Ok(auth) => auth,
        Err(err) => return CommandOutput::usage_or_io_error(&err),
    };
    let app_pack = if let Some(source) = &args.source {
        Some(PackInput::Source(source.clone()))
    } else {
        args.artifacts
            .as_ref()
            .map(|artifacts| PackInput::Artifacts(artifacts.clone()))
    };
    let cell = match boot_local_cell(
        &args.org,
        &args.env,
        None,
        args.db.as_deref(),
        &args.base,
        app_pack.as_ref(),
    ) {
        Ok(cell) => cell,
        Err(failure) => return failure.into_output(),
    };
    let listener = match TcpListener::bind(addr) {
        Ok(listener) => listener,
        Err(err) => {
            return CommandOutput::usage_or_io_error(&format!("failed to bind `{addr}`: {err}"));
        }
    };
    let bound = listener.local_addr().unwrap_or(addr);
    let mode = if args.db.is_some() {
        "sqlite"
    } else {
        "in-memory"
    };
    let token_banner = if auth.from_env() {
        "<from NORN_DEV_TOKEN environment>"
    } else {
        auth.token()
    };
    let mut stderr = std::io::stderr().lock();
    let _written = writeln!(
        stderr,
        "norn dev listening on http://{bound} (org={}, env={}, storage={mode})",
        args.org, args.env
    );
    let _written = writeln!(
        stderr,
        "norn dev trust model: loopback-only; every request requires X-Norn-Dev-Token or Authorization: Bearer <token>; X-Norn-Actor + X-Norn-Roles simulate callers only after token auth"
    );
    let _written = writeln!(stderr, "{DEV_TOKEN_ENV}={token_banner}");
    drop(stderr);
    serve_listener(&listener, &cell, &auth);
    CommandOutput::ok(String::new())
}

/// Serve incoming HTTP/1.1 connections on `listener` until it closes.
pub(super) fn serve_listener(listener: &TcpListener, cell: &LocalCell, auth: &DevAuth) {
    let active = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for stream in listener.incoming().flatten() {
            let Some(slot) = ThreadSlot::try_acquire(&active) else {
                drop(stream);
                continue;
            };
            let _spawned = std::thread::Builder::new()
                .name("norn-dev-http".to_owned())
                .spawn_scoped(scope, move || {
                    let _handled = handle_connection(&stream, cell, auth);
                    drop(slot);
                });
        }
    });
}

struct ThreadSlot<'a>(&'a AtomicUsize);

impl<'a> ThreadSlot<'a> {
    fn try_acquire(counter: &'a AtomicUsize) -> Option<Self> {
        let mut current = counter.load(Ordering::Acquire);
        loop {
            if current >= MAX_THREADS {
                return None;
            }
            match counter.compare_exchange_weak(
                current,
                current.saturating_add(1),
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(Self(counter)),
                Err(next) => current = next,
            }
        }
    }
}

impl Drop for ThreadSlot<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Handle one bounded HTTP/1.1 request on `stream` under the default 30s whole-request deadline.
pub(super) fn handle_connection(
    stream: &TcpStream,
    cell: &LocalCell,
    auth: &DevAuth,
) -> std::io::Result<()> {
    handle_connection_with_timeout(stream, cell, auth, IO_TIMEOUT)
}

/// Handle one bounded HTTP/1.1 request on `stream` under an explicit whole-request `timeout`.
pub(super) fn handle_connection_with_timeout(
    stream: &TcpStream,
    cell: &LocalCell,
    auth: &DevAuth,
    timeout: Duration,
) -> std::io::Result<()> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| std::io::Error::other("request deadline overflow"))?;
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;
    stream.set_nodelay(true)?;
    let mut reader = BufReader::new(stream);
    let response = match read_http_request(&mut reader, deadline) {
        Ok(request) => dispatch_http_request(cell, auth, &request),
        Err(resp) => resp,
    };
    write_http_response(stream, &response)
}

/// Dispatch one parsed HTTP/1.1 request against `cell` after validating headers
/// and authenticating the local session secret.
pub(super) fn dispatch_http_request(
    cell: &LocalCell,
    auth: &DevAuth,
    req: &HttpRequest,
) -> HttpResponse {
    if let Err(resp) = validate_request_headers(&req.headers) {
        return resp;
    }
    if let Err(resp) = authenticate_dev_session(&req.headers, auth) {
        return resp;
    }
    if req.path == "/health" {
        if req.method != "GET" {
            return method_not_allowed("GET");
        }
        return health_response(cell);
    }
    if req.path == "/v1/defs" || req.path == "/v1/definitions" {
        if req.method != "GET" {
            return method_not_allowed("GET");
        }
        return route_defs(cell, req);
    }
    if req.path == PLAN_PATH {
        if req.method != "POST" {
            return method_not_allowed("POST");
        }
        return route_plan(cell, req);
    }
    if req.path == APPLY_PATH {
        if req.method != "POST" {
            return method_not_allowed("POST");
        }
        return route_apply(cell, req);
    }
    if let Some(action_seg) = req.path.strip_prefix("/v1/actions/") {
        if req.method != "POST" {
            return method_not_allowed("POST");
        }
        return route_action(cell, req, action_seg);
    }
    if let Some(obj_rest) = req.path.strip_prefix("/v1/objects/") {
        if req.method != "GET" {
            return method_not_allowed("GET");
        }
        return route_objects(cell, req, obj_rest);
    }
    synthetic_refusal(
        404,
        "route.not_found",
        "visibility",
        "unknown route",
        &format!("route `{}` is not served by norn dev", req.path),
    )
}

fn health_response(cell: &LocalCell) -> HttpResponse {
    let mut obj = Map::new();
    obj.insert("ok".to_owned(), Value::Bool(true));
    obj.insert("org".to_owned(), Value::from(cell.scope().org_id.as_str()));
    obj.insert(
        "env".to_owned(),
        Value::from(cell.scope().environment_id.as_str()),
    );
    HttpResponse {
        status: 200,
        body: Value::Object(obj).to_string(),
    }
}

fn route_defs(cell: &LocalCell, req: &HttpRequest) -> HttpResponse {
    let (scope, query_map) = match extract_scope_and_query(req) {
        Ok(pair) => pair,
        Err(resp) => return resp,
    };
    let version = match query_map.get("version") {
        None => None,
        Some(raw) => match raw.parse::<u64>() {
            Ok(ver) => Some(ver),
            Err(_err) => return refusal_response(&Refusal::invalid("version must be a u64")),
        },
    };
    let actor = extract_actor(&req.headers);
    let defs_query = DefinitionsQuery {
        org_id: scope.org_id,
        environment_id: scope.environment_id,
        version,
    };
    match cell.get_defs(&defs_query, actor.as_ref()) {
        Ok(defs) => encoded_json_response(200, serde_json::to_string(&defs)),
        Err(refusal) => refusal_response(&refusal),
    }
}

fn route_plan(cell: &LocalCell, req: &HttpRequest) -> HttpResponse {
    let (scope, _query_map) = match extract_scope_and_query(req) {
        Ok(pair) => pair,
        Err(resp) => return resp,
    };
    let actor = extract_actor(&req.headers);
    match cell.plan_definitions(&scope, actor.as_ref(), &req.body) {
        Ok(plan) => encoded_json_response(200, serde_json::to_string(&plan)),
        Err(refusal) => refusal_response(&refusal),
    }
}

fn route_apply(cell: &LocalCell, req: &HttpRequest) -> HttpResponse {
    let (scope, _query_map) = match extract_scope_and_query(req) {
        Ok(pair) => pair,
        Err(resp) => return resp,
    };
    let actor = extract_actor(&req.headers);
    match cell.apply_definitions(&scope, actor.as_ref(), &req.body) {
        Ok(applied) => encoded_json_response(200, serde_json::to_string(&applied)),
        Err(refusal) => refusal_response(&refusal),
    }
}

fn route_action(cell: &LocalCell, req: &HttpRequest, raw_segment: &str) -> HttpResponse {
    let (scope, query_map) = match extract_scope_and_query(req) {
        Ok(pair) => pair,
        Err(resp) => return resp,
    };
    let version = match query_map.get("version") {
        None => None,
        Some(raw) => match raw.parse::<u64>() {
            Ok(ver) => Some(ver),
            Err(_err) => return refusal_response(&Refusal::invalid("version must be a u64")),
        },
    };
    let stripped = raw_segment.strip_suffix(":call").unwrap_or(raw_segment);
    if stripped.is_empty() || stripped.contains('/') {
        return refusal_response(&Refusal::invalid("actionId path segment is invalid"));
    }
    let action_id = match percent_decode_path(stripped) {
        Ok(id) => id,
        Err(err) => return refusal_response(&Refusal::invalid(err)),
    };
    let actor = extract_actor(&req.headers);
    let query = ActionQuery {
        org_id: scope.org_id,
        environment_id: scope.environment_id,
        version,
    };
    match cell.call_action(&query, actor.as_ref(), &action_id, &req.body) {
        Ok(ActionAnswer::Receipt(receipt)) => {
            encoded_json_response(200, serde_json::to_string(&receipt))
        }
        Ok(ActionAnswer::Pending(pending)) => {
            encoded_json_response(202, serde_json::to_string(&pending))
        }
        Err(refusal) => refusal_response(&refusal),
    }
}

fn route_objects(cell: &LocalCell, req: &HttpRequest, rest: &str) -> HttpResponse {
    let (scope, query_map) = match extract_scope_and_query(req) {
        Ok(pair) => pair,
        Err(resp) => return resp,
    };
    let actor = extract_actor(&req.headers);
    let parts: Vec<&str> = rest.split('/').collect();
    match parts.as_slice() {
        [raw_type] if !raw_type.is_empty() => {
            let object_type = match percent_decode_path(raw_type) {
                Ok(ty) => ty,
                Err(err) => return refusal_response(&Refusal::invalid(err)),
            };
            let params = ObjectQueryParams {
                limit: query_map.get("limit").cloned(),
                sort: query_map.get("sort").cloned(),
                filter: query_map.get("filter").cloned(),
                after: query_map.get("after").cloned(),
            };
            match cell.query_objects(&scope, actor.as_ref(), &object_type, &params) {
                Ok(page) => disclosed_ok(&page),
                Err(refusal) => refusal_response(&refusal),
            }
        }
        [raw_type, raw_id] if !raw_type.is_empty() && !raw_id.is_empty() => {
            let (object_type, id) =
                match (percent_decode_path(raw_type), percent_decode_path(raw_id)) {
                    (Ok(ty), Ok(id)) => (ty, id),
                    (Err(err), _) | (_, Err(err)) => {
                        return refusal_response(&Refusal::invalid(err));
                    }
                };
            match cell.get_object(&scope, actor.as_ref(), &object_type, &id) {
                Ok(record) => disclosed_ok(&record),
                Err(refusal) => refusal_response(&refusal),
            }
        }
        _ => synthetic_refusal(
            404,
            "route.not_found",
            "visibility",
            "unknown object route",
            "expected /v1/objects/{type} or /v1/objects/{type}/{id}",
        ),
    }
}

fn extract_scope_and_query(
    req: &HttpRequest,
) -> Result<(ScopeQuery, BTreeMap<String, String>), HttpResponse> {
    let query_map =
        parse_query(&req.query).map_err(|err| refusal_response(&Refusal::invalid(err)))?;
    let header_org = single_header(&req.headers, "x-norn-org-id")
        .or_else(|| single_header(&req.headers, "x-norn-org"))
        .map(str::to_owned);
    let header_env = single_header(&req.headers, "x-norn-environment-id")
        .or_else(|| single_header(&req.headers, "x-norn-env"))
        .map(str::to_owned);
    let query_org = query_map.get("orgId").cloned();
    let query_env = query_map.get("environmentId").cloned();
    if let (Some(q), Some(h)) = (&query_org, &header_org)
        && q != h
    {
        return Err(refusal_response(&Refusal::invalid(
            "conflicting orgId query parameter and header",
        )));
    }
    if let (Some(q), Some(h)) = (&query_env, &header_env)
        && q != h
    {
        return Err(refusal_response(&Refusal::invalid(
            "conflicting environmentId query parameter and header",
        )));
    }
    Ok((
        ScopeQuery {
            org_id: query_org.or(header_org),
            environment_id: query_env.or(header_env),
        },
        query_map,
    ))
}
