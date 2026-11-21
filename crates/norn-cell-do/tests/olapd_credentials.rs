//! OLAP-CRED-1 (#1493): `norn-olapd` serves each cell for its own tenant
//! only, on the cell's own credential, over its real HTTP transport. A cell's
//! credential reads and writes nothing of another cell's database, and the
//! answer is byte-identical to the answer for a vacant scope (and for no
//! credential at all), so it reveals nothing about whether the other cell
//! exists. Expired and rotated-out credentials are refused, and rotation
//! needs no restart. A credential is good for the one operation and body it
//! was minted for (OLAP-03); the scope is checked before the request is
//! routed or parsed (OLAP-04); concurrent loads never share a scratch file
//! (OLAP-02); and the retired token refuses startup whatever its value
//! (OLAP-05).
#![cfg(all(feature = "duckdb", not(target_arch = "wasm32")))]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "credential fixtures must build before the daemon's answers are compared"
)]

use std::ffi::OsString;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use norn_api::analytics_routes::{EngineCall, Transport as _};
use norn_api::clock::ManualClock;
use norn_api::olap_credential::{
    CREDENTIAL_LIFETIME_MS, Issuer, Keyring, MAX_CREDENTIAL_LIFETIME_MS, body_digest,
};
use norn_cell::analytics::engine::Settings;
use norn_cell::analytics::{
    Column, ColumnKind, FileFormat, FileSource, LandRequest, LandResponse, LoadRequest,
    TableSchema, TenantId,
};
use norn_cell::dataset_manifest::ManifestPins;
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

/// What the cell mints for the one call `op` with `body`, for `of`, at `NOW`.
fn mint(issuer: &Issuer, of: &TenantId, op: &str, body: &str) -> String {
    issuer.mint(of, op, &body_digest(body.as_bytes()), NOW)
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
    std::thread::spawn(move || serve(&listener, config, serving));
    Daemon {
        addr,
        root,
        keys,
        clock,
        _directory: directory,
    }
}

