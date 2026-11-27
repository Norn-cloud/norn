//! APPROVAL-1 through the DO fetch dispatch: while an `approval` definition
//! gates an action, a call that would commit parks as one open decision
//! (202), a call that would be refused is refused as it would be ungated and
//! parks nothing, the requester never decides their own call, and an
//! approved call commits on the requester's own retry, under the
//! requester's current authority: the decider's request never runs it
//! (APPROVAL-RECOVERY-1). A flow step's call parks exactly like a direct
//! one, and so does an MCP tool call (APPROVAL-MCP-PARKS).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "dispatch correspondence"
)]

#[path = "support/step_cell.rs"]
mod step_cell;
use norn_cell::engine::SqlEngine;
use serde_json::{Value, json};
use step_cell::{
    APPROVER, Cell, FLOW_ID, MELT_ACTION, MEMBER, NOW, OPERATOR, SCOPE, call, call_at, call_with,
    cell, melt_body,
};

const APPROVAL_ID: &str = "ops.melt_approval";

/// Install the approval gating the melt action, deciders `admin`, signed by
/// the operator.
fn gate<E: SqlEngine>(cell: &Cell<E>, expires_in_ms: Option<u64>) {
    gate_asking(cell, expires_in_ms, "Convert this melt?", None);
}

/// [`gate`], asking `question`, replacing revision `replaces` when given:
/// installing it again with another question is a new definitions version.
fn gate_asking<E: SqlEngine>(
    cell: &Cell<E>,
    expires_in_ms: Option<u64>,
    question: &str,
    replaces: Option<u64>,
) {
    let mut payload = json!({
        "action": MELT_ACTION,
        "decider": { "roles": ["admin"] },
        "question": question,
        "risk": "high",
    });
    if let Some(ms) = expires_in_ms {
        payload["expiresInMs"] = json!(ms);
    }
    let mut body = json!({ "kind": "approval", "id": APPROVAL_ID, "payload": payload });
    let (status, admitted) = call(
        cell,
        "POST",
        "/v1/definitions:admit",
        OPERATOR,
        &body.to_string(),
    );
    assert_eq!(status, 200, "{admitted}");
    body["signoff"] = json!({ "restatementSha256": admitted["restatementSha256"] });
    if let Some(revision) = replaces {
        body["expectedVersion"] = json!(revision);
    }
    let (status, installed) = call(
        cell,
        "POST",
        "/v1/definitions:install",
        OPERATOR,
        &body.to_string(),
    );
    assert_eq!(status, 200, "{installed}");
}

fn melt7(key: &str) -> String {
    melt_body("melt-7", "12", "10", key)
}

fn convert<E: SqlEngine>(cell: &Cell<E>, auth: Option<&str>, body: &str) -> (u16, Value) {
    call(
        cell,
        "POST",
        &format!("/v1/actions/{MELT_ACTION}"),
        auth,
        body,
    )
}

fn pending(cell: &Cell, auth: Option<&str>) -> Value {
    let query = format!("{SCOPE}&state=pending");
    let (status, page) = call_with(cell, "GET", "/v1/decisions", &query, auth, "");
    assert_eq!(status, 200, "{page}");
    page
}

fn decide<E: SqlEngine>(
    cell: &Cell<E>,
    auth: Option<&str>,
    id: &str,
    choice: &str,
) -> (u16, Value) {
    call(
        cell,
        "POST",
        &format!("/v1/decisions/{id}:decide"),
        auth,
        &json!({ "choice": choice, "idempotencyKey": format!("{choice}-1") }).to_string(),
    )
}

/// The answer's `call` while the approved call waits for its requester.
fn not_executed() -> Value {
    json!({ "committed": false, "state": "not_executed" })
}

/// The run's starter retries its start: the exact retry drives the run, and
/// runs a step's approved call under the starter's own authority.
fn restart(cell: &Cell, run: &str) -> Value {
    let (status, run) = call(
        cell,
        "POST",
        "/v1/runs",
        OPERATOR,
        &json!({ "flow": FLOW_ID, "runId": run }).to_string(),
    );
    assert_eq!(status, 200, "{run}");
    run
}

fn melt_status(cell: &Cell) -> u16 {
    call(
        cell,
        "GET",
        "/v1/objects/constitution.Melt/melt-7",
        OPERATOR,
        "",
    )
    .0
}

/// APPROVAL-NO-RIGHT-NO-PARK: a caller who may not commit the call gets the
/// ungated opaque denial, and no decision is created.
#[test]
fn a_caller_without_the_right_parks_nothing() {
    let ungated = cell();
    let (status, denied) = convert(&ungated, MEMBER, &melt7("member-1"));
    assert_eq!(status, 403, "{denied}");
    let gated = cell();
    gate(&gated, None);
    let (status, refused) = convert(&gated, MEMBER, &melt7("member-1"));
    assert_eq!((status, &refused), (403, &denied));
    assert_eq!(pending(&gated, APPROVER)["total"], json!(0));
    assert_eq!(melt_status(&gated), 404);
}

/// APPROVAL-COMMITS-AS-REQUESTER: a gated call parks (202, nothing committed); only a holder of a decider
/// role who is not the requester sees and decides it; approving runs
/// nothing (the decider's request never carries the requester's current
/// authority), and the requester's retry commits it once.
#[test]
fn an_approved_call_commits_on_its_requesters_retry() {
    let cell = cell();
    gate(&cell, None);
    let body = melt7("operator-1");
    let (status, parked) = convert(&cell, OPERATOR, &body);
    assert_eq!(status, 202, "{parked}");
    assert_eq!(parked["outcome"], json!("pending"));
    let id = parked["decision"]["id"].as_str().unwrap().to_owned();
    assert_eq!(melt_status(&cell), 404, "a parked call commits nothing");

    // The same call parks once.
    let (status, again) = convert(&cell, OPERATOR, &body);
    assert_eq!((status, &again), (202, &parked));

    // The requester never decides their own call (#618).
    assert_eq!(pending(&cell, OPERATOR)["total"], json!(0));
    let (status, own) = decide(&cell, OPERATOR, &id, "approve");
    assert_eq!(status, 404, "{own}");
    assert_eq!(pending(&cell, MEMBER)["total"], json!(0));

    let page = pending(&cell, APPROVER);
    assert_eq!(page["total"], json!(1), "{page}");
    let listed = &page["items"][0];
    assert_eq!(listed["id"], json!(id));
    assert_eq!(listed["step"]["id"], json!("approval"));
    assert_eq!(
        listed["call"],
        json!({
            "action": MELT_ACTION,
            "definition": APPROVAL_ID,
            "input": { "id": "melt-7", "input_fine_g": "12", "output_fine_g": "10" },
        })
    );

    let (status, decided) = decide(&cell, APPROVER, &id, "approve");
    assert_eq!(status, 200, "{decided}");
    assert_eq!(decided["call"], not_executed(), "{decided}");
    assert_eq!(
        melt_status(&cell),
        404,
        "the decider's request runs nothing"
    );
    assert_eq!(pending(&cell, APPROVER)["total"], json!(0));

    let (status, committed) = convert(&cell, OPERATOR, &body);
    assert_eq!(status, 200, "{committed}");
    assert_eq!(committed["outcome"], json!("committed"), "{committed}");
    assert_eq!(melt_status(&cell), 200);
    let (status, replayed) = convert(&cell, OPERATOR, &body);
    assert_eq!(status, 200, "{replayed}");
    assert_eq!(replayed["outcome"], json!("replayed"));
    // Every request ends with no caller recorded.
    assert_eq!(cell.caller.current(), None);
}

