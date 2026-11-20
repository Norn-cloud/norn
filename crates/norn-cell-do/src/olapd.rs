//! `norn-olapd` (D102): the tenant analytics engine as a native sidecar next
//! to the cell host. It serves the typed operations of
//! [`norn_cell::analytics`] over a bounded HTTP/1.1 transport to one caller,
//! the cell. OLAP-CRED-1 (#1493): every call carries the calling cell's own
//! credential ([`norn_api::olap_credential`]), and the daemon serves it only
//! for the one tenant that credential names; there is no shared token. The
//! daemon holds public keys only, read from its keyring file on every call,
//! so a key is added or rotated out without a restart. Every refused
//! credential (missing, malformed, unknown or rotated-out key, bad
//! signature, expired, or another tenant's) gets the same answer, before any
//! tenant database is opened, so a caller learns nothing about whether
//! another tenant exists. The cell still authorizes every request before it
//! sends it and checks every answer. The daemon holds no storage
//! credentials; a load reads a presigned URL the cell minted for that one
//! object.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use norn_api::clock::Clock;
use norn_api::olap_credential::{Denied, Keyring};
use norn_cell::analytics::engine::{Engine, Outcome, Settings};
use norn_cell::analytics::{
    EngineRefusal, EvaluateRequest, ExportRequest, FrontierRequest, LoadRequest, MembersRequest,
    PageRequest, ProfileRequest, SyncRequest, TenantId,
};
use norn_kernel::LogicalTime;
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
/// Longest accepted keyring file, in bytes.
const MAX_KEYRING_BYTES: u64 = 64 * 1024;
/// The variable of the retired shared token; its presence refuses startup.
const RETIRED_TOKEN: &str = "NORN_SECRET_DUCKDB_TOKEN";

/// The daemon's configuration, read from its environment.
pub struct Config {
    /// Listen address, `NORN_OLAP_ADDR` (default `0.0.0.0:8790`).
    pub addr: String,
    /// OLAP-CRED-1: the keyring file, `NORN_OLAP_KEYS_FILE`: one
    /// `<kid>:<base64url Ed25519 public key>` per line. Read on every call.
    pub keys_file: PathBuf,
    /// Engine settings: `NORN_OLAP_DATA`, `NORN_OLAP_MEMORY_LIMIT`, `NORN_OLAP_THREADS`.
    pub settings: Settings,
}

impl Config {
    /// Read the configuration from the process environment.
    ///
    /// # Errors
    /// As [`Config::from_vars`].
    pub fn from_env() -> Result<Self, String> {
        Self::from_vars(|name| std::env::var(name).ok())
    }

    /// Read the configuration from `var`. The keyring must be readable now;
    /// the retired shared token must be absent, so no deployment believes
    /// it is still protected by it.
    ///
    /// # Errors
    /// The retired shared token is set, the keyring file is missing or
    /// unreadable, or the thread count is unreadable.
    pub fn from_vars(var: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let var = |name: &str| var(name).filter(|v| !v.is_empty());
        if var(RETIRED_TOKEN).is_some() {
            return Err(format!(
                "{RETIRED_TOKEN} is retired (#1493): cells present their own credentials; \
                 remove it and set NORN_OLAP_KEYS_FILE"
            ));
        }
        let keys_file = PathBuf::from(
            var("NORN_OLAP_KEYS_FILE")
                .ok_or("NORN_OLAP_KEYS_FILE must name the keyring of the cells' public keys")?,
        );
        read_keyring(&keys_file)?;
        let threads = var("NORN_OLAP_THREADS")
            .map(|t| {
                t.parse::<u32>()
                    .map_err(|e| format!("NORN_OLAP_THREADS: {e}"))
            })
            .transpose()?
            .unwrap_or(1);
        Ok(Self {
            addr: var("NORN_OLAP_ADDR").unwrap_or_else(|| "0.0.0.0:8790".to_owned()),
            keys_file,
            settings: Settings {
                root: PathBuf::from(var("NORN_OLAP_DATA").unwrap_or_else(|| "/data".to_owned())),
                memory_limit: var("NORN_OLAP_MEMORY_LIMIT").unwrap_or_else(|| "512MB".to_owned()),
                threads,
            },
        })
    }
}

