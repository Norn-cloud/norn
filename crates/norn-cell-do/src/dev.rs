//! `norn-dev-host`: author, check, run and observe in one loop (#207).
//!
//! The authoring CLI's `norn dev` finds the project and runs this host with
//! its compile worker.
//!
//! One process owns the project's SQLite cell. It serves the cell on a
//! loopback address, runs the TypeScript compile worker over the project's
//! sources, admits each compiled set with the native checker and installs
//! it as the next revision. Named invariants:
//!
//! - DEV-LASTGOOD-1: no compile, admission or install failure changes the
//!   serving revision, its ledger or any object; the last good revision keeps
//!   serving.
//! - DEV-IDEMPOTENT-1: a save that yields the active canonical set installs
//!   nothing (`unchanged`).
//! - DEV-SUPERSEDE-1: only the newest compiled generation is admitted;
//!   installs are compare-and-set on the active version.
//! - DEV-ONE-AUTHORITY-1: only this process installs. The worker's output is
//!   untrusted input, re-admitted here; no network route installs.
//! - DEV-LOOPBACK-1: the host binds a loopback address or refuses to start,
//!   so the manifest's all-grants developer never leaves the machine.
//! - HOST-REOPEN-1: a restart reopens the cell at its active revision without
//!   writing.
//! - DEV-REVIEWER-1: a second local principal, the reviewer
//!   (`.norn/dev.reviewer.token`), holds the same grants, so an approval the
//!   developer's call waits on can be decided locally (`dev/policy.rs`).
//! - DEV-LOAD-REFUSED-1: a recorded snapshot (`--load`) the cell refuses,
//!   or a row of it the mapping quarantines, ends the session with exit 3
//!   and the refusal; the session never serves after a refused load, and a
//!   restart loads the same snapshot again (rows already landed stay landed).
//! - DEV-DEFINITIONS-1: the sources' retained definitions (approvals) are
//!   admitted, signed and installed through the hosted riding-apply path, in
//!   process, as the manifest's actor (`dev/definitions.rs`).
//! - DEV-DEFINITIONS-ATOMIC-1: a reload commits the set, its approvals (with
//!   retirements) and the action policy in one step under the serving write
//!   lock; a refused approval publishes nothing.
//! - DEV-ACCESS-1: an action whose `access` names a policy is decided by that
//!   policy for every local principal, as on a hosted cell
//!   (`dev/policy.rs`).
//! - DEV-READ-DEFINITIONS-1: the sources' questions and dashboards are
//!   installed once a revision serves, in process, as the manifest's actor,
//!   through the hosted admit, sign-off and install path; those the sources
//!   drop retire (`dev/definitions.rs`).
//! - DEV-PRINCIPALS-1: the manifest's `dev.principals` are more local
//!   principals, each holding exactly its declared roles, with its own
//!   session token (`.norn/dev.principal.<n>.token`); the descriptor lists
//!   them (`dev/policy.rs`).
//! - DEV-CONFINED-1: the cell and the session files stay inside the project:
//!   the manifest's cell is a relative path under the project root, and the
//!   token and descriptor are owner-only files never written through a symlink.
//!
//! Stability: the `--json` events and the `.norn/dev.json` descriptor
//! (`norn-dev/1`) are experimental, not a frozen contract (D84). The
//! descriptor and the `ready` event say so with `"stability": "experimental"`.
use clap::Parser;
use norn_api::host::DevHost;
use norn_api::scope::CellScope;
use norn_cell::engine::NativeEngine;
use norn_cell::{CellStore, InstalledDefinitionSet};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::fmt::Write as _;
use std::io::{BufRead as _, BufReader, Write as _};
use std::net::{IpAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

mod admit;
mod checkpoint_key;
mod definitions;
mod http;
mod load;
mod named;
mod policy;
mod state;
mod timers;

use named::{
    ManifestPrincipal, named_principals, principal_token_file, remove_stale_principal_tokens,
};
use policy::{DevMetrics, DevPrincipals, LocalDeveloper};
use state::{cell_path, lock, private_dir, reopen, token, write_private};

/// `norn-dev-host` options. The compile worker command follows `--`; the
/// authoring CLI's `norn dev` supplies it.
#[derive(Debug, Parser)]
#[command(
    name = "norn-dev-host",
    version,
    about = "Serve a Norn project's cell on loopback and re-admit its sources on every save"
)]
pub struct DevArgs {
    /// The project directory holding `norn.json`.
    #[arg(long, default_value = ".")]
    project: PathBuf,
    /// Port to listen on; 0 picks a free one. Defaults to the manifest's `dev.port`.
    #[arg(long)]
    port: Option<u16>,
    /// Loopback address to listen on; any other address refuses.
    #[arg(long, default_value = "127.0.0.1")]
    host: String,
    /// Print one JSON event per line instead of the human view.
    #[arg(long)]
    json: bool,
    /// A recorded source snapshot to load through the cell's own mapping
    /// path when the cell opens (repeatable). See `dev/load.rs`.
    #[arg(long = "load", value_name = "SNAPSHOT")]
    load: Vec<PathBuf>,
    /// The compile worker command (program and arguments); `norn dev` in the
    /// authoring CLI supplies it.
    #[arg(last = true, required = true)]
    worker: Vec<String>,
}

/// `norn-project/1`, as `norn init` writes it. Unknown keys refuse.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    format: String,
    name: String,
    #[serde(rename = "sources")]
    _sources: Vec<String>,
    scope: ManifestScope,
    dev: ManifestDev,
    cell: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ManifestScope {
    org_id: String,
    environment_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestDev {
    port: u16,
    actor: String,
    /// More local principals, each with only its declared roles
    /// (DEV-PRINCIPALS-1).
    #[serde(default)]
    principals: Vec<ManifestPrincipal>,
}

/// A setup or usage failure: a stable code, an actionable message and the
/// shared exit status (2 usage, 3 setup).
#[derive(Debug)]
struct Failure {
    code: &'static str,
    message: String,
    exit: u8,
}

impl Failure {
    fn setup(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            exit: 3,
        }
    }
}

