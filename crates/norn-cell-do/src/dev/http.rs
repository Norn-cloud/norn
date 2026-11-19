//! A bounded HTTP/1.1 transport for the dev host: one request per
//! connection, served in arrival order, over the same dispatch the Durable
//! Object host uses (`norn_cell_do::server`). It carries method, path, query,
//! the `Authorization` header and a bounded body, nothing else.
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, RwLock};
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

/// Serve `listener` until the process exits.
pub(crate) fn serve(
    listener: &TcpListener,
    serving: &Serving,
    principals: &DevPrincipals,
    questions: &DevMetrics,
    engine: Option<&dyn Transport>,
    connections: &DevConnections,
) {
    for stream in listener.incoming().flatten() {
        // A broken connection affects only itself.
        let _ignored = connection(stream, serving, principals, questions, engine, connections);
    }
}

fn connection(
    mut stream: TcpStream,
    serving: &Serving,
    principals: &DevPrincipals,
    questions: &DevMetrics,
    engine: Option<&dyn Transport>,
    connections: &DevConnections,
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let response = match read_request(&stream) {
        Ok(parsed) => dispatch(&parsed, serving, principals, questions, engine, connections),
        Err(refused) => refusal(&refused),
    };
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
    write!(
        stream,
        "HTTP/1.1 {} {reason}\r\ncontent-type: application/json\r\ncache-control: no-store\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
        response.status,
        response.body.len(),
        response.body
    )?;
    stream.flush()
}

struct Parsed {
    method: String,
    path: String,
    query: String,
    authorization: Option<String>,
    body: String,
}

fn read_request(stream: &TcpStream) -> Result<Parsed, Refusal> {
    let mut reader = BufReader::new(
        stream
            .take(u64::try_from(MAX_HEAD_BYTES.saturating_add(MAX_BODY_BYTES)).unwrap_or(u64::MAX)),
    );
    let mut line = String::new();
    let mut head_bytes = 0_usize;
    let mut next = |reader: &mut BufReader<_>, line: &mut String| -> Result<(), Refusal> {
        line.clear();
        let read = reader
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
    next(&mut reader, &mut line)?;
    let mut parts = line.split_whitespace();
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
        return Err(Refusal::invalid("the request line is malformed"));
    };
    let (method, target) = (method.to_owned(), target.to_owned());
    let (mut length, mut authorization) = (0_usize, None);
    loop {
        next(&mut reader, &mut line)?;
        let header = line.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            let value = value.trim();
            if name.eq_ignore_ascii_case("content-length") {
                length = value
                    .parse()
                    .map_err(|error| Refusal::invalid(format!("content-length: {error}")))?;
            } else if name.eq_ignore_ascii_case("authorization") {
                authorization = Some(value.to_owned());
            }
        }
    }
    if length > MAX_BODY_BYTES {
        return Err(Refusal::new(
            http::StatusCode::PAYLOAD_TOO_LARGE,
            "request.too_large",
            "the request body exceeds the host bound",
            "a dev host request body is at most 512 KiB",
        ));
    }
    let mut body = vec![0_u8; length];
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
    // DEV-ONE-AUTHORITY-1: only this process installs definitions, from the
    // project's sources; the hosted plan/apply routes are not served here.
    if parsed.path == norn_api::cell_definitions::PLAN_PATH
        || parsed.path == norn_api::cell_definitions::APPLY_PATH
    {
        return refusal(&Refusal::new(
            http::StatusCode::CONFLICT,
            "defs.dev_sources_only",
            "norn dev installs definitions from the project's sources",
            "save the project's sources; norn dev admits and installs every change itself",
        ));
    }
    let now = super::now_ms().checked_div(1000).unwrap_or_default();
    // The active revision's metrics, derived before the request runs.
    let questions = match questions.questions(host) {
        Ok(questions) => questions,
        Err(refused) => return refusal(&refused),
    };
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
    // D102: with an engine linked in, questions and profiles run on it.
    let Some(engine) = engine else {
        return server::handle_with(host, principals, Some(&questions), &request);
    };
    if let Some(response) = server::analytics(host, principals, engine, None, &request) {
        return response;
    }
    let on_engine = EngineQuestions::new(&questions, engine, host.scope());
    server::handle_with(host, principals, Some(&on_engine), &request)
}

fn refusal(refused: &Refusal) -> Response {
    Response {
        status: refused.status,
        body: serde_json::to_string(refused).unwrap_or_else(|_error| String::from("{}")),
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
mod tests {
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
            "{method} {target} HTTP/1.1\r\nhost: 127.0.0.1\r\n{auth}content-length: {}\r\n\r\n{body}",
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
            .with_authorizer(LocalDeveloper {
                actor: ACTOR.into(),
            });
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
            let principals = DevPrincipals {
                bearer: BEARER.to_owned(),
                actor: ACTOR.to_owned(),
            };
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
        assert!(
            bodies.lock().unwrap().is_empty(),
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
        assert!(bodies.lock().unwrap().is_empty());

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