/// The caller's own approval-gated calls (`GET /v1/decisions:requested`),
/// or with `id` that one decision.
fn requested<E: SqlEngine>(cell: &Cell<E>, auth: Option<&str>, id: Option<&str>) -> Value {
    let query = match id {
        Some(id) => format!("{SCOPE}&id={id}"),
        None => SCOPE.to_owned(),
    };
    let (status, page) = call_with(cell, "GET", "/v1/decisions:requested", &query, auth, "");
    assert_eq!(status, 200, "{page}");
    page
}

/// APPROVAL-REQUESTED-1: the requester reads their own parked call, exactly
/// as it waits (its input as canonical text and its key), and is offered to
/// run it (`canRun`) once it is approved and not yet executed. Retrying
/// exactly what the document serves commits it; nobody else, the approver
/// included, ever reads it, and a rejected call is never offered.
#[test]
fn the_requester_reads_and_runs_their_approved_call() {
    let cell = cell();
    gate(&cell, None);
    let (status, parked) = convert(&cell, OPERATOR, &melt7("operator-r1"));
    assert_eq!(status, 202, "{parked}");
    let id = parked["decision"]["id"].as_str().unwrap().to_owned();

    let page = requested(&cell, OPERATOR, None);
    assert_eq!(page["items"].as_array().unwrap().len(), 1, "{page}");
    let waiting = &page["items"][0];
    assert_eq!(waiting["id"], json!(id));
    assert_eq!(waiting["status"], json!("pending"));
    assert_eq!(waiting["canRun"], json!(false));
    assert_eq!(waiting["question"], json!("Convert this melt?"));
    assert_eq!(waiting["expiresAtMs"], parked["decision"]["expiresAtMs"]);
    assert_eq!(waiting["call"]["action"], json!(MELT_ACTION));
    assert_eq!(waiting["call"]["idempotencyKey"], json!("operator-r1"));
    assert_eq!(
        waiting["call"]["encodedInput"],
        json!(r#"{"id":"melt-7","input_fine_g":"12","output_fine_g":"10"}"#)
    );
    assert_eq!(waiting["call"].get("state"), None);

    // Never the approver's, nor anyone else's, even by id.
    for other in [APPROVER, MEMBER] {
        assert_eq!(requested(&cell, other, None)["items"], json!([]));
        assert_eq!(requested(&cell, other, Some(&id))["items"], json!([]));
    }

    let (status, decided) = decide(&cell, APPROVER, &id, "approve");
    assert_eq!(status, 200, "{decided}");
    assert_eq!(decided["call"], not_executed(), "{decided}");
    assert_eq!(requested(&cell, APPROVER, Some(&id))["items"], json!([]));

    let page = requested(&cell, OPERATOR, None);
    let approved = &page["items"][0];
    assert_eq!(approved["status"], json!("approved"), "{page}");
    assert_eq!(approved["canRun"], json!(true));
    assert_eq!(approved["call"]["state"], not_executed());
    assert!(approved["resolvedAtMs"].is_u64(), "{approved}");

    // The served call, sent back exactly, is the requester's retry.
    let retry = format!(
        r#"{{"input":{},"idempotencyKey":{}}}"#,
        approved["call"]["encodedInput"].as_str().unwrap(),
        approved["call"]["idempotencyKey"]
    );
    let (status, committed) = convert(&cell, OPERATOR, &retry);
    assert_eq!(status, 200, "{committed}");
    assert_eq!(committed["outcome"], json!("committed"));
    assert_eq!(melt_status(&cell), 200);

    // It no longer waits; by id it reads committed, with nothing to run.
    assert_eq!(requested(&cell, OPERATOR, None)["items"], json!([]));
    let done = requested(&cell, OPERATOR, Some(&id));
    assert_eq!(done["items"][0]["status"], json!("approved"), "{done}");
    assert_eq!(done["items"][0]["canRun"], json!(false));
    assert_eq!(
        done["items"][0]["call"]["state"],
        json!({ "committed": true, "state": "committed" })
    );

    // A rejected call reads rejected and is never offered.
    let (_, parked) = convert(
        &cell,
        OPERATOR,
        &melt_body("melt-8", "12", "10", "operator-r2"),
    );
    let rejected_id = parked["decision"]["id"].as_str().unwrap().to_owned();
    let (status, _) = decide(&cell, APPROVER, &rejected_id, "reject");
    assert_eq!(status, 200);
    assert_eq!(requested(&cell, OPERATOR, None)["items"], json!([]));
    let rejected = requested(&cell, OPERATOR, Some(&rejected_id));
    assert_eq!(
        rejected["items"][0]["status"],
        json!("rejected"),
        "{rejected}"
    );
    assert_eq!(rejected["items"][0]["canRun"], json!(false));
}

/// The active definitions version.
fn definitions_version(cell: &Cell) -> u64 {
    let (status, catalog) = call_with(cell, "GET", "/v1/definitions", SCOPE, OPERATOR, "");
    assert_eq!(status, 200, "{catalog}");
    catalog["version"].as_u64().unwrap()
}

/// Park the melt call pinned to `version` (`?version=`).
fn convert_pinned(cell: &Cell, version: u64, body: &str) -> (u16, Value) {
    let query = format!("{SCOPE}&version={version}");
    let path = format!("/v1/actions/{MELT_ACTION}");
    call_with(cell, "POST", &path, &query, OPERATOR, body)
}

/// APPROVAL-PIN-1: a call pinned to a definitions version commits under its
/// approval only while that version is active: a catalog that changed
/// between parking and the requester's retry refuses it (`defs.stale`) and
/// commits nothing, and
/// the requester's retry under the current catalog parks for a fresh
/// decision instead of reusing that approval.
#[test]
fn an_approval_keeps_the_callers_definitions_pin() {
    let cell = cell();
    gate(&cell, None);
    let pinned = definitions_version(&cell);
    let (status, parked) = convert_pinned(&cell, pinned, &melt7("operator-pin"));
    assert_eq!(status, 202, "{parked}");
    let id = parked["decision"]["id"].as_str().unwrap().to_owned();
    gate_asking(&cell, None, "Convert this melt now?", Some(1));
    assert_ne!(definitions_version(&cell), pinned, "the catalog changed");
    let (status, decided) = decide(&cell, APPROVER, &id, "approve");
    assert_eq!(status, 200, "{decided}");
    assert_eq!(decided["call"], not_executed(), "{decided}");
    let (status, stale) = convert_pinned(&cell, pinned, &melt7("operator-pin"));
    assert_eq!(status, 409, "{stale}");
    assert_eq!(stale["code"], json!("defs.stale"), "{stale}");
    assert_eq!(melt_status(&cell), 404, "nothing committed");

    // The requester's retry under the current catalog is another call: it
    // never finds the earlier approval, and parks for a fresh decision.
    let current = definitions_version(&cell);
    let (status, retried) = convert_pinned(&cell, current, &melt7("operator-pin"));
    assert_eq!(status, 202, "{retried}");
    let fresh = retried["decision"]["id"].as_str().unwrap().to_owned();
    assert_ne!(fresh, id, "a fresh decision");
    let (status, unpinned) = convert(&cell, OPERATOR, &melt7("operator-pin"));
    assert_eq!(status, 202, "{unpinned}");
    assert_ne!(unpinned["decision"]["id"], json!(id), "{unpinned}");
    assert_eq!(
        melt_status(&cell),
        404,
        "no commit without a fresh decision"
    );
    // The original pin stays stale.
    let (status, stale) = convert_pinned(&cell, pinned, &melt7("operator-pin"));
    assert_ne!(status, 200, "{stale}");
    assert_eq!(stale["code"], json!("defs.stale"), "{stale}");
    assert_eq!(melt_status(&cell), 404);
    // The fresh decision approved, the retry commits.
    let (status, decided) = decide(&cell, APPROVER, &fresh, "approve");
    assert_eq!(status, 200, "{decided}");
    assert_eq!(decided["call"], not_executed(), "{decided}");
    let (status, committed) = convert_pinned(&cell, current, &melt7("operator-pin"));
    assert_eq!(status, 200, "{committed}");
    assert_eq!(melt_status(&cell), 200);

    // Unchanged between parking and approval, the pinned call commits.
    let cell = step_cell::cell();
    gate(&cell, None);
    let pinned = definitions_version(&cell);
    let (status, parked) = convert_pinned(&cell, pinned, &melt7("operator-pin"));
    assert_eq!(status, 202, "{parked}");
    let id = parked["decision"]["id"].as_str().unwrap().to_owned();
    let (status, decided) = decide(&cell, APPROVER, &id, "approve");
    assert_eq!(status, 200, "{decided}");
    assert_eq!(decided["call"], not_executed(), "{decided}");
    let (status, committed) = convert_pinned(&cell, pinned, &melt7("operator-pin"));
    assert_eq!(status, 200, "{committed}");
    assert_eq!(melt_status(&cell), 200);
}

/// APPROVAL-BINDS-INPUT: an approval approves exactly the call it was asked about: the same key
/// with another input is another call, and parks on its own.
#[test]
fn an_approval_binds_the_input() {
    let cell = cell();
    gate(&cell, None);
    let (_, first) = convert(&cell, OPERATOR, &melt7("operator-2"));
    let other = melt_body("melt-7", "12", "11", "operator-2");
    let (status, second) = convert(&cell, OPERATOR, &other);
    assert_eq!(status, 202, "{second}");
    assert_ne!(first["decision"]["id"], second["decision"]["id"]);
    assert_eq!(pending(&cell, APPROVER)["total"], json!(2));
}

/// APPROVAL-REJECTED: a rejected call never commits; its retry is refused `approval.rejected`.
#[test]
fn a_rejected_call_never_commits() {
    let cell = cell();
    gate(&cell, None);
    let body = melt7("operator-3");
    let (_, parked) = convert(&cell, OPERATOR, &body);
    let id = parked["decision"]["id"].as_str().unwrap();
    let (status, decided) = decide(&cell, APPROVER, id, "reject");
    assert_eq!(status, 200, "{decided}");
    assert_eq!(decided.get("call"), None, "{decided}");
    let (status, refused) = convert(&cell, OPERATOR, &body);
    assert_eq!(status, 409, "{refused}");
    assert_eq!(refused["code"], json!("approval.rejected"));
    assert_eq!(melt_status(&cell), 404);
}

/// APPROVAL-EXPIRED: an approval no one decides in time expires; the call is refused
/// `approval.expired` and never commits.
#[test]
fn an_undecided_approval_expires() {
    let cell = cell();
    gate(&cell, Some(60_000));
    let body = melt7("operator-4");
    let (status, _) = convert(&cell, OPERATOR, &body);
    assert_eq!(status, 202);
    let path = format!("/v1/actions/{MELT_ACTION}");
    let late = NOW + 61;
    let (status, refused) = call_at(&cell, ("POST", &path, SCOPE), OPERATOR, &body, late);
    assert_eq!(status, 410, "{refused}");
    assert_eq!(refused["code"], json!("approval.expired"));
    let (status, again) = call_at(&cell, ("POST", &path, SCOPE), OPERATOR, &body, late);
    assert_eq!((status, &again["code"]), (410, &json!("approval.expired")));
    assert_eq!(melt_status(&cell), 404);
}

/// Start the melt flow as the operator and approve its ask: its convert step
/// calls the gated action as the operator, and parks.
fn start_gated_run(cell: &Cell, run: &str) -> String {
    let (status, opened) = call(
        cell,
        "POST",
        "/v1/runs",
        OPERATOR,
        &json!({ "flow": FLOW_ID, "runId": run }).to_string(),
    );
    assert_eq!(status, 201, "{opened}");
    let page = pending(cell, APPROVER);
    let ask = page["items"][0]["id"].as_str().unwrap().to_owned();
    let (status, decided) = decide(cell, APPROVER, &ask, "approve");
    assert_eq!(status, 200, "{decided}");
    assert_eq!(decided["run"]["state"], json!("awaiting_perform_convert"));
    assert_eq!(melt_status(cell), 404, "the step's call parked");
    let page = pending(cell, APPROVER);
    assert_eq!(page["total"], json!(1), "{page}");
    let approval = &page["items"][0];
    assert_eq!(approval["call"]["action"], json!(MELT_ACTION));
    assert_eq!(approval["run"]["id"], json!(run), "{approval}");
    approval["id"].as_str().unwrap().to_owned()
}

/// APPROVAL-STEP-PARKS: a flow step's call parks exactly like a direct call;
/// once it is approved, the starter's own retry of the run commits it under
/// the starter's current authority and the run completes.
#[test]
fn a_flow_step_parks_like_a_direct_call() {
    let cell = cell();
    gate(&cell, None);
    let approval = start_gated_run(&cell, "melt-run-a");
    let (status, decided) = decide(&cell, APPROVER, &approval, "approve");
    assert_eq!(status, 200, "{decided}");
    assert_eq!(decided["run"]["id"], json!("melt-run-a"));
    assert_eq!(decided["run"]["state"], json!("awaiting_perform_convert"));
    assert_eq!(decided["call"], not_executed(), "{decided}");
    assert_eq!(melt_status(&cell), 404);
    let (status, waiting) = call(&cell, "GET", "/v1/runs/melt-run-a", OPERATOR, "");
    let pinned: Value = serde_json::from_str(include_str!(
        "../../../apps/studio/test/fixtures/run-executor.json"
    ))
    .unwrap();
    assert_eq!(json!({ "status": status, "json": waiting }), pinned);
    assert_eq!(restart(&cell, "melt-run-a")["state"], json!("completed"));
    assert_eq!(melt_status(&cell), 200);
}

/// APPROVAL-STEP-REJECTED: a rejected step call fails the step with the typed refusal, and the run
/// ends without converting.
#[test]
fn a_rejected_step_call_fails_the_run() {
    let cell = cell();
    gate(&cell, None);
    let approval = start_gated_run(&cell, "melt-run-b");
    let (status, decided) = decide(&cell, APPROVER, &approval, "reject");
    assert_eq!(status, 200, "{decided}");
    let (_, run) = call(&cell, "GET", "/v1/runs/melt-run-b", OPERATOR, "");
    assert_eq!(run["terminal"], json!(true), "{run}");
    assert_eq!(
        run["steps"][1]["error"]["code"],
        json!("failed_refused"),
        "{run}"
    );
    assert_eq!(melt_status(&cell), 404);
}

/// APPROVAL-MCP-PARKS: a gated action's MCP tool parks exactly like the
/// HTTP call pinned to the catalog version the tool came from. MCP's loopback
/// goes through the same dispatch, so the tool answers the same pending
/// decision, not an error, and commits nothing.
#[test]
fn an_mcp_tool_call_parks_like_http() {
    let cell = cell();
    gate(&cell, None);
    let rpc = |message: &Value| {
        let (status, reply) = call(&cell, "POST", "/v1/mcp", OPERATOR, &message.to_string());
        assert_eq!(status, 200, "{reply}");
        reply
    };
    let tools =
        rpc(&json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {"limit": 100}}));
    let name = tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["_meta"]["norn/actionId"] == MELT_ACTION)
        .and_then(|tool| tool["name"].as_str())
        .expect("the operator may call the melt action")
        .to_owned();
    let key = "operator-mcp";
    let called = rpc(
        &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
            "name": name, "arguments": {"id": "melt-7", "input_fine_g": "12", "output_fine_g": "10"},
            "_meta": {"norn/idempotencyKey": key}
        }}),
    );
    assert_eq!(called["result"]["isError"], false, "{called}");
    let parked = called["result"]["structuredContent"].clone();
    assert_eq!(parked["outcome"], json!("pending"), "{called}");
    assert_eq!(melt_status(&cell), 404);
    // MCP calls an action pinned to the catalog it listed (APPROVAL-PIN-1):
    // the same call over HTTP pinned to that version is the same parked call.
    let version = definitions_version(&cell);
    let (status, direct) = convert_pinned(&cell, version, &melt7(key));
    assert_eq!((status, &direct), (202, &parked));
    assert_eq!(pending(&cell, APPROVER)["total"], json!(1));
}

