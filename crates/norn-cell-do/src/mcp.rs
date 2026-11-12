//! `POST /v1/mcp`: the cell's tools over MCP's stateless Streamable HTTP
//! transport, for the chat assistants a person already uses.
//!
//! The catalog is projected from what the cell declares, never written per
//! screen: generic operations over its object types, admitted questions and
//! retained definitions, plus one tool per action the caller may call
//! ([`norn_api::action_catalog::callable_actions`], projected by
//! [`norn_projections::mcp::action_tool`]). Every tool call is a loopback
//! through [`crate::server::handle_all`] with the caller's own
//! `Authorization` header and scope: the same routes, principal, policy and
//! refusals a direct HTTP caller gets. Nothing here reads data, decides access
//! or accepts SQL, table or column names (#733).
//!
//! The protocol itself is [`norn_api::mcp::Session`]. Each POST is answered
//! on its own over the catalog rebuilt for its caller; tools that render in a
//! chat carry the MCP Apps `ui.resourceUri` key (and its Apps SDK alias),
//! whose static templates the edge serves.

use std::collections::BTreeMap;

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
        }
    }

    /// Every view, in a fixed order.
    pub const ALL: [Self; 4] = [Self::Answer, Self::Lineage, Self::Dashboard, Self::Receipt];
}

/// What the model is told about this server in `initialize`.
const INSTRUCTIONS: &str = "Norn is the system of record for this workspace. \
Read it only through these tools: norn_catalog describes its object types, questions and actions; \
norn_query_objects, norn_get_object and norn_profile_type read records the caller may see; norn_answer \
answers an admitted question with its freshness, and norn_question_catalog says what else can be asked; norn_trace_answer and norn_trace_object trace a value to its source records. \
Actions change state only through their own tools: each call is decided by the workspace's laws and \
policy and comes back accepted with a receipt or refused with the reason. Decimal values travel as exact \
strings; never round them. There is no SQL or table access.";

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
        Err(refused) => Response {
            status: refused.status,
            body: serde_json::to_string(&refused).unwrap_or_else(|_| String::from("{}")),
        },
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
        Catalog::build(host, questions, &scope, actor)?
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
        actions: catalog.actions,
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
    version: u64,
    binding: String,
}

