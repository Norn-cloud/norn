//! In-cell source sync (#921): the cell pulls its connected source systems
//! itself, through the generic engine ([`crate::pull`]), and lands every row
//! through its own mapping path ([`DevHost::ingest`](crate::host::DevHost::ingest)).
//!
//! The host owns the network: it asks [`Sources::step`](crate::sources::Sources::step) for the next request,
//! sends it exactly as planned and hands back the status and body. Credentials
//! are server configuration resolved here by the connector's secret reference
//! (`NORN_SECRET_<REF>`); a request carries them and nothing else does.
//!
//! SYNC-ONE-1: a connection runs at most one sync. [`Sources::claim`](crate::sources::Sources::claim) and
//! [`Sources::due`](crate::sources::Sources::due) check and mark it in one call, so two requests can never
//! start two syncs of one connection.
//! SYNC-ACTOR-1: a scheduled sync runs as the connection's own principal
//! (`connection:<id>`) and is recorded so; it never takes the rights of the
//! request that happened to trigger it. A manual sync records its caller.
//! SYNC-REPORT-1: each run is reported as observations of the cell's own
//! `norn-connect` source (dataset `connection` at its start and end,
//! `sync_run` at its end), mapped by `norn.connect.mapping` into
//! `source.Connection` and `source.SyncRun`. The connection's watermark is
//! read back from that object, so freshness is what the cell recorded.
//! SYNC-URL-1: a base URL is https, or http only to loopback or a
//! single-label (private network) host.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use norn_cell::MappingOutcome;
use norn_cell::engine::SqlEngine;
use norn_cell::mapping_bundle::MappingInput;
use norn_ir::RuntimeValue;
use serde::{Deserialize, Serialize};

use crate::host::DevHost;
use crate::pull::{Limits, Pull, PullError, PullProfile, Request, Watermark, observation};

/// The source system the cell reports its own syncs as.
pub const REPORT_SOURCE: &str = "norn-connect";
/// The mapping that lands sync reports as connections and runs.
pub const REPORT_MAPPING: &str = "norn.connect.mapping";
/// The connection object type.
pub const CONNECTION_TYPE: &str = "source.Connection";
/// Minutes between scheduled syncs when a connection does not say.
pub const DEFAULT_EVERY_MINUTES: u32 = 15;

/// One configured connection: which connector, mapping and endpoint. Server
/// configuration, never request input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConnectionConfig {
    /// The connection id, e.g. `erpnext.sandbox`.
    pub id: String,
    /// The source system (`erpnext`, `odoo`, `shopify`).
    pub system: String,
    /// The display name.
    pub label: String,
    /// The installed connector artifact carrying the `norn.pull/1` profile.
    pub connector: String,
    /// The installed mapping its rows go through.
    pub mapping: String,
    /// The connector instance the mapping's source schema pins.
    pub instance: String,
    /// The endpoint the profile's paths are relative to.
    pub base_url: String,
    /// Fixed, non-secret headers this endpoint needs.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// Minutes between scheduled syncs; `0` turns scheduling off.
    #[serde(default = "default_every")]
    pub every_minutes: u32,
    /// Rows per source request for every feed, in place of the connector's
    /// (SYNC-PAGE-1): a host tunes how many requests a sync makes without a
    /// new definitions version. At most the pull's item bound
    /// ([`Limits`]); the cell still maps and commits
    /// what a request returns in its own pages
    /// ([`PAGE_ROWS`](crate::host::PAGE_ROWS)).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_size: Option<u32>,
}

const fn default_every() -> u32 {
    DEFAULT_EVERY_MINUTES
}

/// What started a sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    /// The connection's schedule.
    Schedule,
    /// A person asked for it.
    Manual,
    /// A person loaded an uploaded file.
    Upload,
}

impl Trigger {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Schedule => "schedule",
            Self::Manual => "manual",
            Self::Upload => "upload",
        }
    }
}

/// A request the host sends as is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HttpRequest {
    /// `GET` or `POST`.
    pub method: &'static str,
    /// The absolute URL.
    pub url: String,
    /// Every header, credentials included.
    pub headers: Vec<(String, String)>,
    /// The body, when there is one.
    pub body: Option<String>,
}

/// A finished run, as reported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunSummary {
    /// The run id.
    pub run: String,
    /// `ok`, `partial` (some rows quarantined) or `failed`.
    pub outcome: &'static str,
    /// Rows pulled.
    pub rows_read: u64,
    /// Rows in the cell after the run (new or unchanged).
    pub rows_landed: u64,
    /// Rows held back as quarantined evidence.
    pub rows_quarantined: u64,
    /// Why rows were held back, by code.
    pub quarantine_reasons: BTreeMap<String, u64>,
    /// Why the run failed.
    pub error: Option<(String, String)>,
}

/// The connector instance of the schemas the cell writes itself (its sync
/// reports), reserved: no configured connection uses it (SYNC-INSTANCE-1).
pub const CELL_INSTANCE: &str = "cell";

