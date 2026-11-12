//! `POST /v1/mcp` over the analytics cell: the catalog an assistant sees, and
//! each tool call answered exactly as the route it loops back into answers
//! the same caller (MCP-CATALOG-1, #733, #744).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "route correspondence"
)]
use norn_api::host::DevHost;
use norn_cell::engine::NativeEngine;
use norn_cell_do::analytics::{self, DemoQuestions, QUESTION};
use norn_cell_do::mcp::{MCP_PATH, UI_EXTENSION, UI_MIME_TYPE, View};
use norn_cell_do::policy::{DEMO_GRANTS, ReadPolicy};
use norn_cell_do::principal::{DEMO_FIXTURES, FixturePrincipals};
use norn_cell_do::server::{self, Response};
use serde_json::{Value, json};

const SCOPE: &str = "orgId=org_demo&environmentId=analytics";
const VIEWER: Option<&str> = Some("Bearer fixture:viewer@demo");
const GUEST: Option<&str> = Some("Bearer fixture:guest@demo");
const STRANGER: Option<&str> = Some("Bearer fixture:stranger@demo");
const SECRET: [u8; 32] = [5; 32];

type Cell = (DevHost<NativeEngine>, DemoQuestions, FixturePrincipals);

fn cell() -> Cell {
    let engine = NativeEngine::open_in_memory().unwrap();
    let scope = analytics::analytics_scope();
    let opened = analytics::provision(engine, &scope).unwrap();
    let policy =
        ReadPolicy::compile(opened.store.model().object_types(), &scope, &DEMO_GRANTS).unwrap();
    let principals = FixturePrincipals::new(DEMO_FIXTURES, policy.caller());
    let host = DevHost::from_store(opened.store, scope.clone(), |command| {
        command.payload.clone()
    })
    .unwrap()
    .with_authorizer(policy);
    let runtime = DemoQuestions::new(opened.plan, scope)
        .with_handle_key(&SECRET)
        .with_metrics(opened.definitions, opened.catalog);
    (host, runtime, principals)
}

fn send(
    cell: &Cell,
    method: &str,
    path: &str,
    query: &str,
    auth: Option<&str>,
    body: &str,
) -> Response {
    server::handle_with(
        &cell.0,
        &cell.2,
        Some(&cell.1),
        &server::Request {
            method,
            path,
            query,
            authorization: auth,
            now: 0,
            body,
        },
    )
}

fn rpc(cell: &Cell, auth: Option<&str>, message: &Value) -> Response {
    send(cell, "POST", MCP_PATH, SCOPE, auth, &message.to_string())
}

fn result(cell: &Cell, auth: Option<&str>, method: &str, params: Value) -> Value {
    let response = rpc(
        cell,
        auth,
        &json!({"jsonrpc": "2.0", "id": 7, "method": method, "params": params}),
    );
    assert_eq!(response.status, 200, "{}", response.body);
    let body: Value = serde_json::from_str(&response.body).unwrap();
    assert_eq!(body["id"], 7);
    body["result"].clone()
}

fn tools(cell: &Cell, auth: Option<&str>) -> Vec<Value> {
    result(cell, auth, "tools/list", json!({"limit": 100}))["tools"]
        .as_array()
        .unwrap()
        .clone()
}

fn call_tool(cell: &Cell, auth: Option<&str>, name: &str, arguments: Value) -> Value {
    result(
        cell,
        auth,
        "tools/call",
        json!({"name": name, "arguments": arguments}),
    )
}

fn direct(cell: &Cell, path: &str, query: &str, auth: Option<&str>) -> (u16, Value) {
    let response = send(cell, "GET", path, query, auth, "");
    (
        response.status,
        serde_json::from_str(&response.body).unwrap(),
    )
}

#[test]
fn initialize_negotiates_the_revision_and_declares_the_ui_extension() {
    let cell = cell();
    for (asked, answered) in [
        ("2025-06-18", "2025-06-18"),
        ("2025-11-25", "2025-11-25"),
        ("2099-01-01", "2025-11-25"),
    ] {
        let init = result(
            &cell,
            VIEWER,
            "initialize",
            json!({"protocolVersion": asked, "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}}),
        );
        assert_eq!(init["protocolVersion"], answered);
        assert_eq!(init["capabilities"]["tools"]["listChanged"], false);
        assert_eq!(
            init["capabilities"]["extensions"][UI_EXTENSION]["mimeTypes"],
            json!([UI_MIME_TYPE])
        );
        assert!(
            init["instructions"]
                .as_str()
                .unwrap()
                .contains("norn_catalog")
        );
    }
    // A stateless request needs no prior handshake.
    assert!(!tools(&cell, VIEWER).is_empty());
}

#[test]
fn notifications_are_accepted_without_a_body() {
    let cell = cell();
    let response = rpc(
        &cell,
        VIEWER,
        &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    );
    assert_eq!((response.status, response.body.as_str()), (202, ""));
}

#[test]
fn only_post_from_a_verified_caller_in_the_cells_own_scope_is_served() {
    let cell = cell();
    let list = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}).to_string();
    let refused = |response: Response| {
        let body: Value = serde_json::from_str(&response.body).unwrap();
        (response.status, body["code"].as_str().unwrap().to_owned())
    };
    assert_eq!(
        refused(send(&cell, "GET", MCP_PATH, SCOPE, VIEWER, "")).0,
        405
    );
    for (query, auth) in [
        (SCOPE, None),
        (SCOPE, STRANGER),
        ("orgId=org_demo&environmentId=dev", VIEWER),
        ("orgId=org_demo", VIEWER),
    ] {
        let (status, _) = refused(send(&cell, "POST", MCP_PATH, query, auth, &list));
        assert!(status == 403 || status == 400, "{query} {auth:?}: {status}");
    }
}

