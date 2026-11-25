//! TOOL-MANIFEST-1 (#1762): the server's route-backed tools, read from the
//! `OpenAPI` operations that declare them.
//!
//! A route the server offers as a tool carries its declaration under
//! [`TOOL_EXTENSION`] on its own `OpenAPI` operation: the tool's name, its
//! label (`title`), description, input schema, annotations, the view a chat
//! draws its answer with, to whom it is offered, and how each argument binds
//! to that operation (a path segment, a query parameter, a body field, or an
//! argument the tool reads itself). [`ToolManifest::from_openapi_paths`] reads
//! the manifest from those operations and refuses a declaration that names a
//! path segment, query parameter or body the operation does not declare, or an
//! argument its input schema does not describe. One manifest therefore serves
//! the published `OpenAPI` document, the MCP catalog ([`route_tool`]) and the
//! labels a client shows for each tool call; none of them restates a route.
//!
//! The manifest grants nothing: a tool call is the route's own request,
//! decided again by the route's scope, policy and redaction.

use std::collections::BTreeSet;

use norn_ir::{JsonObject, JsonValue, McpTool, McpToolAnnotations};

/// The `OpenAPI` operation extension that declares a route's tool.
pub const TOOL_EXTENSION: &str = "x-norn-tool";

/// The key of an input-schema placeholder for a value drawn from what the
/// workspace declares (see [`Domain`]).
pub const DOMAIN_KEY: &str = "x-norn-domain";

/// The HTTP method of a tool's route.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolMethod {
    /// `GET`: the arguments travel in the path and query.
    Get,
    /// `POST`: the body arguments travel as one JSON object.
    Post,
}

impl ToolMethod {
    /// The method as HTTP writes it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
        }
    }
}

/// To whom the catalog offers a tool. The route decides every call again; this
/// only keeps a tool out of a catalog where it could never succeed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Offered {
    /// Every verified caller in the cell's scope.
    Always,
    /// A caller the cell lets write definitions.
    ToDefinitionWriters,
    /// Every caller, on a host that answers questions.
    WithQuestions,
}

/// Where an argument travels in the route's request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Location {
    /// A `{name}` segment of the path template.
    Path,
    /// A query parameter of the same name.
    Query,
    /// A field of the same name in the JSON body.
    Body,
    /// Read by the tool itself; never sent to the route.
    Local,
}

/// How an argument's value is written into the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    /// A string, or a number as its decimal text.
    Text,
    /// Any JSON value, sent as given (`null` when absent).
    Value,
    /// A JSON object, sent in a query parameter as its canonical JSON text.
    Object,
    /// A JSON array.
    Array,
    /// A JSON number.
    Number,
    /// A bucket of an answer: its canonical JSON key, or a plain value read as
    /// that string.
    Bucket,
}

/// One argument of a route-backed tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolArgument {
    /// The argument's name: its input-schema property, path segment, query
    /// parameter or body field.
    pub name: String,
    /// Where it travels.
    pub location: Location,
    /// How it is written.
    pub encoding: Encoding,
    /// Whether the input schema requires it.
    pub required: bool,
    /// The explanation of a refusal for a missing or malformed value, when the
    /// declaration gives one.
    pub refusal: Option<String>,
}

/// One route-backed tool: its route, as the `OpenAPI` operation declares it,
/// and the server's declaration of the tool.
#[derive(Debug, Clone, PartialEq)]
pub struct RouteTool {
    /// The route's `operationId`.
    pub operation_id: String,
    /// The route's method.
    pub method: ToolMethod,
    /// The route's path template.
    pub path: String,
    /// The tool's name.
    pub name: String,
    /// The server's label for the tool: the MCP `title`, and what a client
    /// shows for a call of it.
    pub title: String,
    /// What the tool does, for a model.
    pub description: String,
    /// The tool's input schema, with [`DOMAIN_KEY`] placeholders.
    pub input_schema: JsonObject,
    /// The tool's annotations.
    pub annotations: McpToolAnnotations,
    /// The view a chat draws the tool's answer with, by name.
    pub view: Option<String>,
    /// To whom it is offered.
    pub offered: Offered,
    /// Its arguments, in the order they are read.
    pub arguments: Vec<ToolArgument>,
}

/// A set of values drawn from what the workspace declares, which an input
/// schema names with a [`DOMAIN_KEY`] placeholder.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Domain {
    /// The object types the workspace declares (`"objectType"`).
    ObjectType,
    /// The workspace's admitted questions (`"question"`).
    Question,
}

