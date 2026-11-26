//! `norn cell plan|apply`: change a running hosted cell's definitions
//! (HOST-INSTALL-1) through its governed routes. The cell decides: it admits
//! the pack over its active set, refuses with the checker's findings, and
//! installs only for an owner or admin. This client reads the pack, sends it,
//! and prints the cell's answer.
use clap::{Args, Subcommand};
use serde_json::{Value, json};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The largest pack this client sends, as the cell bounds it.
const MAX_BODY: usize = 1 << 20;
/// [`MAX_BODY`] as a read budget across every file of a pack.
const MAX_FILE: u64 = 1 << 20;

#[derive(Debug, Args)]
pub(super) struct CellArgs {
    #[command(subcommand)]
    command: CellCommand,
}

#[derive(Debug, Args)]
struct Target {
    /// The cell's base URL: the host itself, or Studio's proxy
    /// (for example <https://studio.example/api/host>).
    #[arg(long)]
    url: String,
    #[arg(long)]
    org: String,
    #[arg(long)]
    env: String,
    /// Environment variable holding a host bearer token; never a token argument.
    #[arg(long, conflicts_with = "email")]
    token_env: Option<String>,
    /// Sign in to Studio as this user (with `--password-env`), and call the
    /// cell through Studio's proxy as that user.
    #[arg(long, requires = "password_env")]
    email: Option<String>,
    /// Environment variable holding the Studio password; never a password argument.
    #[arg(long, requires = "email")]
    password_env: Option<String>,
    /// Print the cell's JSON documents instead of the summary.
    #[arg(long)]
    json: bool,
    /// Artifact JSON files, or directories of them (every `*.json`, by name).
    #[arg(required = true)]
    paths: Vec<PathBuf>,
}

#[derive(Debug, Subcommand)]
enum CellCommand {
    /// Show what installing the pack over the cell's active definitions
    /// would change; the cell checks it and writes nothing.
    Plan(Target),
    /// Install the pack over the cell's active definitions as the next
    /// revision: planned first, then applied against the planned revision.
    Apply {
        #[command(flatten)]
        target: Target,
        /// The revision the change was planned against; without it the
        /// command plans first and applies against that plan.
        #[arg(long)]
        expected_version: Option<u64>,
    },
}

type Artifact = Box<serde_json::value::RawValue>;

/// The pack's artifacts, each file's exact JSON text, in path order. Read
/// once per command: `apply` plans and applies this one snapshot. At most
/// [`MAX_FILE`] bytes (plus one, to detect excess) are read across the
/// whole pack, so many small files cannot exhaust memory either.
fn artifacts(paths: &[PathBuf]) -> Result<Vec<Artifact>, String> {
    let mut budget = MAX_FILE;
    let mut read = Vec::new();
    for path in paths {
        read_path(path, &mut budget, &mut read)?;
    }
    if read.is_empty() {
        return Err("no artifact files".to_owned());
    }
    Ok(read)
}

/// Collect `*.json` names from an open pack directory within `name_budget` bytes.
#[cfg(unix)]
fn collect_json_names(
    dir: &std::fs::File,
    shown: &Path,
    name_budget: usize,
) -> Result<Vec<std::ffi::CString>, String> {
    use std::ffi::{CString, OsStr};
    use std::os::unix::ffi::OsStrExt as _;
    let err = |e| format!("{}: {e}", shown.display());
    let (mut names, mut retained) = (Vec::new(), 0usize);
    for entry in rustix::fs::Dir::read_from(dir).map_err(err)? {
        let name = entry.map_err(err)?.file_name().to_owned();
        if Path::new(OsStr::from_bytes(name.to_bytes())).extension() != Some(OsStr::new("json")) {
            continue;
        }
        let bytes = name.as_bytes().len().saturating_add(size_of::<CString>());
        retained = retained
            .checked_add(bytes)
            .filter(|&total| total <= name_budget)
            .ok_or_else(|| {
                format!(
                    "{}: directory listing exceeds {name_budget} bytes",
                    shown.display()
                )
            })?;
        names.push(name);
    }
    names.sort();
    Ok(names)
}

/// Read one named path (regular file or directory's `*.json` children) via one
/// no-follow open and directory-relative child opens.
#[cfg(unix)]
fn read_path(path: &Path, budget: &mut u64, read: &mut Vec<Artifact>) -> Result<(), String> {
    use std::os::unix::ffi::OsStrExt as _;
    let shown = path.display();
    let raw = path.as_os_str().as_bytes();
    let must_be_dir = raw.ends_with(b"/") || raw.ends_with(b"/.");
    // Strip trailing `/` and `/.` so `O_NOFOLLOW` applies to the named path.
    let target: PathBuf = path.components().collect();
    let opened = open_no_follow(rustix::fs::CWD, &target).map_err(|e| format!("{shown}: {e}"))?;
    let kind = opened
        .metadata()
        .map_err(|e| format!("{shown}: {e}"))?
        .file_type();
    if kind.is_file() {
        if must_be_dir {
            return Err(format!("{shown}: not a directory"));
        }
        read.push(artifact(path, opened, budget)?);
        return Ok(());
    }
    if !kind.is_dir() {
        return Err(format!("{shown}: not a regular file or directory"));
    }
    for name in collect_json_names(&opened, path, MAX_BODY)? {
        let child = path.join(std::ffi::OsStr::from_bytes(name.to_bytes()));
        let file = open_no_follow(&opened, name.as_c_str())
            .map_err(|e| format!("{}: {e}", child.display()))?;
        let regular = file
            .metadata()
            .map_err(|e| format!("{}: {e}", child.display()))?
            .is_file();
        if !regular {
            return Err(format!("{}: not a regular file", child.display()));
        }
        read.push(artifact(&child, file, budget)?);
    }
    Ok(())
}