/// SYNC-LEASE-1: a run the host has not stepped for this long was abandoned
/// (its fetch loop died: an exception, an evicted instance), and a claim or
/// the schedule may take its connection over; a live run steps at least
/// once per fetch (bounded at 30 s) or page commit.
pub const RUN_LEASE_MS: u64 = 10 * 60 * 1000;

/// Whether `run` still holds its connection at `now_ms` (SYNC-LEASE-1).
fn held(run: &Run, now_ms: u64) -> bool {
    now_ms.saturating_sub(run.stepped_ms) < RUN_LEASE_MS
}

/// The host's next move.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Step {
    /// Send this and call [`Sources::step`](crate::sources::Sources::step) with the response.
    Request(HttpRequest),
    /// Rows are still being mapped: yield, then call `step` again with no
    /// response. SYNC-YIELD-1: a step maps one OBS-PAGE-1 page (at most
    /// [`PAGE_ROWS`](crate::host::PAGE_ROWS) rows of one dataset, within the
    /// cell's page byte bound), so a sync holds the cell for one page commit
    /// at a time, however much a response carried.
    Continue,
    /// The run is over.
    Done(RunSummary),
}

/// What the run does next.
enum Next {
    Request(HttpRequest),
    Continue,
    Done,
}

/// Why a sync could not be claimed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimError {
    /// No such connection.
    Unknown,
    /// A sync of it is already running.
    Running,
}

struct Run {
    id: String,
    trigger: Trigger,
    actor: String,
    started_ms: u64,
    /// When the host last stepped the run (SYNC-LEASE-1).
    stepped_ms: u64,
    pull: Option<Pull>,
    auth: Vec<(String, String)>,
    /// The first data request, held while an access token is fetched.
    held: Option<Request>,
    /// Sealed rows of the last page not yet mapped, and what follows them.
    queue: std::collections::VecDeque<(norn_kernel::Observed<RuntimeValue>, MappingInput)>,
    after: Option<HttpRequest>,
    watermark_from: String,
    read: u64,
    landed: u64,
    quarantined: u64,
    reasons: BTreeMap<String, u64>,
}

#[derive(Default)]
struct ConnectionState {
    run: Option<Run>,
    last_end_ms: Option<u64>,
    watermarks: Option<BTreeMap<String, Watermark>>,
}

/// Every configured connection of one cell and its running syncs.
pub struct Sources {
    connections: Vec<ConnectionConfig>,
    secrets: BTreeMap<String, String>,
    state: BTreeMap<String, ConnectionState>,
}

impl std::fmt::Debug for Sources {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Secrets are never formatted.
        f.debug_struct("Sources")
            .field("connections", &self.connections)
            .finish_non_exhaustive()
    }
}

impl Sources {
    /// The connections in `config` (a JSON array of [`ConnectionConfig`]),
    /// with `secrets` keyed by their environment names.
    ///
    /// # Errors
    /// Malformed configuration, a duplicate id, or a base URL SYNC-URL-1 refuses.
    pub fn new(config: &str, secrets: BTreeMap<String, String>) -> Result<Self, String> {
        let connections: Vec<ConnectionConfig> =
            serde_json::from_str(config).map_err(|e| format!("source configuration: {e}"))?;
        let mut ids = std::collections::BTreeSet::new();
        for connection in &connections {
            if !ids.insert(connection.id.as_str()) {
                return Err(format!("source configuration: {} twice", connection.id));
            }
            // SYNC-INSTANCE-1: `cell` names the schemas the cell writes itself
            // (its sync reports); no connected system may claim it.
            if connection.instance == CELL_INSTANCE {
                return Err(format!(
                    "source configuration: {} may not use the instance {CELL_INSTANCE}, which is the cell's own",
                    connection.id
                ));
            }
            let bound = u32::try_from(Limits::default().max_items).unwrap_or(u32::MAX);
            if connection
                .page_size
                .is_some_and(|size| size == 0 || size > bound)
            {
                return Err(format!(
                    "source configuration: {} pageSize is 1 to {bound}",
                    connection.id
                ));
            }
            if !allowed_base(&connection.base_url) {
                return Err(format!(
                    "source configuration: {} base URL must be https, or http to loopback or a private single-label host",
                    connection.id
                ));
            }
        }
        Ok(Self {
            connections,
            secrets,
            state: BTreeMap::new(),
        })
    }

    /// The configured connections.
    #[must_use]
    pub fn connections(&self) -> &[ConnectionConfig] {
        &self.connections
    }

    /// Whether `id` is running a sync now.
    #[must_use]
    pub fn running(&self, id: &str) -> bool {
        self.state.get(id).is_some_and(|s| s.run.is_some())
    }