impl Domain {
    fn parse(name: &str) -> Option<Self> {
        match name {
            "objectType" => Some(Self::ObjectType),
            "question" => Some(Self::Question),
            _ => None,
        }
    }

    /// The schema of one value of this domain: an enum of `values`, or, when
    /// the workspace declares none, a string pointing at the tool that lists
    /// them.
    fn schema(self, values: &[String]) -> JsonValue {
        let string = |text: &str| JsonValue::String(text.to_owned());
        let (open, closed) = match self {
            Self::ObjectType => (
                "An object type from norn_catalog.",
                "An object type this workspace declares.",
            ),
            Self::Question => (
                "An admitted question name from norn_question_catalog.",
                "An admitted question of this workspace.",
            ),
        };
        let mut schema = JsonObject::from([("type".to_owned(), string("string"))]);
        if values.is_empty() {
            schema.insert("description".to_owned(), string(open));
        } else {
            schema.insert(
                "enum".to_owned(),
                JsonValue::Array(values.iter().map(|value| string(value)).collect()),
            );
            schema.insert("description".to_owned(), string(closed));
        }
        JsonValue::Object(schema)
    }
}

/// The values of each [`Domain`] in one caller's workspace.
#[derive(Debug, Clone, Copy)]
pub struct Domains<'a> {
    /// The object types the workspace declares.
    pub object_types: &'a [String],
    /// The admitted questions.
    pub questions: &'a [String],
}

/// Why an operation's tool declaration is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestError {
    /// The operation (`METHOD path`) whose declaration is refused.
    pub operation: String,
    /// What is wrong with it.
    pub reason: String,
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.operation, self.reason)
    }
}

impl std::error::Error for ManifestError {}

/// The route-backed tools the server declares.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ToolManifest {
    tools: Vec<RouteTool>,
}

impl ToolManifest {
    /// Read the manifest from an `OpenAPI` `paths` object (`{path: {method:
    /// operation}}`): every operation that carries [`TOOL_EXTENSION`].
    ///
    /// # Errors
    ///
    /// A [`ManifestError`] for the first declaration that is malformed, binds
    /// an argument the operation or the input schema does not declare, leaves
    /// a path segment unbound, or repeats a tool name.
    pub fn from_openapi_paths(paths: &JsonValue) -> Result<Self, ManifestError> {
        let refused = |operation: String, reason: &str| ManifestError {
            operation,
            reason: reason.to_owned(),
        };
        let paths = paths
            .as_object()
            .ok_or_else(|| refused("paths".to_owned(), "is not an object"))?;
        let mut tools = Vec::new();
        let mut names = BTreeSet::new();
        for (path, item) in paths {
            let Some(item) = item.as_object() else {
                return Err(refused(path.clone(), "is not a path item"));
            };
            for (method, operation) in item {
                let Some(declared) = operation.as_object().and_then(|op| op.get(TOOL_EXTENSION))
                else {
                    continue;
                };
                let at = format!("{} {path}", method.to_uppercase());
                let method = match method.as_str() {
                    "get" => ToolMethod::Get,
                    "post" => ToolMethod::Post,
                    _ => return Err(refused(at, "a tool's route is a GET or a POST")),
                };
                let tool = route(method, path, operation, declared)
                    .map_err(|reason| refused(at.clone(), &reason))?;
                if !names.insert(tool.name.clone()) {
                    return Err(refused(at, "repeats another operation's tool name"));
                }
                tools.push(tool);
            }
        }
        Ok(Self { tools })
    }

    /// Every declared tool, by path and method.
    #[must_use]
    pub fn tools(&self) -> &[RouteTool] {
        &self.tools
    }

    /// The declared tool named `name`.
    #[must_use]
    pub fn tool(&self, name: &str) -> Option<&RouteTool> {
        self.tools.iter().find(|tool| tool.name == name)
    }
}

fn text<'a>(object: &'a JsonObject, key: &str) -> Result<&'a str, String> {
    object
        .get(key)
        .and_then(JsonValue::as_string)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("{TOOL_EXTENSION}.{key} must be a non-empty string"))
}

/// The `{name}` segments of a path template.
fn template_segments(path: &str) -> BTreeSet<&str> {
    path.split('{')
        .skip(1)
        .filter_map(|rest| rest.split_once('}').map(|(name, _)| name))
        .collect()
}

