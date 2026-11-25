//! `norn-olapd` (D102): the tenant analytics engine as a native sidecar next
//! to the cell host. It serves the typed operations of
//! [`norn_cell::analytics`] over a bounded HTTP/1.1 transport to one caller,
//! the cell. OLAP-CRED-1 (#1493): every call carries the calling cell's own
//! credential ([`norn_api::olap_credential`]), bound to that call's operation
//! and body, and the daemon serves it only for the one tenant that
//! credential names; there is no shared token. The daemon holds public keys
//! only, read from its keyring file on every call, so a key is added or
//! rotated out without a restart.
//!
//! OLAP-04: authorization comes first and is complete before the daemon
//! routes or parses anything. #1747: the credential carries the digest of
//! the body it was minted for, signed, so it is verified against the
//! operation the path names from the request's head alone, before a single
//! body byte is read or a body buffer allocated; the body then read must
//! have exactly that digest, the body's `tenant` must be the credential's,
//! and only then is the path routed and the body parsed into its typed
//! request. Every refusal before that point (no, malformed, unknown or
//! rotated-out key, bad signature, expired, minted for another operation or
//! body, another tenant, a body naming no tenant, any method but `POST`, an
//! unknown path) gets the same `401`, and no tenant database is opened, so
//! a caller learns nothing about whether another tenant exists. A `400`
//! about the request's own parse or a `404` is reachable only with a
//! credential the cell minted for that exact request; the transport's own
//! refusals come earlier and say nothing about any tenant: a malformed head
//! is a `400` before any credential is read, and a body over the bound
//! (`413`) or shorter than its declared length (`400`) is answered only to
//! a credential that verified. Residual timing: every request pays the
//! keyring read; only a well-formed, live credential naming a key id the
//! keyring holds pays the signature check, and only one that verifies gets
//! its body read and digested (and, when that matches, the scope check), so
//! response time can tell such a credential from garbage. Key ids are not
//! secret, and nothing about any tenant (whether it exists, what it holds)
//! changes the time of a refusal.
//!
//! #1710: every connection's reads and writes run against absolute
//! deadlines (`transport`), so how long one peer holds a connection is
//! bounded. The head must arrive within `HEAD_DEADLINE` and within
//! `MAX_HEAD_BYTES`, read no further than that budget; a body earns its time
//! by the bytes that arrived, not the length it declares; and an answer has
//! a deadline sized to its length. #1747: that reading happens in a bounded
//! pre-admission stage apart from the engine's workers (`admission`): at
//! most `MAX_PENDING` connections, `MAX_PENDING_PER_PEER` from one peer
//! address, each on its own thread. Only a request whose credential and
//! body both verified and whose scope is the credential's tenant is handed
//! to the [`ENGINE_WORKERS`] engine workers, through a queue of
//! [`ENGINE_QUEUE`]; a full stage, a full queue or too many held body bytes
//! is answered `503` at once. So idle, trickling or forged connections,
//! however many, never hold an engine worker.
//!
//! The cell still authorizes every request before it sends it and checks
//! every answer. The daemon holds no storage credentials; a load reads a
//! presigned URL the cell minted for that one object. OLAP-02: each tenant
//! has its own scratch directory (exports and `DuckDB` spill), and each
//! landing attempt downloads into a directory of its own that the engine
//! creates exclusively (MANIFEST-02); nothing in a request names a path.
//! The engine mints each attempt's token itself and holds the attempt for
//! one operation at a time; that operation is its one cleanup owner, so a
//! store, replayed or not, never removes an attempt still downloading or
//! one a later landing made.

use std::ffi::OsString;
use std::io::{BufReader, Read as _, Write as _};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use norn_api::clock::Clock;
use norn_api::olap_credential::{Denied, Keyring, Verified};
use norn_cell::analytics::engine::{Engine, Settings};
use norn_cell::analytics::{
    AnalyticalEngine, AnalyticalEngineId, EngineOutcome, EngineRefusal, EvaluateRequest,
    ExportRequest, FrontierRequest, LandRequest, LandResponse, LineageRequest, MembersRequest,
    PageRequest, ProfileRequest, SettleRequest, StoreBody, SyncRequest, TenantId,
    dispatch_evaluate, dispatch_members,
};
use norn_kernel::LogicalTime;
use serde::Serialize;
use serde::de::DeserializeOwned;