    /// SYNC-ONE-1: claim a sync of `id` for `actor`, or refuse.
    ///
    /// # Errors
    /// [`ClaimError::Unknown`] or [`ClaimError::Running`].
    pub fn claim(
        &mut self,
        id: &str,
        trigger: Trigger,
        actor: &str,
        now_ms: u64,
    ) -> Result<String, ClaimError> {
        if !self.connections.iter().any(|c| c.id == id) {
            return Err(ClaimError::Unknown);
        }
        let state = self.state.entry(id.to_owned()).or_default();
        if state.run.as_ref().is_some_and(|run| held(run, now_ms)) {
            return Err(ClaimError::Running);
        }
        let run = format!("{id}/{now_ms}");
        state.run = Some(Run {
            id: run.clone(),
            trigger,
            actor: actor.to_owned(),
            started_ms: now_ms,
            stepped_ms: now_ms,
            pull: None,
            auth: Vec::new(),
            held: None,
            queue: std::collections::VecDeque::new(),
            after: None,
            watermark_from: String::new(),
            read: 0,
            landed: 0,
            quarantined: 0,
            reasons: BTreeMap::new(),
        });
        Ok(run)
    }

    /// SYNC-ONE-1, SYNC-ACTOR-1: claim the first connection whose schedule
    /// is due at `now_ms` and that is not running, as its own principal.
    pub fn due(&mut self, now_ms: u64) -> Option<String> {
        let id = self.connections.iter().find_map(|c| {
            let state = self.state.get(&c.id);
            if c.every_minutes == 0
                || state.is_some_and(|s| s.run.as_ref().is_some_and(|run| held(run, now_ms)))
            {
                return None;
            }
            let every = u64::from(c.every_minutes).saturating_mul(60_000);
            let last = state.and_then(|s| s.last_end_ms);
            last.is_none_or(|last| now_ms.saturating_sub(last) >= every)
                .then(|| c.id.clone())
        })?;
        self.claim(&id, Trigger::Schedule, &format!("connection:{id}"), now_ms)
            .ok()
            .map(|_| id)
    }

    /// Advance the claimed sync of `id`: `None` begins it; otherwise
    /// `response` is the status and body of the request last returned.
    pub fn step<E: SqlEngine>(
        &mut self,
        host: &DevHost<E>,
        id: &str,
        response: Option<(u16, &[u8])>,
        now_ms: u64,
    ) -> Step {
        let Some(config) = self.connections.iter().find(|c| c.id == id).cloned() else {
            return Step::Done(failed_summary("", "source.unknown", "no such connection"));
        };
        let watermarks_known = self.state.get(id).and_then(|s| s.watermarks.clone());
        let Some(run) = self.state.get_mut(id).and_then(|s| s.run.as_mut()) else {
            return Step::Done(failed_summary(
                "",
                "source.not_running",
                "no sync is running",
            ));
        };
        run.stepped_ms = now_ms;
        let outcome = match response {
            None if run.pull.is_some() => drain(host, run),
            None => begin(host, &config, &self.secrets, run, watermarks_known)
                .map(|first| first.map_or(Next::Done, Next::Request)),
            Some((status, body)) => advance(&config, run, status, body, now_ms).and_then(|after| {
                run.after = after;
                drain(host, run)
            }),
        };
        match outcome {
            Ok(Next::Request(request)) => Step::Request(request),
            Ok(Next::Continue) => Step::Continue,
            Ok(Next::Done) => self.finish(host, &config, None, now_ms),
            Err(error) => self.finish(host, &config, Some(error), now_ms),
        }
    }

    fn finish<E: SqlEngine>(
        &mut self,
        host: &DevHost<E>,
        config: &ConnectionConfig,
        error: Option<(String, String)>,
        now_ms: u64,
    ) -> Step {
        let state = self.state.entry(config.id.clone()).or_default();
        let Some(run) = state.run.take() else {
            return Step::Done(failed_summary(
                "",
                "source.not_running",
                "no sync is running",
            ));
        };
        state.last_end_ms = Some(now_ms);
        let watermarks = run
            .pull
            .as_ref()
            .map(|p| p.watermarks().clone())
            .or_else(|| state.watermarks.clone())
            .unwrap_or_default();
        state.watermarks = Some(watermarks.clone());
        let outcome = if error.is_some() {
            "failed"
        } else if run.quarantined > 0 {
            "partial"
        } else {
            "ok"
        };
        let summary = RunSummary {
            run: run.id.clone(),
            outcome,
            rows_read: run.read,
            rows_landed: run.landed,
            rows_quarantined: run.quarantined,
            quarantine_reasons: run.reasons.clone(),
            error,
        };
        let watermark = watermark_text(&watermarks);
        report(host, config, &run, &summary, &watermark, Some(now_ms));
        Step::Done(summary)
    }
}

type Failure = (String, String);

fn fail(code: &str, message: impl Into<String>) -> Failure {
    (code.to_owned(), message.into())
}

fn failed_summary(run: &str, code: &str, message: &str) -> RunSummary {
    RunSummary {
        run: run.to_owned(),
        outcome: "failed",
        rows_read: 0,
        rows_landed: 0,
        rows_quarantined: 0,
        quarantine_reasons: BTreeMap::new(),
        error: Some(fail(code, message)),
    }
}