/// Without a no-follow, directory-relative open this client cannot read a
/// pack safely, so it refuses rather than follow links.
#[cfg(not(unix))]
fn read_path(path: &Path, _budget: &mut u64, _read: &mut Vec<Artifact>) -> Result<(), String> {
    Err(format!(
        "{}: pack reads need no-follow opens, which this client supports only on Unix",
        path.display()
    ))
}

/// Open `name` under `dir` read-only: never through a final link
/// (`O_NOFOLLOW`), and never blocking on a FIFO (`O_NONBLOCK`).
#[cfg(unix)]
fn open_no_follow<P: rustix::path::Arg>(
    dir: impl std::os::fd::AsFd,
    name: P,
) -> Result<std::fs::File, String> {
    use rustix::fs::{Mode, OFlags};
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    rustix::fs::openat(dir, name, flags, Mode::empty())
        .map(std::fs::File::from)
        .map_err(|e| {
            if e == rustix::io::Errno::LOOP {
                "links are refused".to_owned()
            } else {
                e.to_string()
            }
        })
}

fn read_file_bounded(
    path: &Path,
    file: std::fs::File,
    limit: u64,
    label: &str,
) -> Result<String, String> {
    let shown = path.display();
    let mut text = String::new();
    file.take(limit.saturating_add(1))
        .read_to_string(&mut text)
        .map_err(|e| format!("{shown}: {e}"))?;
    let used = u64::try_from(text.len()).map_err(|e| format!("{shown}: {e}"))?;
    if used > limit {
        return Err(format!("{shown}: exceeds the {label}"));
    }
    Ok(text)
}

/// Read one regular file without following final symlinks or blocking on FIFOs.
#[cfg(unix)]
pub(super) fn read_regular_file_bounded(
    path: &Path,
    limit: u64,
    label: &str,
) -> Result<String, String> {
    use std::os::unix::ffi::OsStrExt as _;
    let shown = path.display();
    let raw = path.as_os_str().as_bytes();
    if raw.ends_with(b"/") || raw.ends_with(b"/.") {
        return Err(format!("{shown}: not a regular file"));
    }
    let target: PathBuf = path.components().collect();
    let opened = open_no_follow(rustix::fs::CWD, &target).map_err(|e| format!("{shown}: {e}"))?;
    let regular = opened
        .metadata()
        .map_err(|e| format!("{shown}: {e}"))?
        .is_file();
    if !regular {
        return Err(format!("{shown}: not a regular file"));
    }
    read_file_bounded(path, opened, limit, label)
}

/// Read one regular file without following symlinks (non-Unix fallback).
#[cfg(not(unix))]
pub(super) fn read_regular_file_bounded(
    path: &Path,
    limit: u64,
    label: &str,
) -> Result<String, String> {
    let shown = path.display();
    let meta = std::fs::symlink_metadata(path).map_err(|e| format!("{shown}: {e}"))?;
    if meta.file_type().is_symlink() {
        return Err(format!("{shown}: links are refused"));
    }
    if !meta.is_file() {
        return Err(format!("{shown}: not a regular file"));
    }
    let file = std::fs::File::open(path).map_err(|e| format!("{shown}: {e}"))?;
    if !file
        .metadata()
        .map_err(|e| format!("{shown}: {e}"))?
        .is_file()
    {
        return Err(format!("{shown}: not a regular file"));
    }
    read_file_bounded(path, file, limit, label)
}

/// Read every `*.json` regular file directly under `dir` in sorted name order,
/// enforcing `total_limit` across the whole directory without following links.
#[cfg(unix)]
pub(super) fn read_json_dir_bounded(
    dir: &Path,
    total_limit: u64,
    label: &str,
) -> Result<Vec<String>, String> {
    let shown = dir.display();
    let target: PathBuf = dir.components().collect();
    let opened = open_no_follow(rustix::fs::CWD, &target).map_err(|e| format!("{shown}: {e}"))?;
    if !opened
        .metadata()
        .map_err(|e| format!("{shown}: {e}"))?
        .is_dir()
    {
        return Err(format!("{shown}: not a directory"));
    }
    read_open_json_dir(&opened, dir, total_limit, label)
}

#[cfg(unix)]
fn read_open_json_dir(
    opened: &std::fs::File,
    dir: &Path,
    total_limit: u64,
    label: &str,
) -> Result<Vec<String>, String> {
    use std::os::unix::ffi::OsStrExt as _;
    let mut budget = total_limit;
    let mut documents = Vec::new();
    for name in collect_json_names(opened, dir, MAX_BODY)? {
        let child = dir.join(std::ffi::OsStr::from_bytes(name.to_bytes()));
        let file = open_no_follow(opened, name.as_c_str())
            .map_err(|e| format!("{}: {e}", child.display()))?;
        let regular = file
            .metadata()
            .map_err(|e| format!("{}: {e}", child.display()))?
            .is_file();
        if !regular {
            return Err(format!("{}: not a regular file", child.display()));
        }
        let text = read_file_bounded(&child, file, budget, label)?;
        let used = u64::try_from(text.len()).map_err(|e| format!("{}: {e}", child.display()))?;
        budget = budget
            .checked_sub(used)
            .ok_or_else(|| format!("definition set exceeds the {label}"))?;
        documents.push(text);
    }
    Ok(documents)
}

