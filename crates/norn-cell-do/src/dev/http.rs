//! A bounded HTTP/1.1 transport for the dev host over the same dispatch the
//! Durable Object host uses (`norn_cell_do::server`). It carries method, path,
//! query, the `Authorization` header and a bounded body, nothing else.
//!
//! DEV-KEEPALIVE-1 (#1480): a connection stays open for the client's next
//! request until the client closes it, asks `connection: close` or idles past
//! [`IDLE`]. Bun's `fetch` reuses a pooled connection even after a
//! `connection: close` answer, so a host that closed after one answer reset
//! that client's next request. Each connection gets its own thread, and at
//! most [`WORKERS`] requests are answered at once (DEV-CONCURRENT-1), so a long
//! request (a load) never holds the others back.
use std::io::{BufRead, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};
use std::time::Duration;

use crate::server::{self, Request, Response};
use norn_api::analytics_routes::{EngineQuestions, Transport};
use norn_api::host::DevHost;
use norn_api::refusal::Refusal;
use norn_cell::engine::NativeEngine;

use super::policy::{DevMetrics, DevPrincipals};

/// Longest accepted request head (request line plus headers), in bytes.
const MAX_HEAD_BYTES: usize = 16 * 1024;
/// Longest accepted body, in bytes; the action route's own bound is lower.
const MAX_BODY_BYTES: usize = 512 * 1024;
/// How long a connection waits for a request (the first or the next) before
/// it closes.
const IDLE: Duration = Duration::from_secs(10);
/// Most connections held open at once. Past it a connection is answered once
/// and closed on the accepting thread, so a flood of idle connections cannot
/// grow the host's threads without bound.
const MAX_CONNECTIONS: usize = 32;
/// Requests the dev host answers at once (DEV-CONCURRENT-1).
const WORKERS: usize = 4;

/// The serving state shared with the session: `None` until the first
/// revision is installed.
pub(crate) type Serving = Arc<RwLock<Option<DevHost<NativeEngine>>>>;

/// The project's connections (D126): configured from the `NORN_*`
/// environment of `norn dev` (the development KEK is admitted), opened over
/// the project's cell file on the first authorized connection request
/// (CONN-AUTHZ-1), so an unprovisioned cell file stays fresh.
pub(crate) type DevConnections = crate::connections::Routes<NativeEngine>;

/// The dev host's connection routes over the cell file at `cell_path`.
///
/// # Errors
/// A malformed or refused connection configuration (a set
/// `NORN_EGRESS_PROXY`, EGRESS-PROXY-1): `norn dev` refuses to start.
pub(crate) fn dev_connections(cell_path: std::path::PathBuf) -> Result<DevConnections, String> {
    let config = crate::connections::Config::from_env(true)?;
    Ok(crate::connections::Routes::new(
        Ok(config),
        move || {
            NativeEngine::open(&cell_path, norn_cell::StoreOptions::default())
                .map_err(|error| error.to_string())
        },
        std::sync::Arc::new(crate::connections::egress::SystemLookup),
    ))
}

/// What every connection answers with.
struct Handler<'a> {
    serving: &'a Serving,
    principals: &'a DevPrincipals,
    questions: &'a DevMetrics,
    engine: Option<&'a dyn Transport>,
    connections: &'a DevConnections,
    /// DEV-CONCURRENT-1: how many requests are being answered, at most
    /// [`WORKERS`]. Each request runs on its connection's thread from start to
    /// answer, so its verified caller stays its own
    /// ([`crate::policy::CallerSlot`]); the cell is shared under its own lock,
    /// which a load takes one commit at a time (INGEST-YIELD-1).
    /// Ordering contract: requests on one connection are answered in the
    /// order they arrive. Across connections, requests are answered
    /// concurrently, once fully read; the cell orders their steps. A client
    /// that waits for an answer before sending its next request (every dev
    /// client) sees its causal order kept.
    answering: (Mutex<usize>, Condvar),
}

/// One of the [`WORKERS`] turns to answer a request, given back when dropped.
struct Turn<'a>(&'a (Mutex<usize>, Condvar));

impl<'a> Turn<'a> {
    fn take(gate: &'a (Mutex<usize>, Condvar)) -> Self {
        let (count, freed) = gate;
        let mut answering = count
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while *answering >= WORKERS {
            answering = freed
                .wait(answering)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        *answering = answering.saturating_add(1);
        Self(gate)
    }
}

impl Drop for Turn<'_> {
    fn drop(&mut self) {
        let (count, freed) = self.0;
        let mut answering = count
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *answering = answering.saturating_sub(1);
        freed.notify_one();
    }
}

impl Handler<'_> {
    fn answer(&self, parsed: &Parsed) -> Response {
        let _turn = Turn::take(&self.answering);
        dispatch(
            parsed,
            self.serving,
            self.principals,
            self.questions,
            self.engine,
            self.connections,
        )
    }
}

/// Serve `listener` until the process exits.
pub(crate) fn serve(
    listener: &TcpListener,
    serving: &Serving,
    principals: &DevPrincipals,
    questions: &DevMetrics,
    engine: Option<&dyn Transport>,
    connections: &DevConnections,
) {
    let handler = Handler {
        serving,
        principals,
        questions,
        engine,
        connections,
        answering: (Mutex::new(0), Condvar::new()),
    };
    let open = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for stream in listener.incoming().flatten() {
            let (handler, open) = (&handler, &open);
            let slot = Slot::take(open);
            // Every connection, even one over the bound, is read on its own
            // thread, so a silent client never holds up the accept loop. Over
            // the bound it gets one answer and closes, as before keep-alive;
            // the per-request deadline bounds how long that thread lives.
            scope.spawn(move || {
                // A broken connection, even a panicking one, affects only itself.
                let _ignored = connection(&stream, handler, slot.within_bound);
                drop(slot);
            });
        }
    });
}

