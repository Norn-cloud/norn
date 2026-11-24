//! The cell actor: one tenant cell per Rivet actor (D131). This is the native
//! port of the Durable Object class in `apps/norn-cell-do/worker.mjs` (`Cell`)
//! and its wake (`wake.mjs`); the decisions are the cell's, in
//! [`crate::core`], run on the actor's cell thread.
//!
//! - Nothing is written in `create` or `on_start`: `RivetKit` holds a new
//!   actor's startup writes in memory until startup completes (#1130,
//!   section 1), so the cell opens on the first request and provisioning is
//!   acknowledged only from a request.
//! - The cell's identity is the rows in its own database (`Tenant::open`
//!   checks the binding); the actor key only routes. The router names the
//!   scope in `x-norn-cell`, and the actor refuses a scope its key does not
//!   name.
//! - TIMER-ALARM-1: the wake rules of `wake.mjs`, over [`crate::alarm`].
//!   Writer admission, a write's deadline update and the wake's final
//!   deadline decision run under one actor lock (`deadline`): a wake that
//!   found no writer can never erase the recovery wake of a writer admitted
//!   after it looked.
//! - ENGINE-UPGRADE-1: every start reads the cell's storage version first,
//!   without writing, and a cell this engine refuses (a newer or unreadable
//!   stamp, or one it cannot read) fails the start. A started actor's
//!   `RivetKit` claims each due schedule row (it deletes the one-shot rows
//!   from `_rivet_schedule_events`) before any handler runs, at the end of
//!   its start and whenever a trigger comes due, so only an actor that never
//!   starts leaves a refused cell's database, host tables included, as it
//!   was. The engine retries the start; a release that reads the cell serves
//!   it. The wake checks the version again before it writes.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow};
use async_trait::async_trait;
use norn_api::scope::CellScope;
use norn_cell::CellError;
use norn_cell::engine_release;
use rivetkit::action::Raw;
use rivetkit::{Action, Actor, Ctx, Handler, Request, Response, WebSocket};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{Mutex, MutexGuard};

use crate::alarm::{Alarm, WAKE_ACTION};
use crate::config::{FIXTURE_STAGES, HostConfig};
use crate::core::{Call, CellCore, OpenArgs, Reply};
use crate::thread::{CellGone, CellThread};

/// The cell actor's name in the registry.
pub const CELL_ACTOR: &str = "cell";
/// The header the router names the cell's scope in, `[orgId, environmentId]`.
pub const CELL_HEADER: &str = "x-norn-cell";
/// The largest submission a cell accepts (an ad-hoc `QuestionIR`), in bytes.
pub const MAX_BODY: usize = 65_536;
/// The largest provisioning or definitions submission, in bytes.
pub const MAX_PROVISION_BODY: usize = 1_048_576;
/// Paths that carry a pack, bounded like a provisioning set.
pub const LARGE_BODY_PATHS: &[&str] = &[
    "/v1/cell:provision",
    "/v1/cell/definitions:plan",
    "/v1/cell/definitions:apply",
];
/// How long a guard or recovery wake waits (`wake.mjs` `RECOVERY_MS`).
pub const RECOVERY_MS: u64 = 60_000;
/// A failed cold open is not retried sooner (`OPEN_RETRY_MS`).
const OPEN_RETRY_MS: u64 = 60_000;
/// A hung JWKS fetch must not hold the cell.
const JWKS_TIMEOUT: Duration = Duration::from_secs(5);
/// Engine steps one request may take (D102).
const ENGINE_STEPS: usize = 64;
/// A hung engine call must not hold the cell.
const ENGINE_TIMEOUT: Duration = Duration::from_mins(2);
/// The largest engine answer read back, in bytes.
const MAX_ENGINE_BODY: usize = 64 * 1024 * 1024;
/// A source page or an uploaded object, read once and bounded.
const MAX_SOURCE_BODY: usize = 20 * 1024 * 1024;
/// One outbound source or storage request may take this long.
const SOURCE_FETCH_TIMEOUT: Duration = Duration::from_secs(30);
const FIRST_STEP_ROWS: usize = 8;
const MAX_STEP_ROWS: usize = 500;

/// The host clock, Unix milliseconds.
#[must_use]
#[allow(
    clippy::disallowed_methods,
    reason = "the host is the trusted boundary that supplies the cell's clock, as the Worker passes Date.now()"
)]
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

fn signed(ms: u64) -> i64 {
    i64::try_from(ms).unwrap_or(i64::MAX)
}

/// `CellScope::key`: `%` and `/` escaped in each part.
#[must_use]
pub fn cell_key(scope: &CellScope) -> String {
    let escape = |part: &str| part.replace('%', "%25").replace('/', "%2F");
    format!(
        "{}/{}",
        escape(&scope.org_id),
        escape(&scope.environment_id)
    )
}

/// The scope a router header names.
///
/// # Errors
/// The header is not `[orgId, environmentId]`.
pub fn scope_of(header: Option<&str>) -> Result<CellScope> {
    let parts: Vec<String> =
        serde_json::from_str(header.context("no cell scope")?).context("a malformed cell scope")?;
    match parts.as_slice() {
        [org_id, environment_id] => Ok(CellScope {
            org_id: org_id.clone(),
            environment_id: environment_id.clone(),
        }),
        _ => Err(anyhow!("a cell scope is [orgId, environmentId]")),
    }
}

/// The key this actor's scope must have: the cell key, or (conformance
/// only) a conformance object of the synthetic scope.
fn key_admits(key: &str, scope: &CellScope, conformance: bool) -> bool {
    key == cell_key(scope)
        || (conformance && key.starts_with("conformance/") && crate::conformance_scope(scope))
}

