//! `POST /v1/mcp`: the cell's tools over MCP's stateless Streamable HTTP
//! transport, for the chat assistants a person already uses.
//!
//! The catalog is projected from what the cell declares, never written per
//! screen: the route-backed tools of the server's manifest (TOOL-MANIFEST-1,
//! [`norn_api::tool_catalog`], read by
//! [`norn_projections::tool_manifest`]) over its object types, admitted
//! questions and retained definitions, plus one tool per action the caller
//! may call ([`norn_api::action_catalog::callable_actions`], projected by
//! [`norn_projections::mcp::action_tool`]). Every tool call is a loopback
//! through [`crate::server::handle_all`] with the caller's own
//! `Authorization` header and scope: the same routes, principal, policy and
//! refusals a direct HTTP caller gets. Nothing here reads data, decides access
//! or accepts SQL, table or column names (#733). The one tool without a route
//! is `norn_explain`, which reads only the checker's code registry.
//!
//! The protocol itself is [`norn_api::mcp::Session`]. Each POST is answered
//! on its own over the catalog rebuilt for its caller; tools that render in a
//! chat carry the MCP Apps `ui.resourceUri` key (and its Apps SDK alias),
//! whose static templates the edge serves.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use http::StatusCode;
use norn_api::action_catalog::callable_actions;
use norn_api::actions::ActionQuery;
use norn_api::definition_routes::DefinitionKinds;
use norn_api::host::DevHost;
use norn_api::mcp::{Backend, CallOutput, Session};
use norn_api::question_routes::AdHocQuestions;
use norn_api::refusal::Refusal;
use norn_cell::engine::SqlEngine;
use norn_ir::{JsonObject, JsonValue, McpTool, RuntimeObject, RuntimeValue};
use norn_projections::tool_manifest::{
    Domains, Encoding, Location, Offered, RouteTool, ToolArgument, ToolManifest, ToolMethod,
    route_tool,
};
use serde_json::json;

use crate::principal::{PrincipalResolver, RequestScope};
use crate::server::{Request, Response, handle_all};

/// The MCP endpoint's path.
pub const MCP_PATH: &str = "/v1/mcp";

/// The MCP Apps extension identifier a server declares in `initialize`.
pub const UI_EXTENSION: &str = "io.modelcontextprotocol/ui";

/// The MIME type of an MCP Apps template resource.
pub const UI_MIME_TYPE: &str = "text/html;profile=mcp-app";

/// Longest accepted tool name; both assistants refuse longer function names.
const MAX_TOOL_NAME: usize = 64;

/// Digest characters that disambiguate a colliding or overlong tool name.
const SUFFIX: usize = 10;

/// The views a widget renders, each a static `ui://` template the edge serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    /// An answer card with its chart and freshness.
    Answer,
    /// A lineage trace from a value to its source records.
    Lineage,
    /// A proposed dashboard or question definition and its checker verdict.
    Dashboard,
    /// An action's receipt: accepted with its effects, or refused with why.
    Receipt,
    /// A page of records of one type, as a table.
    Records,
    /// Held-back records and the findings that hold them: law, values, remedy.
    Findings,
    /// A planned definitions change: what it adds, replaces and finds.
    Plan,
    /// The caller's work queue: held-back groups, decisions, failing sources.
    Inbox,
}

impl View {
    /// The template's `ui://` URI.
    #[must_use]
    pub const fn uri(self) -> &'static str {
        match self {
            Self::Answer => "ui://norn/answer.html",
            Self::Lineage => "ui://norn/lineage.html",
            Self::Dashboard => "ui://norn/dashboard.html",
            Self::Receipt => "ui://norn/receipt.html",
            Self::Records => "ui://norn/records.html",
            Self::Findings => "ui://norn/findings.html",
            Self::Plan => "ui://norn/plan.html",
            Self::Inbox => "ui://norn/inbox.html",
        }
    }

    /// The view a tool declaration names (`x-norn-tool.view`).
    #[must_use]
    pub fn named(name: &str) -> Option<Self> {
        match name {
            "answer" => Some(Self::Answer),
            "lineage" => Some(Self::Lineage),
            "dashboard" => Some(Self::Dashboard),
            "receipt" => Some(Self::Receipt),
            "records" => Some(Self::Records),
            "findings" => Some(Self::Findings),
            "plan" => Some(Self::Plan),
            "inbox" => Some(Self::Inbox),
            _ => None,
        }
    }

    /// Every view, in a fixed order.
    pub const ALL: [Self; 8] = [
        Self::Answer,
        Self::Lineage,
        Self::Dashboard,
        Self::Receipt,
        Self::Records,
        Self::Findings,
        Self::Plan,
        Self::Inbox,
    ];
}

/// What the model is told about this server in `initialize`.
const INSTRUCTIONS: &str = "Norn is the system of record for this workspace. \
Read it only through these tools: norn_catalog summarizes its object types, laws, actions and questions; \
norn_inbox lists what needs the caller's attention (held-back records with their fixes, decisions, failing sources); \
norn_query_objects, norn_get_object and norn_profile_type read records the caller may see; norn_list_held \
lists the records Norn holds back and norn_get_findings says why (the law, the values it compared and the \
action tools that fix it); norn_answer answers an admitted question with its freshness, and \
norn_question_catalog says what else can be asked; norn_trace_answer, norn_trace_evaluated and \
norn_trace_object trace a value to its source records; norn_explain turns any refusal or diagnostic code \
into guidance. Actions change state only through their own tools (or norn_call_action with an action id, for one installed during the conversation): each call is decided by the workspace's \
laws and policy and comes back accepted with a receipt or refused with the reason; a refusal is an answer, \
so read it, fix the input and call again rather than guessing. Decimal values travel as exact strings; \
never round them. There is no SQL or table access.";

/// Serve one MCP POST. Notifications answer 202 with no body.
pub fn serve<E: SqlEngine>(
    host: &DevHost<E>,
    principals: &dyn PrincipalResolver,
    questions: Option<&dyn AdHocQuestions<E>>,
    kinds: &DefinitionKinds,
    request: &Request<'_>,
) -> Response {
    match session(host, principals, questions, kinds, request) {
        Ok(Some(body)) => Response {
            status: StatusCode::OK.as_u16(),
            body,
        },
        Ok(None) => Response {
            status: StatusCode::ACCEPTED.as_u16(),
            body: String::new(),
        },
        Err(refused) => crate::incident::respond_at(refused, request.now),
    }
}

