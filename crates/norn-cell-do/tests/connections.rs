//! Connector credentials (D126): the vault, OAuth + PKCE, single-flight
//! refresh, the egress client and the routes' authorization.
#![cfg(not(target_arch = "wasm32"))]
#![allow(
    clippy::expect_used,
    clippy::unwrap_in_result,
    clippy::too_many_lines,
    reason = "test harness: a failed setup step fails the test"
)]

use std::collections::BTreeMap;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{IpAddr, TcpListener};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use norn_api::authz::{AccessRequest, Authorizer, Operation};
use norn_api::host::DevHost;
use norn_api::refusal::Refusal;
use norn_api::scope::CellScope;
use norn_cell::engine::NativeEngine;
use norn_cell::{CellStore, Model, StoreOptions};
use norn_cell_do::connections::egress::{Egress, EgressError, Lookup};
use norn_cell_do::connections::providers::{self, ClientAuth, Provider};
use norn_cell_do::connections::service::{
    ClientCredentials, ConnError, Connections, SetCredentials,
};
use norn_cell_do::connections::vault::{KeyId, KeyWrap, LocalKeyring, Secret, Vault, VaultError};
use norn_cell_do::principal::PrincipalResolver;
use norn_cell_do::server::Request;
use sha2::Digest as _;

const SCOPE: &str = "org_demo/dev";
const ACCESS: &str = "access-token-SECRET-0001-wxyz";
const REFRESH: &str = "refresh-token-SECRET-0002";
const NEW_ACCESS: &str = "access-token-ROTATED-0003-abcd";
const NEW_REFRESH: &str = "refresh-token-ROTATED-0004";

// ---------------------------------------------------------------- helpers

fn keyring(current: &str, ids: &[(&str, u8)]) -> LocalKeyring {
    LocalKeyring::new(
        current,
        ids.iter()
            .map(|(id, fill)| ((*id).to_owned(), [*fill; 32]))
            .collect(),
    )
    .expect("keyring")
}

fn temp_db() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("dir");
    let path = dir.path().join("cell.sqlite");
    (dir, path)
}

fn engine(path: &std::path::Path) -> NativeEngine {
    NativeEngine::open(path, StoreOptions::default()).expect("engine")
}

/// One recorded request: request line, lower-cased headers, body.
#[derive(Debug, Clone)]
struct Seen {
    line: String,
    headers: BTreeMap<String, String>,
    body: String,
}

type Handler = Arc<dyn Fn(&Seen) -> (u16, Vec<(String, String)>, String) + Send + Sync>;

/// A loopback HTTP/1.1 server answering every request with `handler`.
struct Mock {
    port: u16,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Mock {
    fn start(handler: Handler) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let record = Arc::clone(&seen);
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let handler = Arc::clone(&handler);
                let record = Arc::clone(&record);
                std::thread::spawn(move || {
                    let mut reader = BufReader::new(stream.try_clone().expect("clone"));
                    let mut line = String::new();
                    if reader.read_line(&mut line).is_err() || line.is_empty() {
                        return;
                    }
                    let mut headers = BTreeMap::new();
                    loop {
                        let mut header = String::new();
                        reader.read_line(&mut header).expect("header");
                        let header = header.trim_end();
                        if header.is_empty() {
                            break;
                        }
                        if let Some((name, value)) = header.split_once(':') {
                            headers
                                .insert(name.trim().to_ascii_lowercase(), value.trim().to_owned());
                        }
                    }
                    let length = headers
                        .get("content-length")
                        .and_then(|v| v.parse::<usize>().ok())
                        .unwrap_or(0);
                    let mut body = vec![0; length];
                    reader.read_exact(&mut body).expect("body");
                    let seen = Seen {
                        line: line.trim_end().to_owned(),
                        headers,
                        body: String::from_utf8_lossy(&body).into_owned(),
                    };
                    record.lock().expect("seen").push(seen.clone());
                    let (status, extra, body) = handler(&seen);
                    let mut out = stream;
                    let mut head = format!(
                        "HTTP/1.1 {status} X\r\ncontent-length: {}\r\nconnection: close\r\n",
                        body.len()
                    );
                    for (name, value) in extra {
                        head = format!("{head}{name}: {value}\r\n");
                    }
                    // A client that hung up is not this server's failure.
                    if write!(out, "{head}\r\n{body}").is_ok() {
                        out.flush().ok();
                    }
                });
            }
        });
        Self { port, seen }
    }

    fn seen(&self) -> Vec<Seen> {
        self.seen.lock().expect("seen").clone()
    }
}

