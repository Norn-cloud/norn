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
//! SYNC-URL-1: a base URL is https to a public host, read as the host's
//! `fetch` reads it (WHATWG): no credentials, query or fragment, and no
//! internal destination, that is an IP literal in a loopback, private,
//! link-local, unique-local, shared, multicast, IPv4-mapped or unspecified
//! range (in any of its spellings: `0x7f.1`, `2130706433`, `[::ffff:…]`),
//! `localhost`, or a single-label name. Plain http, and any internal host,
//! only to a host the operator lists in [`ALLOWED_HOSTS_VAR`](crate::sources::ALLOWED_HOSTS_VAR)
//! (comma-separated, empty by default). The check is of the URL, not of what
//! a name resolves to.
//! SOURCE-SCOPE-1: a connection, its endpoint and the secrets it may resolve
//! are bound to the cell scope it names: a cell keeps only its own
//! connections and the secrets they bind, and a connector resolves no other.

mod auth;
mod capture;

use std::collections::BTreeMap;

use norn_cell::MappingOutcome;
use norn_cell::engine::SqlEngine;
use norn_cell::mapping_bundle::MappingInput;
use norn_cell::source_cursor::SourceLease;
use norn_ir::RuntimeValue;
use serde::{Deserialize, Serialize};

use crate::host::DevHost;
use crate::pull::{Limits, Pull, PullError, PullProfile, Request, Watermark, observation};
use crate::scope::CellScope;
pub use auth::secret_env;
use auth::{auth_headers, client_credentials};

/// The server variable listing the hosts a base URL may name although they
/// are internal or plain http (SYNC-URL-1): comma-separated host names or IP
/// literals (IPv6 in brackets), for self-hosters whose sources run on their
/// own network. Empty by default.
pub const ALLOWED_HOSTS_VAR: &str = "NORN_SOURCE_ALLOWED_HOSTS";

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
    /// The connection id, e.g. `erpnext.rest`.
    pub id: String,
    /// SOURCE-SCOPE-1: the organization whose cell owns the connection.
    pub org_id: String,
    /// SOURCE-SCOPE-1: the environment whose cell owns the connection.
    pub environment_id: String,
    /// SOURCE-SCOPE-1: the secret references (a connector's `secretRef` or
    /// `clientSecretRef`) the connection may resolve; any other is refused.
    /// Required: a connection without credentials lists none (`[]`).
    pub secret_refs: Vec<String>,
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimError {
    /// No such connection.
    Unknown,
    /// A sync of it is already running.
    Running,
    /// SYNC-SERIAL-1: another connection of the cell is syncing (its id).
    Busy(String),
}

struct Run {
    id: String,
    trigger: Trigger,
    actor: String,
    started_ms: u64,
    /// When the host last stepped the run (SYNC-LEASE-1).
    stepped_ms: u64,
    /// SYNC-YIELD-2: the most rows the next step maps, as the host paces it.
    step_rows: usize,
    /// Rows the current step mapped.
    mapped: usize,
    pull: Option<Pull>,
    drain_only: bool,
    mapping_binding: Option<String>,
    auth: Vec<(String, String)>,
    /// The first data request, held while an access token is fetched.
    held: Option<Request>,
    /// Durably captured pages awaiting paced post-capture canonical processing
    /// (recovered `_norn_source_pending` entries first, then the latest page).
    pending_captures: std::collections::VecDeque<capture::ActiveCapture>,
    after: Option<HttpRequest>,
    /// Active durable fencing lease acquired from `_norn_source_lease`.
    lease: Option<SourceLease>,
    watermark_from: String,
    /// Last page's watermarks whose rows were fully drained/ingested.
    committed_watermarks: BTreeMap<String, Watermark>,
    /// Exact stored `_norn_source_cursor.cursor_json` bytes per dataset for CAS.
    committed_cursor_json: BTreeMap<String, String>,
    read: u64,
    landed: u64,
    quarantined: u64,
    reasons: BTreeMap<String, u64>,
}

#[derive(Default)]
struct ConnectionState {
    run: Option<Run>,
    /// Rows the last step mapped, for the host's pacing (SYNC-YIELD-2).
    mapped: usize,
    last_end_ms: Option<u64>,
    watermarks: Option<BTreeMap<String, Watermark>>,
    /// The schedule was read back from the cell's durable sync record
    /// (TIMER-ALARM-1), so an evicted host never resyncs early.
    restored: bool,
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
    /// The connections in `config` (a JSON array of [`ConnectionConfig`])
    /// that the cell serving `scope` owns, with the `secrets` (keyed by their
    /// environment names) they bind (SOURCE-SCOPE-1).
    ///
    /// # Errors
    /// Malformed configuration, a duplicate id, a base URL SYNC-URL-1
    /// refuses, or a secret bound in two scopes.
    pub fn new(
        scope: &CellScope,
        config: &str,
        secrets: &BTreeMap<String, String>,
    ) -> Result<Self, String> {
        Self::with_allowed_hosts(scope, config, secrets, "")
    }

