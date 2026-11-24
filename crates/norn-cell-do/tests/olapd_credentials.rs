//! OLAP-CRED-1 (#1493): `norn-olapd` serves each cell for its own tenant
//! only, on the cell's own credential, over its real HTTP transport. A cell's
//! credential reads and writes nothing of another cell's database, and the
//! answer is byte-identical to the answer for a vacant scope (and for no
//! credential at all), so it reveals nothing about whether the other cell
//! exists. Expired and rotated-out credentials are refused, and rotation
//! needs no restart. A credential is good for the one operation and body it
//! was minted for (OLAP-03); the scope is checked before the request is
//! routed or parsed (OLAP-04); concurrent loads never share a scratch file,
//! and a landing attempt is the daemon's to name and has one owner at a
//! time, so no store removes another landing's files or a running download
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
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Barrier, Mutex};
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
    StoreRequest, StoredObject, TableSchema, TenantId,
};
use norn_cell::dataset_manifest::ManifestPins;
use norn_cell_do::olap::Exchange;
use norn_cell_do::olapd::{Config, NoSeam, Seam, serve_with};
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
    daemon_with(issuers, Arc::new(NoSeam))
}

/// [`daemon`], announcing its seam points to `seam`.
fn daemon_with(issuers: &[&Issuer], seam: Arc<dyn Seam>) -> Daemon {
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
    std::thread::spawn(move || serve_with(&listener, config, serving, seam));
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
        send_to(self.addr, method, op, authorization, body)
    }

    fn post(&self, op: &str, authorization: Option<&str>, body: &str) -> Vec<u8> {
        self.send("POST", op, authorization, body)
    }

    fn call(&self, op: &str, credential: &str, body: &str) -> Vec<u8> {
        self.post(op, Some(&format!("Bearer {credential}")), body)
    }

    /// `POST /v1/<op>` with the credential `issuer`'s cell for `of` mints for it.
    fn by(&self, issuer: &Issuer, of: &TenantId, op: &str, body: &str) -> Vec<u8> {
        by_at(self.addr, issuer, of, op, body)
    }

    fn database_exists(&self, tenant: &TenantId) -> bool {
        self.root
            .join(format!("{}.duckdb", tenant.as_str()))
            .exists()
    }

    /// The landing attempts of `of` on disk.
    fn attempts(&self, of: &TenantId) -> Vec<String> {
        let scratch = self.root.join("t").join(of.as_str()).join("scratch");
        let mut names: Vec<String> = std::fs::read_dir(scratch).map_or_default(|entries| {
            entries
                .map(|entry| entry.unwrap().file_name().into_string().unwrap())
                .collect()
        });
        names.sort();
        names
    }
}

/// `POST /v1/<op>` to the daemon at `addr`, with `issuer`'s credential for `of`.
fn by_at(addr: SocketAddr, issuer: &Issuer, of: &TenantId, op: &str, body: &str) -> Vec<u8> {
    let credential = mint(issuer, of, op, body);
    send_to(
        addr,
        "POST",
        op,
        Some(&format!("Bearer {credential}")),
        body,
    )
}

