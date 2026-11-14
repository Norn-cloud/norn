//! `norn cell plan|apply`: change a running hosted cell's definitions
//! (HOST-INSTALL-1) through its governed routes. The cell decides: it admits
//! the pack over its active set, refuses with the checker's findings, and
//! installs only for an owner or admin. This client reads the pack, sends it,
//! and prints the cell's answer.
use clap::{Args, Subcommand};
use serde_json::{Value, json};
use std::io::Read as _;
use std::path::PathBuf;
use std::time::Duration;

/// The largest pack this client sends, as the cell bounds it.
const MAX_BODY: usize = 1 << 20;

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

/// The pack's artifacts, each file's exact JSON text, in path order.
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
            let text =
                std::fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?;
            serde_json::value::RawValue::from_string(text)
                .map_err(|e| format!("{}: not JSON: {e}", file.display()))
        })
        .collect()
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

fn origin_of(url: &str) -> Result<String, String> {
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| format!("{url} is not an absolute URL"))?;
    let host = rest.split('/').next().unwrap_or_default();
    if host.is_empty() {
        return Err(format!("{url} names no host"));
    }
    Ok(format!("{scheme}://{host}"))
}

/// Whether `url` may carry credentials: HTTPS, or plain HTTP to loopback.
fn credentials_allowed(url: &str) -> bool {
    url.starts_with("https://")
        || ["http://127.0.0.1", "http://localhost", "http://[::1]"]
            .iter()
            .any(|prefix| {
                url.strip_prefix(prefix)
                    .is_some_and(|rest| rest.is_empty() || rest.starts_with([':', '/']))
            })
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
        let origin = origin_of(&base)?;
        if (target.token_env.is_some() || target.email.is_some()) && !credentials_allowed(&base) {
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
            origin,
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
        .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_owned))
        .unwrap_or_default()
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
                .map(|from| format!("{from} -> "))
                .unwrap_or_default();
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
            .map(|pointer| format!(" at {pointer}"))
            .unwrap_or_default();
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

fn body(paths: &[PathBuf], expected_version: Option<u64>) -> Result<String, String> {
    let artifacts = artifacts(paths)?;
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
            let client = Client::new(target)?;
            let (status, plan) = client.post(
                norn_api::cell_definitions::PLAN_PATH,
                &body(&target.paths, None)?,
            )?;
            Ok(answer(target.json, status, &plan, plan_summary))
        }
        CellCommand::Apply {
            target,
            expected_version,
        } => {
            let client = Client::new(target)?;
            let mut shown = String::new();
            let expected = if let Some(version) = expected_version {
                *version
            } else {
                let (status, plan) = client.post(
                    norn_api::cell_definitions::PLAN_PATH,
                    &body(&target.paths, None)?,
                )?;
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
                &body(&target.paths, Some(expected))?,
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
    fn credentials_go_only_over_https_or_loopback() {
        assert!(credentials_allowed("https://studio.example/api/host"));
        assert!(credentials_allowed("http://127.0.0.1:8787"));
        assert!(credentials_allowed("http://localhost/api/host"));
        assert!(!credentials_allowed("http://studio.example/api/host"));
        assert!(!credentials_allowed("http://localhost.example"));
        assert!(!credentials_allowed("http://127.0.0.1.example"));
    }

    #[test]
    fn a_pack_directory_is_read_in_name_order_and_kept_exact() {
        let pack =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../registry/packs/jewelry/artifacts");
        let read = artifacts(&[pack]).unwrap();
        let ids: Vec<String> = read
            .iter()
            .map(|raw| text(&serde_json::from_str(raw.get()).unwrap(), "artifactId"))
            .collect();
        assert_eq!(
            ids,
            [
                "jewelry.Melt.entity",
                "jewelry.melt.fine_mass_conserved.law",
                "jewelry.melt.record.action"
            ]
        );
    }
}
