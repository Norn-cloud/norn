//! `norn-olapd` (D102): the tenant analytics engine as a native sidecar next
//! to the cell host. It serves the typed operations of
//! [`norn_cell::analytics`] over a bounded HTTP/1.1 transport to one caller,
//! the cell, which proves itself with a shared bearer token. It decides
//! nothing about who may read: the cell authorizes every request before it
//! sends it, names the tenant from the scope it verified, and checks every
//! answer. It holds no storage credentials; a load reads a presigned URL the
//! cell minted for that one object.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use norn_cell::analytics::engine::{Engine, Outcome, Settings};
use norn_cell::analytics::{
    EngineRefusal, EvaluateRequest, ExportRequest, FrontierRequest, LandRequest, LandResponse,
    LineageRequest, MembersRequest, PageRequest, ProfileRequest, SettleRequest, StoreRequest,
    SyncRequest,
};
use serde::Serialize;
use serde::de::DeserializeOwned;

/// Longest accepted request head, in bytes.
const MAX_HEAD_BYTES: usize = 16 * 1024;
/// Longest accepted body (a sync batch), in bytes.
const MAX_BODY_BYTES: usize = 64 * 1024 * 1024;
/// Largest dataset file a load downloads, in bytes.
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024 * 1024;
/// Connections served at once; more are answered 503.
const MAX_CONNECTIONS: usize = 8;

/// The daemon's configuration, read from its environment.
pub struct Config {
    /// Listen address, `NORN_OLAP_ADDR` (default `0.0.0.0:8790`).
    pub addr: String,
    /// Shared bearer token, `NORN_SECRET_DUCKDB_TOKEN`, at least 32 bytes.
    pub token: String,
    /// Engine settings: `NORN_OLAP_DATA`, `NORN_OLAP_MEMORY_LIMIT`, `NORN_OLAP_THREADS`.
    pub settings: Settings,
}

impl Config {
    /// Read the configuration, refusing a missing or short token.
    ///
    /// # Errors
    /// A missing or short token, or an unreadable thread count.
    pub fn from_env() -> Result<Self, String> {
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        let token = var("NORN_SECRET_DUCKDB_TOKEN")
            .filter(|t| t.len() >= 32)
            .ok_or("NORN_SECRET_DUCKDB_TOKEN must hold at least 32 bytes")?;
        let threads = var("NORN_OLAP_THREADS")
            .map(|t| {
                t.parse::<u32>()
                    .map_err(|e| format!("NORN_OLAP_THREADS: {e}"))
            })
            .transpose()?
            .unwrap_or(1);
        Ok(Self {
            addr: var("NORN_OLAP_ADDR").unwrap_or_else(|| "0.0.0.0:8790".to_owned()),
            token,
            settings: Settings {
                root: PathBuf::from(var("NORN_OLAP_DATA").unwrap_or_else(|| "/data".to_owned())),
                memory_limit: var("NORN_OLAP_MEMORY_LIMIT").unwrap_or_else(|| "512MB".to_owned()),
                threads,
            },
        })
    }
}

/// Whether the daemon on this host's `NORN_OLAP_ADDR` port answers
/// `GET /healthz` with 200, within two seconds (the container health check).
#[must_use]
pub fn healthy() -> bool {
    let addr = std::env::var("NORN_OLAP_ADDR").unwrap_or_else(|_| "0.0.0.0:8790".to_owned());
    let port = addr.rsplit(':').next().unwrap_or("8790");
    let Ok(mut stream) = TcpStream::connect(format!("127.0.0.1:{port}")) else {
        return false;
    };
    let _bounded = stream.set_read_timeout(Some(Duration::from_secs(2)));
    if stream
        .write_all(b"GET /healthz HTTP/1.1\r\nhost: localhost\r\nconnection: close\r\n\r\n")
        .is_err()
    {
        return false;
    }
    let mut head = [0_u8; 12];
    stream.read_exact(&mut head).is_ok() && head.starts_with(b"HTTP/1.1 200")
}

