//! The public router refuses a caller without a valid host token before it
//! reads the body, looks up or creates an actor, or upgrades a socket. The
//! engine here is a bare TCP listener that counts connections and signals a
//! shared `Notify` on each accept: every actor lookup, creation or request
//! goes through it, so a refused request leaves the count at zero. A valid
//! token is the control that reaches it.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use axum::response::IntoResponse as _;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signer as _, SigningKey};
use http::StatusCode;
use norn_cell_rivet::actor::{MAX_PROVISION_BODY, now_ms};
use norn_cell_rivet::config::HostConfig;
use norn_cell_rivet::gate::COLD_BACKOFF;
use norn_cell_rivet::router::{Router, app};
use rivetkit::client::{Client, ClientConfig};
use serde_json::{Value, json};
use tokio::sync::Notify;

const ISSUER: &str = "https://studio.test";

fn key() -> SigningKey {
    SigningKey::from_bytes(&[3; 32])
}

fn jwks() -> String {
    json!({ "keys": [{ "kty": "OKP", "crv": "Ed25519", "alg": "EdDSA", "kid": "k1",
                       "x": URL_SAFE_NO_PAD.encode(key().verifying_key().as_bytes()) }] })
    .to_string()
}

fn now() -> u64 {
    now_ms() / 1000
}

/// A host token signed by `signer` for `org`, issued `age` seconds ago.
fn token_with(signer: &SigningKey, org: &str, age: u64) -> String {
    let issued = now().saturating_sub(age);
    let claims = json!({ "iss": ISSUER, "aud": "norn-host", "sub": "u1",
            "iat": issued, "exp": issued.saturating_add(290),
            "norn": { "kind": "user", "org": org, "role": "owner", "amr": "password" } });
    let head = URL_SAFE_NO_PAD.encode(json!({ "alg": "EdDSA", "kid": "k1" }).to_string());
    let body = URL_SAFE_NO_PAD.encode(claims.to_string());
    let signature = signer.sign(format!("{head}.{body}").as_bytes());
    format!(
        "Bearer {head}.{body}.{}",
        URL_SAFE_NO_PAD.encode(signature.to_bytes())
    )
}

fn token(org: &str) -> String {
    token_with(&key(), org, 10)
}

struct Host {
    url: String,
    engine_connections: Arc<AtomicUsize>,
    /// Woken on each accept by the counting engine, so tests can await a
    /// real outbound hit without a yield-spin.
    engine_connected: Arc<Notify>,
    http: reqwest::Client,
    /// Number of JWKS fetches the issuer mock has served (test-controlled).
    jwks_fetches: Arc<AtomicUsize>,
    /// When `true`, the issuer mock serves 200 with the JWKS document;
    /// otherwise it serves 500 and the fetch fails.
    jwks_ok: Arc<AtomicBool>,
}

async fn start_counting_engine() -> (std::net::SocketAddr, Arc<AtomicUsize>, Arc<Notify>) {
    let engine = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let engine_addr = engine.local_addr().unwrap();
    let engine_connections = Arc::new(AtomicUsize::new(0));
    let engine_connected = Arc::new(Notify::new());
    let counted = Arc::clone(&engine_connections);
    let notifier = Arc::clone(&engine_connected);
    tokio::spawn(async move {
        while let Ok((stream, _)) = engine.accept().await {
            counted.fetch_add(1, Ordering::SeqCst);
            notifier.notify_one();
            drop(stream);
        }
    });
    (engine_addr, engine_connections, engine_connected)
}

async fn start_issuer_mock() -> (std::net::SocketAddr, Arc<AtomicUsize>, Arc<AtomicBool>) {
    let issuer = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let issuer_addr = issuer.local_addr().unwrap();
    let fetches = Arc::new(AtomicUsize::new(0));
    let ok = Arc::new(AtomicBool::new(true));
    let counted = Arc::clone(&fetches);
    let ok_flag = Arc::clone(&ok);
    tokio::spawn(async move {
        let document = jwks();
        let keys = axum::Router::new().fallback(move || {
            let document = document.clone();
            let counted = Arc::clone(&counted);
            let ok_flag = Arc::clone(&ok_flag);
            async move {
                counted.fetch_add(1, Ordering::SeqCst);
                if ok_flag.load(Ordering::SeqCst) {
                    (StatusCode::OK, document).into_response()
                } else {
                    (StatusCode::INTERNAL_SERVER_ERROR, String::new()).into_response()
                }
            }
        });
        axum::serve(issuer, keys).await.unwrap();
    });
    (issuer_addr, fetches, ok)
}