mod admission;
mod transport;

use admission::{Admission, Held, Pending};
pub use admission::{MAX_HELD_BODY_BYTES, MAX_PENDING, MAX_PENDING_PER_PEER};
use transport::{
    Deadlined, HEAD_DEADLINE, Head, MAX_BODY_BYTES, Request, drain, read_body, read_head,
    write_budget,
};

/// Largest dataset file a load downloads, in bytes.
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024 * 1024;
/// #1747: engine workers: authenticated calls run on at most this many at once.
pub const ENGINE_WORKERS: usize = 8;
/// #1747: authenticated calls waiting for an engine worker; more are answered 503.
pub const ENGINE_QUEUE: usize = 8;
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
    /// D136: the one engine this daemon runs, `NORN_OLAP_ENGINE` (`duckdb`,
    /// the default, or `datafusion` in a build with it).
    pub engine: AnalyticalEngineId,
}

impl Config {
    /// Read the configuration from the process environment.
    ///
    /// # Errors
    /// As [`Config::from_vars`].
    pub fn from_env() -> Result<Self, String> {
        Self::from_vars(env_var)
    }

    /// Read the configuration from `var`. The keyring must be readable now;
    /// the retired shared token must be absent, so no deployment believes
    /// it is still protected by it. OLAP-05: the retired token's presence
    /// alone refuses, whatever its value, empty or not UTF-8 included.
    ///
    /// # Errors
    /// The retired shared token is set, the keyring file is missing or
    /// unreadable, a setting is not UTF-8, or the thread count is unreadable.
    pub fn from_vars(var: impl Fn(&str) -> Option<OsString>) -> Result<Self, String> {
        if var(RETIRED_TOKEN).is_some() {
            return Err(format!(
                "{RETIRED_TOKEN} is retired (#1493): cells present their own credentials; \
                 unset it (even empty) and set NORN_OLAP_KEYS_FILE"
            ));
        }
        let set = |name: &str| var(name).filter(|v| !v.is_empty());
        let text = |name: &str| {
            set(name)
                .map(|v| v.into_string().map_err(|_v| format!("{name} is not UTF-8")))
                .transpose()
        };
        let keys_file = PathBuf::from(
            set("NORN_OLAP_KEYS_FILE")
                .ok_or("NORN_OLAP_KEYS_FILE must name the keyring of the cells' public keys")?,
        );
        read_keyring(&keys_file)?;
        let threads = text("NORN_OLAP_THREADS")?
            .map(|t| {
                t.parse::<u32>()
                    .map_err(|e| format!("NORN_OLAP_THREADS: {e}"))
            })
            .transpose()?
            .unwrap_or(1);
        let engine = AnalyticalEngineId::from_name(
            text("NORN_OLAP_ENGINE")?.as_deref().unwrap_or("duckdb"),
        )?;
        if engine == AnalyticalEngineId::DataFusion && !cfg!(feature = "datafusion") {
            return Err("NORN_OLAP_ENGINE=datafusion needs a build with `datafusion`".to_owned());
        }
        Ok(Self {
            engine,
            addr: text("NORN_OLAP_ADDR")?.unwrap_or_else(|| "0.0.0.0:8790".to_owned()),
            keys_file,
            settings: Settings {
                root: set("NORN_OLAP_DATA").map_or_else(|| PathBuf::from("/data"), PathBuf::from),
                memory_limit: text("NORN_OLAP_MEMORY_LIMIT")?.unwrap_or_else(|| "512MB".to_owned()),
                threads,
            },
        })
    }
}

/// The process environment's `name`, as it is: empty and non-UTF-8 values
/// included (OLAP-05).
fn env_var(name: &str) -> Option<OsString> {
    std::env::var_os(name)
}

