//! Generic MCP boundary; catalogs are IR and execution remains backend-owned.
use crate::refusal::Refusal;
use norn_ir::{McpTool, RuntimeObject, RuntimeValue};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

/// The backend's unchanged payload and failure classification.
pub struct CallOutput {
    /// Shared checker report, action receipt, or other existing backend output.
    pub value: RuntimeValue,
    /// Whether the backend refused the task.
    pub is_error: bool,
}
/// Implemented by the human operation's service, never by a permission evaluator in a client.
pub trait Backend {
    /// Dispatch through the same authorized implementation used by human callers.
    ///
    /// # Errors
    /// The backend's already-projected shared refusal. Metadata is untrusted
    /// request context (for example retry identity), never a disclosure grant.
    fn call(
        &mut self,
        name: &str,
        arguments: RuntimeObject,
        metadata: RuntimeObject,
    ) -> Result<CallOutput, Refusal>;
}
/// MCP revisions this boundary speaks, newest first. `initialize` answers
/// with the client's revision when it is listed here, else the newest.
pub const PROTOCOL_VERSIONS: [&str; 3] = ["2025-11-25", "2025-06-18", "2025-03-26"];

/// One connection over a fixed, authorized catalog snapshot.
pub struct Session {
    tools: Vec<McpTool>,
    digest: String,
    initialized: bool,
    capabilities: RuntimeObject,
    instructions: Option<String>,
}
#[derive(Deserialize)]
struct Request {
    jsonrpc: String,
    id: Option<RuntimeValue>,
    method: String,
    #[serde(default = "empty_parameters")]
    params: RuntimeValue,
}
fn empty_parameters() -> RuntimeValue {
    RuntimeValue::Object(RuntimeObject::new())
}
#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Parameters {
    name: Option<String>,
    #[serde(default)]
    arguments: RuntimeObject,
    #[serde(default, rename = "_meta")]
    metadata: RuntimeObject,
    cursor: Option<String>,
    limit: Option<usize>,
    protocol_version: Option<String>,
}
#[derive(Serialize)]
struct TextContent {
    r#type: &'static str,
    text: String,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ToolOutput {
    content: Vec<TextContent>,
    structured_content: RuntimeValue,
    is_error: bool,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Page<'a> {
    tools: &'a [McpTool],
    #[serde(skip_serializing_if = "Option::is_none")]
    next_cursor: Option<String>,
    catalog_digest: &'a str,
}
fn encode(value: &impl Serialize) -> Result<RuntimeValue, Refusal> {
    norn_ir::runtime::encode(value).map_err(Refusal::host)
}
fn document(value: &RuntimeValue) -> Result<String, Refusal> {
    String::from_utf8(value.canonical_json().map_err(Refusal::host)?).map_err(Refusal::host)
}
fn response(id: RuntimeValue, field: &str, payload: RuntimeValue) -> Result<String, Refusal> {
    document(&RuntimeValue::Object(RuntimeObject::from([
        ("jsonrpc".into(), RuntimeValue::String("2.0".into())),
        ("id".into(), id),
        (field.into(), payload),
    ])))
}
fn protocol_error(id: RuntimeValue, code: i32, refusal: Refusal) -> Result<String, Refusal> {
    #[derive(Serialize)]
    struct Error {
        code: i32,
        message: String,
        data: Refusal,
    }
    response(
        id,
        "error",
        encode(&Error {
            code,
            message: refusal.message.clone(),
            data: refusal,
        })?,
    )
}
impl Session {
    /// Bind generated tool IR to the server's scope, actor and definition revision.
    /// MCP-CATALOG-1: cursor reuse requires this entire binding and catalog to agree.
    ///
    /// # Errors
    /// Duplicate/empty names, absent descriptions, or non-object input schemas.
    pub fn new(mut tools: Vec<McpTool>, binding: &str) -> Result<Self, Refusal> {
        if binding.trim().is_empty() {
            return Err(Refusal::invalid(
                "catalog requires an explicit authority/revision binding",
            ));
        }
        let mut names = BTreeSet::new();
        for tool in &tools {
            if tool.name.is_empty()
                || !names.insert(tool.name.clone())
                || tool
                    .description
                    .as_ref()
                    .is_none_or(|d| d.trim().is_empty())
                || tool
                    .input_schema
                    .get("type")
                    .and_then(norn_ir::JsonValue::as_string)
                    != Some("object")
            {
                return Err(Refusal::invalid(
                    "tool catalog requires unique names, descriptions and object schemas",
                ));
            }
        }
        tools.sort_by(|a, b| a.name.cmp(&b.name));
        let bytes = encode(&(binding, &tools))?
            .canonical_json()
            .map_err(Refusal::host)?;
        let digest = norn_ir::canonical::HashProfile::Norn1.hash_canonical_bytes(&bytes);
        Ok(Self {
            tools,
            digest,
            initialized: false,
            capabilities: RuntimeObject::new(),
            instructions: None,
        })
    }
    /// A stateless Streamable HTTP request: each POST is answered on its own
    /// over the catalog the server rebuilt for it, so no handshake state is
    /// carried between requests. The transport checks `MCP-Protocol-Version`.
    #[must_use]
    pub fn stateless(mut self) -> Self {
        self.initialized = true;
        self
    }
    /// Capabilities the server declares beside `tools` (for example
    /// `resources`, or an `extensions` map); `tools` itself stays the catalog's.
    #[must_use]
    pub fn with_capabilities(mut self, capabilities: RuntimeObject) -> Self {
        self.capabilities = capabilities;
        self.capabilities.remove("tools");
        self
    }
    /// Plain-language guidance `initialize` returns for the model.
    #[must_use]
    pub fn with_instructions(mut self, instructions: impl Into<String>) -> Self {
        self.instructions = Some(instructions.into());
        self
    }
    /// Handle one bounded JSON-RPC message without converting exact arguments through floats.
    /// Notifications never execute a tool. No transport-side authorization is inferred.
    ///
    /// # Errors
    /// Serialization failure; protocol and backend refusals otherwise stay on the wire.
    pub fn handle(
        &mut self,
        source: &str,
        backend: &mut impl Backend,
    ) -> Result<Option<String>, Refusal> {
        let request = RuntimeValue::from_json_text(source)
            .map_err(Refusal::host)
            .and_then(|v| norn_ir::runtime::decode::<Request>(&v).map_err(Refusal::host));
        let Ok(request) = request else {
            return protocol_error(
                RuntimeValue::Null,
                -32600,
                Refusal::invalid("send one JSON-RPC 2.0 object"),
            )
            .map(Some);
        };
        let Some(id) = request.id else {
            return Ok(None);
        };
        if request.jsonrpc != "2.0"
            || !matches!(id, RuntimeValue::String(_) | RuntimeValue::Number(_))
        {
            return protocol_error(
                RuntimeValue::Null,
                -32600,
                Refusal::invalid("JSON-RPC 2.0 requires a string or number request id"),
            )
            .map(Some);
        }
        let params = match norn_ir::runtime::decode::<Parameters>(&request.params) {
            Ok(params) => params,
            Err(_parameter_error) => {
                return protocol_error(
                    id,
                    -32602,
                    Refusal::invalid("params are malformed; use the advertised object schema"),
                )
                .map(Some);
            }
        };
        if request.method == "ping" {
            return response(id, "result", empty_parameters()).map(Some);
        }
        if request.method == "initialize" {
            return self
                .initialize(id, params.protocol_version.as_deref())
                .map(Some);
        }
        if !self.initialized {
            return protocol_error(
                id,
                -32000,
                Refusal::invalid("initialize this connection before discovering or calling tools"),
            )
            .map(Some);
        }
        self.dispatch(id, &request.method, params, backend)
    }
    fn dispatch(
        &self,
        id: RuntimeValue,
        method: &str,
        params: Parameters,
        backend: &mut impl Backend,
    ) -> Result<Option<String>, Refusal> {
        if method == "tools/list" {
            return match self.catalog(params.cursor.as_deref(), params.limit) {
                Ok(page) => response(id, "result", page).map(Some),
                Err(error) => protocol_error(id, -32602, error).map(Some),
            };
        }
        if method != "tools/call" {
            return protocol_error(id, -32601, Refusal::invalid("use tools/list or tools/call"))
                .map(Some);
        }
        let name = params.name.as_deref().unwrap_or_default();
        if !self.tools.iter().any(|tool| tool.name == name) {
            return protocol_error(
                id,
                -32602,
                Refusal::invalid("unknown tool; discover this connection's tools/list catalog"),
            )
            .map(Some);
        }
        let mut result = match backend.call(name, params.arguments, params.metadata) {
            Ok(output) => output,
            Err(refusal) => CallOutput {
                value: encode(&refusal)?,
                is_error: true,
            },
        };
        if !matches!(&result.value, RuntimeValue::Object(_)) {
            result = CallOutput {
                value: encode(&Refusal::host("backend output must be an object"))?,
                is_error: true,
            };
        }
        let text = document(&result.value)?;
        response(
            id,
            "result",
            encode(&ToolOutput {
                content: vec![TextContent {
                    r#type: "text",
                    text,
                }],
                structured_content: result.value,
                is_error: result.is_error,
            })?,
        )
        .map(Some)
    }
    fn initialize(&mut self, id: RuntimeValue, protocol: Option<&str>) -> Result<String, Refusal> {
        let Some(requested) = protocol.filter(|version| !version.trim().is_empty()) else {
            return protocol_error(
                id,
                -32602,
                Refusal::invalid("provide an MCP protocolVersion"),
            );
        };
        self.initialized = true;
        let version = PROTOCOL_VERSIONS
            .iter()
            .find(|supported| **supported == requested)
            .unwrap_or(&PROTOCOL_VERSIONS[0]);
        let mut capabilities = self.capabilities.clone();
        capabilities.insert(
            "tools".into(),
            RuntimeValue::Object(RuntimeObject::from([(
                "listChanged".into(),
                RuntimeValue::Bool(false),
            )])),
        );
        let mut result = RuntimeObject::from([
            ("protocolVersion".into(), RuntimeValue::String((*version).into())),
            ("capabilities".into(), RuntimeValue::Object(capabilities)),
            (
                "serverInfo".into(),
                RuntimeValue::Object(RuntimeObject::from([
                    ("name".into(), RuntimeValue::String("norn".into())),
                    (
                        "version".into(),
                        RuntimeValue::String(env!("CARGO_PKG_VERSION").into()),
                    ),
                ])),
            ),
        ]);
        if let Some(instructions) = &self.instructions {
            result.insert(
                "instructions".into(),
                RuntimeValue::String(instructions.clone()),
            );
        }
        response(id, "result", RuntimeValue::Object(result))
    }
    /// Read the same bounded catalog snapshot used by MCP discovery.
    /// MCP-CATALOG-1: human services and MCP share tool IR, digests and cursors.
    ///
    /// # Errors
    /// Invalid page bounds, malformed cursors, or a changed authority/revision binding.
    pub fn catalog(
        &self,
        cursor: Option<&str>,
        limit: Option<usize>,
    ) -> Result<RuntimeValue, Refusal> {
        let limit = limit.unwrap_or(32);
        if !(1..=100).contains(&limit) {
            return Err(Refusal::invalid("limit must be between 1 and 100"));
        }
        let offset = match cursor {
            None => 0,
            Some(cursor) => {
                let (digest, index) = cursor
                    .split_once(':')
                    .ok_or_else(|| Refusal::invalid("invalid cursor; restart tools/list"))?;
                let offset = index.parse::<usize>().map_err(|_parse_error| {
                    Refusal::invalid("invalid cursor; restart tools/list")
                })?;
                if digest != self.digest || offset == 0 || offset >= self.tools.len() {
                    return Err(Refusal::invalid(
                        "catalog or authority changed; restart tools/list",
                    ));
                }
                offset
            }
        };
        let end = offset.saturating_add(limit).min(self.tools.len());
        encode(&Page {
            tools: self.tools.get(offset..end).unwrap_or_default(),
            next_cursor: (end < self.tools.len()).then(|| format!("{}:{end}", self.digest)),
            catalog_digest: &self.digest,
        })
    }
}