/// The raw response, status line, headers and body, to `<method> /v1/<op>`.
fn send_to(
    addr: SocketAddr,
    method: &str,
    op: &str,
    authorization: Option<&str>,
    body: &str,
) -> Vec<u8> {
    let mut stream = TcpStream::connect(addr).unwrap();
    let authorization = authorization.map_or_default(|value| format!("authorization: {value}\r\n"));
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

fn status(response: &[u8]) -> &str {
    std::str::from_utf8(response.get(9..12).unwrap_or_default()).unwrap_or_default()
}

fn body(response: &[u8]) -> String {
    let text = String::from_utf8_lossy(response);
    text.split_once("\r\n\r\n")
        .map_or_default(|(_, body)| body.to_owned())
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
/// and landing identity. Each landing reads exactly its own bytes from its
/// own attempt's file, and a file already where the load id would once have
/// put it is neither read, truncated nor removed.
#[test]
fn concurrent_landings_of_the_same_load_never_share_bytes() {
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
        attempt: String::new(),
        checks: Vec::new(),
        residue_page: 64,
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

/// A landing of `bytes` (one text column `n`) by `of`, downloaded from `url`.
fn land_request(of: &TenantId, url: String, bytes: &[u8]) -> LandRequest {
    LandRequest {
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
            load: format!("l{}", body_digest(bytes)),
            file: FileSource {
                url,
                uri: "s3://uploads/sale.csv".to_owned(),
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
        landing: format!("landing-{}", body_digest(bytes)),
        attempt: String::new(),
        checks: Vec::new(),
        residue_page: 64,
    }
}

/// The store of `landed`, the answer of `land`, to objects on `store`.
fn store_request(store: SocketAddr, land: &LandRequest, landed: &LandResponse) -> StoreRequest {
    let object = |sha: &str| StoredObject {
        uri: format!("s3://datasets/{sha}.parquet"),
        url: format!("http://{store}/put/{sha}"),
    };
    StoreRequest {
        land: LandRequest {
            attempt: landed.attempt.clone(),
            ..land.clone()
        },
        landed: landed.clone(),
        data: object(&landed.data.as_ref().unwrap().sha256),
        held: object(&landed.held.as_ref().unwrap().sha256),
    }
}

/// An object store: `GET /<name>` serves `objects[name]`, and `PUT` keeps
/// the body under its path. A `GET` of `gated` sends half its bytes, says so
/// on `started`, and sends the rest only once `release` is signalled, so a
/// test runs calls while that download is in flight, deterministically.
struct ObjectStore {
    addr: SocketAddr,
    puts: Arc<Mutex<std::collections::BTreeMap<String, Vec<u8>>>>,
}

fn object_store(
    objects: Vec<(String, Vec<u8>)>,
    gated: Option<(String, Sender<()>, Receiver<()>)>,
) -> ObjectStore {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let puts = Arc::new(Mutex::new(std::collections::BTreeMap::new()));
    let kept = Arc::clone(&puts);
    let gated = Arc::new(Mutex::new(gated));
    let objects = Arc::new(objects);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let (objects, kept, gated) =
                (Arc::clone(&objects), Arc::clone(&kept), Arc::clone(&gated));
            std::thread::spawn(move || {
                let mut stream = stream.unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let mut parts = line.split_whitespace();
                let (method, path) = (
                    parts.next().unwrap().to_owned(),
                    parts.next().unwrap().to_owned(),
                );
                let mut length = 0_usize;
                loop {
                    let mut header = String::new();
                    reader.read_line(&mut header).unwrap();
                    if header.trim().is_empty() {
                        break;
                    }
                    if let Some((name, value)) = header.split_once(':')
                        && name.eq_ignore_ascii_case("content-length")
                    {
                        length = value.trim().parse().unwrap();
                    }
                }
                if method == "PUT" {
                    let mut sent = vec![0_u8; length];
                    reader.read_exact(&mut sent).unwrap();
                    kept.lock().unwrap().insert(path, sent);
                    stream
                        .write_all(
                            b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
                        )
                        .unwrap();
                    return;
                }
                let name = path.trim_start_matches('/');
                let bytes = &objects.iter().find(|(n, _)| n == name).unwrap().1;
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    bytes.len()
                )
                .unwrap();
                let gate = {
                    let mut gated = gated.lock().unwrap();
                    if gated.as_ref().is_some_and(|(g, _, _)| g == name) {
                        gated.take()
                    } else {
                        None
                    }
                };
                match gate {
                    Some((_, started, release)) => {
                        let half = bytes.len() / 2;
                        stream.write_all(&bytes[..half]).unwrap();
                        stream.flush().unwrap();
                        started.send(()).unwrap();
                        release.recv().unwrap();
                        stream.write_all(&bytes[half..]).unwrap();
                    }
                    None => stream.write_all(bytes).unwrap(),
                }
            });
        }
    });
    ObjectStore { addr, puts }
}

fn rows_csv(prefix: &str, rows: usize) -> Vec<u8> {
    use std::fmt::Write as _;
    let mut text = String::from("n\n");
    for i in 0..rows {
        writeln!(text, "{prefix}{i:06}").unwrap();
    }
    text.into_bytes()
}

/// Runs `then` at the first `/v1/store` that returned from the engine.
#[derive(Default)]
struct AfterStore(Mutex<Option<Box<dyn FnOnce() + Send>>>);

impl Seam for AfterStore {
    fn stored(&self, _tenant: &TenantId, _stored: bool) {
        let then = self.0.lock().unwrap().take();
        if let Some(then) = then {
            then();
        }
    }
}