#[cfg(not(unix))]
pub(super) fn read_json_dir_bounded(
    dir: &Path,
    total_limit: u64,
    label: &str,
) -> Result<Vec<String>, String> {
    let shown = dir.display();
    let meta = std::fs::symlink_metadata(dir).map_err(|e| format!("{shown}: {e}"))?;
    if meta.file_type().is_symlink() {
        return Err(format!("{shown}: links are refused"));
    }
    if !meta.is_dir() {
        return Err(format!("{shown}: not a directory"));
    }
    let mut paths = std::fs::read_dir(dir)
        .map_err(|e| format!("{shown}: {e}"))?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("{shown}: {e}"))?;
    paths.sort();
    let mut budget = total_limit;
    let mut documents = Vec::new();
    for path in paths.into_iter().filter(|path| {
        path.extension()
            .is_some_and(|extension| extension == "json")
    }) {
        let text = read_regular_file_bounded(&path, budget, label)?;
        let used = u64::try_from(text.len()).map_err(|e| format!("{}: {e}", path.display()))?;
        budget = budget
            .checked_sub(used)
            .ok_or_else(|| format!("definition set exceeds the {label}"))?;
        documents.push(text);
    }
    Ok(documents)
}

/// Read either a single JSON file or a directory of `*.json` files wrapped as a JSON array.
#[cfg(unix)]
pub(super) fn read_file_or_json_dir_bounded(
    path: &Path,
    total_limit: u64,
    label: &str,
) -> Result<String, String> {
    use std::os::unix::ffi::OsStrExt as _;
    let shown = path.display();
    let raw = path.as_os_str().as_bytes();
    let must_be_dir = raw.ends_with(b"/") || raw.ends_with(b"/.");
    let target: PathBuf = path.components().collect();
    let opened = open_no_follow(rustix::fs::CWD, &target).map_err(|e| format!("{shown}: {e}"))?;
    let kind = opened
        .metadata()
        .map_err(|e| format!("{shown}: {e}"))?
        .file_type();
    if kind.is_file() {
        if must_be_dir {
            return Err(format!("{shown}: not a directory"));
        }
        return read_file_bounded(path, opened, total_limit, label);
    }
    if !kind.is_dir() {
        return Err(format!("{shown}: not a regular file or directory"));
    }
    let documents = read_open_json_dir(&opened, path, total_limit, label)?;
    Ok(format!("[{}]", documents.join(",")))
}

#[cfg(not(unix))]
pub(super) fn read_file_or_json_dir_bounded(
    path: &Path,
    total_limit: u64,
    label: &str,
) -> Result<String, String> {
    let shown = path.display();
    let meta = std::fs::symlink_metadata(path).map_err(|e| format!("{shown}: {e}"))?;
    if meta.file_type().is_symlink() {
        return Err(format!("{shown}: links are refused"));
    }
    if meta.is_file() {
        return read_regular_file_bounded(path, total_limit, label);
    }
    if !meta.is_dir() {
        return Err(format!("{shown}: not a regular file or directory"));
    }
    let documents = read_json_dir_bounded(path, total_limit, label)?;
    Ok(format!("[{}]", documents.join(",")))
}

/// Read an HTTP response body up to `limit` bytes, failing explicitly if exceeded.
pub(super) fn read_bounded_response(
    reader: impl std::io::Read,
    limit: u64,
    label: &str,
) -> Result<String, String> {
    let mut text = String::new();
    reader
        .take(limit.saturating_add(1))
        .read_to_string(&mut text)
        .map_err(|_error| format!("{label} could not be read"))?;
    let used = u64::try_from(text.len()).map_err(|_error| format!("{label} exceeds size limit"))?;
    if used > limit {
        return Err(format!("{label} exceeds size limit"));
    }
    Ok(text)
}

/// One artifact's exact text, charged against the pack's remaining budget
/// while it is read: never more than the budget plus one byte.
#[cfg(unix)]
fn artifact(path: &Path, file: std::fs::File, budget: &mut u64) -> Result<Artifact, String> {
    let shown = path.display();
    let mut text = String::new();
    file.take(budget.saturating_add(1))
        .read_to_string(&mut text)
        .map_err(|e| format!("{shown}: {e}"))?;
    let used = u64::try_from(text.len()).map_err(|e| format!("{shown}: {e}"))?;
    *budget = budget
        .checked_sub(used)
        .ok_or_else(|| format!("{shown}: the pack exceeds the cell's 1 MiB bound"))?;
    serde_json::value::RawValue::from_string(text).map_err(|e| format!("{shown}: not JSON: {e}"))
}

/// A URL parsed once: scheme, host and origin all come from this one parse,
/// and it carries no userinfo, so `http://127.0.0.1@evil.example` names the
/// host it actually reaches (`evil.example`) and is refused outright.
pub(super) struct Endpoint {
    uri: ureq::http::Uri,
}