#[derive(Debug, Default)]
struct NoLookup;

impl Lookup for NoLookup {
    fn lookup(&self, host: &str, _port: u16) -> std::io::Result<Vec<IpAddr>> {
        Err(std::io::Error::other(format!("no lookup for {host}")))
    }
}

fn loopback_egress() -> Egress {
    Egress::new(vec!["127.0.0.1".to_owned()], None, Arc::new(NoLookup))
        .expect("egress")
        .with_sleep(|_| {})
}

fn test_provider(port: u16) -> Provider {
    Provider {
        id: "acme".to_owned(),
        system: "acme".to_owned(),
        authorization_url: "https://auth.acme.example/authorize".to_owned(),
        token_url: format!("http://127.0.0.1:{port}/token"),
        refresh_token_url: None,
        revocation_url: Some(format!("http://127.0.0.1:{port}/revoke")),
        scopes: vec!["read".to_owned()],
        optional_scopes: vec!["write".to_owned()],
        scope_separator: " ".to_owned(),
        client_auth: ClientAuth::Post,
        pkce_required: false,
        authorization_params: BTreeMap::new(),
        url_parameters: Vec::new(),
    }
}

fn connections(path: &std::path::Path, port: u16) -> Connections<NativeEngine> {
    let vault = Vault::open(
        engine(path),
        SCOPE,
        Arc::new(keyring("test-local-v1", &[("test-local-v1", 7)])),
    )
    .expect("vault");
    Connections::new(
        vault,
        loopback_egress(),
        BTreeMap::from([("acme".to_owned(), test_provider(port))]),
        BTreeMap::from([(
            "acme".to_owned(),
            ClientCredentials {
                client_id: "client-1".to_owned(),
                client_secret: Some(Secret::from("client-secret-SECRET-0005".to_owned())),
            },
        )]),
        Some("https://app.norn.example/v1/connections/oauth:callback".to_owned()),
    )
    .expect("connections")
}

fn token_server(count: Arc<AtomicUsize>, delay: Duration) -> Mock {
    Mock::start(Arc::new(move |seen: &Seen| {
        if seen.line.starts_with("POST /token") {
            count.fetch_add(1, Ordering::SeqCst);
            std::thread::sleep(delay);
            (
                200,
                vec![("content-type".to_owned(), "application/json".to_owned())],
                format!(
                    r#"{{"access_token":"{NEW_ACCESS}","token_type":"bearer","expires_in":3600,"refresh_token":"{NEW_REFRESH}"}}"#
                ),
            )
        } else {
            (200, Vec::new(), String::new())
        }
    }))
}

fn set(conns: &Connections<NativeEngine>, expires_at_ms: Option<u64>) {
    conns
        .set(
            "owner",
            "acme.main",
            SetCredentials::from_parts("acme", ACCESS, Some(REFRESH), expires_at_ms),
            1_000,
        )
        .expect("set");
}

trait FromParts {
    fn from_parts(
        provider: &str,
        access: &str,
        refresh: Option<&str>,
        expires: Option<u64>,
    ) -> Self;
}

impl FromParts for SetCredentials {
    fn from_parts(
        provider: &str,
        access: &str,
        refresh: Option<&str>,
        expires: Option<u64>,
    ) -> Self {
        serde_json::from_value(serde_json::json!({
            "provider": provider,
            "accessToken": access,
            "refreshToken": refresh,
            "expiresAtMs": expires,
            "scopes": ["read"],
        }))
        .expect("body")
    }
}

// ------------------------------------------------------------------ vault

