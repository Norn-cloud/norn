//! The public router: the native port of `apps/norn-cell-do/worker.mjs`'s
//! default export. It only picks the actor: each well-formed scope in a
//! declared environment has its own cell actor, named by its key; anything
//! else reaches the demo cell, whose scope admission answers the same opaque
//! denial as any other failed read. Nothing here inspects data or grants
//! access. Requests reach the actor through the engine's gateway, so any
//! runner process can route to a cell another runner hosts.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::ws::{Message, WebSocket as ClientSocket, WebSocketUpgrade};
use axum::extract::{FromRequestParts as _, Request, State};
use axum::response::{IntoResponse as _, Response};
use futures::{SinkExt as _, StreamExt as _};
use http::{HeaderMap, HeaderValue, Method, StatusCode};
use rivetkit::client::{Client, GetOrCreateOptions};
use serde_json::json;
use tokio_tungstenite::tungstenite::Message as UpstreamMessage;

use crate::actor::{
    CELL_ACTOR, CELL_HEADER, LARGE_BODY_PATHS, MAX_BODY, MAX_PROVISION_BODY, cell_key,
};
use crate::config::HostConfig;
use norn_api::scope::CellScope;

/// The demo cell: where a request naming no well-formed scope lands.
const DEMO: (&str, &str) = ("org_demo", "dev");
/// The parameter a conformance request names its own cell object with.
const OBJECT_PARAM: &str = "conformanceObject";

/// Whether `part` can name an organization.
fn org_part(part: &str) -> bool {
    let bytes = part.as_bytes();
    (1..=128).contains(&bytes.len())
        && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(b))
}

/// Whether `part` can name an environment.
#[must_use]
pub fn environment_part(part: &str) -> bool {
    let bytes = part.as_bytes();
    (1..=64).contains(&bytes.len())
        && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(b))
}

fn is_demo(org: &str, env: &str) -> bool {
    org == DEMO.0 && (env == "dev" || env == "analytics")
}

/// The router's state.
pub struct Router {
    client: Client,
    config: Arc<HostConfig>,
}

impl std::fmt::Debug for Router {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Router")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

/// Where a request goes: an actor, its key, the scope header and the query
/// it is forwarded with.
struct Target {
    actor: &'static str,
    key: String,
    scope: Option<CellScope>,
    query: String,
}

fn params(query: &str) -> Vec<(String, String)> {
    form_pairs(query)
}

fn form_pairs(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            (decode(name), decode(value))
        })
        .collect()
}