fn session<E: SqlEngine>(
    host: &DevHost<E>,
    principals: &dyn PrincipalResolver,
    questions: Option<&dyn AdHocQuestions<E>>,
    kinds: &DefinitionKinds,
    request: &Request<'_>,
) -> Result<Option<String>, Refusal> {
    if request.method != "POST" {
        return Err(Refusal::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "route.method_not_allowed",
            "the route does not accept this method",
            "MCP messages are sent with POST; this server opens no event stream",
        ));
    }
    let scope = scope_query(request.query)?;
    // Nothing is answered, not even `initialize`, before the caller is verified
    // and the scope is this cell's: the same opaque denial the read routes give.
    // The catalog is built under that principal, which ends before any tool
    // call re-enters the routes with its own.
    let catalog = {
        let _request = RequestScope::new(principals);
        let actor = principals.resolve(request.authorization, request.now);
        let admitted = host
            .scope()
            .admit(Some(&scope.org_id), Some(&scope.environment_id));
        let (Some(actor), Ok(())) = (actor.as_deref(), admitted) else {
            return Err(norn_api::authz::read_denied());
        };
        Catalog::build(host, questions, manifest()?, &scope, actor)?
    };
    let mut session = Session::new(catalog.tools, &catalog.binding)?
        .stateless()
        .with_capabilities(capabilities())
        .with_instructions(INSTRUCTIONS);
    let mut backend = Loopback {
        host,
        principals,
        questions,
        kinds,
        request,
        scope,
        manifest: manifest()?,
        actions: catalog.actions,
        remedies: catalog.remedies,
        question_names: catalog.question_names,
        version: catalog.version,
    };
    norn_api::mcp::post_message(&mut session, request.body, &mut backend)
}

/// Scope from the query string: exactly `orgId` and `environmentId`.
#[derive(Debug, Clone)]
struct ScopeQuery {
    org_id: String,
    environment_id: String,
}

impl ScopeQuery {
    fn pairs(&self) -> Vec<(&'static str, String)> {
        vec![
            ("orgId", self.org_id.clone()),
            ("environmentId", self.environment_id.clone()),
        ]
    }
}

fn scope_query(query: &str) -> Result<ScopeQuery, Refusal> {
    let mut org_id = None;
    let mut environment_id = None;
    for (key, value) in form_urlencoded::parse(query.as_bytes()) {
        let slot = match key.as_ref() {
            "orgId" => &mut org_id,
            "environmentId" => &mut environment_id,
            _ => return Err(Refusal::invalid(format!("unknown query parameter {key}"))),
        };
        if slot.replace(value.into_owned()).is_some() {
            return Err(Refusal::invalid(format!("repeated query parameter {key}")));
        }
    }
    match (org_id, environment_id) {
        (Some(org_id), Some(environment_id)) => Ok(ScopeQuery {
            org_id,
            environment_id,
        }),
        _ => Err(norn_api::authz::read_denied()),
    }
}

fn capabilities() -> RuntimeObject {
    RuntimeObject::from([
        (
            "resources".into(),
            RuntimeValue::Object(RuntimeObject::from([(
                "listChanged".into(),
                RuntimeValue::Bool(false),
            )])),
        ),
        (
            "extensions".into(),
            RuntimeValue::Object(RuntimeObject::from([(
                UI_EXTENSION.into(),
                RuntimeValue::Object(RuntimeObject::from([(
                    "mimeTypes".into(),
                    RuntimeValue::Array(vec![RuntimeValue::String(UI_MIME_TYPE.into())]),
                )])),
            )])),
        ),
    ])
}

/// One caller's catalog: tools, the action each action tool calls, and the
/// binding its cursor digest covers (scope, actor, definitions version).
struct Catalog {
    tools: Vec<McpTool>,
    actions: BTreeMap<String, String>,
    /// For each object type, the action tools that write it: a finding's remedies.
    remedies: BTreeMap<String, Vec<String>>,
    question_names: Vec<String>,
    version: u64,
    binding: String,
}

impl Catalog {
    fn build<E: SqlEngine>(
        host: &DevHost<E>,
        questions: Option<&dyn AdHocQuestions<E>>,
        manifest: &ToolManifest,
        scope: &ScopeQuery,
        actor: &str,
    ) -> Result<Self, Refusal> {
        let object_types = host.object_type_names()?;
        let question_names = if let Some(runtime) = questions {
            runtime.question_names()
        } else {
            Vec::new()
        };
        // The definition tools are offered only to a caller the cell lets write
        // definitions; the routes decide every call again.
        let writer = norn_api::definition_routes::authorize(
            host,
            &norn_api::scope::ScopeQuery {
                org_id: Some(scope.org_id.clone()),
                environment_id: Some(scope.environment_id.clone()),
            },
            Some(actor),
            norn_api::authz::Operation::WriteDefinitions,
        )
        .is_ok();
        let domains = Domains {
            object_types: &object_types,
            questions: &question_names,
        };
        let mut tools = route_tools(manifest, &domains, questions.is_some(), writer)?;
        tools.push(explain_tool()?);
        if questions.is_some() {
            tools.push(call_action_tool()?);
        }
        let query = ActionQuery {
            org_id: Some(scope.org_id.clone()),
            environment_id: Some(scope.environment_id.clone()),
            version: None,
        };
        // A caller who may call no action is offered none, not refused the catalog.
        let callable = callable_actions(host, &query, Some(actor)).ok();
        let version = callable.as_ref().map_or(0, |callable| callable.version);
        let mut actions = BTreeMap::new();
        let mut remedies: BTreeMap<String, Vec<String>> = BTreeMap::new();
        if questions.is_some() {
            for (_, action) in callable.map_or_default(|callable| callable.actions) {
                let actor_fields = norn_kernel::actor_derived_fields(&action);
                let Ok(mut tool) =
                    norn_projections::mcp::action_tool_with_server_filled(&action, &actor_fields)
                else {
                    continue;
                };
                let name = tool_name(&action.action_id, &actions, &tools);
                let meta = tool.meta.get_or_insert_with(JsonObject::new);
                meta.insert(
                    "norn/actionId".into(),
                    JsonValue::String(action.action_id.clone()),
                );
                with_view(meta, View::Receipt);
                tool.name.clone_from(&name);
                if tool
                    .description
                    .as_ref()
                    .is_none_or(|text| text.trim().is_empty())
                {
                    tool.description = Some(format!(
                        "Call the governed action {}. The workspace's laws and policy decide it: \
                         the answer is a receipt or a refusal with its reason.",
                        action.action_id
                    ));
                }
                for write in &action.writes {
                    let tools = remedies.entry(write.entity_kind.clone()).or_default();
                    if !tools.contains(&name) {
                        tools.push(name.clone());
                    }
                }
                actions.insert(name, action.action_id);
                tools.push(tool);
            }
        }
        let binding = format!(
            "{}/{}|{actor}|defs@{version}",
            scope.org_id, scope.environment_id
        );
        Ok(Self {
            tools,
            actions,
            remedies,
            question_names,
            version,
            binding,
        })
    }
}