/// A router in front of a counting engine, with a controllable JWKS mock.
/// The gate uses its real (production) cold-start backoff ([`COLD_BACKOFF`]).
async fn host() -> Host {
    let (engine_addr, engine_connections, engine_connected) = start_counting_engine().await;
    let (issuer_addr, jwks_fetches, jwks_ok) = start_issuer_mock().await;
    let config = Arc::new(HostConfig {
        issuer: Some(ISSUER.to_owned()),
        jwks_url: Some(format!("http://{issuer_addr}/jwks")),
        environments: vec!["dev".to_owned()],
        ..HostConfig::default()
    });
    let client = Client::new(ClientConfig::new(format!("http://default:t@{engine_addr}")));
    let router = Arc::new(Router::new(client, config).unwrap());
    let public = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let public_addr = public.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(public, app(router)).await.unwrap() });
    Host {
        url: format!("http://{public_addr}"),
        engine_connections,
        engine_connected,
        http: reqwest::Client::new(),
        jwks_fetches,
        jwks_ok,
    }
}

const SCOPED: &str = "/v1/cell:provision?orgId=org_a&environmentId=dev";

async fn refusal(response: reqwest::Response) -> (u16, Value) {
    let status = response.status().as_u16();
    let text = response.text().await.unwrap();
    (status, serde_json::from_str(&text).unwrap())
}

#[tokio::test]
async fn a_post_without_a_valid_token_never_reaches_the_engine() {
    let host = host().await;
    let forger = SigningKey::from_bytes(&[9; 32]);
    let refused: [(&str, Option<String>); 6] = [
        ("no token", None),
        ("not a bearer", Some("Basic dTpw".to_owned())),
        ("garbage", Some("Bearer not.a.token".to_owned())),
        ("forged", Some(token_with(&forger, "org_a", 10))),
        ("another org", Some(token("org_b"))),
        ("expired", Some(token_with(&key(), "org_a", 3600))),
    ];
    for (case, authorization) in refused {
        // Larger than any body the router would buffer: a refusal before
        // reading answers 403, not 413.
        let mut request = host
            .http
            .post(format!("{}{SCOPED}", host.url))
            .body(vec![b'x'; MAX_PROVISION_BODY.saturating_add(1)]);
        if let Some(authorization) = authorization {
            request = request.header("authorization", authorization);
        }
        let (status, body) = refusal(request.send().await.unwrap()).await;
        assert_eq!(status, 403, "{case}: {body}");
        assert_eq!(body["code"], "authz.denied", "{case}");
    }
    let get = host
        .http
        .get(format!("{}/v1/me?orgId=org_a&environmentId=dev", host.url))
        .send()
        .await
        .unwrap();
    assert_eq!(get.status().as_u16(), 403);
    // No scope named: the demo cell's scope, which org_a's token is not for.
    let unscoped = host
        .http
        .get(format!("{}/v1/me", host.url))
        .header("authorization", token("org_a"))
        .send()
        .await
        .unwrap();
    assert_eq!(unscoped.status().as_u16(), 403);
    assert_eq!(
        host.engine_connections.load(Ordering::SeqCst),
        0,
        "a refused request looked up or created an actor"
    );
}

#[tokio::test]
async fn a_socket_without_a_valid_token_is_refused_before_the_upgrade() {
    let host = host().await;
    let upgrade = |authorization: Option<String>| {
        let mut request = host
            .http
            .get(format!(
                "{}/v1/subscribe?orgId=org_a&environmentId=dev",
                host.url
            ))
            .header("connection", "upgrade")
            .header("upgrade", "websocket")
            .header("sec-websocket-version", "13")
            .header("sec-websocket-key", URL_SAFE_NO_PAD.encode([7_u8; 16]))
            .header("sec-websocket-protocol", "norn.live.v1");
        if let Some(authorization) = authorization {
            request = request.header("authorization", authorization);
        }
        request.send()
    };
    let (status, body) = refusal(upgrade(None).await.unwrap()).await;
    assert_eq!(
        (status, body["code"].as_str()),
        (401, Some("authn.required"))
    );
    for authorization in [
        "Bearer not.a.token".to_owned(),
        token("org_b"),
        token_with(&SigningKey::from_bytes(&[9; 32]), "org_a", 10),
    ] {
        let (status, body) = refusal(upgrade(Some(authorization)).await.unwrap()).await;
        assert_eq!((status, body["code"].as_str()), (403, Some("authz.denied")));
    }
    assert_eq!(
        host.engine_connections.load(Ordering::SeqCst),
        0,
        "a refused socket reached the engine"
    );
}

