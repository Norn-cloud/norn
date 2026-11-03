//! HOST-WIRE/HOST-SCOPE and MCP-CATALOG-1: bounded single-cell MCP transport.
use crate::{
    mcp::{Backend, CallOutput, Session},
    refusal::Refusal,
    scope::CellScope,
};
use norn_ir::{McpTool, RuntimeObject, RuntimeValue};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Content arguments to the same checker ABI used by the human command.
#[derive(Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
struct CheckInput {
    artifacts: String,
    context: String,
}
struct Checker;
impl Backend for Checker {
    fn call(
        &mut self,
        _: &str,
        arguments: RuntimeObject,
        _: RuntimeObject,
    ) -> Result<CallOutput, Refusal> {
        let input: CheckInput =
            norn_ir::runtime::decode(&RuntimeValue::Object(arguments)).map_err(Refusal::invalid)?;
        let report = norn_checker_wasm::check_artifact_set_json(&input.artifacts, &input.context)
            .map_err(|e| Refusal::invalid(e.to_string()))?;
        let value = RuntimeValue::from_json_text(&report).map_err(Refusal::host)?;
        let admitted =
            value.as_object().and_then(|v| v.get("admitted")) == Some(&RuntimeValue::Bool(true));
        Ok(CallOutput {
            value,
            is_error: !admitted,
        })
    }
}
fn session(binding: &str) -> Result<Session, Refusal> {
    let input_schema =
        serde_json::to_value(schemars::schema_for!(CheckInput)).map_err(Refusal::host)?;
    let tool: McpTool = serde_json::from_value(serde_json::json!({"name":"norn.check", "description":"Check artifact-set content through the shared checker; no storage access", "inputSchema":input_schema})).map_err(Refusal::host)?;
    Session::new(vec![tool], binding)
}
/// Transport request; scope is explicit and never inferred from the URL.
#[derive(Deserialize)]
pub struct Request {
    /// HTTP method.
    pub method: String,
    /// Exact path without query.
    pub path: String,
    /// Lowercase transport headers; scope and session remain explicit.
    pub headers: BTreeMap<String, String>,
    /// Original UTF-8 JSON-RPC bytes.
    pub body: String,
}
/// Finite HTTP response; no float conversion of the protocol body.
#[derive(Serialize)]
pub struct Response {
    /// HTTP status.
    pub status: u16,
    /// Exact response text.
    pub body: String,
    /// Connection ID, when initialization succeeded.
    pub session: Option<String>,
}
/// Trusted single-cell configuration and bounded, ephemeral protocol sessions.
pub struct Host {
    scope: CellScope,
    origin: String,
    sessions: BTreeMap<String, (Session, u64)>,
}
impl Host {
    /// Construct from trusted deployment configuration, not request headers.
    #[must_use]
    pub fn new(scope: CellScope, origin: String) -> Self {
        Self {
            scope,
            origin,
            sessions: BTreeMap::new(),
        }
    }
    /// Dispatch a request value to the existing Rust Session and checker service.
    /// `nonce` comes from platform cryptographic randomness; `now` is host time.
    #[must_use]
    pub fn handle(&mut self, request: &Request, nonce: &str, now: u64) -> Response {
        match self.serve(request, nonce, now) {
            Ok(reply) => reply,
            Err(refusal) => Response {
                status: refusal.status,
                body: serde_json::to_string(&refusal).unwrap_or_default(),
                session: None,
            },
        }
    }
    fn serve(&mut self, r: &Request, nonce: &str, now: u64) -> Result<Response, Refusal> {
        let reply = |status, body, session| {
            Ok(Response {
                status,
                body,
                session,
            })
        };
        let header = |key: &str| r.headers.get(key).map(String::as_str);
        if header("origin").is_some_and(|v| self.origin.is_empty() || v != self.origin) {
            return reply(403, String::new(), None);
        }
        self.scope
            .admit(header("norn-org"), header("norn-environment"))?;
        if r.path != "/mcp" && r.path != "/catalog" {
            return reply(404, String::new(), None);
        }
        if r.body.len() > 1024 * 1024 {
            return reply(413, String::new(), None);
        }
        let binding = format!(
            "{}/public-checker/{}",
            self.scope.key(),
            env!("CARGO_PKG_VERSION")
        );
        if r.path == "/catalog" && r.method == "GET" {
            let body = session(&binding)?
                .catalog(None, None)?
                .canonical_json()
                .map_err(Refusal::host)?;
            return reply(200, String::from_utf8(body).map_err(Refusal::host)?, None);
        }
        if r.method != "POST" || r.path != "/mcp" {
            return reply(405, String::new(), None);
        }
        if header("mcp-protocol-version").is_some_and(|v| v != "2025-11-25") {
            return reply(400, String::new(), None);
        }
        self.sessions.retain(|_, (_, expiry)| now < *expiry);
        let id = header("mcp-session-id").unwrap_or(nonce).to_owned();
        if header("mcp-session-id").is_none() {
            let source = serde_json::from_str::<serde_json::Value>(&r.body)
                .map_err(|e| Refusal::invalid(e.to_string()))?;

            if source.get("method").and_then(serde_json::Value::as_str) != Some("initialize") {
                return reply(400, String::new(), None);
            }
            if self.sessions.len() >= 64 || id.is_empty() || self.sessions.contains_key(&id) {
                return reply(503, String::new(), None);
            }
            if source
                .pointer("/params/protocolVersion")
                .and_then(serde_json::Value::as_str)
                != Some("2025-11-25")
            {
                return reply(400, String::new(), None);
            }
            if source.get("id").is_none() {
                return reply(202, String::new(), None);
            }
            self.sessions.insert(
                id.clone(),
                (session(&binding)?, now.saturating_add(300_000)),
            );
        }
        if !self.sessions.contains_key(&id) {
            return reply(404, String::new(), None);
        }
        let (session, _) = self
            .sessions
            .get_mut(&id)
            .ok_or_else(|| Refusal::host("session absent"))?;
        let body = session.handle(&r.body, &mut Checker)?;
        reply(
            if body.is_some() { 200 } else { 202 },
            body.unwrap_or_default(),
            Some(id),
        )
    }
}