impl Catalog {
    fn build<E: SqlEngine>(
        host: &DevHost<E>,
        questions: Option<&dyn AdHocQuestions<E>>,
        scope: &ScopeQuery,
        actor: &str,
    ) -> Result<Self, Refusal> {
        let object_types = host.object_type_names()?;
        let question_names = if let Some(runtime) = questions {
            runtime.question_names()
        } else {
            Vec::new()
        };
        let mut tools = generic_tools(&object_types, &question_names, questions.is_some())?;
        let query = ActionQuery {
            org_id: Some(scope.org_id.clone()),
            environment_id: Some(scope.environment_id.clone()),
            version: None,
        };
        // A caller who may call no action is offered none, not refused the catalog.
        let callable = callable_actions(host, &query, Some(actor)).ok();
        let version = callable.as_ref().map_or(0, |callable| callable.version);
        let mut actions = BTreeMap::new();
        if questions.is_some() {
            for (_, action) in callable
                .map(|callable| callable.actions)
                .unwrap_or_default()
            {
                let Ok(mut tool) = norn_projections::mcp::action_tool(&action) else {
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

#[allow(
    clippy::too_many_lines,
    reason = "one declarative table of the generic tools, read top to bottom"
)]
fn generic_tools(
    object_types: &[String],
    question_names: &[String],
    answers: bool,
) -> Result<Vec<McpTool>, Refusal> {
    let type_schema = if object_types.is_empty() {
        json!({"type": "string", "description": "An object type from norn_catalog."})
    } else {
        json!({"type": "string", "enum": object_types, "description": "An object type this workspace declares."})
    };
    let question_schema = if question_names.is_empty() {
        json!({"type": "string", "description": "An admitted question name from norn_catalog."})
    } else {
        json!({"type": "string", "enum": question_names, "description": "An admitted question of this workspace."})
    };
    let mut tools = vec![
        tool(
            json!({
                "name": "norn_catalog",
                "title": "Describe the workspace",
                "description": "The active definitions of this Norn workspace: object types and their typed properties, relations, laws, lifecycles and actions, with the definitions version. Call this first to learn which types and fields exist.",
                "inputSchema": {"type": "object", "additionalProperties": false, "properties": {}},
                "annotations": read_only(),
            }),
            None,
        )?,
        tool(
            json!({
                "name": "norn_list_definitions",
                "title": "List saved definitions",
                "description": "Saved dashboards, questions, agents and bindings of this workspace, newest version of each.",
                "inputSchema": {"type": "object", "additionalProperties": false, "properties": {
                    "kind": {"type": "string", "enum": ["dashboard", "question", "agent", "binding"]},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 100},
                    "cursor": {"type": "string", "description": "The previous page's cursor."}
                }},
                "annotations": read_only(),
            }),
            None,
        )?,
        tool(
            json!({
                "name": "norn_query_objects",
                "title": "Find records",
                "description": "Read one page of records of a type that the caller may see, optionally filtered and sorted. Filter: {\"all\": [{\"property\": p, \"op\": o, \"value\": v}]} with op one of eq, ne, in, contains, prefix, lt, lte, gt, gte, between (value [low, high]), empty, present; at most 8 clauses. Sort: \"property:asc\" or \"property:desc\", up to 3, comma-separated. Pass the page's `next` as `after` for the next page. Withheld values never match a filter.",
                "inputSchema": {"type": "object", "additionalProperties": false, "required": ["type"], "properties": {
                    "type": type_schema,
                    "filter": {"type": "object", "properties": {"all": {"type": "array", "maxItems": 8, "items": {"type": "object", "required": ["property", "op"], "properties": {"property": {"type": "string"}, "op": {"type": "string", "enum": ["eq", "ne", "in", "contains", "prefix", "lt", "lte", "gt", "gte", "between", "empty", "present"]}, "value": {}}}}}},
                    "sort": {"type": "string"},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 200},
                    "after": {"type": "string"}
                }},
                "annotations": read_only(),
            }),
            None,
        )?,
        tool(
            json!({
                "name": "norn_get_object",
                "title": "Read a record",
                "description": "One record by type and id, with the values the caller may see.",
                "inputSchema": {"type": "object", "additionalProperties": false, "required": ["type", "id"], "properties": {
                    "type": type_schema, "id": {"type": "string"}
                }},
                "annotations": read_only(),
            }),
            None,
        )?,
        tool(
            json!({
                "name": "norn_object_links",
                "title": "Related records",
                "description": "The records linked to one record, by declared relation.",
                "inputSchema": {"type": "object", "additionalProperties": false, "required": ["type", "id"], "properties": {
                    "type": type_schema, "id": {"type": "string"},
                    "direction": {"type": "string", "enum": ["out", "in"]},
                    "link": {"type": "string", "description": "A declared relation name."},
                    "limit": {"type": "integer", "minimum": 1, "maximum": 200},
                    "after": {"type": "string"}
                }},
                "annotations": read_only(),
            }),
            None,
        )?,
        tool(
            json!({
                "name": "norn_trace_object",
                "title": "Trace a record to its source",
                "description": "Where one record came from: the commits, receipts and source rows that produced its current values.",
                "inputSchema": {"type": "object", "additionalProperties": false, "required": ["type", "id"], "properties": {
                    "type": type_schema, "id": {"type": "string"}
                }},
                "annotations": read_only(),
            }),
            Some(View::Lineage),
        )?,
        tool(
            json!({
                "name": "norn_profile_type",
                "title": "Profile a type",
                "description": "Summary statistics of one object type over the records the caller may see: counts, missing values and ranges per property. Use it before asking questions of unfamiliar data.",
                "inputSchema": {"type": "object", "additionalProperties": false, "required": ["type"], "properties": {
                    "type": type_schema
                }},
                "annotations": read_only(),
            }),
            None,
        )?,
    ];
    if answers {
        tools.push(tool(
            json!({
                "name": "norn_question_catalog",
                "title": "What can be asked",
                "description": "The metrics and object-type properties the caller may ask about, with the grains, operators and bounds a question may use. Read it before norn_evaluate or norn_propose_definition.",
                "inputSchema": {"type": "object", "additionalProperties": false, "properties": {}},
                "annotations": read_only(),
            }),
            None,
        )?);
        tools.push(tool(
            json!({
                "name": "norn_list_metrics",
                "title": "List metrics",
                "description": "The workspace's declared metrics: what each measures, its unit and how it adds up.",
                "inputSchema": {"type": "object", "additionalProperties": false, "properties": {}},
                "annotations": read_only(),
            }),
            None,
        )?);
        tools.push(tool(
            json!({
                "name": "norn_answer",
                "title": "Answer a question",
                "description": "The current answer to an admitted question: exact values per bucket, the source frontier it was computed at and its freshness. Each bucket carries a lineageRef for norn_trace_answer.",
                "inputSchema": {"type": "object", "additionalProperties": false, "required": ["question"], "properties": {
                    "question": question_schema
                }},
                "annotations": read_only(),
            }),
            Some(View::Answer),
        )?);
        tools.push(tool(
            json!({
                "name": "norn_trace_answer",
                "title": "Trace an answer to its source",
                "description": "Why a bucket of an answer has its value: the contributing records and their source rows. Use the lineageRef and bucket key the answer returned; use it to explain outliers.",
                "inputSchema": {"type": "object", "additionalProperties": false, "required": ["question", "lineageRef", "bucket"], "properties": {
                    "question": question_schema,
                    "lineageRef": {"type": "string"},
                    "bucket": {"type": "string"}
                }},
                "annotations": read_only(),
            }),
            Some(View::Lineage),
        )?);
        tools.push(tool(
            json!({
                "name": "norn_evaluate",
                "title": "Ask a new question",
                "description": "Admit and answer a typed question (QuestionIR) over the workspace's declared metrics, once, without saving it. The checker refuses a question that names undeclared metrics or mixes incompatible units, with the reason.",
                "inputSchema": {"type": "object", "additionalProperties": false, "required": ["question"], "properties": {
                    "question": {"type": "object", "description": "A QuestionIR document."}
                }},
                "annotations": read_only(),
            }),
            Some(View::Answer),
        )?);
        tools.push(tool(
            json!({
                "name": "norn_trace_evaluated",
                "title": "Trace a new question's answer",
                "description": "Trace one bucket of a norn_evaluate answer to its source records; pass the same question.",
                "inputSchema": {"type": "object", "additionalProperties": false, "required": ["question", "lineageRef", "bucket"], "properties": {
                    "question": {"type": "object"},
                    "lineageRef": {"type": "string"},
                    "bucket": {"type": "string"}
                }},
                "annotations": read_only(),
            }),
            Some(View::Lineage),
        )?);
        tools.push(tool(
            json!({
                "name": "norn_propose_definition",
                "title": "Propose a dashboard or question",
                "description": "Check a proposed dashboard or question definition against the workspace without saving it. The answer is the checker's verdict and diagnostics; a person installs it from Norn Studio by signing its restatement.",
                "inputSchema": {"type": "object", "additionalProperties": false, "required": ["kind", "id", "payload"], "properties": {
                    "kind": {"type": "string", "enum": ["dashboard", "question"]},
                    "id": {"type": "string"},
                    "payload": {"type": "object"}
                }},
                "annotations": read_only(),
            }),
            Some(View::Dashboard),
        )?);
    }
    Ok(tools)
}

/// Tool calls re-enter the cell's own routes as the same caller.
struct Loopback<'a, E: SqlEngine> {
    host: &'a DevHost<E>,
    principals: &'a dyn PrincipalResolver,
    questions: Option<&'a dyn AdHocQuestions<E>>,
    kinds: &'a DefinitionKinds,
    request: &'a Request<'a>,
    scope: ScopeQuery,
    actions: BTreeMap<String, String>,
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
    #[allow(
        clippy::too_many_lines,
        reason = "one table from tool name to the route it calls"
    )]
    fn route(
        &self,
        name: &str,
        arguments: &RuntimeObject,
        metadata: &RuntimeObject,
    ) -> Result<Routed, Refusal> {
        let get = |path: String, query: Vec<(&'static str, String)>| Routed {
            method: "GET",
            path,
            query,
            body: String::new(),
        };
        let mut scoped = self.scope.pairs();
        let optional =
            |scoped: &mut Vec<(&'static str, String)>, key: &'static str, field: &str| {
                if let Some(value) = text(arguments, field)? {
                    scoped.push((key, value));
                }
                Ok::<(), Refusal>(())
            };
        let object_path = |leaf: &str| -> Result<String, Refusal> {
            Ok(format!(
                "/v1/objects/{}/{}{leaf}",
                segment(&required(arguments, "type")?),
                segment(&required(arguments, "id")?)
            ))
        };
        Ok(match name {
            "norn_catalog" => get("/v1/definitions".into(), scoped),
            "norn_list_definitions" => {
                optional(&mut scoped, "kind", "kind")?;
                optional(&mut scoped, "limit", "limit")?;
                optional(&mut scoped, "cursor", "cursor")?;
                get("/v1/definitions:list".into(), scoped)
            }
            "norn_query_objects" => {
                optional(&mut scoped, "sort", "sort")?;
                optional(&mut scoped, "limit", "limit")?;
                optional(&mut scoped, "after", "after")?;
                match arguments.get("filter") {
                    None | Some(RuntimeValue::Null) => {}
                    Some(filter @ RuntimeValue::Object(_)) => {
                        scoped.push(("filter", json_text(filter)?));
                    }
                    Some(
                        RuntimeValue::Bool(_)
                        | RuntimeValue::Number(_)
                        | RuntimeValue::String(_)
                        | RuntimeValue::Array(_),
                    ) => return Err(Refusal::invalid("filter must be an object")),
                }
                get(
                    format!("/v1/objects/{}", segment(&required(arguments, "type")?)),
                    scoped,
                )
            }
            "norn_get_object" => get(object_path("")?, scoped),
            "norn_object_links" => {
                optional(&mut scoped, "direction", "direction")?;
                optional(&mut scoped, "link", "link")?;
                optional(&mut scoped, "limit", "limit")?;
                optional(&mut scoped, "after", "after")?;
                get(object_path("/links")?, scoped)
            }
            "norn_trace_object" => get(object_path("/lineage")?, scoped),
            "norn_profile_type" => get(
                format!(
                    "/v1/objects/{}:profile",
                    segment(&required(arguments, "type")?)
                ),
                scoped,
            ),
            "norn_question_catalog" => get("/v1/questions:catalog".into(), scoped),
            "norn_list_metrics" => get("/v1/questions:metrics".into(), scoped),
            "norn_answer" => get(
                format!(
                    "/v1/questions/{}/answer",
                    segment(&required(arguments, "question")?)
                ),
                scoped,
            ),
            "norn_trace_answer" => {
                optional(&mut scoped, "lineageRef", "lineageRef")?;
                optional(&mut scoped, "bucket", "bucket")?;
                get(
                    format!(
                        "/v1/questions/{}/trace",
                        segment(&required(arguments, "question")?)
                    ),
                    scoped,
                )
            }
            "norn_evaluate" => Routed {
                method: "POST",
                path: "/v1/questions:evaluate".into(),
                query: scoped,
                body: json_text(&object(vec![(
                    "question",
                    arguments.get("question").cloned().unwrap_or_default(),
                )]))?,
            },
            "norn_trace_evaluated" => Routed {
                method: "POST",
                path: "/v1/questions:trace".into(),
                query: scoped,
                body: json_text(&object(vec![
                    (
                        "question",
                        arguments.get("question").cloned().unwrap_or_default(),
                    ),
                    (
                        "lineageRef",
                        RuntimeValue::String(required(arguments, "lineageRef")?),
                    ),
                    (
                        "bucket",
                        RuntimeValue::String(required(arguments, "bucket")?),
                    ),
                ]))?,
            },
            "norn_propose_definition" => Routed {
                method: "POST",
                path: "/v1/definitions:admit".into(),
                query: scoped,
                body: json_text(&object(vec![
                    ("kind", RuntimeValue::String(required(arguments, "kind")?)),
                    ("id", RuntimeValue::String(required(arguments, "id")?)),
                    (
                        "payload",
                        arguments.get("payload").cloned().unwrap_or_default(),
                    ),
                ]))?,
            },
            _ => {
                let action = self
                    .actions
                    .get(name)
                    .ok_or_else(|| Refusal::invalid("unknown tool"))?;
                let key =
                    if let Some(RuntimeValue::String(key)) = metadata.get("norn/idempotencyKey") {
                        key.clone()
                    } else {
                        idempotency_key(action, arguments)?
                    };
                scoped.push(("version", self.version.to_string()));
                Routed {
                    method: "POST",
                    path: format!("/v1/actions/{}", segment(action)),
                    query: scoped,
                    body: json_text(&object(vec![
                        ("input", RuntimeValue::Object(arguments.clone())),
                        ("idempotencyKey", RuntimeValue::String(key)),
                    ]))?,
                }
            }
        })
    }
}

/// Without a caller-supplied key, a call's retry identity is its content: the
/// same action with the same input is one call, and a host's automatic retry
/// returns the original receipt instead of committing twice.
fn idempotency_key(action: &str, arguments: &RuntimeObject) -> Result<String, Refusal> {
    let content = json_text(&object(vec![
        ("action", RuntimeValue::String(action.to_owned())),
        ("input", RuntimeValue::Object(arguments.clone())),
    ]))?;
    let digest = norn_ir::canonical::HashProfile::Norn1.hash_canonical_bytes(content.as_bytes());
    Ok(format!("mcp:{digest}"))
}

impl<E: SqlEngine> Backend for Loopback<'_, E> {
    fn call(
        &mut self,
        name: &str,
        arguments: RuntimeObject,
        metadata: RuntimeObject,
    ) -> Result<CallOutput, Refusal> {
        let routed = self.route(name, &arguments, &metadata)?;
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
        Ok(CallOutput {
            value,
            is_error: response.status != StatusCode::OK.as_u16(),
        })
    }
}
