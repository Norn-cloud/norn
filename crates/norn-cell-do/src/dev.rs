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
//! - DEV-CONFINED-1: the cell and the session files stay inside the project:
//!   the manifest's cell is a relative path under the project root, and the
//!   token and descriptor are owner-only files never written through a symlink.
//!
//! Stability: the `--json` events and the `.norn/dev.json` descriptor
//! (`norn-dev/1`) are experimental, not a frozen contract (D84). The
//! descriptor and the `ready` event say so with `"stability": "experimental"`.
use std::collections::VecDeque;
use std::fmt::Write as _;
use std::fs::{File, OpenOptions};
use std::io::{BufRead as _, BufReader, Write as _};
use std::net::{IpAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use clap::Parser;
use norn_api::host::DevHost;
use norn_api::scope::CellScope;
use norn_cell::engine::NativeEngine;
use norn_cell::{CellStore, InstalledDefinitionSet, StoreOptions};
use serde::Deserialize;
use serde_json::{Value, json};

mod admit;
mod http;
mod policy;

use policy::{DevPrincipals, DevQuestions, LocalDeveloper};

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
#[must_use]
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
    let state = private_dir(&root)?;
    let _lock = lock(&state)?;
    let token = token()?;
    write_private(&state.join("dev.token"), &format!("{}\n", token.bearer))?;
    let scope = CellScope {
        org_id: manifest.scope.org_id.clone(),
        environment_id: manifest.scope.environment_id.clone(),
    };
    let cell_path = cell_path(&root, &manifest.cell)?;
    let serving: http::Serving = Arc::new(RwLock::new(reopen(
        &cell_path,
        &scope,
        &manifest.dev.actor,
    )?));
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
    let principals = DevPrincipals {
        bearer: token.bearer.clone(),
        actor: manifest.dev.actor.clone(),
    };
    let questions = DevQuestions {
        handle_key: token.handle_key,
        catalog: norn_checker::EffectiveCatalog::default(),
    };
    {
        let serving = Arc::clone(&serving);
        std::thread::spawn(move || http::serve(&listener, &serving, &principals, &questions));
    }
    let session = Session {
        root: &root,
        manifest: &manifest,
        scope: &scope,
        url: &url,
        started_at: now_ms(),
    };
    let active =
        active(&serving).map_err(|message| Failure::setup("norn.cell.unreadable", message))?;
    session.header(out, active.as_ref());
    if let Some(active) = &active {
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
        *pending = std::mem::take(pending)
            .into_iter()
            .enumerate()
            .filter(|(index, event)| *index == last || !is_compiled(event))
            .map(|(_, event)| event)
            .collect();
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
    #[allow(
        clippy::disallowed_methods,
        reason = "the dev host's one capability: running the project's compile worker"
    )]
    let mut child = Command::new(program)
        .args(arguments)
        .arg("--project")
        .arg(root)
        // The worker exits when this pipe closes, so it never outlives the host.
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
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