#[test]
fn vault_round_trips_and_binds_each_value_to_its_cell_slot_and_revision() {
    let (_dir, path) = temp_db();
    let keys: Arc<dyn KeyWrap> = Arc::new(keyring("prod-eu-v1", &[("prod-eu-v1", 1)]));
    let vault = Vault::open(engine(&path), SCOPE, Arc::clone(&keys)).expect("vault");
    vault.put("connection:a", 3, b"hello", 1).expect("put");
    let (revision, opened) = vault.get("connection:a").expect("get").expect("value");
    assert_eq!((revision, opened.expose()), (3, &b"hello"[..]));
    let (_, sealed) = vault.sealed("connection:a").expect("row").expect("sealed");
    // Another slot, revision or cell does not open it (VAULT-AAD-1).
    assert_eq!(
        vault.open_sealed("connection:b", 3, &sealed),
        Err(VaultError::Integrity)
    );
    assert_eq!(
        vault.open_sealed("connection:a", 4, &sealed),
        Err(VaultError::Integrity)
    );
    let other_cell = Vault::open(engine(&path), "org_other/dev", keys).expect("vault");
    assert_eq!(
        other_cell.open_sealed("connection:a", 3, &sealed),
        Err(VaultError::Integrity)
    );
    let mut tampered = sealed;
    tampered.ciphertext[0] ^= 1;
    assert_eq!(
        vault.open_sealed("connection:a", 3, &tampered),
        Err(VaultError::Integrity)
    );
    // A keyring of the right shape but another key does not unwrap the DEK.
    let wrong = Vault::open(
        engine(&path),
        SCOPE,
        Arc::new(keyring("prod-eu-v1", &[("prod-eu-v1", 2)])),
    )
    .expect("vault");
    assert_eq!(wrong.get("connection:a").err(), Some(VaultError::Integrity));
    // The plaintext is nowhere in the cell's file.
    let bytes = std::fs::read(&path).expect("file");
    assert!(!bytes.windows(5).any(|w| w == b"hello"));
}

#[test]
fn rotation_rewraps_the_cell_dek_from_v1_to_v2_without_touching_ciphertext() {
    let (_dir, path) = temp_db();
    let v1 = Vault::open(
        engine(&path),
        SCOPE,
        Arc::new(keyring("prod-eu-v1", &[("prod-eu-v1", 1)])),
    )
    .expect("vault");
    v1.put("connection:a", 1, b"credential", 1).expect("put");
    let before = v1.sealed("connection:a").expect("row").expect("sealed");
    assert_eq!(
        v1.dek_key_ids().expect("ids"),
        [(1, "prod-eu-v1".to_owned())]
    );
    drop(v1);

    // v2 is current; v1 is still listed, unwrap only.
    let both = Vault::open(
        engine(&path),
        SCOPE,
        Arc::new(keyring(
            "prod-eu-v2",
            &[("prod-eu-v2", 2), ("prod-eu-v1", 1)],
        )),
    )
    .expect("vault");
    assert_eq!(both.rewrap().expect("rewrap"), 1);
    assert_eq!(
        both.dek_key_ids().expect("ids"),
        [(1, "prod-eu-v2".to_owned())]
    );
    assert_eq!(both.rewrap().expect("idempotent"), 0);
    // VAULT-ROTATE-1: the credential's ciphertext is byte-for-byte unchanged.
    assert_eq!(
        both.sealed("connection:a").expect("row").expect("sealed"),
        before
    );
    drop(both);

    // v1 retired: v2 alone opens the credential.
    let v2 = Vault::open(
        engine(&path),
        SCOPE,
        Arc::new(keyring("prod-eu-v2", &[("prod-eu-v2", 2)])),
    )
    .expect("vault");
    assert_eq!(
        v2.get("connection:a")
            .expect("get")
            .expect("value")
            .1
            .expose(),
        b"credential"
    );
    // v1 alone no longer can.
    let old = Vault::open(
        engine(&path),
        SCOPE,
        Arc::new(keyring("prod-eu-v1", &[("prod-eu-v1", 1)])),
    )
    .expect("vault");
    assert_eq!(
        old.get("connection:a").err(),
        Some(VaultError::UnknownKey("prod-eu-v2".to_owned()))
    );
}