/// A function name both assistants accept (`[A-Za-z0-9_-]{1,64}`), unique in
/// the catalog: `act_` and the action id with every other character as `_`,
/// suffixed by a digest of the id when that collides or is too long.
fn tool_name(action_id: &str, taken: &BTreeMap<String, String>, tools: &[McpTool]) -> String {
    let plain: String = std::iter::once("act_")
        .flat_map(str::chars)
        .chain(action_id.chars())
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let used = |name: &str| taken.contains_key(name) || tools.iter().any(|tool| tool.name == name);
    if plain.len() <= MAX_TOOL_NAME && !used(&plain) {
        return plain;
    }
    let digest = norn_ir::canonical::HashProfile::Norn1.hash_canonical_bytes(action_id.as_bytes());
    let suffix: String = digest
        .rsplit(':')
        .next()
        .unwrap_or_default()
        .chars()
        .take(SUFFIX)
        .collect();
    let head: String = plain
        .chars()
        .take(MAX_TOOL_NAME.saturating_sub(SUFFIX.saturating_add(1)))
        .collect();
    format!("{head}_{suffix}")
}

/// Mark a tool as rendering `view`: the MCP Apps key and the Apps SDK keys.
fn with_view(meta: &mut JsonObject, view: View) {
    let uri = JsonValue::String(view.uri().into());
    meta.insert(
        "ui".into(),
        JsonValue::Object(JsonObject::from([("resourceUri".into(), uri.clone())])),
    );
    // ChatGPT reads this alias of `ui.resourceUri` too.
    meta.insert("openai/outputTemplate".into(), uri);
}

fn tool(value: serde_json::Value, view: Option<View>) -> Result<McpTool, Refusal> {
    let mut tool: McpTool = serde_json::from_value(value).map_err(Refusal::host)?;
    if let Some(view) = view {
        with_view(tool.meta.get_or_insert_with(JsonObject::new), view);
    }
    Ok(tool)
}

fn read_only() -> serde_json::Value {
    json!({"readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false})
}

/// TOOL-MANIFEST-1: the route-backed tools, read once from the routes' own
/// `OpenAPI` operations ([`norn_api::tool_catalog::tool_operations`]).
fn manifest() -> Result<&'static ToolManifest, Refusal> {
    static MANIFEST: OnceLock<Result<ToolManifest, String>> = OnceLock::new();
    MANIFEST
        .get_or_init(|| {
            let operations =
                norn_api::tool_catalog::tool_operations().map_err(|error| error.to_string())?;
            let operations: JsonValue =
                serde_json::from_value(operations).map_err(|error| error.to_string())?;
            ToolManifest::from_openapi_paths(&operations).map_err(|error| error.to_string())
        })
        .as_ref()
        .map_err(Refusal::host)
}

/// The manifest's tools this caller is offered, each projected over the
/// caller's workspace and marked with the view its declaration names.
fn route_tools(
    manifest: &ToolManifest,
    domains: &Domains<'_>,
    answers: bool,
    writer: bool,
) -> Result<Vec<McpTool>, Refusal> {
    let mut tools = Vec::new();
    for declared in manifest.tools() {
        let offered = match declared.offered {
            Offered::Always => true,
            Offered::ToDefinitionWriters => writer,
            Offered::WithQuestions => answers,
        };
        if !offered {
            continue;
        }
        let mut tool = route_tool(declared, domains);
        if let Some(name) = &declared.view {
            let view = View::named(name).ok_or_else(|| {
                Refusal::host(format!(
                    "{} declares the unknown view {name}",
                    declared.name
                ))
            })?;
            with_view(tool.meta.get_or_insert_with(JsonObject::new), view);
        }
        tools.push(tool);
    }
    Ok(tools)
}

/// `norn_explain`: guidance for a refusal or diagnostic code, from the
/// checker's registry. It has no route; it reads nothing of the cell.
fn explain_tool() -> Result<McpTool, Refusal> {
    tool(
        json!({
            "name": "norn_explain",
            "title": "Explain a code",
            "description": "Guidance for a refusal or checker diagnostic code (for example core.laws.violated, authz.denied, defs.refused): what it means and what to do next.",
            "inputSchema": {"type": "object", "additionalProperties": false, "required": ["code"], "properties": {
                "code": {"type": "string", "description": "The code field of a refusal or diagnostic."}
            }},
            "annotations": read_only(),
        }),
        None,
    )
}

/// `norn_call_action`: any action the caller may call, by id, through
/// `/v1/actions/{actionId}` (an action installed during a conversation is
/// callable before its own tool appears).
fn call_action_tool() -> Result<McpTool, Refusal> {
    tool(
        json!({
            "name": "norn_call_action",
            "title": "Call an action by id",
            "description": "Call any governed action you may call, by its action id (norn_catalog lists them; an action installed during this conversation is callable here before its own tool appears). The input is the action's input object; fields the server fills (the actor, the clock) are not yours to send. The workspace's laws and policy decide it: the answer is a receipt or a refusal with its reason.",
            "inputSchema": {"type": "object", "additionalProperties": false, "required": ["action", "input"], "properties": {
                "action": {"type": "string", "description": "An action id, for example lab.batch.record."},
                "input": {"type": "object", "description": "The action's input; decimals as exact strings."},
                "trace": {
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["traceId"],
                    "description": "Optional provenance trace context persisted on the action receipt (no raw transcript text; sessionDigest is sha256:<64 hex>).",
                    "properties": {
                        "traceId": {"type": "string", "minLength": 1, "maxLength": 256},
                        "intentRef": {"type": "string", "minLength": 1, "maxLength": 512},
                        "sessionDigest": {"type": "string", "pattern": "^sha256:[0-9a-f]{64}$"}
                    }
                }
            }},
            "annotations": {"readOnlyHint": false, "destructiveHint": true, "idempotentHint": true, "openWorldHint": false},
        }),
        Some(View::Receipt),
    )
}

/// Tool calls re-enter the cell's own routes as the same caller.
struct Loopback<'a, E: SqlEngine> {
    host: &'a DevHost<E>,
    principals: &'a dyn PrincipalResolver,
    questions: Option<&'a dyn AdHocQuestions<E>>,
    kinds: &'a DefinitionKinds,
    request: &'a Request<'a>,
    scope: ScopeQuery,
    manifest: &'static ToolManifest,
    actions: BTreeMap<String, String>,
    remedies: BTreeMap<String, Vec<String>>,
    question_names: Vec<String>,
    version: u64,
}