struct Session<'a> {
    root: &'a Path,
    manifest: &'a Manifest,
    scope: &'a CellScope,
    url: &'a str,
    started_at: u64,
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
        out.emit(
            &event,
            &format!(
                "✓ serving r{} · receipt {}…\n  API  {}/v1   token .norn/dev.token\nwatching sources",
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
        let admitted = match admit::admit(&artifacts, installed_at) {
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
        if current
            .as_ref()
            .is_some_and(|set| set.canonical_set == admitted.input.canonical_set)
        {
            out.emit(
                &json!({ "event": "unchanged", "gen": event.get("gen"), "sourceDigest": event.get("sourceDigest") }),
                "· unchanged (same definitions)",
            );
            return Ok(None);
        }
        let result = match current {
            Some(set) => {
                let guard = serving.read().map_err(|_poisoned| {
                    Failure::setup("norn.cell.unreadable", "serving state poisoned")
                })?;
                guard
                    .as_ref()
                    .map(|host| {
                        host.install_definitions(admitted.model, admitted.input, Some(set.version))
                    })
                    .ok_or_else(|| {
                        Failure::setup("norn.cell.unreadable", "the serving host disappeared")
                    })?
                    .map_err(|refusal| serde_json::to_value(&refusal).unwrap_or(Value::Null))
            }
            None => self.provision(serving, cell_path, admitted),
        };
        match result {
            Ok(installed) => Ok(Some(installed)),
            Err(refusal) => {
                Self::rejected(
                    out,
                    serving,
                    event,
                    "install",
                    std::slice::from_ref(&refusal),
                );
                Ok(None)
            }
        }
    }

    /// The first revision of a new cell.
    fn provision(
        &self,
        serving: &http::Serving,
        cell_path: &Path,
        admitted: admit::Admitted,
    ) -> Result<InstalledDefinitionSet, Value> {
        let refused = |message: String| json!({ "code": "defs.provision_refused", "severity": "error", "message": message });
        let engine = NativeEngine::open(cell_path, StoreOptions::default())
            .map_err(|error| refused(error.to_string()))?;
        let store = CellStore::provision_definitions(
            engine,
            admitted.model,
            &admitted.input,
            &self.scope.key(),
        )
        .map_err(|error| refused(error.to_string()))?;
        let host =
            DevHost::from_store(store, self.scope.clone(), |command| command.payload.clone())
                .map_err(|refusal| serde_json::to_value(&refusal).unwrap_or(Value::Null))?
                .with_authorizer(LocalDeveloper {
                    actor: self.manifest.dev.actor.clone(),
                });
        let installed = host
            .active_definitions()
            .map_err(|refusal| serde_json::to_value(&refusal).unwrap_or(Value::Null))?
            .ok_or_else(|| refused("the provisioned cell has no active revision".to_owned()))?;
        *serving
            .write()
            .map_err(|_poisoned| refused("serving state poisoned".to_owned()))? = Some(host);
        Ok(installed)
    }
}

fn active(serving: &http::Serving) -> Result<Option<InstalledDefinitionSet>, String> {
    let guard = serving
        .read()
        .map_err(|_poisoned| "serving state poisoned".to_owned())?;
    guard.as_ref().map_or(Ok(None), |host| {
        host.active_definitions().map_err(|refusal| refusal.message)
    })
}

/// HOST-REOPEN-1: an existing cell is reopened at its active revision with
/// the model rebuilt from its retained set; nothing is written. A missing
/// or empty file waits for the first admitted revision.
fn reopen(
    path: &Path,
    scope: &CellScope,
    actor: &str,
) -> Result<Option<DevHost<NativeEngine>>, Failure> {
    let unreadable = |error: String| {
        Failure::setup(
            "norn.cell.unreadable",
            format!("{}: {error}", path.display()),
        )
    };
    if !path.exists() {
        return Ok(None);
    }
    let engine = NativeEngine::open(path, StoreOptions::default())
        .map_err(|error| unreadable(error.to_string()))?;
    let Some(active) = CellStore::read_active_definitions(&engine)
        .map_err(|error| unreadable(error.to_string()))?
    else {
        return Ok(None);
    };
    let model = admit::retained_envelopes(&active.canonical_set)
        .and_then(|artifacts| admit::model_of(&artifacts))
        .map_err(unreadable)?;
    let store =
        CellStore::from_engine(engine, model).map_err(|error| unreadable(error.to_string()))?;
    let host = DevHost::from_store(store, scope.clone(), |command| command.payload.clone())
        .map_err(|refusal| Failure::setup("norn.cell.scope_mismatch", refusal.message))?
        .with_authorizer(LocalDeveloper {
            actor: actor.to_owned(),
        });
    Ok(Some(host))
}

/// One running session per project: `.norn/dev.lock` is held exclusively
/// for the life of the process.
fn lock(state: &Path) -> Result<File, Failure> {
    let path = state.join("dev.lock");
    let mut options = OpenOptions::new();
    options.create(true).truncate(false).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        // Never create or open the lock through a symlink.
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let file = options
        .open(&path)
        .map_err(|error| Failure::setup("norn.dev.io", format!("{}: {error}", path.display())))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(_held) => {
            let running = std::fs::read_to_string(state.join("dev.json"))
                .ok()
                .and_then(|text| serde_json::from_str::<Value>(&text).ok())
                .map(|session| {
                    format!(
                        " (pid {}, {})",
                        session.get("pid").and_then(Value::as_u64).unwrap_or(0),
                        session.get("url").and_then(Value::as_str).unwrap_or("?")
                    )
                })
                .unwrap_or_default();
            Err(Failure::setup(
                "norn.cell.locked",
                format!("norn dev is already running for this project{running}; stop it first"),
            ))
        }
    }
}

struct Token {
    bearer: String,
    handle_key: [u8; 32],
}