/// Read and parse the keyring file, bounded.
fn read_keyring(path: &std::path::Path) -> Result<Keyring, String> {
    let mut text = String::new();
    std::fs::File::open(path)
        .and_then(|file| file.take(MAX_KEYRING_BYTES + 1).read_to_string(&mut text))
        .map_err(|e| format!("the keyring {} is unreadable: {e}", path.display()))?;
    if u64::try_from(text.len()).unwrap_or(u64::MAX) > MAX_KEYRING_BYTES {
        return Err(format!("the keyring {} is too long", path.display()));
    }
    Keyring::parse(&text).map_err(|e| format!("the keyring {}: {e}", path.display()))
}

/// A fresh signing key `kid` for the cell host, and the keyring line that
/// verifies its credentials: (`<kid>:<seed>`, `<kid>:<public key>`).
///
/// # Errors
/// A malformed key id, or no randomness.
pub fn new_key(kid: &str) -> Result<(String, String), String> {
    use base64::Engine as _;
    let mut seed = [0_u8; 32];
    getrandom::fill(&mut seed).map_err(|e| format!("no randomness: {e}"))?;
    let secret = format!(
        "{kid}:{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(seed)
    );
    zeroize::Zeroize::zeroize(&mut seed);
    let issuer = norn_api::olap_credential::Issuer::parse(&secret)?;
    Ok((secret, issuer.public_spec()))
}

/// The daemon's wall clock: credential expiry is checked against it.
pub struct SystemClock;

impl Clock for SystemClock {
    #[allow(
        clippy::disallowed_methods,
        reason = "the daemon is the trusted boundary that checks credential expiry"
    )]
    fn now(&self) -> LogicalTime {
        LogicalTime::from_millis(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |elapsed| {
                    u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
                }),
        )
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
    let listener = TcpListener::bind(&config.addr)?;
    serve(listener, config, Arc::new(SystemClock))
}

/// Serve `listener` until it fails, checking credential expiry against `clock`.
///
/// # Errors
/// The engine directory cannot be opened.
pub fn serve(listener: TcpListener, config: Config, clock: Arc<dyn Clock>) -> std::io::Result<()> {
    let engine = Arc::new(Engine::new(config.settings)?);
    let gate = Arc::new(Gate {
        keys_file: config.keys_file,
        clock,
    });
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
        let (engine, gate, active) = (Arc::clone(&engine), Arc::clone(&gate), Arc::clone(&active));
        std::thread::spawn(move || {
            // A broken connection affects only itself.
            let _ignored = connection(&stream, &engine, &gate);
            active.fetch_sub(1, Ordering::SeqCst);
        });
    }
    Ok(())
}

/// OLAP-CRED-1: who may call. The keyring is read on every call, so adding
/// or removing a key takes effect without a restart; an unreadable keyring
/// refuses every call.
struct Gate {
    keys_file: PathBuf,
    clock: Arc<dyn Clock>,
}

impl Gate {
    /// The tenant the request's credential was minted for.
    fn tenant(&self, authorization: Option<&str>) -> Result<TenantId, Denied> {
        let credential = authorization
            .and_then(|h| h.strip_prefix("Bearer "))
            .ok_or(Denied::Malformed)?;
        let keyring = read_keyring(&self.keys_file).map_err(|error| {
            let _logged = writeln!(std::io::stderr(), "norn-olapd: {error}");
            Denied::UnknownKey
        })?;
        keyring.verify(credential, self.clock.now().as_millis())
    }
}

/// A request that reads or writes one tenant's database.
trait Scoped {
    fn tenant(&self) -> &TenantId;
}

macro_rules! scoped {
    ($($request:ty),*) => {$(
        impl Scoped for $request {
            fn tenant(&self) -> &TenantId {
                &self.tenant
            }
        }
    )*};
}

scoped!(
    FrontierRequest,
    SyncRequest,
    EvaluateRequest,
    MembersRequest,
    ProfileRequest,
    PageRequest,
    LoadRequest,
    ExportRequest
);

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

fn connection(stream: &TcpStream, engine: &Engine, gate: &Gate) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    let request = match read_request(stream) {
        Ok(request) => request,
        Err((status, refused)) => return refuse(stream, status, &refused),
    };
    if request.method == "GET" && request.path == "/healthz" {
        return respond(stream, 200, "application/json", b"{\"ok\":true}");
    }
    let tenant = match gate.tenant(request.authorization.as_deref()) {
        Ok(tenant) => tenant,
        Err(denied) => return unauthorized(stream, denied),
    };
    if request.method != "POST" {
        return refuse(
            stream,
            404,
            &refusal("analytics.unknown", "no such operation"),
        );
    }
    let call = Call {
        stream,
        request: &request,
        tenant: &tenant,
    };
    match request.path.as_str() {
        "/v1/frontier" => call.answer(|r: FrontierRequest| engine.frontier(&r)),
        "/v1/sync" => call.answer(|r: SyncRequest| engine.sync(&r)),
        "/v1/evaluate" => call.answer(|r: EvaluateRequest| engine.evaluate(&r)),
        "/v1/members" => call.answer(|r: MembersRequest| engine.members(&r)),
        "/v1/profile" => call.answer(|r: ProfileRequest| engine.profile(&r)),
        "/v1/page" => call.answer(|r: PageRequest| engine.page(&r)),
        "/v1/load" => call.answer(|r: LoadRequest| load(engine, &r)),
        "/v1/export" => export(&call, engine),
        _ => refuse(
            stream,
            404,
            &refusal("analytics.unknown", "no such operation"),
        ),
    }
}

