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
use std::time::{Duration, Instant};

use base64::Engine as _;
use norn_api::authz::{AccessRequest, Authorizer, Operation};
use norn_api::host::DevHost;
use norn_api::refusal::Refusal;
use norn_api::scope::CellScope;
use norn_cell::engine::{NativeEngine, SqlEngine as _, SqlValue};
use norn_cell::{CellStore, Model, StoreOptions};
use norn_cell_do::connections::egress::{Egress, EgressError, Lookup, Timeouts};
use norn_cell_do::connections::providers::{self, ClientAuth, Provider};
use norn_cell_do::connections::service::{
    ClientCredentials, ConnError, Connections, SetCredentials, Started,
};
use norn_cell_do::connections::vault::{KeyId, KeyWrap, LocalKeyring, Secret, Vault, VaultError};
use norn_cell_do::connections::{Config, Routes};
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
    Egress::new(vec!["127.0.0.1".to_owned()], Arc::new(NoLookup)).with_sleep(|_| {})
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
        withheld: None,
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
    token_server_with(count, delay, Duration::ZERO)
}

/// A token endpoint answering after `delay`, and a revocation endpoint
/// answering after `revoke_delay`.
fn token_server_with(count: Arc<AtomicUsize>, delay: Duration, revoke_delay: Duration) -> Mock {
    Mock::start(Arc::new(move |seen: &Seen| {
        if seen.line.starts_with("POST /revoke") {
            std::thread::sleep(revoke_delay);
            (200, Vec::new(), String::new())
        } else if seen.line.starts_with("POST /token") {
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
        "the keyring printed key bytes"
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
    assert!(
        !error.to_string().contains("bm90LWEta2V5"),
        "a configuration error carries the key value"
    );
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
    let shared: Arc<Rebinding> = Arc::clone(lookup);
    shared
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
    let egress = Egress::with_timeouts(
        Vec::new(),
        lookup_of(&lookup),
        Timeouts {
            connect: Duration::from_millis(300),
            ..Timeouts::default()
        },
    )
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
    assert_eq!(internal.seen().len(), 0, "the internal address was reached");
}

#[test]
fn internal_and_plain_http_destinations_are_refused_before_any_lookup() {
    let lookup = Arc::new(Rebinding::default());
    let egress = Egress::new(Vec::new(), lookup_of(&lookup));
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
        .rfind(|s| s.line.starts_with("POST /token"))
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

#[test]
fn a_transient_token_endpoint_failure_does_not_mark_reconnect_needed() {
    let (_dir, path) = temp_db();
    let attempts = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&attempts);
    let server = Mock::start(Arc::new(move |_: &Seen| {
        if counter.fetch_add(1, Ordering::SeqCst) == 0 {
            (
                502,
                vec![("content-type".to_owned(), "text/html".to_owned())],
                "<html>bad gateway</html>".to_owned(),
            )
        } else {
            (
                200,
                vec![("content-type".to_owned(), "application/json".to_owned())],
                format!(
                    r#"{{"access_token":"{NEW_ACCESS}","token_type":"Bearer","expires_in":3600,"refresh_token":"{NEW_REFRESH}"}}"#
                ),
            )
        }
    }));
    let conns = connections(&path, server.port);
    set(&conns, Some(2_000));
    assert_eq!(
        conns
            .access_token("connection:acme.main", "acme.main", 10_000)
            .err(),
        Some(ConnError::Provider("unreadable_token_response"))
    );
    let after_transient = conns.status("acme.main").expect("status");
    assert_eq!(after_transient.state, "connected");
    assert_eq!(
        after_transient.last_error.as_deref(),
        Some("unreadable_token_response")
    );
    let recovered = conns
        .access_token("connection:acme.main", "acme.main", 20_000)
        .expect("refresh recovers after transient 502");
    assert_eq!(recovered.expose(), NEW_ACCESS.as_bytes());
    let after_recovery = conns.status("acme.main").expect("status");
    assert_eq!(after_recovery.state, "connected");
    assert_eq!(after_recovery.last_error, None);
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

// ------------------------------------------------------------------ oauth

fn query_param(url: &str, name: &str) -> String {
    url::Url::parse(url)
        .expect("url")
        .query_pairs()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.into_owned())
        .expect("the parameter is in the URL")
}

fn code(text: &str) -> Secret {
    Secret::from(text.to_owned())
}

/// A start's URL, expiry and completion key.
fn parts(started: &Started) -> (String, u64, String) {
    (
        started.authorize_url.clone(),
        started.expires_at_ms,
        started
            .completion_key
            .expose_str()
            .expect("text")
            .to_owned(),
    )
}

#[test]
fn oauth_state_is_single_use_bound_to_its_principal_and_expires() {
    let (_dir, path) = temp_db();
    let calls = Arc::new(AtomicUsize::new(0));
    let server = token_server(Arc::clone(&calls), Duration::ZERO);
    let conns = connections(&path, server.port);

    let (url, expires, key) = parts(
        &conns
            .start("owner", "acme.main", "acme", None, &BTreeMap::new(), 1_000)
            .expect("start"),
    );
    assert_eq!(expires, 1_000 + 600_000);
    assert_eq!(query_param(&url, "code_challenge_method"), "S256");
    let challenge = query_param(&url, "code_challenge");
    let state = query_param(&url, "state");

    // S5b, OAUTH-SESSION-1: another principal, another session (a wrong
    // key) or another connection can't complete it, and spends nothing.
    for (actor, connection, presented) in [
        ("intruder", "acme.main", key.as_str()),
        ("owner", "acme.main", "another-sessions-key"),
        ("owner", "acme.other", key.as_str()),
    ] {
        assert_eq!(
            conns
                .complete(actor, connection, &state, presented, &code("code-1"), 2_000)
                .err(),
            Some(ConnError::BadState)
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let first = conns
        .complete("owner", "acme.main", &state, &key, &code("code-1"), 2_000)
        .expect("the starting session still completes");
    assert_eq!(first.state, "connected");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    // Spent: a replay is refused.
    assert_eq!(
        conns
            .complete("owner", "acme.main", &state, &key, &code("code-1"), 2_000)
            .err(),
        Some(ConnError::BadState)
    );

    // Expired after ten minutes.
    let (url, _, key) = parts(
        &conns
            .start("owner", "acme.main", "acme", None, &BTreeMap::new(), 1_000)
            .expect("start"),
    );
    let late = query_param(&url, "state");
    assert_eq!(
        conns
            .complete(
                "owner",
                "acme.main",
                &late,
                &key,
                &code("code-2"),
                1_000 + 600_000
            )
            .err(),
        Some(ConnError::BadState)
    );

    // The right principal in time: exchanged once with the matching verifier.
    let (url, _, key) = parts(
        &conns
            .start(
                "owner",
                "acme.main",
                "acme",
                Some(vec!["read".to_owned(), "write".to_owned()]),
                &BTreeMap::new(),
                5_000,
            )
            .expect("start"),
    );
    let (good, challenge2) = (
        query_param(&url, "state"),
        query_param(&url, "code_challenge"),
    );
    assert_ne!(challenge, challenge2);
    let status = conns
        .complete("owner", "acme.main", &good, &key, &code("code-3"), 6_000)
        .expect("connected");
    assert_eq!(status.state, "connected");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let exchange = server
        .seen()
        .into_iter()
        .rfind(|s| s.line.starts_with("POST /token"))
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
        conns
            .complete("owner", "acme.main", &good, &key, &code("code-3"), 6_000)
            .err(),
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
            (
                Operation::ManageConnection(_) | Operation::RevokeConnectionGrant(_),
                Some("owner"),
            ) => Ok(()),
            // Manages one connection, without authority over its grant.
            (Operation::ManageConnection(id), Some("manager")) if id == "acme.main" => Ok(()),
            _ => Err(norn_api::authz::read_denied()),
        }
    }
}

struct Bearers;

impl PrincipalResolver for Bearers {
    fn resolve(&self, authorization: Option<&str>, _now: u64) -> Option<String> {
        match authorization? {
            "Bearer owner-token" | "Bearer owner-token-2" => Some("owner".to_owned()),
            "Bearer viewer-token" => Some("viewer".to_owned()),
            "Bearer manager-token" => Some("manager".to_owned()),
            _ => None,
        }
    }
    fn end_request(&self) {}
}

fn owner_host(path: &std::path::Path) -> DevHost<NativeEngine> {
    let store = CellStore::from_engine(
        engine(path),
        Model::from_definitions(&[], &[]).expect("model"),
    )
    .expect("store");
    DevHost::from_store(
        store,
        CellScope {
            org_id: "org_demo".into(),
            environment_id: "dev".into(),
        },
        |_| BTreeMap::new(),
    )
    .expect("host")
    .with_authorizer(OwnerOnly)
}

/// Routes over [`connections`], counting how often the vault is opened.
fn routes(path: &std::path::Path, port: u16, opens: &Arc<AtomicUsize>) -> Routes<NativeEngine> {
    let (path, opens) = (path.to_owned(), Arc::clone(opens));
    Routes::with_opener(move |_scope| {
        opens.fetch_add(1, Ordering::SeqCst);
        Ok(connections(&path, port))
    })
}

fn request<'a>(
    method: &'a str,
    path: &'a str,
    query: &'a str,
    authorization: Option<&'a str>,
    body: &'a str,
) -> Request<'a> {
    Request {
        method,
        path,
        query,
        authorization,
        now: 1,
        body,
    }
}

#[test]
fn the_write_only_api_is_owner_only_and_never_returns_or_stores_a_credential() {
    let (_dir, path) = temp_db();
    let calls = Arc::new(AtomicUsize::new(0));
    let server = token_server(Arc::clone(&calls), Duration::ZERO);
    let host = owner_host(&path);
    let opens = Arc::new(AtomicUsize::new(0));
    let routes = routes(&path, server.port, &opens);
    let body = format!(
        r#"{{"provider":"acme","accessToken":"{ACCESS}","refreshToken":"{REFRESH}","scopes":["read"]}}"#
    );
    let mut outputs = Vec::new();
    let mut call = |method: &str, path: &str, query: &str, who: Option<&str>, body: &str| {
        let auth = who.map(|token| format!("Bearer {token}"));
        let response = routes
            .route(
                &host,
                &Bearers,
                &request(method, path, query, auth.as_deref(), body),
                1_000,
            )
            .expect("routed");
        outputs.push(response.body.clone());
        response
    };
    let scope = "orgId=org_demo&environmentId=dev";
    let path_of = "/v1/connections/acme.main/credentials";
    let outside =
        norn_cell_do::server::outside::<NativeEngine>(&request("PUT", path_of, scope, None, ""));
    // CONN-AUTHZ-1: a viewer, an anonymous caller, another scope: the
    // outside answer, and the vault is never opened.
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
    assert_eq!(
        opens.load(Ordering::SeqCst),
        0,
        "the vault opened for a denied caller"
    );

    let set = call("PUT", path_of, scope, Some("owner-token"), &body);
    assert_eq!(set.status, 200, "{}", set.body);
    assert_eq!(opens.load(Ordering::SeqCst), 1);
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
    for (label, secret) in [("access token", ACCESS), ("refresh token", REFRESH)] {
        assert!(
            !bytes.windows(secret.len()).any(|w| w == secret.as_bytes()),
            "the {label} is stored in clear"
        );
    }
    let revoked = call(
        "DELETE",
        path_of,
        &format!("{scope}&revokeGrant=true"),
        Some("owner-token"),
        "",
    );
    assert_eq!(revoked.status, 200);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&revoked.body).expect("json")["state"],
        "not_connected"
    );
    // The provider's revocation endpoint got the refresh token, once.
    let revocation: Vec<Seen> = server
        .seen()
        .into_iter()
        .filter(|s| s.line.starts_with("POST /revoke"))
        .collect();
    assert_eq!(revocation.len(), 1);
    assert!(revocation[0].body.contains("token_type_hint=refresh_token"));
    // No answer, audit row or printed status carries a credential.
    let conns = routes.connections().expect("opened");
    let trail = conns.audit_trail("acme.main").expect("audit");
    let events: Vec<&str> = trail.iter().map(|(_, event, ..)| event.as_str()).collect();
    assert_eq!(
        events,
        [
            "credential.set",
            "oauth.start",
            "credential.revoke",
            "credential.provider_revoke"
        ]
    );
    assert_eq!(trail[3].3.as_deref(), Some("provider_revoked"));
    let printed = format!("{outputs:?}{trail:?}{:?}", conns.status("acme.main"));
    for (label, secret) in [ACCESS, REFRESH, "client-secret-SECRET-0005", "SECRET"]
        .iter()
        .enumerate()
    {
        assert!(!printed.contains(secret), "secret fixture {label} leaked");
    }
}