/// The `(in, name)` of each parameter the operation declares.
fn parameters(operation: &JsonObject) -> BTreeSet<(&str, &str)> {
    operation
        .get("parameters")
        .and_then(JsonValue::as_array)
        .into_iter()
        .flatten()
        .filter_map(|parameter| {
            let parameter = parameter.as_object()?;
            Some((
                parameter.get("in")?.as_string()?,
                parameter.get("name")?.as_string()?,
            ))
        })
        .collect()
}

fn route(
    method: ToolMethod,
    path: &str,
    operation: &JsonValue,
    declared: &JsonValue,
) -> Result<RouteTool, String> {
    let operation = operation
        .as_object()
        .ok_or("the operation is not an object")?;
    let declared = declared
        .as_object()
        .ok_or_else(|| format!("{TOOL_EXTENSION} must be an object"))?;
    let operation_id = operation
        .get("operationId")
        .and_then(JsonValue::as_string)
        .ok_or("the operation has no operationId")?;
    let input_schema = declared
        .get("inputSchema")
        .and_then(JsonValue::as_object)
        .filter(|schema| schema.get("type").and_then(JsonValue::as_string) == Some("object"))
        .ok_or("inputSchema must be an object schema (type: \"object\")")?
        .clone();
    check_domains(&JsonValue::Object(input_schema.clone()))?;
    let annotations: McpToolAnnotations = declared
        .get("annotations")
        .ok_or("annotations are required")?
        .decode()
        .map_err(|error| format!("annotations: {error}"))?;
    let offered = match text(declared, "offered")? {
        "always" => Offered::Always,
        "definitionWriters" => Offered::ToDefinitionWriters,
        "withQuestions" => Offered::WithQuestions,
        other => {
            return Err(format!(
                "offered {other} is not always, definitionWriters or withQuestions"
            ));
        }
    };
    let view = match declared.get("view") {
        None => None,
        Some(view) => Some(
            view.as_string()
                .ok_or("view must be a view name")?
                .to_owned(),
        ),
    };
    let arguments = arguments(method, path, operation, declared, &input_schema)?;
    Ok(RouteTool {
        operation_id: operation_id.to_owned(),
        method,
        path: path.to_owned(),
        name: text(declared, "name")?.to_owned(),
        title: text(declared, "title")?.to_owned(),
        description: text(declared, "description")?.to_owned(),
        input_schema,
        annotations,
        view,
        offered,
        arguments,
    })
}

/// Every [`DOMAIN_KEY`] placeholder names a [`Domain`] and holds nothing else.
fn check_domains(schema: &JsonValue) -> Result<(), String> {
    if let Some(object) = schema.as_object() {
        if let Some(domain) = object.get(DOMAIN_KEY) {
            let known = domain.as_string().and_then(Domain::parse).is_some();
            if !known || object.len() != 1 {
                return Err(format!(
                    "{DOMAIN_KEY} must be the placeholder's only key and name objectType or question"
                ));
            }
        }
        object.values().try_for_each(check_domains)?;
    } else if let Some(items) = schema.as_array() {
        items.iter().try_for_each(check_domains)?;
    }
    Ok(())
}

/// What an operation declares that an argument may bind to.
struct Binds<'a> {
    method: ToolMethod,
    segments: BTreeSet<&'a str>,
    parameters: BTreeSet<(&'a str, &'a str)>,
    body: bool,
}

/// One declared argument, checked against what the operation declares.
fn argument(
    binds: &Binds<'_>,
    argument: &JsonValue,
    required: bool,
) -> Result<ToolArgument, String> {
    let argument = argument
        .as_object()
        .ok_or("each argument must be an object")?;
    let name = text(argument, "name")?;
    let location = match text(argument, "in")? {
        "path" => Location::Path,
        "query" => Location::Query,
        "body" => Location::Body,
        "local" => Location::Local,
        other => {
            return Err(format!(
                "argument {name}: in {other} is not path, query, body or local"
            ));
        }
    };
    let encoding = match argument.get("encoding").map(JsonValue::as_string) {
        None | Some(Some("text")) => Encoding::Text,
        Some(Some("value")) => Encoding::Value,
        Some(Some("object")) => Encoding::Object,
        Some(Some("array")) => Encoding::Array,
        Some(Some("number")) => Encoding::Number,
        Some(Some("bucket")) => Encoding::Bucket,
        Some(_) => return Err(format!("argument {name}: unknown encoding")),
    };
    let fits = match location {
        Location::Path => {
            encoding == Encoding::Text
                && required
                && binds.segments.contains(name)
                && binds.parameters.contains(&("path", name))
        }
        Location::Query => {
            matches!(
                encoding,
                Encoding::Text | Encoding::Object | Encoding::Bucket
            ) && binds.parameters.contains(&("query", name))
        }
        Location::Body => binds.method == ToolMethod::Post && binds.body,
        Location::Local => argument.get("encoding").is_none(),
    };
    if !fits {
        return Err(format!(
            "argument {name} does not fit the operation: a path argument is a required text segment of the template, a query argument a declared query parameter, a body argument a field of a POST body"
        ));
    }
    let refusal = match argument.get("refusal") {
        None => None,
        Some(refusal) => Some(
            refusal
                .as_string()
                .ok_or_else(|| format!("argument {name}: refusal must be a string"))?
                .to_owned(),
        ),
    };
    Ok(ToolArgument {
        name: name.to_owned(),
        location,
        encoding,
        required,
        refusal,
    })
}

