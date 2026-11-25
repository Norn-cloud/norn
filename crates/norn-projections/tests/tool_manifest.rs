//! TOOL-MANIFEST-1 (#1762): a route-backed tool is read from its `OpenAPI`
//! operation, and a declaration that does not fit the operation is refused.
use norn_ir::JsonValue;
use norn_projections::tool_manifest::{
    Domains, Encoding, Location, Offered, TOOL_EXTENSION, ToolManifest, ToolMethod, route_tool,
};
use serde_json::{Value, json};

fn paths(value: &Value) -> Result<JsonValue, serde_json::Error> {
    serde_json::from_value(value.clone())
}

fn declared(arguments: &Value) -> Value {
    json!({
        "name": "norn_get_object",
        "title": "Read a record",
        "description": "One record by type and id.",
        "inputSchema": {"type": "object", "additionalProperties": false, "required": ["type", "id"], "properties": {
            "type": {"x-norn-domain": "objectType"}, "id": {"type": "string"}, "detail": {"type": "string"}
        }},
        "annotations": {"readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false},
        "view": "records",
        "offered": "always",
        "arguments": arguments,
    })
}

fn document(tool: &Value) -> Value {
    json!({
        "/v1/objects/{type}/{id}": {"get": {
            "operationId": "get_object",
            "parameters": [
                {"in": "query", "name": "orgId"},
                {"in": "query", "name": "environmentId"},
                {"in": "path", "name": "type"},
                {"in": "path", "name": "id"}
            ],
            TOOL_EXTENSION: tool,
        }},
        "/v1/inbox": {"get": {"operationId": "get_inbox"}},
    })
}

fn bound() -> Value {
    json!([
        {"name": "type", "in": "path"},
        {"name": "id", "in": "path"},
        {"name": "detail", "in": "local"}
    ])
}

#[test]
fn a_tool_is_read_from_the_operation_that_declares_it() {
    let manifest =
        ToolManifest::from_openapi_paths(&paths(&document(&declared(&bound()))).expect("JSON"))
            .expect("fits");
    assert_eq!(
        manifest.tools().len(),
        1,
        "an operation without a tool is no tool"
    );
    let tool = manifest.tool("norn_get_object").expect("declared");
    assert_eq!(tool.operation_id, "get_object");
    assert_eq!(tool.method, ToolMethod::Get);
    assert_eq!(tool.path, "/v1/objects/{type}/{id}");
    assert_eq!(tool.offered, Offered::Always);
    assert_eq!(tool.view.as_deref(), Some("records"));
    let first = &tool.arguments[0];
    assert_eq!(
        (
            first.name.as_str(),
            first.location,
            first.encoding,
            first.required
        ),
        ("type", Location::Path, Encoding::Text, true)
    );
    assert_eq!(tool.arguments[2].location, Location::Local);
    assert!(!tool.arguments[2].required);
}

#[test]
fn the_projection_binds_each_domain_to_the_callers_workspace() {
    let manifest =
        ToolManifest::from_openapi_paths(&paths(&document(&declared(&bound()))).expect("JSON"))
            .expect("fits");
    let tool = manifest.tool("norn_get_object").expect("declared");
    let types = vec!["demo.Batch".to_owned()];
    let projected = serde_json::to_value(route_tool(
        tool,
        &Domains {
            object_types: &types,
            questions: &[],
        },
    ))
    .expect("JSON");
    assert_eq!(projected["name"], "norn_get_object");
    assert_eq!(projected["title"], "Read a record");
    assert_eq!(
        projected["inputSchema"]["properties"]["type"],
        json!({"type": "string", "enum": ["demo.Batch"], "description": "An object type this workspace declares."})
    );
    assert_eq!(projected["annotations"]["readOnlyHint"], true);
    assert!(projected.get("_meta").is_none() && projected.get("outputSchema").is_none());
    let open = serde_json::to_value(route_tool(
        tool,
        &Domains {
            object_types: &[],
            questions: &[],
        },
    ))
    .expect("JSON");
    assert_eq!(
        open["inputSchema"]["properties"]["type"],
        json!({"type": "string", "description": "An object type from norn_catalog."})
    );
}

#[test]
fn a_declaration_that_does_not_fit_its_operation_is_refused() {
    let refused = |tool: Value| {
        ToolManifest::from_openapi_paths(&paths(&document(&tool)).expect("JSON"))
            .expect_err("refused")
            .to_string()
    };
    // A query parameter the operation does not declare.
    let mut arguments = bound();
    arguments[2] = json!({"name": "detail", "in": "query"});
    assert!(refused(declared(&arguments)).contains("does not fit"));
    // A body on a GET.
    arguments[2] = json!({"name": "detail", "in": "body"});
    assert!(refused(declared(&arguments)).contains("does not fit"));
    // A path segment left unbound, and a property with no argument.
    let unbound = json!([{"name": "type", "in": "path"}, {"name": "detail", "in": "local"}]);
    assert!(refused(declared(&unbound)).contains("id has no argument"));
    // An argument the input schema does not describe.
    let mut extra = bound();
    extra
        .as_array_mut()
        .expect("array")
        .push(json!({"name": "extra", "in": "local"}));
    assert!(refused(declared(&extra)).contains("property of inputSchema"));
    // An unknown domain.
    let mut tool = declared(&bound());
    tool["inputSchema"]["properties"]["id"] = json!({"x-norn-domain": "table"});
    assert!(refused(tool).contains("x-norn-domain"));
    // A tool with no label.
    let mut tool = declared(&bound());
    tool["title"] = json!(" ");
    assert!(refused(tool).contains("title"));
    // Two operations, one tool name.
    let tool = declared(&bound());
    let mut twice = document(&tool);
    let mut again = twice["/v1/objects/{type}/{id}"].clone();
    again["get"]["operationId"] = json!("get_object_again");
    twice["/v1/objects/{type}/{id}/again"] = again;
    let error =
        ToolManifest::from_openapi_paths(&paths(&twice).expect("JSON")).expect_err("refused");
    assert!(error.to_string().contains("repeats"), "{error}");
}

/// A path segment is filled only by an argument that travels in the path: an
/// argument of the same name bound in the query, the body or locally leaves
/// the segment open, so the manifest refuses it rather than the call failing.
#[test]
fn a_path_segment_bound_outside_the_path_is_refused() {
    // The fixture with `id` bound `in` somewhere other than the path, on an
    // operation where that location exists.
    let id_bound_in = |location: &str| {
        let mut arguments = bound();
        arguments[1] = json!({"name": "id", "in": location});
        let mut document = document(&declared(&arguments));
        let operation = &mut document["/v1/objects/{type}/{id}"];
        match location {
            "query" => operation["get"]["parameters"]
                .as_array_mut()
                .expect("array")
                .push(json!({"in": "query", "name": "id"})),
            "body" => {
                let mut post = operation["get"].take();
                post["requestBody"] =
                    json!({"content": {"application/json": {"schema": {"type": "object"}}}});
                *operation = json!({ "post": post });
            }
            _ => {}
        }
        document
    };
    for location in ["local", "query", "body"] {
        let error = ToolManifest::from_openapi_paths(&paths(&id_bound_in(location)).expect("JSON"))
            .expect_err(location)
            .to_string();
        assert!(
            error.contains("path segment id has no argument in the path"),
            "{location}: {error}"
        );
    }
}

/// The caller's scope is the host's to send: no tool argument binds `orgId` or
/// `environmentId`, even where the operation declares them.
#[test]
fn a_scope_parameter_is_never_a_tool_argument() {
    for scope in ["orgId", "environmentId"] {
        let mut tool = declared(&bound());
        tool["inputSchema"]["properties"][scope] = json!({"type": "string"});
        tool["arguments"]
            .as_array_mut()
            .expect("array")
            .push(json!({"name": scope, "in": "query"}));
        let error = ToolManifest::from_openapi_paths(&paths(&document(&tool)).expect("JSON"))
            .expect_err(scope)
            .to_string();
        assert!(error.contains("scope parameters"), "{scope}: {error}");
    }
}

/// A path template with a `{` left open is refused when the manifest is read,
/// not when a call fills it.
#[test]
fn a_path_template_with_an_unclosed_segment_is_refused() {
    let mut arguments = bound();
    arguments[1] = json!({"name": "id", "in": "local"});
    let fixture = document(&declared(&arguments));
    let mut open = serde_json::Map::new();
    open.insert(
        "/v1/objects/{type}/{id".to_owned(),
        fixture["/v1/objects/{type}/{id}"].clone(),
    );
    let error = ToolManifest::from_openapi_paths(&paths(&Value::Object(open)).expect("JSON"))
        .expect_err("refused")
        .to_string();
    assert!(error.contains("unclosed"), "{error}");
}