#[test]
fn the_generated_providers_load_as_connector_auth_and_shopify_is_withheld() {
    let providers = providers::load().expect("providers");
    assert_eq!(
        providers.keys().map(String::as_str).collect::<Vec<_>>(),
        ["github", "google.gmail", "shopify"]
    );
    let shopify = &providers["shopify"];
    assert!(
        shopify
            .withheld
            .as_deref()
            .is_some_and(|why| why.contains("not supported yet"))
    );
    assert!(providers["github"].withheld.is_none() && providers["google.gmail"].withheld.is_none());
    assert_eq!(shopify.scope_separator, ",");
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

/// PROVIDER-WITHHELD-1: Shopify is refused before anything is stored or sent.
#[test]
fn a_withheld_provider_is_refused_everywhere() {
    let (_dir, path) = temp_db();
    let mut registry = providers::load().expect("providers");
    let shopify = registry.remove("shopify").expect("shopify");
    let vault = Vault::open(
        engine(&path),
        SCOPE,
        Arc::new(keyring("test-local-v1", &[("test-local-v1", 7)])),
    )
    .expect("vault");
    let conns = Connections::new(
        vault,
        loopback_egress(),
        BTreeMap::from([("shopify".to_owned(), shopify)]),
        BTreeMap::from([(
            "shopify".to_owned(),
            ClientCredentials {
                client_id: "client-1".to_owned(),
                client_secret: Some(Secret::from("s".to_owned())),
            },
        )]),
        Some("https://app.norn.example/v1/connections/oauth:callback".to_owned()),
    )
    .expect("connections");
    let params = BTreeMap::from([("shopSubdomain".to_owned(), "acme".to_owned())]);
    let started = conns.start("owner", "shop", "shopify", None, &params, 1_000);
    assert!(
        matches!(&started, Err(ConnError::Unsupported(why)) if why.contains("not supported yet")),
        "{started:?}"
    );
    let set = conns.set(
        "owner",
        "shop",
        serde_json::from_value(serde_json::json!({
            "provider": "shopify", "accessToken": ACCESS, "params": {"shopSubdomain": "acme"}
        }))
        .expect("body"),
        1_000,
    );
    assert!(matches!(set, Err(ConnError::Unsupported(_))), "{set:?}");
    assert_eq!(conns.status("shop").expect("status").state, "not_connected");
}

// ------------------------------------------------------ round-1 findings

/// S1, OAUTH-DIRECT-1: token exchange, refresh and revocation follow no
/// redirect; nothing reaches the redirect's target.
#[test]
fn oauth_calls_follow_no_redirect() {
    let (_dir, path) = temp_db();
    let thief = Mock::start(Arc::new(|_: &Seen| (200, Vec::new(), "{}".to_owned())));
    let thief_port = thief.port;
    let server = Mock::start(Arc::new(move |_: &Seen| {
        (
            307,
            vec![(
                "location".to_owned(),
                format!("http://127.0.0.1:{thief_port}/steal"),
            )],
            String::new(),
        )
    }));
    let conns = connections(&path, server.port);
    // Exchange.
    let (url, _, key) = parts(
        &conns
            .start("owner", "acme.main", "acme", None, &BTreeMap::new(), 1_000)
            .expect("start"),
    );
    let exchanged = conns.complete(
        "owner",
        "acme.main",
        &query_param(&url, "state"),
        &key,
        &code("code-1"),
        2_000,
    );
    assert!(
        matches!(exchanged, Err(ConnError::Egress(EgressError::Redirect(_)))),
        "{exchanged:?}"
    );
    // Refresh.
    set(&conns, Some(2_000));
    let refreshed = conns.access_token("connection:acme.main", "acme.main", 10_000);
    assert!(
        matches!(refreshed, Err(ConnError::Egress(EgressError::Redirect(_)))),
        "{refreshed:?}"
    );
    assert_eq!(
        conns
            .status("acme.main")
            .expect("status")
            .last_error
            .as_deref(),
        Some("egress_redirect")
    );
    // Revocation.
    conns
        .revoke("owner", "acme.main", true, 20_000)
        .expect("revoke");
    let trail = conns.audit_trail("acme.main").expect("audit");
    assert_eq!(
        trail
            .last()
            .map(|(_, event, _, detail)| (event.as_str(), detail.as_deref())),
        Some(("credential.provider_revoke", Some("egress_redirect")))
    );
    assert_eq!(thief.seen().len(), 0, "a redirect target got an OAuth body");
    assert_eq!(server.seen().len(), 3);
}

/// S1, EGRESS-REDIRECT-1: a 307 that would resend a body to another origin
/// is refused before anything reaches it.
#[test]
fn a_body_is_never_resent_to_another_origin() {
    let thief = Mock::start(Arc::new(|_: &Seen| (200, Vec::new(), String::new())));
    let thief_port = thief.port;
    let origin = Mock::start(Arc::new(move |_: &Seen| {
        (
            308,
            vec![(
                "location".to_owned(),
                format!("http://127.0.0.1:{thief_port}/x"),
            )],
            String::new(),
        )
    }));
    let refused = loopback_egress().send(post(&format!("http://127.0.0.1:{}/a", origin.port)));
    assert!(
        matches!(refused, Err(EgressError::Redirect(_))),
        "{refused:?}"
    );
    assert_eq!(thief.seen().len(), 0);
}

/// Never answers in time.
#[derive(Debug, Default)]
struct SlowLookup;

impl Lookup for SlowLookup {
    fn lookup(&self, _host: &str, _port: u16) -> std::io::Result<Vec<IpAddr>> {
        std::thread::sleep(Duration::from_secs(5));
        Ok(vec![IpAddr::from([192, 0, 2, 10])])
    }
}

/// S2: a proxy is refused at startup; a lookup and a whole call are bounded.
#[test]
fn proxies_are_refused_and_lookups_and_calls_are_bounded() {
    let vars = BTreeMap::from([
        (
            concat!("NORN_", "VAULT_KEK").to_owned(),
            "dev-insecure".to_owned(),
        ),
        (
            "NORN_EGRESS_PROXY".to_owned(),
            "http://proxy.example:3128".to_owned(),
        ),
    ]);
    let refused = Config::from_vars(&vars, true).err().expect("refused");
    assert!(
        refused.contains("NORN_EGRESS_PROXY") && refused.contains("not supported"),
        "{refused}"
    );

    let quick = Timeouts {
        connect: Duration::from_millis(300),
        resolve: Duration::from_millis(200),
        total: Duration::from_millis(600),
    };
    let egress = Egress::with_timeouts(Vec::new(), Arc::new(SlowLookup), quick).with_sleep(|_| {});
    let started = Instant::now();
    let unresolved = egress.send(
        http::Request::get("https://slow.example.com/")
            .body(Vec::new())
            .expect("request"),
    );
    assert!(
        matches!(unresolved, Err(EgressError::Unresolved(_))),
        "{unresolved:?}"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );

    // A peer that never answers: the whole call, retries included, ends at
    // the deadline.
    let hung = Mock::start(Arc::new(|_: &Seen| {
        std::thread::sleep(Duration::from_secs(5));
        (200, Vec::new(), String::new())
    }));
    let egress = Egress::with_timeouts(vec!["127.0.0.1".to_owned()], Arc::new(NoLookup), quick)
        .with_sleep(|_| {});
    let started = Instant::now();
    let late = egress.send(
        http::Request::get(format!("http://127.0.0.1:{}/", hung.port))
            .body(Vec::new())
            .expect("request"),
    );
    assert!(matches!(late, Err(EgressError::Deadline)), "{late:?}");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
}

/// S3, CONN-ERROR-1: a provider's text never reaches an answer, the status
/// or the audit; the log facade can't record wire rows.
#[test]
fn provider_text_never_reaches_answers_status_or_audit() {
    assert!(log::STATIC_MAX_LEVEL <= log::LevelFilter::Info);
    let (_dir, path) = temp_db();
    let server = Mock::start(Arc::new(|_: &Seen| {
        (
            400,
            vec![("content-type".to_owned(), "application/json".to_owned())],
            r#"{"error":"<script>EVIL-CODE</script>","error_description":"PROVIDER-SECRET-TEXT"}"#
                .to_owned(),
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
    assert_eq!(status.last_error.as_deref(), Some("provider_error"));
    let (url, _, key) = parts(
        &conns
            .start("owner", "acme.main", "acme", None, &BTreeMap::new(), 20_000)
            .expect("start"),
    );
    let refused = conns
        .complete(
            "owner",
            "acme.main",
            &query_param(&url, "state"),
            &key,
            &code("c"),
            21_000,
        )
        .expect_err("refused");
    assert_eq!(refused, ConnError::Provider("provider_error"));
    let trail = conns.audit_trail("acme.main").expect("audit");
    let printed = format!("{status:?}{trail:?}{refused}{refused:?}");
    for (index, text) in ["EVIL", "script", "PROVIDER-SECRET-TEXT"]
        .iter()
        .enumerate()
    {
        assert!(!printed.contains(text), "provider text {index} leaked");
    }
}

/// S4: a refresh in flight when the connection is disconnected and set
/// again never overwrites the replacement.
#[test]
fn a_stale_refresh_never_overwrites_a_replacement() {
    let (_dir, path) = temp_db();
    let calls = Arc::new(AtomicUsize::new(0));
    let server = token_server(Arc::clone(&calls), Duration::from_millis(600));
    let conns = connections(&path, server.port);
    set(&conns, Some(2_000));
    let replacement = "access-token-REPLACED-0009-rplc";
    let stale = std::thread::scope(|scope| {
        let refreshing =
            scope.spawn(|| conns.access_token("connection:acme.main", "acme.main", 10_000));
        std::thread::sleep(Duration::from_millis(200));
        conns
            .revoke("owner", "acme.main", false, 10_100)
            .expect("revoke");
        conns
            .set(
                "owner",
                "acme.main",
                SetCredentials::from_parts("acme", replacement, None, None),
                10_200,
            )
            .expect("replace");
        refreshing.join().expect("thread")
    });
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(stale.err(), Some(ConnError::Conflict));
    assert_eq!(
        conns.status("acme.main").expect("status").last4.as_deref(),
        Some("rplc")
    );
    assert_eq!(
        conns
            .access_token("connection:acme.main", "acme.main", 20_000)
            .expect("token")
            .expose(),
        replacement.as_bytes()
    );
}

/// S4: a disconnect spends pending authorizations; one completing during a
/// disconnect stores nothing; a replacement spends them too.
#[test]
fn a_disconnect_or_replacement_spends_pending_authorizations() {
    let (_dir, path) = temp_db();
    let calls = Arc::new(AtomicUsize::new(0));
    let server = token_server(Arc::clone(&calls), Duration::from_millis(600));
    let conns = connections(&path, server.port);
    let start = |at: u64| {
        let (url, _, key) = parts(
            &conns
                .start("owner", "acme.main", "acme", None, &BTreeMap::new(), at)
                .expect("start"),
        );
        (query_param(&url, "state"), key)
    };
    // Started, disconnected, then completed: refused, nothing exchanged.
    let (state, key) = start(1_000);
    conns
        .revoke("owner", "acme.main", false, 1_100)
        .expect("revoke");
    assert_eq!(
        conns
            .complete("owner", "acme.main", &state, &key, &code("c1"), 1_200)
            .err(),
        Some(ConnError::BadState)
    );
    // Started, replaced by a PUT, then completed: refused too.
    let (state, key) = start(2_000);
    set(&conns, None);
    assert_eq!(
        conns
            .complete("owner", "acme.main", &state, &key, &code("c2"), 2_200)
            .err(),
        Some(ConnError::BadState)
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    // Disconnected while the code is being exchanged: nothing is stored.
    let (state, key) = start(3_000);
    let completed = std::thread::scope(|scope| {
        let completing =
            scope.spawn(|| conns.complete("owner", "acme.main", &state, &key, &code("c3"), 3_100));
        std::thread::sleep(Duration::from_millis(200));
        conns
            .revoke("owner", "acme.main", false, 3_200)
            .expect("revoke");
        completing.join().expect("thread")
    });
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(completed.err(), Some(ConnError::Conflict));
    assert_eq!(
        conns.status("acme.main").expect("status").state,
        "not_connected"
    );
}

/// S4: a slow provider revocation never deletes the credential set after
/// the disconnect.
#[test]
fn a_slow_revoke_never_deletes_a_replacement() {
    let (_dir, path) = temp_db();
    let calls = Arc::new(AtomicUsize::new(0));
    let server = token_server_with(
        Arc::clone(&calls),
        Duration::ZERO,
        Duration::from_millis(600),
    );
    let conns = connections(&path, server.port);
    set(&conns, None);
    let replacement = "access-token-REPLACED-0010-next";
    std::thread::scope(|scope| {
        let revoking = scope.spawn(|| conns.revoke("owner", "acme.main", true, 5_000));
        std::thread::sleep(Duration::from_millis(200));
        conns
            .set(
                "owner",
                "acme.main",
                SetCredentials::from_parts("acme", replacement, None, None),
                5_100,
            )
            .expect("replace");
        revoking.join().expect("thread").expect("revoked");
    });
    let status = conns.status("acme.main").expect("status");
    assert_eq!(
        (status.state.as_str(), status.last4.as_deref()),
        ("connected", Some("next"))
    );
    // The provider was asked to revoke the old grant, not the new one.
    let revoked = server
        .seen()
        .into_iter()
        .find(|s| s.line.starts_with("POST /revoke"))
        .expect("revoke call");
    assert!(revoked.body.contains(REFRESH) && !revoked.body.contains("REPLACED"));
}

/// S8, CONN-SEALED-BINDING-1: editing the plaintext row's provider or
/// parameters can't redirect a credential; the dev key is refused in a
/// normal keyring.
#[test]
fn plaintext_metadata_edits_cannot_redirect_a_credential() {
    let (_dir, path) = temp_db();
    let calls = Arc::new(AtomicUsize::new(0));
    let server = token_server(Arc::clone(&calls), Duration::ZERO);
    let conns = connections(&path, server.port);
    set(&conns, Some(2_000));
    let tamper = engine(&path);
    tamper
        .execute(
            r#"UPDATE "_norn_connection" SET "params" = ?1 WHERE "id" = 'acme.main'"#,
            &[SqlValue::Text(r#"{"shopSubdomain":"evil"}"#.to_owned())],
        )
        .expect("tamper");
    assert_eq!(
        conns
            .access_token("connection:acme.main", "acme.main", 10_000)
            .err(),
        Some(ConnError::Vault(VaultError::Integrity))
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);

    let dev = LocalKeyring::new(
        "prod-eu-v1",
        BTreeMap::from([(
            "prod-eu-v1".to_owned(),
            *b"norn-dev-insecure-vault-kek-v1!!",
        )]),
    );
    assert!(matches!(dev, Err(VaultError::Config(why)) if why.contains("development key")));
}

// -------------------------------------------- authorization: ReadPolicy

/// S7, CONN-AUTHZ-1: through the cell's own `ReadPolicy`, a viewer gets the
/// empty-scope answer and the vault is never opened; an admin is served. The
/// provider's browser redirect opens no vault either.
#[test]
fn read_policy_decides_before_the_vault_opens() {
    use norn_cell_do::analytics;
    use norn_cell_do::policy::{DEMO_GRANTS, ReadPolicy};
    use norn_cell_do::principal::{DEMO_FIXTURES, FixturePrincipals};

    let (_dir, path) = temp_db();
    let scope = analytics::analytics_scope();
    let opened = analytics::provision(engine(&path), &scope).expect("provision");
    let policy = ReadPolicy::compile(opened.store.model().object_types(), &scope, &DEMO_GRANTS)
        .expect("policy");
    let principals = FixturePrincipals::new(DEMO_FIXTURES, policy.caller());
    let host = DevHost::from_store(opened.store, scope, |command| command.payload.clone())
        .expect("host")
        .with_authorizer(policy);
    let calls = Arc::new(AtomicUsize::new(0));
    let server = token_server(Arc::clone(&calls), Duration::ZERO);
    let opens = Arc::new(AtomicUsize::new(0));
    let routes = routes(&path, server.port, &opens);
    let query = "orgId=org_demo&environmentId=analytics";
    let path_of = "/v1/connections/acme.main/credentials";
    let call = |method: &str, path: &str, query: &str, auth: Option<&str>, body: &str| {
        routes
            .route(
                &host,
                &principals,
                &request(method, path, query, auth, body),
                1_000,
            )
            .expect("routed")
    };
    for auth in [
        Some("Bearer fixture:guest@demo"),
        None,
        Some("Bearer fixture:nobody@demo"),
    ] {
        for (method, route, body) in [
            ("GET", path_of, ""),
            ("DELETE", path_of, ""),
            (
                "POST",
                "/v1/connections/acme.main/oauth:start",
                r#"{"provider":"acme"}"#,
            ),
            (
                "POST",
                "/v1/connections/acme.main/oauth:complete",
                r#"{"state":"x","completionKey":"y"}"#,
            ),
        ] {
            let refused = call(method, route, query, auth, body);
            let outside = norn_cell_do::server::outside::<NativeEngine>(&request(
                method, route, query, auth, body,
            ));
            assert_eq!(
                (refused.status, &refused.body),
                (outside.status, &outside.body),
                "{auth:?} {method} {route}"
            );
        }
    }
    let parked = call(
        "GET",
        "/v1/connections/oauth:callback",
        "state=abc&code=def",
        None,
        "",
    );
    assert_eq!(parked.status, 400, "{}", parked.body);
    assert!(routes.handoffs().is_empty(1_000));
    assert_eq!(
        opens.load(Ordering::SeqCst),
        0,
        "the vault opened before authorization"
    );
    assert!(!routes.vault_opened());

    let admin = call("GET", path_of, query, Some("Bearer fixture:admin@demo"), "");
    assert_eq!(admin.status, 200, "{}", admin.body);
    assert_eq!(opens.load(Ordering::SeqCst), 1);
}

#[test]
fn a_transient_vault_open_failure_is_not_cached_forever() {
    let (_dir, path) = temp_db();
    let host = owner_host(&path);
    let calls = Arc::new(AtomicUsize::new(0));
    let server = token_server(Arc::clone(&calls), Duration::ZERO);
    let attempts = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&attempts);
    let vault_path = path.clone();
    let port = server.port;
    let flaky = Routes::with_opener(move |_scope| {
        if counter.fetch_add(1, Ordering::SeqCst) == 0 {
            Err("sqlite busy".to_owned())
        } else {
            Ok(connections(&vault_path, port))
        }
    });
    let req = request(
        "GET",
        "/v1/connections/acme.main/credentials",
        SCOPE_QUERY,
        Some("Bearer owner-token"),
        "",
    );
    let first = flaky.route(&host, &Bearers, &req, 1_000).expect("routed");
    assert_eq!(first.status, 503, "{}", first.body);
    assert!(
        first.body.contains("connection.unavailable"),
        "{}",
        first.body
    );
    assert!(!flaky.vault_opened());
    assert!(flaky.connections().is_none());

    let second = flaky.route(&host, &Bearers, &req, 2_000).expect("routed");
    assert_eq!(second.status, 200, "{}", second.body);
    assert!(flaky.vault_opened());
    assert!(flaky.connections().is_some());
    assert_eq!(attempts.load(Ordering::SeqCst), 2);

    let third = flaky.route(&host, &Bearers, &req, 3_000).expect("routed");
    assert_eq!(third.status, 200, "{}", third.body);
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
}

// ------------------------------------------------------------ round 2

fn json_of(response: &norn_cell_do::server::Response) -> serde_json::Value {
    serde_json::from_str(&response.body).expect("json")
}

/// Drives [`routes`] over [`owner_host`] at a fixed clock.
struct Driver {
    host: DevHost<NativeEngine>,
    routes: Routes<NativeEngine>,
}

const SCOPE_QUERY: &str = "orgId=org_demo&environmentId=dev";

impl Driver {
    fn new(path: &std::path::Path, port: u16) -> Self {
        Self {
            host: owner_host(path),
            routes: routes(path, port, &Arc::new(AtomicUsize::new(0))),
        }
    }

    fn call(
        &self,
        method: &str,
        path: &str,
        query: &str,
        bearer: Option<&str>,
        body: &str,
        now_ms: u64,
    ) -> norn_cell_do::server::Response {
        let auth = bearer.map(|token| format!("Bearer {token}"));
        self.routes
            .route(
                &self.host,
                &Bearers,
                &request(method, path, query, auth.as_deref(), body),
                now_ms,
            )
            .expect("routed")
    }

    /// `oauth:start` as `owner`: `(state, completionKey)`.
    fn start(&self, now_ms: u64) -> (String, String) {
        let started = self.call(
            "POST",
            "/v1/connections/acme.main/oauth:start",
            SCOPE_QUERY,
            Some("owner-token"),
            r#"{"provider":"acme"}"#,
            now_ms,
        );
        assert_eq!(started.status, 200, "{}", started.body);
        let body = json_of(&started);
        (
            query_param(
                body.get("authorizeUrl")
                    .and_then(serde_json::Value::as_str)
                    .expect("url"),
                "state",
            ),
            body.get("completionKey")
                .and_then(serde_json::Value::as_str)
                .expect("key")
                .to_owned(),
        )
    }

    fn callback(&self, state: &str, code: &str, now_ms: u64) -> norn_cell_do::server::Response {
        self.call(
            "GET",
            "/v1/connections/oauth:callback",
            &format!("state={state}&code={code}"),
            None,
            "",
            now_ms,
        )
    }

    fn complete(
        &self,
        connection: &str,
        bearer: &str,
        state: &str,
        key: &str,
        now_ms: u64,
    ) -> norn_cell_do::server::Response {
        self.call(
            "POST",
            &format!("/v1/connections/{connection}/oauth:complete"),
            SCOPE_QUERY,
            Some(bearer),
            &serde_json::json!({"state": state, "completionKey": key}).to_string(),
            now_ms,
        )
    }
}

/// S5a, OAUTH-HANDOFF-1: the unauthenticated callback parks a code only
/// for a state an authorized start reserved; a flood of unknown states
/// stores nothing and blocks no legitimate flow; one principal holds at
/// most `MAX_PER_PRINCIPAL` reservations; expired ones are dropped.
#[test]
fn the_callback_parks_only_reserved_states_and_a_flood_blocks_nothing() {
    use norn_cell_do::connections::{MAX_PARKED, MAX_PER_PRINCIPAL};
    let (_dir, path) = temp_db();
    let calls = Arc::new(AtomicUsize::new(0));
    let server = token_server(Arc::clone(&calls), Duration::ZERO);
    let driver = Driver::new(&path, server.port);

    for flood in 0..(MAX_PARKED * 4) {
        let refused = driver.callback(&format!("unknown-state-{flood}"), "a-code", 1_000);
        assert_eq!(
            (refused.status, &json_of(&refused)["code"]),
            (400, &serde_json::json!("connection.oauth_state"))
        );
    }
    assert!(driver.routes.handoffs().is_empty(1_000));

    // A legitimate flow after the flood: reserved, parked, completed.
    let (state, key) = driver.start(1_000);
    assert_eq!(driver.callback(&state, "the-code", 1_100).status, 200);
    let connected = driver.complete("acme.main", "owner-token", &state, &key, 1_200);
    assert_eq!(connected.status, 200, "{}", connected.body);
    assert_eq!(json_of(&connected)["state"], "connected");
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // The per-principal cap: a principal's oldest reservation goes first.
    let flows: Vec<(String, String)> = (0..MAX_PER_PRINCIPAL + 3)
        .map(|at| driver.start(2_000 + u64::try_from(at).expect("small")))
        .collect();
    assert_eq!(driver.routes.handoffs().len(2_100), MAX_PER_PRINCIPAL);
    assert_eq!(driver.callback(&flows[0].0, "late", 2_100).status, 400);
    let newest = flows.last().expect("flows");
    assert_eq!(driver.callback(&newest.0, "fresh", 2_100).status, 200);

    // Expired reservations are dropped on the next request, code and all.
    let expired = 2_000 + 600_000 + 50;
    assert_eq!(driver.callback(&newest.0, "again", expired).status, 400);
    assert!(driver.routes.handoffs().is_empty(expired));
}

/// S5b, OAUTH-SESSION-1: another session of the same principal (no
/// completion key, or a wrong one) and a caller on another connection can't
/// complete or cancel the flow; the starting session then completes.
#[test]
fn only_the_starting_session_completes_and_a_mismatch_spends_nothing() {
    let (_dir, path) = temp_db();
    let calls = Arc::new(AtomicUsize::new(0));
    let server = token_server(Arc::clone(&calls), Duration::ZERO);
    let driver = Driver::new(&path, server.port);
    let (state, key) = driver.start(1_000);
    assert_eq!(driver.callback(&state, "the-code", 1_100).status, 200);

    for (case, (connection, bearer, presented)) in [
        ("acme.main", "owner-token-2", "a-guessed-key"),
        ("acme.main", "owner-token", "a-guessed-key"),
        ("acme.other", "owner-token", key.as_str()),
        ("acme.other", "owner-token-2", key.as_str()),
    ]
    .into_iter()
    .enumerate()
    {
        let refused = driver.complete(connection, bearer, &state, presented, 1_200);
        assert_eq!(
            (refused.status, &json_of(&refused)["code"]),
            (400, &serde_json::json!("connection.oauth_state")),
            "case {case}"
        );
    }
    let missing_key = driver.call(
        "POST",
        "/v1/connections/acme.main/oauth:complete",
        SCOPE_QUERY,
        Some("owner-token-2"),
        &serde_json::json!({ "state": state }).to_string(),
        1_200,
    );
    assert_eq!(missing_key.status, 400);
    assert_eq!(calls.load(Ordering::SeqCst), 0, "a mismatch exchanged");
    assert_eq!(
        driver.routes.handoffs().len(1_200),
        1,
        "a mismatch spent it"
    );

    let connected = driver.complete("acme.main", "owner-token", &state, &key, 1_300);
    assert_eq!(connected.status, 200, "{}", connected.body);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let trail = driver
        .routes
        .connections()
        .expect("opened")
        .audit_trail("acme.main")
        .expect("audit");
    assert!(
        trail
            .iter()
            .any(|(_, event, ..)| event.as_str() == "oauth.connected")
    );
}

fn connections_with(
    path: &std::path::Path,
    egress: Egress,
    provider: Provider,
) -> Connections<NativeEngine> {
    let vault = Vault::open(
        engine(path),
        SCOPE,
        Arc::new(keyring("test-local-v1", &[("test-local-v1", 7)])),
    )
    .expect("vault");
    Connections::new(
        vault,
        egress,
        BTreeMap::from([(provider.id.clone(), provider)]),
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

/// Resolves every name to loopback.
#[derive(Debug)]
struct ToLoopback;

impl Lookup for ToLoopback {
    fn lookup(&self, _host: &str, _port: u16) -> std::io::Result<Vec<IpAddr>> {
        Ok(vec![IpAddr::from([127, 0, 0, 1])])
    }
}

/// S1, EGRESS-OAUTH-HTTPS-1: an allowlisted plain-http token or
/// revocation endpoint never receives a code, a client secret or a token.
#[test]
fn oauth_calls_go_only_to_https_even_to_a_listed_host() {
    let (_dir, path) = temp_db();
    let calls = Arc::new(AtomicUsize::new(0));
    let server = token_server(Arc::clone(&calls), Duration::ZERO);
    let host = "plain.acme.test";
    let mut provider = test_provider(server.port);
    provider.token_url = format!("http://{host}:{}/token", server.port);
    provider.revocation_url = Some(format!("http://{host}:{}/revoke", server.port));
    let egress = Egress::new(vec![host.to_owned()], Arc::new(ToLoopback)).with_sleep(|_| {});
    // The listed host is reachable for ordinary calls ...
    let reachable = egress.send(
        http::Request::get(format!("http://{host}:{}/ping", server.port))
            .body(Vec::new())
            .expect("request"),
    );
    assert_eq!(reachable.expect("reached").status(), 200);
    let conns = connections_with(&path, egress, provider);
    // ... but no OAuth call goes there.
    let (url, _, key) = parts(
        &conns
            .start("owner", "acme.main", "acme", None, &BTreeMap::new(), 1_000)
            .expect("start"),
    );
    let exchanged = conns.complete(
        "owner",
        "acme.main",
        &query_param(&url, "state"),
        &key,
        &code("code-1"),
        2_000,
    );
    assert!(
        matches!(exchanged, Err(ConnError::Egress(EgressError::Forbidden(_)))),
        "{exchanged:?}"
    );
    set(&conns, Some(2_000));
    assert!(matches!(
        conns.access_token("connection:acme.main", "acme.main", 10_000),
        Err(ConnError::Egress(EgressError::Forbidden(_)))
    ));
    conns
        .revoke("owner", "acme.main", true, 20_000)
        .expect("revoke");
    let trail = conns.audit_trail("acme.main").expect("audit");
    assert_eq!(
        trail
            .last()
            .map(|(_, event, _, detail)| (event.as_str(), detail.as_deref())),
        Some(("credential.provider_revoke", Some("egress_forbidden")))
    );
    let seen = server.seen();
    assert_eq!(
        seen.len(),
        1,
        "an OAuth call reached the plain-http endpoint"
    );
    assert!(seen.iter().all(|s| !s.body.contains("token")));
}

/// A resolver that hangs until opened, counting calls.
#[derive(Debug, Default)]
struct Hanging {
    open: Mutex<bool>,
    opened: std::sync::Condvar,
    calls: AtomicUsize,
}

#[derive(Debug)]
struct HangingLookup(Arc<Hanging>);

impl Lookup for HangingLookup {
    fn lookup(&self, _host: &str, _port: u16) -> std::io::Result<Vec<IpAddr>> {
        self.0.calls.fetch_add(1, Ordering::SeqCst);
        let mut open = self.0.open.lock().expect("gate");
        while !*open {
            open = self.0.opened.wait(open).expect("gate");
        }
        Err(std::io::Error::other("closed"))
    }
}

/// Backoff sleeps taken by [`timed_out_lookups_never_accumulate_beyond_the_bound`].
static BUSY_SLEEPS: AtomicUsize = AtomicUsize::new(0);

/// S2, EGRESS-DNS-BOUND-1: lookups that time out leave at most
/// `MAX_LOOKUPS` threads behind; further lookups are refused at once, and
/// the bound frees as the hung lookups end.
#[test]
fn timed_out_lookups_never_accumulate_beyond_the_bound() {
    use norn_cell_do::connections::egress::MAX_LOOKUPS;
    let hanging = Arc::new(Hanging::default());
    let quick = Timeouts {
        connect: Duration::from_millis(100),
        resolve: Duration::from_millis(20),
        total: Duration::from_millis(500),
    };
    let egress = Egress::with_timeouts(
        Vec::new(),
        Arc::new(HangingLookup(Arc::clone(&hanging))),
        quick,
    )
    .with_sleep(|_| {
        BUSY_SLEEPS.fetch_add(1, Ordering::SeqCst);
    });
    let get = || {
        egress.send(
            http::Request::get("https://hangs.example.com/")
                .body(Vec::new())
                .expect("request"),
        )
    };
    let mut busy = 0;
    for _ in 0..(MAX_LOOKUPS + 8) {
        match get() {
            Err(EgressError::Unresolved(_)) => {}
            Err(EgressError::Transport("resolver_busy")) => busy += 1,
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(hanging.calls.load(Ordering::SeqCst), MAX_LOOKUPS);
    assert_eq!(egress.outstanding_lookups(), MAX_LOOKUPS);
    assert_eq!(busy, 8);
    // S2 (round 3): a busy resolver answers at once; it is never retried
    // through the backoff.
    assert_eq!(
        BUSY_SLEEPS.load(Ordering::SeqCst),
        0,
        "resolver_busy was retried"
    );

    *hanging.open.lock().expect("gate") = true;
    hanging.opened.notify_all();
    let released = Instant::now();
    while egress.outstanding_lookups() > 0 {
        assert!(released.elapsed() < Duration::from_secs(5), "threads stuck");
        std::thread::yield_now();
    }
    assert!(matches!(get(), Err(EgressError::Unresolved(_))));
    assert_eq!(hanging.calls.load(Ordering::SeqCst), MAX_LOOKUPS + 1);
}

/// S4, GRANT-REVOKE-1: a routine disconnect calls no provider, so a grant
/// shared with sibling connections survives it; revoking the grant is its
/// own explicit request.
#[test]
fn a_routine_disconnect_never_revokes_the_shared_grant() {
    let (_dir, path) = temp_db();
    let calls = Arc::new(AtomicUsize::new(0));
    let server = token_server(Arc::clone(&calls), Duration::ZERO);
    let driver = Driver::new(&path, server.port);
    let credentials =
        format!(r#"{{"provider":"acme","accessToken":"{ACCESS}","refreshToken":"{REFRESH}"}}"#);
    let path_of = "/v1/connections/acme.main/credentials";
    let revocations = || {
        server
            .seen()
            .into_iter()
            .filter(|s| s.line.starts_with("POST /revoke"))
            .count()
    };
    for (query, revoked) in [
        (SCOPE_QUERY.to_owned(), 0),
        (format!("{SCOPE_QUERY}&revokeGrant=false"), 0),
        (format!("{SCOPE_QUERY}&revokeGrant=true"), 1),
    ] {
        let set = driver.call(
            "PUT",
            path_of,
            SCOPE_QUERY,
            Some("owner-token"),
            &credentials,
            1_000,
        );
        assert_eq!(set.status, 200, "{}", set.body);
        let gone = driver.call("DELETE", path_of, &query, Some("owner-token"), "", 1_000);
        assert_eq!(gone.status, 200, "{}", gone.body);
        assert_eq!(json_of(&gone)["state"], "not_connected");
        assert_eq!(revocations(), revoked, "{query}");
    }
    let bad = driver.call(
        "DELETE",
        path_of,
        &format!("{SCOPE_QUERY}&revokeGrant=yes"),
        Some("owner-token"),
        "",
        1_000,
    );
    assert_eq!(bad.status, 400);
}

/// S8, REQUEST-DEBUG-1: a transport request's `Debug` shows no query (an
/// OAuth callback's code), no `Authorization` and no body; the startup
/// variables print their names only.
#[test]
fn debug_output_carries_no_secret() {
    let printed = format!(
        "{:?}",
        request(
            "GET",
            "/v1/connections/oauth:callback",
            "state=STATE-SECRET&code=CODE-SECRET",
            Some("Bearer TOKEN-SECRET"),
            r#"{"accessToken":"BODY-SECRET"}"#,
        )
    );
    assert!(
        printed.contains("/v1/connections/oauth:callback"),
        "the path is not printed"
    );
    for (label, secret) in [
        ("state", "STATE-SECRET"),
        ("code", "CODE-SECRET"),
        ("authorization", "TOKEN-SECRET"),
        ("body", "BODY-SECRET"),
    ] {
        assert!(!printed.contains(secret), "the {label} is printed");
    }
    // RESPONSE-DEBUG-1: `oauth:start`'s answer carries the completion key
    // for its one caller; its `Debug` shows the status and length only.
    let (_dir, path) = temp_db();
    let server = token_server(Arc::new(AtomicUsize::new(0)), Duration::ZERO);
    let driver = Driver::new(&path, server.port);
    let started = driver.call(
        "POST",
        "/v1/connections/acme.main/oauth:start",
        SCOPE_QUERY,
        Some("owner-token"),
        r#"{"provider":"acme"}"#,
        1_000,
    );
    assert_eq!(started.status, 200, "{}", started.body);
    let key = json_of(&started)["completionKey"]
        .as_str()
        .expect("key")
        .to_owned();
    let printed = format!("{started:?}");
    assert!(
        printed.contains("200") && !printed.contains(&key) && !printed.contains("completionKey"),
        "the start answer's body is printed"
    );
    let vars = norn_cell_do::connections::SecretVars(BTreeMap::from([(
        "NORN_OAUTH_ACME_CLIENT_SECRET".to_owned(),
        "VAR-SECRET".to_owned(),
    )]));
    let printed = format!("{vars:?}");
    assert!(
        printed.contains("NORN_OAUTH_ACME_CLIENT_SECRET") && !printed.contains("VAR-SECRET"),
        "a variable value is printed"
    );
}

/// S4 (round 3), GRANT-REVOKE-1: `revokeGrant=true` is decided at grant
/// scope before anything is deleted. A manager of one connection without
/// that authority is refused, and the credential, its row and the provider
/// grant all stay; a plain disconnect still works for them.
#[test]
fn revoking_the_grant_needs_grant_scope_before_anything_is_deleted() {
    let (_dir, path) = temp_db();
    let calls = Arc::new(AtomicUsize::new(0));
    let server = token_server(Arc::clone(&calls), Duration::ZERO);
    let driver = Driver::new(&path, server.port);
    let path_of = "/v1/connections/acme.main/credentials";
    let credentials =
        format!(r#"{{"provider":"acme","accessToken":"{ACCESS}","refreshToken":"{REFRESH}"}}"#);
    let set = driver.call(
        "PUT",
        path_of,
        SCOPE_QUERY,
        Some("owner-token"),
        &credentials,
        1_000,
    );
    assert_eq!(set.status, 200, "{}", set.body);

    let refused = driver.call(
        "DELETE",
        path_of,
        &format!("{SCOPE_QUERY}&revokeGrant=true"),
        Some("manager-token"),
        "",
        1_100,
    );
    assert_eq!(
        (refused.status, &json_of(&refused)["code"]),
        (403, &serde_json::json!("connection.grant_forbidden")),
        "{}",
        refused.body
    );
    let status = driver.call(
        "GET",
        path_of,
        SCOPE_QUERY,
        Some("manager-token"),
        "",
        1_200,
    );
    assert_eq!(status.status, 200, "{}", status.body);
    assert_eq!(
        json_of(&status)["state"],
        "connected",
        "a refused revoke deleted"
    );
    assert!(
        server
            .seen()
            .iter()
            .all(|s| !s.line.starts_with("POST /revoke")),
        "a refused revoke reached the provider"
    );
    let conns = driver.routes.connections().expect("opened");
    assert!(
        conns
            .audit_trail("acme.main")
            .expect("audit")
            .iter()
            .all(|(_, event, ..)| event.as_str() != "credential.revoke"),
        "a refused revoke was recorded as one"
    );

    // A plain disconnect is theirs to make.
    let gone = driver.call(
        "DELETE",
        path_of,
        SCOPE_QUERY,
        Some("manager-token"),
        "",
        1_300,
    );
    assert_eq!(gone.status, 200, "{}", gone.body);
    assert_eq!(json_of(&gone)["state"], "not_connected");
}

/// Pending authorizations in the store: `(rows, sealed verifiers)`.
fn pending_records(path: &std::path::Path) -> (i64, i64) {
    let engine = engine(path);
    let count = |sql: &str| {
        engine
            .query(sql, &[])
            .expect("count")
            .first()
            .and_then(|row| row.first())
            .and_then(|value| {
                if let SqlValue::Integer(count) = value {
                    Some(*count)
                } else {
                    None
                }
            })
            .expect("an integer count")
    };
    (
        count(r#"SELECT COUNT(*) FROM "_norn_oauth_state""#),
        count(r#"SELECT COUNT(*) FROM "_norn_vault_sealed" WHERE "slot" LIKE 'oauth-state:%'"#),
    )
}

/// Lets every principal manage every connection (and nothing else).
struct AnyManager;

impl Authorizer for AnyManager {
    fn authorize(&self, request: &AccessRequest<'_>) -> Result<(), Refusal> {
        match (&request.operation, request.actor) {
            (Operation::ManageConnection(_), Some(_)) => Ok(()),
            _ => Err(norn_api::authz::read_denied()),
        }
    }
}

/// `Bearer actor-N` is principal `actor-N`.
struct Numbered;

impl PrincipalResolver for Numbered {
    fn resolve(&self, authorization: Option<&str>, _now: u64) -> Option<String> {
        authorization?
            .strip_prefix("Bearer ")
            .filter(|token| token.starts_with("actor-"))
            .map(str::to_owned)
    }
    fn end_request(&self) {}
}

/// S5a (round 3), OAUTH-HANDOFF-1: a start is admitted before it stores
/// anything, so a start refused at capacity leaves no row; a reservation
/// evicted for a principal's newer one, or expired, takes its row and
/// sealed verifier with it.
#[test]
fn refused_evicted_and_expired_starts_leave_no_pending_records() {
    use norn_cell_do::connections::service::STATE_TTL_MS;
    use norn_cell_do::connections::{MAX_PARKED, MAX_PER_PRINCIPAL};
    let (_dir, path) = temp_db();
    let server = token_server(Arc::new(AtomicUsize::new(0)), Duration::ZERO);
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
    .with_authorizer(AnyManager);
    let routes = routes(&path, server.port, &Arc::new(AtomicUsize::new(0)));
    let start = |actor: usize, now_ms: u64| {
        let auth = format!("Bearer actor-{actor}");
        routes
            .route(
                &host,
                &Numbered,
                &request(
                    "POST",
                    "/v1/connections/acme.main/oauth:start",
                    SCOPE_QUERY,
                    Some(&auth),
                    r#"{"provider":"acme"}"#,
                ),
                now_ms,
            )
            .expect("routed")
    };
    let full = MAX_PARKED / MAX_PER_PRINCIPAL;
    let live = i64::try_from(MAX_PARKED).expect("small");
    for actor in 0..full {
        for _ in 0..MAX_PER_PRINCIPAL {
            let started = start(actor, 1_000);
            assert_eq!(started.status, 200, "{}", started.body);
        }
    }
    assert_eq!(routes.handoffs().len(1_000), MAX_PARKED);
    assert_eq!(pending_records(&path), (live, live));

    // At capacity: a new principal's start is refused and stores nothing.
    let refused = start(full, 1_000);
    assert_eq!(
        (refused.status, &json_of(&refused)["code"]),
        (503, &serde_json::json!("connection.unavailable")),
        "{}",
        refused.body
    );
    assert_eq!(
        pending_records(&path),
        (live, live),
        "a refused start stored"
    );

    // A principal at its own cap replaces its oldest: row and verifier go.
    let replaced = start(0, 1_500);
    assert_eq!(replaced.status, 200, "{}", replaced.body);
    assert_eq!(routes.handoffs().len(1_500), MAX_PARKED);
    assert_eq!(
        pending_records(&path),
        (live, live),
        "an evicted flow kept its rows"
    );

    // Expired: the next request drops the reservations and their records.
    let expired = 1_000 + STATE_TTL_MS;
    let callback = routes
        .route(
            &host,
            &Numbered,
            &request(
                "GET",
                "/v1/connections/oauth:callback",
                "state=unknown&code=a-code",
                None,
                "",
            ),
            expired,
        )
        .expect("routed");
    assert_eq!(callback.status, 400, "{}", callback.body);
    assert_eq!(routes.handoffs().len(expired), 1, "the later start is live");
    assert_eq!(
        pending_records(&path),
        (1, 1),
        "expired flows kept their rows"
    );
    let after = 1_500 + STATE_TTL_MS;
    let late = routes
        .route(
            &host,
            &Numbered,
            &request(
                "GET",
                "/v1/connections/oauth:callback",
                "state=unknown&code=a-code",
                None,
                "",
            ),
            after,
        )
        .expect("routed");
    assert_eq!(late.status, 400, "{}", late.body);
    assert!(routes.handoffs().is_empty(after));
    assert_eq!(pending_records(&path), (0, 0));
}

#[test]
fn provider_fill_rejects_undeclared_params_and_stray_brace() {
    let provider = test_provider(1);
    let extra = BTreeMap::from([("undeclared".to_owned(), "abc".to_owned())]);
    assert!(
        provider
            .fill("https://api.acme.example/v1", &extra)
            .is_err()
    );
    assert!(
        provider
            .fill("https://api.acme.example/v1/}", &BTreeMap::new())
            .is_err()
    );
}

#[test]
fn config_rejects_malformed_redirect_url() {
    let vars = BTreeMap::from([
        (
            norn_cell_do::connections::vault::KEK_VAR.to_owned(),
            norn_cell_do::connections::vault::DEV_KEK_VALUE.to_owned(),
        ),
        (
            norn_cell_do::connections::REDIRECT_URL_VAR.to_owned(),
            "not-a-url".to_owned(),
        ),
    ]);
    assert!(Config::from_vars(&vars, true).is_err());
}