/// Whether `ctx`'s actor must refuse `scope` (its key names another).
#[must_use]
pub fn scope_refused(ctx: &Ctx<CellActor>, scope: &CellScope, conformance: bool) -> bool {
    !key_admits(&key_of(ctx), scope, conformance)
}

/// The one key segment of `ctx`'s actor.
fn key_of<A: Actor>(ctx: &Ctx<A>) -> String {
    match ctx.key().as_slice() {
        [rivetkit::ActorKeySegment::String(key)] => key.clone(),
        _ => String::new(),
    }
}

/// A JSON reply.
#[must_use]
pub fn json_response(status: u16, body: impl Into<Vec<u8>>) -> Response {
    let headers =
        std::iter::once(("content-type".to_owned(), "application/json".to_owned())).collect();
    Response::from_parts(status, headers, body.into())
        .unwrap_or_else(|_error| Response::new(Vec::new()))
}

/// A `norn/refusal@2` reply.
#[must_use]
pub fn refusal(
    status: u16,
    code: &str,
    category: &str,
    message: &str,
    explanation: &str,
    retryable: bool,
) -> Response {
    json_response(
        status,
        json!({"format": "norn/refusal@2", "retryable": retryable, "reason": null, "code": code, "category": category, "message": message, "explanation": explanation}).to_string(),
    )
}

/// The JWKS document this process fetched last, reused by cold opens for
/// `OPEN_RETRY_MS` (one issuer fetch a minute however many cells open).
#[derive(Clone)]
struct Jwks {
    url: String,
    text: String,
    at: u64,
}

static LAST_JWKS: std::sync::Mutex<Option<Jwks>> = std::sync::Mutex::new(None);

/// What the actor keeps about its cell between requests.
#[derive(Default)]
struct OpenState {
    /// The last failed open, its error and when (Unix ms).
    failure: Option<(String, u64)>,
    /// The scope was stored for alarms.
    scope_kept: bool,
    /// The last operator error logged.
    policy_error: Option<String>,
}

/// The scheduled action that fires the actor's alarm ([`crate::alarm`]).
#[derive(Debug, Serialize, Deserialize)]
pub struct WakeNow;

impl Action for WakeNow {
    type Output = ();

    const NAME: &'static str = WAKE_ACTION;
}

/// One tenant cell's actor.
pub struct CellActor {
    config: Arc<HostConfig>,
    cell: CellThread,
    alarm: Alarm,
    open: Mutex<OpenState>,
    /// Requests whose writes may not be reflected in the next deadline yet.
    writing: AtomicU32,
    /// Held across writer admission (`writing` and the guard wake), a
    /// write's deadline update, and the wake's final deadline decision and
    /// its update: the three never interleave.
    deadline: Mutex<()>,
    live: crate::live::Sockets,
    http: reqwest::Client,
    /// The wake's conformance seams.
    #[cfg(feature = "conformance")]
    pub(crate) seam: crate::conformance::WakeSeam,
    /// This instance (conformance identity).
    pub instance: String,
    /// When this instance was constructed (Unix ms).
    pub constructed_at: u64,
}

impl std::fmt::Debug for CellActor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CellActor")
            .field("instance", &self.instance)
            .finish_non_exhaustive()
    }
}

/// An outbound client that never follows a redirect.
///
/// # Errors
/// The TLS backend failed to initialize.
pub fn outbound_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .context("build the outbound HTTP client")
}

#[async_trait]
impl Actor for CellActor {
    type Input = ();
    type Actions = (WakeNow,);
    type Events = ();
    type ConnParams = ();
    type Action = Raw;

    /// Requests interleave at their awaits, as on a Durable Object; the cell
    /// thread serializes every cell call.
    const CONCURRENT_HTTP_CALLBACKS: bool = true;

    async fn create(ctx: &Ctx<Self>) -> Result<Self> {
        let config = crate::config::get();
        let cell = CellThread::spawn(
            ctx.actor_id(),
            ctx.sql().clone(),
            tokio::runtime::Handle::current(),
            config.conformance,
        )?;
        Ok(Self {
            config,
            cell,
            alarm: Alarm::default(),
            open: Mutex::new(OpenState::default()),
            writing: AtomicU32::new(0),
            deadline: Mutex::new(()),
            live: crate::live::Sockets::default(),
            http: outbound_client()?,
            #[cfg(feature = "conformance")]
            seam: crate::conformance::WakeSeam::default(),
            instance: uuid::Uuid::new_v4().to_string(),
            constructed_at: now_ms(),
        })
    }

    /// Every start re-arms the alarm's trigger from its row: a trigger lost
    /// with a crash comes back with the restart.
    ///
    /// A cell this engine refuses fails the start, before anything is
    /// written: `RivetKit` drains the overdue schedule rows right after a
    /// start that succeeds, deleting each before its handler could refuse
    /// it (ENGINE-UPGRADE-1, the module docs).
    async fn on_start(self: Arc<Self>, ctx: Ctx<Self>) -> Result<()> {
        let admitted = self.storage_admitted().await;
        #[cfg(feature = "conformance")]
        if !matches!(admitted, Ok(true)) {
            crate::conformance::start_refused(&ctx).await;
        }
        match admitted {
            Ok(true) => {}
            Ok(false) => {
                return Err(anyhow!(
                    "this engine does not serve the stored cell; the actor does not start"
                ));
            }
            Err(error) => {
                return Err(error.context("the actor does not start"));
            }
        }
        if let Err(error) = self.alarm.resync(&ctx).await {
            tracing::error!(error = %format!("{error:#}"), "alarm resync failed");
        }
        Ok(())
    }

    async fn on_fetch(self: Arc<Self>, ctx: Ctx<Self>, req: Request) -> Result<Response> {
        Ok(self.serve_http(&ctx, &req).await)
    }