fn begin<E: SqlEngine>(
    host: &DevHost<E>,
    config: &ConnectionConfig,
    secrets: &BTreeMap<String, String>,
    run: &mut Run,
    known: Option<BTreeMap<String, Watermark>>,
) -> Result<Option<HttpRequest>, Failure> {
    let connector = host
        .installed_artifact(&config.connector)
        .map_err(|refusal| fail("source.connector_missing", refusal.message))?
        .ok_or_else(|| {
            fail(
                "source.connector_missing",
                format!("{} is not installed", config.connector),
            )
        })?;
    let payload = connector.get("payload").cloned().unwrap_or_default();
    let mut profile = PullProfile::from_connector(&payload)
        .map_err(|e| fail("source.profile_invalid", e.to_string()))?
        .ok_or_else(|| {
            fail(
                "source.profile_missing",
                format!("{} declares no norn.pull/1 profile", config.connector),
            )
        })?;
    // SYNC-PAGE-1: the host's page size, when it sets one.
    if let Some(rows) = config.page_size {
        for feed in &mut profile.feeds {
            match &mut feed.paging {
                crate::pull::Paging::Keyset { size } | crate::pull::Paging::Cursor { size, .. } => {
                    *size = rows;
                }
            }
        }
    }
    let token = client_credentials(&payload, secrets, config)?;
    if token.is_none() {
        run.auth = auth_headers(&payload, secrets)?;
    }
    let stored = host
        .host_object(crate::sources::CONNECTION_TYPE, &config.id)
        .ok()
        .flatten()
        .and_then(|object| {
            object
                .get("watermark")
                .and_then(RuntimeValue::as_str)
                .map(str::to_owned)
        })
        .and_then(|text| serde_json::from_str::<BTreeMap<String, Watermark>>(&text).ok());
    let watermarks = stored.or(known).unwrap_or_default();
    run.watermark_from = watermark_text(&watermarks);
    let mut pull = Pull::new(profile, watermarks, Limits::default());
    let first = pull.next_request();
    run.pull = Some(pull);
    report(
        host,
        config,
        run,
        &RunSummary {
            run: run.id.clone(),
            outcome: "running",
            rows_read: 0,
            rows_landed: 0,
            rows_quarantined: 0,
            quarantine_reasons: BTreeMap::new(),
            error: None,
        },
        &run.watermark_from,
        None,
    );
    if let Some(token) = token {
        run.held = first;
        return Ok(Some(token));
    }
    Ok(first.map(|request| http(config, &run.auth, request)))
}