/// Read and parse the keyring file, bounded.
fn read_keyring(path: &Path) -> Result<Keyring, String> {
    let mut text = String::new();
    std::fs::File::open(path)
        .and_then(|file| {
            file.take(MAX_KEYRING_BYTES.saturating_add(1))
                .read_to_string(&mut text)
        })
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
    serve(&listener, config, Arc::new(SystemClock))
}

/// Serve `listener` until it fails, checking credential expiry against `clock`.
///
/// # Errors
/// The engine directory cannot be opened.
pub fn serve(listener: &TcpListener, config: Config, clock: Arc<dyn Clock>) -> std::io::Result<()> {
    serve_with(listener, config, clock, Arc::new(NoSeam))
}

/// D136: the one engine `selected` at startup, over `settings`.
fn open_engine(
    selected: AnalyticalEngineId,
    settings: Settings,
) -> std::io::Result<Arc<dyn AnalyticalEngine>> {
    match selected {
        AnalyticalEngineId::DuckDb => Ok(Arc::new(Engine::new(settings)?)),
        #[cfg(feature = "datafusion")]
        AnalyticalEngineId::DataFusion => {
            Ok(Arc::new(norn_cell::analytics::datafusion::Engine::new(
                norn_cell::analytics::datafusion::Settings {
                    root: settings.root,
                    memory_limit: settings.memory_limit,
                    threads: settings.threads,
                },
            )?))
        }
        #[cfg(not(feature = "datafusion"))]
        AnalyticalEngineId::DataFusion => {
            Err(std::io::Error::other("this build has no DataFusion engine"))
        }
    }
}

/// Points in a call the daemon announces to a [`Seam`], so a test can run
/// other calls there and interleave them deterministically.
pub trait Seam: Send + Sync {
    /// A `/v1/store` of `tenant` returned from the engine, `stored` or
    /// refused, and its answer is not yet sent.
    fn stored(&self, _tenant: &TenantId, _stored: bool) {}
}

/// The seam of a production daemon: announces nothing.
pub struct NoSeam;

impl Seam for NoSeam {}

/// [`serve`], announcing each [`Seam`] point to `seam`.
///
/// # Errors
/// The engine directory cannot be opened.
pub fn serve_with(
    listener: &TcpListener,
    config: Config,
    clock: Arc<dyn Clock>,
    seam: Arc<dyn Seam>,
) -> std::io::Result<()> {
    let engine = open_engine(config.engine, config.settings)?;
    let gate = Arc::new(Gate {
        keys_file: config.keys_file,
        clock,
        seam,
    });
    let (jobs, queue) = std::sync::mpsc::sync_channel::<Job>(ENGINE_QUEUE);
    let queue = Arc::new(Mutex::new(queue));
    for index in 0..ENGINE_WORKERS {
        let (engine, gate, queue) = (Arc::clone(&engine), Arc::clone(&gate), Arc::clone(&queue));
        std::thread::Builder::new()
            .name(format!("olapd-engine-{index}"))
            .spawn(move || work(&queue, engine.as_ref(), &gate))?;
    }
    let admission = Arc::new(Admission::default());
    for stream in listener.incoming().flatten() {
        // #1747: a slot in the bounded pre-admission stage, or 503 at once.
        let Some(pending) = stream
            .peer_addr()
            .ok()
            .and_then(|peer| admission.enter(peer.ip()))
        else {
            busy(&stream);
            continue;
        };
        let (gate, admission, jobs) = (Arc::clone(&gate), Arc::clone(&admission), jobs.clone());
        let spawned = std::thread::Builder::new().spawn(move || {
            // A broken connection affects only itself.
            let _ignored = preadmit(stream, &gate, &admission, &jobs, pending);
        });
        if spawned.is_err() {
            let _logged = writeln!(std::io::stderr(), "norn-olapd: no thread for a connection");
        }
    }
    Ok(())
}

/// #1747: an authenticated, digest-verified, scoped call waiting for an
/// engine worker, with the body bytes it holds.
struct Job {
    stream: TcpStream,
    request: Request,
    tenant: TenantId,
    _held: Held,
}

