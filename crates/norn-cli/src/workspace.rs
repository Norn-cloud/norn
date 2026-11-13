//! `norn workspace create`: give a Studio workspace its data cell.
//!
//! A thin client of Studio's public API, with no authority of its own: it
//! signs in as a person, reads the workspace and the host's trusted-base
//! digest, and asks the host to provision the cell. The host decides who may
//! (the workspace's owner) and what is admitted.
use clap::{Args, Subcommand};
use serde_json::Value;
use std::io::Read as _;
use std::time::Duration;

#[derive(Debug, Args)]
pub(super) struct WorkspaceArgs {
    #[command(subcommand)]
    command: WorkspaceCommand,
}

#[derive(Debug, Subcommand)]
enum WorkspaceCommand {
    /// Provision the signed-in owner's workspace cell; an existing cell with the same definitions replays.
    #[command(
        long_about = "Provision the data cell of the signed-in person's active Studio workspace. Signs in to Studio with an email and a password read from an environment variable, reads the workspace's organization and environment, reads the host's trusted-base digest from GET /v1/cell:base, then submits the definitions file (default: none, so the cell starts with the trusted base). Only the workspace's owner may provision; anyone else is refused by the host. Running it again with the same definitions replays. Prints the host's provisioning receipt."
    )]
    Create {
        /// Studio's origin, for example <https://studio.example.com>; plain http only on loopback.
        #[arg(long)]
        studio: String,
        /// The account to sign in as: the workspace's owner.
        #[arg(long)]
        email: String,
        /// Environment variable holding the password; never a password argument.
        #[arg(long)]
        password_env: String,
        /// JSON array of the workspace's own artifact envelopes; omit for none.
        #[arg(long)]
        definitions: Option<std::path::PathBuf>,
        /// Connect to this loopback address instead of --studio's (on Studio's own machine, past
        /// its proxy); requests still name --studio as their origin.
        #[arg(long)]
        via: Option<String>,
    },
}

/// The largest response this command reads, in bytes.
const MAX_RESPONSE: u64 = 4 * 1024 * 1024;

/// Whether `origin` is an admissible origin on this machine's loopback.
fn loopback_origin(origin: &str) -> bool {
    admissible_origin(origin)
        && origin.parse::<ureq::http::Uri>().is_ok_and(|uri| {
            matches!(uri.host(), Some("localhost" | "127.0.0.1" | "[::1]" | "::1"))
        })
}

/// Whether `origin` is an https origin, or http on a loopback host.
fn admissible_origin(origin: &str) -> bool {
    let Ok(uri) = origin.parse::<ureq::http::Uri>() else {
        return false;
    };
    // An origin: no path, query or trailing slash.
    let bare = !origin.ends_with('/')
        && uri
            .path_and_query()
            .is_none_or(|rest| rest.as_str().is_empty() || rest.as_str() == "/");
    let loopback = matches!(uri.host(), Some("localhost" | "127.0.0.1" | "[::1]" | "::1"));
    bare && match uri.scheme_str() {
        Some("https") => uri.host().is_some(),
        Some("http") => loopback,
        _ => false,
    }
}

struct Studio {
    agent: ureq::Agent,
    /// Where requests go.
    address: String,
    /// What they name as their origin.
    origin: String,
    cookie: String,
}

impl Studio {
    fn call(&self, method: &str, path: &str, body: Option<&str>) -> Result<(u16, String), String> {
        let url = format!("{}{path}", self.address);
        let request = ureq::http::Request::builder()
            .method(method)
            .uri(&url)
            .header("origin", &self.origin)
            .header("accept", "application/json");
        let request = if self.cookie.is_empty() {
            request
        } else {
            request.header("cookie", &self.cookie)
        };
        let response = match body {
            Some(body) => self.agent.run(
                request
                    .header("content-type", "application/json")
                    .body(body.as_bytes().to_vec())
                    .map_err(|e| e.to_string())?,
            ),
            None => self
                .agent
                .run(request.body(Vec::new()).map_err(|e| e.to_string())?),
        };
        let mut response = response.map_err(|e| format!("{method} {path}: {e}"))?;
        let status = response.status().as_u16();
        let mut text = String::new();
        response
            .body_mut()
            .as_reader()
            .take(MAX_RESPONSE)
            .read_to_string(&mut text)
            .map_err(|e| e.to_string())?;
        Ok((status, text))
    }
}

fn json_of(path: &str, status: u16, text: &str) -> Result<Value, String> {
    if status != 200 {
        return Err(format!("{path} refused (HTTP {status}): {text}"));
    }
    serde_json::from_str(text).map_err(|_e| format!("{path} answered something other than JSON"))
}

