//! DECISIONS-1 through the DO fetch dispatch: a run that asks a person waits
//! on an open decision; only a holder of the decision's declared roles sees
//! and decides it; the decision resolves once, moves the run in the same
//! commit, and replays for its own identical retry. Everyone else sees an
//! absent decision, and a reader of the run learns who resolved it first.
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
const ADMIN: Option<&str> = Some("Bearer fixture:admin@decide");
const OWNER: Option<&str> = Some("Bearer fixture:owner@decide");
const MEMBER: Option<&str> = Some("Bearer fixture:member@decide");
const FIXTURES: &[(&str, &[&str])] = &[
    ("admin@decide", &["admin"]),
    ("owner@decide", &["owner"]),
    ("member@decide", &["member"]),
];
const ROLES: [&str; 4] = ["viewer", "member", "admin", "owner"];
/// `lowerFlow`'s output for `tools/norn-ts/fixtures/flows/ask.builder.ts`.
const LOWERED: &str = include_str!("../../../tools/norn-ts/fixtures/flows/ask.ir.json");
const FLOW_ID: &str = "ops.approve_refund";
/// The host clock (Unix seconds) every request is served at, unless a test
/// moves it.
const NOW: u64 = 1_800_000_000;

struct Cell {
    host: DevHost<NativeEngine>,
    questions: DemoQuestions,
    principals: FixturePrincipals,
    kinds: DefinitionKinds,
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
    let cell = Cell {
        host,
        questions,
        principals,
        kinds: DefinitionKinds::default()
            .with_roles(ROLES)
            .with_restater(norn_api::definition_routes::checker_restatement)
            .with_builtins(norn_cell::builtin_registered),
    };
    install(&cell);
    cell
}