/// One routed call: method, path, query pairs and body.
struct Routed {
    method: &'static str,
    path: String,
    query: Vec<(&'static str, String)>,
    body: String,
}

fn text(arguments: &RuntimeObject, name: &str) -> Result<Option<String>, Refusal> {
    match arguments.get(name) {
        None | Some(RuntimeValue::Null) => Ok(None),
        Some(RuntimeValue::String(value)) => Ok(Some(value.clone())),
        Some(RuntimeValue::Number(number)) => Ok(Some(number.to_string())),
        Some(RuntimeValue::Bool(_) | RuntimeValue::Array(_) | RuntimeValue::Object(_)) => {
            Err(Refusal::invalid(format!("{name} must be a string")))
        }
    }
}

fn required(arguments: &RuntimeObject, name: &str) -> Result<String, Refusal> {
    text(arguments, name)?.ok_or_else(|| Refusal::invalid(format!("{name} is required")))
}

fn json_text(value: &RuntimeValue) -> Result<String, Refusal> {
    String::from_utf8(value.canonical_json().map_err(Refusal::host)?).map_err(Refusal::host)
}

/// Bytes a path segment escapes: those that would end or restructure it.
/// Question names are matched undecoded, so `.` and `_` stay as they are.
const SEGMENT: &percent_encoding::AsciiSet = &percent_encoding::CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'/')
    .add(b'?')
    .add(b'<')
    .add(b'>')
    .add(b'`')
    .add(b'{')
    .add(b'}');

fn segment(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, SEGMENT).to_string()
}

fn object(fields: Vec<(&str, RuntimeValue)>) -> RuntimeValue {
    RuntimeValue::Object(
        fields
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value))
            .collect(),
    )
}

impl<E: SqlEngine> Loopback<'_, E> {
    fn route(
        &self,
        name: &str,
        arguments: &RuntimeObject,
        metadata: &RuntimeObject,
    ) -> Result<Routed, Refusal> {
        if let Some(tool) = self.manifest.tool(name) {
            return self.bind(tool, arguments);
        }
        let scoped = self.scope.pairs();
        if name != "norn_call_action" {
            let action = self
                .actions
                .get(name)
                .ok_or_else(|| Refusal::invalid("unknown tool"))?;
            return self.action_call(action, arguments, None, metadata, scoped);
        }
        if let Some(unknown) = arguments
            .keys()
            .find(|key| !matches!(key.as_str(), "action" | "input" | "trace"))
        {
            return Err(Refusal::invalid(format!(
                "norn_call_action does not accept unexpected argument `{unknown}`; allowed arguments are action, input, and optional trace"
            )));
        }
        let action = required(arguments, "action")?;
        if !self.actions.values().any(|callable| *callable == action) {
            let mut callable: Vec<&str> = self.actions.values().map(String::as_str).collect();
            callable.sort_unstable();
            return Err(Refusal::invalid(format!(
                "{action} is not an action you may call here; you may call: {}",
                if callable.is_empty() {
                    "none".to_owned()
                } else {
                    callable.join(", ")
                }
            )));
        }
        let Some(RuntimeValue::Object(input)) = arguments.get("input") else {
            return Err(Refusal::invalid("input must be the action's input object"));
        };
        let arg_trace = arguments.get("trace").map(parse_trace).transpose()?;
        self.action_call(&action, input, arg_trace.as_ref(), metadata, scoped)
    }
}

impl<E: SqlEngine> Loopback<'_, E> {
    /// A route-backed tool's call as its route's own request: each argument
    /// where the tool's declaration puts it, under the caller's scope.
    fn bind(&self, tool: &'static RouteTool, arguments: &RuntimeObject) -> Result<Routed, Refusal> {
        let mut query = self.scope.pairs();
        let mut segments = BTreeMap::new();
        let mut body = RuntimeObject::new();
        for argument in &tool.arguments {
            let name = argument.name.as_str();
            match argument.location {
                Location::Local => {}
                Location::Path => {
                    segments.insert(name, segment(&required(arguments, name)?));
                }
                Location::Query => {
                    if let Some(value) = query_value(argument, arguments)? {
                        query.push((name, value));
                    }
                }
                Location::Body => {
                    if let Some(value) = body_value(argument, arguments)? {
                        body.insert(argument.name.clone(), value);
                    }
                }
            }
        }
        Ok(Routed {
            method: tool.method.as_str(),
            path: fill(&tool.path, &segments)?,
            query,
            body: match tool.method {
                ToolMethod::Get => String::new(),
                ToolMethod::Post => json_text(&RuntimeValue::Object(body))?,
            },
        })
    }

    /// The route of the manifest's tool `name`, called with `arguments`.
    fn routed(&self, name: &str, arguments: &RuntimeObject) -> Result<Routed, Refusal> {
        let tool = self
            .manifest
            .tool(name)
            .ok_or_else(|| Refusal::host(format!("the tool manifest has no {name}")))?;
        self.bind(tool, arguments)
    }
}

/// `template` with each `{name}` replaced by its encoded segment.
fn fill(template: &str, segments: &BTreeMap<&str, String>) -> Result<String, Refusal> {
    let mut path = String::new();
    let mut rest = template;
    while let Some((head, tail)) = rest.split_once('{') {
        let (name, after) = tail
            .split_once('}')
            .ok_or_else(|| Refusal::host(format!("{template} has an open segment")))?;
        let value = segments
            .get(name)
            .ok_or_else(|| Refusal::host(format!("{template}: {name} is unbound")))?;
        path.push_str(head);
        path.push_str(value);
        rest = after;
    }
    path.push_str(rest);
    Ok(path)
}