/// One open connection, counted against [`MAX_CONNECTIONS`] until dropped.
struct Slot<'a> {
    open: &'a AtomicUsize,
    within_bound: bool,
}

impl<'a> Slot<'a> {
    fn take(open: &'a AtomicUsize) -> Self {
        let within_bound = open.fetch_add(1, Ordering::SeqCst) < MAX_CONNECTIONS;
        Self { open, within_bound }
    }
}

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        self.open.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Answer `stream`'s requests in order; with `persistent` false, only its first.
fn connection(stream: &TcpStream, handler: &Handler<'_>, persistent: bool) -> std::io::Result<()> {
    stream.set_read_timeout(Some(IDLE))?;
    stream.set_write_timeout(Some(IDLE))?;
    // DEV-ONE-WRITE-1: an answer is complete when written; never hold it
    // back waiting for an ACK.
    stream.set_nodelay(true)?;
    let mut reader = BufReader::new(stream);
    for served in 0_usize.. {
        // Between requests, a closed, failed or idle connection just ends:
        // an unasked answer would be read as the reply to the next request.
        if served > 0 {
            reader.get_ref().set_read_timeout(Some(IDLE))?;
            if reader.fill_buf().map_or(true, <[u8]>::is_empty) {
                return Ok(());
            }
        }
        let (response, open) = match read_request(&mut reader) {
            Ok(parsed) => (handler.answer(&parsed), persistent && parsed.keep_alive),
            // The request's framing is unknown past a refused head or body.
            Err(refused) => (refusal(&refused), false),
        };
        write_response(stream, &response, open)?;
        if !open {
            break;
        }
    }
    Ok(())
}

fn write_response(mut stream: &TcpStream, response: &Response, open: bool) -> std::io::Result<()> {
    let reason = match response.status {
        200 => "OK",
        201 => "Created",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        409 => "Conflict",
        413 => "Payload Too Large",
        422 => "Unprocessable Content",
        _ => "Status",
    };
    stream.write_all(&encode_response(
        response.status,
        reason,
        &response.body,
        open,
    ))?;
    stream.flush()
}

/// DEV-ONE-WRITE-1 (#1533): the whole answer, head and body, as one buffer.
/// Written in pieces onto a Nagle socket, each answer waited for the client's
/// delayed ACK, an 88 ms floor per call; one `write_all` (and
/// `TCP_NODELAY`, set per connection) sends it at once.
fn encode_response(status: u16, reason: &str, body: &str, open: bool) -> Vec<u8> {
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncache-control: no-store\r\ncontent-length: {}\r\nconnection: {}\r\n\r\n",
        body.len(),
        if open { "keep-alive" } else { "close" },
    );
    let mut bytes = Vec::with_capacity(head.len().saturating_add(body.len()));
    bytes.extend_from_slice(head.as_bytes());
    bytes.extend_from_slice(body.as_bytes());
    bytes
}

struct Parsed {
    method: String,
    path: String,
    query: String,
    authorization: Option<String>,
    body: String,
    /// The client keeps the connection for another request: HTTP/1.1
    /// without `connection: close`, or HTTP/1.0 with `connection: keep-alive`.
    keep_alive: bool,
}

/// Arm the read timeout with what is left of `deadline`, so one request
/// cannot take longer than [`IDLE`] however slowly its bytes trickle in.
fn arm(reader: &BufReader<&TcpStream>, deadline: std::time::Instant) -> Result<(), Refusal> {
    let left = deadline.saturating_duration_since(std::time::Instant::now());
    if left.is_zero() {
        return Err(Refusal::invalid(
            "the request was not complete within the host bound",
        ));
    }
    reader
        .get_ref()
        .set_read_timeout(Some(left))
        .map_err(|error| Refusal::invalid(format!("unreadable request: {error}")))
}

fn read_request(reader: &mut BufReader<&TcpStream>) -> Result<Parsed, Refusal> {
    let deadline = std::time::Instant::now()
        .checked_add(IDLE)
        .ok_or_else(|| Refusal::invalid("the host clock cannot bound this request"))?;
    let mut line = String::new();
    let mut head_bytes = 0_usize;
    let mut next = |reader: &mut BufReader<&TcpStream>, line: &mut String| -> Result<(), Refusal> {
        arm(reader, deadline)?;
        line.clear();
        // One byte past the bound, so an over-long head is told apart.
        let left = MAX_HEAD_BYTES.saturating_sub(head_bytes).saturating_add(1);
        let read = reader
            .by_ref()
            .take(u64::try_from(left).unwrap_or(u64::MAX))
            .read_line(line)
            .map_err(|error| Refusal::invalid(format!("unreadable request: {error}")))?;
        head_bytes = head_bytes.saturating_add(read);
        if read == 0 || head_bytes > MAX_HEAD_BYTES {
            return Err(Refusal::invalid(
                "the request head is missing or exceeds the host bound",
            ));
        }
        Ok(())
    };
    next(reader, &mut line)?;
    let mut parts = line.split_whitespace();
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
        return Err(Refusal::invalid("the request line is malformed"));
    };
    let http_1_1 = parts.next() != Some("HTTP/1.0");
    let (mut asked_close, mut asked_keep_alive) = (false, false);
    let (method, target) = (method.to_owned(), target.to_owned());
    let (mut length, mut authorization, mut chunked) = (None::<usize>, None, false);
    loop {
        next(reader, &mut line)?;
        let header = line.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            let value = value.trim();
            if name.eq_ignore_ascii_case("content-length") {
                // A repeated length makes the body boundary ambiguous, and a
                // wrong boundary would leak bytes into the next request.
                if length.is_some() {
                    return Err(Refusal::invalid("content-length is repeated"));
                }
                if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
                    return Err(Refusal::invalid("content-length is not a decimal length"));
                }
                length = Some(
                    value
                        .parse()
                        .map_err(|error| Refusal::invalid(format!("content-length: {error}")))?,
                );
            } else if name.eq_ignore_ascii_case("authorization") {
                authorization = Some(value.to_owned());
            } else if name.eq_ignore_ascii_case("transfer-encoding") {
                // Unframed here, as before: the connection ends after it.
                chunked = true;
            } else if name.eq_ignore_ascii_case("connection") {
                for option in value.split(',').map(str::trim) {
                    if option.eq_ignore_ascii_case("close") {
                        asked_close = true;
                    } else if option.eq_ignore_ascii_case("keep-alive") {
                        asked_keep_alive = true;
                    }
                }
            }
        }
    }
    let length = length.unwrap_or(0);
    // `close` wins wherever it appears, in one header or across several.
    let keep_alive = !asked_close && (http_1_1 || asked_keep_alive);
    if length > MAX_BODY_BYTES {
        return Err(Refusal::new(
            http::StatusCode::PAYLOAD_TOO_LARGE,
            "request.too_large",
            "the request body exceeds the host bound",
            "a dev host request body is at most 512 KiB",
        ));
    }
    let mut body = vec![0_u8; length];
    arm(reader, deadline)?;
    reader.read_exact(&mut body).map_err(|error| {
        Refusal::invalid(format!("the body is shorter than content-length: {error}"))
    })?;
    let body = String::from_utf8(body)
        .map_err(|error| Refusal::invalid(format!("the body is not UTF-8: {error}")))?;
    let (path, query) = target.split_once('?').unwrap_or((target.as_str(), ""));
    Ok(Parsed {
        path: path.to_owned(),
        query: query.to_owned(),
        method,
        authorization,
        body,
        keep_alive: keep_alive && !chunked,
    })
}