/// Run the loop until the worker exits or the process is stopped.
pub fn execute(args: &DevArgs) -> ExitCode {
    let mut out = Events { json: args.json };
    match run(args, &mut out) {
        Ok(()) => ExitCode::SUCCESS,
        Err(failure) => {
            out.emit(
                &json!({ "event": "fatal", "diagnostic": { "code": failure.code, "severity": "error", "phase": "setup", "message": failure.message } }),
                &format!("error[{}] {}", failure.code, failure.message),
            );
            ExitCode::from(failure.exit)
        }
    }
}

struct Events {
    json: bool,
}

impl Events {
    fn emit(&mut self, event: &Value, human: &str) {
        let line = if self.json {
            event.to_string()
        } else {
            human.to_owned()
        };
        let _written = writeln!(std::io::stdout().lock(), "{line}");
    }
}

/// The dev host's wall clock: install times and token lifetimes. This
/// process is the trusted host boundary, so it reads the clock here only.
#[allow(
    clippy::disallowed_methods,
    reason = "the dev host is the trusted boundary that supplies the cell's clock"
)]
pub(super) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

fn short(digest: &str) -> &str {
    digest.get(..12).unwrap_or(digest)
}

#[allow(
    clippy::too_many_lines,
    reason = "one session: setup, then the admission loop in its documented order"
)]
fn run(args: &DevArgs, out: &mut Events) -> Result<(), Failure> {
    let root = args.project.canonicalize().map_err(|error| {
        Failure::setup(
            "norn.project.not_found",
            format!("{}: {error}", args.project.display()),
        )
    })?;
    let manifest: Manifest = std::fs::read_to_string(root.join("norn.json"))
        .map_err(|error| {
            Failure::setup(
                "norn.project.not_found",
                format!("no norn.json in {}: {error}", root.display()),
            )
        })
        .and_then(|text| {
            serde_json::from_str(&text).map_err(|error| {
                Failure::setup(
                    "norn.project.invalid_manifest",
                    format!("norn.json: {error}"),
                )
            })
        })?;
    if manifest.format != "norn-project/1" {
        return Err(Failure::setup(
            "norn.project.invalid_manifest",
            "norn.json must be format norn-project/1",
        ));
    }
    let ip: IpAddr = args.host.parse().map_err(|error| Failure {
        code: "norn.dev.usage",
        message: format!("--host {}: {error}", args.host),
        exit: 2,
    })?;
    if !ip.is_loopback() {
        return Err(Failure::setup(
            "norn.dev.non_loopback",
            format!(
                "--host {ip} is not a loopback address; norn dev grants everything to the local developer, so it only listens on 127.0.0.1 or ::1"
            ),
        ));
    }
    let named = named_principals(&manifest.dev)?;
    let state = private_dir(&root)?;
    let _lock = lock(&state)?;
    let token = token(manifest.dev.principals.len())?;
    write_private(
        &state.join("dev.token"),
        &zeroize::Zeroizing::new(format!("{}\n", token.bearer)),
    )?;
    write_private(
        &state.join("dev.reviewer.token"),
        &zeroize::Zeroizing::new(format!("{}\n", token.reviewer)),
    )?;
    for (index, bearer) in token.principals.iter().enumerate() {
        write_private(
            &root.join(principal_token_file(index.saturating_add(1))),
            &zeroize::Zeroizing::new(format!("{bearer}\n")),
        )?;
    }
    remove_stale_principal_tokens(&state, token.principals.len())?;
    let scope = CellScope {
        org_id: manifest.scope.org_id.clone(),
        environment_id: manifest.scope.environment_id.clone(),
    };
    let cell_path = cell_path(&root, &manifest.cell)?;
    let loads = args
        .load
        .iter()
        .map(|path| {
            path.canonicalize().map_err(|error| {
                Failure::setup(
                    "norn.dev.load_missing",
                    format!("{}: {error}", path.display()),
                )
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let actions = policy::dev_actions();
    let (reopened, loaded) = reopen(
        &cell_path,
        &scope,
        (&manifest.dev.actor, &named),
        &loads,
        &actions,
    )?;
    if let Some(loaded) = loaded {
        out.emit(
            &json!({ "event": "loaded", "files": loaded.files }),
            &format!("· loaded {} recorded snapshot(s)", loaded.files.len()),
        );
    }
    let serving: http::Serving = Arc::new(RwLock::new(reopened));
    let listener =
        TcpListener::bind((ip, args.port.unwrap_or(manifest.dev.port))).map_err(|error| {
            if error.kind() == std::io::ErrorKind::AddrInUse {
                Failure::setup(
                    "norn.dev.port_in_use",
                    format!(
                        "{ip}:{} is in use; pass --port 0 or another port",
                        args.port.unwrap_or(manifest.dev.port)
                    ),
                )
            } else {
                Failure::setup("norn.dev.io", format!("cannot listen on {ip}: {error}"))
            }
        })?;
    let address = listener
        .local_addr()
        .map_err(|error| Failure::setup("norn.dev.io", error.to_string()))?;
    let url = format!("http://{address}");
    let mut sessions = vec![
        (token.bearer.clone(), manifest.dev.actor.clone()),
        (
            token.reviewer.clone(),
            policy::reviewer(&manifest.dev.actor),
        ),
    ];
    sessions.extend(
        token.principals.iter().cloned().zip(
            manifest
                .dev
                .principals
                .iter()
                .map(|principal| principal.actor.clone()),
        ),
    );
    let principals = DevPrincipals::new(sessions).with_named(Arc::clone(&named));
    // One derived-metrics runtime serves the HTTP questions and the sources'
    // question and dashboard sync, so a revision is derived once.
    let questions = Arc::new(DevMetrics::new(token.handle_key, scope.clone()));
    // D102: a build with `duckdb` answers questions and profiles on the
    // project's own analytics engine, under `.norn/analytics`.
    #[cfg(feature = "duckdb")]
    let engine =
        norn_cell::analytics::engine::Engine::new(norn_cell::analytics::engine::Settings {
            root: root.join(".norn/analytics"),
            memory_limit: "1GB".to_owned(),
            threads: 2,
        })
        .map(crate::olap::InProcess)
        .map_err(|error| {
            Failure::setup(
                "norn.dev.io",
                format!("cannot open the analytics engine: {error}"),
            )
        })?;
    let connections = http::dev_connections(cell_path.clone())
        .map_err(|message| Failure::setup("norn.dev.connections_config", message))?;
    {
        let serving = Arc::clone(&serving);
        let questions = Arc::clone(&questions);
        std::thread::spawn(move || {
            #[cfg(feature = "duckdb")]
            let engine: Option<&dyn norn_api::analytics_routes::Transport> = Some(&engine);
            #[cfg(not(feature = "duckdb"))]
            let engine: Option<&dyn norn_api::analytics_routes::Transport> = None;
            http::serve(
                &listener,
                &serving,
                &principals,
                &questions,
                engine,
                &connections,
            );
        });
    }
    // TIMER-ALARM-1: the dev host's clock runs the cell's due work, as the hosted alarm does.
    {
        let serving = Arc::clone(&serving);
        std::thread::spawn(move || timers::drive(&serving));
    }
    let session = Session {
        loads: &loads,
        loaded: std::cell::RefCell::new(None),
        root: &root,
        manifest: &manifest,
        named: Arc::clone(&named),
        metrics: questions,
        scope: &scope,
        url: &url,
        started_at: now_ms(),
        actions,
        ungoverned: std::cell::RefCell::new(None),
    };
    let active =
        active(&serving).map_err(|message| Failure::setup("norn.cell.unreadable", message))?;
    session.header(out, active.as_ref());
    if let Some(active) = &active {
        session.access(out, &serving);
        session.ready(out, active)?;
    }
    let lines = spawn_worker(&args.worker, &root)?;
    let mut pending = VecDeque::new();
    let mut ready = active.is_some();
    while let Some(event) = newest(&lines, &mut pending) {
        let kind = event.get("event").and_then(Value::as_str);
        if kind == Some("compiled") {
            if let Some(installed) = session.compiled(out, &serving, &cell_path, &event)? {
                if ready {
                    session.revision(out, &installed, &event)?;
                } else {
                    session.ready(out, &installed)?;
                    ready = true;
                }
            }
        } else if kind == Some("fatal") {
            let message = event
                .get("diagnostic")
                .and_then(|diagnostic| diagnostic.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("the compile worker failed")
                .to_owned();
            return Err(Failure::setup("norn.dev.worker_failed", message));
        }
    }
    Err(Failure::setup(
        "norn.dev.worker_exited",
        "the compile worker exited; restart norn dev",
    ))
}

fn is_compiled(event: &Value) -> bool {
    event.get("event").and_then(Value::as_str) == Some("compiled")
}

/// DEV-SUPERSEDE-1: wait for worker events, keep only the newest queued
/// `compiled` generation, and hand out every other event first. Nothing but
/// an older generation is ever dropped: a compiled event behind another kind
/// of line stays queued in `pending`.
fn newest(lines: &Receiver<Value>, pending: &mut VecDeque<Value>) -> Option<Value> {
    if pending.is_empty() {
        pending.push_back(lines.recv().ok()?);
    }
    while let Ok(next) = lines.try_recv() {
        pending.push_back(next);
    }
    if let Some(last) = pending.iter().rposition(is_compiled) {
        let mut index = 0_usize;
        pending.retain(|event| {
            let keep = index == last || !is_compiled(event);
            index = index.saturating_add(1);
            keep
        });
    }
    let first = pending
        .iter()
        .position(|event| !is_compiled(event))
        .unwrap_or(0);
    pending.remove(first)
}

fn spawn_worker(command: &[String], root: &Path) -> Result<Receiver<Value>, Failure> {
    let (program, arguments) = command.split_first().ok_or_else(|| Failure {
        code: "norn.dev.usage",
        message: "the worker command is missing after --".to_owned(),
        exit: 2,
    })?;
    let names = std::env::vars_os().map(|(name, _)| name);
    let mut child = worker_command(program, arguments, root, names)
        .spawn()
        .map_err(|error| {
            Failure::setup(
                "norn.setup.worker_unavailable",
                format!("cannot start the compile worker {program}: {error}"),
            )
        })?;
    let stdout = child.stdout.take().ok_or_else(|| {
        Failure::setup(
            "norn.setup.worker_unavailable",
            "the compile worker has no stdout",
        )
    })?;
    let lifeline = child.stdin.take();
    let (send, receive) = mpsc::channel();
    std::thread::spawn(move || {
        let _lifeline = lifeline;
        for line in BufReader::new(stdout).lines() {
            let Ok(line) = line else { break };
            // stdout carries only NDJSON events; anything else is not an event.
            if let Ok(event) = serde_json::from_str::<Value>(&line)
                && send.send(event).is_err()
            {
                break;
            }
        }
        let _status = child.wait();
    });
    Ok(receive)
}

/// The compile worker's command. CONN-SECRET-ENV-1: the vault KEKs and the
/// OAuth client secrets this host was given never reach the worker, which
/// runs the project's own code.
/// `inherited` names the variables the worker would inherit.
fn worker_command(
    program: &str,
    arguments: &[String],
    root: &Path,
    inherited: impl Iterator<Item = std::ffi::OsString>,
) -> Command {
    #[allow(
        clippy::disallowed_methods,
        reason = "the dev host's one capability: running the project's compile worker"
    )]
    let mut command = Command::new(program);
    command
        .args(arguments)
        .arg("--project")
        .arg(root)
        // The worker exits when this pipe closes, so it never outlives the host.
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit());
    for name in inherited {
        if name.to_str().is_some_and(crate::connections::secret_var) {
            command.env_remove(name);
        }
    }
    command
}

struct Session<'a> {
    loads: &'a [PathBuf],
    /// What a load during the first provision did, reported with its revision.
    loaded: std::cell::RefCell<Option<load::Loaded>>,
    root: &'a Path,
    manifest: &'a Manifest,
    /// The named principals and their roles (DEV-PRINCIPALS-1).
    named: policy::NamedPrincipals,
    /// The serving revision's metrics, which the sources' questions and
    /// dashboards are admitted against (DEV-READ-DEFINITIONS-1): the HTTP
    /// server's own.
    metrics: Arc<DevMetrics>,
    scope: &'a CellScope,
    url: &'a str,
    started_at: u64,
    /// The active revision's action policy (DEV-ACCESS-1).
    actions: policy::DevActions,
    /// The ungoverned actions last reported, so a change is reported once.
    ungoverned: std::cell::RefCell<Option<Result<Vec<String>, String>>>,
}

impl Session<'_> {
    fn header(&self, out: &mut Events, active: Option<&InstalledDefinitionSet>) {
        if !out.json {
            out.emit(
                &Value::Null,
                &format!(
                    "norn dev · project {} · scope org={} env={} · actor {} (all grants, loopback only)\ncell {} ({})",
                    self.manifest.name,
                    self.scope.org_id,
                    self.scope.environment_id,
                    self.manifest.dev.actor,
                    self.manifest.cell,
                    active.map_or_else(|| "new".to_owned(), |set| format!("reopened at r{}", set.version)),
                ),
            );
        }
    }

    fn descriptor(&self, active: &InstalledDefinitionSet) -> Value {
        json!({
            "format": "norn-dev/1",
            "stability": "experimental",
            "pid": std::process::id(),
            "url": self.url,
            "tokenFile": ".norn/dev.token",
            "scope": { "orgId": self.scope.org_id, "environmentId": self.scope.environment_id },
            "actor": self.manifest.dev.actor,
            "reviewer": { "actor": policy::reviewer(&self.manifest.dev.actor), "tokenFile": ".norn/dev.reviewer.token" },
            "principals": self.manifest.dev.principals.iter().enumerate().map(|(index, principal)| json!({
                "actor": principal.actor,
                "roles": principal.roles,
                "tokenFile": principal_token_file(index.saturating_add(1)),
            })).collect::<Vec<_>>(),
            "active": { "version": active.version, "receiptSha256": active.receipt_sha256 },
            "startedAt": self.started_at,
        })
    }

    fn ready(&self, out: &mut Events, active: &InstalledDefinitionSet) -> Result<(), Failure> {
        let descriptor = self.descriptor(active);
        write_private(
            &self.root.join(".norn/dev.json"),
            &format!("{descriptor}\n"),
        )?;
        let mut event = descriptor;
        if let Some(object) = event.as_object_mut() {
            object.insert("event".to_owned(), json!("ready"));
        }
        let mut named = String::new();
        for (index, principal) in self.manifest.dev.principals.iter().enumerate() {
            let _written = write!(
                named,
                "\n  as {} ({}): {}",
                principal.actor,
                principal.roles.join(", "),
                principal_token_file(index.saturating_add(1))
            );
        }
        out.emit(
            &event,
            &format!(
                "✓ serving r{} · receipt {}…\n  API  {}/v1   token .norn/dev.token (reviewer: .norn/dev.reviewer.token){named}\nwatching sources",
                active.version,
                short(&active.receipt_sha256),
                self.url
            ),
        );
        Ok(())
    }

    fn revision(
        &self,
        out: &mut Events,
        active: &InstalledDefinitionSet,
        compiled: &Value,
    ) -> Result<(), Failure> {
        write_private(
            &self.root.join(".norn/dev.json"),
            &format!("{}\n", self.descriptor(active)),
        )?;
        out.emit(
            &json!({ "event": "revision", "version": active.version, "receiptSha256": active.receipt_sha256,
                     "gen": compiled.get("gen"), "sourceDigest": compiled.get("sourceDigest") }),
            &format!("✓ r{} admitted · receipt {}…", active.version, short(&active.receipt_sha256)),
        );
        Ok(())
    }

    fn rejected(
        out: &mut Events,
        serving: &http::Serving,
        compiled: &Value,
        phase: &str,
        diagnostics: &[Value],
    ) {
        let version = active(serving).ok().flatten().map(|set| set.version);
        let mut human = format!(
            "✗ change refused ({phase}) · {}",
            version.map_or_else(
                || "nothing served yet".to_owned(),
                |version| format!("still serving r{version}")
            )
        );
        for diagnostic in diagnostics {
            let text = |key: &str| {
                diagnostic
                    .get(key)
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned()
            };
            let at = diagnostic.get("source").map_or_else(String::new, |source| {
                format!(
                    " {}:{}",
                    source.get("file").and_then(Value::as_str).unwrap_or("?"),
                    source.get("line").and_then(Value::as_u64).unwrap_or(0)
                )
            });
            let _written = write!(human, "\nerror[{}]{at} {}", text("code"), text("message"));
        }
        out.emit(
            &json!({ "event": "rejected", "phase": phase, "gen": compiled.get("gen"), "sourceDigest": compiled.get("sourceDigest"),
                     "diagnostics": diagnostics, "serving": version.map(|version| json!({ "version": version })) }),
            &human,
        );
    }

    /// One compiled generation: admit it, then install it unless it is the
    /// active set. Returns the new revision, or `None` when nothing changed.
    fn compiled(
        &self,
        out: &mut Events,
        serving: &http::Serving,
        cell_path: &Path,
        event: &Value,
    ) -> Result<Option<InstalledDefinitionSet>, Failure> {
        if event.get("ok").and_then(Value::as_bool) != Some(true) {
            let diagnostics = event
                .get("diagnostics")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            Self::rejected(out, serving, event, "compile", &diagnostics);
            return Ok(None);
        }
        let artifacts = event
            .get("artifacts")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let source_map = event
            .get("sourceMap")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let current =
            active(serving).map_err(|message| Failure::setup("norn.cell.unreadable", message))?;
        let installed_at = now_ms().max(
            current
                .as_ref()
                .map_or(0, |set| set.installed_at.saturating_add(1)),
        );
        let admitted = match admit::admit(&artifacts, installed_at, admit::keeps(serving)) {
            Ok(admitted) => admitted,
            Err(refused) => {
                Self::rejected(
                    out,
                    serving,
                    event,
                    "admission",
                    &admit::source_mapped(refused.diagnostics, &source_map),
                );
                return Ok(None);
            }
        };
        let definitions = event
            .get("definitions")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        // DEV-DEFINITIONS-ATOMIC-1: the set, its approvals and the action
        // policy commit together under the serving write lock.
        let before = current.as_ref().map(|set| set.version);
        let result = match current {
            Some(set) => {
                let guard = serving.write().map_err(|_poisoned| {
                    Failure::setup("norn.cell.unreadable", "serving state poisoned")
                })?;
                let host = guard.as_ref().ok_or_else(|| {
                    Failure::setup("norn.cell.unreadable", "the serving host disappeared")
                })?;
                definitions::commit(
                    host,
                    &self.manifest.dev.actor,
                    policy::dev_kinds(),
                    &definitions,
                    admitted,
                    &set,
                    &self.actions,
                    now_ms(),
                )
            }
            None => self.provision(serving, cell_path, admitted, &definitions),
        };
        match result {
            Ok(definitions::Committed::Unchanged) => {
                self.unchanged(out, serving, event, &definitions, before)
            }
            Ok(definitions::Committed::Installed(installed, events)) => {
                if let Some(loaded) = self.loaded.take() {
                    out.emit(
                        &json!({ "event": "loaded", "files": loaded.files }),
                        &format!("· loaded {} recorded snapshot(s)", loaded.files.len()),
                    );
                }
                Self::definitions(out, &events);
                // The revision reported is the one serving after the reads
                // synced over it, so `.norn/dev.json` and the events name the
                // version a client pins. The install committed: a failed
                // readback after it is a warning, never the session's end.
                let (moved, readback) = self.served(out, serving, &definitions);
                Ok(reported(out, *installed, moved, readback))
            }
            Err(definitions::Rejected { phase, diagnostics }) => {
                Self::rejected(out, serving, event, phase, &diagnostics);
                // DEV-LOAD-REFUSED-1: a `--load` the cell refuses ends the
                // session with that refusal; it never waits unserved for a
                // source save that cannot change the snapshot.
                if let Some(refusal) = diagnostics.iter().find(|refusal| {
                    refusal.get("code").and_then(Value::as_str) == Some("norn.dev.load_refused")
                }) {
                    let message = refusal
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("a recorded snapshot was refused")
                        .to_owned();
                    return Err(Failure::setup("norn.dev.load_refused", message));
                }
                Ok(None)
            }
        }
    }

    /// The set is unchanged: sync the sources' reads over it. A question or
    /// dashboard the save changed installs (or retires) as the next
    /// definitions version, so that is a new revision to report, never
    /// "unchanged".
    fn unchanged(
        &self,
        out: &mut Events,
        serving: &http::Serving,
        event: &Value,
        definitions: &[Value],
        before: Option<u64>,
    ) -> Result<Option<InstalledDefinitionSet>, Failure> {
        let (_moved, after) = self.served(out, serving, definitions);
        let after = after.map_err(|message| Failure::setup("norn.cell.unreadable", message))?;
        if after.as_ref().map(|set| set.version) == before {
            out.emit(
                &json!({ "event": "unchanged", "gen": event.get("gen"), "sourceDigest": event.get("sourceDigest") }),
                "· unchanged (same definitions)",
            );
            return Ok(None);
        }
        Ok(after)
    }

    /// Report each approval revision a commit appended.
    fn definitions(out: &mut Events, events: &[Value]) {
        for event in events {
            let text = |key: &str| event.get(key).and_then(Value::as_str).unwrap_or_default();
            out.emit(
                event,
                &format!(
                    "✓ {} {} v{} {}",
                    text("kind"),
                    text("id"),
                    event.get("version").and_then(Value::as_u64).unwrap_or(0),
                    text("lifecycle"),
                ),
            );
        }
    }

    /// A revision serves: report its access, then sync the sources'
    /// questions and dashboards over it. Returns the active revision after
    /// the sync, which each install or retirement moved to its next
    /// definitions version, or why it could not be read; and whether the
    /// sync moved it.
    fn served(
        &self,
        out: &mut Events,
        serving: &http::Serving,
        definitions: &[Value],
    ) -> (bool, Result<Option<InstalledDefinitionSet>, String>) {
        self.access(out, serving);
        let moved = self.sync_reads(out, serving, definitions);
        (moved, active(serving))
    }

    /// DEV-READ-DEFINITIONS-1: install the sources' changed questions and
    /// dashboards over the serving revision, retire the dropped ones, and
    /// report both; a refused one is reported and changes nothing. Returns
    /// whether any was installed or retired.
    fn sync_reads(&self, out: &mut Events, serving: &http::Serving, definitions: &[Value]) -> bool {
        let synced = {
            let Ok(guard) = serving.read() else {
                return false;
            };
            let Some(host) = guard.as_ref() else {
                return false;
            };
            let runtime = self.metrics.questions(host);
            let questions: Option<&dyn norn_api::question_routes::AdHocQuestions<NativeEngine>> =
                match &runtime {
                    Ok(runtime) => Some(runtime),
                    Err(_unavailable) => None,
                };
            definitions::sync_reads(
                host,
                &self.manifest.dev.actor,
                policy::dev_kinds(),
                questions,
                definitions,
                now_ms(),
            )
        };
        Self::definitions(out, &synced.events);
        if !synced.refused.is_empty() {
            let count = synced.refused.len();
            out.emit(
                &json!({ "event": "definitions_refused", "diagnostics": synced.refused }),
                &format!(
                    "✗ {count} question or dashboard finding(s); the installed ones keep serving"
                ),
            );
        }
        !synced.events.is_empty()
    }

    /// Recompile the active revision's action policy and report what it
    /// governs when that changed.
    fn access(&self, out: &mut Events, serving: &http::Serving) {
        let refreshed = {
            let Ok(guard) = serving.read() else {
                return;
            };
            let Some(host) = guard.as_ref() else {
                return;
            };
            policy::refresh_actions(&self.actions, host)
        };
        if self.ungoverned.borrow().as_ref() == Some(&refreshed) {
            return;
        }
        match &refreshed {
            Ok(ungoverned) if ungoverned.is_empty() => {}
            Ok(ungoverned) => out.emit(
                &json!({ "event": "access", "ungoverned": ungoverned }),
                &format!(
                    "· no access policy governs {}: only the developer and the reviewer may call them, and a hosted cell would serve none of them",
                    ungoverned.join(", ")
                ),
            ),
            Err(reason) => out.emit(
                &json!({ "event": "access", "error": reason }),
                &format!("✗ the access policies do not compile, so no governed action is served: {reason}"),
            ),
        }
        self.ungoverned.replace(Some(refreshed));
    }

    /// The first revision of a new cell, with the sources' approvals in the
    /// provisioning transaction (DEV-DEFINITIONS-ATOMIC-1). The approvals
    /// are admitted first against the candidate on a scratch in-memory cell,
    /// so a refused one writes no file; the host is served only once its
    /// action policy is compiled.
    fn provision(
        &self,
        serving: &http::Serving,
        cell_path: &Path,
        admitted: admit::Admitted,
        definitions: &[Value],
    ) -> Result<definitions::Committed, definitions::Rejected> {
        let install = |diagnostic: Value| definitions::Rejected {
            phase: "install",
            diagnostics: vec![diagnostic],
        };
        let refused = |message: String| {
            install(
                json!({ "code": "defs.provision_refused", "severity": "error", "message": message }),
            )
        };
        let host_refused = |refusal: norn_api::refusal::Refusal| {
            install(serde_json::to_value(&refusal).unwrap_or(Value::Null))
        };
        let prepared = {
            let scratch = NativeEngine::open_in_memory()
                .and_then(|engine| {
                    CellStore::provision_definitions(
                        engine,
                        admitted.model.clone(),
                        &admitted.input,
                        &self.scope.key(),
                    )
                })
                .map_err(|error| refused(error.to_string()))?;
            let scratch = DevHost::from_store(scratch, self.scope.clone(), |command| {
                command.payload.clone()
            })
            .map_err(host_refused)?;
            definitions::prepare(
                &scratch,
                &self.manifest.dev.actor,
                policy::dev_kinds(),
                definitions,
                &admitted.input.canonical_set,
                now_ms(),
            )
            .map_err(|diagnostics| definitions::Rejected {
                phase: "definitions",
                diagnostics,
            })?
        };
        let engine = NativeEngine::open(cell_path, checkpoint_key::dev_store_options(cell_path))
            .map_err(|error| refused(error.to_string()))?;
        let store = CellStore::provision_definitions_riding(
            engine,
            admitted.model,
            &admitted.input,
            &self.scope.key(),
            &prepared.riding,
        )
        .map_err(|error| refused(error.to_string()))?;
        let store = if self.loads.is_empty() {
            store
        } else {
            let (store, loaded) = load::load(store, &self.scope.key(), self.loads).map_err(
                |message| install(json!({ "code": "norn.dev.load_refused", "severity": "error", "message": message })),
            )?;
            self.loaded.replace(Some(loaded));
            store
        };
        let host =
            DevHost::from_store(store, self.scope.clone(), |command| command.payload.clone())
                .map_err(host_refused)?
                .with_authorizer(
                    LocalDeveloper::for_actor(
                        &self.manifest.dev.actor,
                        std::sync::Arc::clone(&self.actions),
                    )
                    .with_named(Arc::clone(&self.named)),
                );
        let installed = host
            .active_definitions()
            .map_err(host_refused)?
            .ok_or_else(|| refused("the provisioned cell has no active revision".to_owned()))?;
        let _ungoverned = policy::refresh_actions(&self.actions, &host);
        *serving
            .write()
            .map_err(|_poisoned| refused("serving state poisoned".to_owned()))? = Some(host);
        Ok(definitions::Committed::Installed(
            Box::new(installed),
            prepared.events,
        ))
    }
}

