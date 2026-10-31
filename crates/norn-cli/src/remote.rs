//! Thin remote client: the server's live MCP catalog supplies every tool.

use std::io::{BufRead as _, Read as _, Write as _};
use std::process::ExitCode;
use std::time::Duration;

use clap::{Args, Subcommand};
use norn_ir::RuntimeValue;
use serde_json::{Value, json};

#[derive(Debug, Args)]
pub(super) struct RemoteArgs {
    /// Full MCP endpoint URL, for example https://norn.example/mcp.
    #[arg(long)]
    url: String,
    #[arg(long)]
    org: String,
    #[arg(long)]
    env: String,
    /// Environment variable holding the bearer token; never a token argument.
    #[arg(long)]
    token_env: Option<String>,
    /// Actor header for a server explicitly configured for local development.
    #[arg(long, conflicts_with = "token_env")]
    dev_actor: Option<String>,
    #[command(subcommand)]
    command: RemoteCommand,
}

#[derive(Debug, Subcommand)]
enum RemoteCommand {
    /// Print the live tools/list result, including schemas and transitions.
    List,
    /// Call any live tool; read its exact JSON object arguments from stdin.
    Call {
        name: String,
        /// Stable operation identity; reuse only when retrying the same call.
        #[arg(long)]
        idempotency_key: String,
    },
}

pub(super) fn run(args: RemoteArgs) -> ExitCode {
    match execute(&args) {
        Ok((text, failed)) => {
            if std::io::stdout().write_all(text.as_bytes()).is_err() {
                return ExitCode::FAILURE;
            }
            if failed {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(error) => {
            super::write_err(&format!("norn remote: {error}"));
            ExitCode::FAILURE
        }
    }
}

fn request(command: &RemoteCommand, input: &str) -> Result<String, String> {
    match command {
        RemoteCommand::List => Ok(json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/list"
        })
        .to_string()),
        RemoteCommand::Call {
            name,
            idempotency_key,
        } => {
            if idempotency_key.trim().is_empty() {
                return Err("idempotency key must be nonblank".into());
            }
            let input_value = RuntimeValue::from_json_text(input).map_err(|e| e.to_string())?;
            if input_value.as_object().is_none() {
                return Err("tool arguments must be a JSON object".into());
            }
            // Keep the validated original tokens; serde_json::Value can round
            // business decimals when compiled without arbitrary_precision.
            let name = serde_json::to_string(name).map_err(|e| e.to_string())?;
            let key = serde_json::to_string(idempotency_key).map_err(|e| e.to_string())?;
            Ok(format!(
                r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":{name},"arguments":{input},"_meta":{{"norn/idempotencyKey":{key}}}}}}}"#
            ))
        }
    }
}

fn execute(args: &RemoteArgs) -> Result<(String, bool), String> {
    let mut input = String::new();
    if matches!(args.command, RemoteCommand::Call { .. }) {
        std::io::stdin()
            .take(4 * 1024 * 1024 + 1)
            .read_to_string(&mut input)
            .map_err(|e| e.to_string())?;
        if input.len() > 4 * 1024 * 1024 {
            return Err("tool arguments exceed 4 MiB".into());
        }
    }
    let body = request(&args.command, &input)?;
    if args.token_env.is_some() && !args.url.starts_with("https://") {
        return Err("bearer authentication requires an HTTPS endpoint".into());
    }
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .max_redirects(0)
        .timeout_global(Some(Duration::from_secs(30)))
        .build()
        .into();
    let mut call = agent
        .post(&args.url)
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .header("mcp-protocol-version", "2025-11-25")
        .header("x-norn-org-id", &args.org)
        .header("x-norn-environment-id", &args.env);
    if let Some(variable) = &args.token_env {
        let token =
            std::env::var(variable).map_err(|_e| "token environment variable is unavailable")?;
        call = call.header("authorization", format!("Bearer {token}"));
    }
    if let Some(actor) = &args.dev_actor {
        call = call.header("x-norn-actor", actor);
    }
    let mut response = call
        .send(body.as_bytes())
        .map_err(|_e| "MCP HTTP request failed")?;
    let is_sse = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.split(';').next() == Some("text/event-stream"));
    let text = response_text(response.body_mut().as_reader(), is_sse)?;
    let value: Value = serde_json::from_str(&text).map_err(|_e| "invalid MCP JSON response")?;
    if value.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
        || value.get("id") != Some(&json!(1))
        || (value.get("result").is_some() == value.get("error").is_some())
    {
        return Err("invalid MCP response envelope".into());
    }
    let failed = value.get("error").is_some()
        || value.pointer("/result/isError").and_then(Value::as_bool) == Some(true);
    Ok((text, failed))
}

fn response_text(reader: impl std::io::Read, is_sse: bool) -> Result<String, String> {
    let mut reader = std::io::BufReader::new(reader.take(8 * 1024 * 1024));
    if !is_sse {
        let mut text = String::new();
        reader
            .read_to_string(&mut text)
            .map_err(|_e| "MCP response could not be read")?;
        return Ok(text);
    }
    let mut data = String::new();
    loop {
        let mut line = String::new();
        if reader
            .read_line(&mut line)
            .map_err(|_e| "MCP stream could not be read")?
            == 0
        {
            return Err("MCP stream ended before its response".into());
        }
        if line.trim_end_matches(['\r', '\n']).is_empty() {
            if serde_json::from_str::<Value>(&data)
                .ok()
                .is_some_and(|value| value.get("id") == Some(&json!(1)))
            {
                return Ok(data);
            }
            data.clear();
        } else if let Some(field) = line.strip_prefix("data:") {
            data.push_str(field.strip_prefix(' ').unwrap_or(field));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_preserves_exact_arguments_and_operation_identity() -> Result<(), String> {
        let body = request(
            &RemoteCommand::Call {
                name: "sales_order.approve".into(),
                idempotency_key: "operation-1".into(),
            },
            r#"{"amount":12345678901234567890.123456789}"#,
        )?;
        assert!(body.contains("12345678901234567890.123456789"));
        let value: Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
        assert_eq!(
            value.pointer("/params/_meta/norn~1idempotencyKey"),
            Some(&json!("operation-1"))
        );
        assert!(
            request(
                &RemoteCommand::Call {
                    name: "sales_order.approve".into(),
                    idempotency_key: "operation-1".into(),
                },
                "[]"
            )
            .is_err()
        );
        Ok(())
    }
}