    async fn on_websocket(
        self: Arc<Self>,
        ctx: Ctx<Self>,
        ws: WebSocket,
        _req: Request,
    ) -> Result<()> {
        crate::live::accept(self, ctx, &ws);
        Ok(())
    }
}

impl Handler<WakeNow> for CellActor {
    async fn handle(self: Arc<Self>, ctx: Ctx<Self>, _action: WakeNow) -> Result<()> {
        self.wake(&ctx, signed(now_ms())).await
    }
}

impl CellActor {
    /// The configuration this actor serves with.
    #[must_use]
    pub fn config(&self) -> &HostConfig {
        &self.config
    }

    /// The actor's cell thread.
    #[must_use]
    pub const fn cell(&self) -> &CellThread {
        &self.cell
    }

    /// The actor's alarm (conformance seams).
    #[cfg(feature = "conformance")]
    pub(crate) const fn alarm(&self) -> &Alarm {
        &self.alarm
    }

    /// Forget the last failed open and the kept-scope flag (conformance
    /// seams: the next request or wake opens the cell from storage again).
    #[cfg(feature = "conformance")]
    pub(crate) async fn forget_open(&self) {
        let mut state = self.open.lock().await;
        state.failure = None;
        state.scope_kept = false;
    }

    /// The alarm's trigger fired at `now` (Unix ms): the `WakeNow` handler.
    /// A refused cell's actor does not start, so this is not reached for
    /// one; checked again, the wake writes nothing for a cell restamped
    /// while its actor runs.
    pub(crate) async fn wake(self: &Arc<Self>, ctx: &Ctx<Self>, now: i64) -> Result<()> {
        if !self.storage_admitted().await? {
            return Ok(());
        }
        let actor = Arc::clone(self);
        let woken = ctx.clone();
        self.alarm
            .fire(ctx, now, |_delivery| async move {
                actor.alarm_run(&woken).await
            })
            .await
    }