fn dispatch(
    parsed: &Parsed,
    serving: &Serving,
    principals: &DevPrincipals,
    questions: &DevMetrics,
    engine: Option<&dyn Transport>,
    connections: &DevConnections,
) -> Response {
    let Ok(guard) = serving.read() else {
        return refusal(&Refusal::host("the serving state is poisoned"));
    };
    let Some(host) = guard.as_ref() else {
        return refusal(&Refusal::new(
            http::StatusCode::CONFLICT,
            "defs.not_provisioned",
            "the project has no admitted revision yet",
            "fix the refused change shown by norn dev; the first admitted revision is served",
        ));
    };
    let now = super::now_ms().checked_div(1000).unwrap_or_default();
    let request = Request {
        method: &parsed.method,
        path: &parsed.path,
        query: &parsed.query,
        authorization: parsed.authorization.as_deref(),
        now,
        body: &parsed.body,
    };
    // D126: connection credentials, decided by the same policy.
    if let Some(response) = connections.route(host, principals, &request, super::now_ms()) {
        return response;
    }
    answer(host, principals, questions, engine, &request)
}

/// One request against the serving cell: what the transport answers once it
/// has parsed the request. Tests drive the dev host through this.
pub(crate) fn answer(
    host: &DevHost<NativeEngine>,
    principals: &DevPrincipals,
    questions: &DevMetrics,
    engine: Option<&dyn Transport>,
    request: &Request<'_>,
) -> Response {
    // DEV-ONE-AUTHORITY-1: only this process installs definitions, from the
    // project's sources; no route that changes them is served here.
    if changes_definitions(request.path) {
        return refusal(&Refusal::new(
            http::StatusCode::CONFLICT,
            "defs.dev_sources_only",
            "norn dev installs definitions from the project's sources",
            "save the project's sources; norn dev admits and installs every change itself",
        ));
    }
    // A recorded export loads as on a hosted cell, decided by the same policy
    // first (CONNECT-AUTH-1), so one loader serves both hosts.
    if let Some(response) = crate::connect::observations(host, principals, request) {
        return response;
    }
    // The hosted host's definition kinds: an approval names hosted roles.
    let kinds = super::policy::dev_kinds();
    // The active revision's metrics, derived before the request runs.
    let questions = match questions.questions(host) {
        Ok(questions) => questions,
        Err(refused) => return refusal(&refused),
    };
    // D102: with an engine linked in, questions and profiles run on it.
    let Some(engine) = engine else {
        return server::handle_all(host, principals, Some(&questions), &kinds, request);
    };
    if let Some(response) = server::analytics(host, principals, engine, None, request) {
        return response;
    }
    let on_engine = EngineQuestions::new(&questions, engine, host.scope());
    server::handle_all(host, principals, Some(&on_engine), &kinds, request)
}

/// The routes that change the installed definitions: the hosted plan/apply
/// pair and the retained-definition install and retire. Admit only checks a
/// definition and stays served.
fn changes_definitions(path: &str) -> bool {
    path == norn_api::cell_definitions::PLAN_PATH
        || path == norn_api::cell_definitions::APPLY_PATH
        || path == "/v1/definitions:install"
        || path
            .strip_prefix("/v1/definitions/")
            .is_some_and(|rest| rest.ends_with(":retire"))
}

fn refusal(refused: &Refusal) -> Response {
    Response {
        status: refused.status,
        body: serde_json::to_string(refused).unwrap_or_else(|_error| String::from("{}")),
    }
}

/// The admitted constitution melt cell the action tests share.
#[cfg(test)]
#[path = "../../../norn-api/tests/support/action_cell.rs"]
#[allow(
    dead_code,
    reason = "the shared fixture module serves the action tests too"
)]
pub(in crate::dev) mod action_cell;