impl Endpoint {
    /// Parse `url`: absolute, HTTP or HTTPS, with a host and no userinfo.
    /// Errors never echo the URL, which may hold a secret.
    pub(super) fn parse(url: &str) -> Result<Self, String> {
        // `Uri` drops a fragment silently; refuse it before parsing.
        if url.contains('#') {
            return Err("URL must not contain a fragment".into());
        }
        let uri: ureq::http::Uri = url.parse().map_err(|_e| "URL is not valid")?;
        let authority = uri.authority().ok_or("URL must be absolute")?;
        if authority.as_str().contains('@') {
            return Err("URL must not contain credentials".into());
        }
        check_authority(authority)?;
        let endpoint = Self { uri };
        if !endpoint.is_https() && !endpoint.is_http() {
            return Err("URL must use http or https".into());
        }
        Ok(endpoint)
    }

    pub(super) fn is_https(&self) -> bool {
        self.uri
            .scheme_str()
            .is_some_and(|scheme| scheme.eq_ignore_ascii_case("https"))
    }

    fn is_http(&self) -> bool {
        self.uri
            .scheme_str()
            .is_some_and(|scheme| scheme.eq_ignore_ascii_case("http"))
    }

    /// The host, without an IPv6 literal's brackets.
    fn host(&self) -> &str {
        let host = self.uri.host().unwrap_or_default();
        host.strip_prefix('[')
            .and_then(|inner| inner.strip_suffix(']'))
            .unwrap_or(host)
    }

    pub(super) fn has_query(&self) -> bool {
        self.uri.query().is_some()
    }

    /// Plain HTTP to a loopback address or `localhost`.
    pub(super) fn is_loopback_http(&self) -> bool {
        let host = self.host();
        self.is_http()
            && (host.eq_ignore_ascii_case("localhost")
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback()))
    }

    /// Whether this endpoint may carry credentials: HTTPS, or plain HTTP to
    /// a loopback address or `localhost`.
    fn carries_credentials(&self) -> bool {
        self.is_https() || self.is_loopback_http()
    }

    /// The agent configuration for this endpoint: plain HTTP to loopback is
    /// reached directly, never through an environment proxy (`HTTP_PROXY`),
    /// which may be remote and would read its credentials in plaintext.
    pub(super) fn configure(
        &self,
        builder: ureq::config::ConfigBuilder<ureq::typestate::AgentScope>,
    ) -> ureq::config::ConfigBuilder<ureq::typestate::AgentScope> {
        if self.is_loopback_http() {
            builder.proxy(None)
        } else {
            builder
        }
    }

    /// `scheme://host[:port]`, where Studio's sign-in lives.
    fn origin(&self) -> String {
        format!(
            "{}://{}",
            self.uri.scheme_str().unwrap_or_default(),
            self.uri
                .authority()
                .map_or("", ureq::http::uri::Authority::as_str)
        )
    }
}

struct Client {
    agent: ureq::Agent,
    base: String,
    org: String,
    env: String,
    authorization: Option<String>,
    cookie: Option<String>,
    origin: String,
}

/// The authority's host is well formed (an IPv6 literal parses) and any
/// `:port` is a decimal `u16`: a garbage port is refused, never read as the
/// scheme's default. Userinfo is refused before this runs.
fn check_authority(authority: &ureq::http::uri::Authority) -> Result<(), String> {
    let host = authority.host();
    if host.is_empty() {
        return Err("URL names no host".into());
    }
    if let Some(literal) = host.strip_prefix('[') {
        let valid = literal
            .strip_suffix(']')
            .is_some_and(|inner| inner.parse::<std::net::Ipv6Addr>().is_ok());
        if !valid {
            return Err("URL has a malformed IPv6 host".into());
        }
    } else if host.contains(['[', ']']) {
        // `http` admits balanced brackets inside a name (`host[x]`).
        return Err("URL has a malformed host".into());
    }
    let rest = authority
        .as_str()
        .strip_prefix(host)
        .ok_or("URL has a malformed authority")?;
    if rest.is_empty() {
        return Ok(());
    }
    let port = rest
        .strip_prefix(':')
        .filter(|digits| !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit()))
        .and_then(|digits| digits.parse::<u16>().ok());
    if port.is_some() && port == authority.port_u16() {
        Ok(())
    } else {
        Err("URL has an invalid port".into())
    }
}

impl Client {
    fn new(target: &Target) -> Result<Self, String> {
        let base = target.url.trim_end_matches('/').to_owned();
        let endpoint = Endpoint::parse(&base)?;
        if endpoint.has_query() {
            // API paths are appended to the base; a query there would swallow them.
            return Err("URL must not contain a query".into());
        }
        let agent: ureq::Agent = endpoint
            .configure(ureq::Agent::config_builder())
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_global(Some(Duration::from_secs(120)))
            .build()
            .into();
        if (target.token_env.is_some() || target.email.is_some()) && !endpoint.carries_credentials()
        {
            return Err("credentials are sent only over HTTPS (or HTTP to loopback)".into());
        }
        let authorization = match &target.token_env {
            Some(variable) => Some(format!(
                "Bearer {}",
                std::env::var(variable)
                    .map_err(|_e| "token environment variable is unavailable")?
            )),
            None => None,
        };
        let mut client = Self {
            agent,
            base,
            org: target.org.clone(),
            env: target.env.clone(),
            authorization,
            cookie: None,
            origin: endpoint.origin(),
        };
        if let (Some(email), Some(variable)) = (&target.email, &target.password_env) {
            let password = std::env::var(variable)
                .map_err(|_e| "password environment variable is unavailable")?;
            client.cookie = Some(client.sign_in(email, &password)?);
        }
        Ok(client)
    }