#[test]
fn a_keyring_holds_one_environment_and_region_and_never_prints_a_key() {
    assert!(KeyId::parse("prod-eu-v3").is_ok());
    for bad in [
        "prod-eu",
        "prod-eu-v0",
        "prod-eu-3",
        "Prod-eu-v1",
        "prod-eu-west-v1",
        "prod--v1",
        "",
    ] {
        assert!(KeyId::parse(bad).is_err(), "{bad}");
    }
    let mixed = LocalKeyring::new(
        "prod-eu-v2",
        BTreeMap::from([
            ("prod-eu-v2".to_owned(), [1; 32]),
            ("prod-us-v1".to_owned(), [2; 32]),
        ]),
    );
    assert!(matches!(mixed, Err(VaultError::Config(_))));
    let staging = LocalKeyring::new(
        "prod-eu-v2",
        BTreeMap::from([
            ("prod-eu-v2".to_owned(), [1; 32]),
            ("staging-eu-v1".to_owned(), [2; 32]),
        ]),
    );
    assert!(matches!(staging, Err(VaultError::Config(_))));

    let key = base64::engine::general_purpose::STANDARD.encode([9_u8; 32]);
    let old = base64::engine::general_purpose::STANDARD.encode([8_u8; 32]);
    let vars = |entries: &[(&str, &str)]| -> BTreeMap<String, String> {
        entries
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    };
    let (kek, kek_id, previous) = (
        concat!("NORN_", "VAULT_KEK"),
        concat!("NORN_", "VAULT_KEK_ID"),
        concat!("NORN_", "VAULT_KEKS_PREVIOUS"),
    );
    let ring = LocalKeyring::from_vars(
        &vars(&[
            (kek, &key),
            (kek_id, "prod-eu-v2"),
            (previous, &format!("prod-eu-v1={old}")),
        ]),
        false,
    )
    .expect("config")
    .expect("keyring");
    assert_eq!(ring.current(), "prod-eu-v2");
    assert_eq!(ring.ids(), ["prod-eu-v1", "prod-eu-v2"]);
    let printed = format!("{ring:?}");
    assert!(
        !printed.contains(&key) && !printed.contains("9, 9, 9"),
        "{printed}"
    );
    assert!(
        LocalKeyring::from_vars(&vars(&[]), false)
            .expect("off")
            .is_none()
    );
    assert!(LocalKeyring::from_vars(&vars(&[(kek, &key)]), false).is_err());
    assert!(LocalKeyring::from_vars(&vars(&[(kek, "dev-insecure")]), false).is_err());
    assert!(
        LocalKeyring::from_vars(&vars(&[(kek, "dev-insecure")]), true)
            .expect("dev")
            .is_some()
    );
    let error = LocalKeyring::from_vars(
        &vars(&[(kek, "bm90LWEta2V5"), (kek_id, "prod-eu-v1")]),
        false,
    )
    .expect_err("short key");
    assert!(!error.to_string().contains("bm90LWEta2V5"), "{error}");
    assert_eq!(
        format!("{:?}", Secret::from(ACCESS.to_owned())),
        "[redacted]"
    );
}

// ----------------------------------------------------------------- egress

/// Answers public first, then loopback: a rebinding name.
#[derive(Debug, Default)]
struct Rebinding(AtomicUsize);

impl Lookup for Rebinding {
    fn lookup(&self, _host: &str, _port: u16) -> std::io::Result<Vec<IpAddr>> {
        let call = self.0.fetch_add(1, Ordering::SeqCst);
        Ok(vec![if call == 0 {
            IpAddr::from([192, 0, 2, 10])
        } else {
            IpAddr::from([127, 0, 0, 1])
        }])
    }
}

fn lookup_of(lookup: &Arc<Rebinding>) -> Arc<dyn Lookup> {
    Arc::clone(lookup)
}

fn post(url: &str) -> http::Request<Vec<u8>> {
    http::Request::post(url)
        .body(b"x".to_vec())
        .expect("request")
}

#[test]
fn dns_rebinding_is_refused_and_never_reaches_the_internal_address() {
    let internal = Mock::start(Arc::new(|_: &Seen| {
        (200, Vec::new(), "internal".to_owned())
    }));
    let lookup = Arc::new(Rebinding::default());
    let egress = Egress::with_connect_timeout(
        Vec::new(),
        None,
        lookup_of(&lookup),
        Duration::from_millis(300),
    )
    .expect("egress")
    .with_sleep(|_| {});
    let url = format!("https://rebind.example.com:{}/x", internal.port);
    // First answer public: connected to that pinned address only (it does
    // not answer), never re-resolved to the second, internal answer.
    let first = egress.send(post(&url));
    assert!(matches!(first, Err(EgressError::Transport(_))), "{first:?}");
    assert_eq!(lookup.0.load(Ordering::SeqCst), 1);
    // The next resolution answers loopback: refused after resolution.
    let second = egress.send(post(&url));
    assert!(
        matches!(&second, Err(EgressError::Forbidden(why)) if why.contains("127.0.0.1")),
        "{second:?}"
    );
    assert!(
        internal.seen().is_empty(),
        "the internal address was reached"
    );
}