/// Serve until the process exits.
///
/// # Errors
/// The engine directory or the listener cannot be opened.
pub fn run(config: Config) -> std::io::Result<()> {
    let engine = Arc::new(Engine::new(config.settings)?);
    let token = Arc::new(config.token);
    let listener = TcpListener::bind(&config.addr)?;
    let active = Arc::new(AtomicUsize::new(0));
    for stream in listener.incoming().flatten() {
        if active.fetch_add(1, Ordering::SeqCst) >= MAX_CONNECTIONS {
            active.fetch_sub(1, Ordering::SeqCst);
            let _busy = respond(
                &stream,
                503,
                "application/json",
                b"{\"code\":\"analytics.busy\"}",
            );
            continue;
        }
        let (engine, token, active) =
            (Arc::clone(&engine), Arc::clone(&token), Arc::clone(&active));
        std::thread::spawn(move || {
            // A broken connection affects only itself.
            let _ignored = connection(&stream, &engine, &token);
            active.fetch_sub(1, Ordering::SeqCst);
        });
    }
    Ok(())
}

struct Request {
    method: String,
    path: String,
    authorization: Option<String>,
    body: Vec<u8>,
}

fn respond(mut stream: &TcpStream, status: u16, kind: &str, body: &[u8]) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status} {}\r\ncontent-type: {kind}\r\ncache-control: no-store\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        reason(status),
        body.len()
    )?;
    stream.write_all(body)?;
    stream.flush()
}

const fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        409 => "Conflict",
        413 => "Payload Too Large",
        422 => "Unprocessable Content",
        503 => "Service Unavailable",
        _ => "Internal Server Error",
    }
}

fn connection(stream: &TcpStream, engine: &Engine, token: &str) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    let request = match read_request(stream) {
        Ok(request) => request,
        Err((status, refused)) => return refuse(stream, status, &refused),
    };
    if request.method == "GET" && request.path == "/healthz" {
        return respond(stream, 200, "application/json", b"{\"ok\":true}");
    }
    if !authorized(request.authorization.as_deref(), token) {
        return refuse(
            stream,
            401,
            &refusal("analytics.unauthorized", "the caller is not the cell"),
        );
    }
    if request.method != "POST" {
        return refuse(
            stream,
            404,
            &refusal("analytics.unknown", "no such operation"),
        );
    }
    match request.path.as_str() {
        "/v1/frontier" => answer(stream, &request, |r: FrontierRequest| engine.frontier(&r)),
        "/v1/sync" => answer(stream, &request, |r: SyncRequest| engine.sync(&r)),
        "/v1/evaluate" => answer(stream, &request, |r: EvaluateRequest| engine.evaluate(&r)),
        "/v1/members" => answer(stream, &request, |r: MembersRequest| engine.members(&r)),
        "/v1/profile" => answer(stream, &request, |r: ProfileRequest| engine.profile(&r)),
        "/v1/page" => answer(stream, &request, |r: PageRequest| engine.page(&r)),
        "/v1/land" => answer(stream, &request, |r: LandRequest| land(engine, &r)),
        "/v1/settle" => answer(stream, &request, |r: SettleRequest| settle(engine, &r)),
        "/v1/lineage" => answer(stream, &request, |r: LineageRequest| engine.lineage(&r)),
        "/v1/store" => answer(stream, &request, |r: StoreRequest| store(engine, &r)),
        "/v1/export" => export(stream, &request, engine),
        _ => refuse(
            stream,
            404,
            &refusal("analytics.unknown", "no such operation"),
        ),
    }
}