/// OLAP-02: one tenant, two landings. Landing B is made at the exact point
/// where store A has returned from the engine and its answer is not yet
/// sent, the window in which A's cleanup once removed its attempt a second
/// time, by name. B cannot take A's attempt: the daemon names every attempt
/// and refuses a caller-named one, and the token it mints for B is B's
/// alone. B's files survive A's store and B stores them.
#[test]
fn a_store_never_removes_a_landing_made_while_it_finishes() {
    let host = issuer("k1", 7);
    let seam = Arc::new(AfterStore::default());
    let shared: Arc<dyn Seam> = Arc::<AfterStore>::clone(&seam);
    let daemon = daemon_with(&[&host], shared);
    let of = tenant("org_a/prod");
    let (bytes_a, bytes_b) = (rows_csv("a", 300), rows_csv("b", 500));
    let store = object_store(
        vec![
            ("a.csv".to_owned(), bytes_a.clone()),
            ("b.csv".to_owned(), bytes_b.clone()),
        ],
        None,
    );
    let land_a = land_request(&of, format!("http://{}/a.csv", store.addr), &bytes_a);
    let answer = daemon.by(&host, &of, "land", &serde_json::to_string(&land_a).unwrap());
    assert_eq!(status(&answer), "200", "{}", body(&answer));
    let landed_a: LandResponse = serde_json::from_str(&body(&answer)).unwrap();
    assert_ne!(landed_a.attempt.len(), 0, "the daemon names the attempt");
    // Landing B, run inside store A's window.
    let land_b = land_request(&of, format!("http://{}/b.csv", store.addr), &bytes_b);
    let reused = LandRequest {
        attempt: landed_a.attempt.clone(),
        ..land_b.clone()
    };
    let b_answers = Arc::new(Mutex::new(Vec::new()));
    {
        let (addr, answers, of) = (daemon.addr, Arc::clone(&b_answers), of.clone());
        let (reused, fresh) = (
            serde_json::to_string(&reused).unwrap(),
            serde_json::to_string(&land_b).unwrap(),
        );
        *seam.0.lock().unwrap() = Some(Box::new(move || {
            let minting = issuer("k1", 7);
            let mut answers = answers.lock().unwrap();
            answers.push(by_at(addr, &minting, &of, "land", &reused));
            answers.push(by_at(addr, &minting, &of, "land", &fresh));
        }));
    }
    let stored_a = daemon.by(
        &host,
        &of,
        "store",
        &serde_json::to_string(&store_request(store.addr, &land_a, &landed_a)).unwrap(),
    );
    assert_eq!(status(&stored_a), "200", "{}", body(&stored_a));
    let b_answers = b_answers.lock().unwrap().clone();
    let [reused_answer, fresh_answer] = <[Vec<u8>; 2]>::try_from(b_answers).unwrap();
    assert_eq!(
        status(&reused_answer),
        "400",
        "a caller never names an attempt: {}",
        body(&reused_answer)
    );
    assert_eq!(status(&fresh_answer), "200", "{}", body(&fresh_answer));
    let landed_b: LandResponse = serde_json::from_str(&body(&fresh_answer)).unwrap();
    assert_ne!(landed_b.attempt, landed_a.attempt);
    assert_eq!(
        daemon.attempts(&of),
        vec![landed_b.attempt.clone()],
        "B's attempt survives A's store"
    );
    let stored_b = daemon.by(
        &host,
        &of,
        "store",
        &serde_json::to_string(&store_request(store.addr, &land_b, &landed_b)).unwrap(),
    );
    assert_eq!(status(&stored_b), "200", "{}", body(&stored_b));
    let puts = store.puts.lock().unwrap();
    assert!(puts.contains_key(&format!("/put/{}", landed_b.data.as_ref().unwrap().sha256)));
    assert_eq!(daemon.attempts(&of).len(), 0);
}

