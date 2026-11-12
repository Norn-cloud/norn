//! APPROVAL-1 through the DO fetch dispatch: while an `approval` definition
//! gates an action, a call that would commit parks as one open decision
//! (202), a call that would be refused is refused as it would be ungated and
//! parks nothing, the requester never decides their own call, and an
//! approved call commits as its requester. A flow step's call parks exactly
//! like a direct one, and so does an MCP tool call (APPROVAL-MCP-PARKS).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "dispatch correspondence"
)]

#[path = "support/step_cell.rs"]
mod step_cell;
use serde_json::{Value, json};
use step_cell::{
    APPROVER, Cell, FLOW_ID, MELT_ACTION, MEMBER, NOW, OPERATOR, SCOPE, call, call_at, call_with,
    cell, melt_body,
};

const APPROVAL_ID: &str = "ops.melt_approval";

/// Install the approval gating the melt action, deciders `admin`, signed by
/// the operator.
fn gate(cell: &Cell, expires_in_ms: Option<u64>) {
    let mut payload = json!({
        "action": MELT_ACTION,
        "decider": { "roles": ["admin"] },
        "question": "Convert this melt?",
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

fn convert(cell: &Cell, auth: Option<&str>, body: &str) -> (u16, Value) {
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

fn decide(cell: &Cell, auth: Option<&str>, id: &str, choice: &str) -> (u16, Value) {
    call(
        cell,
        "POST",
        &format!("/v1/decisions/{id}:decide"),
        auth,
        &json!({ "choice": choice, "idempotencyKey": format!("{choice}-1") }).to_string(),
    )
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
/// role who is not the requester sees and decides it; approving commits the
/// call as the requester, whose retry then replays it.
#[test]
fn an_approved_call_commits_for_its_requester() {
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
    assert_eq!(decided["call"], json!({ "committed": true }), "{decided}");
    assert_eq!(melt_status(&cell), 200);
    assert_eq!(pending(&cell, APPROVER)["total"], json!(0));

    let (status, replayed) = convert(&cell, OPERATOR, &body);
    assert_eq!(status, 200, "{replayed}");
    assert_eq!(replayed["outcome"], json!("replayed"));
    // Every request ends with no caller recorded.
    assert_eq!(cell.caller.current(), None);
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

/// APPROVAL-STEP-PARKS: a flow step's call parks exactly like a direct call; approving it
/// commits as the run's starter and the run completes.
#[test]
fn a_flow_step_parks_like_a_direct_call() {
    let cell = cell();
    gate(&cell, None);
    let approval = start_gated_run(&cell, "melt-run-a");
    let (status, decided) = decide(&cell, APPROVER, &approval, "approve");
    assert_eq!(status, 200, "{decided}");
    assert_eq!(decided["run"]["id"], json!("melt-run-a"));
    assert_eq!(decided["run"]["state"], json!("completed"), "{decided}");
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
/// HTTP call. MCP's loopback goes through the same dispatch, so the tool
/// answers the same pending decision, not an error, and commits nothing.
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
    // The same call over HTTP is the same parked call.
    let (status, direct) = convert(&cell, OPERATOR, &melt7(key));
    assert_eq!((status, &direct), (202, &parked));
    assert_eq!(pending(&cell, APPROVER)["total"], json!(1));
}