/// Constant-time comparison of the bearer token.
fn authorized(header: Option<&str>, token: &str) -> bool {
    let Some(given) = header.and_then(|h| h.strip_prefix("Bearer ")) else {
        return false;
    };
    let (given, token) = (given.as_bytes(), token.as_bytes());
    given.len() == token.len()
        && given
            .iter()
            .zip(token)
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

fn refusal(code: &str, detail: &str) -> EngineRefusal {
    EngineRefusal {
        code: code.to_owned(),
        detail: detail.to_owned(),
    }
}

fn status_of(refused: &EngineRefusal) -> u16 {
    crate::olap::refusal_status(&refused.code)
}

/// Every refusal is answered to the cell, which maps it to its own, and
/// logged for the operator: the cell's answer to its caller may be opaque.
fn refuse(stream: &TcpStream, status: u16, refused: &EngineRefusal) -> std::io::Result<()> {
    let _logged = writeln!(
        std::io::stderr(),
        "norn-olapd: refused {status} {}: {}",
        refused.code,
        refused.detail
    );
    let body = serde_json::to_string(refused).unwrap_or_else(|_e| String::from("{}"));
    respond(stream, status, "application/json", body.as_bytes())
}

fn answer<T: DeserializeOwned, R: Serialize>(
    stream: &TcpStream,
    request: &Request,
    operation: impl FnOnce(T) -> Outcome<R>,
) -> std::io::Result<()> {
    let parsed: T = match serde_json::from_slice(&request.body) {
        Ok(parsed) => parsed,
        Err(error) => {
            return refuse(
                stream,
                400,
                &refusal("analytics.invalid", &error.to_string()),
            );
        }
    };
    match operation(parsed) {
        Ok(result) => match serde_json::to_string(&result) {
            Ok(body) => respond(stream, 200, "application/json", body.as_bytes()),
            Err(error) => refuse(
                stream,
                500,
                &refusal("analytics.engine", &error.to_string()),
            ),
        },
        Err(refused) => refuse(stream, status_of(&refused), &refused),
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(0)
        .timeout_global(Some(Duration::from_secs(600)))
        .build()
        .into()
}

/// MANIFEST-02: download the presigned object into `file`, the attempt's
/// own source file (created exclusively by the engine), at most
/// [`MAX_FILE_BYTES`].
fn download(agent: &ureq::Agent, url: &str, mut file: std::fs::File) -> Outcome<()> {
    let unreadable = |detail: String| refusal("bulk.unreadable", &detail);
    let mut response = agent
        .get(url)
        .call()
        .map_err(|e| unreadable(format!("the object could not be read: {e}")))?;
    if response.status() != 200 {
        return Err(unreadable(format!(
            "the object store answered {}",
            response.status()
        )));
    }
    let copied = (|| -> std::io::Result<()> {
        let mut reader = response
            .body_mut()
            .with_config()
            .limit(MAX_FILE_BYTES)
            .reader();
        std::io::copy(&mut reader, &mut file)?;
        file.sync_all()
    })();
    copied.map_err(|e| unreadable(format!("the object could not be read: {e}")))
}

/// A reader that hashes every byte it hands out.
struct Hashing<R> {
    inner: R,
    hasher: Arc<std::sync::Mutex<sha2::Sha256>>,
}

impl<R: std::io::Read> std::io::Read for Hashing<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        use sha2::Digest as _;
        let read = self.inner.read(buf)?;
        self.hasher
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .update(buf.get(..read).unwrap_or_default());
        Ok(read)
    }
}

/// MANIFEST-02: send `path` to the presigned PUT `url`, hashing the bytes as
/// they are sent: the object is accepted only if what was sent is `sha256`,
/// the digest the landing answered and the manifest will name.
fn upload(agent: &ureq::Agent, url: &str, path: &std::path::Path, sha256: &str) -> Outcome<()> {
    use sha2::Digest as _;
    let unwritable = |detail: String| refusal("bulk.unwritable", &detail);
    let file = std::fs::File::open(path)
        .map_err(|e| unwritable(format!("a landed file could not be read: {e}")))?;
    let length = file
        .metadata()
        .map_err(|e| unwritable(format!("a landed file could not be read: {e}")))?
        .len();
    let hasher = Arc::new(std::sync::Mutex::new(sha2::Sha256::new()));
    let body = Hashing {
        inner: file,
        hasher: Arc::clone(&hasher),
    };
    let response = agent
        .put(url)
        .header("content-type", "application/vnd.apache.parquet")
        .header("content-length", &length.to_string())
        .send(ureq::SendBody::from_owned_reader(body))
        .map_err(|e| unwritable(format!("the object could not be written: {e}")))?;
    if !response.status().is_success() {
        return Err(unwritable(format!(
            "the object store answered {}",
            response.status()
        )));
    }
    let sent = std::mem::take(
        &mut *hasher
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    )
    .finalize()
    .iter()
    .fold(String::new(), |mut out, b| {
        use std::fmt::Write as _;
        let _infallible = write!(out, "{b:02x}");
        out
    });
    if sent == sha256 {
        Ok(())
    } else {
        Err(refusal(
            "bulk.digest_mismatch",
            "the bytes sent are not the landed file",
        ))
    }
}

/// MANIFEST-LAND-1: download the presigned source into the attempt's own
/// directory and land it there. A failed landing removes the attempt.
fn land(engine: &Engine, request: &LandRequest) -> Outcome<LandResponse> {
    let tenant = &request.load.tenant;
    let (_path, file) = engine.begin_attempt(tenant, &request.attempt)?;
    let landed =
        download(&agent(), &request.load.file.url, file).and_then(|()| engine.land(request));
    if landed.is_err() {
        engine.end_attempt(tenant, &request.attempt);
    }
    landed
}