#[test]
fn internal_and_plain_http_destinations_are_refused_before_any_lookup() {
    let lookup = Arc::new(Rebinding::default());
    let egress = Egress::new(Vec::new(), None, lookup_of(&lookup)).expect("egress");
    for url in [
        "https://169.254.169.254/latest/meta-data",
        "https://10.0.0.1/",
        "https://100.64.0.1/",
        "https://[fd00::1]/",
        "https://[::ffff:127.0.0.1]/",
        "https://localhost/",
        "https://metadata/",
        "http://api.example.com/",
        "https://user:pw@api.example.com/",
    ] {
        let refused = egress.send(post(url));
        assert!(
            matches!(refused, Err(EgressError::Forbidden(_))),
            "{url}: {refused:?}"
        );
    }
    assert_eq!(lookup.0.load(Ordering::SeqCst), 0);
    assert!(Egress::new(Vec::new(), Some("not a proxy"), Arc::new(NoLookup)).is_err());
}

#[test]
fn a_cross_origin_redirect_drops_credentials_and_a_same_origin_one_keeps_them() {
    let target = Mock::start(Arc::new(|_: &Seen| (200, Vec::new(), "landed".to_owned())));
    let target_port = target.port;
    let origin = Mock::start(Arc::new(move |seen: &Seen| {
        if seen.line.contains("/away") {
            (
                302,
                vec![(
                    "location".to_owned(),
                    format!("http://127.0.0.1:{target_port}/landing"),
                )],
                String::new(),
            )
        } else if seen.line.contains("/here") {
            (
                307,
                vec![("location".to_owned(), "/after".to_owned())],
                String::new(),
            )
        } else if seen.line.contains("/internal") {
            (
                302,
                vec![("location".to_owned(), "http://10.1.2.3/".to_owned())],
                String::new(),
            )
        } else if seen.line.contains("/see-other") {
            (
                303,
                vec![(
                    "location".to_owned(),
                    format!("http://127.0.0.1:{target_port}/got"),
                )],
                String::new(),
            )
        } else {
            (200, Vec::new(), "same".to_owned())
        }
    }));
    let egress = loopback_egress();
    let request = |method: &str, path: &str| {
        http::Request::builder()
            .method(method)
            .uri(format!("http://127.0.0.1:{}{path}", origin.port))
            .header("authorization", format!("Bearer {ACCESS}"))
            .header("x-shopify-access-token", REFRESH)
            .header("cookie", "session=SECRET")
            .header("accept", "application/json")
            .header("content-type", "application/json")
            .header("x-request-id", "req-1")
            .body(if method == "POST" {
                b"{}".to_vec()
            } else {
                Vec::new()
            })
            .expect("request")
    };
    let response = egress.send(request("GET", "/away")).expect("followed");
    assert_eq!(response.body(), b"landed");
    let landed = target.seen().pop().expect("landed");
    for dropped in ["authorization", "x-shopify-access-token", "cookie"] {
        assert!(
            !landed.headers.contains_key(dropped),
            "{dropped} crossed origins"
        );
    }
    assert_eq!(
        landed.headers.get("accept").map(String::as_str),
        Some("application/json")
    );
    assert_eq!(
        landed.headers.get("x-request-id").map(String::as_str),
        Some("req-1")
    );

    egress.send(request("GET", "/here")).expect("followed");
    let after = origin.seen().pop().expect("after");
    assert!(after.line.contains("/after"));
    assert_eq!(
        after.headers.get("authorization").map(String::as_str),
        Some(format!("Bearer {ACCESS}").as_str())
    );

    // 303 after a POST is a body-less GET, credentials dropped across origins.
    egress
        .send(request("POST", "/see-other"))
        .expect("followed");
    let got = target.seen().pop().expect("got");
    assert!(got.line.starts_with("GET /got"), "{}", got.line);
    assert!(got.body.is_empty() && !got.headers.contains_key("content-type"));
    assert!(!got.headers.contains_key("authorization"));

    // A redirect to an internal host is refused like a first request.
    let refused = egress.send(request("GET", "/internal"));
    assert!(
        matches!(refused, Err(EgressError::Forbidden(_))),
        "{refused:?}"
    );
}

// ---------------------------------------------------------------- refresh