    /// Sign in to Studio with email and password; the session cookie.
    fn sign_in(&self, email: &str, password: &str) -> Result<String, String> {
        let body = json!({ "email": email, "password": password }).to_string();
        let response = self
            .agent
            .post(format!("{}/api/auth/sign-in/email", self.origin))
            .header("content-type", "application/json")
            .header("origin", &self.origin)
            .send(body.as_bytes())
            .map_err(|_e| "the Studio sign-in request failed")?;
        if !response.status().is_success() {
            return Err(format!("Studio sign-in refused ({})", response.status()));
        }
        let cookie_header = collect_cookies(response.headers())
            .ok_or_else(|| "Studio sign-in returned no session".to_owned())?;
        Ok(cookie_header)
    }

    /// POST `body` to `path`; the status and the JSON document answered.
    fn post(&self, path: &str, body: &str) -> Result<(u16, Value), String> {
        let url = format!(
            "{}{path}?orgId={}&environmentId={}",
            self.base,
            encode(&self.org),
            encode(&self.env)
        );
        let mut call = self
            .agent
            .post(&url)
            .header("content-type", "application/json")
            .header("accept", "application/json");
        if let Some(authorization) = &self.authorization {
            call = call.header("authorization", authorization);
        }
        if let Some(cookie) = &self.cookie {
            // Studio accepts a state change only from its own origin.
            call = call.header("cookie", cookie).header("origin", &self.origin);
        }
        let mut response = call
            .send(body.as_bytes())
            .map_err(|_e| format!("the request to {path} failed"))?;
        let status = response.status().as_u16();
        let text = read_bounded_response(
            response.body_mut().as_reader(),
            16 * 1024 * 1024,
            &format!("{path} response"),
        )?;
        let value = serde_json::from_str(&text)
            .map_err(|_e| format!("{path} answered {status} with a non-JSON body"))?;
        Ok((status, value))
    }
}

/// Deduplicate `Set-Cookie` name=value pairs by cookie name (last-wins, trimmed).
fn collect_cookies(headers: &ureq::http::HeaderMap) -> Option<String> {
    let mut cookies: Vec<(String, String)> = Vec::new();
    for header in headers.get_all("set-cookie") {
        let Ok(raw) = header.to_str() else {
            continue;
        };
        let Some(pair) = raw.split(';').next().map(str::trim) else {
            continue;
        };
        let Some((name, value)) = pair.split_once('=') else {
            continue;
        };
        let name = name.trim();
        let value = value.trim();
        if name.is_empty() {
            continue;
        }
        if let Some(existing) = cookies.iter_mut().find(|(k, _)| k == name) {
            value.clone_into(&mut existing.1);
        } else {
            cookies.push((name.to_owned(), value.to_owned()));
        }
    }
    if cookies.is_empty() {
        return None;
    }
    let joined = cookies
        .into_iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("; ");
    Some(joined)
}

pub(super) fn encode(value: &str) -> String {
    let hex = |nibble: u8| {
        char::from(if nibble < 10 {
            b'0'.saturating_add(nibble)
        } else {
            b'A'.saturating_add(nibble.saturating_sub(10))
        })
    };
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(char::from(byte));
        } else {
            out.push('%');
            out.push(hex(byte >> 4));
            out.push(hex(byte & 0x0F));
        }
    }
    out
}

fn text(value: &Value, key: &str) -> String {
    value
        .get(key)
        .map_or_default(|v| v.as_str().map_or_else(|| v.to_string(), str::to_owned))
}

/// Append one line to `out`.
fn line(out: &mut String, text: &str) {
    out.push_str(text);
    out.push('\n');
}

fn plan_summary(plan: &Value) -> String {
    let mut out = String::new();
    line(
        &mut out,
        &format!(
            "plan over revision {} -> {}{}",
            text(plan, "baseVersion"),
            text(plan, "nextVersion"),
            if plan.get("unchanged") == Some(&Value::Bool(true)) {
                " (unchanged: the pack is already installed)"
            } else {
                ""
            }
        ),
    );
    for (key, mark) in [("added", "+"), ("changed", "~")] {
        for artifact in plan
            .get(key)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let from = artifact
                .get("fromVersion")
                .and_then(Value::as_str)
                .map_or_default(|from| format!("{from} -> "));
            line(
                &mut out,
                &format!(
                    "  {mark} {} ({} {from}{})",
                    text(artifact, "artifactId"),
                    text(artifact, "kind"),
                    text(artifact, "toVersion")
                ),
            );
        }
    }
    for id in plan
        .get("same")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        line(
            &mut out,
            &format!("  = {}", id.as_str().unwrap_or_default()),
        );
    }
    let types: Vec<&str> = plan
        .get("newObjectTypes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    if !types.is_empty() {
        line(&mut out, &format!("  new types: {}", types.join(", ")));
    }
    line(
        &mut out,
        &format!(
            "  checked {} stored object(s) under the new laws",
            text(plan, "checkedObjects")
        ),
    );
    diagnostics(&mut out, plan);
    out.trim_end().to_owned()
}