/// Install the fixture's `set` as the cell's next definitions revision.
fn revise(cell: &Cell, set: &str) {
    // After every commit so far: the cell's clock never runs backwards.
    revise_at(cell, set, NOW * 1000 + 1);
}

/// [`revise`], installed at logical time `at`.
fn revise_at(cell: &Cell, set: &str, at: u64) {
    let version = cell.host.active_definitions_version().unwrap();
    let (model, input) = step_cell::action_cell::melt_install(
        set,
        norn_api::action_admission::prepare_action_model,
        at,
    );
    cell.host
        .install_definitions(model, input, version)
        .expect("the next revision installs");
    assert_ne!(cell.host.active_definitions_version().unwrap(), version);
}

/// APPROVAL-REVISION-1: an approval given for a call parked under revision N
/// never executes it under N+1 when N+1 changed the action: the
/// requester's call under the new action is another call, and parks anew
/// for a fresh approval.
#[test]
fn an_approval_never_executes_a_changed_action() {
    let cell = cell();
    gate(&cell, None);
    let body = melt7("operator-rev");
    let (status, parked) = convert(&cell, OPERATOR, &body);
    assert_eq!(status, 202, "{parked}");
    let id = parked["decision"]["id"].as_str().unwrap().to_owned();

    // N+1: the action's writer no longer derives the loss.
    revise(&cell, step_cell::action_cell::OMITS_LOSS);
    let (status, decided) = decide(&cell, APPROVER, &id, "approve");
    assert_eq!(status, 200, "{decided}");
    assert_eq!(decided["call"], not_executed(), "{decided}");
    assert_eq!(melt_status(&cell), 404, "nothing ran under the new action");

    // The same call under the new action asks for a new approval.
    let (status, again) = convert(&cell, OPERATOR, &body);
    assert_eq!(status, 202, "{again}");
    assert_ne!(again["decision"]["id"], json!(id), "{again}");
    assert_eq!(melt_status(&cell), 404);
}