#[test]
fn concurrent_refreshes_make_exactly_one_token_endpoint_call() {
    let (_dir, path) = temp_db();
    let calls = Arc::new(AtomicUsize::new(0));
    let server = token_server(Arc::clone(&calls), Duration::from_millis(200));
    let conns = connections(&path, server.port);
    set(&conns, Some(2_000)); // expires at once
    let tokens: Vec<_> = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..16)
            .map(|_| {
                scope.spawn(|| conns.access_token("connection:acme.main", "acme.main", 10_000))
            })
            .collect();
        handles
            .into_iter()
            .map(|h| h.join().expect("thread"))
            .collect()
    });
    assert_eq!(calls.load(Ordering::SeqCst), 1, "token endpoint calls");
    for token in tokens {
        assert_eq!(token.expect("token").expose(), NEW_ACCESS.as_bytes());
    }
    let refresh = server
        .seen()
        .into_iter()
        .find(|s| s.line.starts_with("POST /token"))
        .expect("call");
    assert!(
        refresh.body.contains("grant_type=refresh_token"),
        "{}",
        refresh.body
    );
    assert!(refresh.body.contains(REFRESH));
    let trail = conns.audit_trail("acme.main").expect("audit");
    assert_eq!(
        trail
            .iter()
            .filter(|(_, event, ..)| event == "credential.refresh")
            .count(),
        1
    );
    assert_eq!(
        trail
            .iter()
            .filter(|(_, event, ..)| event == "credential.use")
            .count(),
        16
    );
    // The rotated refresh token was saved: the next refresh sends it.
    let status = conns.status("acme.main").expect("status");
    assert_eq!(status.last4.as_deref(), Some("abcd"));
    conns
        .access_token("connection:acme.main", "acme.main", 10_000 + 3_600_000)
        .expect("again");
    let second = server
        .seen()
        .into_iter()
        .filter(|s| s.line.starts_with("POST /token"))
        .next_back()
        .expect("second");
    assert!(second.body.contains(NEW_REFRESH), "{}", second.body);
}

#[test]
fn a_refused_refresh_marks_the_connection_reconnect_needed() {
    let (_dir, path) = temp_db();
    let server = Mock::start(Arc::new(|_: &Seen| {
        (
            400,
            vec![("content-type".to_owned(), "application/json".to_owned())],
            r#"{"error":"invalid_grant","error_description":"revoked"}"#.to_owned(),
        )
    }));
    let conns = connections(&path, server.port);
    set(&conns, Some(2_000));
    assert_eq!(
        conns
            .access_token("connection:acme.main", "acme.main", 10_000)
            .err(),
        Some(ConnError::ReconnectNeeded)
    );
    let status = conns.status("acme.main").expect("status");
    assert_eq!(status.state, "reconnect_needed");
    assert_eq!(status.last_error.as_deref(), Some("invalid_grant"));
    // Until connected again, nothing uses it and nothing calls the provider.
    let before = server.seen().len();
    assert_eq!(
        conns
            .access_token("connection:acme.main", "acme.main", 20_000)
            .err(),
        Some(ConnError::ReconnectNeeded)
    );
    assert_eq!(server.seen().len(), before);
}

// ------------------------------------------------------------------ oauth

fn query_param(url: &str, name: &str) -> String {
    url::Url::parse(url)
        .expect("url")
        .query_pairs()
        .find(|(key, _)| key == name)
        .map_or_else(
            || panic!("{name} missing from {url}"),
            |(_, value)| value.into_owned(),
        )
}