    /// [`Sources::new`], with `allowed_hosts` (the [`ALLOWED_HOSTS_VAR`]
    /// value) naming the internal or plain-http hosts a base URL may use.
    ///
    /// # Errors
    /// What [`Sources::new`] refuses, or an `allowed_hosts` entry that is not
    /// a bare host.
    pub fn with_allowed_hosts(
        scope: &CellScope,
        config: &str,
        secrets: &BTreeMap<String, String>,
        allowed_hosts: &str,
    ) -> Result<Self, String> {
        let allowed_hosts = allowed_host_list(allowed_hosts)?;
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
            if !allowed_base(&connection.base_url, &allowed_hosts) {
                return Err(format!(
                    "source configuration: {} base URL must be https to a public host; plain http or an internal host only when {ALLOWED_HOSTS_VAR} lists it",
                    connection.id
                ));
            }
        }
        // SOURCE-SCOPE-1: every connection was checked; the cell keeps its own.
        let (connections, secrets) = auth::bind(scope, connections, secrets)?;
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
    /// [`ClaimError::Unknown`], [`ClaimError::Running`], or
    /// [`ClaimError::Busy`] while another connection of the cell syncs.
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
        if let Some(other) = self.syncing(now_ms).filter(|other| other != id) {
            return Err(ClaimError::Busy(other));
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
            step_rows: crate::host::PAGE_ROWS,
            mapped: 0,
            pull: None,
            drain_only: false,
            mapping_binding: None,
            auth: Vec::new(),
            held: None,
            pending_captures: std::collections::VecDeque::new(),
            after: None,
            lease: None,
            watermark_from: String::new(),
            committed_watermarks: BTreeMap::new(),
            committed_cursor_json: BTreeMap::new(),
            read: 0,
            landed: 0,
            quarantined: 0,
            reasons: BTreeMap::new(),
        });
        Ok(run)
    }

    /// SYNC-YIELD-2: map at most `rows` (1 to a page) in each step of the run
    /// of `id` from now on. The runtime's clock stands still inside a step,
    /// so the host paces from outside: it times each step against the rows it
    /// mapped ([`Self::mapped`]) and keeps steps near a time budget.
    pub fn pace(&mut self, id: &str, rows: usize) {
        if let Some(run) = self.state.get_mut(id).and_then(|s| s.run.as_mut()) {
            run.step_rows = rows.clamp(1, crate::host::PAGE_ROWS);
        }
    }

    /// SYNC-SERIAL-1: the connection whose sync holds the cell at `now_ms`,
    /// if any. One sync runs per cell at a time, so syncs never interleave
    /// their steps in one host instance.
    fn syncing(&self, now_ms: u64) -> Option<String> {
        self.state
            .iter()
            .find(|(_, s)| s.run.as_ref().is_some_and(|run| held(run, now_ms)))
            .map(|(id, _)| id.clone())
    }

    /// Rows the last step of `id` mapped (SYNC-YIELD-2).
    #[must_use]
    pub fn mapped(&self, id: &str) -> usize {
        self.state.get(id).map_or(0, |s| s.mapped)
    }

    /// SYNC-ONE-1, SYNC-ACTOR-1: claim the first connection whose schedule
    /// is due at `now_ms` and that is not running, as its own principal.
    pub fn due(&mut self, now_ms: u64) -> Option<String> {
        if self.syncing(now_ms).is_some() {
            return None;
        }
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

    /// TIMER-ALARM-1: read each scheduled connection's last sync end back
    /// from the cell's durable connection record (SYNC-REPORT-1), once per
    /// host instance, so a host that was evicted keeps the schedule the
    /// cell recorded instead of syncing every connection at once. A
    /// connection the cell holds no ended sync for stays never synced.
    ///
    /// # Errors
    /// The cell could not be read: nothing is restored, and the host claims
    /// nothing on the schedule until it can be.
    pub fn restore_schedule<E: SqlEngine>(&mut self, host: &DevHost<E>) -> Result<(), String> {
        let pending = |state: Option<&ConnectionState>| !state.is_some_and(|s| s.restored);
        if !self
            .connections
            .iter()
            .any(|c| c.every_minutes > 0 && pending(self.state.get(&c.id)))
        {
            return Ok(());
        }
        // A cell from before connection records has no such type: no
        // recorded schedule, not a restore error.
        let recorded = host
            .object_type_names()
            .map_err(|refusal| format!("{}: {}", refusal.code, refusal.message))?
            .iter()
            .any(|name| name == CONNECTION_TYPE);
        for connection in &self.connections {
            let state = self.state.entry(connection.id.clone()).or_default();
            // Only a scheduled connection has a schedule to restore.
            if state.restored || connection.every_minutes == 0 || !recorded {
                state.restored = true;
                continue;
            }
            let ended = host
                .host_object(CONNECTION_TYPE, &connection.id)
                .map_err(|refusal| format!("{}: {}", refusal.code, refusal.message))?
                .and_then(|record| {
                    record
                        .get("last_sync_ended_at")
                        .and_then(RuntimeValue::as_str)
                        .and_then(|text| text.parse::<jiff::Timestamp>().ok())
                        .and_then(|at| u64::try_from(at.as_millisecond()).ok())
                });
            if let Some(ended) = ended {
                state.last_end_ms = Some(state.last_end_ms.map_or(ended, |known| known.max(ended)));
            }
            state.restored = true;
        }
        Ok(())
    }

    /// TIMER-ALARM-1: when the next scheduled sync falls due, at or after
    /// `now_ms` (the host wakes then and claims it with [`Self::due`]): a
    /// connection never synced is due now, and any other one `every_minutes`
    /// after its last run ended. While any connection's sync holds the cell
    /// (SYNC-SERIAL-1), no other can be claimed, so the next is when that
    /// run's lease lapses (SYNC-LEASE-1); its end re-arms the host. `None`
    /// when no connection is scheduled.
    #[must_use]
    pub fn next_due(&self, now_ms: u64) -> Option<u64> {
        let lease = self
            .state
            .values()
            .filter_map(|s| s.run.as_ref())
            .filter(|run| held(run, now_ms))
            .map(|run| run.stepped_ms.saturating_add(RUN_LEASE_MS))
            .min();
        if lease.is_some() && self.connections.iter().any(|c| c.every_minutes > 0) {
            return lease;
        }
        self.connections
            .iter()
            .filter(|c| c.every_minutes > 0)
            .map(|c| {
                let every = u64::from(c.every_minutes).saturating_mul(60_000);
                self.state
                    .get(&c.id)
                    .and_then(|s| s.last_end_ms)
                    .map_or(now_ms, |last| last.saturating_add(every))
                    .max(now_ms)
            })
            .min()
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
        if !auth::owns(host.scope(), &config) {
            // SOURCE-SCOPE-1: never stepped, nor reported, in another cell.
            if let Some(state) = self.state.get_mut(id) {
                state.run = None;
            }
            let message = "the connection belongs to another cell";
            return Step::Done(failed_summary("", "source.scope_mismatch", message));
        }
        let watermarks_known = self.state.get(id).and_then(|s| s.watermarks.clone());
        let Some(run) = self.state.get_mut(id).and_then(|s| s.run.as_mut()) else {
            return Step::Done(failed_summary(
                "",
                "source.not_running",
                "no sync is running",
            ));
        };
        run.stepped_ms = now_ms;
        run.mapped = 0;
        let outcome = match response {
            None if run.pull.is_some() || run.drain_only => drain(host, run, now_ms),
            None => begin(host, &config, &self.secrets, run, watermarks_known, now_ms),
            Some((status, body)) => {
                advance(host, &config, run, status, body, now_ms).and_then(|after| {
                    run.after = after;
                    drain(host, run, now_ms)
                })
            }
        };
        let mapped = run.mapped;
        if let Some(state) = self.state.get_mut(id) {
            state.mapped = mapped;
        }
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
        let superseded = capture::is_lease_superseded(host, config, &run, error.as_ref(), now_ms);
        let outcome = if error.is_some() {
            "failed"
        } else if run.quarantined > 0 {
            "partial"
        } else {
            "ok"
        };
        let mut summary = RunSummary {
            run: run.id.clone(),
            outcome,
            rows_read: run.read,
            rows_landed: run.landed,
            rows_quarantined: run.quarantined,
            quarantine_reasons: run.reasons.clone(),
            error,
        };
        if superseded {
            capture::mark_fenced(
                &mut summary,
                fail(
                    "source.lease_fenced",
                    "sync lease or mapping revision was superseded",
                ),
            );
        } else {
            let watermarks = if run.pull.is_some() || !run.committed_watermarks.is_empty() {
                run.committed_watermarks.clone()
            } else {
                state.watermarks.clone().unwrap_or_default()
            };
            let watermark = watermark_text(&watermarks);
            // SOURCE-CAPTURE-1: report while holding the lease, then update
            // cached state only if the report was not fenced.
            if let Some(fenced) = report(host, config, &run, &summary, &watermark, Some(now_ms)) {
                capture::mark_fenced(&mut summary, fenced);
            } else {
                if !run.drain_only {
                    state.last_end_ms = Some(now_ms);
                }
                state.watermarks = Some(watermarks);
            }
        }
        if let Some(lease) = &run.lease {
            let _released =
                host.release_source_lease(&config.id, &lease.lease_id, lease.generation);
        }
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
    now_ms: u64,
) -> Result<Next, Failure> {
    let (profile, payload, expected_binding) = capture::load_connector_profile(host, config)?;
    run.mapping_binding = Some(expected_binding.clone());
    let token = client_credentials(&payload, secrets, config)?;
    if token.is_none() {
        run.auth = auth_headers(&payload, secrets, config)?;
    }
    let lease = host
        .acquire_source_lease(&config.id, &run.id, now_ms, RUN_LEASE_MS)
        .map_err(|error| {
            let (code, message) = error.code_and_message();
            fail(code, message)
        })?;
    run.lease = Some(lease.clone());
    let pending = capture::load_pending_captures(host, &config.id)?;
    let (watermarks, raw_cursors, drain_only) = capture::load_and_seed_watermarks(
        host,
        config,
        &lease,
        &expected_binding,
        known,
        !pending.is_empty(),
        now_ms,
    )?;
    run.drain_only = drain_only;
    if drain_only {
        run.mapping_binding = None;
    }
    run.watermark_from = watermark_text(&watermarks);
    run.committed_watermarks = watermarks.clone();
    run.committed_cursor_json = raw_cursors;
    run.pending_captures.extend(pending);
    let first = (!drain_only)
        .then(|| {
            let mut pull = Pull::new(profile, watermarks, Limits::default());
            let first = pull.next_request();
            run.pull = Some(pull);
            first
        })
        .flatten();
    if let Some(fenced) = report(
        host,
        config,
        run,
        &capture::running_summary(&run.id),
        &run.watermark_from,
        None,
    ) {
        return Err(fenced);
    }
    let next_req = if drain_only {
        None
    } else if let Some(token) = token {
        run.held = first;
        Some(token)
    } else {
        first.map(|request| http(config, &run.auth, request))
    };
    if !run.pending_captures.is_empty() {
        run.after = next_req;
        return drain(host, run, now_ms);
    }
    Ok(next_req.map_or(Next::Done, Next::Request))
}

fn advance<E: SqlEngine>(
    host: &DevHost<E>,
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
    let accepted = pull.accept(status, body).map_err(|error| match &error {
        PullError::Status { .. } => fail("source.http_status", error.to_string()),
        PullError::Shape { .. } => fail("source.response_shape", error.to_string()),
        PullError::Bound { .. } => fail("source.bound", error.to_string()),
        PullError::Profile(_) | PullError::NotWaiting => {
            fail("source.profile_invalid", error.to_string())
        }
    })?;
    let rows_len = accepted.rows.len();
    let profile = pull.profile().clone();
    let lease = run
        .lease
        .as_ref()
        .ok_or_else(|| fail("source.lease_fenced", "no active durable sync lease"))?;
    let (captured, advance) = capture::capture_accepted_page(
        host,
        config,
        &profile,
        accepted,
        &mut capture::CaptureTurn {
            lease,
            mapping_binding: run.mapping_binding.as_deref(),
            committed_cursor_json: &mut run.committed_cursor_json,
            step_rows: run.step_rows,
            now_ms,
        },
    )?;
    run.read = run
        .read
        .saturating_add(u64::try_from(rows_len).unwrap_or(u64::MAX));
    if let Some(active) = captured {
        run.pending_captures.push_back(active);
    } else if let Some(pull) = run.pull.as_mut() {
        // Empty terminal page committed its cursor atomically with zero rows to drain.
        pull.confirm(&advance);
        run.committed_watermarks = pull.watermarks().clone();
    }
    let next = run
        .pull
        .as_mut()
        .and_then(Pull::next_request)
        .map(|request| http(config, &run.auth, request));
    Ok(next)
}

/// SYNC-YIELD-1 + SOURCE-CAPTURE-1: process up to `run.step_rows` rows of the
/// next already-captured page (recovered or freshly captured), confirming the
/// ephemeral `Pull` cursor and deleting `_norn_source_pending` once all rows of
/// that page have been processed.
fn drain<E: SqlEngine>(host: &DevHost<E>, run: &mut Run, now_ms: u64) -> Result<Next, Failure> {
    let done_front = match run.pending_captures.front_mut() {
        Some(capture::ActiveCapture::Captured {
            pending, outcomes, ..
        }) => {
            let step = host
                .process_source_page_step(pending, outcomes, run.step_rows)
                .map_err(|refusal| fail("source.ingest", refusal.message))?;
            run.mapped = step.len();
            for ingested in step {
                crate::pull::tally_outcome(
                    &ingested.outcome,
                    &mut run.landed,
                    &mut run.quarantined,
                    &mut run.reasons,
                );
            }
            pending.pages.is_empty() && pending.records.is_empty()
        }
        Some(capture::ActiveCapture::DisputeFallback { state, .. }) => {
            let step = host
                .step_source_dispute_fallback(state, now_ms, run.step_rows)
                .map_err(|error| {
                    let (code, message) = error.code_and_message();
                    fail(code, message)
                })?;
            run.mapped = step.len();
            for ingested in step {
                crate::pull::tally_outcome(
                    &ingested.outcome,
                    &mut run.landed,
                    &mut run.quarantined,
                    &mut run.reasons,
                );
            }
            state.is_complete()
        }
        None => false,
    };
    if done_front && let Some(finished) = run.pending_captures.pop_front() {
        let advance = match finished {
            capture::ActiveCapture::Captured {
                pending, advance, ..
            } => {
                host.complete_source_pending(
                    &pending.connection_id,
                    &pending.dataset,
                    &pending.page_digest,
                )
                .map_err(|refusal| fail("source.ingest", refusal.message))?;
                advance
            }
            capture::ActiveCapture::DisputeFallback { advance, .. } => Some(advance),
        };
        if let (Some(pull), Some(advance)) = (run.pull.as_mut(), advance) {
            if let Some(next_wm) = &advance.next
                && let Ok(raw) = next_wm.cursor_json()
            {
                run.committed_cursor_json
                    .insert(advance.dataset.clone(), raw);
            }
            pull.confirm(&advance);
            run.committed_watermarks = pull.watermarks().clone();
        }
    }
    if !run.pending_captures.is_empty() {
        return Ok(Next::Continue);
    }
    Ok(run.after.take().map_or(Next::Done, Next::Request))
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

/// The hosts of an [`ALLOWED_HOSTS_VAR`] value, normalized as a URL's host
/// is (lowercase, IP literals canonical). An entry that is not a bare host
/// (a port, a scheme, a path, userinfo) refuses the configuration, naming the
/// entry, rather than leaving the operator's host silently unlisted. The raw
/// entry is checked before URL parsing, because parsing drops a default port
/// (`127.0.0.1:443`) and empty userinfo (`@127.0.0.1`) and would list the bare
/// host on every port and scheme. Connection egress reads the same list.
///
/// # Errors
/// An entry that is not a bare host, named.
pub fn allowed_host_list(value: &str) -> Result<Vec<String>, String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(|entry| {
            Some(entry)
                .filter(|entry| bare_host(entry))
                .and_then(|entry| url::Url::parse(&format!("https://{entry}/")).ok())
                .filter(|parsed| {
                    parsed.port().is_none()
                        && parsed.path() == "/"
                        && parsed.username().is_empty()
                        && parsed.password().is_none()
                        && parsed.query().is_none()
                        && parsed.fragment().is_none()
                })
                .and_then(|parsed| parsed.host_str().map(str::to_owned))
                .ok_or_else(|| {
                    format!(
                        "source configuration: {ALLOWED_HOSTS_VAR} entry {entry:?} is not a host; list host names or IP literals (IPv6 in brackets), without a scheme, port or path"
                    )
                })
        })
        .collect()
}

/// Whether a raw allowlist entry is exactly a host: a DNS name or IPv4
/// dotted quad (letters, digits, hyphens, non-empty dot-separated labels, an
/// optional trailing dot), or a bracketed IPv6 literal. Anything else (`@`,
/// `:` outside brackets, `/`, `?`, `#`, whitespace) is not.
fn bare_host(entry: &str) -> bool {
    if let Some(inner) = entry
        .strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
    {
        return !inner.is_empty()
            && inner
                .chars()
                .all(|c| c.is_ascii_hexdigit() || c == ':' || c == '.');
    }
    let name = entry.strip_suffix('.').unwrap_or(entry);
    !name.is_empty()
        && name.split('.').all(|label| {
            !label.is_empty() && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
}

/// SYNC-URL-1, on the URL as the host's `fetch` parses it.
fn allowed_base(base: &str, allowed_hosts: &[String]) -> bool {
    let Ok(parsed) = url::Url::parse(base) else {
        return false;
    };
    if !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return false;
    }
    let (Some(host), Some(name)) = (parsed.host(), parsed.host_str()) else {
        return false;
    };
    if allowed_hosts.iter().any(|allowed| allowed == name) {
        return matches!(parsed.scheme(), "https" | "http");
    }
    parsed.scheme() == "https" && !crate::net::internal_host(&host)
}

fn watermark_text(watermarks: &BTreeMap<String, Watermark>) -> String {
    serde_json::to_string(watermarks).unwrap_or_else(|_| "{}".to_owned())
}

/// RFC 3339 UTC with milliseconds; millis outside jiff's range render as
/// the epoch, `1970-01-01T00:00:00.000Z`.
#[must_use]
pub fn rfc3339(ms: u64) -> String {
    i64::try_from(ms)
        .ok()
        .and_then(|ms| jiff::Timestamp::from_millisecond(ms).ok())
        .map_or_else(
            || "1970-01-01T00:00:00.000Z".to_owned(),
            |t| t.strftime("%Y-%m-%dT%H:%M:%S%.3fZ").to_string(),
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
    /// The generated mapping; empty for a dataset load, which its admitted
    /// definition types directly.
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
        org_id: host.scope().org_id.clone(),
        environment_id: host.scope().environment_id.clone(),
        secret_refs: Vec::new(),
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
        step_rows: crate::host::PAGE_ROWS,
        mapped: 0,
        pull: None,
        drain_only: false,
        mapping_binding: None,
        auth: Vec::new(),
        held: None,
        pending_captures: std::collections::VecDeque::new(),
        after: None,
        lease: None,
        watermark_from: "{}".to_owned(),
        committed_watermarks: BTreeMap::new(),
        committed_cursor_json: BTreeMap::new(),
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

/// The source schema a mapping reads; none for a load without a mapping.
fn source_schema_id(mapping: &str) -> String {
    if mapping.is_empty() {
        String::new()
    } else {
        format!("{}.source", mapping.trim_end_matches(".mapping"))
    }
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
) -> Option<Failure> {
    if run.drain_only && ended_ms.is_none() {
        return None;
    }
    let started = rfc3339(run.started_ms);
    let ended = ended_ms.map(rfc3339);
    let phase = if ended.is_some() { "end" } else { "start" };
    let rows = capture::report_rows(
        host,
        config,
        run,
        summary,
        (&started, ended.as_deref(), watermark),
    );
    let profile = connect_profile();
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
    // When a durable lease is held, check ownership inside the capture tx so a
    // concurrent takeover cannot be overwritten by a stale report.
    let lease_check = run.lease.as_ref().map(|lease| {
        (
            config.id.as_str(),
            lease.lease_id.as_str(),
            lease.generation,
            now,
        )
    });
    let expected_binding = run
        .mapping_binding
        .as_deref()
        .map(|binding| (config.mapping.as_str(), binding));
    match host.ingest_source_report_checked(&batch, lease_check, expected_binding) {
        Ok(()) => None,
        Err(error) => {
            let (code, message) = error.code_and_message();
            Some(fail(code, message))
        }
    }
}

fn connect_profile() -> PullProfile {
    PullProfile {
        source: REPORT_SOURCE.to_owned(),
        pipeline: "norn-connect/1".to_owned(),
        feeds: Vec::new(),
        token: None,
    }
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
/// A cell whose definitions predate `source.Upload` first gets the cell's
/// own sources pack through the governed install ([`ensure_upload_records`]).
///
/// # Errors
/// The pack could not be installed, the cell refused the record, or the
/// write failed.
pub fn keep_upload<E: SqlEngine>(
    host: &DevHost<E>,
    upload: &KeptUpload,
    now_ms: u64,
) -> Result<(), String> {
    ensure_upload_records(host, now_ms)?;
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
    let profile = connect_profile();
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
            reasons
                .iter()
                .map(|r| format!("{}: {}", r.code, r.reason))
                .collect::<Vec<_>>()
                .join(", ")
        )),
        None => Err("the cell captured no upload record".to_owned()),
    }
}

/// The cell's own sources pack: connections, sync runs and upload records,
/// and the mapping that lands the cell's reports as them.
const SOURCES_PACK: [&str; 6] = [
    include_str!("../../../registry/packs/sources/artifacts/source.Synced.interface.json"),
    include_str!("../../../registry/packs/sources/artifacts/source.Connection.entity.json"),
    include_str!("../../../registry/packs/sources/artifacts/source.SyncRun.entity.json"),
    include_str!("../../../registry/packs/sources/artifacts/source.Upload.entity.json"),
    include_str!("../../../registry/packs/sources/artifacts/norn.connect.source.json"),
    include_str!("../../../registry/packs/sources/artifacts/norn.connect.mapping.json"),
];

/// UPLOAD-RECORD-1 for a cell provisioned before `source.Upload`: install the
/// cell's own sources pack as the next governed definitions version (the
/// shared install path, DEFS-REOPEN-1), once. `Ok(true)` when it installed.
///
/// # Errors
/// The pack is refused or the install fails; the upload is then refused,
/// never completed without its record.
pub fn ensure_upload_records<E: SqlEngine>(
    host: &DevHost<E>,
    installed_at: u64,
) -> Result<bool, String> {
    if host
        .serves_object_type(UPLOAD_TYPE)
        .map_err(|refusal| refusal.message)?
    {
        return Ok(false);
    }
    let pack = SOURCES_PACK
        .iter()
        .map(|text| serde_json::from_str::<norn_ir::ArtifactEnvelope>(text))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("the sources pack: {e}"))?;
    host.install_artifacts(&pack, installed_at)
        .map_err(|reason| format!("the sources pack was not installed: {reason}"))?;
    Ok(true)
}

/// The kept record of the upload `upload_id`, if the cell completed it.
///
/// # Errors
/// A store failure, or a kept record the cell cannot read back.
pub fn kept_upload<E: SqlEngine>(
    host: &DevHost<E>,
    upload_id: &str,
) -> Result<Option<KeptUpload>, String> {
    if !host
        .serves_object_type(UPLOAD_TYPE)
        .map_err(|refusal| refusal.message)?
    {
        return Ok(None);
    }
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

    #[test]
    fn rfc3339_is_utc_with_milliseconds_and_out_of_range_reads_as_the_epoch() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(rfc3339(1_700_000_000_123), "2023-11-14T22:13:20.123Z");
        assert_eq!(rfc3339(1_700_000_000_000), "2023-11-14T22:13:20.000Z");
        assert_eq!(rfc3339(u64::MAX), "1970-01-01T00:00:00.000Z");
    }

    fn config() -> ConnectionConfig {
        serde_json::from_value(serde_json::json!({
            "id": "shop.dev", "system": "shopify", "label": "Dev store", "connector": "shopify.dev.connector",
            "mapping": "shopify.dev.mapping", "instance": "dev", "baseUrl": "https://dev-store.example",
            "orgId": "org_a", "environmentId": "prod", "secretRefs": ["shopify_dev_client", "shop_token"]
        }))
        .expect("config")
    }

    fn scope(org: &str, environment: &str) -> CellScope {
        CellScope {
            org_id: org.to_owned(),
            environment_id: environment.to_owned(),
        }
    }

    /// SOURCE-SCOPE-1: a cell keeps only its own connections and the secrets
    /// they bind, and one secret bound in two scopes refuses the configuration.
    #[test]
    fn a_cell_keeps_only_its_own_connections_and_their_secrets() -> Result<(), String> {
        let one = |id: &str, org: &str, environment: &str, refs: &[&str]| {
            serde_json::json!({"id": id, "system": "erpnext", "label": id, "connector": "c",
                "mapping": "m", "instance": "i", "baseUrl": "https://erp.example.test",
                "orgId": org, "environmentId": environment, "secretRefs": refs})
        };
        let config = serde_json::json!([
            one("a", "org_a", "prod", &["erp_a"]),
            one("b", "org_b", "prod", &["erp_b"]),
            one("c", "org_a", "dev", &["erp_c"]),
        ])
        .to_string();
        let secrets = BTreeMap::from([
            ("NORN_SECRET_ERP_A".to_owned(), "a".to_owned()),
            ("NORN_SECRET_ERP_B".to_owned(), "b".to_owned()),
            ("NORN_SECRET_ERP_C".to_owned(), "c".to_owned()),
        ]);
        let sources = Sources::new(&scope("org_a", "prod"), &config, &secrets)?;
        let ids: Vec<&str> = sources
            .connections()
            .iter()
            .map(|c| c.id.as_str())
            .collect();
        assert_eq!(ids, ["a"]);
        assert_eq!(
            sources.secrets.keys().collect::<Vec<_>>(),
            ["NORN_SECRET_ERP_A"]
        );
        let other = Sources::new(&scope("org_b", "dev"), &config, &secrets)?;
        assert_eq!(other.connections().len(), 0);
        assert_eq!(other.secrets.len(), 0);
        let shared = serde_json::json!([
            one("a", "org_a", "prod", &["erp_shared"]),
            one("b", "org_b", "prod", &["erp-shared"]),
        ])
        .to_string();
        let refused = Sources::new(&scope("org_a", "prod"), &shared, &secrets)
            .err()
            .ok_or("a secret in two scopes was accepted")?;
        assert!(refused.contains("NORN_SECRET_ERP_SHARED"), "{refused}");
        let unscoped = r#"[{"id": "a", "system": "erpnext", "label": "a", "connector": "c",
            "mapping": "m", "instance": "i", "baseUrl": "https://erp.example.test"}]"#;
        assert!(Sources::new(&scope("org_a", "prod"), unscoped, &secrets).is_err());
        // A connection that omits `secretRefs` is refused: `[]` says it has none.
        let silent = r#"[{"id": "a", "system": "erpnext", "label": "a", "connector": "c",
            "mapping": "m", "instance": "i", "baseUrl": "https://erp.example.test",
            "orgId": "org_a", "environmentId": "prod"}]"#;
        let refused = Sources::new(&scope("org_a", "prod"), silent, &secrets)
            .err()
            .ok_or("a connection without secretRefs was accepted")?;
        assert!(refused.contains("secretRefs"), "{refused}");
        Ok(())
    }

    /// SOURCE-SCOPE-1: a connector resolves only the secrets its connection
    /// binds, whatever the server holds.
    #[test]
    fn a_connector_resolves_only_secrets_its_connection_binds() -> Result<(), String> {
        let secrets = BTreeMap::from([
            ("NORN_SECRET_SHOP_TOKEN".to_owned(), "mine".to_owned()),
            ("NORN_SECRET_OTHER_CELL".to_owned(), "theirs".to_owned()),
        ]);
        let header = |reference: &str| {
            serde_json::json!({"auth": {"kind": "api_key", "placement": "header",
                "name": "Authorization", "prefix": "Bearer ", "secretRef": reference}})
        };
        assert_eq!(
            auth_headers(&header("shop_token"), &secrets, &config()),
            Ok(vec![("Authorization".to_owned(), "Bearer mine".to_owned())])
        );
        let unbound = auth_headers(&header("other_cell"), &secrets, &config())
            .err()
            .ok_or("an unbound secret was resolved")?;
        assert_eq!(unbound.0, "source.secret_unbound");
        let oauth = serde_json::json!({"auth": {"kind": "oauth2", "tokenUrl": "/token",
            "clientSecretRef": "other_cell"}});
        let unbound = client_credentials(&oauth, &secrets, &config())
            .err()
            .ok_or("an unbound client secret was resolved")?;
        assert_eq!(unbound.0, "source.secret_unbound");
        Ok(())
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
    fn base_urls_are_https_to_public_hosts() {
        for base in [
            "https://erp.example",
            "https://erp.example.com:8443/api",
            "https://ERP.Example.com/",
            "https://93.184.215.14",
            "https://[2606:2800:21f:cb07:6820:80da:af6b:8b2c]/",
        ] {
            assert!(allowed_base(base, &[]), "{base}");
        }
    }

    #[test]
    fn base_urls_refuse_other_schemes_and_parts() {
        for base in [
            "http://erp.example",
            "ftp://erp.example",
            "file:///etc/hosts",
            "https://user@erp.example",
            "https://user:pass@erp.example",
            "https://erp.example/?x=1",
            "https://erp.example/#x",
            "erp.example",
            "",
        ] {
            assert!(!allowed_base(base, &[]), "{base}");
        }
    }

    #[test]
    fn base_urls_refuse_internal_hosts_in_every_spelling() {
        for base in [
            // Loopback and the local name.
            "https://127.0.0.1",
            "https://127.8.9.10:8443",
            "https://localhost",
            "https://LOCALHOST.",
            "https://api.localhost",
            "https://[::1]",
            // Integer, octal, hex and short IPv4 forms of loopback.
            "https://2130706433",
            "https://017700000001",
            "https://0x7f000001",
            "https://0x7f.0.0.1",
            "https://0177.0.0.01",
            "https://127.1",
            // Private, shared, link-local and this-network IPv4.
            "https://10.0.0.1",
            "https://172.16.0.1",
            "https://172.31.255.255",
            "https://192.168.1.1",
            "https://100.64.0.1",
            "https://169.254.169.254",
            "https://0.0.0.0",
            "https://0",
            "https://255.255.255.255",
            "https://224.0.0.1",
            // Protocol-assignment, benchmarking and reserved IPv4.
            "https://192.0.0.1",
            "https://192.0.0.255",
            "https://198.18.0.1",
            "https://198.19.255.255",
            "https://240.0.0.1",
            "https://254.1.2.3",
            // Unspecified, link-local, unique-local and multicast IPv6.
            "https://[::]",
            "https://[fe80::1]",
            "https://[febf::1]",
            "https://[fc00::1]",
            "https://[fd12:3456::1]",
            "https://[ff02::1]",
            // IPv4-mapped and IPv4-compatible IPv6.
            "https://[::ffff:127.0.0.1]",
            "https://[::ffff:a9fe:a9fe]",
            "https://[::ffff:8.8.8.8]",
            "https://[::127.0.0.1]",
            // NAT64, local-use NAT64, 6to4 and Teredo carry an IPv4 destination.
            "https://[64:ff9b::7f00:1]",
            "https://[64:ff9b::8.8.8.8]",
            "https://[64:ff9b:1::a00:1]",
            "https://[64:ff9b:1:ffff::1]",
            "https://[2002:7f00:1::1]",
            "https://[2002:808:808::1]",
            "https://[2001:0:4136:e378::1]",
            // Site-local IPv6.
            "https://[fec0::1]",
            "https://[feff::1]",
            // Single-label (private network) names.
            "https://erp-sandbox:8000",
            "http://erp-sandbox:8000",
            "http://127.0.0.1:8069",
        ] {
            assert!(!allowed_base(base, &[]), "{base}");
        }
    }

    #[test]
    fn the_operator_allowlist_admits_named_internal_hosts_only() {
        let allowed =
            allowed_host_list(" erp-sandbox , 127.0.0.1,[::1], , LocalHost").expect("hosts");
        assert_eq!(
            allowed,
            ["erp-sandbox", "127.0.0.1", "[::1]", "localhost"].map(String::from)
        );
        for base in [
            "http://erp-sandbox:8000",
            "https://erp-sandbox",
            "http://127.0.0.1:8069",
            "http://[::1]:8000",
            "http://localhost:8000",
        ] {
            assert!(allowed_base(base, &allowed), "{base}");
        }
        for base in [
            "http://127.0.0.2",
            "http://erp-other:8000",
            "http://10.0.0.5",
            "ftp://erp-sandbox",
            "http://user@erp-sandbox",
        ] {
            assert!(!allowed_base(base, &allowed), "{base}");
        }
    }

    #[test]
    fn bare_hosts_are_admitted_normalized() {
        assert_eq!(
            allowed_host_list("erp-sandbox,10.0.0.5,[fd00::1],ERP-Sandbox,erp-sandbox.")
                .expect("hosts"),
            [
                "erp-sandbox",
                "10.0.0.5",
                "[fd00::1]",
                "erp-sandbox",
                "erp-sandbox."
            ]
            .map(String::from)
        );
    }

    #[test]
    fn neighbouring_public_addresses_stay_admitted() {
        for base in [
            "https://192.0.1.1",
            "https://198.17.255.255",
            "https://198.20.0.1",
            "https://[64:ff9b:2::1]",
            "https://[2001:db9::1]",
            "https://[2001:4860::8888]",
            "https://[2003::1]",
            "https://[2606:4700::1111]",
        ] {
            assert!(allowed_base(base, &[]), "{base}");
        }
    }

    #[test]
    fn a_malformed_allowlist_entry_refuses_the_configuration_naming_it() {
        for entry in [
            "10.0.0.5:80",
            "https://erp-sandbox",
            "erp-sandbox/api",
            "not a host",
            "user@erp-sandbox",
            "127.0.0.1:443",
            "@127.0.0.1",
            "erp-sandbox?x",
            "erp-sandbox#f",
            "user:pw@host",
            "[::1]:443",
            "host/",
            "[]",
            "[::1",
            ".",
            "erp..sandbox",
            ".erp-sandbox",
        ] {
            let found = allowed_host_list(&format!("erp-sandbox,{entry}")).expect_err("refused");
            assert!(
                found.contains(ALLOWED_HOSTS_VAR) && found.contains(&format!("{entry:?}")),
                "{entry}: {found}"
            );
            let refused =
                Sources::with_allowed_hosts(&scope("org_a", "prod"), "[]", &BTreeMap::new(), entry)
                    .err()
                    .unwrap_or_default();
            assert!(
                refused.contains(&format!("{entry:?}")),
                "{entry}: {refused}"
            );
        }
        assert_eq!(allowed_host_list(" , ,"), Ok(Vec::new()));
    }
}
