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
/// [`MAX_BODY`] as a read limit: no one artifact file may exceed it.
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

/// The pack's artifacts, each file's exact JSON text, in path order. Read
/// once per command: `apply` plans and applies this one snapshot.
fn artifacts(paths: &[PathBuf]) -> Result<Vec<Box<serde_json::value::RawValue>>, String> {
    let mut files = Vec::new();
    for path in paths {
        if path.is_dir() {
            let mut inside: Vec<PathBuf> = std::fs::read_dir(path)
                .map_err(|e| format!("{}: {e}", path.display()))?
                .map(|entry| entry.map(|entry| entry.path()))
                .collect::<Result<_, _>>()
                .map_err(|e| format!("{}: {e}", path.display()))?;
            inside.retain(|file| file.extension().is_some_and(|ext| ext == "json"));
            inside.sort();
            files.extend(inside);
        } else {
            files.push(path.clone());
        }
    }
    if files.is_empty() {
        return Err("no artifact files".to_owned());
    }
    files
        .iter()
        .map(|file: &PathBuf| {
            let text = read_artifact(file)?;
            serde_json::value::RawValue::from_string(text)
                .map_err(|e| format!("{}: not JSON: {e}", file.display()))
        })
        .collect()
}

/// One artifact file's text: a regular file, never a link or anything else,
/// read to at most the cell's bound so a huge file is refused unread.
fn read_artifact(file: &Path) -> Result<String, String> {
    let shown = file.display();
    let linked = std::fs::symlink_metadata(file).map_err(|e| format!("{shown}: {e}"))?;
    if !linked.file_type().is_file() {
        return Err(format!("{shown}: not a regular file (links are refused)"));
    }
    let handle = std::fs::File::open(file).map_err(|e| format!("{shown}: {e}"))?;
    let opened = handle.metadata().map_err(|e| format!("{shown}: {e}"))?;
    if !opened.is_file() || !same_file(&linked, &opened) {
        return Err(format!(
            "{shown}: changed while being opened (links are refused)"
        ));
    }
    let mut text = String::new();
    handle
        .take(MAX_FILE.saturating_add(1))
        .read_to_string(&mut text)
        .map_err(|e| format!("{shown}: {e}"))?;
    if text.len() > MAX_BODY {
        return Err(format!("{shown}: exceeds the cell's 1 MiB bound"));
    }
    Ok(text)
}

/// Whether the opened handle is the file `symlink_metadata` saw: a link
/// swapped in between the check and the open names another inode.
#[cfg(unix)]
fn same_file(linked: &std::fs::Metadata, opened: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    linked.dev() == opened.dev() && linked.ino() == opened.ino()
}

/// Whether the opened handle is the file `symlink_metadata` saw (without
/// inode numbers, the regular-file checks are what this platform offers).
#[cfg(not(unix))]
fn same_file(linked: &std::fs::Metadata, opened: &std::fs::Metadata) -> bool {
    linked.is_file() && opened.is_file()
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
        let uri: ureq::http::Uri = url.parse().map_err(|_e| "URL is not valid")?;
        let authority = uri.authority().ok_or("URL must be absolute")?;
        if authority.as_str().contains('@') {
            return Err("URL must not contain credentials".into());
        }
        if uri.host().is_none_or(str::is_empty) {
            return Err("URL names no host".into());
        }
        let endpoint = Self { uri };
        if !endpoint.is_https() && !endpoint.is_http() {
            return Err("URL must use http or https".into());
        }
        Ok(endpoint)
    }

    pub(super) fn is_https(&self) -> bool {
        self.uri.scheme_str() == Some("https")
    }

    fn is_http(&self) -> bool {
        self.uri.scheme_str() == Some("http")
    }

    /// The host, without an IPv6 literal's brackets.
    fn host(&self) -> &str {
        let host = self.uri.host().unwrap_or_default();
        host.strip_prefix('[')
            .and_then(|inner| inner.strip_suffix(']'))
            .unwrap_or(host)
    }

    /// Plain HTTP to exactly `host`.
    pub(super) fn is_http_to(&self, host: &str) -> bool {
        self.is_http() && self.host() == host
    }

    pub(super) fn has_query(&self) -> bool {
        self.uri.query().is_some()
    }

    /// Whether this endpoint may carry credentials: HTTPS, or plain HTTP to
    /// a loopback address or `localhost`.
    fn carries_credentials(&self) -> bool {
        let host = self.host();
        self.is_https()
            || (self.is_http()
                && (host.eq_ignore_ascii_case("localhost")
                    || host
                        .parse::<std::net::IpAddr>()
                        .is_ok_and(|ip| ip.is_loopback())))
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

impl Client {
    fn new(target: &Target) -> Result<Self, String> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_global(Some(Duration::from_secs(120)))
            .build()
            .into();
        let base = target.url.trim_end_matches('/').to_owned();
        let endpoint = Endpoint::parse(&base)?;
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
        let cookies: Vec<String> = response
            .headers()
            .get_all("set-cookie")
            .iter()
            .filter_map(|value| value.to_str().ok())
            .filter_map(|value| value.split(';').next())
            .map(str::to_owned)
            .collect();
        if cookies.is_empty() {
            return Err("Studio sign-in returned no session".into());
        }
        Ok(cookies.join("; "))
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
        let mut text = String::new();
        response
            .body_mut()
            .as_reader()
            .take(16 * 1024 * 1024)
            .read_to_string(&mut text)
            .map_err(|e| e.to_string())?;
        let value = serde_json::from_str(&text)
            .map_err(|_e| format!("{path} answered {status} with a non-JSON body"))?;
        Ok((status, value))
    }
}

fn encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
                char::from(byte).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
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

fn body(
    artifacts: &[Box<serde_json::value::RawValue>],
    expected_version: Option<u64>,
) -> Result<String, String> {
    let body = match expected_version {
        Some(version) => json!({ "artifacts": artifacts, "expectedVersion": version }),
        None => json!({ "artifacts": artifacts }),
    };
    let body = body.to_string();
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
    use std::path::Path;

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
    fn scratch(name: &str) -> Result<PathBuf, std::io::Error> {
        let dir = std::env::temp_dir().join(format!("norn-cli-cell-{}-{name}", std::process::id()));
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
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
            for email in [None, Some("owner@example.test")] {
                let target = Target {
                    url: url.to_owned(),
                    org: "org".to_owned(),
                    env: "env".to_owned(),
                    token_env: email.is_none().then(|| "NORN_TEST_UNSET_TOKEN".to_owned()),
                    email: email.map(str::to_owned),
                    password_env: email.map(|_email| "NORN_TEST_UNSET_PASSWORD".to_owned()),
                    json: false,
                    paths: Vec::new(),
                };
                let Err(refused) = Client::new(&target) else {
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
}
