//! The public router refuses a caller without a valid host token before it
//! reads the body, looks up or creates an actor, or upgrades a socket. The
//! engine here is a bare TCP listener that counts connections: every actor
//! lookup, creation or request goes through it, so a refused request leaves
//! the count at zero. A valid token is the control that reaches it.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signer as _, SigningKey};
use norn_cell_rivet::actor::{MAX_PROVISION_BODY, now_ms};
use norn_cell_rivet::config::HostConfig;
use norn_cell_rivet::router::{Router, app};
use rivetkit::client::{Client, ClientConfig};
use serde_json::{Value, json};

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
    http: reqwest::Client,
}

/// A router in front of a counting engine, with its issuer's JWKS served
/// locally.
async fn host() -> Host {
    let engine = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let engine_addr = engine.local_addr().unwrap();
    let engine_connections = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&engine_connections);
    tokio::spawn(async move {
        while let Ok((stream, _)) = engine.accept().await {
            counted.fetch_add(1, Ordering::SeqCst);
            drop(stream);
        }
    });
    let issuer = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let issuer_addr = issuer.local_addr().unwrap();
    let document = jwks();
    tokio::spawn(async move {
        let keys = axum::Router::new().fallback(move || {
            let document = document.clone();
            async move { document }
        });
        axum::serve(issuer, keys).await.unwrap();
    });
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
        http: reqwest::Client::new(),
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
            .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
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