/// The revision to report after `installed` committed: the one serving once
/// the reads synced (`readback`). A readback that fails is a warning, not the
/// session's end: `installed` is still reported when no read moved the
/// version past it; otherwise no version is reported, never a stale one, and
/// the next save reports the serving revision.
fn reported(
    out: &mut Events,
    installed: InstalledDefinitionSet,
    moved: bool,
    readback: Result<Option<InstalledDefinitionSet>, String>,
) -> Option<InstalledDefinitionSet> {
    let message = match readback {
        Ok(after) => return Some(after.unwrap_or(installed)),
        Err(message) => message,
    };
    let known = (!moved).then_some(installed);
    let version = known.as_ref().map(|set| set.version);
    out.emit(
        &json!({ "event": "warning", "code": "norn.cell.unreadable", "message": message,
                 "version": version }),
        &version.map_or_else(
            || format!("! the serving revision could not be read back ({message}); the next save reports it"),
            |version| format!("! r{version} installed; reading it back failed ({message})"),
        ),
    );
    known
}

fn active(serving: &http::Serving) -> Result<Option<InstalledDefinitionSet>, String> {
    let guard = serving
        .read()
        .map_err(|_poisoned| "serving state poisoned".to_owned())?;
    guard.as_ref().map_or(Ok(None), |host| {
        host.active_definitions().map_err(|refusal| refusal.message)
    })
}