impl Daemon {
    /// The raw response, status line, headers and body, to `<method> /v1/<op>`.
    fn send(&self, method: &str, op: &str, authorization: Option<&str>, body: &str) -> Vec<u8> {
        let mut stream = TcpStream::connect(self.addr).unwrap();
        let authorization = authorization
            .map(|value| format!("authorization: {value}\r\n"))
            .unwrap_or_default();
        write!(
            stream,
            "{method} /v1/{op} HTTP/1.1\r\nhost: olapd\r\n{authorization}content-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).unwrap();
        response
    }

    fn post(&self, op: &str, authorization: Option<&str>, body: &str) -> Vec<u8> {
        self.send("POST", op, authorization, body)
    }

    fn call(&self, op: &str, credential: &str, body: &str) -> Vec<u8> {
        self.post(op, Some(&format!("Bearer {credential}")), body)
    }

    /// `POST /v1/<op>` with the credential `issuer`'s cell for `of` mints for it.
    fn by(&self, issuer: &Issuer, of: &TenantId, op: &str, body: &str) -> Vec<u8> {
        self.call(op, &mint(issuer, of, op, body), body)
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
    let as_a = |op: &str, body: &str| daemon.by(&host, &a, op, body);
    let as_b = |op: &str, body: &str| daemon.by(&host, &b, op, body);

    // B exists and holds a mirror at commit 3, written with B's own credential.
    assert_eq!(status(&as_b("frontier", &frontier(&b))), "200");
    let synced = as_b("sync", &sync(&b, 0, 3));
    assert_eq!(status(&synced), "200", "{}", body(&synced));
    assert_eq!(body(&as_b("frontier", &frontier(&b))), r#"{"frontier":3}"#);
    // A's credential is good for A.
    assert_eq!(body(&as_a("frontier", &frontier(&a))), r#"{"frontier":0}"#);

    let uniform = refused(&daemon);
    assert_eq!(status(&uniform), "401");
    // Every body below parses: a refusal is the tenant check, never a 400.
    serde_json::from_str::<norn_cell::analytics::ExportRequest>(&export(&b)).unwrap();
    serde_json::from_str::<norn_cell::analytics::SyncRequest>(&sync(&b, 3, 9)).unwrap();
    assert_eq!(
        body(&uniform),
        r#"{"code":"analytics.unauthorized","detail":"the caller is not the cell"}"#
    );
    for (op, into_other, into_vacant) in [
        ("frontier", frontier(&b), frontier(&vacant)),
        ("sync", sync(&b, 3, 9), sync(&vacant, 3, 9)),
        ("export", export(&b), export(&vacant)),
    ] {
        let towards_other = as_a(op, &into_other);
        let towards_vacant = as_a(op, &into_vacant);
        assert_eq!(towards_other, towards_vacant, "{op}: B's existence shows");
        assert_eq!(
            towards_other, uniform,
            "{op}: unlike any other refused credential"
        );
        assert_eq!(
            daemon.post(op, Some("Bearer nonsense"), &into_other),
            uniform,
            "{op}"
        );
    }
    // Nothing was written or opened: B is still at 3, the vacant scope has no database.
    assert_eq!(body(&as_b("frontier", &frontier(&b))), r#"{"frontier":3}"#);
    assert!(daemon.database_exists(&b) && !daemon.database_exists(&vacant));
}

#[test]
fn an_expired_or_overlong_credential_is_refused() {
    let host = issuer("k1", 7);
    let daemon = daemon(&[&host]);
    let a = tenant("org_a/prod");
    let uniform = refused(&daemon);
    let credential = mint(&host, &a, "frontier", &frontier(&a));
    assert_eq!(
        status(&daemon.call("frontier", &credential, &frontier(&a))),
        "200"
    );
    daemon.clock.set(NOW + CREDENTIAL_LIFETIME_MS);
    assert_eq!(daemon.call("frontier", &credential, &frontier(&a)), uniform);
    daemon.clock.set(NOW);
    let overlong = host.mint_until(
        &a,
        "frontier",
        &body_digest(frontier(&a).as_bytes()),
        NOW + MAX_CREDENTIAL_LIFETIME_MS + 1,
    );
    assert_eq!(daemon.call("frontier", &overlong, &frontier(&a)), uniform);
}

#[test]
fn a_rotated_out_key_is_refused_and_rotation_needs_no_restart() {
    let (old, new) = (issuer("old", 1), issuer("new", 2));
    let daemon = daemon(&[&old]);
    let a = tenant("org_a/prod");
    let uniform = refused(&daemon);
    let (by_old, by_new) = (
        mint(&old, &a, "frontier", &frontier(&a)),
        mint(&new, &a, "frontier", &frontier(&a)),
    );
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
    let with = |pairs: Vec<(&'static str, OsString)>| {
        Config::from_vars(move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.clone())
        })
    };
    assert!(with(vec![("NORN_OLAP_KEYS_FILE", keys.clone().into())]).is_ok());
    // OLAP-05: present is present: an empty or non-UTF-8 value refuses too.
    let not_utf8 = {
        use std::os::unix::ffi::OsStringExt as _;
        OsString::from_vec(vec![b'x', 0xff, 0xfe])
    };
    for value in ["x".repeat(40).into(), OsString::new(), not_utf8] {
        let retired = with(vec![
            ("NORN_OLAP_KEYS_FILE", keys.clone().into()),
            ("NORN_SECRET_DUCKDB_TOKEN", value.clone()),
        ]);
        assert!(retired.is_err_and(|e| e.contains("retired")), "{value:?}");
    }
    assert!(with(vec![]).is_err());
    assert!(
        with(vec![(
            "NORN_OLAP_KEYS_FILE",
            format!("{keys}.missing").into()
        )])
        .is_err()
    );
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
    // OLAP-03: bound to the call's operation and the body the host sends,
    // not to the call's own `digest` field.
    let sent = body_digest(pending.body.as_bytes());
    assert_eq!(keyring.verify(&credential, "frontier", &sent, NOW), Ok(a));
    assert!(keyring.verify(&credential, "sync", &sent, NOW).is_err());
    assert!(keyring.verify(&credential, "frontier", "d", NOW).is_err());
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

/// OLAP-03: a credential is good for the one operation and body it was minted
/// for; a captured read's credential never authorizes a write, and it writes
/// nothing. Replaying the same request byte for byte within its life is the
/// documented residual window.
#[test]
fn a_credential_is_good_for_its_own_operation_and_body_only() {
    let host = issuer("k1", 7);
    let daemon = daemon(&[&host]);
    let a = tenant("org_a/prod");
    let uniform = refused(&daemon);
    let read = mint(&host, &a, "frontier", &frontier(&a));
    // Another operation (a write), and another body for the same operation.
    assert_eq!(daemon.call("sync", &read, &sync(&a, 0, 3)), uniform);
    let staging = json!({ "tenant": &a, "world": "staging", "tables": [] }).to_string();
    assert_eq!(daemon.call("frontier", &read, &staging), uniform);
    assert_eq!(daemon.call("export", &read, &export(&a)), uniform);
    // Nothing was written; the request it was minted for still answers.
    assert_eq!(
        body(&daemon.call("frontier", &read, &frontier(&a))),
        r#"{"frontier":0}"#
    );
}

/// OLAP-04: the scope is decided before the request is routed or parsed. A
/// malformed body, a body naming no tenant, an unknown operation and any
/// method but POST are refused exactly as garbage is, unless the cell minted
/// a credential for that very request and tenant.
#[test]
fn malformed_bodies_and_unknown_routes_are_refused_before_routing() {
    let host = issuer("k1", 7);
    let daemon = daemon(&[&host]);
    let (a, b) = (tenant("org_a/prod"), tenant("org_b/prod"));
    let uniform = refused(&daemon);
    let other_request = mint(&host, &a, "frontier", &frontier(&a));
    for (op, body) in [
        ("frontier", "not json".to_owned()),
        ("frontier", "{}".to_owned()),
        ("frontier", json!({ "world": WORLD }).to_string()),
        ("frontier", json!({ "tenant": &a }).to_string()),
        ("nosuch", frontier(&a)),
        ("", frontier(&a)),
    ] {
        // Garbage, and a real credential minted for another request: alike.
        assert_eq!(
            daemon.post(op, Some("Bearer nonsense"), &body),
            uniform,
            "{op} {body}"
        );
        assert_eq!(daemon.post(op, None, &body), uniform, "{op} {body}");
        assert_eq!(
            daemon.call(op, &other_request, &body),
            uniform,
            "{op} {body}"
        );
    }
    // Minted for exactly the request, but the scope fails: still the 401,
    // never the 400 or 404 the parse or the router would give.
    for (op, body) in [
        ("frontier", "not json".to_owned()),
        ("frontier", json!({ "world": WORLD }).to_string()),
        ("nosuch", frontier(&b)),
        ("frontier", r#"{"tenant":"nope"}"#.to_owned()),
    ] {
        assert_eq!(daemon.by(&host, &a, op, &body), uniform, "{op} {body}");
    }
    // Any method but POST, whatever the credential.
    let get = |authorization: Option<&str>| daemon.send("GET", "frontier", authorization, "");
    assert_eq!(get(None), uniform);
    assert_eq!(
        get(Some(&format!("Bearer {}", mint(&host, &a, "frontier", "")))),
        uniform
    );
    // Only the cell's own credential for that exact request and tenant gets
    // past the scope to the router and the typed parse.
    let unknown = daemon.by(&host, &a, "nosuch", &frontier(&a));
    assert_eq!(status(&unknown), "404");
    let untyped = json!({ "tenant": &a, "world": WORLD }).to_string();
    assert_eq!(status(&daemon.by(&host, &a, "frontier", &untyped)), "400");
    assert!(!daemon.database_exists(&b));
}

/// An object store serving `objects` by path, each response held half-sent
/// until every one is, so the daemon's downloads overlap.
fn overlapping_store(objects: Vec<(String, Vec<u8>)>) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let barrier = Arc::new(Barrier::new(objects.len()));
    let objects = Arc::new(objects);
    std::thread::spawn(move || {
        for stream in listener.incoming().take(objects.len()) {
            let (barrier, objects) = (Arc::clone(&barrier), Arc::clone(&objects));
            std::thread::spawn(move || {
                let mut stream = stream.unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let path = line.split_whitespace().nth(1).unwrap().to_owned();
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).unwrap();
                    if header.trim().is_empty() {
                        break;
                    }
                }
                let bytes = &objects.iter().find(|(p, _)| *p == path).unwrap().1;
                let half = bytes.len() / 2;
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    bytes.len()
                )
                .unwrap();
                stream.write_all(&bytes[..half]).unwrap();
                stream.flush().unwrap();
                barrier.wait();
                // Let the daemon write every first half before any second half.
                std::thread::sleep(Duration::from_millis(300));
                stream.write_all(&bytes[half..]).unwrap();
            });
        }
    });
    addr
}

/// OLAP-02 / MANIFEST-02: two tenants land at once under the same load id
/// and attempt. Each landing reads exactly its own bytes from its own
/// attempt's file, and a file already where the load id would once have put
/// it is neither read, truncated nor removed.
#[test]
fn concurrent_landings_with_the_same_attempt_never_share_bytes() {
    let host = issuer("k1", 7);
    let daemon = daemon(&[&host]);
    let (a, b) = (tenant("org_a/prod"), tenant("org_b/prod"));
    let csv = |prefix: &str, rows: usize| {
        use std::fmt::Write as _;
        let mut text = String::from("n\n");
        for i in 0..rows {
            writeln!(text, "{prefix}{i:06}").unwrap();
        }
        text.into_bytes()
    };
    let (bytes_a, bytes_b) = (csv("a", 4000), csv("b", 6000));
    let store = overlapping_store(vec![
        ("/a.csv".to_owned(), bytes_a.clone()),
        ("/b.csv".to_owned(), bytes_b.clone()),
    ]);
    let planted = daemon.root.join("tmp").join("load-same");
    std::fs::create_dir_all(daemon.root.join("tmp")).unwrap();
    std::fs::write(&planted, b"planted").unwrap();
    let request = |of: &TenantId, name: &str, bytes: &[u8]| LandRequest {
        load: LoadRequest {
            tenant: of.clone(),
            table: TableSchema {
                name: "shop.Sale".to_owned(),
                columns: vec![Column {
                    id: "n".to_owned(),
                    kind: ColumnKind::Text,
                    nullable: false,
                    laws: Vec::new(),
                }],
                pin: "p1".to_owned(),
            },
            load: "same".to_owned(),
            file: FileSource {
                url: format!("http://{store}/{name}.csv"),
                uri: format!("s3://uploads/{name}.csv"),
                sha256: body_digest(bytes),
                format: FileFormat::Csv,
            },
            headers: std::collections::BTreeMap::new(),
        },
        pins: ManifestPins {
            definitions_version: 1,
            model_hash: "model".to_owned(),
            binding: "binding".to_owned(),
            mapping: "mapping@sha".to_owned(),
            laws: "laws".to_owned(),
        },
        landing: "same".to_owned(),
        attempt: "same".to_owned(),
    };
    let loads = [
        (
            a.clone(),
            serde_json::to_string(&request(&a, "a", &bytes_a)).unwrap(),
        ),
        (
            b.clone(),
            serde_json::to_string(&request(&b, "b", &bytes_b)).unwrap(),
        ),
    ];
    let (on, minting) = (&daemon, &host);
    let answers: Vec<Vec<u8>> = std::thread::scope(|scope| {
        let ((a_of, a_body), (b_of, b_body)) = (&loads[0], &loads[1]);
        // Both loads are in flight before either is awaited.
        let first = scope.spawn(move || on.by(minting, a_of, "land", a_body));
        let second = scope.spawn(move || on.by(minting, b_of, "land", b_body));
        vec![first.join().unwrap(), second.join().unwrap()]
    });
    for (answer, rows) in answers.iter().zip([4000, 6000]) {
        assert_eq!(status(answer), "200", "{}", body(answer));
        let landed: LandResponse = serde_json::from_str(&body(answer)).unwrap();
        assert_eq!((landed.load.rows, landed.load.rejected), (rows, 0));
    }
    assert_eq!(std::fs::read(&planted).unwrap(), b"planted");
}