#[cfg(test)]
#[path = "series_approval_tests.rs"]
mod series_approval_tests;

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unwrap_in_result,
    clippy::indexing_slicing,
    reason = "dev-host dispatch scenarios"
)]
mod tests {
    //! DEV-REVIEWER-1 through the dev host's own dispatch: the hosted
    //! definition kinds admit an approval over hosted roles, a gated call
    //! waits as one open decision, its requester never decides it, and the
    //! local reviewer's approval commits it as the requester.
    use super::*;
    use crate::dev::policy::{LocalDeveloper, dev_actions, refresh_actions, reviewer};
    use norn_api::scope::CellScope;
    use serde_json::{Value, json};

    use super::action_cell::{DERIVES_LOSS, MELT_ACTION, melt_body, melt_store};

    const DEVELOPER: &str = "local:developer";
    const DEVELOPER_TOKEN: &str = "developer-token-0123456789";
    const REVIEWER_TOKEN: &str = "reviewer-token-01234567890";
    const SCOPE: &str = "orgId=org_demo&environmentId=dev";

    struct Dev {
        host: DevHost<NativeEngine>,
        principals: DevPrincipals,
        metrics: DevMetrics,
    }

    fn dev() -> Dev {
        let scope = CellScope {
            org_id: "org_demo".into(),
            environment_id: "dev".into(),
        };
        let store = melt_store(
            DERIVES_LOSS,
            norn_api::action_admission::prepare_action_model,
        );
        let actions = dev_actions();
        let host = DevHost::from_store(store, scope.clone(), |command| command.payload.clone())
            .unwrap()
            .with_authorizer(LocalDeveloper::for_actor(
                DEVELOPER,
                std::sync::Arc::clone(&actions),
            ));
        refresh_actions(&actions, &host).unwrap();
        Dev {
            host,
            principals: DevPrincipals::new(vec![
                (DEVELOPER_TOKEN.into(), DEVELOPER.into()),
                (REVIEWER_TOKEN.into(), reviewer(DEVELOPER)),
            ]),
            metrics: DevMetrics::new([9; 32], scope),
        }
    }

    fn call(
        dev: &Dev,
        token: Option<&str>,
        (method, path, query): (&str, &str, &str),
        body: &str,
    ) -> (u16, Value) {
        let authorization = token.map(|token| format!("Bearer {token}"));
        let response = answer(
            &dev.host,
            &dev.principals,
            &dev.metrics,
            None,
            &Request {
                method,
                path,
                query,
                authorization: authorization.as_deref(),
                now: 1_800_000_000,
                body,
            },
        );
        (
            response.status,
            serde_json::from_str(&response.body).unwrap(),
        )
    }

    fn approval(roles: &[&str]) -> Value {
        json!({ "kind": "approval", "id": "ops.melt_approval", "payload": {
            "action": MELT_ACTION, "decider": { "roles": roles },
            "question": "Convert this melt?", "risk": "high" } })
    }

    /// Installs the approval in process, as norn dev installs from the
    /// project's sources: no network route installs (DEV-ONE-AUTHORITY-1).
    fn gate(dev: &Dev) {
        let mut body = approval(&["admin"]);
        let (status, admitted) = call(
            dev,
            Some(DEVELOPER_TOKEN),
            ("POST", "/v1/definitions:admit", SCOPE),
            &body.to_string(),
        );
        assert_eq!(status, 200, "{admitted}");
        body["signoff"] = json!({ "restatementSha256": admitted["restatementSha256"] });
        norn_api::definition_routes::install_definition(
            &dev.host,
            &norn_api::scope::ScopeQuery {
                org_id: Some("org_demo".into()),
                environment_id: Some("dev".into()),
            },
            Some(DEVELOPER),
            &crate::dev::policy::dev_kinds(),
            None,
            &serde_json::from_str(&body.to_string()).unwrap(),
            1_800_000_000_000,
        )
        .unwrap();
    }

    #[test]
    fn no_route_changes_the_definitions_on_the_dev_host() {
        let dev = dev();
        let mut body = approval(&["admin"]);
        let (_, admitted) = call(
            &dev,
            Some(DEVELOPER_TOKEN),
            ("POST", "/v1/definitions:admit", SCOPE),
            &body.to_string(),
        );
        body["signoff"] = json!({ "restatementSha256": admitted["restatementSha256"] });
        for path in [
            norn_api::cell_definitions::PLAN_PATH,
            norn_api::cell_definitions::APPLY_PATH,
            "/v1/definitions:install",
            "/v1/definitions/ops.melt_approval:retire",
        ] {
            let (status, refused) = call(
                &dev,
                Some(DEVELOPER_TOKEN),
                ("POST", path, SCOPE),
                &body.to_string(),
            );
            assert_eq!(
                (status, &refused["code"]),
                (409, &json!("defs.dev_sources_only")),
                "{path}: {refused}"
            );
        }
        let query = format!("{SCOPE}&kind=approval");
        let (status, listed) = call(
            &dev,
            Some(DEVELOPER_TOKEN),
            ("GET", "/v1/definitions:list", &query),
            "",
        );
        assert_eq!(status, 200, "{listed}");
        assert!(
            !listed.to_string().contains("ops.melt_approval"),
            "nothing was installed: {listed}"
        );
    }

    fn pending(dev: &Dev, token: &str) -> Value {
        let query = format!("{SCOPE}&state=pending");
        let (status, page) = call(dev, Some(token), ("GET", "/v1/decisions", &query), "");
        assert_eq!(status, 200, "{page}");
        page
    }

    fn melt(dev: &Dev) -> u16 {
        let path = "/v1/objects/constitution.Melt/melt-7";
        call(dev, Some(DEVELOPER_TOKEN), ("GET", path, SCOPE), "").0
    }