/// One engine worker: run each queued call until the daemon stops.
fn work(queue: &Mutex<Receiver<Job>>, engine: &dyn AnalyticalEngine, gate: &Gate) {
    loop {
        let next = queue.lock().unwrap_or_else(PoisonError::into_inner).recv();
        let Ok(job) = next else { return };
        // A broken connection affects only itself.
        let _ignored = route(
            &Call {
                stream: &job.stream,
                request: &job.request,
                tenant: &job.tenant,
            },
            engine,
            gate,
        );
    }
}

/// #1747: the one answer to a connection the daemon has no room for, sent
/// without waiting: a peer that does not read it holds nothing.
fn busy(stream: &TcpStream) {
    let answer = b"HTTP/1.1 503 Service Unavailable\r\ncontent-type: application/json\r\ncache-control: no-store\r\ncontent-length: 25\r\nconnection: close\r\n\r\n{\"code\":\"analytics.busy\"}";
    if stream.set_nonblocking(true).is_ok() {
        let mut stream = stream;
        let _sent = stream.write(answer);
    }
}

/// OLAP-CRED-1: who may call. The keyring is read on every call, so adding
/// or removing a key takes effect without a restart; an unreadable keyring
/// refuses every call.
struct Gate {
    keys_file: PathBuf,
    clock: Arc<dyn Clock>,
    seam: Arc<dyn Seam>,
}

/// The one field the scope check reads before the request is routed or
/// parsed into its type; every other field is left for the typed parse. A
/// landing names its tenant in its load (`load.tenant`), and a settle or a
/// store in the landing it settles or stores (`land.load.tenant`).
#[derive(serde::Deserialize)]
struct Named {
    tenant: Option<TenantId>,
    load: Option<NamedLoad>,
    land: Option<NamedLand>,
}

#[derive(serde::Deserialize)]
struct NamedLoad {
    tenant: TenantId,
}

#[derive(serde::Deserialize)]
struct NamedLand {
    load: NamedLoad,
}

impl Named {
    /// The tenant the body of operation `op` names, where that operation's
    /// request names it.
    fn tenant(self, op: &str) -> Option<TenantId> {
        match op {
            "land" => self.load.map(|load| load.tenant),
            "settle" | "store" => self.land.map(|land| land.load.tenant),
            _ => self.tenant,
        }
    }
}

impl Gate {
    /// #1747: the credential verified from the request's head alone, for
    /// the operation the path names, before any body is read or allocated:
    /// its signature covers the body digest it declares.
    fn verify_head(&self, head: &Head) -> Result<Verified, Denied> {
        let credential = head
            .authorization
            .as_deref()
            .and_then(|h| h.strip_prefix("Bearer "))
            .ok_or(Denied::Malformed)?;
        let keyring = read_keyring(&self.keys_file).map_err(|error| {
            let _logged = writeln!(std::io::stderr(), "norn-olapd: {error}");
            Denied::UnknownKey
        })?;
        let op = head.path.strip_prefix("/v1/").unwrap_or_default();
        keyring.verify_head(credential, op, self.clock.now().as_millis())
    }

    /// OLAP-04: the tenant `request` may touch, decided before it is routed
    /// or parsed: its body must be the exact body `verified` was minted for
    /// (OLAP-03), and must name the credential's tenant.
    fn admit(verified: Verified, request: &Request) -> Result<TenantId, Denied> {
        let op = request.path.strip_prefix("/v1/").unwrap_or_default();
        let tenant = verified.body(&request.body)?;
        let named = serde_json::from_slice::<Named>(&request.body)
            .ok()
            .and_then(|named| named.tenant(op))
            .ok_or(Denied::Unscoped)?;
        if named != tenant {
            return Err(Denied::OtherTenant);
        }
        Ok(tenant)
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
    LineageRequest,
    ExportRequest
);

impl Scoped for LandRequest {
    fn tenant(&self) -> &TenantId {
        &self.load.tenant
    }
}

impl Scoped for StoreBody {
    fn tenant(&self) -> &TenantId {
        &self.request().land.load.tenant
    }
}

impl Scoped for SettleRequest {
    fn tenant(&self) -> &TenantId {
        &self.land.load.tenant
    }
}

/// DEV-ONE-WRITE-1 (#1533): head and body go out in one write with
/// `TCP_NODELAY`, so an answer never waits for the client's delayed ACK.
fn respond(stream: &TcpStream, status: u16, kind: &str, body: &[u8]) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 {status} {}\r\ncontent-type: {kind}\r\ncache-control: no-store\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
        reason(status),
        body.len()
    );
    let mut bytes = Vec::with_capacity(head.len().saturating_add(body.len()));
    bytes.extend_from_slice(head.as_bytes());
    bytes.extend_from_slice(body);
    stream.set_nodelay(true)?;
    let mut out = Deadlined::new(stream, write_budget(bytes.len()))?;
    out.write_all(&bytes)?;
    out.flush()
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