fn advance(
    config: &ConnectionConfig,
    run: &mut Run,
    status: u16,
    body: &[u8],
    now_ms: u64,
) -> Result<Option<HttpRequest>, Failure> {
    let pull = run
        .pull
        .as_mut()
        .ok_or_else(|| fail("source.not_running", "the sync has not begun"))?;
    if run.auth.is_empty() && pull.profile().token.is_some() {
        // The answer to the token request: send the held first request with it.
        let placement =
            pull.profile()
                .token
                .clone()
                .unwrap_or_else(|| crate::pull::TokenPlacement {
                    header: "Authorization".into(),
                    prefix: "Bearer ".into(),
                });
        let document: serde_json::Value = (status == 200)
            .then(|| serde_json::from_slice(body).ok())
            .flatten()
            .ok_or_else(|| {
                fail(
                    "source.token_refused",
                    format!("the token endpoint answered HTTP {status}"),
                )
            })?;
        let token = document
            .get("access_token")
            .and_then(serde_json::Value::as_str)
            .filter(|t| !t.is_empty())
            .ok_or_else(|| {
                fail(
                    "source.token_refused",
                    "the token endpoint returned no access token",
                )
            })?;
        run.auth = vec![(placement.header, format!("{}{token}", placement.prefix))];
        return Ok(run
            .held
            .take()
            .map(|request| http(config, &run.auth, request)));
    }
    let rows = pull.accept(status, body).map_err(|error| match &error {
        PullError::Status { .. } => fail("source.http_status", error.to_string()),
        PullError::Shape { .. } => fail("source.response_shape", error.to_string()),
        PullError::Bound { .. } => fail("source.bound", error.to_string()),
        PullError::Profile(_) | PullError::NotWaiting => {
            fail("source.profile_invalid", error.to_string())
        }
    })?;
    let recorded_at = norn_ir::provenance::UtcTimestamp(rfc3339(now_ms));
    let profile = pull.profile().clone();
    let batch = rows
        .iter()
        .map(|row| {
            observation(&profile, row, now_ms).map(|raw| {
                (
                    raw,
                    MappingInput {
                        mapping: config.mapping.clone(),
                        dataset: row.dataset.clone(),
                        connector_instance: config.instance.clone(),
                        partition: row.dataset.clone(),
                        recorded_at: recorded_at.clone(),
                    },
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| fail("source.seal", e))?;
    run.read = run
        .read
        .saturating_add(u64::try_from(rows.len()).unwrap_or(u64::MAX));
    run.queue.extend(batch);
    Ok(pull
        .next_request()
        .map(|request| http(config, &run.auth, request)))
}

/// SYNC-YIELD-1: map the next page of queued rows (one mapping input, at
/// most [`PAGE_ROWS`](crate::host::PAGE_ROWS), no source record twice);
/// continue while rows remain, then send what follows them.
fn drain<E: SqlEngine>(host: &DevHost<E>, run: &mut Run) -> Result<Next, Failure> {
    let mut seen = std::collections::BTreeSet::new();
    let first = run.queue.front().map(|(_, input)| input.clone());
    let take = run
        .queue
        .iter()
        .take(crate::host::PAGE_ROWS)
        .take_while(|(raw, input)| {
            Some(input) == first.as_ref() && seen.insert(raw.provenance.source_id.clone())
        })
        .count();
    let chunk: Vec<_> = run.queue.drain(..take).collect();
    if !chunk.is_empty() {
        let outcomes = host
            .ingest(&chunk)
            .map_err(|refusal| fail("source.ingest", refusal.message))?;
        for ingested in outcomes {
            match ingested.outcome {
                MappingOutcome::Landed | MappingOutcome::AlreadyLanded => {
                    run.landed = run.landed.saturating_add(1);
                }
                MappingOutcome::Quarantined(codes) => {
                    run.quarantined = run.quarantined.saturating_add(1);
                    for code in codes {
                        let n = run.reasons.entry(code).or_insert(0);
                        *n = n.saturating_add(1);
                    }
                }
            }
        }
    }
    if !run.queue.is_empty() {
        return Ok(Next::Continue);
    }
    Ok(run.after.take().map_or(Next::Done, Next::Request))
}

/// For an `oauth2` connector, the client-credentials token request (RFC 6749
/// §4.4) to the connector's relative `tokenUrl` on the connection's base URL,
/// with the client id and secret from the `clientSecretRef` secret
/// (`<id>:<secret>`).
fn client_credentials(
    payload: &serde_json::Value,
    secrets: &BTreeMap<String, String>,
    config: &ConnectionConfig,
) -> Result<Option<HttpRequest>, Failure> {
    let auth = payload.get("auth").cloned().unwrap_or_default();
    let field = |name: &str| auth.get(name).and_then(serde_json::Value::as_str);
    if field("kind") != Some("oauth2") {
        return Ok(None);
    }
    let path = field("tokenUrl")
        .filter(|p| p.starts_with('/') && !p.contains("://"))
        .ok_or_else(|| {
            fail(
                "source.auth_unsupported",
                "an oauth2 tokenUrl is a path on the connection",
            )
        })?;
    let name = secret_env(field("clientSecretRef").unwrap_or_default());
    let (id, secret) = secrets
        .get(&name)
        .and_then(|pair| pair.split_once(':'))
        .ok_or_else(|| {
            fail(
                "source.secret_missing",
                format!("the server has no {name} (<client id>:<client secret>)"),
            )
        })?;
    let form = |text: &str| -> String {
        let mut out = String::with_capacity(text.len());
        for b in text.bytes() {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
                out.push(char::from(b));
            } else {
                // Writing to a String cannot fail.
                let _written = write!(out, "%{b:02X}");
            }
        }
        out
    };
    Ok(Some(HttpRequest {
        method: "POST",
        url: format!("{}{path}", config.base_url.trim_end_matches('/')),
        headers: vec![(
            "Content-Type".into(),
            "application/x-www-form-urlencoded".into(),
        )],
        body: Some(format!(
            "grant_type=client_credentials&client_id={}&client_secret={}",
            form(id),
            form(secret)
        )),
    }))
}

/// The connector's declared header auth, resolved from server secrets.
fn auth_headers(
    payload: &serde_json::Value,
    secrets: &BTreeMap<String, String>,
) -> Result<Vec<(String, String)>, Failure> {
    let auth = payload.get("auth").cloned().unwrap_or_default();
    let field = |name: &str| auth.get(name).and_then(serde_json::Value::as_str);
    match field("kind") {
        Some("none") => Ok(Vec::new()),
        Some("api_key") if field("placement") == Some("header") => {
            let reference = field("secretRef").unwrap_or_default();
            let name = secret_env(reference);
            let secret = secrets
                .get(&name)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    fail("source.secret_missing", format!("the server has no {name}"))
                })?;
            let header = field("name").unwrap_or("Authorization");
            Ok(vec![(
                header.to_owned(),
                format!("{}{secret}", field("prefix").unwrap_or_default()),
            )])
        }
        _ => Err(fail(
            "source.auth_unsupported",
            "this connector's auth is not supported by the pull host",
        )),
    }
}

/// The environment name of a secret reference: `erpnext_sandbox_token` is
/// `NORN_SECRET_ERPNEXT_SANDBOX_TOKEN`.
#[must_use]
pub fn secret_env(reference: &str) -> String {
    let mut name = String::from("NORN_SECRET_");
    name.extend(reference.chars().map(|c| {
        if c.is_ascii_alphanumeric() {
            c.to_ascii_uppercase()
        } else {
            '_'
        }
    }));
    name
}

fn http(config: &ConnectionConfig, auth: &[(String, String)], request: Request) -> HttpRequest {
    let mut headers = request.headers;
    headers.extend(config.headers.iter().map(|(k, v)| (k.clone(), v.clone())));
    headers.extend(auth.iter().cloned());
    HttpRequest {
        method: request.method,
        url: format!("{}{}", config.base_url.trim_end_matches('/'), request.path),
        headers,
        body: request.body,
    }
}

fn allowed_base(url: &str) -> bool {
    let (scheme, rest) = url.split_once("://").unwrap_or(("", ""));
    let authority = rest.split('/').next().unwrap_or_default();
    if authority.is_empty() || authority.contains('@') || rest.contains('?') {
        return false;
    }
    let host = authority.rsplit_once(':').map_or(authority, |(h, _)| h);
    match scheme {
        "https" => true,
        "http" => host == "127.0.0.1" || host == "localhost" || !host.contains('.'),
        _ => false,
    }
}

fn watermark_text(watermarks: &BTreeMap<String, Watermark>) -> String {
    serde_json::to_string(watermarks).unwrap_or_else(|_| "{}".to_owned())
}

/// RFC 3339 UTC with milliseconds.
#[must_use]
pub fn rfc3339(ms: u64) -> String {
    i64::try_from(ms)
        .ok()
        .and_then(chrono::DateTime::from_timestamp_millis)
        .map_or_else(
            || "1970-01-01T00:00:00.000Z".to_owned(),
            |t| t.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        )
}

/// One upload's load, as reported (SYNC-REPORT-1).
#[derive(Debug, Clone)]
pub struct UploadRun<'a> {
    /// The connection id, e.g. `upload.sales`.
    pub id: &'a str,
    /// The display name (the file name).
    pub label: &'a str,
    /// The object's URI.
    pub uri: &'a str,
    /// The object's SHA-256.
    pub sha256: &'a str,
    /// The generated mapping.
    pub mapping: &'a str,
    /// Who loaded it.
    pub actor: &'a str,
    /// When the load started and ended.
    pub started_ms: u64,
    /// When it ended.
    pub ended_ms: u64,
    /// Rows read, landed and quarantined, and why.
    pub counts: (u64, u64, u64),
    /// Quarantine reasons by code.
    pub reasons: BTreeMap<String, u64>,
}

