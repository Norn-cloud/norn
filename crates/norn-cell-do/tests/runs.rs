//! RUN-ROUTES-1 through the DO fetch dispatch: a run of a flow installed
//! after the cell opened starts once, reads back with its step timeline under
//! the ordinary Values read gate, and is started only by the declared run
//! starters. A run the caller may not read is indistinguishable from an
//! absent one, and a step the cell cannot move itself waits, never errs.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "route correspondence"
)]

use norn_api::definition_routes::DefinitionKinds;
use norn_api::host::DevHost;
use norn_cell::engine::NativeEngine;
use norn_cell_do::analytics::{self, DemoQuestions};
use norn_cell_do::policy::{DEMO_GRANTS, ReadPolicy};
use norn_cell_do::principal::FixturePrincipals;
use norn_cell_do::server::{self, Response};
use serde_json::{Value, json};

const SCOPE: &str = "orgId=org_demo&environmentId=analytics";
const ADMIN: Option<&str> = Some("Bearer fixture:admin@runs");
const MEMBER: Option<&str> = Some("Bearer fixture:member@runs");
const GUEST: Option<&str> = Some("Bearer fixture:guest@runs");
const FIXTURES: &[(&str, &[&str])] = &[
    ("admin@runs", &["admin"]),
    ("member@runs", &["member"]),
    ("guest@runs", &["viewer"]),
];
const ROLES: [&str; 4] = ["viewer", "member", "admin", "owner"];

/// `lowerFlow`'s golden output: one perform step, `classify`.
const LOWERED: &str =
    include_str!("../../../tools/norn-ts/fixtures/vertical/route-message.ir.json");
const FLOW_ID: &str = "crm.route_message";
const TOOL: &str = "norn.classify.whatsapp-intent";

struct Cell {
    host: DevHost<NativeEngine>,
    questions: DemoQuestions,
    principals: FixturePrincipals,
    kinds: DefinitionKinds,
}

/// The registered built-ins, and the test's tool as if a built-in.
fn resolve(function: &str, version: u32) -> bool {
    norn_cell::builtin_registered(function, version) || (function == TOOL && version == 1)
}

fn cell() -> Cell {
    let scope = analytics::analytics_scope();
    let opened = analytics::provision(NativeEngine::open_in_memory().unwrap(), &scope).unwrap();
    let policy =
        ReadPolicy::compile(opened.store.model().object_types(), &scope, &DEMO_GRANTS).unwrap();
    let principals = FixturePrincipals::new(FIXTURES, policy.caller());
    let host = DevHost::from_store(opened.store, scope.clone(), |command| {
        command.payload.clone()
    })
    .unwrap()
    .with_authorizer(policy);
    let questions = DemoQuestions::new(opened.plan, scope)
        .with_handle_key(&[5; 32])
        .with_metrics(opened.definitions, opened.catalog);
    Cell {
        host,
        questions,
        principals,
        kinds: DefinitionKinds::default()
            .with_roles(ROLES)
            .with_restater(norn_api::definition_routes::checker_restatement)
            .with_builtins(resolve),
    }
}

fn call(
    cell: &Cell,
    method: &str,
    path: &str,
    query: &str,
    auth: Option<&str>,
    body: &str,
) -> (u16, Value) {
    let request = server::Request {
        method,
        path,
        query,
        authorization: auth,
        now: 1_800_000_000,
        body,
    };
    let Response { status, body } = server::handle_all(
        &cell.host,
        &cell.principals,
        Some(&cell.questions),
        &cell.kinds,
        &request,
    );
    (status, serde_json::from_str(&body).unwrap())
}

fn post(cell: &Cell, path: &str, auth: Option<&str>, body: &Value) -> (u16, Value) {
    call(cell, "POST", path, SCOPE, auth, &body.to_string())
}

fn get(cell: &Cell, path: &str, query: &str, auth: Option<&str>) -> (u16, Value) {
    call(cell, "GET", path, &format!("{SCOPE}{query}"), auth, "")
}

/// The flow with its one perform naming `function` (version 1), scoped to
/// this cell, and installed by signing the server's words for it.
fn install(cell: &Cell, function: &str) {
    // Every mention of the tool (the command record and the rule's output
    // schema) names `function` instead.
    let artifacts: Vec<Value> = serde_json::from_str(&LOWERED.replace(TOOL, function)).unwrap();
    let artifacts: Vec<Value> = artifacts
        .into_iter()
        .map(|mut artifact| {
            artifact["scope"] = json!({ "orgId": "org_demo", "environmentId": "analytics" });
            artifact
        })
        .collect();
    let mut body = json!({ "kind": "flow", "id": FLOW_ID, "payload": {
        "flow": { "id": FLOW_ID, "version": "1.0.0", "title": "Route a message",
            "steps": [{ "id": "classify", "label": "Classify the message" }] },
        "artifacts": artifacts } });
    let (_, admitted) = post(cell, "/v1/definitions:admit", ADMIN, &body);
    body["signoff"] = json!({ "restatementSha256": admitted["restatementSha256"] });
    let (status, installed) = post(cell, "/v1/definitions:install", ADMIN, &body);
    assert_eq!(status, 200, "{installed}");
}