/// The refusal of a missing or malformed argument: the declaration's own
/// explanation, or that it must be `kind`.
fn malformed(argument: &ToolArgument, kind: &str) -> Refusal {
    Refusal::invalid(
        argument
            .refusal
            .clone()
            .unwrap_or_else(|| format!("{} must be {kind}", argument.name)),
    )
}

/// A query argument's text, `None` when absent.
fn query_value(
    argument: &ToolArgument,
    arguments: &RuntimeObject,
) -> Result<Option<String>, Refusal> {
    let name = argument.name.as_str();
    match argument.encoding {
        Encoding::Object => match arguments.get(name) {
            None | Some(RuntimeValue::Null) => Ok(None),
            Some(value @ RuntimeValue::Object(_)) => json_text(value).map(Some),
            Some(
                RuntimeValue::Bool(_)
                | RuntimeValue::Number(_)
                | RuntimeValue::String(_)
                | RuntimeValue::Array(_),
            ) => Err(malformed(argument, "an object")),
        },
        Encoding::Bucket => Ok(text(arguments, name)?.map(|bucket| bucket_key(&bucket))),
        Encoding::Text | Encoding::Value | Encoding::Array | Encoding::Number => {
            text(arguments, name)
        }
    }
}

/// A body argument that must hold one kind of value: itself, or `None` when
/// it is optional and absent.
fn shaped(
    argument: &ToolArgument,
    given: Option<&RuntimeValue>,
    fits: fn(&RuntimeValue) -> bool,
    kind: &str,
) -> Result<Option<RuntimeValue>, Refusal> {
    match given {
        None if !argument.required => Ok(None),
        Some(value) if fits(value) => Ok(Some(value.clone())),
        None | Some(_) => Err(malformed(argument, kind)),
    }
}

/// A body argument's value, `None` when it is optional and absent.
fn body_value(
    argument: &ToolArgument,
    arguments: &RuntimeObject,
) -> Result<Option<RuntimeValue>, Refusal> {
    let name = argument.name.as_str();
    let given = arguments.get(name);
    match argument.encoding {
        Encoding::Value => Ok(Some(given.cloned().unwrap_or_default())),
        Encoding::Text | Encoding::Bucket => {
            let value = if argument.required {
                Some(required(arguments, name)?)
            } else {
                text(arguments, name)?
            };
            Ok(value.map(|value| {
                RuntimeValue::String(if argument.encoding == Encoding::Bucket {
                    bucket_key(&value)
                } else {
                    value
                })
            }))
        }
        Encoding::Array => shaped(
            argument,
            given,
            |value| matches!(value, RuntimeValue::Array(_)),
            "an array",
        ),
        Encoding::Number => shaped(
            argument,
            given,
            |value| matches!(value, RuntimeValue::Number(_)),
            "a number",
        ),
        Encoding::Object => shaped(
            argument,
            given,
            |value| matches!(value, RuntimeValue::Object(_)),
            "an object",
        ),
    }
}

fn parse_trace(value: &RuntimeValue) -> Result<norn_ir::TraceContext, Refusal> {
    let trace: norn_ir::TraceContext = norn_ir::runtime::decode(value)
        .map_err(|error| Refusal::invalid(format!("trace is malformed: {error}")))?;
    trace
        .validate()
        .map_err(|error| Refusal::invalid(error.to_string()))?;
    Ok(trace)
}

impl<E: SqlEngine> Loopback<'_, E> {
    /// One governed action call through `/v1/actions/{action}`, pinned to the
    /// definitions version this catalog was built from.
    fn action_call(
        &self,
        action: &str,
        input: &RuntimeObject,
        arg_trace: Option<&norn_ir::TraceContext>,
        metadata: &RuntimeObject,
        mut scoped: Vec<(&'static str, String)>,
    ) -> Result<Routed, Refusal> {
        let meta_trace = metadata.get("norn/trace").map(parse_trace).transpose()?;
        let trace = match (arg_trace, meta_trace) {
            (Some(left), Some(right)) if *left != right => {
                return Err(Refusal::invalid(
                    "conflicting trace context in arguments and _meta",
                ));
            }
            (Some(trace), _) => Some(trace.clone()),
            (None, meta) => meta,
        };
        let key = if let Some(RuntimeValue::String(key)) = metadata.get("norn/idempotencyKey") {
            key.clone()
        } else {
            idempotency_key(action, input, trace.as_ref())?
        };
        scoped.push(("version", self.version.to_string()));
        let mut fields = vec![
            ("input", RuntimeValue::Object(input.clone())),
            ("idempotencyKey", RuntimeValue::String(key)),
        ];
        if let Some(trace) = &trace {
            fields.push(("trace", trace.to_runtime_value()));
        }
        Ok(Routed {
            method: "POST",
            path: format!("/v1/actions/{}", segment(action)),
            query: scoped,
            body: json_text(&object(fields))?,
        })
    }
}

/// Without a caller-supplied key, a call's retry identity is its content: the
/// same action with the same input (and trace, when present) is one call, and
/// a host's automatic retry returns the original receipt instead of committing twice.
fn idempotency_key(
    action: &str,
    arguments: &RuntimeObject,
    trace: Option<&norn_ir::TraceContext>,
) -> Result<String, Refusal> {
    let mut fields = vec![
        ("action", RuntimeValue::String(action.to_owned())),
        ("input", RuntimeValue::Object(arguments.clone())),
    ];
    if let Some(trace) = trace {
        fields.push(("trace", trace.to_runtime_value()));
    }
    let content = json_text(&object(fields))?;
    let digest = norn_ir::canonical::HashProfile::Norn1.hash_canonical_bytes(content.as_bytes());
    Ok(format!("mcp:{digest}"))
}

/// A bucket as the answer keys it: the canonical JSON of its grouping value.
/// A plain value an agent copied from a label (`USD`) is read as that string.
fn bucket_key(bucket: &str) -> String {
    if serde_json::from_str::<serde_json::Value>(bucket).is_ok() {
        bucket.to_owned()
    } else {
        serde_json::Value::String(bucket.to_owned()).to_string()
    }
}

fn string(value: &str) -> RuntimeValue {
    RuntimeValue::String(value.to_owned())
}