    async fn serve_http(self: &Arc<Self>, ctx: &Ctx<Self>, req: &Request) -> Response {
        let header = req
            .headers()
            .get(CELL_HEADER)
            .and_then(|value| value.to_str().ok());
        let scope = match scope_of(header) {
            Ok(scope) => scope,
            Err(error) => {
                return refusal(
                    400,
                    "host.scope",
                    "invalid",
                    "no cell scope",
                    &format!("{error:#}"),
                    false,
                );
            }
        };
        if scope_refused(ctx, &scope, self.config.conformance) {
            return refusal(
                403,
                "host.scope",
                "permission",
                "this cell serves another scope",
                "the scope and the actor key differ",
                false,
            );
        }
        if req.has_body_stream() {
            tracing::error!(
                "a streamed request body reached the cell; the router sends bodies whole"
            );
            return refusal(
                413,
                "request.too_large",
                "limit",
                "the request body is too large",
                "submissions are sent whole",
                false,
            );
        }
        let path = req.uri().path().to_owned();
        let limit = if LARGE_BODY_PATHS.contains(&path.as_str()) {
            MAX_PROVISION_BODY
        } else {
            MAX_BODY
        };
        if req.body().len() > limit {
            return refusal(
                413,
                "request.too_large",
                "limit",
                "the request body is too large",
                &format!("submissions are at most {limit} bytes"),
                false,
            );
        }
        let call = Call {
            method: req.method().as_str().to_owned(),
            path,
            query: req.uri().query().unwrap_or_default().to_owned(),
            authorization: req
                .headers()
                .get("authorization")
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned),
            now_ms: now_ms(),
            body: (req.method() == http::Method::POST)
                .then(|| String::from_utf8_lossy(req.body()).into_owned()),
        };
        #[cfg(feature = "conformance")]
        if self.config.conformance {
            return crate::conformance::serve_cell(self, ctx, req, scope, call).await;
        }
        self.fetch(ctx, scope, call).await
    }

    /// Serve one request of `scope` (`Cell.fetch`).
    pub async fn fetch(
        self: &Arc<Self>,
        ctx: &Ctx<Self>,
        scope: CellScope,
        call: Call,
    ) -> Response {
        if call.path == "/v1/subscribe" {
            return refusal(
                426,
                "live.upgrade_required",
                "invalid",
                "live reads are a WebSocket",
                &format!(
                    "open a WebSocket with the {} protocol",
                    crate::live::PROTOCOL
                ),
                false,
            );
        }
        if let Err(error) = self.ready(ctx, &scope).await {
            tracing::error!(error = %error, "cell open refused");
            return refusal(
                503,
                "host.unavailable",
                "host",
                "the cell is unavailable",
                "retry the request",
                true,
            );
        }
        if call.method == "GET" {
            return self.respond(ctx, call).await;
        }
        if let Err(error) = self.before_write(ctx, now_ms()).await {
            tracing::error!(error = %format!("{error:#}"), "write guard failed");
            return refusal(
                503,
                "host.unavailable",
                "host",
                "the cell is unavailable",
                "retry the request",
                true,
            );
        }
        let response = self.respond(ctx, call).await;
        #[cfg(feature = "conformance")]
        self.seam.after_respond().await;
        if let Err(error) = self.after_write(ctx, now_ms()).await {
            tracing::error!(error = %format!("{error:#}"), "alarm rearm failed; the recovery wake stands");
        }
        if let Err(error) = self.keep_scope(ctx, &scope).await {
            tracing::error!(error = %format!("{error:#}"), "keeping the scope failed");
        }
        response
    }

    /// Open the cell, then refresh aged keys (`Cell.ready`).
    ///
    /// # Errors
    /// The cell does not open.
    pub async fn ready(self: &Arc<Self>, ctx: &Ctx<Self>, scope: &CellScope) -> Result<()> {
        self.open_cell(ctx, scope).await?;
        let now = now_ms();
        if self
            .cell
            .call(move |slot| slot.core.as_ref().is_some_and(|core| core.jwks_due(now)))
            .await?
        {
            self.refresh_jwks().await;
        }
        Ok(())
    }

    /// The pinned JWKS document and when it was fetched; `reuse` accepts this
    /// process's last one while it is younger than `OPEN_RETRY_MS`.
    async fn jwks(&self, reuse: bool) -> Result<Jwks> {
        let url = self
            .config
            .jwks_url
            .clone()
            .context("NORN_HOST_JWKS_URL is not set")?;
        if reuse {
            let last = LAST_JWKS
                .lock()
                .map_err(|_poisoned| anyhow!("the JWKS cache is poisoned"))?
                .clone();
            if let Some(last) = last
                && last.url == url
                && now_ms()
                    .checked_sub(last.at)
                    .is_some_and(|age| age < OPEN_RETRY_MS)
            {
                return Ok(last);
            }
        }
        let response = self
            .http
            .get(&url)
            .timeout(JWKS_TIMEOUT)
            .send()
            .await
            .context("JWKS fetch failed")?;
        if response.status() != reqwest::StatusCode::OK {
            return Err(anyhow!("JWKS fetch failed: HTTP {}", response.status()));
        }
        let text = response.text().await.context("JWKS read failed")?;
        let fetched = Jwks {
            url,
            text,
            at: now_ms(),
        };
        *LAST_JWKS
            .lock()
            .map_err(|_poisoned| anyhow!("the JWKS cache is poisoned"))? = Some(fetched.clone());
        Ok(fetched)
    }

    async fn refresh_jwks(&self) -> bool {
        let fetched = match self.jwks(false).await {
            Ok(fetched) => fetched,
            Err(error) => {
                tracing::error!(error = %format!("{error:#}"), "JWKS refresh failed");
                return false;
            }
        };
        let installed = self
            .cell
            .call(move |slot| {
                slot.core
                    .as_ref()
                    .map(|core| core.replace_jwks(&fetched.text, fetched.at))
            })
            .await;
        matches!(installed, Ok(Some(Ok(()))))
    }

    /// One opening at a time; a failed open is refused at once for
    /// `OPEN_RETRY_MS` with the same error (`Cell.open`).
    async fn open_cell(self: &Arc<Self>, ctx: &Ctx<Self>, scope: &CellScope) -> Result<()> {
        let mut state = self.open.lock().await;
        if self.cell.call(|slot| slot.core.is_some()).await? {
            return Ok(());
        }
        if let Some((error, at)) = &state.failure {
            if now_ms()
                .checked_sub(*at)
                .is_some_and(|elapsed| elapsed < OPEN_RETRY_MS)
            {
                return Err(anyhow!("{error}"));
            }
            state.failure = None;
        }
        match self.cold_open(scope).await {
            Ok(serving) => {
                if serving {
                    drop(state);
                    self.keep_scope(ctx, scope).await?;
                    self.opened(ctx, now_ms()).await?;
                }
                Ok(())
            }
            Err(error) => {
                let text = format!("{error:#}");
                state.failure = Some((text.clone(), now_ms()));
                Err(anyhow!(text))
            }
        }
    }

    /// Construct the cell on its thread; whether it serves a provisioned cell.
    async fn cold_open(&self, scope: &CellScope) -> Result<bool> {
        let fixtures = norn_cell_do::fixtures_enabled();
        if fixtures
            && !self
                .config
                .stage
                .as_deref()
                .is_some_and(|stage| FIXTURE_STAGES.contains(&stage))
        {
            return Err(anyhow!(
                "this cell build accepts fixture principals and serves only a smoke or test stage (NORN_STAGE)"
            ));
        }
        let jwks = match self.config.issuer {
            Some(_) => Some(self.jwks(true).await?),
            None => None,
        };
        let args = OpenArgs {
            scope: scope.clone(),
            issuer: self.config.issuer.clone(),
            jwks_at_ms: jwks.as_ref().map_or_else(now_ms, |jwks| jwks.at),
            jwks: jwks.map(|jwks| jwks.text),
            lineage_key: self.config.lineage_key.clone(),
        };
        let sources = self.config.sources.clone();
        let vars = self.config.connect_vars.clone();
        let analytics = self.config.olap_url.is_some();
        let signing_key = self.config.olap_signing_key.clone();
        let olap_engine = self.config.olap_engine.clone();
        self.cell
            .call(move |slot| -> Result<bool, String> {
                let mut core = CellCore::open(slot.engine(), args)?;
                if let Err(error) = core.configure_connect(&sources, &vars) {
                    tracing::error!(error = %error, "source connections not configured");
                }
                if analytics
                    && let Err(error) =
                        core.enable_analytics(signing_key.as_deref(), olap_engine.as_deref())
                {
                    tracing::error!(error = %error, "analytics not enabled");
                }
                let serving = core.serving();
                slot.core = Some(core);
                Ok(serving)
            })
            .await?
            .map_err(|error| anyhow!(error))
    }

    /// Store the scope for alarms, for a serving cell only.
    async fn keep_scope(&self, ctx: &Ctx<Self>, scope: &CellScope) -> Result<()> {
        let mut state = self.open.lock().await;
        if state.scope_kept || !self.serving().await? {
            return Ok(());
        }
        let wanted = serde_json::to_string(&[&scope.org_id, &scope.environment_id])?;
        if self.alarm.scope(ctx).await?.as_deref() != Some(wanted.as_str()) {
            self.alarm.keep_scope(ctx, &wanted).await?;
        }
        state.scope_kept = true;
        Ok(())
    }

    /// ENGINE-UPGRADE-1, read only: whether this host opens the cell stored
    /// in the actor's database (none yet, current, or upgradable within
    /// [`RIVET_ENGINE`](crate::config::RIVET_ENGINE)). A refusal is logged and
    /// answers `false`.
    ///
    /// # Errors
    /// The cell thread stopped, or the storage could not be read.
    async fn storage_admitted(&self) -> Result<bool> {
        let admitted = self
            .cell
            .call(|slot| {
                engine_release::stored_version(&slot.engine()()).and_then(|stored| admit(&stored))
            })
            .await?;
        match admitted {
            Ok(_) => Ok(true),
            Err(
                refusal @ (CellError::EngineDowngrade { .. }
                | CellError::EngineUnknownVersion { .. }
                | CellError::EngineUpgradeUnsupported { .. }),
            ) => {
                tracing::error!(error = %refusal, "this engine does not serve the stored cell; nothing is written");
                Ok(false)
            }
            Err(error) => Err(anyhow!("read the cell's storage version: {error}")),
        }
    }

    /// Take the deadline lock (the `deadline` field).
    async fn deadline_lock(&self) -> MutexGuard<'_, ()> {
        #[cfg(feature = "conformance")]
        match self.deadline.try_lock() {
            Ok(held) => return held,
            Err(_busy) => self.seam.waiting(),
        }
        self.deadline.lock().await
    }

    async fn serving(&self) -> Result<bool, CellGone> {
        self.cell
            .call(|slot| slot.core.as_ref().is_some_and(CellCore::serving))
            .await
    }

    async fn next_due(&self, now: u64) -> Result<Option<u64>> {
        self.cell
            .call(move |slot| {
                slot.core
                    .as_mut()
                    .map_or(Ok(None), |core| core.next_due(now))
            })
            .await?
            .map_err(|error| anyhow!(error))
    }

    // -- the wake (wake.mjs)

    /// Move the alarm to `at` when none is set or the set one is later.
    async fn earliest(&self, ctx: &Ctx<Self>, at: Option<u64>, now: u64) -> Result<()> {
        let Some(at) = at else { return Ok(()) };
        let when = signed(at.max(now));
        if self
            .alarm
            .get(ctx)
            .await?
            .is_some_and(|current| current <= when)
        {
            return Ok(());
        }
        self.alarm.set(ctx, when).await
    }

    async fn opened(&self, ctx: &Ctx<Self>, now: u64) -> Result<()> {
        if !self.serving().await? {
            return Ok(());
        }
        let _decision = self.deadline_lock().await;
        let next = self.next_due(now).await?;
        self.earliest(ctx, next, now).await
    }

    /// Before a request that may write: the guard wake is durable first.
    async fn before_write(&self, ctx: &Ctx<Self>, now: u64) -> Result<()> {
        let _admission = self.deadline_lock().await;
        self.writing.fetch_add(1, Ordering::AcqRel);
        let guarded = async {
            if !self.serving().await? {
                return Ok(());
            }
            self.earliest(ctx, Some(now.saturating_add(RECOVERY_MS)), now)
                .await
        };
        let result = guarded.await;
        if result.is_err() {
            self.release_writer();
        }
        result
    }

    fn release_writer(&self) {
        let _previous = self
            .writing
            .try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                Some(count.saturating_sub(1))
            });
    }

    /// After that request, whatever its reply: the alarm follows the cell's
    /// next deadline.
    async fn after_write(&self, ctx: &Ctx<Self>, now: u64) -> Result<()> {
        let _decision = self.deadline_lock().await;
        self.release_writer();
        if !self.serving().await? {
            return Ok(());
        }
        let next = self.next_due(now).await?;
        self.earliest(ctx, next, now).await
    }

    /// The alarm fired (`Cell.alarm`, `Wake.alarm`).
    async fn alarm_run(self: &Arc<Self>, ctx: &Ctx<Self>) -> Result<()> {
        let Some(kept) = self.alarm.scope(ctx).await? else {
            return Ok(());
        };
        let scope = scope_of(Some(&kept))?;
        self.ready(ctx, &scope).await?;
        if !self.serving().await? {
            self.alarm.forget_scope(ctx).await?;
            return Ok(());
        }
        let now = now_ms();
        self.alarm
            .set(ctx, signed(now.saturating_add(RECOVERY_MS)))
            .await?;
        let due = self
            .cell
            .call(move |slot| slot.core.as_mut().map(|core| core.run_due(now)))
            .await?
            .context("the cell closed")?
            .map_err(|error| anyhow!(error))?;
        let sync = self
            .cell
            .call(move |slot| slot.core.as_mut().and_then(|core| core.sync_due(now)))
            .await?;
        {
            // No writer is admitted, and none updates its deadline, until
            // this decision is stored.
            let _decision = self.deadline_lock().await;
            let writing = self.writing.load(Ordering::Acquire) > 0;
            let next = self.next_due(now).await?;
            let recovery = now.saturating_add(RECOVERY_MS);
            #[cfg(feature = "conformance")]
            self.seam.decided().await;
            if writing {
                if let Some(next) = next
                    && next.max(now) < recovery
                {
                    self.alarm.set(ctx, signed(next.max(now))).await?;
                }
            } else if let Some(next) = next {
                self.alarm.set(ctx, signed(next.max(now))).await?;
            } else {
                self.alarm.delete(ctx).await?;
            }
        }
        if let Some(failed) = due.failed {
            tracing::error!(failed = %failed, "due work not handled");
        }
        if due.committed {
            self.publish(ctx);
        }
        if let Some(sync) = sync {
            self.run_sync(ctx, sync).await;
        }
        Ok(())
    }

    // -- serving

    /// The cell's own answer to `call`, as its caller (a live subscription's
    /// re-read): no wake, no continuation, no engine call.
    ///
    /// # Errors
    /// The cell call panicked or the cell stopped.
    pub async fn ask(&self, call: Call) -> Result<Reply, CellGone> {
        self.cell_fetch(call).await
    }

    async fn cell_fetch(&self, call: Call) -> Result<Reply, CellGone> {
        self.cell
            .call(move |slot| match slot.core.as_mut() {
                Some(core) => core.fetch(&call),
                None => Reply {
                    status: 503,
                    body: json!({"format": "norn/refusal@2", "retryable": true, "reason": null, "code": "host.unavailable", "category": "host", "message": "the cell is unavailable", "explanation": "retry the request"}).to_string(),
                    ..Reply::default()
                },
            })
            .await
    }

    /// Serve one request on the opened cell (`Cell.respond`).
    pub async fn respond(self: &Arc<Self>, ctx: &Ctx<Self>, call: Call) -> Response {
        match self.respond_inner(ctx, &call).await {
            Ok(response) => response,
            Err(error) => {
                tracing::error!(error = %format!("{error:#}"), "request failed");
                refusal(
                    503,
                    "host.unavailable",
                    "host",
                    "the cell is unavailable",
                    "retry the request",
                    true,
                )
            }
        }
    }

    async fn respond_inner(self: &Arc<Self>, ctx: &Ctx<Self>, call: &Call) -> Result<Response> {
        let mut reply = self.cell_fetch(call.clone()).await?;
        if reply.refresh_jwks && self.refresh_jwks().await {
            reply = self.cell_fetch(call.clone()).await?;
        }
        while let Some((token, planned)) = reply.then.take() {
            let (status, bytes) = self.perform(&planned).await;
            let now = now_ms();
            reply = self
                .cell
                .call(move |slot| match slot.core.as_mut() {
                    Some(core) => core.resume(&token, status, &bytes, now),
                    None => Reply {
                        status: 503,
                        body: "{}".to_owned(),
                        ..Reply::default()
                    },
                })
                .await?;
        }
        if let Some(sync) = reply.start_sync.take() {
            self.keep_sync(ctx, sync);
        }
        let now = now_ms();
        if let Some(sync) = self
            .cell
            .call(move |slot| slot.core.as_mut().and_then(|core| core.sync_due(now)))
            .await?
        {
            self.keep_sync(ctx, sync);
        }
        {
            let mut state = self.open.lock().await;
            if reply.policy_error != state.policy_error {
                if let Some(error) = &reply.policy_error {
                    tracing::error!(error = %error, "cell needs an operator");
                }
                state.policy_error.clone_from(&reply.policy_error);
            }
        }
        let settled = self.settle(call, reply).await?;
        let reply = match settled {
            Settled::Reply(reply) => reply,
            Settled::Stream(response) => return Ok(response),
        };
        if call.method != "GET" && reply.status < 400 {
            self.publish(ctx);
        }
        Ok(json_response(reply.status, reply.body))
    }

    /// Run the cell's engine calls until it answers (`Cell.settle`).
    async fn settle(&self, call: &Call, mut reply: Reply) -> Result<Settled> {
        let mut steps = 0;
        while let Some(engine) = reply.engine.take() {
            if steps >= ENGINE_STEPS {
                return Ok(Settled::Reply(Reply {
                    status: 503,
                    body: json!({"format": "norn/refusal@2", "retryable": true, "reason": null, "code": "analytics.unsettled", "category": "host", "message": "the analytics engine did not settle", "explanation": "retry the request"}).to_string(),
                    ..Reply::default()
                }));
            }
            steps = steps.saturating_add(1);
            let parsed: Value = serde_json::from_str(&engine).context("a malformed engine call")?;
            let (status, body) = match self.engine_call(&parsed).await {
                EngineAnswer::Answer(status, body) => (status, body),
                EngineAnswer::Stream(response) => return Ok(Settled::Stream(response)),
            };
            let resumed = call.clone();
            reply = self
                .cell
                .call(move |slot| match slot.core.as_mut() {
                    Some(core) => core.resume_engine(&resumed, &engine, status, body),
                    None => Reply {
                        status: 503,
                        body: "{}".to_owned(),
                        ..Reply::default()
                    },
                })
                .await?;
        }
        Ok(Settled::Reply(reply))
    }

    /// D102: one engine call the cell asked for; a `stream` call (an export
    /// the cell authorized) answers the caller with the engine's file.
    async fn engine_call(&self, call: &Value) -> EngineAnswer {
        let unreachable = || {
            EngineAnswer::Answer(503, json!({"code": "analytics.unavailable", "detail": "the analytics engine is unreachable"}).to_string())
        };
        let (Some(url), Some(op)) = (
            self.config.olap_url.as_deref(),
            call.get("op").and_then(Value::as_str),
        ) else {
            return unreachable();
        };
        let body = call
            .get("body")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let sent = self
            .http
            .post(format!("{url}/v1/{op}"))
            // OLAP-CRED-1: the credential the cell minted for this call.
            .bearer_auth(
                call.get("credential")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            )
            .header("content-type", "application/json")
            .body(body)
            .timeout(ENGINE_TIMEOUT)
            .send()
            .await;
        let response = match sent {
            Ok(response) => response,
            Err(error) => {
                tracing::error!(error = %error, "analytics engine call failed");
                return unreachable();
            }
        };
        let status = response.status().as_u16();
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("application/octet-stream")
            .to_owned();
        let Some(bytes) = read_bounded(response, MAX_ENGINE_BODY).await else {
            return unreachable();
        };
        if status != 200 {
            tracing::error!(op, status, "analytics engine answered");
        }
        match call.get("stream").and_then(Value::as_object) {
            Some(stream) if status == 200 => {
                let mut headers: std::collections::HashMap<String, String> = stream
                    .iter()
                    .filter_map(|(name, value)| {
                        value.as_str().map(|value| (name.clone(), value.to_owned()))
                    })
                    .collect();
                headers.insert("content-type".to_owned(), content_type);
                headers.insert("cache-control".to_owned(), "no-store".to_owned());
                EngineAnswer::Stream(
                    Response::from_parts(200, headers, bytes)
                        .unwrap_or_else(|_error| Response::new(Vec::new())),
                )
            }
            Some(_) | None => {
                EngineAnswer::Answer(status, String::from_utf8_lossy(&bytes).into_owned())
            }
        }
    }

    /// Perform exactly the request the cell planned; the status and bytes go
    /// back to the cell, unread here. A network failure is status 0.
    async fn perform(&self, planned: &str) -> (u16, Vec<u8>) {
        let Ok(planned) = serde_json::from_str::<Value>(planned) else {
            return (0, Vec::new());
        };
        let Some(url) = planned.get("url").and_then(Value::as_str) else {
            return (0, Vec::new());
        };
        let method = planned
            .get("method")
            .and_then(Value::as_str)
            .and_then(|method| reqwest::Method::from_bytes(method.as_bytes()).ok())
            .unwrap_or(reqwest::Method::GET);
        let mut request = self.http.request(method, url).timeout(SOURCE_FETCH_TIMEOUT);
        if let Some(headers) = planned.get("headers").and_then(Value::as_object) {
            for (name, value) in headers {
                if let Some(value) = value.as_str() {
                    request = request.header(name.as_str(), value);
                }
            }
        }
        if let Some(body) = planned.get("body").and_then(Value::as_str) {
            request = request.body(body.to_owned());
        }
        match request.send().await {
            Ok(response) => {
                let status = response.status().as_u16();
                read_bounded(response, MAX_SOURCE_BODY)
                    .await
                    .map_or((413, Vec::new()), |bytes| (status, bytes))
            }
            Err(_error) => (0, Vec::new()),
        }
    }

    /// After a commit, every live subscription on this actor is answered again.
    pub fn publish(self: &Arc<Self>, ctx: &Ctx<Self>) {
        if self.live.is_empty() {
            return;
        }
        let actor = Arc::clone(self);
        ctx.wait_until(async move { crate::live::publish(&actor).await });
    }

    /// The actor's live sockets.
    #[must_use]
    pub const fn sockets(&self) -> &crate::live::Sockets {
        &self.live
    }

    fn keep_sync(self: &Arc<Self>, ctx: &Ctx<Self>, id: String) {
        let actor = Arc::clone(self);
        let ctx_inner = ctx.clone();
        ctx.wait_until(async move { actor.run_sync(&ctx_inner, id).await });
    }

    /// Run one claimed sync to its end (`Cell.runSync`).
    async fn run_sync(self: &Arc<Self>, ctx: &Ctx<Self>, id: String) {
        if let Err(error) = self.before_write(ctx, now_ms()).await {
            tracing::error!(error = %format!("{error:#}"), "source sync not started");
            return;
        }
        if let Err(error) = self.sync_run(ctx, &id).await {
            tracing::error!(error = %format!("{error:#}"), "source sync failed");
        }
        if let Err(error) = self.after_write(ctx, now_ms()).await {
            tracing::error!(error = %format!("{error:#}"), "alarm rearm after a sync failed");
        }
    }

    async fn sync_step(
        &self,
        id: &str,
        response: Option<(u16, Vec<u8>)>,
        rows: usize,
    ) -> Result<Value> {
        let id = id.to_owned();
        let now = now_ms();
        let text = self
            .cell
            .call(move |slot| match slot.core.as_mut() {
                Some(core) => core.sync_step(
                    &id,
                    response
                        .as_ref()
                        .map(|(status, body)| (*status, body.as_slice())),
                    now,
                    Some(rows),
                ),
                None => String::from(r#"{"done":{"outcome":"failed"}}"#),
            })
            .await?;
        serde_json::from_str(&text).context("a malformed sync step")
    }

    async fn sync_run(self: &Arc<Self>, ctx: &Ctx<Self>, id: &str) -> Result<()> {
        let started = std::time::Instant::now();
        let budget = u128::from(self.config.sync_step_ms);
        let mut rows = FIRST_STEP_ROWS;
        let (mut steps, mut fetches) = (0_u32, 0_u32);
        let mut response = None;
        loop {
            let before = std::time::Instant::now();
            let step = self.sync_step(id, response.take(), rows).await?;
            // SYNC-YIELD-1: requests waiting on this cell run between steps.
            tokio::task::yield_now().await;
            steps = steps.saturating_add(1);
            let spent = before.elapsed().as_millis().max(1);
            let mapped = step.get("mapped").and_then(Value::as_u64).unwrap_or(0);
            if mapped > 0 {
                let paced = budget
                    .saturating_mul(u128::from(mapped))
                    .checked_div(spent)
                    .unwrap_or(0);
                rows = usize::try_from(paced)
                    .unwrap_or(MAX_STEP_ROWS)
                    .clamp(1, MAX_STEP_ROWS);
            }
            if let Some(planned) = step.get("request") {
                fetches = fetches.saturating_add(1);
                response = Some(self.perform(&planned.to_string()).await);
                continue;
            }
            if step.get("continue").is_some() {
                continue;
            }
            let done = step.get("done").cloned().unwrap_or(Value::Null);
            let outcome = done.get("outcome").map_or_default(Value::to_string);
            let landed = done.get("rowsLanded").map_or_default(Value::to_string);
            tracing::info!(
                sync = %id,
                outcome = %outcome,
                rows_landed = %landed,
                steps,
                fetches,
                wall_ms = %started.elapsed().as_millis(),
                "sync finished"
            );
            if done.get("rowsLanded").and_then(Value::as_u64).unwrap_or(0) > 0 {
                self.publish(ctx);
            }
            return Ok(());
        }
    }
}