fn decode(text: &str) -> String {
    let replaced = text.replace('+', " ");
    let bytes = replaced.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while let Some(&byte) = bytes.get(index) {
        if byte == b'%'
            && let Some(hex) = replaced.get(index.saturating_add(1)..index.saturating_add(3))
            && let Ok(value) = u8::from_str_radix(hex, 16)
        {
            out.push(value);
            index = index.saturating_add(3);
            continue;
        }
        out.push(byte);
        index = index.saturating_add(1);
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn refuse(
    status: StatusCode,
    code: &str,
    category: &str,
    message: &str,
    explanation: &str,
) -> Response {
    let body = json!({"format": "norn/refusal@2", "retryable": false, "reason": null, "code": code, "category": category, "message": message, "explanation": explanation});
    (
        status,
        [("content-type", "application/json")],
        body.to_string(),
    )
        .into_response()
}

impl Router {
    /// A router over the engine at `client`.
    #[must_use]
    pub const fn new(client: Client, config: Arc<HostConfig>) -> Self {
        Self { client, config }
    }

    fn cors(&self, headers: &HeaderMap) -> Vec<(&'static str, String)> {
        let origin = headers.get("origin").and_then(|value| value.to_str().ok());
        match origin {
            Some(origin)
                if self
                    .config
                    .allowed_origins
                    .iter()
                    .any(|allowed| allowed == origin) =>
            {
                vec![
                    ("access-control-allow-origin", origin.to_owned()),
                    (
                        "access-control-allow-headers",
                        "authorization, content-type".to_owned(),
                    ),
                    ("access-control-allow-methods", "GET, POST".to_owned()),
                    ("vary", "origin".to_owned()),
                ]
            }
            Some(_) | None => Vec::new(),
        }
    }

    fn target(&self, path: &str, query: &str) -> Result<Target, Box<Response>> {
        let pairs = params(query);
        let all = |name: &str| {
            pairs
                .iter()
                .filter(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
                .collect::<Vec<_>>()
        };
        let orgs = all("orgId");
        let envs = all("environmentId");
        if self.config.conformance {
            if path.starts_with("/__probe/") {
                let object = all("object")
                    .into_iter()
                    .next()
                    .unwrap_or_else(|| "probe".to_owned());
                return Ok(Target {
                    actor: crate::conformance_probe_actor(),
                    key: object,
                    scope: None,
                    query: query.to_owned(),
                });
            }
            let synthetic = crate::synthetic_scope();
            if let Some(object) = all(OBJECT_PARAM).into_iter().next() {
                let kept = query
                    .split('&')
                    .filter(|pair| !pair.starts_with(&format!("{OBJECT_PARAM}=")))
                    .collect::<Vec<_>>()
                    .join("&");
                return Ok(Target {
                    actor: CELL_ACTOR,
                    key: format!("conformance/{object}"),
                    scope: Some(synthetic),
                    query: kept,
                });
            }
            if !(orgs.len() == 1
                && envs.len() == 1
                && orgs.first() == Some(&synthetic.org_id)
                && envs.first() == Some(&synthetic.environment_id))
            {
                return Err(Box::new(refuse(
                    StatusCode::FORBIDDEN,
                    "conformance.scope",
                    "permission",
                    "this conformance entry serves only its synthetic scope",
                    "a conformance build serves only an opted-in test stage and its synthetic scope",
                )));
            }
        }
        let named = match (orgs.as_slice(), envs.as_slice()) {
            ([org], [env])
                if org_part(org)
                    && environment_part(env)
                    && (is_demo(org, env)
                        || self
                            .config
                            .environments
                            .iter()
                            .any(|declared| declared == env)) =>
            {
                Some(CellScope {
                    org_id: org.clone(),
                    environment_id: env.clone(),
                })
            }
            _ => None,
        };
        let scope = named.unwrap_or_else(|| CellScope {
            org_id: DEMO.0.to_owned(),
            environment_id: DEMO.1.to_owned(),
        });
        Ok(Target {
            actor: CELL_ACTOR,
            key: cell_key(&scope),
            scope: Some(scope),
            query: query.to_owned(),
        })
    }

    /// Serve one request.
    pub async fn serve(self: Arc<Self>, request: Request) -> Response {
        let cors = self.cors(request.headers());
        let mut response = self.dispatch(request).await;
        for (name, value) in cors {
            if let Ok(value) = HeaderValue::from_str(&value) {
                response.headers_mut().insert(name, value);
            }
        }
        response
    }

    async fn dispatch(self: &Arc<Self>, request: Request) -> Response {
        if request.method() == Method::OPTIONS {
            return StatusCode::NO_CONTENT.into_response();
        }
        let path = request.uri().path().to_owned();
        let query = request.uri().query().unwrap_or_default().to_owned();
        let target = match self.target(&path, &query) {
            Ok(target) => target,
            Err(refused) => return *refused,
        };
        if path == "/v1/subscribe" {
            return self.subscribe(request, target).await;
        }
        let limit = if LARGE_BODY_PATHS.contains(&path.as_str()) {
            MAX_PROVISION_BODY
        } else {
            MAX_BODY
        };
        let (parts, body) = request.into_parts();
        let body = if parts.method == Method::POST {
            match axum::body::to_bytes(body, limit).await {
                Ok(bytes) => Some(bytes),
                Err(_too_large) => {
                    return refuse(
                        StatusCode::PAYLOAD_TOO_LARGE,
                        "request.too_large",
                        "limit",
                        "the request body is too large",
                        &format!("submissions are at most {limit} bytes"),
                    );
                }
            }
        } else {
            None
        };
        let mut headers = HeaderMap::new();
        for (name, value) in &parts.headers {
            let name_text = name.as_str();
            if name_text == "authorization"
                || name_text == "content-type"
                || name_text.starts_with("x-norn-conformance-")
            {
                headers.insert(name.clone(), value.clone());
            }
        }
        if let Some(scope) = &target.scope
            && let Ok(value) =
                HeaderValue::from_str(&json!([scope.org_id, scope.environment_id]).to_string())
        {
            headers.insert(CELL_HEADER, value);
        }
        let forwarded = if target.query.is_empty() {
            path.clone()
        } else {
            format!("{path}?{}", target.query)
        };
        let handle = match self.client.get_or_create(
            target.actor,
            vec![target.key],
            GetOrCreateOptions::default(),
        ) {
            Ok(handle) => handle,
            Err(error) => return unavailable(&error),
        };
        let answered = match handle.fetch(&forwarded, parts.method, headers, body).await {
            Ok(answered) => answered,
            Err(error) => return unavailable(&error),
        };
        let status = answered.status();
        let mut out = HeaderMap::new();
        for (name, value) in answered.headers() {
            let text = name.as_str();
            if text == "content-type"
                || text == "cache-control"
                || text == "content-disposition"
                || text.starts_with("x-norn-")
            {
                out.insert(name.clone(), value.clone());
            }
        }
        let bytes = match answered.bytes().await {
            Ok(bytes) => bytes,
            Err(error) => return unavailable(&anyhow::Error::new(error)),
        };
        let mut response = Response::new(Body::from(bytes));
        *response.status_mut() = status;
        *response.headers_mut() = out;
        response
    }

    /// `/v1/subscribe`: refuse without an upgrade or a token, then join the
    /// client's socket to one with the cell actor.
    async fn subscribe(self: &Arc<Self>, request: Request, target: Target) -> Response {
        let (mut parts, _body) = request.into_parts();
        // As the Durable Object: the upgrade header, then the token, then the handshake.
        let upgrade_required = || {
            refuse(
                StatusCode::UPGRADE_REQUIRED,
                "live.upgrade_required",
                "invalid",
                "live reads are a WebSocket",
                &format!(
                    "open a WebSocket with the {} protocol",
                    crate::live::PROTOCOL
                ),
            )
        };
        let asked = parts
            .headers
            .get("upgrade")
            .and_then(|value| value.to_str().ok());
        if !asked.is_some_and(|value| value.eq_ignore_ascii_case("websocket")) {
            return upgrade_required();
        }
        let Some(authorization) = parts
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
        else {
            return refuse(
                StatusCode::UNAUTHORIZED,
                "authn.required",
                "permission",
                "sign in first",
                "a live socket carries the caller's token",
            );
        };
        let Ok(upgrade) = WebSocketUpgrade::from_request_parts(&mut parts, &()).await else {
            return upgrade_required();
        };
        let Some(scope) = target.scope else {
            return refuse(
                StatusCode::NOT_FOUND,
                "live.not_subscribable",
                "invalid",
                "no cell here",
                "live reads name a cell scope",
            );
        };
        let router = Arc::clone(self);
        upgrade
            .protocols([crate::live::PROTOCOL])
            .on_upgrade(move |socket| async move {
                if let Err(error) = router
                    .join(socket, target.actor, target.key, &scope, authorization)
                    .await
                {
                    tracing::warn!(error = %format!("{error:#}"), "live socket ended");
                }
            })
    }

    async fn join(
        &self,
        client: ClientSocket,
        actor: &str,
        key: String,
        scope: &CellScope,
        authorization: String,
    ) -> anyhow::Result<()> {
        let handle = self
            .client
            .get_or_create(actor, vec![key], GetOrCreateOptions::default())?;
        let mut upstream = handle.web_socket("/v1/subscribe", None).await?;
        let hello = json!({"type": crate::live::HELLO, "authorization": authorization, "scope": [scope.org_id, scope.environment_id]});
        upstream
            .send(UpstreamMessage::text(hello.to_string()))
            .await?;
        let (mut to_cell, mut from_cell) = upstream.split();
        let (mut to_client, mut from_client) = client.split();
        let inbound = async {
            while let Some(Ok(message)) = from_client.next().await {
                let forwarded = match message {
                    Message::Text(text) => UpstreamMessage::text(text.as_str()),
                    Message::Binary(bytes) => UpstreamMessage::binary(bytes),
                    Message::Close(_) => break,
                    Message::Ping(_) | Message::Pong(_) => continue,
                };
                if to_cell.send(forwarded).await.is_err() {
                    break;
                }
            }
            let _closed = to_cell.close().await;
        };
        let outbound = async {
            while let Some(Ok(message)) = from_cell.next().await {
                let forwarded = match message {
                    UpstreamMessage::Text(text) => Message::text(text.as_str()),
                    UpstreamMessage::Binary(bytes) => Message::binary(bytes),
                    UpstreamMessage::Close(frame) => {
                        let frame = frame.map(|frame| axum::extract::ws::CloseFrame {
                            code: frame.code.into(),
                            reason: frame.reason.as_str().into(),
                        });
                        let _closed = to_client.send(Message::Close(frame)).await;
                        break;
                    }
                    UpstreamMessage::Ping(_)
                    | UpstreamMessage::Pong(_)
                    | UpstreamMessage::Frame(_) => continue,
                };
                if to_client.send(forwarded).await.is_err() {
                    break;
                }
            }
        };
        tokio::select! {
            () = inbound => {}
            () = outbound => {}
        }
        Ok(())
    }
}

fn unavailable(error: &anyhow::Error) -> Response {
    tracing::warn!(error = %format!("{error:#}"), "the cell actor did not answer");
    let body = json!({"format": "norn/refusal@2", "retryable": true, "reason": null, "code": "host.unavailable", "category": "host", "message": "the cell is unavailable", "explanation": "retry the request"});
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [("content-type", "application/json")],
        body.to_string(),
    )
        .into_response()
}

/// The router as an axum service on every path.
pub fn app(router: Arc<Router>) -> axum::Router {
    axum::Router::new()
        .fallback(
            move |State(router): State<Arc<Router>>, request: Request| async move {
                router.serve(request).await
            },
        )
        .with_state(router)
}

/// The router's listener: every accepted connection sets `TCP_NODELAY`.
/// hyper writes a response in more than one segment, and with Nagle's
/// algorithm on the last one waits for the client's delayed ACK (an 88 ms
/// floor per request at one client on the dev host).
pub fn listener(
    listener: tokio::net::TcpListener,
) -> impl axum::serve::Listener<Io = tokio::net::TcpStream, Addr = std::net::SocketAddr> {
    use axum::serve::ListenerExt as _;
    listener.tap_io(|tcp| {
        if let Err(error) = tcp.set_nodelay(true) {
            tracing::warn!(%error, "could not set TCP_NODELAY on an accepted connection");
        }
    })
}

#[cfg(test)]
mod tests {
    use axum::serve::Listener as _;

    #[tokio::test]
    async fn accepted_connections_set_nodelay() {
        let bound = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = bound.local_addr().unwrap();
        let mut listener = super::listener(bound);
        let ((stream, _), client) =
            tokio::join!(listener.accept(), tokio::net::TcpStream::connect(addr));
        client.unwrap();
        assert!(stream.nodelay().unwrap(), "an accepted connection has TCP_NODELAY");
    }
}