/// APPROVAL-REVISION-1: a later revision that leaves the action as it was
/// keeps the approval: the approved call executes.
#[test]
fn an_approval_survives_a_revision_that_keeps_the_action() {
    let cell = cell();
    gate(&cell, None);
    let (status, parked) = convert(&cell, OPERATOR, &melt7("operator-same"));
    assert_eq!(status, 202, "{parked}");
    let id = parked["decision"]["id"].as_str().unwrap().to_owned();
    revise(&cell, step_cell::DERIVES_LOSS);
    let (status, decided) = decide(&cell, APPROVER, &id, "approve");
    assert_eq!(status, 200, "{decided}");
    assert_eq!(decided["call"], not_executed(), "{decided}");
    let (status, committed) = convert(&cell, OPERATOR, &melt7("operator-same"));
    assert_eq!(status, 200, "{committed}");
    assert_eq!(melt_status(&cell), 200);
}

/// APPROVAL-REVISION-1: the digest an approval binds covers what the action
/// writes. A revision that changes only a default of the type the action
/// writes (the facts a call commits change) makes the approval stale.
#[test]
fn an_approval_never_executes_under_a_changed_default() {
    let cell = cell();
    gate(&cell, None);
    let (status, parked) = convert(&cell, OPERATOR, &melt7("operator-default"));
    assert_eq!(status, 202, "{parked}");
    let id = parked["decision"]["id"].as_str().unwrap().to_owned();

    let version = cell.host.active_definitions_version().unwrap();
    let (model, input) = step_cell::action_cell::melt_install_edited(
        step_cell::DERIVES_LOSS,
        |documents| {
            let melt = documents
                .iter_mut()
                .find(|document| document["artifactId"] == "constitution.Melt.entity")
                .unwrap();
            let properties = melt["payload"]["objectType"]["properties"]
                .as_array_mut()
                .unwrap();
            let loss = properties
                .iter_mut()
                .find(|property| property["id"] == "loss_fine_g")
                .unwrap();
            loss["default"] = json!("1");
        },
        norn_api::action_admission::prepare_action_model,
        NOW * 1000 + 1,
    );
    cell.host
        .install_definitions(model, input, version)
        .expect("the default-only revision installs");

    let (status, decided) = decide(&cell, APPROVER, &id, "approve");
    assert_eq!(status, 200, "{decided}");
    assert_eq!(decided["call"], not_executed(), "{decided}");
    let (status, again) = convert(&cell, OPERATOR, &melt7("operator-default"));
    assert_eq!(status, 202, "{again}");
    assert_ne!(again["decision"]["id"], json!(id), "{again}");
    assert_eq!(melt_status(&cell), 404);
}

/// APPROVAL-REVISION-1: an approval is bound to the gate revision whose
/// decider roles it was asked under: a new gate revision (here, more
/// decider roles) makes the approval stale, and the call asks anew.
#[test]
fn an_approval_never_executes_under_a_changed_gate() {
    let cell = cell();
    gate(&cell, None);
    let body = melt7("operator-gate");
    let (status, parked) = convert(&cell, OPERATOR, &body);
    assert_eq!(status, 202, "{parked}");
    let id = parked["decision"]["id"].as_str().unwrap().to_owned();

    let mut revised = json!({ "kind": "approval", "id": APPROVAL_ID,
        "payload": { "action": MELT_ACTION, "decider": { "roles": ["admin", "member"] },
                     "question": "Convert this melt?", "risk": "high" } });
    let (status, admitted) = call(
        &cell,
        "POST",
        "/v1/definitions:admit",
        OPERATOR,
        &revised.to_string(),
    );
    assert_eq!(status, 200, "{admitted}");
    revised["signoff"] = json!({ "restatementSha256": admitted["restatementSha256"] });
    revised["expectedVersion"] = json!(1);
    let (status, installed) = call(
        &cell,
        "POST",
        "/v1/definitions:install",
        OPERATOR,
        &revised.to_string(),
    );
    assert_eq!(status, 200, "{installed}");

    let (status, decided) = decide(&cell, APPROVER, &id, "approve");
    assert_eq!(status, 200, "{decided}");
    assert_eq!(decided["call"], not_executed(), "{decided}");
    assert_eq!(melt_status(&cell), 404);
    let (status, again) = convert(&cell, OPERATOR, &body);
    assert_eq!(status, 202, "{again}");
    assert_ne!(again["decision"]["id"], json!(id), "{again}");
}