fn diagnostics(out: &mut String, document: &Value) {
    for finding in document
        .get("diagnostics")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let at = finding
            .pointer("/span/pointer")
            .and_then(Value::as_str)
            .map_or_default(|pointer| format!(" at {pointer}"));
        line(
            out,
            &format!(
                "  {} {}{at}: {}",
                text(finding, "severity"),
                text(finding, "code"),
                text(finding, "message")
            ),
        );
    }
}

fn refusal_summary(status: u16, refusal: &Value) -> String {
    let mut out = format!(
        "refused ({status}) {}: {}\n  {}\n",
        text(refusal, "code"),
        text(refusal, "message"),
        text(refusal, "explanation")
    );
    diagnostics(&mut out, refusal);
    out.trim_end().to_owned()
}

fn answer(
    json_out: bool,
    status: u16,
    document: &Value,
    summary: impl Fn(&Value) -> String,
) -> (String, bool) {
    let accepted = (200..300).contains(&status);
    let shown = if json_out {
        document.to_string()
    } else if accepted {
        summary(document)
    } else {
        refusal_summary(status, document)
    };
    (shown, accepted)
}

fn body(artifacts: &[Artifact], expected_version: Option<u64>) -> Result<String, String> {
    // Each artifact's exact text is spliced in as read: never through
    // `Value`, which would round a high-precision number.
    let artifacts = serde_json::to_string(artifacts).map_err(|e| e.to_string())?;
    let body = match expected_version {
        Some(version) => format!(r#"{{"artifacts":{artifacts},"expectedVersion":{version}}}"#),
        None => format!(r#"{{"artifacts":{artifacts}}}"#),
    };
    if body.len() > MAX_BODY {
        return Err("the pack exceeds the cell's 1 MiB bound".into());
    }
    Ok(body)
}

pub(super) fn execute(args: &CellArgs) -> Result<(String, bool), String> {
    match &args.command {
        CellCommand::Plan(target) => {
            let pack = artifacts(&target.paths)?;
            let client = Client::new(target)?;
            let (status, plan) =
                client.post(norn_api::cell_definitions::PLAN_PATH, &body(&pack, None)?)?;
            Ok(answer(target.json, status, &plan, plan_summary))
        }
        CellCommand::Apply {
            target,
            expected_version,
        } => {
            // One snapshot: the plan shown is exactly the pack applied.
            let pack = artifacts(&target.paths)?;
            let client = Client::new(target)?;
            let mut shown = String::new();
            let expected = if let Some(version) = expected_version {
                *version
            } else {
                let (status, plan) =
                    client.post(norn_api::cell_definitions::PLAN_PATH, &body(&pack, None)?)?;
                let (text, accepted) = answer(target.json, status, &plan, plan_summary);
                if !accepted {
                    return Ok((text, false));
                }
                shown.push_str(&text);
                shown.push('\n');
                plan.get("baseVersion")
                    .and_then(Value::as_u64)
                    .ok_or("the plan names no base revision")?
            };
            let (status, applied) = client.post(
                norn_api::cell_definitions::APPLY_PATH,
                &body(&pack, Some(expected))?,
            )?;
            let (text, accepted) = answer(target.json, status, &applied, |applied| {
                format!(
                    "{} revision {} over {} (receipt sha256 {}, commit {})",
                    if text(applied, "outcome") == "unchanged" {
                        "unchanged: already at"
                    } else {
                        "installed"
                    },
                    text(applied, "definitionsVersion"),
                    text(applied, "previousVersion"),
                    text(applied, "receiptSha256"),
                    text(applied, "commitSeq"),
                )
            });
            shown.push_str(&text);
            Ok((shown, accepted))
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "fixture assertions")]
mod tests {
    use super::*;
    #[cfg(unix)]
    use std::path::Path;

    #[cfg(unix)]
    #[test]
    fn a_pack_directory_is_read_in_name_order_and_kept_exact() {
        let pack =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/packs/batches/artifacts");
        let read = artifacts(&[pack]).unwrap();
        let ids: Vec<String> = read
            .iter()
            .map(|raw| text(&serde_json::from_str(raw.get()).unwrap(), "artifactId"))
            .collect();
        assert_eq!(
            ids,
            [
                "lab.Batch.entity",
                "lab.batch.access.policy",
                "lab.batch.mass_conserved.law",
                "lab.batch.record.action"
            ]
        );
    }

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    /// A fresh directory under the system temp dir, unique to this test.
    #[cfg(unix)]
    fn scratch(name: &str) -> Result<PathBuf, std::io::Error> {
        let dir = std::env::temp_dir().join(format!("norn-cli-cell-{}-{name}", std::process::id()));
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
    }

    #[derive(Clone, Copy)]
    enum Credential<'a> {
        Token,
        SignIn(&'a str),
    }

    /// A target for `url`, carrying a token or a Studio sign-in when asked.
    fn target(url: &str, credential: Option<Credential<'_>>) -> Target {
        let email = credential.and_then(|credential| match credential {
            Credential::SignIn(email) => Some(email.to_owned()),
            Credential::Token => None,
        });
        Target {
            url: url.to_owned(),
            org: "org".to_owned(),
            env: "env".to_owned(),
            token_env: matches!(credential, Some(Credential::Token))
                .then(|| "NORN_TEST_UNSET_TOKEN".to_owned()),
            password_env: email
                .as_ref()
                .map(|_email| "NORN_TEST_UNSET_PASSWORD".to_owned()),
            email,
            json: false,
            paths: Vec::new(),
        }
    }

    fn admits(url: &str) -> bool {
        Endpoint::parse(url).is_ok_and(|endpoint| endpoint.carries_credentials())
    }

    /// Hosts that spoof loopback through userinfo, or are remote over HTTP.
    const REFUSED: [&str; 4] = [
        "http://127.0.0.1@evil.example",
        "http://localhost@evil.example",
        "http://user:pass@127.0.0.1:8787",
        "http://example.com",
    ];

    #[test]
    fn credentials_go_only_over_https_or_loopback() {
        for url in [
            "http://127.0.0.1:8787",
            "http://[::1]:8787",
            "http://localhost:8787",
            "https://alpha.norn.cloud",
            "https://studio.example/api/host",
            "http://localhost/api/host",
        ] {
            assert!(admits(url), "{url} should carry credentials");
        }
        for url in REFUSED.into_iter().chain([
            "http://studio.example/api/host",
            "http://localhost.example",
            "http://127.0.0.1.example",
            "ftp://127.0.0.1",
        ]) {
            assert!(!admits(url), "{url} must not carry credentials");
        }
    }

    #[test]
    fn the_sign_in_origin_comes_from_the_parsed_url() -> TestResult {
        assert_eq!(
            Endpoint::parse("http://[::1]:8787/api/host")?.origin(),
            "http://[::1]:8787"
        );
        assert_eq!(
            Endpoint::parse("https://alpha.norn.cloud/api/host")?.origin(),
            "https://alpha.norn.cloud"
        );
        Ok(())
    }

    #[test]
    fn a_client_sends_no_token_and_signs_in_nowhere_off_loopback() -> TestResult {
        for url in REFUSED {
            for credential in [Credential::Token, Credential::SignIn("owner@example.test")] {
                let Err(refused) = Client::new(&target(url, Some(credential))) else {
                    return Err(format!("{url} was admitted").into());
                };
                // Refused on the URL, before any secret is read or sent.
                assert!(
                    !refused.contains("environment variable"),
                    "{url}: {refused}"
                );
            }
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_artifact_is_refused_not_followed() -> TestResult {
        let root = scratch("link")?;
        let pack = root.join("pack");
        std::fs::create_dir(&pack)?;
        let outside = root.join("outside.json");
        std::fs::write(&outside, r#"{"secret":true}"#)?;
        std::fs::write(pack.join("a.json"), "{}")?;
        std::os::unix::fs::symlink(&outside, pack.join("b.json"))?;
        let Err(refused) = artifacts(std::slice::from_ref(&pack)) else {
            return Err("a linked artifact was read".into());
        };
        assert!(refused.contains("b.json"), "{refused}");
        assert!(artifacts(&[pack.join("b.json")]).is_err());
        std::fs::remove_dir_all(&root)?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn an_oversized_artifact_is_refused_while_reading() -> TestResult {
        let pack = scratch("oversized")?;
        std::fs::write(
            pack.join("big.json"),
            format!("{}{{}}", " ".repeat(MAX_BODY)),
        )?;
        let Err(refused) = artifacts(std::slice::from_ref(&pack)) else {
            return Err("an oversized artifact was read".into());
        };
        assert!(refused.contains("exceeds"), "{refused}");
        std::fs::remove_dir_all(&pack)?;
        Ok(())
    }

    #[test]
    fn malformed_authorities_queries_and_fragments_are_refused() -> TestResult {
        for url in [
            "http://127.0.0.1:abc",
            "http://127.0.0.1:",
            "http://127.0.0.1:99999",
            "http://127.0.0.1:8787x",
            "http://[::1",
            "http://[not-ipv6]:8787",
            "https://host[x]/",
            "https://host[x]:443/api",
            "https://alpha.norn.cloud#frag",
            "https://alpha.norn.cloud/api/host#frag",
        ] {
            assert!(Endpoint::parse(url).is_err(), "{url} must be refused");
        }
        for url in [
            "https://alpha.norn.cloud?foo=1",
            "https://alpha.norn.cloud/api?foo=1",
        ] {
            let Err(refused) = Client::new(&target(url, None)) else {
                return Err(format!("{url} was admitted").into());
            };
            assert!(refused.contains("query"), "{url}: {refused}");
        }
        Ok(())
    }

    #[test]
    fn loopback_http_is_never_proxied() -> TestResult {
        let proxy = ureq::Proxy::new("http://proxy.example:3128")?;
        for (url, direct) in [
            ("http://127.0.0.1:8787", true),
            ("http://[::1]:8787", true),
            ("http://localhost:8787", true),
            ("https://alpha.norn.cloud", false),
        ] {
            let config = Endpoint::parse(url)?
                .configure(ureq::Agent::config_builder().proxy(Some(proxy.clone())))
                .build();
            assert_eq!(config.proxy().is_none(), direct, "{url}");
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_pack_directory_is_refused_not_followed() -> TestResult {
        let root = scratch("linked-dir")?;
        let outside = root.join("outside");
        std::fs::create_dir(&outside)?;
        std::fs::write(outside.join("a.json"), r#"{"secret":true}"#)?;
        let pack = root.join("pack");
        std::os::unix::fs::symlink(&outside, &pack)?;
        let slash = PathBuf::from(format!("{}/", pack.display()));
        for candidate in [pack.clone(), slash, pack.join(".")] {
            let Err(refused) = artifacts(std::slice::from_ref(&candidate)) else {
                return Err(format!("linked pack read: {}", candidate.display()).into());
            };
            assert!(refused.contains("links are refused"), "{refused}");
        }
        let file = outside.join("a.json");
        let file_slash = PathBuf::from(format!("{}/", file.display()));
        for candidate in [file_slash, file.join(".")] {
            let Err(refused) = artifacts(std::slice::from_ref(&candidate)) else {
                return Err(format!("file read as dir: {}", candidate.display()).into());
            };
            assert!(refused.contains("not a directory"), "{refused}");
        }
        std::fs::remove_dir_all(&root)?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn directory_listing_memory_is_bounded_while_enumerating() -> TestResult {
        let pack = scratch("dir-names")?;
        let one = "alpha.json".len() + std::mem::size_of::<std::ffi::CString>();
        std::fs::write(pack.join("alpha.json"), "{}")?;
        let dir = open_no_follow(rustix::fs::CWD, &pack)?;
        assert_eq!(collect_json_names(&dir, &pack, one)?.len(), 1);
        std::fs::write(pack.join("bravo.json"), "{}")?;
        let dir = open_no_follow(rustix::fs::CWD, &pack)?;
        let Err(refused) = collect_json_names(&dir, &pack, one) else {
            return Err("oversized directory listing was retained".into());
        };
        assert!(refused.contains("directory listing exceeds"), "{refused}");
        std::fs::remove_dir_all(&pack)?;
        Ok(())
    }

    // `mkfifoat` is Linux-only in rustix; the open path under test is all Unix.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_fifo_artifact_is_refused_without_blocking() -> TestResult {
        let pack = scratch("fifo")?;
        let fifo = pack.join("a.json");
        let mode = rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR;
        rustix::fs::mkfifoat(rustix::fs::CWD, &fifo, mode)?;
        let Err(refused) = artifacts(std::slice::from_ref(&pack)) else {
            return Err("a FIFO artifact was read".into());
        };
        assert!(refused.contains("not a regular file"), "{refused}");
        std::fs::remove_dir_all(&pack)?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn the_byte_budget_spans_the_whole_pack() -> TestResult {
        let pack = scratch("budget")?;
        // Each file is under the bound; together they exceed it.
        for name in ["a.json", "b.json"] {
            std::fs::write(pack.join(name), format!("{}{{}}", " ".repeat(600_000)))?;
        }
        let Err(refused) = artifacts(std::slice::from_ref(&pack)) else {
            return Err("a pack over the bound was read".into());
        };
        assert!(refused.contains("b.json"), "{refused}");
        assert!(refused.contains("exceeds"), "{refused}");
        std::fs::remove_dir_all(&pack)?;
        Ok(())
    }

    #[cfg(not(unix))]
    #[test]
    fn packs_are_refused_without_no_follow_opens() {
        assert!(artifacts(&[PathBuf::from("pack")]).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn plan_and_apply_send_exact_numbers() -> TestResult {
        let pack = scratch("exact")?;
        std::fs::write(
            pack.join("a.json"),
            r#"{"artifactId":"a","amount":1000.00000000000001}"#,
        )?;
        let snapshot = artifacts(std::slice::from_ref(&pack))?;
        for sent in [body(&snapshot, None)?, body(&snapshot, Some(3))?] {
            assert!(sent.contains(r#""amount":1000.00000000000001"#), "{sent}");
        }
        std::fs::remove_dir_all(&pack)?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn plan_and_apply_send_one_snapshot() -> TestResult {
        let pack = scratch("snapshot")?;
        std::fs::write(pack.join("a.json"), r#"{"artifactId":"a","v":1}"#)?;
        let snapshot = artifacts(std::slice::from_ref(&pack))?;
        // A build replacing the artifact after the read changes nothing sent.
        std::fs::write(pack.join("a.json"), r#"{"artifactId":"a","v":2}"#)?;
        let plan: Value = serde_json::from_str(&body(&snapshot, None)?)?;
        let apply: Value = serde_json::from_str(&body(&snapshot, Some(7))?)?;
        assert_eq!(plan.get("artifacts"), apply.get("artifacts"));
        assert_eq!(apply.pointer("/artifacts/0/v"), Some(&json!(1)));
        assert_eq!(apply.get("expectedVersion"), Some(&json!(7)));
        std::fs::remove_dir_all(&pack)?;
        Ok(())
    }

    #[test]
    fn cookies_are_trimmed_and_deduplicated_last_wins() -> TestResult {
        let mut headers = ureq::http::HeaderMap::new();
        headers.append("set-cookie", " sid = old ; Path=/; HttpOnly".parse()?);
        headers.append("set-cookie", "csrf=tok1; SameSite=Lax".parse()?);
        headers.append("set-cookie", "sid=new; Path=/".parse()?);
        assert_eq!(
            collect_cookies(&headers),
            Some("sid=new; csrf=tok1".to_owned())
        );
        assert!(Endpoint::parse("HTTPS://alpha.norn.cloud")?.is_https());
        Ok(())
    }

    #[test]
    fn read_bounded_response_refuses_oversized_body() {
        assert_eq!(
            read_bounded_response(b"hello".as_slice(), 5, "test"),
            Ok("hello".to_owned())
        );
        assert!(read_bounded_response(b"hello!".as_slice(), 5, "test").is_err());
    }
}