fn token() -> Result<Token, Failure> {
    let mut bytes = [0_u8; 64];
    getrandom::fill(&mut bytes)
        .map_err(|error| Failure::setup("norn.dev.io", format!("no randomness: {error}")))?;
    let (bearer, key) = bytes.split_at(32);
    let mut handle_key = [0_u8; 32];
    handle_key.copy_from_slice(key);
    let hex = bearer
        .iter()
        .fold(String::with_capacity(64), |mut hex, byte| {
            let _written = write!(hex, "{byte:02x}");
            hex
        });
    Ok(Token {
        bearer: hex,
        handle_key,
    })
}

fn is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink())
}

/// The project's `.norn` state directory, created owner-only. A symlinked
/// `.norn` refuses: the session token must never land outside the project.
fn private_dir(root: &Path) -> Result<PathBuf, Failure> {
    let state = root.join(".norn");
    if is_symlink(&state) {
        return Err(Failure::setup(
            "norn.dev.unsafe_state",
            format!(
                "{} is a symlink; norn dev keeps its session token only in a real .norn directory",
                state.display()
            ),
        ));
    }
    std::fs::create_dir_all(&state)
        .map_err(|error| Failure::setup("norn.dev.io", format!("{}: {error}", state.display())))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o700)).map_err(
            |error| Failure::setup("norn.dev.io", format!("{}: {error}", state.display())),
        )?;
    }
    Ok(state)
}

/// The manifest's cell, which must be a relative path inside the project:
/// no absolute path, no `..`, and no symlinked directory leading out of it.
/// `norn dev` grants everything to the local developer, so it must never be
/// pointable at another cell on the machine.
fn cell_path(root: &Path, cell: &str) -> Result<PathBuf, Failure> {
    use std::path::Component;
    let outside = || {
        Failure::setup(
            "norn.project.cell_outside",
            format!("norn.json cell {cell:?} must be a relative path inside the project"),
        )
    };
    let relative = Path::new(cell);
    if relative.as_os_str().is_empty()
        || !relative
            .components()
            .all(|part| matches!(part, Component::Normal(_) | Component::CurDir))
    {
        return Err(outside());
    }
    let path = root.join(relative);
    let parent = path.parent().ok_or_else(outside)?;
    // Nothing is created until the deepest directory that already exists is
    // known to be inside the project: a symlinked ancestor leads out of it.
    let existing = parent
        .ancestors()
        .find(|ancestor| ancestor.exists())
        .ok_or_else(outside)?;
    let within = |directory: &Path| -> Result<bool, Failure> {
        directory
            .canonicalize()
            .map(|real| real.starts_with(root))
            .map_err(|error| {
                Failure::setup("norn.dev.io", format!("{}: {error}", directory.display()))
            })
    };
    if !within(existing)? {
        return Err(outside());
    }
    std::fs::create_dir_all(parent)
        .map_err(|error| Failure::setup("norn.dev.io", format!("{}: {error}", parent.display())))?;
    if !within(parent)? || is_symlink(&path) {
        return Err(outside());
    }
    Ok(path)
}

/// Write a session file readable by the owner only. The file is always a
/// new one: an existing file is unlinked first, so a descriptor someone opened
/// on an older, wider file never sees the new contents, and the file is
/// created exclusively (`O_EXCL`, `O_NOFOLLOW`, mode 0600), never through a
/// symlink.
fn write_private(path: &Path, text: &str) -> Result<(), Failure> {
    let failed = |error: std::io::Error| {
        Failure::setup("norn.dev.io", format!("{}: {error}", path.display()))
    };
    if is_symlink(path) {
        return Err(Failure::setup(
            "norn.dev.unsafe_state",
            format!(
                "{} is a symlink; norn dev refuses to write a session file through it",
                path.display()
            ),
        ));
    }
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(failed(error)),
    }
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    options
        .open(path)
        .and_then(|mut file| file.write_all(text.as_bytes()))
        .map_err(failed)
}

#[cfg(all(test, unix))]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "dev-host session-file and queue tests"
)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    fn project() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        (dir, root)
    }

    #[test]
    fn the_cell_stays_inside_the_project() {
        let (_dir, root) = project();
        assert!(cell_path(&root, ".norn/cell.sqlite").is_ok());
        for cell in [
            "/tmp/other.sqlite",
            "../other.sqlite",
            ".norn/../../other.sqlite",
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
        let mut stale = File::open(&token).unwrap();
        write_private(&token, "secret\n").unwrap();
        let mut seen = String::new();
        std::io::Read::read_to_string(&mut stale, &mut seen).unwrap();
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
        assert!(pending.is_empty());
    }
}