    #[test]
    fn an_approval_waits_on_the_dev_host_until_the_reviewer_decides() {
        let dev = dev();
        gate(&dev);
        let body = melt_body("melt-7", "12", "10", "dev-1");
        let action = format!("/v1/actions/{MELT_ACTION}");
        let (status, parked) = call(&dev, Some(DEVELOPER_TOKEN), ("POST", &action, SCOPE), &body);
        assert_eq!(status, 202, "{parked}");
        assert_eq!(parked["outcome"], json!("pending"));
        let id = parked["decision"]["id"].as_str().unwrap().to_owned();
        assert_eq!(melt(&dev), 404, "a waiting call commits nothing");

        // The requester never decides their own call (#618).
        assert_eq!(pending(&dev, DEVELOPER_TOKEN)["total"], json!(0));
        let decide = format!("/v1/decisions/{id}:decide");
        let choice = json!({ "choice": "approve", "idempotencyKey": "approve-1" }).to_string();
        let (status, own) = call(
            &dev,
            Some(DEVELOPER_TOKEN),
            ("POST", &decide, SCOPE),
            &choice,
        );
        assert_eq!(status, 404, "{own}");

        // Without a session token nothing is listed or decided.
        let (status, _) = call(&dev, None, ("POST", &decide, SCOPE), &choice);
        assert_ne!(status, 200);

        assert_eq!(pending(&dev, REVIEWER_TOKEN)["total"], json!(1));
        let (status, decided) = call(
            &dev,
            Some(REVIEWER_TOKEN),
            ("POST", &decide, SCOPE),
            &choice,
        );
        assert_eq!(status, 200, "{decided}");
        // The reviewer's request never runs the developer's call
        // (APPROVAL-RECOVERY-1): it waits for the developer's own retry.
        assert_eq!(
            decided["call"],
            json!({ "committed": false, "state": "not_executed" }),
            "{decided}"
        );
        assert_eq!(melt(&dev), 404);
        let (status, committed) =
            call(&dev, Some(DEVELOPER_TOKEN), ("POST", &action, SCOPE), &body);
        assert_eq!(status, 200, "{committed}");
        assert_eq!(melt(&dev), 200);

        // The approved call committed as its requester, never as the
        // reviewer who approved it: the version's writer names the developer.
        let path = "/v1/objects/constitution.Melt/melt-7/history";
        let (status, history) = call(&dev, Some(DEVELOPER_TOKEN), ("GET", path, SCOPE), "");
        assert_eq!(status, 200, "{history}");
        let writer = &history["versions"][0]["writer"];
        assert_eq!(
            (&writer["kind"], &writer["actionId"], &writer["actor"]),
            (&json!("action"), &json!(MELT_ACTION), &json!(DEVELOPER)),
            "{history}"
        );
    }

    /// The source commit of `approvals` over the active melt set, as norn
    /// dev runs it on a save.
    fn sync(
        dev: &Dev,
        approvals: &[Value],
    ) -> Result<crate::dev::definitions::Committed, crate::dev::definitions::Rejected> {
        let active = dev
            .host
            .active_definitions()
            .ok()
            .flatten()
            .expect("active");
        let (model, input) = super::action_cell::melt_install(
            DERIVES_LOSS,
            norn_api::action_admission::prepare_action_model,
            active.installed_at.saturating_add(1),
        );
        crate::dev::definitions::commit(
            &dev.host,
            DEVELOPER,
            &crate::dev::policy::dev_kinds(),
            approvals,
            crate::dev::admit::Admitted { model, input },
            &active,
            &dev_actions(),
            1_800_000_000_000,
        )
    }

    fn named(id: &str, roles: &[&str]) -> Value {
        let mut approval = approval(roles);
        approval["id"] = json!(id);
        approval
    }

    /// The ids of the active approvals.
    fn gates(dev: &Dev) -> Vec<String> {
        let query = format!("{SCOPE}&kind=approval");
        let (status, listed) = call(
            dev,
            Some(DEVELOPER_TOKEN),
            ("GET", "/v1/definitions:list", &query),
            "",
        );
        assert_eq!(status, 200, "{listed}");
        listed["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["id"].as_str().unwrap().to_owned())
            .collect()
    }

    #[test]
    fn a_renamed_approval_keeps_its_action_gated() {
        let dev = dev();
        sync(&dev, &[named("ops.old_gate", &["admin"])]).expect("gated");
        sync(&dev, &[named("ops.new_gate", &["admin"])]).expect("renamed");
        assert_eq!(gates(&dev), ["ops.new_gate"]);
        let action = format!("/v1/actions/{MELT_ACTION}");
        let body = melt_body("melt-7", "12", "10", "dev-1");
        let (status, parked) = call(&dev, Some(DEVELOPER_TOKEN), ("POST", &action, SCOPE), &body);
        assert_eq!(status, 202, "the action is still gated: {parked}");
    }

    #[test]
    fn a_refused_approval_set_keeps_the_active_gates() {
        let dev = dev();
        sync(&dev, &[named("ops.old_gate", &["admin"])]).expect("gated");
        let refused = sync(&dev, &[named("ops.new_gate", &["auditor"])]).expect_err("refused");
        let codes: Vec<&Value> = refused
            .diagnostics
            .iter()
            .map(|finding| &finding["code"])
            .collect();
        assert!(
            codes.contains(&&json!("definition.approval_role_unknown")),
            "{codes:?}"
        );
        assert_eq!(gates(&dev), ["ops.old_gate"], "nothing retired");
    }

    #[test]
    fn an_approval_admits_against_the_hosted_roles() {
        let dev = dev();
        let (status, refused) = call(
            &dev,
            Some(DEVELOPER_TOKEN),
            ("POST", "/v1/definitions:admit", SCOPE),
            &approval(&["auditor"]).to_string(),
        );
        assert_eq!(
            (status, &refused["admitted"]),
            (200, &json!(false)),
            "{refused}"
        );
        assert_eq!(
            refused["diagnostics"][0]["code"],
            json!("definition.approval_role_unknown")
        );
        let (status, admitted) = call(
            &dev,
            Some(DEVELOPER_TOKEN),
            ("POST", "/v1/definitions:admit", SCOPE),
            &approval(&["admin"]).to_string(),
        );
        assert_eq!(
            (status, &admitted["admitted"]),
            (200, &json!(true)),
            "{admitted}"
        );
    }

    #[test]
    fn a_load_without_a_session_token_is_answered_as_outside() {
        let dev = dev();
        let body = json!({ "profile": "synthetic-erpnext-three/1", "snapshot": [] }).to_string();
        let path = "/v1/observations:load";
        let anonymous = call(&dev, None, ("POST", path, SCOPE), &body);
        let outside = crate::server::outside::<NativeEngine>(&Request {
            method: "POST",
            path,
            query: SCOPE,
            authorization: None,
            now: 1_800_000_000,
            body: &body,
        });
        assert_eq!(
            anonymous,
            (outside.status, serde_json::from_str(&outside.body).unwrap())
        );
        // The developer reaches the loader, which refuses a load with no rows
        // before it looks up the mapping. A real load is driven end to end in
        // norn-ts `three_systems_dev_load`.
        let (status, refused) = call(&dev, Some(DEVELOPER_TOKEN), ("POST", path, SCOPE), &body);
        assert_eq!(status, 400, "{refused}");
        assert!(
            refused["explanation"]
                .as_str()
                .is_some_and(|detail| detail.contains("at least one row")),
            "{refused}"
        );
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::too_many_lines,
    reason = "dev-host OAuth handoff end to end"
)]
mod oauth_tests {
    //! OAUTH-HANDOFF-1 at the real entry point: requests over TCP to
    //! [`serve`], the provider's redirect arriving as a browser sends it,
    //! with no `Authorization` header.
    use super::*;
    use crate::connections::egress::{Egress, SystemLookup};
    use crate::connections::providers::{ClientAuth, Provider};
    use crate::connections::service::{ClientCredentials, Connections};
    use crate::connections::vault::{LocalKeyring, Secret, Vault};
    use crate::dev::policy::LocalDeveloper;
    use norn_api::scope::CellScope;
    use norn_cell::{CellStore, Model, StoreOptions};
    use std::collections::BTreeMap;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    const ACTOR: &str = "dev@example";
    const BEARER: &str = "test-bearer-oauth-0123456789";
    const QUERY: &str = "orgId=org_dev&environmentId=local";