/// A query value, percent-encoded: every byte but RFC 3986's unreserved ones.
fn percent(value: &str) -> String {
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

pub(super) fn execute(args: &WorkspaceArgs) -> Result<(String, bool), String> {
    let WorkspaceCommand::Create {
        studio,
        email,
        password_env,
        definitions,
        via,
    } = &args.command;
    if !admissible_origin(studio) {
        return Err("--studio must be an https origin without a path, or http on localhost".into());
    }
    let address = match via {
        Some(via) if loopback_origin(via) => via.clone(),
        Some(_) => {
            return Err("--via must be a loopback origin, for example http://127.0.0.1:8080".into());
        }
        None => studio.clone(),
    };
    let password =
        std::env::var(password_env).map_err(|_e| "the password environment variable is unset")?;
    // The definitions are passed on as written: their bytes are the submission.
    let artifacts = match definitions {
        Some(path) => {
            let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
            let value: Value =
                serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
            if !value.is_array() {
                return Err(format!("{}: not a JSON array of artifact envelopes", path.display()));
            }
            text
        }
        None => "[]".to_owned(),
    };
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(0)
        .timeout_global(Some(Duration::from_secs(60)))
        .build()
        .into();
    let mut session = Studio {
        agent,
        address,
        origin: studio.clone(),
        cookie: String::new(),
    };

    // Sign in. The session cookie is kept in memory for these calls only.
    let credentials = serde_json::json!({ "email": email, "password": password }).to_string();
    let url = format!("{}/api/auth/sign-in/email", session.address);
    let request = ureq::http::Request::builder()
        .method("POST")
        .uri(&url)
        .header("origin", studio)
        .header("content-type", "application/json")
        .body(credentials.into_bytes())
        .map_err(|e| e.to_string())?;
    let response = session
        .agent
        .run(request)
        .map_err(|e| format!("sign-in: {e}"))?;
    if response.status().as_u16() != 200 {
        return Err(format!(
            "sign-in refused (HTTP {}): check the email and password",
            response.status().as_u16()
        ));
    }
    session.cookie = response
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .filter_map(|line| line.split(';').next())
        .collect::<Vec<_>>()
        .join("; ");

    let (status, text) = session.call("GET", "/api/me", None)?;
    let me = json_of("/api/me", status, &text)?;
    let org = me
        .pointer("/organization/id")
        .and_then(Value::as_str)
        .ok_or("the account has no active workspace")?;
    let environment = me
        .get("environmentId")
        .and_then(Value::as_str)
        .ok_or("the workspace has no data environment; this Studio gives new workspaces none")?;

    let (status, text) = session.call("GET", "/api/host/v1/cell:base", None)?;
    let base = json_of("/v1/cell:base", status, &text)?;
    let base = base
        .get("baseSha256")
        .and_then(Value::as_str)
        .ok_or("the host named no trusted base")?;

    let submission = format!(
        r#"{{"baseSha256":{},"artifacts":{artifacts}}}"#,
        serde_json::to_string(base).map_err(|e| e.to_string())?
    );
    let path = format!(
        "/api/host/v1/cell:provision?orgId={}&environmentId={}",
        percent(org),
        percent(environment)
    );
    let (status, text) = session.call("POST", &path, Some(&submission))?;
    // The receipt, or the host's refusal, is printed as the host wrote it.
    Ok((text, status == 200))
}

#[cfg(test)]
mod tests {
    use super::admissible_origin;

    #[test]
    fn only_https_or_loopback_http_origins_are_used() {
        for origin in [
            "https://studio.example.com",
            "https://studio.example.com:8443",
            "http://localhost:8080",
            "http://127.0.0.1:8080",
            "http://[::1]:8080",
        ] {
            assert!(admissible_origin(origin), "{origin}");
        }
        for origin in [
            "http://studio.example.com",
            "http://localhost.example.com",
            "https://studio.example.com/",
            "https://studio.example.com/x",
            "https://studio.example.com?x=1",
            "ftp://localhost",
            "localhost:8080",
        ] {
            assert!(!admissible_origin(origin), "{origin}");
        }
    }

    #[test]
    fn via_is_loopback_only() {
        assert!(super::loopback_origin("http://127.0.0.1:8080"));
        assert!(super::loopback_origin("http://localhost:8080"));
        assert!(!super::loopback_origin("https://studio.example.com"));
        assert!(!super::loopback_origin("http://10.0.0.2:8080"));
    }

    #[test]
    fn query_values_are_percent_encoded() {
        assert_eq!(super::percent("workspace_ab-1.x~"), "workspace_ab-1.x~");
        assert_eq!(super::percent("a b&c=d/é"), "a%20b%26c%3Dd%2F%C3%A9");
    }
}