enum Settled {
    Reply(Reply),
    Stream(Response),
}

enum EngineAnswer {
    Answer(u16, String),
    Stream(Response),
}

/// A response body counted as it arrives; `None` once it passes `limit`.
async fn read_bounded(mut response: reqwest::Response, limit: usize) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                if bytes.len().saturating_add(chunk.len()) > limit {
                    return None;
                }
                bytes.extend_from_slice(&chunk);
            }
            Ok(None) => return Some(bytes),
            Err(_error) => return None,
        }
    }
}

/// ENGINE-UPGRADE-1 on this host: what it does with a cell whose metadata
/// says `stored`, within [`RIVET_ENGINE`](crate::config::RIVET_ENGINE). A
/// stamp below the retained `RivetKit` channel's floor is refused like one
/// below the engine's own (`core.engine.upgrade_unsupported`); a newer or
/// unreadable stamp as everywhere else.
///
/// # Errors
/// As [`engine_release::admit`].
fn admit(stored: &engine_release::StoredVersion) -> Result<engine_release::Admission, CellError> {
    engine_release::admit(stored, crate::config::RIVET_ENGINE)
}

#[cfg(test)]
mod tests {
    use norn_cell::CellError;
    use norn_cell::engine_release::{
        self, Admission, EngineStamp, EngineVersion, STAMP_FORMAT, STORAGE_VERSION, StoredVersion,
    };