#[test]
fn oauth_state_is_single_use_bound_to_its_principal_and_expires() {
    let (_dir, path) = temp_db();
    let calls = Arc::new(AtomicUsize::new(0));
    let server = token_server(Arc::clone(&calls), Duration::ZERO);
    let conns = connections(&path, server.port);

    let (url, expires) = conns
        .start("owner", "acme.main", "acme", None, &BTreeMap::new(), 1_000)
        .expect("start");
    assert_eq!(expires, 1_000 + 600_000);
    assert_eq!(query_param(&url, "code_challenge_method"), "S256");
    let challenge = query_param(&url, "code_challenge");
    let state = query_param(&url, "state");

    // Another principal cannot complete it; the state is then spent.
    assert_eq!(
        conns.callback("intruder", &state, "code-1", 2_000).err(),
        Some(ConnError::BadState)
    );
    assert_eq!(
        conns.callback("owner", &state, "code-1", 2_000).err(),
        Some(ConnError::BadState)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    // Expired after ten minutes.
    let (url, _) = conns
        .start("owner", "acme.main", "acme", None, &BTreeMap::new(), 1_000)
        .expect("start");
    let late = query_param(&url, "state");
    assert_eq!(
        conns
            .callback("owner", &late, "code-2", 1_000 + 600_000)
            .err(),
        Some(ConnError::BadState)
    );

    // The right principal in time: exchanged once with the matching verifier.
    let (url, _) = conns
        .start(
            "owner",
            "acme.main",
            "acme",
            Some(vec!["read".to_owned(), "write".to_owned()]),
            &BTreeMap::new(),
            5_000,
        )
        .expect("start");
    let (good, challenge2) = (
        query_param(&url, "state"),
        query_param(&url, "code_challenge"),
    );
    assert_ne!(challenge, challenge2);
    let status = conns
        .callback("owner", &good, "code-3", 6_000)
        .expect("connected");
    assert_eq!(status.state, "connected");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let exchange = server
        .seen()
        .into_iter()
        .find(|s| s.line.starts_with("POST /token"))
        .expect("exchange");
    let verifier = exchange
        .body
        .split('&')
        .find_map(|pair| pair.strip_prefix("code_verifier="))
        .expect("verifier");
    let derived = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(sha2::Sha256::digest(verifier.as_bytes()));
    assert_eq!(derived, challenge2);
    assert_eq!(
        conns.callback("owner", &good, "code-3", 6_000).err(),
        Some(ConnError::BadState)
    );
    // Unoffered scopes are refused before anything is stored.
    assert!(matches!(
        conns.start(
            "owner",
            "acme.main",
            "acme",
            Some(vec!["admin".to_owned()]),
            &BTreeMap::new(),
            7_000
        ),
        Err(ConnError::Invalid(_))
    ));
}

// ------------------------------------------------------- routes and leaks

/// `owner` manages connections; nobody else does.
struct OwnerOnly;

impl Authorizer for OwnerOnly {
    fn authorize(&self, request: &AccessRequest<'_>) -> Result<(), Refusal> {
        match (&request.operation, request.actor) {
            (Operation::ManageConnection(_), Some("owner")) => Ok(()),
            _ => Err(norn_api::authz::read_denied()),
        }
    }
}

struct Bearers;

impl PrincipalResolver for Bearers {
    fn resolve(&self, authorization: Option<&str>, _now: u64) -> Option<String> {
        match authorization? {
            "Bearer owner-token" => Some("owner".to_owned()),
            "Bearer viewer-token" => Some("viewer".to_owned()),
            _ => None,
        }
    }
    fn end_request(&self) {}
}

#[test]
fn the_write_only_api_is_owner_only_and_never_returns_or_stores_a_credential() {
    let (_dir, path) = temp_db();
    let calls = Arc::new(AtomicUsize::new(0));
    let server = token_server(Arc::clone(&calls), Duration::ZERO);
    let store = CellStore::from_engine(
        engine(&path),
        Model::from_definitions(&[], &[]).expect("model"),
    )
    .expect("store");
    let host = DevHost::from_store(
        store,
        CellScope {
            org_id: "org_demo".into(),
            environment_id: "dev".into(),
        },
        |_| BTreeMap::new(),
    )
    .expect("host")
    .with_authorizer(OwnerOnly);
    let conns = connections(&path, server.port);
    let body = format!(
        r#"{{"provider":"acme","accessToken":"{ACCESS}","refreshToken":"{REFRESH}","scopes":["read"]}}"#
    );
    let mut outputs = Vec::new();
    let mut call = |method: &str, path: &str, query: &str, who: Option<&str>, body: &str| {
        let auth = who.map(|token| format!("Bearer {token}"));
        let response = conns
            .route(
                &host,
                &Bearers,
                &Request {
                    method,
                    path,
                    query,
                    authorization: auth.as_deref(),
                    now: 1,
                    body,
                },
                1_000,
            )
            .expect("routed");
        outputs.push(response.body.clone());
        response
    };
    let scope = "orgId=org_demo&environmentId=dev";
    let path_of = "/v1/connections/acme.main/credentials";
    let outside = norn_cell_do::server::outside::<NativeEngine>(&Request {
        method: "PUT",
        path: path_of,
        query: scope,
        authorization: None,
        now: 1,
        body: "",
    });
    // CONN-AUTHZ-1: a viewer, an anonymous caller, another scope: the outside answer.
    for (who, query) in [
        (Some("viewer-token"), scope),
        (None, scope),
        (Some("owner-token"), "orgId=org_x&environmentId=dev"),
    ] {
        let refused = call("PUT", path_of, query, who, &body);
        assert_eq!(
            (refused.status, &refused.body),
            (outside.status, &outside.body)
        );
    }
    assert!(
        conns
            .status("acme.main")
            .expect("status")
            .provider
            .is_none()
    );

    let set = call("PUT", path_of, scope, Some("owner-token"), &body);
    assert_eq!(set.status, 200, "{}", set.body);
    let status: serde_json::Value = serde_json::from_str(&set.body).expect("json");
    assert_eq!(status["state"], "connected");
    assert_eq!(status["last4"], "wxyz");
    let read = call("GET", path_of, scope, Some("owner-token"), "");
    assert_eq!(read.status, 200);
    let started = call(
        "POST",
        "/v1/connections/acme.main/oauth:start",
        scope,
        Some("owner-token"),
        r#"{"provider":"acme"}"#,
    );
    assert_eq!(started.status, 200, "{}", started.body);
    let viewer_read = call("GET", path_of, scope, Some("viewer-token"), "");
    assert_eq!(viewer_read.status, outside.status);
    let bad = call(
        "PUT",
        path_of,
        scope,
        Some("owner-token"),
        r#"{"provider":"acme","accessToken":"x","unknown":1}"#,
    );
    assert_eq!(bad.status, 400);
    // The ciphertext is in the cell's storage; the plaintext is not.
    let bytes = std::fs::read(&path).expect("file");
    for secret in [ACCESS, REFRESH] {
        assert!(
            !bytes.windows(secret.len()).any(|w| w == secret.as_bytes()),
            "{secret} stored in clear"
        );
    }
    let revoked = call("DELETE", path_of, scope, Some("owner-token"), "");
    assert_eq!(revoked.status, 200);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&revoked.body).expect("json")["state"],
        "not_connected"
    );
    // The provider's revocation endpoint is asked over https only; this
    // mock is plain http, so the revoke is refused before any call, and audited.
    assert!(
        !server
            .seen()
            .iter()
            .any(|s| s.line.starts_with("POST /revoke"))
    );
    // No answer, audit row or printed status carries a credential.
    let trail = conns.audit_trail("acme.main").expect("audit");
    let events: Vec<&str> = trail.iter().map(|(_, event, ..)| event.as_str()).collect();
    assert_eq!(
        events,
        ["credential.set", "oauth.start", "credential.revoke"]
    );
    let printed = format!("{outputs:?}{trail:?}{:?}", conns.status("acme.main"));
    for secret in [ACCESS, REFRESH, "client-secret-SECRET-0005", "SECRET"] {
        assert!(!printed.contains(secret), "{secret} leaked: {printed}");
    }
}