fn start(cell: &Cell, auth: Option<&str>, flow: &str, run: &str) -> (u16, Value) {
    post(
        cell,
        "/v1/runs",
        auth,
        &json!({ "flow": flow, "runId": run }),
    )
}

/// RUN-START-ONCE: the start turn commits the run and its first step; an
/// exact retry, the read and the list return the same document.
#[test]
fn a_run_starts_once_and_reads_back_with_its_timeline() {
    let cell = cell();
    install(&cell, "norn.decision");
    let (status, run) = start(&cell, MEMBER, FLOW_ID, "run-1");
    assert_eq!(status, 201, "{run}");
    assert_eq!(run["id"], json!("run-1"));
    assert_eq!(
        run["flow"],
        json!({ "id": FLOW_ID, "ruleId": "crm.route_message.flow.rule", "version": "1.0.0",
            "title": "Route a message" })
    );
    assert_eq!(run["objectType"], json!("crm.RouteMessageRun"));
    assert_eq!(run["generation"], json!(1));
    assert_eq!(run["state"], json!("awaiting_retry_classify_attempt_1"));
    assert_eq!(run["terminal"], json!(false));
    // The ask waits on a person: a state, not an error.
    assert_eq!(
        run["steps"],
        json!([{ "id": "classify", "label": "Classify the message", "kind": "decide",
            "status": "waiting", "waitingOn": "decision", "startedAtMs": run["createdAtMs"],
            "endedAtMs": null, "receipts": [], "error": null }])
    );

    let (status, again) = start(&cell, MEMBER, FLOW_ID, "run-1");
    assert_eq!((status, &again), (200, &run));
    assert_eq!(get(&cell, "/v1/runs/run-1", "", MEMBER), (200, run.clone()));
    assert_eq!(get(&cell, "/v1/runs/run-1", "", ADMIN), (200, run));
}

/// A tool step the cell does not execute waits on an executor.
#[test]
fn a_tool_step_waits_on_an_executor_and_is_not_an_error() {
    let cell = cell();
    install(&cell, TOOL);
    let (status, run) = start(&cell, ADMIN, FLOW_ID, "run-tool");
    assert_eq!(status, 201, "{run}");
    let step = &run["steps"][0];
    assert_eq!(
        (
            &step["kind"],
            &step["status"],
            &step["waitingOn"],
            &step["error"]
        ),
        (
            &json!("perform"),
            &json!("waiting"),
            &json!("executor"),
            &Value::Null
        )
    );
}

/// RUN-START-REFUSE: only a declared starter in this scope starts a run,
/// every denial is the same document, and nothing commits.
#[test]
fn starts_are_decided_by_the_declared_starters_and_refuse_with_typed_codes() {
    let cell = cell();
    install(&cell, "norn.decision");
    let (status, denied) = start(&cell, GUEST, FLOW_ID, "run-1");
    assert_eq!(status, 403, "{denied}");
    assert_eq!(denied["code"], json!("authz.denied"));
    assert_eq!(start(&cell, None, FLOW_ID, "run-1"), (403, denied.clone()));
    let foreign = call(
        &cell,
        "POST",
        "/v1/runs",
        "orgId=org_other&environmentId=analytics",
        MEMBER,
        &json!({ "flow": FLOW_ID, "runId": "run-1" }).to_string(),
    );
    assert_eq!(foreign, (403, denied));

    let (status, unknown) = start(&cell, MEMBER, "crm.missing", "run-1");
    assert_eq!(
        (status, &unknown["code"]),
        (404, &json!("run.flow_not_found"))
    );
    let (status, invalid) = start(&cell, MEMBER, FLOW_ID, "no spaces");
    assert_eq!((status, &invalid["code"]), (400, &json!("request.invalid")));
    let (status, extra) = post(
        &cell,
        "/v1/runs",
        MEMBER,
        &json!({ "flow": FLOW_ID, "runId": "run-1", "as": "admin" }),
    );
    assert_eq!((status, &extra["code"]), (400, &json!("request.invalid")));
    // Nothing committed: the refused run ids are free.
    assert_eq!(
        get(&cell, "/v1/runs/run-1", "", ADMIN).1["code"],
        json!("run.not_found")
    );
    let (status, method) = call(&cell, "DELETE", "/v1/runs/run-1", SCOPE, ADMIN, "");
    assert_eq!(
        (status, &method["code"]),
        (405, &json!("route.method_not_allowed"))
    );
}

/// RUN-READ-GATED: a reader without Values on the Run type gets exactly the
/// absent run's refusal.
#[test]
fn an_unreadable_run_is_indistinguishable_from_an_absent_one() {
    let cell = cell();
    install(&cell, "norn.decision");
    assert_eq!(start(&cell, MEMBER, FLOW_ID, "run-1").0, 201);
    let (status, hidden) = get(&cell, "/v1/runs/run-1", "", GUEST);
    assert_eq!((status, &hidden["code"]), (404, &json!("run.not_found")));
    assert_eq!(
        get(&cell, "/v1/runs/absent", "", GUEST),
        (404, hidden.clone())
    );
    assert_eq!(get(&cell, "/v1/runs/absent", "", MEMBER), (404, hidden));
    let (status, anonymous) = get(&cell, "/v1/runs/run-1", "", None);
    assert_eq!((status, &anonymous["code"]), (403, &json!("authz.denied")));
}