    use super::admit;
    use crate::config::{
        ADMISSION_FLOOR, RIVET_ENGINE, RIVETKIT_CHANNEL, RivetkitRelease, admission_floor,
    };

    fn stamped(storage: u32) -> StoredVersion {
        StoredVersion::Stamped(
            serde_json::to_string(&EngineStamp {
                format: STAMP_FORMAT.to_owned(),
                storage,
                writer: "drop-1".to_owned(),
            })
            .expect("a stamp serializes"),
        )
    }

    /// The host's floor is the retained channel's (or the engine's own, when
    /// higher), read from [`RIVET_ENGINE`] rather than restated: a cell
    /// stamped one below it is refused at startup, though the shared engine
    /// (`THIS_ENGINE`, floor `UPGRADES_FROM`) would upgrade it whenever the
    /// channel's floor is the higher one. The floor itself and every stamp up
    /// to `STORAGE_VERSION` are admitted.
    #[test]
    fn startup_refuses_a_stamp_below_the_retained_channel() {
        let floor = RIVET_ENGINE.upgrades_from;
        assert_eq!(floor, ADMISSION_FLOOR.max(engine_release::UPGRADES_FROM));
        assert_eq!(RIVET_ENGINE.storage, STORAGE_VERSION);
        let retired = floor.saturating_sub(1);
        if ADMISSION_FLOOR > engine_release::UPGRADES_FROM {
            assert!(matches!(
                engine_release::admit(&stamped(retired), engine_release::THIS_ENGINE),
                Ok(Admission::Upgrade { .. })
            ));
        }
        assert!(matches!(
            admit(&stamped(retired)),
            Err(CellError::EngineUpgradeUnsupported { cell, .. }) if cell == retired
        ));
        if floor > 1 {
            assert!(matches!(
                admit(&StoredVersion::Unstamped),
                Err(CellError::EngineUpgradeUnsupported { cell: 1, .. })
            ));
        }
        for retained in floor..=STORAGE_VERSION {
            assert!(
                admit(&stamped(retained)).is_ok(),
                "storage version {retained} is admitted"
            );
        }
        assert_eq!(admit(&StoredVersion::Fresh).ok(), Some(Admission::Fresh));
        assert!(matches!(
            admit(&stamped(STORAGE_VERSION.saturating_add(1))),
            Err(CellError::EngineDowngrade { .. })
        ));
    }

