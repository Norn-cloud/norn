//! TOOL-MANIFEST-1 (#1762): the tools the server offers over its own routes,
//! declared next to the actions it offers ([`crate::action_catalog`]).
//!
//! Each declaration names its route by the route's generated `OpenAPI`
//! operation (the `__path_*` type `utoipa` writes for it), never by a path:
//! the method, path template and parameters are the route's own. The
//! declaration is the server's word on the tool: its name, its label
//! (`title`), what it does, its input schema (with `x-norn-domain`
//! placeholders for the caller's object types and questions), annotations,
//! the view a chat draws its answer with, to whom it is offered, and where
//! each argument travels in the route's request.
//!
//! It is stamped onto that operation as `x-norn-tool`, so the published
//! `OpenAPI` document carries the manifest;
//! [`tool_operations`](crate::tool_catalog::tool_operations) returns the same
//! operations for a host to build its MCP catalog from
//! (`norn_projections::tool_manifest`) without generating the whole document;
//! and that catalog's titles are the labels Studio's Sidekick shows for each
//! tool call. Nothing here grants anything: a call is the route's own
//! request, decided again by its scope, policy and redaction.

use serde_json::{Value, json};
use utoipa::Path;
use utoipa::openapi::path::{HttpMethod, Operation, PathItem};

/// The `OpenAPI` operation extension that carries a route's tool.
pub const TOOL_EXTENSION: &str = "x-norn-tool";

/// One route and the tool declared over it.
struct Declared {
    path: fn() -> String,
    methods: fn() -> Vec<HttpMethod>,
    operation: fn() -> Operation,
    tool: Value,
}

/// The tool `tool` over the route whose generated operation is `P`.
fn on<P: Path>(tool: Value) -> Declared {
    Declared {
        path: P::path,
        methods: P::methods,
        operation: P::operation,
        tool,
    }
}