/// #1747: one connection's pre-admission, on its own thread and off the
/// engine's workers: its head, its credential verified from the head, its
/// body read within the transport's bounds and checked against the digest
/// the credential signed, and its scope. Only then is it queued for an
/// engine worker, and its pre-admission slot given back.
fn preadmit(
    stream: TcpStream,
    gate: &Gate,
    admission: &Arc<Admission>,
    jobs: &SyncSender<Job>,
    pending: Pending,
) -> std::io::Result<()> {
    let (request, tenant, room) = {
        let mut reader = BufReader::new(Deadlined::new(&stream, HEAD_DEADLINE)?);
        let head = match read_head(&mut reader) {
            Ok(head) => head,
            Err((status, refused)) => {
                let sent = refuse(&stream, status, &refused);
                drain(&mut reader);
                return sent;
            }
        };
        if head.method == "GET" && head.path == "/healthz" {
            return respond(&stream, 200, "application/json", b"{\"ok\":true}");
        }
        // Refuse a credential that does not verify, a body over the bound,
        // and a body the daemon has no room for, before any body is read.
        let verified = match gate.verify_head(&head) {
            Ok(verified) => verified,
            Err(denied) => {
                let sent = unauthorized(&stream, denied);
                drain(&mut reader);
                return sent;
            }
        };
        let room = if head.length > MAX_BODY_BYTES {
            None
        } else {
            admission.hold(head.length)
        };
        let Some(room) = room else {
            let sent = if head.length > MAX_BODY_BYTES {
                refuse(
                    &stream,
                    413,
                    &refusal("analytics.too_large", "the request body exceeds the bound"),
                )
            } else {
                busy(&stream);
                Ok(())
            };
            drain(&mut reader);
            return sent;
        };
        let request = match read_body(&mut reader, head) {
            Ok(request) => request,
            Err((status, refused)) => return refuse(&stream, status, &refused),
        };
        let tenant = match Gate::admit(verified, &request) {
            Ok(tenant) => tenant,
            Err(denied) => return unauthorized(&stream, denied),
        };
        if request.method != "POST" {
            return refuse(
                &stream,
                404,
                &refusal("analytics.unknown", "no such operation"),
            );
        }
        (request, tenant, room)
    };
    drop(pending);
    match jobs.try_send(Job {
        stream,
        request,
        tenant,
        _held: room,
    }) {
        Ok(()) => Ok(()),
        Err(TrySendError::Full(job) | TrySendError::Disconnected(job)) => {
            busy(&job.stream);
            Ok(())
        }
    }
}

