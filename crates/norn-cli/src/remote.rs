//! Thin remote client: the server's live MCP catalog supplies every tool.
use clap::{Args, Subcommand};
use futures_util::StreamExt as _;
use norn_ir::RuntimeValue;
use serde_json::{Value, json};
use std::io::Read as _;
use std::time::Duration;

#[derive(Debug, Args)]
pub(super) struct RemoteArgs {
    /// Full MCP endpoint URL, for example <https://norn.example/mcp>.
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
        /// Optional opaque correlation identifier tying multi-step causes and effects together.
        #[arg(long)]
        trace_id: Option<String>,
        /// Optional reference to the goal, prompt, commitment, or decision that initiated the call.
        #[arg(long, requires = "trace_id")]
        intent_ref: Option<String>,
        /// Optional SHA-256 digest (`sha256:<64 lower-case hex>`) of an off-ledger session transcript.
        #[arg(long, requires = "trace_id")]
        session_digest: Option<String>,
    },
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
            trace_id,
            intent_ref,
            session_digest,
        } => {
            if idempotency_key.trim().is_empty() {
                return Err("idempotency key must be nonblank".into());
            }
            let trace = match (trace_id, intent_ref, session_digest) {
                (None, None, None) => None,
                (None, Some(_), _) | (None, _, Some(_)) => {
                    return Err("--intent-ref and --session-digest require --trace-id".into());
                }
                (Some(trace_id), intent_ref, session_digest) => {
                    let context = norn_ir::TraceContext {
                        trace_id: trace_id.clone(),
                        intent_ref: intent_ref.clone(),
                        session_digest: session_digest
                            .as_ref()
                            .map(|digest| norn_ir::function::Digest(digest.clone())),
                    };
                    context.validate().map_err(|e| e.to_string())?;
                    Some(context)
                }
            };
            let input_value = RuntimeValue::from_json_text(input).map_err(|e| e.to_string())?;
            if input_value.as_object().is_none() {
                return Err("tool arguments must be a JSON object".into());
            }
            // Keep the validated original tokens; serde_json::Value can round
            // business decimals when compiled without arbitrary_precision.
            let name = serde_json::to_string(name).map_err(|e| e.to_string())?;
            let key = serde_json::to_string(idempotency_key).map_err(|e| e.to_string())?;
            let meta = match trace {
                None => format!(r#"{{"norn/idempotencyKey":{key}}}"#),
                Some(trace) => {
                    let trace = serde_json::to_string(&trace).map_err(|e| e.to_string())?;
                    format!(r#"{{"norn/idempotencyKey":{key},"norn/trace":{trace}}}"#)
                }
            };
            Ok(format!(
                r#"{{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{{"name":{name},"arguments":{input},"_meta":{meta}}}}}"#
            ))
        }
    }
}

pub(super) fn execute(args: &RemoteArgs) -> Result<(String, bool), String> {
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
    if args.token_env.is_some()
        && !super::cell::Endpoint::parse(&args.url).is_ok_and(|endpoint| endpoint.is_https())
    {
        return Err("bearer authentication requires an HTTPS endpoint".into());
    }
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
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
    let status = response.status();
    let http_failed = !status.is_success();
    let is_sse = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.split(';').next() == Some("text/event-stream"));
    let text = response_text(response.body_mut().as_reader(), is_sse)?;
    let value: Value = serde_json::from_str(&text).map_err(|_e| {
        if http_failed {
            format!("MCP HTTP request failed ({status})")
        } else {
            "invalid MCP JSON response".to_owned()
        }
    })?;
    if value.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
        || value.get("id") != Some(&json!(1))
        || (value.get("result").is_some() == value.get("error").is_some())
    {
        return Err("invalid MCP response envelope".into());
    }
    let failed = http_failed
        || value.get("error").is_some()
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
    let chunks = std::iter::from_fn(move || {
        let mut buffer = vec![0; 8192];
        match reader.read(&mut buffer) {
            Ok(0) => None,
            Ok(size) => {
                buffer.truncate(size);
                Some(Ok(bytes::Bytes::from(buffer)))
            }
            Err(error) => Some(Err(error)),
        }
    });
    let events = sse_stream::SseStream::from_bytes_stream(futures_util::stream::iter(chunks));
    tokio::runtime::Builder::new_current_thread()
        .build()
        .map_err(|e| e.to_string())?
        .block_on(async {
            futures_util::pin_mut!(events);
            while let Some(event) = events.next().await {
                if let Some(data) = event.map_err(|e| e.to_string())?.data
                    && serde_json::from_str::<Value>(&data)
                        .is_ok_and(|value| value.get("id") == Some(&json!(1)))
                {
                    return Ok(data);
                }
            }
            Err("MCP stream ended before its response".into())
        })
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
                trace_id: None,
                intent_ref: None,
                session_digest: None,
            },
            r#"{"amount":12345678901234567890.123456789}"#,
        )?;
        assert!(body.contains("12345678901234567890.123456789"));
        let value: Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
        assert_eq!(
            value.pointer("/params/_meta/norn~1idempotencyKey"),
            Some(&json!("operation-1"))
        );
        assert_eq!(value.pointer("/params/_meta/norn~1trace"), None);
        let response =
            r#"{"jsonrpc":"2.0","id":1,"result":{"amount":12345678901234567890.123456789}}"#;
        let stream = format!("event: message\r\ndata: {response}\r\n\r\n");
        assert_eq!(response_text(stream.as_bytes(), true)?, response);
        Ok(())
    }

    #[test]
    fn call_threads_optional_trace_context_and_refuses_malformed_trace() -> Result<(), String> {
        let digest = format!("sha256:{}", "a".repeat(64));
        let body = request(
            &RemoteCommand::Call {
                name: "sales_order.approve".into(),
                idempotency_key: "operation-2".into(),
                trace_id: Some("trace-123".into()),
                intent_ref: Some("decision:dec_abc".into()),
                session_digest: Some(digest.clone()),
            },
            r#"{"id":"SO-1"}"#,
        )?;
        let value: Value = serde_json::from_str(&body).map_err(|e| e.to_string())?;
        assert_eq!(
            value.pointer("/params/_meta/norn~1trace"),
            Some(&json!({
                "traceId": "trace-123",
                "intentRef": "decision:dec_abc",
                "sessionDigest": digest,
            }))
        );
        // Raw transcript text in session_digest must refuse before sending.
        assert!(
            request(
                &RemoteCommand::Call {
                    name: "sales_order.approve".into(),
                    idempotency_key: "operation-3".into(),
                    trace_id: Some("trace-123".into()),
                    intent_ref: None,
                    session_digest: Some("user: please approve this order".into()),
                },
                r#"{"id":"SO-1"}"#,
            )
            .is_err()
        );
        // Blank trace_id must refuse.
        assert!(
            request(
                &RemoteCommand::Call {
                    name: "sales_order.approve".into(),
                    idempotency_key: "operation-4".into(),
                    trace_id: Some("   ".into()),
                    intent_ref: None,
                    session_digest: None,
                },
                r#"{"id":"SO-1"}"#,
            )
            .is_err()
        );
        Ok(())
    }
}
