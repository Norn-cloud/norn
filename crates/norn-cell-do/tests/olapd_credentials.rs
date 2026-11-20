//! OLAP-CRED-1 (#1493): `norn-olapd` serves each cell for its own tenant
//! only, on the cell's own credential, over its real HTTP transport. A cell's
//! credential reads and writes nothing of another cell's database, and the
//! answer is byte-identical to the answer for a vacant scope (and for no
//! credential at all), so it reveals nothing about whether the other cell
//! exists. Expired and rotated-out credentials are refused, and rotation
//! needs no restart.
#![cfg(all(feature = "duckdb", not(target_arch = "wasm32")))]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "credential fixtures must build before the daemon's answers are compared"
)]

use std::io::{Read as _, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use norn_api::analytics_routes::{EngineCall, Transport as _};
use norn_api::clock::ManualClock;
use norn_api::olap_credential::{
    CREDENTIAL_LIFETIME_MS, Issuer, Keyring, MAX_CREDENTIAL_LIFETIME_MS,
};
use norn_cell::analytics::TenantId;
use norn_cell::analytics::engine::Settings;
use norn_cell_do::olap::Exchange;
use norn_cell_do::olapd::{Config, serve};
use serde_json::json;

const NOW: u64 = 1_800_000_000_000;
const WORLD: &str = "production";

fn issuer(kid: &str, seed: u8) -> Issuer {
    Issuer::parse(&format!("{kid}:{}", URL_SAFE_NO_PAD.encode([seed; 32]))).unwrap()
}

fn tenant(scope: &str) -> TenantId {
    TenantId::of_scope(scope)
}

/// Replace the keyring as an operator does: write a copy, rename it over.
fn write_keyring(path: &Path, issuers: &[&Issuer]) {
    let mut text = issuers
        .iter()
        .map(|issuer| issuer.public_spec())
        .collect::<Vec<_>>()
        .join("\n");
    text.push('\n');
    let staged = path.with_extension("new");
    std::fs::write(&staged, text).unwrap();
    std::fs::rename(&staged, path).unwrap();
}

struct Daemon {
    addr: SocketAddr,
    root: PathBuf,
    keys: PathBuf,
    clock: Arc<ManualClock>,
    _directory: tempfile::TempDir,
}

/// A running daemon whose keyring holds `issuers`, at `NOW`.
fn daemon(issuers: &[&Issuer]) -> Daemon {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("olap");
    let keys = directory.path().join("keyring");
    write_keyring(&keys, issuers);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let clock = Arc::new(ManualClock::at(NOW));
    let config = Config {
        addr: addr.to_string(),
        keys_file: keys.clone(),
        settings: Settings {
            root: root.clone(),
            memory_limit: "256MB".to_owned(),
            threads: 1,
        },
    };
    let serving = Arc::clone(&clock);
    std::thread::spawn(move || serve(listener, config, serving));
    Daemon {
        addr,
        root,
        keys,
        clock,
        _directory: directory,
    }
}

impl Daemon {
    /// The raw response, status line, headers and body, to `POST /v1/<op>`.
    fn post(&self, op: &str, authorization: Option<&str>, body: &str) -> Vec<u8> {
        let mut stream = TcpStream::connect(self.addr).unwrap();
        let authorization = authorization
            .map(|value| format!("authorization: {value}\r\n"))
            .unwrap_or_default();
        write!(
            stream,
            "POST /v1/{op} HTTP/1.1\r\nhost: olapd\r\n{authorization}content-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).unwrap();
        response
    }

    fn call(&self, op: &str, credential: &str, body: &str) -> Vec<u8> {
        self.post(op, Some(&format!("Bearer {credential}")), body)
    }

    fn database_exists(&self, tenant: &TenantId) -> bool {
        self.root
            .join(format!("{}.duckdb", tenant.as_str()))
            .exists()
    }
}

fn status(response: &[u8]) -> &str {
    std::str::from_utf8(response.get(9..12).unwrap_or_default()).unwrap_or_default()
}

fn body(response: &[u8]) -> String {
    let text = String::from_utf8_lossy(response);
    text.split_once("\r\n\r\n")
        .map(|(_, body)| body.to_owned())
        .unwrap_or_default()
}

fn frontier(of: &TenantId) -> String {
    json!({ "tenant": of, "world": WORLD, "tables": [] }).to_string()
}

fn sync(of: &TenantId, after: u64, through: u64) -> String {
    json!({ "tenant": of, "world": WORLD, "after": after, "through": through, "rows": [] })
        .to_string()
}

fn export(of: &TenantId) -> String {
    json!({
        "tenant": of,
        "source": { "kind": "mirror", "world": WORLD, "table": "shop.Sale", "frontier": 3, "scope": "s" },
        "columns": [],
        "format": "jsonl",
    })
    .to_string()
}

/// The one refusal every refused credential gets.
fn refused(daemon: &Daemon) -> Vec<u8> {
    daemon.post("frontier", None, &frontier(&tenant("anyone")))
}

#[test]
fn a_cells_credential_reaches_nothing_of_another_cell_and_cannot_tell_it_exists() {
    let host = issuer("k1", 7);
    let daemon = daemon(&[&host]);
    let (a, b, vacant) = (
        tenant("org_a/prod"),
        tenant("org_b/prod"),
        tenant("org_v/prod"),
    );
    let (as_a, as_b) = (host.mint(&a, NOW), host.mint(&b, NOW));

    // B exists and holds a mirror at commit 3, written with B's own credential.
    assert_eq!(
        status(&daemon.call("frontier", &as_b, &frontier(&b))),
        "200"
    );
    let synced = daemon.call("sync", &as_b, &sync(&b, 0, 3));
    assert_eq!(status(&synced), "200", "{}", body(&synced));
    assert_eq!(
        body(&daemon.call("frontier", &as_b, &frontier(&b))),
        r#"{"frontier":3}"#
    );
    // A's credential is good for A.
    assert_eq!(
        body(&daemon.call("frontier", &as_a, &frontier(&a))),
        r#"{"frontier":0}"#
    );

    let uniform = refused(&daemon);
    assert_eq!(status(&uniform), "401");
    // Every body below parses: a refusal is the tenant check, never a 400.
    serde_json::from_str::<norn_cell::analytics::ExportRequest>(&export(&b)).unwrap();
    serde_json::from_str::<norn_cell::analytics::SyncRequest>(&sync(&b, 3, 9)).unwrap();
    assert_eq!(
        body(&uniform),
        r#"{"code":"analytics.unauthorized","detail":"the caller is not the cell"}"#
    );
    for (op, of) in [
        ("frontier", frontier as fn(&TenantId) -> String),
        ("sync", |t: &TenantId| sync(t, 3, 9)),
        ("export", export),
    ] {
        let at_b = daemon.call(op, &as_a, &of(&b));
        let at_vacant = daemon.call(op, &as_a, &of(&vacant));
        assert_eq!(at_b, at_vacant, "{op}: B's existence shows");
        assert_eq!(at_b, uniform, "{op}: unlike any other refused credential");
        assert_eq!(
            daemon.post(op, Some("Bearer nonsense"), &of(&b)),
            uniform,
            "{op}"
        );
    }
    // Nothing was written or opened: B is still at 3, the vacant scope has no database.
    assert_eq!(
        body(&daemon.call("frontier", &as_b, &frontier(&b))),
        r#"{"frontier":3}"#
    );
    assert!(daemon.database_exists(&b) && !daemon.database_exists(&vacant));
}

#[test]
fn an_expired_or_overlong_credential_is_refused() {
    let host = issuer("k1", 7);
    let daemon = daemon(&[&host]);
    let a = tenant("org_a/prod");
    let uniform = refused(&daemon);
    let credential = host.mint(&a, NOW);
    assert_eq!(
        status(&daemon.call("frontier", &credential, &frontier(&a))),
        "200"
    );
    daemon.clock.set(NOW + CREDENTIAL_LIFETIME_MS);
    assert_eq!(daemon.call("frontier", &credential, &frontier(&a)), uniform);
    daemon.clock.set(NOW);
    let overlong = host.mint_until(&a, NOW + MAX_CREDENTIAL_LIFETIME_MS + 1);
    assert_eq!(daemon.call("frontier", &overlong, &frontier(&a)), uniform);
}

#[test]
fn a_rotated_out_key_is_refused_and_rotation_needs_no_restart() {
    let (old, new) = (issuer("old", 1), issuer("new", 2));
    let daemon = daemon(&[&old]);
    let a = tenant("org_a/prod");
    let uniform = refused(&daemon);
    let (by_old, by_new) = (old.mint(&a, NOW), new.mint(&a, NOW));
    let ask = |credential: &str| daemon.call("frontier", credential, &frontier(&a));
    assert_eq!(status(&ask(&by_old)), "200");
    assert_eq!(ask(&by_new), uniform, "not yet added");
    // Overlap: both keys verify while the host moves to the new one.
    write_keyring(&daemon.keys, &[&old, &new]);
    assert_eq!(status(&ask(&by_old)), "200");
    assert_eq!(status(&ask(&by_new)), "200");
    // The old key is rotated out: its credentials are refused at once.
    write_keyring(&daemon.keys, &[&new]);
    assert_eq!(ask(&by_old), uniform);
    assert_eq!(status(&ask(&by_new)), "200");
    // A keyring the daemon cannot read refuses every call; none falls back.
    std::fs::remove_file(&daemon.keys).unwrap();
    assert_eq!(ask(&by_new), uniform);
    std::fs::write(&daemon.keys, "not a keyring\n").unwrap();
    assert_eq!(ask(&by_new), uniform);
}

#[test]
fn the_shared_token_is_retired_and_a_keyring_is_required() {
    let directory = tempfile::tempdir().unwrap();
    let keys = directory.path().join("keyring");
    write_keyring(&keys, &[&issuer("k1", 7)]);
    let keys = keys.to_string_lossy().into_owned();
    let with = |pairs: Vec<(&'static str, String)>| {
        Config::from_vars(move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.clone())
        })
    };
    assert!(with(vec![("NORN_OLAP_KEYS_FILE", keys.clone())]).is_ok());
    let retired = with(vec![
        ("NORN_OLAP_KEYS_FILE", keys.clone()),
        ("NORN_SECRET_DUCKDB_TOKEN", "x".repeat(40)),
    ]);
    assert!(retired.is_err_and(|e| e.contains("retired")));
    assert!(with(vec![]).is_err());
    assert!(with(vec![("NORN_OLAP_KEYS_FILE", format!("{keys}.missing"))]).is_err());
}

/// The cell's transport hands the host each call with a fresh credential for
/// the cell's own tenant, whatever the call's body names.
#[test]
fn the_exchange_mints_each_call_for_its_own_tenant() {
    let host = issuer("k1", 7);
    let keyring = Keyring::parse(&host.public_spec()).unwrap();
    let a = tenant("org_a/prod");
    let exchange = Exchange::new(issuer("k1", 7), a.clone());
    let call = EngineCall {
        op: "frontier".to_owned(),
        digest: "d".to_owned(),
        body: frontier(&tenant("org_b/prod")),
        stream: None,
        credential: None,
    };
    assert!(exchange.call(&call).is_none(), "the host performs it");
    let pending = exchange.take_pending(NOW).unwrap();
    let credential = pending.credential.unwrap();
    assert_eq!(keyring.verify(&credential, NOW), Ok(a));
    assert!(exchange.take_pending(NOW).is_none(), "taken once");
    let wire = serde_json::to_value(EngineCall {
        credential: Some(credential),
        ..call
    })
    .unwrap();
    assert!(
        wire["credential"]
            .as_str()
            .unwrap()
            .starts_with("nolap1.k1.")
    );
}