/// An admitted call, routed by its path.
fn route(call: &Call<'_>, engine: &dyn AnalyticalEngine, gate: &Gate) -> std::io::Result<()> {
    match call.request.path.as_str() {
        "/v1/frontier" => call.answer(|r: FrontierRequest| engine.mirror_frontier(&r)),
        "/v1/sync" => call.answer(|r: SyncRequest| engine.mirror_sync(&r)),
        "/v1/evaluate" => call.answer(|r: EvaluateRequest| dispatch_evaluate(engine, &r)),
        "/v1/members" => call.answer(|r: MembersRequest| dispatch_members(engine, &r)),
        "/v1/profile" => call.answer(|r: ProfileRequest| engine.profile_objects(&r)),
        "/v1/page" => call.answer(|r: PageRequest| engine.query_page(&r)),
        "/v1/lineage" => call.answer(|r: LineageRequest| engine.query_lineage(&r)),
        "/v1/land" => call.answer(|r: LandRequest| land(engine, &r)),
        "/v1/settle" => call.answer(|r: SettleRequest| settle(engine, &r)),
        "/v1/store" => call.answer(|r: StoreBody| {
            let stored = store(engine, &r);
            gate.seam.stored(call.tenant, stored.is_ok());
            stored
        }),
        "/v1/export" => export(call, engine),
        _ => refuse(
            call.stream,
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
        operation: impl FnOnce(T) -> EngineOutcome<R>,
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
fn download(agent: &ureq::Agent, url: &str, mut file: std::fs::File) -> EngineOutcome<()> {
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
fn upload(
    agent: &ureq::Agent,
    url: &str,
    path: &std::path::Path,
    sha256: &str,
) -> EngineOutcome<()> {
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

/// MANIFEST-LAND-1: land the presigned source in a fresh attempt the engine
/// mints, downloading it into the attempt's own source file. The engine
/// holds the attempt throughout; a failed landing removes it.
fn land(engine: &dyn AnalyticalEngine, request: &LandRequest) -> EngineOutcome<LandResponse> {
    engine.land_dataset(request, &mut |file| {
        download(&agent(), &request.load.file.url, file)
    })
}

/// MANIFEST-LAWS-1: settle an attempt's residue rows. The engine's settle
/// takes the attempt by its token and owns its cleanup: a refused settle
/// removes the attempt it took, and nothing else.
fn settle(engine: &dyn AnalyticalEngine, request: &SettleRequest) -> EngineOutcome<LandResponse> {
    engine.settle_dataset(request)
}

/// MANIFEST-LAND-1: send an attempt's landed files to their content-addressed
/// objects, each only if the bytes sent are its answered digest, then keep
/// them for the dataset's cache identity. The engine's store owns the
/// attempt's cleanup; nothing here removes anything.
fn store(
    engine: &dyn AnalyticalEngine,
    body: &StoreBody,
) -> EngineOutcome<Box<serde_json::value::RawValue>> {
    let request = body.request();
    // The engine sends only files whose answer names both digests.
    let sha256 = |file: Option<&norn_cell::analytics::LandedFile>| {
        file.map_or_default(|file| file.sha256.clone())
    };
    let (data_sha256, held_sha256) = (
        sha256(request.landed.data.as_ref()),
        sha256(request.landed.held.as_ref()),
    );
    engine.store_dataset(body, &mut |data, held| {
        let agent = agent();
        upload(&agent, &request.data.url, data, &data_sha256)?;
        upload(&agent, &request.held.url, held, &held_sha256)
    })
}

/// Export into a scratch file, stream it as the body, then delete it.
fn export(call: &Call<'_>, engine: &dyn AnalyticalEngine) -> std::io::Result<()> {
    let stream = call.stream;
    let parsed: ExportRequest = match serde_json::from_slice(&call.request.body) {
        Ok(parsed) => parsed,
        Err(error) => {
            return refuse(
                stream,
                400,
                &refusal("analytics.invalid", &error.to_string()),
            );
        }
    };
    if parsed.tenant() != call.tenant {
        return unauthorized(stream, Denied::OtherTenant);
    }
    let kind = match parsed.format {
        norn_cell::analytics::ExportFormat::Parquet => "application/vnd.apache.parquet",
        norn_cell::analytics::ExportFormat::Jsonl => "application/x-ndjson",
    };
    let path = match engine.export_dataset(&parsed) {
        Ok(path) => path,
        Err(refused) => return refuse(stream, status_of(&refused), &refused),
    };
    let sent = (|| -> std::io::Result<()> {
        let mut file = std::fs::File::open(&path)?;
        let length = file.metadata()?.len();
        // #1710: the whole answer is sent within its own deadline.
        let mut out = Deadlined::new(stream, write_budget(length))?;
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