/// Report an upload's load as a connection of system `upload` and its run.
pub fn report_upload<E: SqlEngine>(host: &DevHost<E>, upload: &UploadRun<'_>) {
    let config = ConnectionConfig {
        id: upload.id.to_owned(),
        system: "upload".to_owned(),
        label: upload.label.to_owned(),
        connector: String::new(),
        mapping: upload.mapping.to_owned(),
        instance: "s3".to_owned(),
        base_url: upload.uri.to_owned(),
        headers: BTreeMap::new(),
        every_minutes: 0,
        page_size: None,
    };
    let (read, landed, quarantined) = upload.counts;
    let run = Run {
        id: format!("{}/{}", upload.id, upload.started_ms),
        trigger: Trigger::Upload,
        actor: upload.actor.to_owned(),
        started_ms: upload.started_ms,
        stepped_ms: upload.ended_ms,
        pull: None,
        auth: Vec::new(),
        held: None,
        queue: std::collections::VecDeque::new(),
        after: None,
        watermark_from: "{}".to_owned(),
        read,
        landed,
        quarantined,
        reasons: upload.reasons.clone(),
    };
    let summary = RunSummary {
        run: run.id.clone(),
        outcome: if quarantined > 0 { "partial" } else { "ok" },
        rows_read: read,
        rows_landed: landed,
        rows_quarantined: quarantined,
        quarantine_reasons: upload.reasons.clone(),
        error: None,
    };
    let watermark = serde_json::json!({"sha256": upload.sha256, "rows": read}).to_string();
    report(
        host,
        &config,
        &run,
        &summary,
        &watermark,
        Some(upload.ended_ms),
    );
}

