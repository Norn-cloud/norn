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
/// never executes it under N+1 when N+1 changed the action: approving it is
/// refused `approval.definition_changed` and commits nothing, and the
/// requester's call under the new action parks anew for a fresh approval.
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
    assert_eq!(
        decided["call"],
        json!({ "committed": false, "code": "approval.definition_changed" }),
        "{decided}"
    );
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
    assert_eq!(decided["call"], json!({ "committed": true }), "{decided}");
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
    assert_eq!(
        decided["call"],
        json!({ "committed": false, "code": "approval.definition_changed" }),
        "{decided}"
    );
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
    assert_eq!(
        decided["call"],
        json!({ "committed": false, "code": "approval.definition_changed" }),
        "{decided}"
    );
    assert_eq!(melt_status(&cell), 404);
    let (status, again) = convert(&cell, OPERATOR, &body);
    assert_eq!(status, 202, "{again}");
    assert_ne!(again["decision"]["id"], json!(id), "{again}");
}

/// APPROVAL-REVISION-1 on the hosted path: an approved step call refused
/// `approval.definition_changed` drives nothing. The run's waiting step is
/// not called again under the changed action (no new approval parks), and
/// the run stays where it was.
#[test]
fn a_stale_approval_never_drives_its_flow_step() {
    let cell = cell();
    gate(&cell, None);
    let approval = start_gated_run(&cell, "melt-run-stale");
    // The action's next revision, over the active set (the flow stays).
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
    assert_eq!(
        decided["call"]["code"],
        json!("approval.definition_changed"),
        "{decided}"
    );
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