/// APPROVAL-REVISION-1 on the hosted path: an approved step call whose
/// approval gate changed drives nothing on the decider's request. The run's
/// waiting step is not called again under the changed gate (no new approval
/// parks), and the run stays where it was; the starter's retry calls the
/// unchanged pinned action as another call, which asks for a new approval.
#[test]
fn a_stale_approval_never_drives_its_flow_step() {
    let cell = cell();
    gate(&cell, None);
    let approval = start_gated_run(&cell, "melt-run-stale");
    // Revise the approval gate while keeping the run's pinned action unchanged.
    let mut revised = json!({ "kind": "approval", "id": APPROVAL_ID,
        "payload": { "action": MELT_ACTION, "decider": { "roles": ["admin", "member"] },
                     "question": "Convert this melt?", "risk": "high" } });
    let (status, admitted) = call(
        &cell,
        "POST",
        "/v1/definitions:admit",
        OPERATOR,
        &revised.to_string(),
    );
    assert_eq!(status, 200, "{admitted}");
    revised["signoff"] = json!({ "restatementSha256": admitted["restatementSha256"] });
    revised["expectedVersion"] = json!(1);
    let (status, installed) = call(
        &cell,
        "POST",
        "/v1/definitions:install",
        OPERATOR,
        &revised.to_string(),
    );
    assert_eq!(status, 200, "{installed}");

    let (status, decided) = decide(&cell, APPROVER, &approval, "approve");
    assert_eq!(status, 200, "{decided}");
    assert_eq!(decided["call"], not_executed(), "{decided}");
    assert_eq!(
        decided["run"]["state"],
        json!("awaiting_perform_convert"),
        "{decided}"
    );
    assert_eq!(
        pending(&cell, APPROVER)["total"],
        json!(0),
        "nothing parked anew"
    );
    assert_eq!(melt_status(&cell), 404);
    let run = restart(&cell, "melt-run-stale");
    assert_eq!(run["state"], json!("awaiting_perform_convert"), "{run}");
    assert_eq!(
        pending(&cell, APPROVER)["total"],
        json!(1),
        "a new approval"
    );
    assert_eq!(melt_status(&cell), 404);
}

/// APPROVAL-REVISION-1 + STEP-PIN-1 on the hosted path: when the action
/// definition itself changes (`OMITS_LOSS`) after a flow step parks for
/// approval, the stale approval drives nothing on the decider's request, and
/// the starter's retry fails closed (`failed_refused`) rather than parking or
/// executing under the mutated action.
#[test]
fn a_stale_approval_after_action_change_fails_closed_on_retry() {
    let cell = cell();
    gate(&cell, None);
    let approval = start_gated_run(&cell, "melt-run-stale-action");
    let fixture: Value = serde_json::from_str(include_str!(
        "../../norn-cell/tests/fixtures/constitution.json"
    ))
    .unwrap();
    let without: Vec<norn_ir::ArtifactEnvelope> =
        serde_json::from_value(fixture[step_cell::action_cell::OMITS_LOSS].clone()).unwrap();
    cell.host
        .install_artifacts(&without, NOW * 1000 + 10)
        .expect("the action's next revision installs");
    let (status, decided) = decide(&cell, APPROVER, &approval, "approve");
    assert_eq!(status, 200, "{decided}");
    assert_eq!(decided["call"], not_executed(), "{decided}");
    assert_eq!(
        decided["run"]["state"],
        json!("awaiting_perform_convert"),
        "{decided}"
    );
    assert_eq!(
        pending(&cell, APPROVER)["total"],
        json!(0),
        "nothing parked anew"
    );
    assert_eq!(melt_status(&cell), 404);
    let run = restart(&cell, "melt-run-stale-action");
    assert_eq!(run["state"], json!("failed_refused"), "{run}");
    assert_eq!(
        pending(&cell, APPROVER)["total"],
        json!(0),
        "mutated action must never park a fresh approval for the old run"
    );
    assert_eq!(melt_status(&cell), 404);
}

/// APPROVAL-REVISION-1 (round 2 S6): a replayed approval is fenced like the
/// first dispatch. The gate is retired after the step's call parked; neither
/// the decide nor its identical replay runs the call or drives the waiting
/// step, so the action never executes under that approval.
#[test]
fn a_replayed_approval_never_drives_a_step_past_a_retired_gate() {
    let cell = cell();
    gate(&cell, None);
    let approval = start_gated_run(&cell, "melt-run-retired");
    let (status, retired) = call(
        &cell,
        "POST",
        &format!("/v1/definitions/{APPROVAL_ID}:retire"),
        OPERATOR,
        &json!({ "expectedVersion": 1 }).to_string(),
    );
    assert_eq!(status, 200, "{retired}");
    let stale = not_executed();
    let (status, decided) = decide(&cell, APPROVER, &approval, "approve");
    assert_eq!(status, 200, "{decided}");
    assert_eq!(decided["call"], stale, "{decided}");
    // The same key and body: a replay.
    let (status, replayed) = decide(&cell, APPROVER, &approval, "approve");
    assert_eq!(status, 200, "{replayed}");
    assert_eq!(replayed["outcome"], json!("replayed"), "{replayed}");
    assert_eq!(replayed["call"], stale, "{replayed}");
    assert_eq!(
        replayed["run"]["state"],
        json!("awaiting_perform_convert"),
        "{replayed}"
    );
    assert_eq!(melt_status(&cell), 404, "the action never ran ungated");
}

/// A replay of an approval whose call already committed drives nothing new
/// and answers the committed call again.
#[test]
fn a_replayed_approval_of_a_committed_step_replays() {
    let cell = cell();
    gate(&cell, None);
    let approval = start_gated_run(&cell, "melt-run-replay");
    let (status, decided) = decide(&cell, APPROVER, &approval, "approve");
    assert_eq!(status, 200, "{decided}");
    assert_eq!(
        restart(&cell, "melt-run-replay")["state"],
        json!("completed")
    );
    let (status, replayed) = decide(&cell, APPROVER, &approval, "approve");
    assert_eq!(status, 200, "{replayed}");
    assert_eq!(replayed["outcome"], json!("replayed"), "{replayed}");
    assert_eq!(replayed["run"]["state"], json!("completed"), "{replayed}");
    assert_eq!(
        replayed["call"],
        json!({ "committed": true, "state": "committed" }),
        "{replayed}"
    );
    assert_eq!(melt_status(&cell), 200);
}