fn field<'a>(value: &'a RuntimeValue, key: &str) -> Option<&'a str> {
    value.get(key).and_then(RuntimeValue::as_str)
}

/// The first sentence of a description, for a one-line summary.
fn line(text: &str) -> String {
    let trimmed = text.trim();
    let end = trimmed
        .char_indices()
        .find(|&(index, c)| {
            c == '\n'
                || (c == '.'
                    && trimmed
                        .get(index.saturating_add(1)..)
                        .is_none_or(|rest| rest.is_empty() || rest.starts_with(' ')))
        })
        .map_or(trimmed.len(), |(index, _)| index);
    trimmed.get(..end).unwrap_or(trimmed).to_owned()
}

/// `stock_cost` as `stock cost`, `docs.ScannedDocument` as `ScannedDocument`.
fn words(name: &str) -> String {
    name.rsplit('.').next().unwrap_or(name).replace('_', " ")
}

/// A human title for a derived metric, from its declared parts (never its
/// encoded name): `Holding stock cost, latest by observed at`.
fn metric_title(
    model: &str,
    expression: &str,
    time: &str,
    links: &[String],
    latest: bool,
) -> String {
    let path: Vec<String> = links
        .iter()
        .map(|link| words(link))
        .chain([words(time)])
        .collect();
    let how = if latest { "latest by" } else { "by" };
    format!(
        "{} {}, {how} {}",
        words(model),
        words(expression),
        path.join(" ")
    )
}

/// `norn_list_metrics` and `norn_question_catalog` with a `title` on each
/// metric: the same metrics, named for a person.
fn titled_metrics(mut value: RuntimeValue) -> RuntimeValue {
    let Some(RuntimeValue::Array(metrics)) = value
        .as_object_mut()
        .and_then(|root| root.get_mut("metrics"))
    else {
        return value;
    };
    for metric in metrics.iter_mut() {
        let snapshot = metric.clone();
        let Some(entry) = metric.as_object_mut() else {
            continue;
        };
        if entry.contains_key("title") {
            continue;
        }
        let title = if let Some(model) = field(&snapshot, "objectType") {
            // norn_list_metrics: objectType, expression, timeProperty, timeLinks, additivity.
            let links: Vec<String> = snapshot
                .get("timeLinks")
                .and_then(RuntimeValue::as_array)
                .map_or_default(|links| {
                    links
                        .iter()
                        .filter_map(|link| link.as_str().map(str::to_owned))
                        .collect()
                });
            let latest = snapshot
                .pointer("/additivity/aggregate")
                .and_then(RuntimeValue::as_str)
                == Some("last");
            metric_title(
                model,
                field(&snapshot, "expression").unwrap_or_default(),
                field(&snapshot, "timeProperty").unwrap_or_default(),
                &links,
                latest,
            )
        } else if let Some(params) = snapshot.pointer("/type_params/metric_aggregation_params") {
            // norn_question_catalog: MetricFlow-shaped type_params and x-norn.
            let links: Vec<String> = snapshot
                .pointer("/x-norn/timeDimension/links")
                .and_then(RuntimeValue::as_array)
                .map_or_default(|links| {
                    links
                        .iter()
                        .filter_map(|link| link.as_str().map(str::to_owned))
                        .collect()
                });
            let time = snapshot
                .pointer("/x-norn/timeDimension/property")
                .and_then(RuntimeValue::as_str)
                .or_else(|| field(params, "agg_time_dimension"))
                .unwrap_or_default();
            let latest = params
                .get("non_additive_dimension")
                .is_some_and(|value| !value.is_null());
            metric_title(
                field(params, "semantic_model").unwrap_or_default(),
                snapshot
                    .pointer("/type_params/expr")
                    .and_then(RuntimeValue::as_str)
                    .unwrap_or_default(),
                time,
                &links,
                latest,
            )
        } else {
            continue;
        };
        entry.insert("title".into(), RuntimeValue::String(title));
    }
    value
}

/// One declared type, one line: its name, what it is, its properties and links.
fn type_summary(id: &str, payload: &RuntimeValue) -> Option<RuntimeValue> {
    let declared = payload
        .get("interface")
        .or_else(|| payload.get("objectType"))
        .or_else(|| payload.get("entity"))?;
    let properties: Vec<RuntimeValue> = declared
        .get("properties")
        .and_then(RuntimeValue::as_array)
        .into_iter()
        .flatten()
        .filter_map(|property| field(property, "id").map(string))
        .collect();
    let links: Vec<RuntimeValue> = declared
        .get("links")
        .and_then(RuntimeValue::as_array)
        .into_iter()
        .flatten()
        .filter_map(|link| {
            Some(string(&format!(
                "{} -> {}",
                field(link, "id")?,
                field(link, "target")?
            )))
        })
        .collect();
    Some(object(vec![
        ("name", string(field(declared, "name").unwrap_or(id))),
        ("artifactId", string(id)),
        (
            "about",
            string(&line(field(declared, "description").unwrap_or_default())),
        ),
        ("properties", RuntimeValue::Array(properties)),
        ("links", RuntimeValue::Array(links)),
    ]))
}