/// The one answer to every refused credential, whatever the reason and
/// whichever tenant was asked for; the reason goes to the operator log.
fn unauthorized(stream: &TcpStream, denied: Denied) -> std::io::Result<()> {
    let _logged = writeln!(
        std::io::stderr(),
        "norn-olapd: refused credential: {}",
        denied.label()
    );
    let body = serde_json::to_string(&refusal(
        "analytics.unauthorized",
        "the caller is not the cell",
    ))
    .unwrap_or_else(|_e| String::from("{}"));
    respond(stream, 401, "application/json", body.as_bytes())
}

/// One authenticated call: the credential's tenant is the only one it may touch.
struct Call<'a> {
    stream: &'a TcpStream,
    request: &'a Request,
    tenant: &'a TenantId,
}

impl Call<'_> {
    /// The typed request, when it parses and names the credential's tenant.
    /// `Err` holds the refusal already sent.
    fn parse<T: DeserializeOwned + Scoped>(&self) -> Result<T, std::io::Result<()>> {
        let parsed: T = serde_json::from_slice(&self.request.body).map_err(|error| {
            refuse(
                self.stream,
                400,
                &refusal("analytics.invalid", &error.to_string()),
            )
        })?;
        if parsed.tenant() != self.tenant {
            return Err(unauthorized(self.stream, Denied::OtherTenant));
        }
        Ok(parsed)
    }

    fn answer<T: DeserializeOwned + Scoped, R: Serialize>(
        &self,
        operation: impl FnOnce(T) -> Outcome<R>,
    ) -> std::io::Result<()> {
        let parsed = match self.parse::<T>() {
            Ok(parsed) => parsed,
            Err(sent) => return sent,
        };
        match operation(parsed) {
            Ok(result) => match serde_json::to_string(&result) {
                Ok(body) => respond(self.stream, 200, "application/json", body.as_bytes()),
                Err(error) => refuse(
                    self.stream,
                    500,
                    &refusal("analytics.engine", &error.to_string()),
                ),
            },
            Err(refused) => refuse(self.stream, status_of(&refused), &refused),
        }
    }
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

/// Download the presigned object into the engine's scratch directory, then load it.
fn load(engine: &Engine, request: &LoadRequest) -> Outcome<norn_cell::analytics::LoadResponse> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(0)
        .timeout_global(Some(Duration::from_secs(600)))
        .build()
        .into();
    let unreadable = |detail: String| refusal("bulk.unreadable", &detail);
    let mut response = agent
        .get(&request.file.url)
        .call()
        .map_err(|e| unreadable(format!("the object could not be read: {e}")))?;
    if response.status() != 200 {
        return Err(unreadable(format!(
            "the object store answered {}",
            response.status()
        )));
    }
    let path = engine.scratch().join(format!(
        "load-{}",
        request
            .load
            .replace(|c: char| !c.is_ascii_alphanumeric(), "_")
    ));
    let copied = (|| -> std::io::Result<()> {
        let mut file = std::fs::File::create(&path)?;
        let mut reader = response
            .body_mut()
            .with_config()
            .limit(MAX_FILE_BYTES)
            .reader();
        std::io::copy(&mut reader, &mut file)?;
        file.sync_all()
    })();
    let loaded = copied
        .map_err(|e| unreadable(format!("the object could not be read: {e}")))
        .and_then(|()| engine.load(request, &path));
    let _removed = std::fs::remove_file(&path);
    loaded
}

/// Export into a scratch file, stream it as the body, then delete it.
fn export(call: &Call<'_>, engine: &Engine) -> std::io::Result<()> {
    let stream = call.stream;
    let parsed: ExportRequest = match call.parse() {
        Ok(parsed) => parsed,
        Err(sent) => return sent,
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