/// OLAP-02: a store of an attempt whose download is still running, first
/// or replayed, is refused and touches nothing: the download finishes, the
/// landing answers its rows, and its own store then succeeds.
#[test]
fn a_store_during_a_download_leaves_the_download_intact() {
    let host = issuer("k1", 7);
    let daemon = daemon(&[&host]);
    let of = tenant("org_a/prod");
    let bytes = rows_csv("s", 4000);
    let (started_tx, started) = channel();
    let (release, release_rx) = channel();
    let store = object_store(
        vec![("slow.csv".to_owned(), bytes.clone())],
        Some(("slow.csv".to_owned(), started_tx, release_rx)),
    );
    let land = land_request(&of, format!("http://{}/slow.csv", store.addr), &bytes);
    let land_body = serde_json::to_string(&land).unwrap();
    let addr = daemon.addr;
    let (landing_of, landing_body) = (of.clone(), land_body);
    let landing = std::thread::spawn(move || {
        by_at(addr, &issuer("k1", 7), &landing_of, "land", &landing_body)
    });
    started.recv().unwrap();
    // The download is half written. A racing store can know only the
    // attempt's directory; it names that and the landing.
    let [token] = <[String; 1]>::try_from(daemon.attempts(&of)).unwrap();
    let file = |sha: &str| norn_cell::analytics::LandedFile {
        sha256: sha.repeat(32),
        rows: 1,
        bytes: 1,
    };
    let early = LandResponse {
        landing: land.landing.clone(),
        attempt: token.clone(),
        load: norn_cell::analytics::LoadResponse {
            load: land.load.load.clone(),
            rows: 1,
            rejected: 0,
            reasons: std::collections::BTreeMap::new(),
            bytes: 1,
            millis: 0,
        },
        data: Some(file("ab")),
        held: Some(file("cd")),
        valid_time: Vec::new(),
        findings: std::collections::BTreeMap::new(),
        held_by_rules: 0,
        residue_total: 0,
        residue: Vec::new(),
        stored: None,
    };
    let early = serde_json::to_string(&store_request(store.addr, &land, &early)).unwrap();
    for _replay in 0..2 {
        let refused = daemon.by(&host, &of, "store", &early);
        assert_eq!(status(&refused), "422", "{}", body(&refused));
        assert!(
            body(&refused).contains("bulk.attempt_busy"),
            "{}",
            body(&refused)
        );
        assert_eq!(
            daemon.attempts(&of),
            vec![token.clone()],
            "the download is intact"
        );
    }
    release.send(()).unwrap();
    let answer = landing.join().unwrap();
    assert_eq!(status(&answer), "200", "{}", body(&answer));
    let landed: LandResponse = serde_json::from_str(&body(&answer)).unwrap();
    assert_eq!(
        (landed.attempt.as_str(), landed.load.rows),
        (token.as_str(), 4000)
    );
    let stored = daemon.by(
        &host,
        &of,
        "store",
        &serde_json::to_string(&store_request(store.addr, &land, &landed)).unwrap(),
    );
    assert_eq!(status(&stored), "200", "{}", body(&stored));
    // A replay of the finished store answers alike and removes nothing.
    let replayed = daemon.by(
        &host,
        &of,
        "store",
        &serde_json::to_string(&store_request(store.addr, &land, &landed)).unwrap(),
    );
    assert_eq!(
        (status(&replayed), body(&replayed)),
        (status(&stored), body(&stored))
    );
    assert_eq!(daemon.attempts(&of).len(), 0);
}

fn send_path(
    addr: SocketAddr,
    method: &str,
    path: &str,
    authorization: Option<&str>,
    body: &str,
) -> Vec<u8> {
    let mut stream = TcpStream::connect(addr).unwrap();
    let authorization = authorization.map_or_default(|value| format!("authorization: {value}\r\n"));
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nhost: olapd\r\n{authorization}content-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).unwrap();
    response
}