/// `norn_catalog`'s default answer: every declared type, law, action and
/// question, one line each, with the version; the full artifacts stay behind
/// `detail: full`.
fn catalog_summary(
    catalog: &RuntimeValue,
    question_names: &[String],
    tools: &BTreeMap<String, String>,
) -> RuntimeValue {
    let mut types = Vec::new();
    let mut laws = Vec::new();
    let mut actions = Vec::new();
    let tool_of = |action_id: &str| {
        tools
            .iter()
            .find(|(_, id)| id.as_str() == action_id)
            .map(|(tool, _)| tool.clone())
    };
    for artifact in catalog
        .get("artifacts")
        .and_then(RuntimeValue::as_array)
        .into_iter()
        .flatten()
    {
        let kind = field(artifact, "kind").unwrap_or_default();
        let id = field(artifact, "artifactId").unwrap_or_default();
        let payload = artifact.get("payload").cloned().unwrap_or_default();
        match kind {
            "entity" => types.extend(type_summary(id, &payload)),
            "law" => laws.push(object(vec![
                ("id", string(field(&payload, "id").unwrap_or(id))),
                (
                    "about",
                    string(&line(
                        field(&payload, "description")
                            .or_else(|| field(&payload, "rationale"))
                            .unwrap_or_default(),
                    )),
                ),
            ])),
            "action" => {
                let action_id = field(&payload, "actionId").unwrap_or(id);
                let mut entry = vec![
                    ("id", string(action_id)),
                    (
                        "title",
                        string(field(&payload, "title").unwrap_or_default()),
                    ),
                ];
                if let Some(tool) = tool_of(action_id) {
                    entry.push(("tool", RuntimeValue::String(tool)));
                }
                actions.push(object(entry));
            }
            _ => {}
        }
    }
    object(vec![
        (
            "version",
            catalog.get("version").cloned().unwrap_or_default(),
        ),
        (
            "admitted",
            catalog.get("admitted").cloned().unwrap_or_default(),
        ),
        ("types", RuntimeValue::Array(types)),
        ("laws", RuntimeValue::Array(laws)),
        ("actions", RuntimeValue::Array(actions)),
        (
            "questions",
            RuntimeValue::Array(question_names.iter().map(|name| string(name)).collect()),
        ),
        (
            "more",
            string(
                "Actions without a tool are not callable by you. For an artifact's full definition call norn_catalog with detail \"full\" and its names; for what can be asked, norn_question_catalog.",
            ),
        ),
    ])
}

/// `norn_catalog` with `detail: full`: the route's document with only the
/// artifacts the caller named. The host's built-in pages are not the
/// workspace's definitions (the summary lists none of them), so none is
/// returned, even when named.
fn catalog_full(mut catalog: RuntimeValue, names: &[String]) -> RuntimeValue {
    if let Some(RuntimeValue::Array(artifacts)) = catalog
        .as_object_mut()
        .and_then(|root| root.get_mut("artifacts"))
    {
        artifacts.retain(|artifact| {
            let id = field(artifact, "artifactId").unwrap_or_default();
            let name = field(artifact, "name").unwrap_or_default();
            names
                .iter()
                .any(|wanted| id.contains(wanted.as_str()) || name.contains(wanted.as_str()))
        });
    }
    if let Some(root) = catalog.as_object_mut() {
        // The full admission receipt restates every artifact; the digest names it.
        root.remove("receipt");
        // Studio's built-in pages are the host's, served for Studio to draw records with.
        root.remove("builtins");
    }
    catalog
}

/// Guidance for runtime refusal codes, which the checker's registry does not
/// carry (it holds authoring diagnostics).
const RUNTIME_GUIDANCE: &[(&str, &str, &str)] = &[
    (
        "core.laws.violated",
        "A law of the workspace refused the change: the record would break it.",
        "Read the refusal's reason (the law and the values it compared); correct the input so the law holds and call again. norn_get_findings shows the law and values for a held record.",
    ),
    (
        "authz.denied",
        "Your role may not do this here, or the request names a scope you are not in.",
        "Do not retry. Ask an owner or admin of the workspace; the policy decides who may call each action.",
    ),
    (
        "action.invalid_input",
        "The input does not match the action's declared schema.",
        "Read the path in the explanation, fix that field (decimals are exact strings like \"72.00\") and call again; do not send fields the schema does not list.",
    ),
    (
        "object.not_found",
        "No record of that type and id is readable by you.",
        "Check the type and id. A record a law holds back is not returned by norn_get_object; look for it with norn_list_held.",
    ),
    (
        "question.not_found",
        "No admitted question has that name.",
        "Pick a name from norn_question_catalog, or ask a new question with norn_evaluate.",
    ),
    (
        "request.invalid",
        "The request is malformed: a required field is missing or has the wrong type.",
        "Read the explanation for the field, supply it and call again.",
    ),
    (
        "core.projection.lineage_incomplete",
        "The trace cannot be computed for that lineageRef and bucket.",
        "Pass the lineageRef from the answer you are tracing and a bucket key that answer returned.",
    ),
    (
        "action.receipts_unconfigured",
        "The cell cannot commit governed actions now: its host has no receipt key.",
        "Retry later; the operator must configure the host.",
    ),
    (
        "defs.refused",
        "The checker refused the definitions change; nothing was written.",
        "Fix each diagnostic in the refusal (norn_explain explains each code), recompile and plan again.",
    ),
    (
        "defs.live_data_refused",
        "Stored records would break the new laws, so the change is held back.",
        "Correct those records first (each finding names the object and law), or change the law, then plan again.",
    ),
    (
        "defs.live_data_unbounded",
        "Too many stored records to check the new laws in one plan.",
        "Split the change so fewer types' laws change at once.",
    ),
    (
        "defs.not_installed",
        "The workspace has no active definitions to change.",
        "Install the workspace's first definitions from Norn Studio.",
    ),
];

/// `norn_explain`: a code's meaning and next step, from the checker's
/// diagnostic registry or the runtime refusals above.
fn explain(code: &str) -> RuntimeValue {
    if let Some(entry) = norn_checker::codes::lookup(code) {
        let mut fields = vec![
            ("code", string(entry.code)),
            ("known", RuntimeValue::Bool(true)),
            ("source", string("checker")),
            ("tier", string(entry.tier.as_str())),
            (
                "severity",
                string(norn_checker::codes::severity_str(&entry.severity)),
            ),
            ("meaning", string(entry.message)),
        ];
        if let Some(fix) = entry.fix {
            fields.push(("fix", string(fix)));
        }
        if let Some(no_fix) = entry.no_fix {
            fields.push(("noFix", string(no_fix)));
        }
        return object(fields);
    }
    if let Some((code, meaning, fix)) = RUNTIME_GUIDANCE.iter().find(|(known, ..)| *known == code) {
        return object(vec![
            ("code", string(code)),
            ("known", RuntimeValue::Bool(true)),
            ("source", string("runtime")),
            ("meaning", string(meaning)),
            ("fix", string(fix)),
        ]);
    }
    object(vec![
        ("code", string(code)),
        ("known", RuntimeValue::Bool(false)),
        (
            "fix",
            string(
                "No guidance is recorded for this code; the refusal's message and explanation say what was refused.",
            ),
        ),
    ])
}

/// Held records listed in one answer, each with its findings' summary.
const HELD_SUMMARIES: usize = 25;