/// APPROVAL-REVISION-1, the race: a revision installed after the approval is
/// recorded and before its call is dispatched (natively, between the two)
/// is caught by the dispatch's own fence, inside its definitions epoch and
/// before anything parks or writes: the call is refused, nothing commits,
/// and no new approval parks.
#[test]
fn a_revision_racing_the_approved_dispatch_is_fenced() {
    use norn_cell_do::policy::Caller;
    let cell = cell();
    gate(&cell, None);
    let (status, parked) = convert(&cell, OPERATOR, &melt7("operator-race"));
    assert_eq!(status, 202, "{parked}");
    let id = parked["decision"]["id"].as_str().unwrap().to_owned();

    let as_caller = |subject: &str, roles: &[String], table: &str| Caller {
        subject_id: subject.to_owned(),
        roles: roles.to_vec(),
        actor_kind: "user",
        role_table: Box::leak(table.to_owned().into_boxed_str()),
        accountable_person: Some(subject.to_owned()),
        attributes: std::collections::BTreeMap::new(),
    };
    cell.caller.record(Some(as_caller(
        "approver@steps",
        &["admin".to_owned()],
        norn_cell_do::principal::FIXTURE_ROLES,
    )));
    let scope = norn_api::scope::ScopeQuery {
        org_id: Some("org_demo".into()),
        environment_id: Some("dev".into()),
    };
    let body = norn_api::decisions::DecideRequest {
        choice: "approve".into(),
        note: None,
        idempotency_key: "race-1".into(),
    };
    let reads = |capability: norn_api::authz::ReadCapability, object_type: &str| {
        norn_cell_do::policy::collection_access(
            cell.host.scope(),
            "approver@steps",
            capability,
            object_type,
        )
    };
    let raced = std::cell::Cell::new(false);
    let as_requester = |requester: &norn_cell::flow_run::RunStarter,
                        dispatch: &mut dyn FnMut() -> Result<
        norn_api::actions::ActionAnswer,
        norn_api::refusal::Refusal,
    >| {
        // The concurrent revision lands here: after the approval is
        // recorded, before its call is dispatched.
        revise_at(&cell, step_cell::action_cell::OMITS_LOSS, NOW * 1000 + 10);
        raced.set(true);
        cell.caller.record(Some(as_caller(
            &requester.subject,
            &requester.roles,
            &requester.role_table,
        )));
        Some(dispatch())
    };
    let decided = norn_api::decisions::decide(
        &cell.host,
        &norn_api::decisions::DecideCall {
            scope: &scope,
            actor: Some("approver@steps"),
            id: &id,
            body: &body,
            now: norn_kernel::LogicalTime::from_millis(NOW * 1000 + 2),
        },
        &reads,
        &cell.handles,
        &as_requester,
    )
    .unwrap();
    cell.caller.record(None);
    assert!(raced.get());
    let dispatched = decided.call.expect("the approved call was dispatched");
    assert_eq!(
        (dispatched.committed, dispatched.code.as_deref()),
        (false, Some("approval.definition_changed"))
    );
    assert_eq!(melt_status(&cell), 404, "nothing ran under the new action");
    assert_eq!(
        pending(&cell, APPROVER)["total"],
        json!(0),
        "nothing parked anew"
    );
}

/// APPROVAL-RECOVERY-1 (S6), the interleaving: a dispatch of an approved
/// call reads it not executed and releases the cell; the requester's retry
/// commits exactly that call; a revision lands; the dispatch then reaches
/// its fence and is refused. Under its own hold it reads the call again
/// before recording anything, so the refusal is never recorded over the
/// commit: the call stands committed, for the approver and the requester.
#[test]
fn a_refusal_racing_an_exact_commit_never_masks_it() {
    use norn_cell_do::policy::Caller;
    let cell = cell();
    gate(&cell, None);
    let body = melt7("operator-mask");
    let (status, parked) = convert(&cell, OPERATOR, &body);
    assert_eq!(status, 202, "{parked}");
    let id = parked["decision"]["id"].as_str().unwrap().to_owned();

    let as_caller = |subject: &str, roles: &[String], table: &str| Caller {
        subject_id: subject.to_owned(),
        roles: roles.to_vec(),
        actor_kind: "user",
        role_table: Box::leak(table.to_owned().into_boxed_str()),
        accountable_person: Some(subject.to_owned()),
        attributes: std::collections::BTreeMap::new(),
    };
    cell.caller.record(Some(as_caller(
        "approver@steps",
        &["admin".to_owned()],
        norn_cell_do::principal::FIXTURE_ROLES,
    )));
    let scope = norn_api::scope::ScopeQuery {
        org_id: Some("org_demo".into()),
        environment_id: Some("dev".into()),
    };
    let request = norn_api::decisions::DecideRequest {
        choice: "approve".into(),
        note: None,
        idempotency_key: "mask-1".into(),
    };
    let reads = |capability: norn_api::authz::ReadCapability, object_type: &str| {
        norn_cell_do::policy::collection_access(
            cell.host.scope(),
            "approver@steps",
            capability,
            object_type,
        )
    };
    let raced = std::cell::Cell::new(None);
    let as_requester = |requester: &norn_cell::flow_run::RunStarter,
                        dispatch: &mut dyn FnMut() -> Result<
        norn_api::actions::ActionAnswer,
        norn_api::refusal::Refusal,
    >| {
        // Between the dispatch's read and its own hold: the requester's
        // retry commits the exact call, and a revision changes the action.
        let (status, committed) = convert(&cell, OPERATOR, &body);
        assert_eq!(status, 200, "{committed}");
        revise_at(&cell, step_cell::action_cell::OMITS_LOSS, NOW * 1000 + 10);
        raced.set(Some(cell.host.verified_head().unwrap().commits));
        cell.caller.record(Some(as_caller(
            &requester.subject,
            &requester.roles,
            &requester.role_table,
        )));
        Some(dispatch())
    };
    let decided = norn_api::decisions::decide(
        &cell.host,
        &norn_api::decisions::DecideCall {
            scope: &scope,
            actor: Some("approver@steps"),
            id: &id,
            body: &request,
            now: norn_kernel::LogicalTime::from_millis(NOW * 1000 + 2),
        },
        &reads,
        &cell.handles,
        &as_requester,
    )
    .unwrap();
    cell.caller.record(None);
    let before = raced.get().expect("the race ran");
    assert_eq!(
        cell.host.verified_head().unwrap().commits,
        before,
        "no refusal was recorded over the commit"
    );
    let standing = decided.call.expect("the approved call's state");
    assert_eq!(
        (standing.committed, standing.state),
        (true, norn_api::decisions::DecidedCallState::Committed)
    );
    assert_eq!(melt_status(&cell), 200);
    let (status, replayed) = convert(&cell, OPERATOR, &body);
    assert_eq!(status, 200, "{replayed}");
    assert_eq!(replayed["outcome"], json!("replayed"), "{replayed}");
}

/// `GET /v1/decisions:requested` as the host's verified principal
/// `caller`, called directly so the principal's actor kind and role table
/// are the test's to choose.
fn requested_as(cell: &Cell, caller: norn_cell_do::policy::Caller, id: Option<&str>) -> Value {
    let subject = caller.subject_id.clone();
    cell.caller.record(Some(caller));
    let page = norn_api::decisions::requested_decisions(
        &cell.host,
        &norn_api::decisions::RequestedQuery {
            org_id: Some("org_demo".into()),
            environment_id: Some("dev".into()),
            id: id.map(str::to_owned),
        },
        Some(&subject),
        norn_kernel::LogicalTime::from_millis(NOW * 1000),
    );
    cell.caller.record(None);
    serde_json::to_value(page.unwrap()).unwrap()
}

/// #1532 S1: the requester is the principal the dispatch admits: subject,
/// actor kind and role table. The same subject under another actor kind, or
/// another role table, is someone else: it reads neither the call's input
/// nor its key, and is offered nothing to run, by list or by id.
#[test]
fn the_requesters_subject_under_another_kind_or_role_table_reads_nothing() {
    use norn_cell_do::policy::Caller;
    let cell = cell();
    gate(&cell, None);
    let (status, parked) = convert(&cell, OPERATOR, &melt7("operator-kind"));
    assert_eq!(status, 202, "{parked}");
    let id = parked["decision"]["id"].as_str().unwrap().to_owned();
    let (status, decided) = decide(&cell, APPROVER, &id, "approve");
    assert_eq!(status, 200, "{decided}");
    let as_operator = |kind: &'static str, table: &'static str| Caller {
        subject_id: "operator@steps".to_owned(),
        roles: vec!["admin".to_owned()],
        actor_kind: kind,
        role_table: table,
        accountable_person: Some("operator@steps".to_owned()),
        attributes: std::collections::BTreeMap::new(),
    };
    // The requester itself, as the dispatch admits it, is offered the run.
    let own = requested_as(
        &cell,
        as_operator("user", norn_cell_do::principal::FIXTURE_ROLES),
        Some(&id),
    );
    assert_eq!(own["items"][0]["canRun"], json!(true), "{own}");
    for other in [
        as_operator("service", norn_cell_do::principal::FIXTURE_ROLES),
        as_operator("user", "fixtures/other"),
    ] {
        for wanted in [None, Some(id.as_str())] {
            let page = requested_as(&cell, other.clone(), wanted);
            assert_eq!(page["items"], json!([]), "{page}");
            assert!(!page.to_string().contains("operator-kind"), "{page}");
        }
    }
}