fn by_v2(addr: SocketAddr, issuer: &Issuer, of: &TenantId, op: &str, body: &str) -> Vec<u8> {
    let full_op = format!("v2/{op}");
    let credential = mint(issuer, of, &full_op, body);
    send_path(
        addr,
        "POST",
        &format!("/v2/{op}"),
        Some(&format!("Bearer {credential}")),
        body,
    )
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LegacyEvaluateWire {
    population: u64,
    excluded: Vec<String>,
    buckets: Vec<serde_json::Value>,
    #[serde(default)]
    unlinked: u64,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LegacyProfileWire {
    rows: u64,
    quarantined: u64,
    columns: Vec<serde_json::Value>,
}

struct MissingProvenanceEngine;

impl norn_cell::analytics::AnalyticalEngine for MissingProvenanceEngine {
    fn engine_id(&self) -> norn_cell::analytics::AnalyticalEngineId {
        norn_cell::analytics::AnalyticalEngineId::DataFusion
    }
    fn capabilities(&self) -> norn_cell::analytics::EngineCapabilities {
        norn_cell::analytics::EngineCapabilities::duckdb_default()
    }
    fn mirror_frontier(
        &self,
        _request: &norn_cell::analytics::FrontierRequest,
    ) -> norn_cell::analytics::EngineOutcome<norn_cell::analytics::FrontierResponse> {
        Ok(norn_cell::analytics::FrontierResponse { frontier: 0 })
    }
    fn mirror_sync(
        &self,
        _request: &norn_cell::analytics::SyncRequest,
    ) -> norn_cell::analytics::EngineOutcome<norn_cell::analytics::FrontierResponse> {
        Ok(norn_cell::analytics::FrontierResponse { frontier: 0 })
    }
    fn evaluate_question(
        &self,
        _request: &norn_cell::analytics::EvaluateRequest,
    ) -> norn_cell::analytics::EngineOutcome<norn_cell::analytics::EvaluateResponse> {
        Ok(norn_cell::analytics::EvaluateResponse {
            population: 0,
            excluded: Vec::new(),
            buckets: Vec::new(),
            unlinked: 0,
            provenance: None,
        })
    }
    fn query_members(
        &self,
        _request: &norn_cell::analytics::MembersRequest,
    ) -> norn_cell::analytics::EngineOutcome<norn_cell::analytics::MembersResponse> {
        Ok(norn_cell::analytics::MembersResponse {
            members: Vec::new(),
        })
    }
    fn load_dataset(
        &self,
        _request: &LoadRequest,
        _file: &Path,
    ) -> norn_cell::analytics::EngineOutcome<norn_cell::analytics::LoadResponse> {
        Err(norn_cell::analytics::EngineRefusal {
            code: "analytics.engine".to_owned(),
            detail: "stub".to_owned(),
        })
    }
    fn land_dataset(
        &self,
        _request: &LandRequest,
        _fetch: &mut dyn FnMut(std::fs::File) -> norn_cell::analytics::EngineOutcome<()>,
    ) -> norn_cell::analytics::EngineOutcome<LandResponse> {
        Err(norn_cell::analytics::EngineRefusal {
            code: "analytics.engine".to_owned(),
            detail: "stub".to_owned(),
        })
    }
    fn settle_dataset(
        &self,
        _request: &norn_cell::analytics::SettleRequest,
    ) -> norn_cell::analytics::EngineOutcome<LandResponse> {
        Err(norn_cell::analytics::EngineRefusal {
            code: "analytics.engine".to_owned(),
            detail: "stub".to_owned(),
        })
    }
    fn store_dataset(
        &self,
        _request: &StoreRequest,
        _send: &mut dyn FnMut(&Path, &Path) -> norn_cell::analytics::EngineOutcome<()>,
    ) -> norn_cell::analytics::EngineOutcome<LandResponse> {
        Err(norn_cell::analytics::EngineRefusal {
            code: "analytics.engine".to_owned(),
            detail: "stub".to_owned(),
        })
    }
    fn store_dataset_json(
        &self,
        _body: &norn_cell::analytics::StoreBody,
        _send: &mut dyn FnMut(&Path, &Path) -> norn_cell::analytics::EngineOutcome<()>,
    ) -> norn_cell::analytics::EngineOutcome<Box<serde_json::value::RawValue>> {
        Err(norn_cell::analytics::EngineRefusal {
            code: "analytics.engine".to_owned(),
            detail: "stub".to_owned(),
        })
    }
    fn query_lineage(
        &self,
        _request: &norn_cell::analytics::LineageRequest,
    ) -> norn_cell::analytics::EngineOutcome<norn_cell::analytics::LineageResponse> {
        Err(norn_cell::analytics::EngineRefusal {
            code: "analytics.engine".to_owned(),
            detail: "stub".to_owned(),
        })
    }
    fn query_page(
        &self,
        _request: &norn_cell::analytics::PageRequest,
    ) -> norn_cell::analytics::EngineOutcome<norn_cell::analytics::PageResponse> {
        Err(norn_cell::analytics::EngineRefusal {
            code: "analytics.engine".to_owned(),
            detail: "stub".to_owned(),
        })
    }
    fn profile_objects(
        &self,
        _request: &norn_cell::analytics::ProfileRequest,
    ) -> norn_cell::analytics::EngineOutcome<norn_cell::analytics::ProfileResponse> {
        Ok(norn_cell::analytics::ProfileResponse {
            rows: 0,
            quarantined: 0,
            columns: Vec::new(),
            provenance: None,
        })
    }
    fn export_dataset(
        &self,
        _request: &norn_cell::analytics::ExportRequest,
    ) -> norn_cell::analytics::EngineOutcome<PathBuf> {
        Err(norn_cell::analytics::EngineRefusal {
            code: "analytics.engine".to_owned(),
            detail: "stub".to_owned(),
        })
    }
    fn admit_transform(
        &self,
        _plan: &norn_checker::transform::TransformPlan,
    ) -> Result<Box<dyn norn_cell::analytics::AdmittedTransform>, norn_cell::CellError> {
        Err(norn_cell::CellError::ComputeRefused(
            "core.compute.unsupported_engine",
        ))
    }
}

fn compat_bodies(of: &TenantId) -> (String, String) {
    let source = norn_cell::analytics::Source::Mirror {
        world: WORLD.to_owned(),
        table: "shop.Sale".to_owned(),
        frontier: 0,
        scope: "org_compat/prod".to_owned(),
        excluded: Vec::new(),
        model: None,
    };
    let eval_body = serde_json::to_string(&norn_cell::analytics::EvaluateRequest {
        tenant: of.clone(),
        source: source.clone(),
        query: norn_cell::analytics::Lowered {
            filters: Vec::new(),
            window: norn_cell::analytics::Window {
                column: "occurred_at".to_owned(),
                start: (1_700_000_000, 0),
                end: (1_900_000_000, 0),
            },
            dimensions: Vec::new(),
            operands: vec![norn_cell::analytics::Operand {
                column: "amount".to_owned(),
                filters: Vec::new(),
            }],
            as_of: None,
            time_link: None,
        },
        inline_members: 100,
    })
    .unwrap();
    let profile_body = serde_json::to_string(&norn_cell::analytics::ProfileRequest {
        tenant: of.clone(),
        source,
        columns: vec!["amount".to_owned()],
    })
    .unwrap();
    (eval_body, profile_body)
}

#[test]
fn olapd_v1_omits_provenance_for_legacy_consumers_and_v2_enforces_it() {
    let host = issuer("k1", 77);
    let daemon = daemon(&[&host]);
    let of = tenant("org_compat/prod");
    let schema_pin = "pin-compat-v1";
    let init = serde_json::to_string(&norn_cell::analytics::FrontierRequest {
        tenant: of.clone(),
        world: WORLD.to_owned(),
        tables: vec![TableSchema {
            name: "shop.Sale".to_owned(),
            pin: schema_pin.to_owned(),
            columns: vec![
                Column {
                    id: "amount".to_owned(),
                    kind: ColumnKind::Decimal { scale: 2 },
                    nullable: false,
                    laws: Vec::new(),
                },
                Column {
                    id: "occurred_at".to_owned(),
                    kind: ColumnKind::Timestamp,
                    nullable: false,
                    laws: Vec::new(),
                },
            ],
        }],
        links: Vec::new(),
    })
    .unwrap();
    let initialized = daemon.by(&host, &of, "frontier", &init);
    assert_eq!(status(&initialized), "200", "{}", body(&initialized));
    let (eval_body, profile_body) = compat_bodies(&of);

    // (a) Legacy /v1/evaluate and /v1/profile omit `provenance` and decode under legacy deny_unknown_fields.
    let v1_eval = daemon.by(&host, &of, "evaluate", &eval_body);
    assert_eq!(status(&v1_eval), "200", "{}", body(&v1_eval));
    assert!(!body(&v1_eval).contains("provenance"), "{}", body(&v1_eval));
    let decoded_v1_eval: LegacyEvaluateWire = serde_json::from_str(&body(&v1_eval)).unwrap();
    assert_eq!(decoded_v1_eval.population, 0);

    let v1_prof = daemon.by(&host, &of, "profile", &profile_body);
    assert_eq!(status(&v1_prof), "200", "{}", body(&v1_prof));
    assert!(!body(&v1_prof).contains("provenance"), "{}", body(&v1_prof));
    let decoded_v1_prof: LegacyProfileWire = serde_json::from_str(&body(&v1_prof)).unwrap();
    assert_eq!(decoded_v1_prof.rows, 0);

    // OLAP-03: a v1 credential never authorizes a v2 route.
    let v1_cred = mint(&host, &of, "evaluate", &eval_body);
    let cross = send_path(
        daemon.addr,
        "POST",
        "/v2/evaluate",
        Some(&format!("Bearer {v1_cred}")),
        &eval_body,
    );
    assert_eq!(status(&cross), "401", "{}", body(&cross));

    // (b) /v2/evaluate and /v2/profile emit and verify full provenance including schema_pin & Exact freshness.
    let v2_eval = by_v2(daemon.addr, &host, &of, "evaluate", &eval_body);
    assert_eq!(status(&v2_eval), "200", "{}", body(&v2_eval));
    let eval_resp: norn_cell::analytics::EvaluateResponse =
        serde_json::from_str(&body(&v2_eval)).unwrap();
    let eval_prov = eval_resp
        .provenance
        .expect("v2 evaluate carries provenance");
    assert_eq!(
        eval_prov.engine,
        norn_cell::analytics::AnalyticalEngineId::DuckDb
    );
    assert_eq!(
        eval_prov.freshness,
        Some(norn_cell::compute::Freshness::Exact)
    );
    let expected_source = norn_cell::analytics::Source::Mirror {
        world: WORLD.to_owned(),
        table: "shop.Sale".to_owned(),
        frontier: 0,
        scope: "org_compat/prod".to_owned(),
        excluded: Vec::new(),
        model: None,
    };
    assert!(
        eval_prov
            .verify_against(&of, &expected_source, Some(schema_pin))
            .is_ok()
    );

    let v2_prof = by_v2(daemon.addr, &host, &of, "profile", &profile_body);
    assert_eq!(status(&v2_prof), "200", "{}", body(&v2_prof));
    let prof_resp: norn_cell::analytics::ProfileResponse =
        serde_json::from_str(&body(&v2_prof)).unwrap();
    let prof_prov = norn_cell::analytics::verify_profile_provenance(
        &prof_resp,
        &of,
        &expected_source,
        Some(schema_pin),
    )
    .expect("v2 profile provenance verifies");
    assert_eq!(
        prof_prov.engine,
        norn_cell::analytics::AnalyticalEngineId::DuckDb
    );
}

#[test]
fn olapd_v2_refuses_missing_provenance_fail_closed() {
    let host = issuer("k1", 78);
    let of = tenant("org_compat/prod");
    let (eval_body, profile_body) = compat_bodies(&of);
    let stub_dir = tempfile::tempdir().unwrap();
    let stub_keys = stub_dir.path().join("keyring");
    write_keyring(&stub_keys, &[&host]);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let stub_addr = listener.local_addr().unwrap();
    let stub_clock = Arc::new(ManualClock::at(NOW));
    let stub_engine: Arc<dyn norn_cell::analytics::AnalyticalEngine> =
        Arc::new(MissingProvenanceEngine);
    std::thread::spawn(move || {
        norn_cell_do::olapd::serve_engine(
            &listener,
            stub_keys,
            &stub_engine,
            stub_clock,
            Arc::new(NoSeam),
        )
    });
    // Missing provenance on /v2/evaluate and /v2/profile fails closed.
    let stub_v2_eval = by_v2(stub_addr, &host, &of, "evaluate", &eval_body);
    assert_eq!(status(&stub_v2_eval), "500", "{}", body(&stub_v2_eval));
    let stub_v2_prof = by_v2(stub_addr, &host, &of, "profile", &profile_body);
    assert_eq!(status(&stub_v2_prof), "500", "{}", body(&stub_v2_prof));
    // Legacy /v1/evaluate and /v1/profile still answer cleanly without provenance.
    let stub_v1_eval = by_at(stub_addr, &host, &of, "evaluate", &eval_body);
    assert_eq!(status(&stub_v1_eval), "200", "{}", body(&stub_v1_eval));
    let stub_v1_prof = by_at(stub_addr, &host, &of, "profile", &profile_body);
    assert_eq!(status(&stub_v1_prof), "200", "{}", body(&stub_v1_prof));
}