#[test]
fn the_catalog_is_projected_from_the_cell_and_offers_no_raw_storage() {
    let cell = cell();
    let listed = tools(&cell, VIEWER);
    let names: Vec<&str> = listed
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    for expected in [
        "norn_catalog",
        "norn_query_objects",
        "norn_get_object",
        "norn_trace_object",
        "norn_profile_type",
        "norn_answer",
        "norn_trace_answer",
        "norn_evaluate",
        "norn_propose_definition",
    ] {
        assert!(
            names.contains(&expected),
            "{expected} missing from {names:?}"
        );
    }
    // Names both assistants accept as function names.
    assert!(names.iter().all(|name| {
        !name.is_empty()
            && name.len() <= 64
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    }));
    // Types and questions come from the cell, not from this file.
    let query = listed
        .iter()
        .find(|tool| tool["name"] == "norn_query_objects")
        .unwrap();
    assert_eq!(
        query["inputSchema"]["properties"]["type"]["enum"],
        json!([analytics::TYPE])
    );
    let answer = listed
        .iter()
        .find(|tool| tool["name"] == "norn_answer")
        .unwrap();
    assert_eq!(
        answer["inputSchema"]["properties"]["question"]["enum"],
        json!([QUESTION])
    );
    assert_eq!(answer["_meta"]["ui"]["resourceUri"], View::Answer.uri());
    assert_eq!(answer["_meta"]["openai/outputTemplate"], View::Answer.uri());
    assert_eq!(answer["annotations"]["readOnlyHint"], true);
    // #733: no tool takes or names physical storage, at any depth of its schema.
    let forbidden = [
        "sql",
        "table",
        "tables",
        "column",
        "columns",
        "statement",
        "schema_name",
    ];
    fn keys(value: &Value, out: &mut Vec<String>) {
        match value {
            Value::Object(map) => {
                for (key, inner) in map {
                    out.push(key.to_lowercase());
                    keys(inner, out);
                }
            }
            Value::Array(items) => items.iter().for_each(|item| keys(item, out)),
            _ => {}
        }
    }
    for tool in &listed {
        let mut found = Vec::new();
        keys(&tool["inputSchema"], &mut found);
        let name = tool["name"].as_str().unwrap().to_lowercase();
        assert!(
            forbidden
                .iter()
                .all(|word| !name.contains(word) && !found.iter().any(|key| key == word)),
            "{name} exposes raw storage"
        );
    }
    // An unknown tool (say, a raw query) is refused by the protocol, not routed.
    let response = rpc(
        &cell,
        VIEWER,
        &json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "sql", "arguments": {"sql": "select 1"}}}),
    );
    let body: Value = serde_json::from_str(&response.body).unwrap();
    assert_eq!(body["error"]["code"], -32602);
}

#[test]
fn a_tool_answers_exactly_what_its_route_answers_the_same_caller() {
    let cell = cell();
    for auth in [VIEWER, GUEST] {
        let (status, answer) = direct(
            &cell,
            &format!("/v1/questions/{QUESTION}/answer"),
            SCOPE,
            auth,
        );
        assert_eq!(status, 200);
        let called = call_tool(&cell, auth, "norn_answer", json!({"question": QUESTION}));
        assert_eq!(called["isError"], false);
        assert_eq!(called["structuredContent"], answer);
        // The text block is the same document, for hosts without structured content.
        let text: Value =
            serde_json::from_str(called["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(text, answer);

        let bucket = answer["rows"]
            .as_object()
            .unwrap()
            .keys()
            .next()
            .unwrap()
            .clone();
        let lineage = answer["lineageRef"].as_str().unwrap();
        let pins = form_urlencoded::Serializer::new(String::new())
            .append_pair("lineageRef", lineage)
            .append_pair("bucket", &bucket)
            .finish();
        let (_, trace) = direct(
            &cell,
            &format!("/v1/questions/{QUESTION}/trace"),
            &format!("{SCOPE}&{pins}"),
            auth,
        );
        let traced = call_tool(
            &cell,
            auth,
            "norn_trace_answer",
            json!({"question": QUESTION, "lineageRef": lineage, "bucket": bucket}),
        );
        assert_eq!(traced["structuredContent"], trace);

        let (_, catalog) = direct(&cell, "/v1/definitions", SCOPE, auth);
        assert_eq!(
            call_tool(&cell, auth, "norn_catalog", json!({}))["structuredContent"],
            catalog
        );
    }
}

#[test]
fn refusals_come_back_as_tool_errors_with_the_routes_own_refusal() {
    let cell = cell();
    let (status, refusal) = direct(&cell, "/v1/questions/nope/answer", SCOPE, VIEWER);
    assert_ne!(status, 200);
    let called = call_tool(&cell, VIEWER, "norn_answer", json!({"question": "nope"}));
    assert_eq!(called["isError"], true);
    assert_eq!(called["structuredContent"], refusal);

    let (_, refusal) = direct(&cell, "/v1/objects/demo.Nothing", SCOPE, VIEWER);
    let called = call_tool(
        &cell,
        VIEWER,
        "norn_query_objects",
        json!({"type": "demo.Nothing"}),
    );
    assert_eq!(called["isError"], true);
    assert_eq!(called["structuredContent"], refusal);

    // A malformed argument is refused before any route runs.
    let called = call_tool(&cell, VIEWER, "norn_query_objects", json!({"type": [1]}));
    assert_eq!(called["isError"], true);
}