fn read_only() -> Value {
    json!({"readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false})
}

/// Every route-backed tool: the reads, and the definitions plan and apply.
fn declarations() -> Vec<Declared> {
    let mut declared = workspace_tools();
    declared.extend(record_tools());
    declared.extend(definition_tools());
    declared.extend(question_tools());
    declared
}

/// The workspace and its records: what it declares, what needs attention, and reads of its objects.
fn workspace_tools() -> Vec<Declared> {
    vec![
        on::<crate::routes::__path_get_defs>(json!({
            "name": "norn_catalog",
            "title": "Describe the workspace",
            "description": "What this Norn workspace declares, one line each: its object types (with their properties), laws, actions and admitted questions, and the definitions version. Call this first. For an artifact's full definition pass detail \"full\" with the artifact ids or names you need.",
            "inputSchema": {"type": "object", "additionalProperties": false, "properties": {
                "detail": {"type": "string", "enum": ["summary", "full"], "description": "summary (the default) or full artifacts; full needs names."},
                "names": {"type": "array", "maxItems": 20, "items": {"type": "string"}, "description": "With detail full: artifact ids or names to return, matched by substring, for example docs.ScannedDocument."}
            }},
            "annotations": read_only(),
            "offered": "always",
            "arguments": [{"name": "detail", "in": "local"}, {"name": "names", "in": "local"}],
        })),
        on::<crate::inbox::__path_get_inbox>(json!({
            "name": "norn_inbox",
            "title": "What needs attention",
            "description": "The caller's work queue: records a law holds back, grouped by the law with counts (each record with the finding in plain words and, when the caller may make it, the fix the server computed: the governed action and its arguments), decisions waiting on the caller, and connected sources whose last sync failed. Every item is what the caller may read, and each names the Studio page to act at. Apply a fix only through its action tool.",
            "inputSchema": {"type": "object", "additionalProperties": false, "properties": {}},
            "annotations": read_only(),
            "view": "inbox",
            "offered": "always",
            "arguments": [],
        })),
        on::<crate::definition_routes::__path_list_definitions>(json!({
            "name": "norn_list_definitions",
            "title": "List saved definitions",
            "description": "Saved dashboards, questions, agents and bindings of this workspace, newest version of each.",
            "inputSchema": {"type": "object", "additionalProperties": false, "properties": {
                "kind": {"type": "string", "enum": ["dashboard", "question", "agent", "binding"]},
                "limit": {"type": "integer", "minimum": 1, "maximum": 100},
                "cursor": {"type": "string", "description": "The previous page's cursor."}
            }},
            "annotations": read_only(),
            "offered": "always",
            "arguments": [{"name": "kind", "in": "query"}, {"name": "limit", "in": "query"}, {"name": "cursor", "in": "query"}],
        })),
        on::<crate::object_routes::__path_get_object_query>(json!({
            "name": "norn_query_objects",
            "title": "Find records",
            "description": "Read one page of records of a type that the caller may see, optionally filtered and sorted. Filter: {\"all\": [{\"property\": p, \"op\": o, \"value\": v}]} with op one of eq, ne, in, contains, prefix, lt, lte, gt, gte, between (value [low, high]), empty, present; at most 8 clauses. Sort: \"property:asc\" or \"property:desc\", up to 3, comma-separated. Pass the page's `next` as `after` for the next page. Withheld values never match a filter.",
            "inputSchema": {"type": "object", "additionalProperties": false, "required": ["type"], "properties": {
                "type": {"x-norn-domain": "objectType"},
                "filter": {"type": "object", "properties": {"all": {"type": "array", "maxItems": 8, "items": {"type": "object", "required": ["property", "op"], "properties": {"property": {"type": "string"}, "op": {"type": "string", "enum": ["eq", "ne", "in", "contains", "prefix", "lt", "lte", "gt", "gte", "between", "empty", "present"]}, "value": {"anyOf": [{"type": "string"}, {"type": "number"}, {"type": "boolean"}, {"type": "array", "maxItems": 100, "items": {"anyOf": [{"type": "string"}, {"type": "number"}, {"type": "boolean"}]}}], "description": "A scalar; an array for in (the values) and between ([low, high]); omitted for empty and present."}}}}}},
                "sort": {"type": "string"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 200},
                "after": {"type": "string"}
            }},
            "annotations": read_only(),
            "view": "records",
            "offered": "always",
            "arguments": [{"name": "sort", "in": "query"}, {"name": "limit", "in": "query"}, {"name": "after", "in": "query"}, {"name": "filter", "in": "query", "encoding": "object"}, {"name": "type", "in": "path"}],
        })),
        on::<crate::object_routes::__path_get_queried_object>(json!({
            "name": "norn_get_object",
            "title": "Read a record",
            "description": "One record by type and id, with the values the caller may see.",
            "inputSchema": {"type": "object", "additionalProperties": false, "required": ["type", "id"], "properties": {
                "type": {"x-norn-domain": "objectType"}, "id": {"type": "string"}
            }},
            "annotations": read_only(),
            "offered": "always",
            "arguments": [{"name": "type", "in": "path"}, {"name": "id", "in": "path"}],
        })),
        on::<crate::object_links::__path_get_object_links>(json!({
            "name": "norn_object_links",
            "title": "Related records",
            "description": "The records linked to one record, by declared relation.",
            "inputSchema": {"type": "object", "additionalProperties": false, "required": ["type", "id"], "properties": {
                "type": {"x-norn-domain": "objectType"}, "id": {"type": "string"},
                "direction": {"type": "string", "enum": ["out", "in"]},
                "link": {"type": "string", "description": "A declared relation name."},
                "limit": {"type": "integer", "minimum": 1, "maximum": 200},
                "after": {"type": "string"}
            }},
            "annotations": read_only(),
            "offered": "always",
            "arguments": [{"name": "direction", "in": "query"}, {"name": "link", "in": "query"}, {"name": "limit", "in": "query"}, {"name": "after", "in": "query"}, {"name": "type", "in": "path"}, {"name": "id", "in": "path"}],
        })),
    ]
}

/// Where a record came from and why it is held, and the shape of a type.
fn record_tools() -> Vec<Declared> {
    vec![
        on::<crate::object_links::__path_get_object_lineage>(json!({
            "name": "norn_trace_object",
            "title": "Trace a record to its source",
            "description": "Where one record came from: the commits, receipts and source rows that produced its current values.",
            "inputSchema": {"type": "object", "additionalProperties": false, "required": ["type", "id"], "properties": {
                "type": {"x-norn-domain": "objectType"}, "id": {"type": "string"}
            }},
            "annotations": read_only(),
            "view": "lineage",
            "offered": "always",
            "arguments": [{"name": "type", "in": "path"}, {"name": "id", "in": "path"}],
        })),
        on::<crate::object_profile::__path_get_object_profile>(json!({
            "name": "norn_profile_type",
            "title": "Profile a type",
            "description": "Summary statistics of one object type over the records the caller may see: counts, missing values and ranges per property. Use it before asking questions of unfamiliar data.",
            "inputSchema": {"type": "object", "additionalProperties": false, "required": ["type"], "properties": {
                "type": {"x-norn-domain": "objectType"}
            }},
            "annotations": read_only(),
            "offered": "always",
            "arguments": [{"name": "type", "in": "path"}],
        })),
        on::<crate::object_review::__path_get_quarantined_objects>(json!({
            "name": "norn_list_held",
            "title": "Held-back records",
            "description": "The records of a type that Norn holds back because a law refused them, each with a one-line summary of its open findings (the law and the values it compared). Held records are not returned by norn_query_objects or norn_get_object; read why with norn_get_findings.",
            "inputSchema": {"type": "object", "additionalProperties": false, "required": ["type"], "properties": {
                "type": {"x-norn-domain": "objectType"}
            }},
            "annotations": read_only(),
            "view": "findings",
            "offered": "always",
            "arguments": [{"name": "type", "in": "path"}],
        })),
        on::<crate::object_review::__path_get_object_findings>(json!({
            "name": "norn_get_findings",
            "title": "Why a record is held",
            "description": "The findings on one record: the law that refused it, its message, the values the law compared, when it was first and last seen, whether the record is held back, and the action tools that can correct it. Call the remedy with corrected values; the laws decide again.",
            "inputSchema": {"type": "object", "additionalProperties": false, "required": ["type", "id"], "properties": {
                "type": {"x-norn-domain": "objectType"}, "id": {"type": "string"}
            }},
            "annotations": read_only(),
            "view": "findings",
            "offered": "always",
            "arguments": [{"name": "type", "in": "path"}, {"name": "id", "in": "path"}],
        })),
    ]
}

/// A definitions change, planned and applied, for a caller who may write definitions.
fn definition_tools() -> Vec<Declared> {
    vec![
        on::<crate::cell_definitions::__path_plan_definitions>(json!({
            "name": "norn_plan_definitions",
            "title": "Plan a definitions change",
            "description": "Check compiled definition artifacts (a pack's artifact envelopes) against the workspace without changing it: what they add and replace, the new object types, the checker's findings, and stored records the new laws would refuse. Returns baseVersion: pass it to norn_apply_definitions as expectedVersion.",
            "inputSchema": {"type": "object", "additionalProperties": false, "required": ["artifacts"], "properties": {
                "artifacts": {"type": "array", "minItems": 1, "maxItems": 500, "items": {"type": "object"}, "description": "Compiled artifact envelopes; none in a reserved namespace (core., std.)."},
                "approvals": {"type": "array", "maxItems": 100, "description": "The pack's approvals: admitted against these artifacts' actions and installed with them in one transaction, so a new action is never live without its gate. A plan returns each one's restatement and restatementSha256; an apply carries each one's signoff of it and the plan's fromRevision as expectedVersion. Any refusal installs nothing.", "items": {"type": "object", "additionalProperties": false, "required": ["kind", "id", "payload"], "properties": {
                    "kind": {"type": "string", "enum": ["approval"]},
                    "id": {"type": "string"},
                    "payload": {"type": "object"},
                    "expectedVersion": {"type": "integer", "minimum": 0, "description": "The approval's fromRevision from the plan; omitted for a new approval."},
                    "signoff": {"type": "object", "additionalProperties": false, "required": ["restatementSha256"], "properties": {"restatementSha256": {"type": "string", "description": "The plan's restatementSha256 for this approval: the words the caller signs."}}}
                }}}
            }},
            "annotations": read_only(),
            "view": "plan",
            "offered": "definitionWriters",
            "arguments": [{"name": "artifacts", "in": "body", "encoding": "array", "refusal": "artifacts must be an array of artifact envelopes"}, {"name": "approvals", "in": "body", "encoding": "array", "refusal": "approvals must be an array of { kind, id, payload, expectedVersion, signoff }"}],
        })),
        on::<crate::cell_definitions::__path_apply_definitions>(json!({
            "name": "norn_apply_definitions",
            "title": "Apply a planned definitions change",
            "description": "Install the artifacts a plan checked, compare-and-set on the plan's baseVersion: refused with nothing written if the workspace moved, the checker refuses, or stored records break the new laws. The answer is the installed version and its receipt. The pack's actions are callable at once with norn_call_action, before their own tools appear.",
            "inputSchema": {"type": "object", "additionalProperties": false, "required": ["artifacts", "expectedVersion"], "properties": {
                "artifacts": {"type": "array", "minItems": 1, "maxItems": 500, "items": {"type": "object"}},
                "expectedVersion": {"type": "integer", "minimum": 0, "description": "The baseVersion norn_plan_definitions returned."},
                "approvals": {"type": "array", "maxItems": 100, "description": "The pack's approvals: admitted against these artifacts' actions and installed with them in one transaction, so a new action is never live without its gate. A plan returns each one's restatement and restatementSha256; an apply carries each one's signoff of it and the plan's fromRevision as expectedVersion. Any refusal installs nothing.", "items": {"type": "object", "additionalProperties": false, "required": ["kind", "id", "payload"], "properties": {
                    "kind": {"type": "string", "enum": ["approval"]},
                    "id": {"type": "string"},
                    "payload": {"type": "object"},
                    "expectedVersion": {"type": "integer", "minimum": 0, "description": "The approval's fromRevision from the plan; omitted for a new approval."},
                    "signoff": {"type": "object", "additionalProperties": false, "required": ["restatementSha256"], "properties": {"restatementSha256": {"type": "string", "description": "The plan's restatementSha256 for this approval: the words the caller signs."}}}
                }}}
            }},
            "annotations": {"readOnlyHint": false, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false},
            "view": "receipt",
            "offered": "definitionWriters",
            "arguments": [{"name": "artifacts", "in": "body", "encoding": "array", "refusal": "artifacts must be an array of artifact envelopes"}, {"name": "approvals", "in": "body", "encoding": "array", "refusal": "approvals must be an array of { kind, id, payload, expectedVersion, signoff }"}, {"name": "expectedVersion", "in": "body", "encoding": "number", "refusal": "expectedVersion is required: the plan's baseVersion"}],
        })),
    ]
}

/// Questions: what can be asked, answers and their traces, and proposed definitions.
fn question_tools() -> Vec<Declared> {
    vec![
        on::<crate::question_routes::__path_question_catalog>(json!({
            "name": "norn_question_catalog",
            "title": "What can be asked",
            "description": "The metrics and object-type properties the caller may ask about, with the grains, operators and bounds a question may use. Read it before norn_evaluate or norn_propose_definition.",
            "inputSchema": {"type": "object", "additionalProperties": false, "properties": {}},
            "annotations": read_only(),
            "offered": "withQuestions",
            "arguments": [],
        })),
        on::<crate::question_routes::__path_list_metrics>(json!({
            "name": "norn_list_metrics",
            "title": "List metrics",
            "description": "The workspace's declared metrics: what each measures, its unit and how it adds up.",
            "inputSchema": {"type": "object", "additionalProperties": false, "properties": {}},
            "annotations": read_only(),
            "offered": "withQuestions",
            "arguments": [],
        })),
        on::<crate::question_routes::__path_get_answer>(json!({
            "name": "norn_answer",
            "title": "Answer a question",
            "description": "The current answer to an admitted question: exact values per bucket, the source frontier it was computed at and its freshness. Each bucket carries a lineageRef for norn_trace_answer.",
            "inputSchema": {"type": "object", "additionalProperties": false, "required": ["question"], "properties": {
                "question": {"x-norn-domain": "question"}
            }},
            "annotations": read_only(),
            "view": "answer",
            "offered": "withQuestions",
            "arguments": [{"name": "question", "in": "path"}],
        })),
        on::<crate::question_routes::__path_get_numeric_trace>(json!({
            "name": "norn_trace_answer",
            "title": "Trace an answer to its source",
            "description": "Why a bucket of an answer has its value: the contributing records and their source rows. Pass the answer's lineageRef and a bucket: its key in rows, or the plain value (USD); use it to explain outliers.",
            "inputSchema": {"type": "object", "additionalProperties": false, "required": ["question", "lineageRef", "bucket"], "properties": {
                "question": {"x-norn-domain": "question"},
                "lineageRef": {"type": "string"},
                "bucket": {"type": "string"}
            }},
            "annotations": read_only(),
            "view": "lineage",
            "offered": "withQuestions",
            "arguments": [{"name": "lineageRef", "in": "query"}, {"name": "bucket", "in": "query", "encoding": "bucket"}, {"name": "question", "in": "path"}],
        })),
        on::<crate::question_routes::__path_evaluate_question>(json!({
            "name": "norn_evaluate",
            "title": "Ask a new question",
            "description": "Admit and answer a typed question (QuestionIR) over the workspace's declared metrics, once, without saving it. The checker refuses a question that names undeclared metrics or mixes incompatible units, with the reason. Take the metric name and its object type from norn_question_catalog (a metric's type_params.metric_aggregation_params.semantic_model is the objectSet). Every field below is required; keep the window to the period asked about (a year at most), and add groupBy only when a breakdown is asked for: {\"irVersion\": \"0.1\", \"name\": \"stock_now\", \"objectSet\": \"canon.Holding\", \"aggregate\": [{\"metric\": \"<metric name>\"}], \"groupBy\": [], \"filter\": [], \"window\": {\"start\": \"2026-01-01T00:00:00Z\", \"end\": \"2026-12-01T00:00:00Z\"}, \"calendar\": \"iso_utc\", \"freshness\": {\"kind\": \"live\", \"maxAgeMs\": 5000}}. Then trace a bucket with norn_trace_evaluated.",
            "inputSchema": {"type": "object", "additionalProperties": false, "required": ["question"], "properties": {
                "question": {"type": "object", "description": "A QuestionIR document."}
            }},
            "annotations": read_only(),
            "view": "answer",
            "offered": "withQuestions",
            "arguments": [{"name": "question", "in": "body", "encoding": "value"}],
        })),
        on::<crate::question_routes::__path_trace_ad_hoc>(json!({
            "name": "norn_trace_evaluated",
            "title": "Trace a new question's answer",
            "description": "Trace one bucket of a norn_evaluate answer to its source records: pass the same question, the answer's lineageRef and a bucket (its key in rows, or the plain value, such as USD).",
            "inputSchema": {"type": "object", "additionalProperties": false, "required": ["question", "lineageRef", "bucket"], "properties": {
                "question": {"type": "object"},
                "lineageRef": {"type": "string"},
                "bucket": {"type": "string"}
            }},
            "annotations": read_only(),
            "view": "lineage",
            "offered": "withQuestions",
            "arguments": [{"name": "question", "in": "body", "encoding": "value"}, {"name": "lineageRef", "in": "body"}, {"name": "bucket", "in": "body", "encoding": "bucket"}],
        })),
        on::<crate::definition_routes::__path_admit_definition>(json!({
            "name": "norn_propose_definition",
            "title": "Propose a dashboard or question",
            "description": "Check a proposed dashboard or question definition against the workspace without saving it. The answer is the checker's verdict and diagnostics; a person installs it from Norn Studio by signing its restatement.",
            "inputSchema": {"type": "object", "additionalProperties": false, "required": ["kind", "id", "payload"], "properties": {
                "kind": {"type": "string", "enum": ["dashboard", "question"]},
                "id": {"type": "string"},
                "payload": {"type": "object"}
            }},
            "annotations": read_only(),
            "view": "dashboard",
            "offered": "withQuestions",
            "arguments": [{"name": "kind", "in": "body"}, {"name": "id", "in": "body"}, {"name": "payload", "in": "body", "encoding": "value"}],
        })),
    ]
}

/// `method` as an `OpenAPI` path item writes it.
fn method_key(method: &HttpMethod) -> &'static str {
    match method {
        HttpMethod::Get => "get",
        HttpMethod::Put => "put",
        HttpMethod::Post => "post",
        HttpMethod::Delete => "delete",
        HttpMethod::Options => "options",
        HttpMethod::Head => "head",
        HttpMethod::Patch => "patch",
        HttpMethod::Trace => "trace",
    }
}

/// Stamp the tool declaration `tool` onto `operation`.
fn stamp(operation: &mut Operation, tool: &Value) {
    operation
        .extensions
        .get_or_insert_with(Default::default)
        .insert(TOOL_EXTENSION.to_owned(), tool.clone());
}

/// The route-backed tools as `OpenAPI` path items, `{path: {method:
/// operation}}`, each operation as the route declares it with its tool
/// stamped on: the input `norn_projections::tool_manifest` reads.
///
/// # Errors
/// Returns a serialization failure.
pub fn tool_operations() -> Result<Value, serde_json::Error> {
    let mut paths = serde_json::Map::new();
    for declared in declarations() {
        let mut operation = (declared.operation)();
        stamp(&mut operation, &declared.tool);
        let operation = serde_json::to_value(operation)?;
        let item = paths
            .entry((declared.path)())
            .or_insert_with(|| Value::Object(serde_json::Map::new()));
        for method in (declared.methods)() {
            if let Value::Object(item) = item {
                item.insert(method_key(&method).to_owned(), operation.clone());
            }
        }
    }
    Ok(Value::Object(paths))
}

/// The operation of `item` served at `method`.
fn operation_at<'a>(item: &'a mut PathItem, method: &HttpMethod) -> Option<&'a mut Operation> {
    match method {
        HttpMethod::Get => item.get.as_mut(),
        HttpMethod::Put => item.put.as_mut(),
        HttpMethod::Post => item.post.as_mut(),
        HttpMethod::Delete => item.delete.as_mut(),
        HttpMethod::Options => item.options.as_mut(),
        HttpMethod::Head => item.head.as_mut(),
        HttpMethod::Patch => item.patch.as_mut(),
        HttpMethod::Trace => item.trace.as_mut(),
    }
}