    /// A token endpoint recording each request body.
    fn token_endpoint(bodies: Arc<Mutex<Vec<String>>>) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut length = 0;
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                loop {
                    line.clear();
                    reader.read_line(&mut line).unwrap();
                    if line.trim_end().is_empty() {
                        break;
                    }
                    if let Some((name, value)) = line.split_once(':')
                        && name.eq_ignore_ascii_case("content-length")
                    {
                        length = value.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                bodies
                    .lock()
                    .unwrap()
                    .push(String::from_utf8(body).unwrap());
                let answer = r#"{"access_token":"access-token-DEV-0001-done","token_type":"bearer","expires_in":3600}"#;
                let mut out = stream;
                let _sent = write!(
                    out,
                    "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{answer}",
                    answer.len()
                );
            }
        });
        port
    }

    fn send(
        port: u16,
        method: &str,
        target: &str,
        bearer: Option<&str>,
        body: &str,
    ) -> (u16, serde_json::Value) {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let auth = bearer.map_or_else(String::new, |token| {
            format!("authorization: Bearer {token}\r\n")
        });
        write!(
            stream,
            "{method} {target} HTTP/1.1\r\nhost: 127.0.0.1\r\n{auth}content-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut answer = String::new();
        stream.read_to_string(&mut answer).unwrap();
        let (head, body) = answer.split_once("\r\n\r\n").unwrap();
        let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
        (status, serde_json::from_str(body).unwrap())
    }

    #[test]
    fn a_browser_redirect_parks_the_code_and_only_the_starting_session_completes() {
        let dir = tempfile::tempdir().unwrap();
        let cell = dir.path().join("cell.sqlite");
        let scope = CellScope {
            org_id: "org_dev".into(),
            environment_id: "local".into(),
        };
        let store = CellStore::from_engine(
            NativeEngine::open(&cell, StoreOptions::default()).unwrap(),
            Model::from_definitions(&[], &[]).unwrap(),
        )
        .unwrap();
        let host = DevHost::from_store(store, scope.clone(), |_| BTreeMap::new())
            .unwrap()
            .with_authorizer(LocalDeveloper::for_actor(
                ACTOR,
                super::super::policy::dev_actions(),
            ));
        let bodies = Arc::new(Mutex::new(Vec::new()));
        let token_port = token_endpoint(Arc::clone(&bodies));
        let opens = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&opens);
        let vault_path = cell;
        let routes = crate::connections::Routes::with_opener(move |scope_key| {
            counted.fetch_add(1, Ordering::SeqCst);
            let vault = Vault::open(
                NativeEngine::open(&vault_path, StoreOptions::default()).unwrap(),
                scope_key,
                Arc::new(
                    LocalKeyring::new(
                        "test-local-v1",
                        BTreeMap::from([("test-local-v1".to_owned(), [7; 32])]),
                    )
                    .unwrap(),
                ),
            )
            .unwrap();
            let provider = Provider {
                id: "acme".to_owned(),
                system: "acme".to_owned(),
                authorization_url: "https://auth.acme.example/authorize".to_owned(),
                token_url: format!("http://127.0.0.1:{token_port}/token"),
                refresh_token_url: None,
                revocation_url: None,
                scopes: vec!["read".to_owned()],
                optional_scopes: Vec::new(),
                scope_separator: " ".to_owned(),
                client_auth: ClientAuth::Post,
                pkce_required: true,
                authorization_params: BTreeMap::new(),
                url_parameters: Vec::new(),
                withheld: None,
            };
            Connections::new(
                vault,
                Egress::new(vec!["127.0.0.1".to_owned()], Arc::new(SystemLookup)),
                BTreeMap::from([("acme".to_owned(), provider)]),
                BTreeMap::from([(
                    "acme".to_owned(),
                    ClientCredentials {
                        client_id: "client-1".to_owned(),
                        client_secret: Some(Secret::from("client-secret-1".to_owned())),
                    },
                )]),
                Some("http://127.0.0.1/v1/connections/oauth:callback".to_owned()),
            )
            .map_err(|error| error.to_string())
        });
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let serving: Serving = Arc::new(RwLock::new(Some(host)));
        std::thread::spawn(move || {
            let principals = DevPrincipals::new(vec![(BEARER.to_owned(), ACTOR.to_owned())]);
            let questions = DevMetrics::new([3; 32], scope);
            serve(&listener, &serving, &principals, &questions, None, &routes);
        });

        let (status, started) = send(
            port,
            "POST",
            &format!("/v1/connections/acme.main/oauth:start?{QUERY}"),
            Some(BEARER),
            r#"{"provider":"acme"}"#,
        );
        assert_eq!(status, 200, "{started}");
        let authorize = url::Url::parse(started["authorizeUrl"].as_str().unwrap()).unwrap();
        let state = authorize
            .query_pairs()
            .find(|(name, _)| name == "state")
            .unwrap()
            .1
            .into_owned();
        let key = started["completionKey"].as_str().unwrap().to_owned();
        let complete = format!("/v1/connections/acme.main/oauth:complete?{QUERY}");
        let body = serde_json::json!({ "state": state, "completionKey": key }).to_string();

        // Before the redirect: pending, and the state is not spent.
        let (status, pending) = send(port, "POST", &complete, Some(BEARER), &body);
        assert_eq!(
            (status, &pending["code"]),
            (409, &serde_json::json!("connection.oauth_pending"))
        );

        // The provider's redirect, as a browser sends it: no credential.
        let (status, parked) = send(
            port,
            "GET",
            &format!("/v1/connections/oauth:callback?state={state}&code=the-code-1"),
            None,
            "",
        );
        assert_eq!(
            (status, &parked["code"]),
            (200, &serde_json::json!("connection.oauth_received"))
        );
        assert_eq!(
            bodies.lock().unwrap().len(),
            0,
            "the redirect alone exchanged the code"
        );

        // Anyone else (no credential, a wrong one) gets the outside answer;
        // the right credential without the completion key is refused
        // (OAUTH-SESSION-1). None of them spends the code.
        for bearer in [None, Some("not-the-session")] {
            let (status, _) = send(port, "POST", &complete, bearer, &body);
            assert_ne!(status, 200);
        }
        let guessed = serde_json::json!({ "state": state, "completionKey": "guess" }).to_string();
        let (status, _) = send(port, "POST", &complete, Some(BEARER), &guessed);
        assert_eq!(status, 400);
        assert_eq!(bodies.lock().unwrap().len(), 0);

        // The starting session completes: exchanged once, with the code.
        let (status, connected) = send(port, "POST", &complete, Some(BEARER), &body);
        assert_eq!(status, 200, "{connected}");
        assert_eq!(connected["state"], "connected");
        assert_eq!(connected["last4"], "done");
        let exchanged = bodies.lock().unwrap().clone();
        assert_eq!(exchanged.len(), 1);
        assert!(
            exchanged[0].contains("code=the-code-1") && exchanged[0].contains("code_verifier=")
        );
        // Spent: a replay finds nothing reserved.
        let (status, _) = send(port, "POST", &complete, Some(BEARER), &body);
        assert_eq!(status, 400);
        assert_eq!(opens.load(Ordering::SeqCst), 1);
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "transport scenarios over a real socket"
)]
mod keepalive_tests {
    //! DEV-KEEPALIVE-1 (#1480) at the real entry point: a client that sends
    //! its next request on the connection it already has is answered, never
    //! reset, and `connection: close` still ends the connection.
    use super::*;
    use norn_api::scope::CellScope;