#[test]
fn the_generated_providers_load_as_connector_auth() {
    let providers = providers::load().expect("providers");
    assert_eq!(
        providers.keys().map(String::as_str).collect::<Vec<_>>(),
        ["github", "google.gmail", "shopify"]
    );
    let shopify = &providers["shopify"];
    assert_eq!(shopify.scope_separator, ",");
    assert_eq!(
        shopify.fill(
            &shopify.token_url,
            &BTreeMap::from([("shopSubdomain".to_owned(), "acme".to_owned())])
        ),
        Ok("https://acme.myshopify.com/admin/oauth/access_token".to_owned())
    );
    for evil in ["evil.com/x", "a@b", "", "UPPER", "-x"] {
        assert!(
            shopify
                .fill(
                    &shopify.token_url,
                    &BTreeMap::from([("shopSubdomain".to_owned(), evil.to_owned())])
                )
                .is_err(),
            "{evil}"
        );
    }
    let google = &providers["google.gmail"];
    assert_eq!(
        google.revocation_url.as_deref(),
        Some("https://oauth2.googleapis.com/revoke")
    );
    assert_eq!(
        google
            .authorization_params
            .get("access_type")
            .map(String::as_str),
        Some("offline")
    );
    assert_eq!(providers["github"].scopes, ["read:user", "repo"]);
}