/// SYNC-REPORT-1: the run, as the cell's own observations. A cell without
/// the report mapping keeps its in-memory state only.
fn report<E: SqlEngine>(
    host: &DevHost<E>,
    config: &ConnectionConfig,
    run: &Run,
    summary: &RunSummary,
    watermark: &str,
    ended_ms: Option<u64>,
) {
    let started = rfc3339(run.started_ms);
    let ended = ended_ms.map(rfc3339);
    let reasons = summary
        .quarantine_reasons
        .iter()
        .map(|(code, n)| format!("{code}={n}"))
        .collect::<Vec<_>>()
        .join("; ");
    let (error_code, error_message) = summary
        .error
        .clone()
        .map_or((None, None), |(c, m)| (Some(c), Some(m)));
    let status = if ended.is_none() {
        "syncing"
    } else {
        summary.outcome
    };
    let auth = if config.connector.is_empty() {
        "presigned_upload"
    } else {
        "api_key"
    };
    let connection = norn_ir::runtime_object! {
        "id" => config.id.as_str(), "system" => config.system.as_str(), "label" => config.label.as_str(),
        "target" => config.base_url.as_str(), "auth" => auth, "status" => status,
        "last_sync_started_at" => started.as_str(), "last_sync_ended_at" => ended,
        "last_trigger" => run.trigger.as_str(), "watermark" => watermark,
        "rows_read" => summary.rows_read, "rows_landed" => summary.rows_landed,
        "rows_quarantined" => summary.rows_quarantined, "quarantine_reasons" => reasons.as_str(),
        "last_error_code" => error_code, "last_error_message" => error_message,
        "connector_id" => config.connector.as_str(), "source_schema_id" => format!("{}.source", config.mapping.trim_end_matches(".mapping")),
        "mapping_id" => config.mapping.as_str(),
    };
    let phase = if ended.is_some() { "end" } else { "start" };
    let mut rows = vec![("connection", config.id.clone(), connection)];
    if ended.is_some() {
        rows.push((
            "sync_run",
            run.id.clone(),
            norn_ir::runtime_object! {
                "id" => run.id.as_str(), "connection_id" => config.id.as_str(), "trigger" => run.trigger.as_str(),
                "run_by" => run.actor.as_str(), "started_at" => started.as_str(), "ended_at" => ended,
                "outcome" => summary.outcome, "watermark_from" => run.watermark_from.as_str(), "watermark_to" => watermark,
                "rows_read" => summary.rows_read, "rows_landed" => summary.rows_landed,
                "rows_quarantined" => summary.rows_quarantined, "quarantine_reasons" => reasons.as_str(),
                "error_code" => error_code, "error_message" => error_message,
            },
        ));
    }
    let profile = PullProfile {
        source: REPORT_SOURCE.to_owned(),
        pipeline: "norn-connect/1".to_owned(),
        feeds: Vec::new(),
        token: None,
    };
    let now = ended_ms.unwrap_or(run.started_ms);
    let batch: Vec<_> = rows
        .into_iter()
        .filter_map(|(dataset, source_id, payload)| {
            let row = crate::pull::Row {
                dataset: dataset.to_owned(),
                source_id,
                version: format!("{}/{phase}", run.id),
                payload,
            };
            observation(&profile, &row, now).ok().map(|raw| {
                (
                    raw,
                    MappingInput {
                        mapping: REPORT_MAPPING.to_owned(),
                        dataset: dataset.to_owned(),
                        connector_instance: CELL_INSTANCE.to_owned(),
                        partition: dataset.to_owned(),
                        recorded_at: norn_ir::provenance::UtcTimestamp(rfc3339(now)),
                    },
                )
            })
        })
        .collect();
    // A cell without the report definitions refuses these; the run still ends.
    let _reported = host.ingest(&batch);
}

/// The object type of a completed upload's durable record.
pub const UPLOAD_TYPE: &str = "source.Upload";

/// UPLOAD-RECORD-1: a completed upload as the cell keeps it, written through
/// its own `norn-connect` mapping when `:complete` reads the object back, so
/// it outlives the host. Every value is what the cell read or decided,
/// never what a client claimed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeptUpload {
    /// The sealed upload id.
    pub upload_id: String,
    /// The store the object is in.
    pub store: crate::uploads::Store,
    /// The object's URI.
    pub uri: String,
    /// The SHA-256 of the bytes the cell read.
    pub sha256: String,
    /// Their size.
    pub size: u64,
    /// The content type the upload began with.
    pub content_type: String,
    /// The file name the upload began with.
    pub file_name: String,
    /// Whether the bytes are CSV the cell profiled.
    pub csv: bool,
    /// The profile's columns.
    pub columns: Vec<crate::uploads::Column>,
    /// Who completed it.
    pub owner: String,
    /// When, as RFC 3339.
    pub completed_at: String,
}

/// The record key of the upload `upload_id`: its SHA-256.
#[must_use]
pub fn kept_upload_key(upload_id: &str) -> String {
    crate::uploads::sha256_hex(upload_id.as_bytes())
}

/// Keep `upload` in the cell (UPLOAD-RECORD-1). The same bytes kept again
/// are already kept; new bytes for the same upload are its next version.
///
/// # Errors
/// The cell has no `source.Upload` definitions, or refused the record.
pub fn keep_upload<E: SqlEngine>(
    host: &DevHost<E>,
    upload: &KeptUpload,
    now_ms: u64,
) -> Result<(), String> {
    let key = kept_upload_key(&upload.upload_id);
    let columns = serde_json::to_string(&upload.columns).map_err(|e| e.to_string())?;
    let store = match upload.store {
        crate::uploads::Store::Uploads => "uploads",
        crate::uploads::Store::Documents => "documents",
    };
    let payload = norn_ir::runtime_object! {
        "id" => key.as_str(), "upload_id" => upload.upload_id.as_str(), "store" => store,
        "uri" => upload.uri.as_str(), "sha256" => upload.sha256.as_str(), "size" => upload.size,
        "content_type" => upload.content_type.as_str(), "file_name" => upload.file_name.as_str(),
        "format" => if upload.csv { "csv" } else { "other" }, "columns" => columns.as_str(),
        "owner" => upload.owner.as_str(), "completed_at" => upload.completed_at.as_str(),
    };
    let profile = PullProfile {
        source: REPORT_SOURCE.to_owned(),
        pipeline: "norn-connect/1".to_owned(),
        feeds: Vec::new(),
        token: None,
    };
    let row = crate::pull::Row {
        dataset: "upload".to_owned(),
        source_id: key,
        version: upload.sha256.clone(),
        payload,
    };
    let raw = observation(&profile, &row, now_ms)?;
    let input = MappingInput {
        mapping: REPORT_MAPPING.to_owned(),
        dataset: "upload".to_owned(),
        connector_instance: CELL_INSTANCE.to_owned(),
        partition: "upload".to_owned(),
        recorded_at: norn_ir::provenance::UtcTimestamp(rfc3339(now_ms)),
    };
    let ingested = host
        .ingest(&[(raw, input)])
        .map_err(|refusal| refusal.message)?;
    match ingested.first().map(|i| &i.outcome) {
        Some(MappingOutcome::Landed | MappingOutcome::AlreadyLanded) => Ok(()),
        Some(MappingOutcome::Quarantined(reasons)) => Err(format!(
            "the cell refused the upload record: {}",
            reasons.join(", ")
        )),
        None => Err("the cell captured no upload record".to_owned()),
    }
}