/// Stamps each route-backed tool onto its operation in the generated
/// document. A declaration whose route the document lacks stamps nothing;
/// the test below requires every one to land.
pub(crate) struct ToolDeclarations;

impl utoipa::Modify for ToolDeclarations {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        for declared in declarations() {
            let Some(item) = openapi.paths.paths.get_mut(&(declared.path)()) else {
                continue;
            };
            for method in (declared.methods)() {
                if let Some(operation) = operation_at(item, &method) {
                    stamp(operation, &declared.tool);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::TOOL_EXTENSION;

    /// The published document and [`super::tool_operations`] carry the same
    /// declarations on the same operations, and every declaration landed.
    #[test]
    fn the_published_document_carries_every_declared_tool() {
        let document: serde_json::Value =
            serde_json::from_str(&crate::openapi::openapi_json().unwrap()).unwrap();
        let operations = super::tool_operations().unwrap();
        let mut published = 0_usize;
        for (path, item) in document["paths"].as_object().unwrap() {
            for (method, operation) in item.as_object().unwrap() {
                let Some(tool) = operation.get(TOOL_EXTENSION) else {
                    continue;
                };
                published += 1;
                assert_eq!(
                    &operations[path][method][TOOL_EXTENSION], tool,
                    "{method} {path}"
                );
                assert_eq!(
                    operations[path][method]["operationId"], operation["operationId"],
                    "{method} {path}"
                );
            }
        }
        assert_eq!(published, super::declarations().len());
        let declared: usize = operations
            .as_object()
            .unwrap()
            .values()
            .map(|item| item.as_object().unwrap().len())
            .sum();
        assert_eq!(declared, published);
    }
}