#[tokio::test]
async fn the_health_check_answers_without_a_token_or_the_engine() {
    let host = host().await;
    let response = host
        .http
        .get(format!("{}/healthz", host.url))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    assert_eq!(host.engine_connections.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_valid_token_reaches_the_engine() {
    let host = host().await;
    let response = host
        .http
        .post(format!("{}{SCOPED}", host.url))
        .header("authorization", token("org_a"))
        .body("{}")
        .send()
        .await
        .unwrap();
    // The counting engine speaks no protocol, so the actor is unavailable;
    // what matters is that the router went to it.
    assert_eq!(response.status().as_u16(), 503);
    assert!(
        host.engine_connections.load(Ordering::SeqCst) > 0,
        "an admitted request did not reach the engine"
    );
}

/// The WebSocket subscribe path with a valid token: the gate admits, axum
/// completes the upgrade handshake (101 Switching Protocols), and the
/// spawned join task opens a connection to the counting engine.
#[tokio::test]
async fn a_valid_token_on_a_websocket_upgrade_reaches_the_engine() {
    let host = host().await;
    let public_addr = host.url.strip_prefix("http://").unwrap();
    let stream = tokio::net::TcpStream::connect(public_addr).await.unwrap();
    let request = http::Request::builder()
        .uri(format!(
            "ws://{public_addr}/v1/subscribe?orgId=org_a&environmentId=dev"
        ))
        .header("host", public_addr)
        .header("connection", "upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-version", "13")
        .header("sec-websocket-key", URL_SAFE_NO_PAD.encode([5_u8; 16]))
        .header("sec-websocket-protocol", "norn.live.v1")
        .header("authorization", token("org_a"))
        .body(())
        .unwrap();
    let (socket, response) = tokio_tungstenite::client_async(request, stream)
        .await
        .unwrap();
    assert_eq!(
        response.status().as_u16(),
        101,
        "a valid-token WebSocket upgrade was not accepted"
    );
    // axum spawns the join future after the upgrade. The counting engine
    // notifies on accept; await under a timeout so a regression fails the
    // test instead of hanging.
    tokio::time::timeout(Duration::from_secs(5), host.engine_connected.notified())
        .await
        .expect("the admitted WebSocket upgrade never reached the engine");
    assert!(host.engine_connections.load(Ordering::SeqCst) > 0);
    drop(socket);
}

/// A cold-start JWKS outage returns 503 for every request and backs off:
/// a concurrent burst coalesces on the gate's mutex and hits the issuer
/// exactly once, the backoff holds a recovered JWKS behind 503 until
/// the real [`COLD_BACKOFF`] elapses (advanced via `tokio::time::pause`
/// so reqwest's live-time HTTP timeouts are untouched), and then the gate
/// refetches and admits.
#[tokio::test]
async fn jwks_unreachable_at_cold_start_backs_off_and_recovers() {
    let host = host().await;
    host.jwks_ok.store(false, Ordering::SeqCst);

    // Five concurrent requests: the coalescing path (mutex waiters) must
    // dedupe to one outbound JWKS fetch.
    let request = || async {
        host.http
            .post(format!("{}{SCOPED}", host.url))
            .header("authorization", token("org_a"))
            .body("{}")
            .send()
            .await
    };
    let (r1, r2, r3, r4, r5) = tokio::join!(request(), request(), request(), request(), request());
    for r in [r1, r2, r3, r4, r5] {
        assert_eq!(r.unwrap().status().as_u16(), 503);
    }
    assert_eq!(
        host.jwks_fetches.load(Ordering::SeqCst),
        1,
        "cold-start backoff did not coalesce the burst (expected 1 fetch)"
    );
    assert_eq!(
        host.engine_connections.load(Ordering::SeqCst),
        0,
        "an unavailable gate let a request through to the engine"
    );

    // JWKS recovers, but the backoff still holds within the cooldown.
    host.jwks_ok.store(true, Ordering::SeqCst);
    let response = request().await.unwrap();
    assert_eq!(response.status().as_u16(), 503);
    assert_eq!(
        host.jwks_fetches.load(Ordering::SeqCst),
        1,
        "the gate hit JWKS inside the cold-start backoff window"
    );

    // Real cooldown elapses on the monotonic clock. `pause` + `advance`
    // only around the time jump, then `resume` immediately so reqwest's
    // live HTTP timeouts are not affected.
    tokio::time::pause();
    tokio::time::advance(COLD_BACKOFF.saturating_add(Duration::from_millis(1))).await;
    tokio::time::resume();

    // The next request refetches and admits.
    let response = request().await.unwrap();
    // The counting engine drops the connection: the admitted request
    // answers 503, but the engine saw it.
    assert_eq!(response.status().as_u16(), 503);
    assert_eq!(
        host.jwks_fetches.load(Ordering::SeqCst),
        2,
        "the gate did not refetch JWKS after the cooldown elapsed"
    );
    assert!(
        host.engine_connections.load(Ordering::SeqCst) > 0,
        "the gate did not recover: an admitted request did not reach the engine"
    );
}