fn call(
    cell: &Cell,
    method: &str,
    path: &str,
    query: &str,
    auth: Option<&str>,
    body: &str,
    now: u64,
) -> (u16, Value) {
    let request = server::Request {
        method,
        path,
        query,
        authorization: auth,
        now,
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
    call(cell, "POST", path, SCOPE, auth, &body.to_string(), NOW)
}

fn get(cell: &Cell, path: &str, query: &str, auth: Option<&str>) -> (u16, Value) {
    call(cell, "GET", path, &format!("{SCOPE}{query}"), auth, "", NOW)
}

fn install(cell: &Cell) {
    let artifacts: Vec<Value> = serde_json::from_str::<Vec<Value>>(LOWERED)
        .unwrap()
        .into_iter()
        .map(|mut artifact| {
            artifact["scope"] = json!({ "orgId": "org_demo", "environmentId": "analytics" });
            artifact
        })
        .collect();
    let mut body = json!({ "kind": "flow", "id": FLOW_ID, "payload": {
        "flow": { "id": FLOW_ID, "version": "1.0.0", "title": "Approve a refund",
            "steps": [{ "id": "ask_review", "label": "Review the refund" }] },
        "artifacts": artifacts } });
    let (_, admitted) = post(cell, "/v1/definitions:admit", ADMIN, &body);
    body["signoff"] = json!({ "restatementSha256": admitted["restatementSha256"] });
    let (status, installed) = post(cell, "/v1/definitions:install", ADMIN, &body);
    assert_eq!(status, 200, "{installed}");
}

fn start(cell: &Cell, run: &str) -> Value {
    let (status, started) = post(
        cell,
        "/v1/runs",
        MEMBER,
        &json!({ "flow": FLOW_ID, "runId": run }),
    );
    assert_eq!(status, 201, "{started}");
    started
}

fn decide(cell: &Cell, id: &str, auth: Option<&str>, body: &Value, now: u64) -> (u16, Value) {
    call(
        cell,
        "POST",
        &format!("/v1/decisions/{id}:decide"),
        SCOPE,
        auth,
        &body.to_string(),
        now,
    )
}

/// DECIDE-ONCE: the decider sees the open decision, decides it once, and the
/// run moves on in the same commit; a retry replays, a second decider learns
/// who was first, and a non-decider sees nothing.
#[test]
fn a_decider_decides_once_and_the_run_moves_on() {
    let cell = cell();
    let run = start(&cell, "refund-1");
    assert_eq!(run["state"], json!("awaiting_perform_ask_review"));
    assert_eq!(run["steps"][0]["waitingOn"], json!("decision"));
    // The starter is not a decider: the run waits on no decision it may decide.
    assert_eq!(run["decision"], Value::Null);

    let (status, page) = get(&cell, "/v1/decisions", "&state=pending", ADMIN);
    assert_eq!(status, 200, "{page}");
    assert_eq!(page["total"], json!(1));
    let decision = &page["items"][0];
    let id = decision["id"].as_str().unwrap().to_owned();
    assert!(id.starts_with("dec_"), "{id}");
    assert_eq!(decision["question"], json!("Refund order 1042 in full?"));
    assert_eq!(decision["risk"], json!("high"));
    assert_eq!(decision["agentPick"], json!("approve"));
    assert_eq!(
        decision["step"],
        json!({ "id": "ask_review", "label": "Review the refund" })
    );
    assert_eq!(decision["run"]["id"], json!("refund-1"));
    assert_eq!(decision["flow"]["title"], json!("Approve a refund"));
    assert_eq!(
        decision["expiresAtMs"].as_u64().unwrap() - decision["requestedAtMs"].as_u64().unwrap(),
        86_400_000
    );
    assert_eq!(
        decision["options"][0]["effect"],
        json!({ "action": "ops.refund.approve" })
    );
    let (_, read) = get(&cell, "/v1/runs/refund-1", "", ADMIN);
    assert_eq!(
        read["decision"],
        json!({ "id": id, "expiresAtMs": decision["expiresAtMs"] })
    );

    // A non-decider sees no decision, and the badge count is 0 for it.
    assert_eq!(
        get(&cell, "/v1/decisions", "&state=pending&limit=0", MEMBER).1,
        json!({ "items": [], "total": 0, "nextCursor": null, "serverNowMs": page["serverNowMs"] })
    );
    let answer = json!({ "choice": "approve", "note": "Within policy", "idempotencyKey": "k-1" });
    let (status, absent) = decide(&cell, &id, MEMBER, &answer, NOW);
    assert_eq!(
        (status, &absent["code"]),
        (404, &json!("decision.not_found"))
    );
    assert_eq!(
        decide(&cell, "dec_missing", ADMIN, &answer, NOW),
        (404, absent)
    );

    let (status, decided) = decide(&cell, &id, ADMIN, &answer, NOW);
    assert_eq!(status, 200, "{decided}");
    assert_eq!(decided["outcome"], json!("decided"));
    assert_eq!(decided["decision"]["choice"], json!("approve"));
    assert_eq!(decided["decision"]["decidedBy"], json!("you"));
    assert_eq!(
        decided["run"],
        json!({ "id": "refund-1", "state": "completed" })
    );

    // The same key and body replays the same commit; another body conflicts.
    let (status, replayed) = decide(&cell, &id, ADMIN, &answer, NOW);
    assert_eq!(status, 200, "{replayed}");
    assert_eq!(replayed["outcome"], json!("replayed"));
    assert_eq!(replayed["receipt"], decided["receipt"]);
    let other = json!({ "choice": "reject", "idempotencyKey": "k-1" });
    let (status, conflict) = decide(&cell, &id, ADMIN, &other, NOW);
    assert_eq!(
        (status, &conflict["code"]),
        (409, &json!("decision.idempotency_conflict"))
    );

    // A second decider learns who was first, and what, since it reads the run.
    let (status, resolved) = decide(&cell, &id, OWNER, &other, NOW);
    assert_eq!(status, 409, "{resolved}");
    assert_eq!(resolved["code"], json!("decision.resolved"));
    assert_eq!(resolved["resolution"]["resolution"], json!("decided"));
    assert_eq!(resolved["resolution"]["by"], json!("another decider"));
    assert_eq!(resolved["resolution"]["choice"], json!("approve"));

    let (_, done) = get(&cell, "/v1/runs/refund-1", "", MEMBER);
    assert_eq!(done["terminal"], json!(true));
    assert_eq!(done["decision"], Value::Null);
    let kinds: Vec<&str> = done["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|step| step["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["decide", "succeed"], "{done}");
    assert_eq!(
        get(&cell, "/v1/decisions", "&state=pending", ADMIN).1["total"],
        json!(0)
    );
}

/// Choice and note refusals point at the field; nothing is recorded.
#[test]
fn a_bad_choice_or_note_is_refused_and_records_nothing() {
    let cell = cell();
    start(&cell, "refund-2");
    let (_, page) = get(&cell, "/v1/decisions", "&state=pending", ADMIN);
    let id = page["items"][0]["id"].as_str().unwrap().to_owned();
    let (status, unknown) = decide(
        &cell,
        &id,
        ADMIN,
        &json!({ "choice": "maybe", "idempotencyKey": "k" }),
        NOW,
    );
    assert_eq!(
        (status, &unknown["code"]),
        (422, &json!("decision.unknown_choice"))
    );
    assert_eq!(
        unknown["diagnostics"][0]["span"]["pointer"],
        json!("/choice")
    );
    let long = "x".repeat(501);
    let (status, note) = decide(
        &cell,
        &id,
        ADMIN,
        &json!({ "choice": "approve", "note": long, "idempotencyKey": "k" }),
        NOW,
    );
    assert_eq!(
        (status, &note["code"]),
        (422, &json!("decision.note_too_long"))
    );
    assert_eq!(
        get(&cell, "/v1/decisions", "&state=pending", ADMIN).1["total"],
        json!(1)
    );
}

/// DECIDE-EXPIRED: a decision past its deadline is no longer open; the first
/// call after the deadline resolves it as expired and moves the run.
#[test]
fn an_expired_decision_is_gone() {
    let cell = cell();
    start(&cell, "refund-3");
    let (_, page) = get(&cell, "/v1/decisions", "&state=pending", ADMIN);
    let id = page["items"][0]["id"].as_str().unwrap().to_owned();
    let later = NOW + 2 * 86_400;
    let (status, expired) = decide(
        &cell,
        &id,
        ADMIN,
        &json!({ "choice": "approve", "idempotencyKey": "k" }),
        later,
    );
    assert_eq!(
        (status, &expired["code"]),
        (410, &json!("decision.expired"))
    );
    // The run received the step's typed `Expired` failure, in the same commit
    // that recorded the resolution; a later call learns it expired.
    let (_, run) = call(&cell, "GET", "/v1/runs/refund-3", SCOPE, MEMBER, "", later);
    assert_eq!(run["state"], json!("failed_expired"), "{run}");
    assert_eq!(run["terminal"], json!(true));
    let (status, again) = decide(
        &cell,
        &id,
        ADMIN,
        &json!({ "choice": "approve", "idempotencyKey": "k" }),
        later,
    );
    assert_eq!(status, 409, "{again}");
    assert_eq!(again["resolution"]["resolution"], json!("expired"));
    assert_eq!(again["resolution"]["by"], Value::Null);
    let (_, listed) = call(
        &cell,
        "GET",
        "/v1/decisions",
        &format!("{SCOPE}&state=pending"),
        ADMIN,
        "",
        later,
    );
    assert_eq!(listed["total"], json!(0));
}