/// MANIFEST-LAWS-1: settle an attempt's residue rows; a refused settle
/// removes the attempt.
fn settle(engine: &Engine, request: &SettleRequest) -> Outcome<LandResponse> {
    let settled = engine.settle(request);
    if settled.is_err() {
        engine.end_attempt(&request.land.load.tenant, &request.land.attempt);
    }
    settled
}

/// MANIFEST-LAND-1: send an attempt's landed files to their content-addressed
/// objects, each only if the bytes sent are its answered digest, then keep
/// them for the dataset's cache identity. The attempt is removed either way.
fn store(engine: &Engine, request: &StoreRequest) -> Outcome<LandResponse> {
    let tenant = &request.land.load.tenant;
    let attempt = &request.land.attempt;
    let stored = engine
        .landed_files(tenant, attempt)
        .and_then(|(data, held)| {
            let (Some(data_file), Some(held_file)) = (&request.landed.data, &request.landed.held)
            else {
                return Err(refusal(
                    "analytics.invalid",
                    "the landing has residue to settle",
                ));
            };
            let agent = agent();
            upload(&agent, &request.data.url, &data, &data_file.sha256)?;
            upload(&agent, &request.held.url, &held, &held_file.sha256)?;
            engine.store(request)
        });
    engine.end_attempt(tenant, attempt);
    stored
}

/// Export into a scratch file, stream it as the body, then delete it.
fn export(stream: &TcpStream, request: &Request, engine: &Engine) -> std::io::Result<()> {
    let parsed: ExportRequest = match serde_json::from_slice(&request.body) {
        Ok(parsed) => parsed,
        Err(error) => {
            return refuse(
                stream,
                400,
                &refusal("analytics.invalid", &error.to_string()),
            );
        }
    };
    let kind = match parsed.format {
        norn_cell::analytics::ExportFormat::Parquet => "application/vnd.apache.parquet",
        norn_cell::analytics::ExportFormat::Jsonl => "application/x-ndjson",
    };
    let path = match engine.export(&parsed) {
        Ok(path) => path,
        Err(refused) => return refuse(stream, status_of(&refused), &refused),
    };
    let sent = (|| -> std::io::Result<()> {
        let mut file = std::fs::File::open(&path)?;
        let length = file.metadata()?.len();
        let mut out = stream;
        write!(
            out,
            "HTTP/1.1 200 OK\r\ncontent-type: {kind}\r\ncache-control: no-store\r\ncontent-length: {length}\r\nconnection: close\r\n\r\n"
        )?;
        std::io::copy(&mut file, &mut out)?;
        out.flush()
    })();
    let _removed = std::fs::remove_file(&path);
    sent
}

fn read_request(stream: &TcpStream) -> Result<Request, (u16, EngineRefusal)> {
    let invalid = |detail: String| (400, refusal("analytics.invalid", &detail));
    let mut reader = BufReader::new(
        stream
            .take(u64::try_from(MAX_HEAD_BYTES.saturating_add(MAX_BODY_BYTES)).unwrap_or(u64::MAX)),
    );
    let mut line = String::new();
    let mut head = 0_usize;
    let mut next = |reader: &mut BufReader<_>, line: &mut String| {
        line.clear();
        let read = reader
            .read_line(line)
            .map_err(|e| invalid(format!("unreadable request: {e}")))?;
        head = head.saturating_add(read);
        if read == 0 || head > MAX_HEAD_BYTES {
            return Err(invalid(
                "the request head is missing or too long".to_owned(),
            ));
        }
        Ok(())
    };
    next(&mut reader, &mut line)?;
    let mut parts = line.split_whitespace();
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
        return Err(invalid("the request line is malformed".to_owned()));
    };
    let (method, path) = (
        method.to_owned(),
        target.split('?').next().unwrap_or_default().to_owned(),
    );
    let (mut length, mut authorization) = (0_usize, None);
    loop {
        next(&mut reader, &mut line)?;
        let header = line.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            if name.eq_ignore_ascii_case("content-length") {
                length = value
                    .trim()
                    .parse()
                    .map_err(|e| invalid(format!("content-length: {e}")))?;
            } else if name.eq_ignore_ascii_case("authorization") {
                authorization = Some(value.trim().to_owned());
            }
        }
    }
    if length > MAX_BODY_BYTES {
        return Err((
            413,
            refusal("analytics.too_large", "the request body exceeds the bound"),
        ));
    }
    let mut body = vec![0_u8; length];
    reader
        .read_exact(&mut body)
        .map_err(|e| invalid(format!("the body is shorter than content-length: {e}")))?;
    Ok(Request {
        method,
        path,
        authorization,
        body,
    })
}