/// The kept record of the upload `upload_id`, if the cell completed it.
///
/// # Errors
/// A store failure, or a kept record the cell cannot read back.
pub fn kept_upload<E: SqlEngine>(
    host: &DevHost<E>,
    upload_id: &str,
) -> Result<Option<KeptUpload>, String> {
    let Some(object) = host
        .host_object(UPLOAD_TYPE, &kept_upload_key(upload_id))
        .map_err(|refusal| refusal.message)?
    else {
        return Ok(None);
    };
    let text = |name: &str| {
        object
            .get(name)
            .and_then(RuntimeValue::as_str)
            .map(str::to_owned)
            .ok_or_else(|| format!("a kept upload has no {name}"))
    };
    if text("upload_id")? != upload_id {
        return Ok(None);
    }
    let store = match text("store")?.as_str() {
        "documents" => crate::uploads::Store::Documents,
        _ => crate::uploads::Store::Uploads,
    };
    Ok(Some(KeptUpload {
        upload_id: upload_id.to_owned(),
        store,
        uri: text("uri")?,
        sha256: text("sha256")?,
        size: object
            .get("size")
            .and_then(RuntimeValue::as_i64)
            .and_then(|n| u64::try_from(n).ok())
            .ok_or("a kept upload has no size")?,
        content_type: text("content_type")?,
        file_name: text("file_name")?,
        csv: text("format")? == "csv",
        columns: serde_json::from_str(&text("columns")?).map_err(|e| e.to_string())?,
        owner: text("owner")?,
        completed_at: text("completed_at").unwrap_or_default(),
    }))
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "token request scenarios")]
mod tests {
    use super::*;

    fn config() -> ConnectionConfig {
        serde_json::from_value(serde_json::json!({
            "id": "shop.dev", "system": "shopify", "label": "Dev store", "connector": "shopify.dev.connector",
            "mapping": "shopify.dev.mapping", "instance": "dev", "baseUrl": "https://dev-store.example"
        }))
        .expect("config")
    }

    #[test]
    fn client_credentials_post_the_form_to_the_connection_and_need_their_secret() {
        let payload = serde_json::json!({"auth": {"kind": "oauth2", "authId": "shop", "authorizationUrl": "/admin/oauth/authorize",
            "tokenUrl": "/admin/oauth/access_token", "scopes": ["read_products"], "clientSecretRef": "shopify_dev_client"}});
        let secrets = BTreeMap::from([(
            "NORN_SECRET_SHOPIFY_DEV_CLIENT".to_owned(),
            "id 1:s&ecret".to_owned(),
        )]);
        let request = client_credentials(&payload, &secrets, &config())
            .expect("planned")
            .expect("a token request");
        assert_eq!(
            request.url,
            "https://dev-store.example/admin/oauth/access_token"
        );
        assert_eq!(
            request.body.as_deref(),
            Some("grant_type=client_credentials&client_id=id%201&client_secret=s%26ecret")
        );
        assert!(client_credentials(&payload, &BTreeMap::new(), &config()).is_err());
        let absolute = serde_json::json!({"auth": {"kind": "oauth2", "tokenUrl": "https://elsewhere.example/token", "clientSecretRef": "shopify_dev_client"}});
        assert!(client_credentials(&absolute, &secrets, &config()).is_err());
        assert_eq!(
            client_credentials(
                &serde_json::json!({"auth": {"kind": "none"}}),
                &secrets,
                &config()
            ),
            Ok(None)
        );
    }

    #[test]
    fn base_urls_are_https_or_private_http() {
        assert!(allowed_base("https://erp.example"));
        assert!(allowed_base("http://erp-sandbox:8000"));
        assert!(allowed_base("http://127.0.0.1:8069"));
        assert!(!allowed_base("http://erp.example"));
        assert!(!allowed_base("https://user@erp.example"));
        assert!(!allowed_base("ftp://erp"));
    }
}