    /// Retiring the oldest release raises the floor past its interval: with
    /// a channel of 3.1.1 from 4 and 3.2.0 from 5, a stamp at 4 is admitted;
    /// once 3.1.1 is dropped, it is refused and 5 is still admitted.
    #[test]
    fn retiring_a_release_refuses_its_interval() {
        let both = [
            RivetkitRelease {
                rivetkit: "3.1.1",
                from_storage: 4,
            },
            RivetkitRelease {
                rivetkit: "3.2.0",
                from_storage: 5,
            },
        ];
        let window = |channel: &[RivetkitRelease]| EngineVersion {
            storage: 5,
            upgrades_from: admission_floor(channel),
        };
        let (_retired, retained) = both.split_at(1);
        assert_eq!(admission_floor(&both), 4);
        assert_eq!(admission_floor(retained), 5);
        assert_eq!(
            admission_floor(&[]),
            u32::MAX,
            "an empty channel admits nothing"
        );
        assert!(engine_release::admit(&stamped(4), window(&both)).is_ok());
        assert!(matches!(
            engine_release::admit(&stamped(4), window(retained)),
            Err(CellError::EngineUpgradeUnsupported { cell: 4, .. })
        ));
        assert_eq!(
            engine_release::admit(&stamped(5), window(retained)).ok(),
            Some(Admission::Current)
        );
        assert_eq!(admission_floor(RIVETKIT_CHANNEL), ADMISSION_FLOOR);
    }
}