/// The native engine, counting the statements a request runs and those
/// that load a parked call's payload (an outbox row's `command`) or a
/// receipt.
struct Counted {
    inner: norn_cell::engine::NativeEngine,
    counts: std::rc::Rc<std::cell::Cell<(u64, u64)>>,
}

impl Counted {
    fn tick(&self, sql: &str) {
        let (all, payloads) = self.counts.get();
        let select = sql.split_once(" FROM ").map_or("", |(select, _)| select);
        let payload = (sql.contains(r#"FROM "_norn_outbox""#) && select.contains(r#""command""#))
            || sql.contains(r#"FROM "_norn_receipt""#);
        self.counts.set((
            all.saturating_add(1),
            payloads.saturating_add(u64::from(payload)),
        ));
    }
}

impl SqlEngine for Counted {
    fn execute(
        &self,
        sql: &str,
        params: &[norn_cell::engine::SqlValue],
    ) -> Result<(), norn_cell::CellError> {
        self.tick(sql);
        self.inner.execute(sql, params)
    }
    fn query(
        &self,
        sql: &str,
        params: &[norn_cell::engine::SqlValue],
    ) -> Result<Vec<Vec<norn_cell::engine::SqlValue>>, norn_cell::CellError> {
        self.tick(sql);
        self.inner.query(sql, params)
    }
    fn execute_batch(&self, sql: &str) -> Result<(), norn_cell::CellError> {
        self.tick(sql);
        self.inner.execute_batch(sql)
    }
    fn transaction<T, X: From<norn_cell::CellError>>(
        &self,
        f: impl FnOnce() -> Result<T, X>,
    ) -> Result<T, X> {
        self.inner.transaction(f)
    }
    fn platform_table_globs(&self) -> &'static [&'static str] {
        self.inner.platform_table_globs()
    }
    fn read_isolation(&self) -> norn_cell::engine::ReadIsolation {
        self.inner.read_isolation()
    }
    fn read_cache_revision(&self) -> Result<Option<(u64, u64)>, norn_cell::CellError> {
        self.inner.read_cache_revision()
    }
    fn admitted(&self) -> Result<(), norn_cell::CellError> {
        self.inner.admitted()
    }
    fn definition_writes<T>(
        &self,
        f: impl FnOnce() -> Result<T, norn_cell::CellError>,
    ) -> Result<T, norn_cell::CellError> {
        self.inner.definition_writes(f)
    }
}

/// #1532 S2: another requester's decision id is looked up by the caller's
/// own identity first: it loads no parked payload and no receipt, and costs
/// exactly what an id that exists nowhere costs, while it waits and after
/// it is approved. The requester's own id loads its call (the control).
#[test]
fn another_requesters_id_loads_no_payload() {
    let counts = std::rc::Rc::new(std::cell::Cell::new((0, 0)));
    let engine = Counted {
        inner: norn_cell::engine::NativeEngine::open_in_memory().unwrap(),
        counts: std::rc::Rc::clone(&counts),
    };
    let cell = step_cell::cell_on(engine, true);
    gate(&cell, None);
    let (status, parked) = convert(&cell, OPERATOR, &melt7("operator-cost"));
    assert_eq!(status, 202, "{parked}");
    let id = parked["decision"]["id"].as_str().unwrap().to_owned();
    let nowhere = format!("dec_{}", "0".repeat(32));
    let cost = |auth: Option<&str>, wanted: &str| {
        // Warm: a first read may compile what any later read reuses.
        requested(&cell, auth, Some(wanted));
        counts.set((0, 0));
        let page = requested(&cell, auth, Some(wanted));
        (page["items"].as_array().unwrap().len(), counts.get())
    };
    for phase in ["pending", "approved"] {
        let (items, foreign) = cost(APPROVER, &id);
        let (_, absent) = cost(APPROVER, &nowhere);
        assert_eq!(items, 0, "{phase}");
        assert_eq!(foreign.1, 0, "{phase}: a foreign id loaded a payload");
        assert_eq!(
            foreign, absent,
            "{phase}: a foreign id costs what no id costs"
        );
        let (items, own) = cost(OPERATOR, &id);
        assert_eq!(items, 1, "{phase}");
        assert!(own.1 > 0, "{phase}: the control loads the call");
        if phase == "pending" {
            let (status, decided) = decide(&cell, APPROVER, &id, "approve");
            assert_eq!(status, 200, "{decided}");
        }
    }
}

/// #1532 S4: the list holds only calls that wait. A call whose deadline
/// passed is not listed, whether or not its expiry is recorded yet (by id
/// it reads expired, with nothing to run).
#[test]
fn a_call_past_its_deadline_is_not_listed() {
    let cell = cell();
    gate(&cell, Some(60_000));
    let (status, parked) = convert(&cell, OPERATOR, &melt7("operator-late"));
    assert_eq!(status, 202, "{parked}");
    let id = parked["decision"]["id"].as_str().unwrap().to_owned();
    let read = |query: String, at: u64| {
        let (status, page) = call_at(
            &cell,
            ("GET", "/v1/decisions:requested", &query),
            OPERATOR,
            "",
            at,
        );
        assert_eq!(status, 200, "{page}");
        page
    };
    assert_eq!(
        read(SCOPE.to_owned(), NOW)["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let late = NOW + 120;
    assert_eq!(read(SCOPE.to_owned(), late)["items"], json!([]));
    let by_id = read(format!("{SCOPE}&id={id}"), late);
    assert_eq!(by_id["items"][0]["status"], json!("expired"), "{by_id}");
    assert_eq!(by_id["items"][0]["canRun"], json!(false));
}

/// #1532 S4: the list's limit counts only the caller's own calls. Another
/// requester's approved calls, as many as the limit and approved first,
/// neither hide the caller's approved call nor mark the page capped.
#[test]
fn other_requesters_approved_calls_never_hide_yours() {
    let cell = cell();
    gate(&cell, None);
    for n in 0..norn_api::decisions::MAX_DECISION_SCAN {
        let body = melt_body(&format!("melt-o{n}"), "12", "10", &format!("approver-{n}"));
        let (status, parked) = convert(&cell, APPROVER, &body);
        assert_eq!(status, 202, "{parked}");
        let other = parked["decision"]["id"].as_str().unwrap().to_owned();
        let (status, decided) = decide(&cell, OPERATOR, &other, "approve");
        assert_eq!(status, 200, "{decided}");
    }
    let (status, parked) = convert(&cell, OPERATOR, &melt7("operator-mine"));
    assert_eq!(status, 202, "{parked}");
    let id = parked["decision"]["id"].as_str().unwrap().to_owned();
    let (status, decided) = decide(&cell, APPROVER, &id, "approve");
    assert_eq!(status, 200, "{decided}");
    let page = requested(&cell, OPERATOR, None);
    assert_eq!(page["items"].as_array().unwrap().len(), 1, "{page}");
    assert_eq!(page["items"][0]["id"], json!(id));
    assert_eq!(page["items"][0]["canRun"], json!(true));
    assert_eq!(page.get("totalCapped"), None, "{page}");
}

/// DECISION-SCAN-1 (#1702): a pending-decisions list reads at most
/// `MAX_DECISION_SCAN` parked commands, a storage page at a time, and charges
/// every one it reads: one the caller may not decide (their own call) and one
/// past its deadline cost the same as one it lists. A decision parked after
/// the budget is spent is not listed, and the page says its total is capped.
#[test]
fn a_decision_list_charges_every_parked_command_it_reads() {
    let cell = cell();
    gate(&cell, Some(60_000));
    let scan = norn_api::decisions::MAX_DECISION_SCAN;
    let half = scan / 2;
    // The approver's own calls: parked, but never theirs to decide.
    for n in 0..half {
        let body = melt_body(&format!("melt-i{n}"), "12", "10", &format!("approver-i{n}"));
        let (status, parked) = convert(&cell, APPROVER, &body);
        assert_eq!(status, 202, "{parked}");
    }
    // The operator's calls: the approver's to decide until their deadline.
    for n in half..scan {
        let body = melt_body(&format!("melt-e{n}"), "12", "10", &format!("operator-e{n}"));
        let (status, parked) = convert(&cell, OPERATOR, &body);
        assert_eq!(status, 202, "{parked}");
    }
    let list = |at: u64| {
        let query = format!("{SCOPE}&state=pending&limit=200");
        let (status, page) = call_at(&cell, ("GET", "/v1/decisions", &query), APPROVER, "", at);
        assert_eq!(status, 200, "{page}");
        page
    };
    // Exactly the budget read, nothing left: the total is exact.
    let page = list(NOW);
    assert_eq!(page["total"], json!(scan.saturating_sub(half)), "{page}");
    assert_eq!(page.get("totalCapped"), None, "{page}");
    // One more decision, parked later, lies past the budget.
    let path = format!("/v1/actions/{MELT_ACTION}");
    let body = melt_body("melt-last", "12", "10", "operator-last");
    let (status, parked) = call_at(&cell, ("POST", &path, SCOPE), OPERATOR, &body, NOW + 30);
    assert_eq!(status, 202, "{parked}");
    let newest = parked["decision"]["id"].clone();
    assert!(newest.is_string(), "{parked}");
    let page = list(NOW + 30);
    assert_eq!(page["total"], json!(scan.saturating_sub(half)), "{page}");
    assert_eq!(page["totalCapped"], json!(true), "{page}");
    // The operator's first calls have expired and the approver's are not
    // theirs: none is listed, yet each was read and charged, so the last
    // decision (open, and the approver's to decide) still lies past the budget.
    let page = list(NOW + 61);
    assert_eq!(page["items"], json!([]), "{page}");
    assert_eq!(page["total"], json!(0), "{page}");
    assert_eq!(page["totalCapped"], json!(true), "{page}");
    assert!(!page.to_string().contains(&newest.to_string()), "{page}");
}

/// #1587: an approval-gated action with optional `trace` context parks with
/// that exact trace visible on pending and requested decision reads, binds the
/// approval to the trace (omitting or altering `trace` parks a distinct
/// decision instead of consuming the approval), and commits & replays with the
/// exact same `trace` on receipts and object history.
#[test]
fn an_approved_call_preserves_and_binds_trace_context() {
    let cell = cell();
    gate(&cell, None);
    let trace = json!({
        "traceId": "trc_approval_e2e_01",
        "intentRef": "goal:approve-melt-7",
        "sessionDigest": format!("sha256:{}", "b".repeat(64)),
    });
    let traced_body = json!({
        "input": { "id": "melt-7", "input_fine_g": "12", "output_fine_g": "10" },
        "idempotencyKey": "operator-traced-1",
        "trace": trace,
    })
    .to_string();

    let (status, parked) = convert(&cell, OPERATOR, &traced_body);
    assert_eq!(status, 202, "{parked}");
    assert_eq!(parked["outcome"], json!("pending"));
    let id = parked["decision"]["id"].as_str().unwrap().to_owned();
    assert_eq!(melt_status(&cell), 404);

    // Pending view for approver exposes the parked call's exact trace.
    let page = pending(&cell, APPROVER);
    assert_eq!(page["total"], json!(1), "{page}");
    assert_eq!(page["items"][0]["call"]["trace"], trace, "{page}");

    // Requested view for requester exposes the exact trace.
    let req_page = requested(&cell, OPERATOR, None);
    assert_eq!(req_page["items"].as_array().unwrap().len(), 1, "{req_page}");
    assert_eq!(req_page["items"][0]["call"]["trace"], trace, "{req_page}");

    let (status, decided) = decide(&cell, APPROVER, &id, "approve");
    assert_eq!(status, 200, "{decided}");
    assert_eq!(decided["call"], not_executed(), "{decided}");
    assert_eq!(melt_status(&cell), 404);

    // Retrying with an altered trace or omitted trace does not consume the
    // approval: each parks a distinct decision and leaves the melt uncommitted.
    let other_trace_body = json!({
        "input": { "id": "melt-7", "input_fine_g": "12", "output_fine_g": "10" },
        "idempotencyKey": "operator-traced-1",
        "trace": { "traceId": "trc_approval_e2e_other" },
    })
    .to_string();
    let (status, parked_other) = convert(&cell, OPERATOR, &other_trace_body);
    assert_eq!(status, 202, "{parked_other}");
    assert_ne!(parked_other["decision"]["id"], json!(id), "{parked_other}");

    let untrace_body = melt7("operator-traced-1");
    let (status, parked_untraced) = convert(&cell, OPERATOR, &untrace_body);
    assert_eq!(status, 202, "{parked_untraced}");
    assert_ne!(
        parked_untraced["decision"]["id"],
        json!(id),
        "{parked_untraced}"
    );
    assert_eq!(melt_status(&cell), 404);

    // Rebuilding the retry directly from GET /v1/decisions:requested commits it.
    let approved_page = requested(&cell, OPERATOR, Some(&id));
    let approved_call = &approved_page["items"][0]["call"];
    assert_eq!(approved_call["trace"], trace, "{approved_page}");
    let retry = format!(
        r#"{{"input":{},"idempotencyKey":{},"trace":{}}}"#,
        approved_call["encodedInput"].as_str().unwrap(),
        approved_call["idempotencyKey"],
        approved_call["trace"],
    );
    let (status, committed) = convert(&cell, OPERATOR, &retry);
    assert_eq!(status, 200, "{committed}");
    assert_eq!(committed["outcome"], json!("committed"), "{committed}");
    assert_eq!(committed["trace"], trace, "{committed}");
    assert_eq!(melt_status(&cell), 200);

    // Exact replay returns the same receipt with the same trace.
    let (status, replayed) = convert(&cell, OPERATOR, &retry);
    assert_eq!(status, 200, "{replayed}");
    assert_eq!(replayed["outcome"], json!("replayed"), "{replayed}");
    assert_eq!(replayed["trace"], trace, "{replayed}");

    // Object version history discloses the action writer with the recorded trace.
    let (status, history) = call(
        &cell,
        "GET",
        "/v1/objects/constitution.Melt/melt-7/history",
        OPERATOR,
        "",
    );
    assert_eq!(status, 200, "{history}");
    assert_eq!(
        history["versions"][0]["writer"]["kind"],
        json!("action"),
        "{history}"
    );
    assert_eq!(
        history["versions"][0]["writer"]["trace"], trace,
        "{history}"
    );
}