/// The declared arguments, each a property of the input schema, bound once,
/// and together binding every property and path segment.
fn arguments(
    method: ToolMethod,
    path: &str,
    operation: &JsonObject,
    declared: &JsonObject,
    input_schema: &JsonObject,
) -> Result<Vec<ToolArgument>, String> {
    let binds = Binds {
        method,
        segments: template_segments(path),
        parameters: parameters(operation),
        body: operation.contains_key("requestBody"),
    };
    let properties: BTreeSet<&str> = input_schema
        .get("properties")
        .and_then(JsonValue::as_object)
        .map(|properties| properties.keys().map(String::as_str).collect())
        .unwrap_or_default();
    let required: BTreeSet<&str> = input_schema
        .get("required")
        .and_then(JsonValue::as_array)
        .into_iter()
        .flatten()
        .filter_map(JsonValue::as_string)
        .collect();
    let mut arguments: Vec<ToolArgument> = Vec::new();
    for declared in declared
        .get("arguments")
        .and_then(JsonValue::as_array)
        .ok_or("arguments must be an array")?
    {
        let name = declared
            .as_object()
            .and_then(|declared| declared.get("name"))
            .and_then(JsonValue::as_string)
            .unwrap_or_default();
        let parsed = argument(&binds, declared, required.contains(name))?;
        if !properties.contains(name) || arguments.iter().any(|bound| bound.name == name) {
            return Err(format!(
                "argument {name} must be a property of inputSchema, bound once"
            ));
        }
        arguments.push(parsed);
    }
    let bound = |name: &&str| arguments.iter().any(|argument| argument.name == *name);
    if let Some(unbound) = properties.iter().find(|name| !bound(name)) {
        return Err(format!("inputSchema property {unbound} has no argument"));
    }
    if let Some(unbound) = binds.segments.iter().find(|name| !bound(name)) {
        return Err(format!("path segment {unbound} has no argument"));
    }
    Ok(arguments)
}

/// Replace each [`DOMAIN_KEY`] placeholder with its domain's schema.
fn bind_domains(schema: &mut JsonValue, domains: &Domains<'_>) {
    if let JsonValue::Object(object) = schema {
        if let Some(domain) = object
            .get(DOMAIN_KEY)
            .and_then(JsonValue::as_string)
            .and_then(Domain::parse)
        {
            *schema = domain.schema(match domain {
                Domain::ObjectType => domains.object_types,
                Domain::Question => domains.questions,
            });
            return;
        }
        for value in object.values_mut() {
            bind_domains(value, domains);
        }
    } else if let JsonValue::Array(items) = schema {
        for item in items {
            bind_domains(item, domains);
        }
    }
}

/// Project a route-backed tool onto its MCP tool for one caller's workspace:
/// the declared name, label, description, annotations and input schema, with
/// each domain placeholder bound to the values the workspace declares.
#[must_use]
pub fn route_tool(tool: &RouteTool, domains: &Domains<'_>) -> McpTool {
    let mut input_schema = JsonValue::Object(tool.input_schema.clone());
    bind_domains(&mut input_schema, domains);
    let input_schema = match input_schema {
        JsonValue::Object(schema) => schema,
        JsonValue::Null
        | JsonValue::Bool(_)
        | JsonValue::Number(_)
        | JsonValue::String(_)
        | JsonValue::Array(_) => tool.input_schema.clone(),
    };
    McpTool {
        name: tool.name.clone(),
        title: Some(tool.title.clone()),
        description: Some(tool.description.clone()),
        input_schema,
        output_schema: None,
        annotations: Some(tool.annotations.clone()),
        meta: None,
    }
}