#[cfg(all(test, unix))]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "dev-host session-file and queue tests"
)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    fn project() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        (dir, root)
    }

    fn dev(principals: &Value) -> ManifestDev {
        serde_json::from_value(
            json!({ "port": 0, "actor": "local:developer", "principals": principals }),
        )
        .unwrap()
    }

    /// DEV-PRINCIPALS-1: a named principal holds exactly its declared roles;
    /// a manifest that names none keeps the two local principals.
    #[test]
    fn named_principals_hold_their_declared_roles() {
        let none: ManifestDev =
            serde_json::from_value(json!({ "port": 0, "actor": "local:developer" })).unwrap();
        assert!(named_principals(&none).unwrap().is_empty());
        let named = named_principals(&dev(&json!([
            { "actor": "clerk:hana", "roles": ["warehouse_clerk"] },
            { "actor": "ops:omar", "roles": ["ops_manager", "planner"] },
        ])))
        .unwrap();
        assert_eq!(
            named.get("clerk:hana").map(Vec::as_slice),
            Some(&["warehouse_clerk".to_owned()][..])
        );
        assert_eq!(named.len(), 2);
        assert_eq!(principal_token_file(1), ".norn/dev.principal.1.token");
    }

    /// A manifest that names fewer principals than the last session leaves
    /// no token file of the dropped ones; the kept ones, every other session
    /// file, and whatever a dropped link names stay.
    #[test]
    fn dropped_principals_keep_no_token_file() {
        let (_dir, root) = project();
        let state = private_dir(&root).unwrap();
        for file in [
            "dev.principal.1.token",
            "dev.principal.2.token",
            "dev.principal.3.token",
            "dev.token",
            "dev.principal.x.token",
        ] {
            write_private(&state.join(file), "secret\n").unwrap();
        }
        // A dropped token file that is a link: the link goes, never what it names.
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("kept");
        std::fs::write(&target, "not a token\n").unwrap();
        symlink(&target, state.join("dev.principal.4.token")).unwrap();
        remove_stale_principal_tokens(&state, 1).unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "not a token\n");
        let mut left: Vec<String> = std::fs::read_dir(&state)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(
            left,
            [
                "dev.principal.1.token",
                "dev.principal.x.token",
                "dev.token"
            ]
        );
    }

    /// DEV-PRINCIPALS-1: a named principal never takes the developer's or the
    /// reviewer's name, holds at least one role, and is named once.
    #[test]
    fn invalid_named_principals_refuse() {
        for principals in [
            json!([{ "actor": "local:developer", "roles": ["planner"] }]),
            json!([{ "actor": "local:developer+reviewer", "roles": ["planner"] }]),
            json!([{ "actor": "developer", "roles": ["planner"] }]),
            json!([{ "actor": "reviewer", "roles": ["planner"] }]),
            json!([{ "actor": " ", "roles": ["planner"] }]),
            json!([{ "actor": "a", "roles": [] }]),
            json!([{ "actor": "a", "roles": ["planner", "planner"] }]),
            json!([{ "actor": "a", "roles": [""] }]),
            json!([{ "actor": "a", "roles": ["x"] }, { "actor": "a", "roles": ["y"] }]),
        ] {
            let refused = named_principals(&dev(&principals)).err();
            assert_eq!(
                refused.map(|failure| failure.code),
                Some("norn.project.invalid_manifest"),
                "{principals}"
            );
        }
        let many: Vec<Value> = (0..=named::MAX_PRINCIPALS)
            .map(|index| json!({ "actor": format!("p{index}"), "roles": ["r"] }))
            .collect();
        assert!(named_principals(&dev(&Value::Array(many))).is_err());
        assert!(
            serde_json::from_value::<ManifestDev>(json!({ "port": 0, "actor": "a", "principals": [{ "actor": "b", "roles": ["r"], "admin": true }] }))
                .is_err(),
            "unknown keys refuse"
        );
    }

    /// CONN-SECRET-ENV-1: the compile worker never inherits a vault KEK or
    /// an OAuth client variable; other variables pass through.
    #[test]
    fn the_worker_never_inherits_connection_secrets() {
        let (_dir, root) = project();
        let inherited = [
            concat!("NORN_", "VAULT_KEK"),
            concat!("NORN_", "VAULT_KEK_ID"),
            concat!("NORN_", "VAULT_KEKS_PREVIOUS"),
            "NORN_OAUTH_GITHUB_CLIENT_SECRET",
            "NORN_OAUTH_REDIRECT_URL",
            "NORN_SOURCE_ALLOWED_HOSTS",
            "PATH",
        ]
        .map(std::ffi::OsString::from);
        let command = worker_command("worker", &[], &root, inherited.into_iter());
        let mut removed: Vec<_> = command
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .map(|(name, _)| name.to_string_lossy().into_owned())
            .collect();
        removed.sort();
        assert_eq!(
            removed,
            [
                "NORN_OAUTH_GITHUB_CLIENT_SECRET",
                "NORN_OAUTH_REDIRECT_URL",
                concat!("NORN_", "VAULT_KEK"),
                concat!("NORN_", "VAULT_KEKS_PREVIOUS"),
                concat!("NORN_", "VAULT_KEK_ID"),
            ]
        );
    }

    #[test]
    fn the_cell_stays_inside_the_project() {
        let (_dir, root) = project();
        assert!(cell_path(&root, ".norn/cell.sqlite").is_ok());
        for cell in [
            "/tmp/other.sqlite",
            "../other.sqlite",
            ".norn/../../other.sqlite",
            ".",
            ".norn/.",
            "",
        ] {
            let refused = cell_path(&root, cell).err().unwrap();
            assert_eq!(refused.code, "norn.project.cell_outside", "{cell}");
        }
        // A directory inside the project that links out is outside too.
        let elsewhere = tempfile::tempdir().unwrap();
        symlink(elsewhere.path(), root.join("linked")).unwrap();
        // Nothing is created outside the project before the refusal.
        assert_eq!(
            cell_path(&root, "linked/sub/cell.sqlite")
                .err()
                .unwrap()
                .code,
            "norn.project.cell_outside"
        );
        assert!(!elsewhere.path().join("sub").exists());
        assert_eq!(
            cell_path(&root, "linked/cell.sqlite").err().unwrap().code,
            "norn.project.cell_outside"
        );
        // So is a cell file that is itself a symlink.
        std::fs::create_dir_all(root.join("cells")).unwrap();
        symlink(
            elsewhere.path().join("x.sqlite"),
            root.join("cells/cell.sqlite"),
        )
        .unwrap();
        assert_eq!(
            cell_path(&root, "cells/cell.sqlite").err().unwrap().code,
            "norn.project.cell_outside"
        );
    }

    #[test]
    fn the_token_file_is_owner_only_and_never_followed() {
        let (_dir, root) = project();
        let state = private_dir(&root).unwrap();
        let token = state.join("dev.token");
        // A new file is created 0600.
        write_private(&token, "a\n").unwrap();
        assert_eq!(
            std::fs::metadata(&token).unwrap().permissions().mode() & 0o777,
            0o600
        );
        // An existing world-readable file (say, from a checkout) is narrowed.
        std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_private(&token, "b\n").unwrap();
        assert_eq!(
            std::fs::metadata(&token).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(std::fs::read_to_string(&token).unwrap(), "b\n");
        // The file is replaced, never reused: a descriptor opened on the old
        // world-readable file does not see the new token.
        std::fs::set_permissions(&token, std::fs::Permissions::from_mode(0o644)).unwrap();
        let mut earlier_reader = File::open(&token).unwrap();
        write_private(&token, "secret\n").unwrap();
        let mut seen = String::new();
        std::io::Read::read_to_string(&mut earlier_reader, &mut seen).unwrap();
        assert_eq!(seen, "b\n");
        assert_eq!(std::fs::read_to_string(&token).unwrap(), "secret\n");
        // A symlinked token is refused and its target left alone.
        std::fs::remove_file(&token).unwrap();
        let target = root.join("victim");
        std::fs::write(&target, "keep").unwrap();
        symlink(&target, &token).unwrap();
        assert_eq!(
            write_private(&token, "c\n").err().unwrap().code,
            "norn.dev.unsafe_state"
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "keep");
    }

    #[test]
    fn a_symlinked_state_directory_is_refused() {
        let (_dir, root) = project();
        let elsewhere = tempfile::tempdir().unwrap();
        symlink(elsewhere.path(), root.join(".norn")).unwrap();
        assert_eq!(
            private_dir(&root).err().unwrap().code,
            "norn.dev.unsafe_state"
        );
    }

    #[test]
    fn the_lock_is_never_opened_through_a_symlink() {
        let (_dir, root) = project();
        let state = private_dir(&root).unwrap();
        let target = root.join("elsewhere.lock");
        symlink(&target, state.join("dev.lock")).unwrap();
        assert_eq!(lock(&state).err().unwrap().code, "norn.dev.io");
        assert!(!target.exists());
    }

    #[test]
    fn a_compiled_generation_is_never_lost_behind_another_line() {
        let (send, receive) = mpsc::channel();
        let mut pending = VecDeque::new();
        for event in [
            json!({ "event": "compiled", "gen": 1 }),
            json!({ "event": "compiled", "gen": 2 }),
            json!(1),
            json!(null),
        ] {
            send.send(event).unwrap();
        }
        // Other lines come out first, then only the newest generation.
        assert_eq!(newest(&receive, &mut pending), Some(json!(1)));
        assert_eq!(newest(&receive, &mut pending), Some(json!(null)));
        assert_eq!(
            newest(&receive, &mut pending),
            Some(json!({ "event": "compiled", "gen": 2 }))
        );
        assert_eq!(pending.len(), 0);
    }
}