    /// A dev host with no admitted revision: every request is answered 409.
    fn host() -> (u16, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let connections = dev_connections(dir.path().join("cell.sqlite")).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let scope = CellScope {
                org_id: "local".into(),
                environment_id: "dev".into(),
            };
            let serving: Serving = Arc::new(RwLock::new(None));
            let principals = DevPrincipals::new(vec![("t".repeat(32), "local:dev".into())]);
            let questions = DevMetrics::new([5; 32], scope);
            serve(
                &listener,
                &serving,
                &principals,
                &questions,
                None,
                &connections,
            );
        });
        (port, dir)
    }

    /// One answer read off `reader`: its status, `connection` header and body.
    fn answer(reader: &mut BufReader<TcpStream>) -> (u16, String, String) {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let status = line.split_whitespace().nth(1).unwrap().parse().unwrap();
        let (mut length, mut connection) = (0, String::new());
        loop {
            line.clear();
            reader.read_line(&mut line).unwrap();
            let Some((name, value)) = line.trim_end().split_once(':') else {
                break;
            };
            if name.eq_ignore_ascii_case("content-length") {
                length = value.trim().parse().unwrap();
            } else if name.eq_ignore_ascii_case("connection") {
                value.trim().clone_into(&mut connection);
            }
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body).unwrap();
        (status, connection, String::from_utf8(body).unwrap())
    }

    fn request(stream: &mut TcpStream, sku: u32, close: bool) {
        let body = format!(r#"{{"input":{{"sku":"SKU-{sku}"}},"idempotencyKey":"add-{sku}"}}"#);
        let connection = if close { "connection: close\r\n" } else { "" };
        write!(
            stream,
            "POST /v1/actions/shop.product.add?orgId=local&environmentId=dev HTTP/1.1\r\nhost: 127.0.0.1\r\n{connection}content-length: {}\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
    }

    #[test]
    fn the_next_request_on_an_answered_connection_is_answered_not_reset() {
        let (port, _dir) = host();
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        // Three requests on one connection, as Bun's fetch reuses its pool;
        // a host that closed after the first answer reset the second.
        for sku in 0..3 {
            request(&mut stream, sku, false);
            let (status, connection, body) = answer(&mut reader);
            assert_eq!((status, connection.as_str()), (409, "keep-alive"), "{body}");
            assert!(body.contains("defs.not_provisioned"), "{body}");
        }
        // Two pipelined requests, the second asking to close: both are
        // answered in order, then the connection ends.
        request(&mut stream, 3, false);
        request(&mut stream, 4, true);
        assert_eq!(answer(&mut reader).1, "keep-alive");
        assert_eq!(answer(&mut reader).1, "close");
        let mut rest = Vec::new();
        assert_eq!(reader.read_to_end(&mut rest).unwrap(), 0);
    }

    #[test]
    fn an_answer_is_sent_whole_without_waiting_for_an_ack() {
        // DEV-ONE-WRITE-1: an answer written in pieces onto a Nagle socket
        // waited for the client's delayed ACK (about 40 ms each, twice per
        // call). Twenty keep-alive round trips of a cheap answer must take
        // far less than one delayed ACK each.
        let (port, _dir) = host();
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream.set_nodelay(true).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let started = std::time::Instant::now();
        for sku in 0..20_u32 {
            let body = format!(r#"{{"input":{{"sku":"SKU-{sku}"}},"idempotencyKey":"add-{sku}"}}"#);
            let whole = format!(
                "POST /v1/actions/shop.product.add?orgId=local&environmentId=dev HTTP/1.1\r\nhost: 127.0.0.1\r\ncontent-length: {}\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(whole.as_bytes()).unwrap();
            assert_eq!(answer(&mut reader).0, 409);
        }
        let took = started.elapsed();
        assert!(
            took < Duration::from_millis(500),
            "20 round trips took {took:?}"
        );
    }

    #[test]
    fn an_encoded_answer_is_its_head_then_its_body() {
        let bytes = encode_response(409, "Conflict", r#"{"a":1}"#, true);
        assert_eq!(
            String::from_utf8(bytes).unwrap(),
            "HTTP/1.1 409 Conflict\r\ncontent-type: application/json\r\ncache-control: no-store\r\ncontent-length: 7\r\nconnection: keep-alive\r\n\r\n{\"a\":1}"
        );
    }

    #[test]
    fn an_idle_connection_does_not_hold_up_another_client() {
        let (port, _dir) = host();
        // A client that connects and stays silent, as a browser's spare
        // connection does, gets its own thread: the next client is answered.
        let _idle = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        request(&mut stream, 0, true);
        assert_eq!(answer(&mut reader).0, 409);
    }

    /// Send a raw request head (and body) and read the single answer.
    fn raw(port: u16, head: &str, body: &str) -> (u16, String, Vec<u8>) {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        write!(stream, "{head}\r\n\r\n{body}").unwrap();
        let (status, connection, _) = answer(&mut reader);
        let mut rest = Vec::new();
        // A read error here leaves `rest` as read so far; callers assert on it.
        if let Err(_closed) = reader.read_to_end(&mut rest) {}
        (status, connection, rest)
    }

    #[test]
    fn a_repeated_or_malformed_content_length_is_refused_and_closes() {
        let (port, _dir) = host();
        let target =
            "POST /v1/actions/shop.product.add?orgId=local&environmentId=dev HTTP/1.1\r\nhost: x";
        // Two lengths make the body boundary ambiguous: refuse and close, so
        // no leftover byte is ever read as a second request. (No body is sent:
        // unread bytes at close would make the kernel reset the connection.)
        let (status, connection, rest) = raw(
            port,
            &format!("{target}\r\ncontent-length: 2\r\ncontent-length: 40"),
            "",
        );
        assert_eq!((status, connection.as_str()), (400, "close"));
        assert_eq!(rest.len(), 0, "nothing after the refusal");
        for bad in ["+2", "2 2", "0x2", ""] {
            let (status, connection, _) =
                raw(port, &format!("{target}\r\ncontent-length: {bad}"), "");
            assert_eq!((status, connection.as_str()), (400, "close"), "{bad:?}");
        }
    }

    #[test]
    fn close_wins_over_keep_alive_in_either_order() {
        let (port, _dir) = host();
        let target = "POST /v1/actions/shop.product.add?orgId=local&environmentId=dev HTTP/1.1\r\nhost: x\r\ncontent-length: 2";
        for connection in [
            "connection: close, keep-alive",
            "connection: keep-alive, close",
            "connection: close\r\nconnection: keep-alive",
            "connection: keep-alive\r\nconnection: close",
        ] {
            let (status, answered, rest) = raw(port, &format!("{target}\r\n{connection}"), "{}");
            assert_eq!(
                (status, answered.as_str()),
                (409, "close"),
                "{connection:?}"
            );
            assert_eq!(rest.len(), 0, "{connection:?}: the connection ends");
        }
    }

    #[test]
    fn a_client_over_the_bound_does_not_stall_the_accept_loop() {
        let (port, _dir) = host();
        // Fill every slot, plus one over the bound, with silent clients.
        let _silent: Vec<_> = (0..=MAX_CONNECTIONS)
            .map(|_| TcpStream::connect(("127.0.0.1", port)).unwrap())
            .collect();
        // A further client is still answered promptly (one answer, then close).
        let started = std::time::Instant::now();
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(30)))
            .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        request(&mut stream, 0, false);
        let (status, connection, _) = answer(&mut reader);
        assert_eq!((status, connection.as_str()), (409, "close"));
        assert!(started.elapsed() < IDLE, "answered before any idle timeout");
    }
}