impl<E: SqlEngine> Loopback<'_, E> {
    /// One loopback call through the cell's own routes, as this caller.
    fn send(&self, routed: &Routed) -> Result<(bool, RuntimeValue), Refusal> {
        let query = form_urlencoded::Serializer::new(String::new())
            .extend_pairs(
                routed
                    .query
                    .iter()
                    .map(|(key, value)| (*key, value.as_str())),
            )
            .finish();
        let response = handle_all(
            self.host,
            self.principals,
            self.questions,
            self.kinds,
            &Request {
                method: routed.method,
                path: &routed.path,
                query: &query,
                authorization: self.request.authorization,
                now: self.request.now,
                body: &routed.body,
            },
        );
        let value = RuntimeValue::from_json_text(&response.body).map_err(Refusal::host)?;
        // A route's success is any 2xx: a call an approval holds answers 202 (APPROVAL-1).
        Ok(((200..300).contains(&response.status), value))
    }

    /// An answer or trace of a question the workspace does not have: named as
    /// such when the caller may read the question catalog, else the route's
    /// own refusal.
    fn unknown_question(&self, arguments: &RuntimeObject) -> Result<Option<Refusal>, Refusal> {
        let Some(question) = text(arguments, "question")? else {
            return Ok(None);
        };
        if self.question_names.contains(&question) {
            return Ok(None);
        }
        let (readable, _) =
            self.send(&self.routed("norn_question_catalog", &RuntimeObject::new())?)?;
        if !readable {
            return Ok(None);
        }
        Ok(Some(Refusal::new(
            StatusCode::NOT_FOUND,
            "question.not_found",
            "no admitted question has this name",
            format!(
                "{question} is not an admitted question; the admitted ones are: {}. Ask a new one with norn_evaluate.",
                if self.question_names.is_empty() {
                    "none".to_owned()
                } else {
                    self.question_names.join(", ")
                }
            ),
        )))
    }

    /// The held records with a one-line summary of each one's open findings.
    fn held(
        &self,
        arguments: &RuntimeObject,
        mut listed: RuntimeValue,
    ) -> Result<RuntimeValue, Refusal> {
        let kind = required(arguments, "type")?;
        let Some(RuntimeValue::Array(items)) = listed
            .as_object_mut()
            .and_then(|root| root.get_mut("items"))
        else {
            return Ok(listed);
        };
        for item in items.iter_mut().take(HELD_SUMMARIES) {
            let Some(id) = field(item, "id").map(str::to_owned) else {
                continue;
            };
            let target = RuntimeObject::from([
                ("type".to_owned(), string(&kind)),
                ("id".to_owned(), RuntimeValue::String(id)),
            ]);
            let (ok, findings) = self.send(&self.routed("norn_get_findings", &target)?)?;
            if !ok {
                continue;
            }
            let summaries: Vec<RuntimeValue> = findings
                .get("findings")
                .and_then(RuntimeValue::as_array)
                .into_iter()
                .flatten()
                .filter(|finding| field(finding, "status") != Some("resolved"))
                .map(|finding| {
                    object(vec![
                        (
                            "law",
                            finding
                                .pointer("/ruleRef/lawId")
                                .cloned()
                                .unwrap_or_default(),
                        ),
                        (
                            "message",
                            finding.get("message").cloned().unwrap_or_default(),
                        ),
                        ("values", finding.get("values").cloned().unwrap_or_default()),
                    ])
                })
                .collect();
            if let Some(entry) = item.as_object_mut() {
                entry.insert("findings".into(), RuntimeValue::Array(summaries));
            }
        }
        if let Some(root) = listed.as_object_mut() {
            root.insert("remedies".into(), self.remedies_for(&kind));
        }
        Ok(listed)
    }

    fn remedies_for(&self, kind: &str) -> RuntimeValue {
        RuntimeValue::Array(
            self.remedies
                .get(kind)
                .into_iter()
                .flatten()
                .map(|tool| string(tool))
                .collect(),
        )
    }

    /// A record's findings with the action tools that may correct it.
    fn findings(
        &self,
        arguments: &RuntimeObject,
        mut found: RuntimeValue,
    ) -> Result<RuntimeValue, Refusal> {
        let kind = required(arguments, "type")?;
        if let Some(root) = found.as_object_mut() {
            root.insert("remedies".into(), self.remedies_for(&kind));
            root.insert(
                "next".into(),
                string("To fix it, call a remedy tool with corrected values (copy evidence ids from the finding); the laws decide again and answer with a receipt or the refusal's reason. norn_explain explains any code."),
            );
        }
        Ok(found)
    }
}

impl<E: SqlEngine> Backend for Loopback<'_, E> {
    fn call(
        &mut self,
        name: &str,
        arguments: RuntimeObject,
        metadata: RuntimeObject,
    ) -> Result<CallOutput, Refusal> {
        if name == "norn_explain" {
            return Ok(CallOutput {
                value: explain(&required(&arguments, "code")?),
                is_error: false,
            });
        }
        if matches!(name, "norn_answer" | "norn_trace_answer")
            && let Some(refusal) = self.unknown_question(&arguments)?
        {
            return Err(refusal);
        }
        let full = text(&arguments, "detail")?.as_deref() == Some("full");
        let names: Vec<String> = match arguments.get("names") {
            None | Some(RuntimeValue::Null) => Vec::new(),
            Some(RuntimeValue::Array(names)) => names
                .iter()
                .map(|name| {
                    name.as_str()
                        .map(str::to_owned)
                        .ok_or_else(|| Refusal::invalid("names must be strings"))
                })
                .collect::<Result<_, _>>()?,
            Some(_) => return Err(Refusal::invalid("names must be an array of strings")),
        };
        if name == "norn_catalog" && full && names.is_empty() {
            return Err(Refusal::invalid(
                "detail full needs names: the artifact ids or names to return (norn_catalog without detail lists them)",
            ));
        }
        let routed = self.route(name, &arguments, &metadata)?;
        let (ok, value) = self.send(&routed)?;
        if !ok {
            return Ok(CallOutput {
                value,
                is_error: true,
            });
        }
        let value = match name {
            "norn_catalog" if full => catalog_full(value, &names),
            "norn_catalog" => catalog_summary(&value, &self.question_names, &self.actions),
            "norn_list_metrics" | "norn_question_catalog" => titled_metrics(value),
            "norn_list_held" => self.held(&arguments, value)?,
            "norn_get_findings" => self.findings(&arguments, value)?,
            _ => value,
        };
        Ok(CallOutput {
            value,
            is_error: false,
        })
    }
}
