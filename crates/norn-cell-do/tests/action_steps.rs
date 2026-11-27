//! ACTION-STEP-1 through the DO fetch dispatch: a run's `norn.action@1` step
//! calls a governed action under the authority of whoever started the run,
//! for the whole run. The policy here decides an action for the cell's
//! current verified caller, as the hosted cells' does, so a step that ran as
//! the request's own caller (the decider) instead of the recorded starter
//! would be decided for the decider. A decision adds the decider's approval
//! as a condition and lends the run none of the decider's rights.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "dispatch correspondence"
)]

#[path = "support/step_cell.rs"]
pub(super) mod step_cell;
use serde_json::{Value, json};
use step_cell::{APPROVER, Cell, FLOW_ID, MEMBER, OPERATOR, SCOPE, call, call_with, cell};

/// Start `run` as `starter`, then have the approver approve it.
fn start_and_approve(cell: &Cell, starter: Option<&str>, run: &str) -> Value {
    let (status, opened) = call(
        cell,
        "POST",
        "/v1/runs",
        starter,
        &json!({ "flow": FLOW_ID, "runId": run }).to_string(),
    );
    assert_eq!(status, 201, "{opened}");
    assert_eq!(opened["state"], json!("awaiting_perform_ask_review"));
    let pending = format!("{SCOPE}&state=pending");
    let (_, page) = call_with(cell, "GET", "/v1/decisions", &pending, APPROVER, "");
    assert_eq!(page["total"], json!(1), "{page}");
    let id = page["items"][0]["id"].as_str().unwrap().to_owned();
    let (status, decided) = call(
        cell,
        "POST",
        &format!("/v1/decisions/{id}:decide"),
        APPROVER,
        &json!({ "choice": "approve", "idempotencyKey": "k-1" }).to_string(),
    );
    assert_eq!(status, 200, "{decided}");
    decided
}

fn melt(cell: &Cell) -> u16 {
    call(
        cell,
        "GET",
        "/v1/objects/constitution.Melt/melt-7",
        OPERATOR,
        "",
    )
    .0
}

/// STEP-AS-STARTER: approved, the step converts the melt under the starter's
/// authority and the run completes; the decider's own caller is back after.
#[test]
fn an_approved_step_runs_as_the_starter() {
    let cell = cell();
    let decided = start_and_approve(&cell, OPERATOR, "melt-run-1");
    assert_eq!(decided["run"]["state"], json!("completed"), "{decided}");
    assert_eq!(melt(&cell), 200);
    let (_, run) = call(&cell, "GET", "/v1/runs/melt-run-1", OPERATOR, "");
    let steps: Vec<(&str, &str, &str)> = run["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|step| {
            (
                step["id"].as_str().unwrap(),
                step["kind"].as_str().unwrap(),
                step["status"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        steps,
        [
            ("ask_review", "decide", "done"),
            ("convert", "perform", "done"),
            ("succeed", "succeed", "done")
        ],
        "{run}"
    );
    // Each request ends with no caller recorded: the step's starter never
    // outlives its one call, and the request's own principal ends with it.
    assert_eq!(cell.caller.current(), None);
}

/// STEP-NO-LAUNDERING (coordinator ruling): a starter without the right to
/// convert cannot get a conversion run by routing it after a decision whose
/// decider has that right. The step is denied as the starter, the run fails,
/// and nothing is converted.
#[test]
fn a_decider_lends_the_run_none_of_its_rights() {
    let cell = cell();
    let decided = start_and_approve(&cell, MEMBER, "melt-run-2");
    assert_eq!(decided["run"]["state"], json!("failed_denied"), "{decided}");
    assert_eq!(melt(&cell), 404);
    let (_, run) = call(&cell, "GET", "/v1/runs/melt-run-2", MEMBER, "");
    assert_eq!(run["terminal"], json!(true));
    let convert = &run["steps"][1];
    assert_eq!(
        (
            &convert["id"],
            &convert["status"],
            &convert["error"]["code"]
        ),
        (&json!("convert"), &json!("failed"), &json!("failed_denied")),
        "{run}"
    );
}

/// #1587: a flow's `norn.action@1` step may carry an optional `trace` context
/// that propagates into the committed `norn.ActionCall` record and object
/// version history.
#[test]
fn a_flow_action_step_propagates_optional_trace_context() {
    let cell = step_cell::cell_on(
        norn_cell::engine::NativeEngine::open_in_memory().unwrap(),
        false,
    );
    let session_digest = format!("sha256:{}", "c".repeat(64));
    let expected_trace = json!({
        "traceId": "trc_flow_step_01",
        "intentRef": "goal:flow-melt-7",
        "sessionDigest": session_digest,
    });
    let mut artifacts: Vec<Value> = serde_json::from_str(step_cell::LOWERED).unwrap();
    for artifact in &mut artifacts {
        artifact["scope"] = json!({ "orgId": "org_demo", "environmentId": "dev" });
    }
    let rule = &mut artifacts[1]["payload"];
    let rules = rule["graph"]["nodes"][1]["content"]["rules"]
        .as_array_mut()
        .unwrap();
    let convert_rule = rules
        .iter_mut()
        .find(|entry| entry["_id"] == "awaiting_perform_ask_review/result/1")
        .unwrap();
    convert_rule["o_commands"] = json!(format!(
        "[{{kind: 'perform', runId: slice.run.run_id, step: 'convert', function: 'norn.action', functionVersion: '1', idempotencyKey: slice.run.run_id + ':convert', attempt: 1, input: {{convert: {{action: 'constitution.melt.convert', input: {{id: 'melt-7', input_fine_g: '12', output_fine_g: '10'}}, trace: {{traceId: 'trc_flow_step_01', intentRef: 'goal:flow-melt-7', sessionDigest: '{session_digest}'}}}}}}}}]"
    ));
    rule["outputSchema"]["properties"]["commands"]["items"]["properties"]["input"]["properties"]
        ["convert"]["properties"]["trace"] = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["traceId"],
        "properties": {
            "traceId": { "type": "string" },
            "intentRef": { "type": "string" },
            "sessionDigest": { "type": "string" }
        }
    });
    let mut body = json!({
        "kind": "flow",
        "id": FLOW_ID,
        "payload": {
            "flow": { "id": FLOW_ID, "version": "1.0.0", "title": "Approve a melt" },
            "artifacts": artifacts,
        }
    });
    let (status, admitted) = call(
        &cell,
        "POST",
        "/v1/definitions:admit",
        OPERATOR,
        &body.to_string(),
    );
    assert_eq!(status, 200, "{admitted}");
    body["signoff"] = json!({ "restatementSha256": admitted["restatementSha256"] });
    let (status, installed) = call(
        &cell,
        "POST",
        "/v1/definitions:install",
        OPERATOR,
        &body.to_string(),
    );
    assert_eq!(status, 200, "{installed}");

    let decided = start_and_approve(&cell, OPERATOR, "melt-run-traced");
    assert_eq!(decided["run"]["state"], json!("completed"), "{decided}");
    assert_eq!(melt(&cell), 200);

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
        history["versions"][0]["writer"]["trace"], expected_trace,
        "{history}"
    );
}

/// SHA-256 of the receipt and ledger rows [`flow_receipts_and_ledger_match_the_pre_u1_bytes`]
/// reads, as the build before automations U1 (#1650, `f51541ce72`) wrote them, plus
/// the one field AUTH-INDEPENDENT-PERSON (#618) adds: the run starter's
/// `startedBy.accountablePerson`. Without that field the rows hash to the pre-U1
/// `37486316913f7a28786a73fc7d515c0ef335ce5094a91d614c9c70f3adea384c`.
const PRE_U1_FLOW_BYTES_SHA256: &str =
    "31027e19d7f8df6ac4bb52ab4cfffb299489f05ea95ade88adde52471df44275";

/// #1704: a cell whose flow runs with no automation installed writes exactly
/// the receipt and ledger bytes it wrote before automations U1 (#1650), so
/// the outbox, its trigger index and its continuation leave every
/// automation-free commit untouched.
#[test]
fn flow_receipts_and_ledger_match_the_pre_u1_bytes() {
    use norn_cell::engine::{NativeEngine, SqlEngine};
    use sha2::{Digest, Sha256};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("flow-bytes.sqlite");
    {
        let engine = NativeEngine::open(&path, norn_cell::StoreOptions::default()).unwrap();
        let cell = step_cell::cell_on(engine, true);
        let decided = start_and_approve(&cell, OPERATOR, "melt-run-bytes");
        assert_eq!(decided["run"]["state"], json!("completed"), "{decided}");
    }
    let engine = NativeEngine::open(&path, norn_cell::StoreOptions::default()).unwrap();
    let rows = |sql: &str| -> String {
        engine
            .query_row(sql, &[], |row| row.get::<Option<String>>(0))
            .unwrap()
            .unwrap_or_default()
    };
    let receipts = rows(
        "SELECT group_concat(line, char(10)) FROM (SELECT commit_seq || '|' || world || '|' \
         || coalesce(idempotency_key, '') || '|' || request_hash || '|' || receipt || '|' \
         || receipt_hash AS line FROM _norn_receipt ORDER BY commit_seq)",
    );
    let ledger = rows(
        "SELECT group_concat(line, char(10)) FROM (SELECT seq || '|' || commit_seq || '|' \
         || world || '|' || event || '|' || prev_hash || '|' || hash AS line \
         FROM _norn_ledger ORDER BY seq)",
    );
    assert!(!receipts.is_empty() && !ledger.is_empty());
    let hex = format!("{:x}", Sha256::digest(format!("{receipts}\n--\n{ledger}")));
    assert_eq!(
        hex, PRE_U1_FLOW_BYTES_SHA256,
        "receipts:\n{receipts}\nledger:\n{ledger}"
    );
}

/// STEP-PIN-1 (D2): when the action targeted by a parked flow step changes
/// after the flow run started, resuming the run refuses the step fail-closed
/// with `action_step.definition_changed` rather than executing the mutated
/// action definition.
#[test]
fn a_parked_step_refuses_when_its_action_definition_changes() {
    let cell = cell();
    let (status, opened) = call(
        &cell,
        "POST",
        "/v1/runs",
        OPERATOR,
        &json!({ "flow": FLOW_ID, "runId": "melt-run-drifted" }).to_string(),
    );
    assert_eq!(status, 201, "{opened}");
    assert_eq!(opened["state"], json!("awaiting_perform_ask_review"));

    // Revise the action definition from DERIVES_LOSS to OMITS_LOSS while the
    // run is parked on `ask_review`, keeping all installed flow artifacts intact.
    revise_action_to_omits_loss(&cell);

    let pending = format!("{SCOPE}&state=pending");
    let (_, page) = call_with(&cell, "GET", "/v1/decisions", &pending, APPROVER, "");
    assert_eq!(page["total"], json!(1), "{page}");
    let id = page["items"][0]["id"].as_str().unwrap().to_owned();
    let (status, decided) = call(
        &cell,
        "POST",
        &format!("/v1/decisions/{id}:decide"),
        APPROVER,
        &json!({ "choice": "approve", "idempotencyKey": "k-drift" }).to_string(),
    );
    assert_eq!(status, 200, "{decided}");
    assert_eq!(
        decided["run"]["state"],
        json!("failed_refused"),
        "{decided}"
    );
    assert_eq!(melt(&cell), 404, "mutated action must never execute");
}

fn install_flow_with_pin(cell: &Cell, pin_expr: &str) {
    let mut artifacts: Vec<Value> = serde_json::from_str(step_cell::LOWERED).unwrap();
    for artifact in &mut artifacts {
        artifact["scope"] = json!({ "orgId": "org_demo", "environmentId": "dev" });
    }
    let rule = &mut artifacts[1]["payload"];
    let rules = rule["graph"]["nodes"][1]["content"]["rules"]
        .as_array_mut()
        .unwrap();
    let convert_rule = rules
        .iter_mut()
        .find(|entry| entry["_id"] == "awaiting_perform_ask_review/result/1")
        .unwrap();
    convert_rule["o_commands"] = json!(format!(
        "[{{kind: 'perform', runId: slice.run.run_id, step: 'convert', function: 'norn.action', functionVersion: '1', idempotencyKey: slice.run.run_id + ':convert', attempt: 1, input: {{convert: {{action: 'constitution.melt.convert', input: {{id: 'melt-7', input_fine_g: '12', output_fine_g: '10'}}, pin: {pin_expr}}}}}}}]"
    ));
    rule["outputSchema"]["properties"]["commands"]["items"]["properties"]["input"]["properties"]
        ["convert"]["properties"]["pin"] = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "artifactId": { "type": "string" },
            "version": { "type": "string" },
            "digest": { "type": "string" },
            "definitionsVersion": { "type": "integer" }
        }
    });
    let mut body = json!({
        "kind": "flow",
        "id": FLOW_ID,
        "payload": {
            "flow": { "id": FLOW_ID, "version": "1.0.0", "title": "Approve a melt" },
            "artifacts": artifacts,
        }
    });
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

/// STEP-PIN-1 (D2): an explicit `pin` matching the active action definition
/// executes cleanly, while a mismatched explicit `pin` refuses fail-closed
/// without executing the action.
#[test]
fn a_parked_step_enforces_explicit_pin_match_and_mismatch() {
    let ok_cell = step_cell::cell_on(
        norn_cell::engine::NativeEngine::open_in_memory().unwrap(),
        false,
    );
    install_flow_with_pin(
        &ok_cell,
        "{artifactId: 'constitution.melt.convert.action', version: '1.0.0', definitionsVersion: 2}",
    );
    let decided = start_and_approve(&ok_cell, OPERATOR, "melt-run-pin-ok");
    assert_eq!(decided["run"]["state"], json!("completed"), "{decided}");
    assert_eq!(melt(&ok_cell), 200);

    let bad_cell = step_cell::cell_on(
        norn_cell::engine::NativeEngine::open_in_memory().unwrap(),
        false,
    );
    install_flow_with_pin(
        &bad_cell,
        "{artifactId: 'constitution.melt.convert.action', version: '9.9.9'}",
    );
    let refused = start_and_approve(&bad_cell, OPERATOR, "melt-run-pin-bad");
    assert_eq!(
        refused["run"]["state"],
        json!("failed_refused"),
        "{refused}"
    );
    assert_eq!(melt(&bad_cell), 404);
}

fn revise_action_to_omits_loss(cell: &Cell) {
    let version = cell.host.active_definitions_version().unwrap();
    let active_set = cell.host.active_definitions().unwrap().unwrap();
    let active_docs: Vec<Value> = serde_json::from_str(&active_set.canonical_set).unwrap();
    let (model, input) = step_cell::action_cell::melt_install_edited(
        step_cell::action_cell::OMITS_LOSS,
        |docs| {
            for doc in active_docs {
                if !docs.iter().any(|d| d["artifactId"] == doc["artifactId"]) {
                    docs.push(doc);
                }
            }
        },
        norn_api::action_admission::prepare_action_model,
        step_cell::NOW.saturating_mul(1000).saturating_add(1),
    );
    cell.host
        .install_definitions(model, input, version)
        .expect("mutated action installs");
}

fn starter_caller(starter: &norn_cell::flow_run::RunStarter) -> norn_cell_do::policy::Caller {
    let actor_kind = match starter.actor_kind.as_str() {
        "service" => "service",
        _ => "user",
    };
    let role_table = match starter.role_table.as_str() {
        "studio-roles/1" => "studio-roles/1",
        _ => norn_cell_do::principal::FIXTURE_ROLES,
    };
    norn_cell_do::policy::Caller {
        subject_id: starter.subject.clone(),
        roles: starter.roles.clone(),
        actor_kind,
        role_table,
        accountable_person: starter.accountable_person.clone(),
    }
}

fn install_melt_approval_gate(cell: &Cell) {
    let mut body = json!({
        "kind": "approval",
        "id": "ops.approve_melt_convert",
        "payload": {
            "action": step_cell::MELT_ACTION,
            "decider": { "roles": ["admin"] },
            "question": "Convert this melt?",
            "risk": "high"
        }
    });
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

fn decide_as_approver(
    cell: &Cell,
    decision_id: &str,
    key: &str,
    now_ms: u64,
    as_requester: norn_api::flow_driver::AsStarter<'_>,
) -> norn_api::decisions::Decided {
    let scope = norn_api::scope::ScopeQuery {
        org_id: Some("org_demo".to_owned()),
        environment_id: Some("dev".to_owned()),
    };
    let reads = |cap: norn_api::authz::ReadCapability, ty: &str| {
        norn_cell_do::policy::collection_access(cell.host.scope(), "approver@steps", cap, ty)
    };
    let body = norn_api::decisions::DecideRequest {
        choice: "approve".to_owned(),
        note: None,
        idempotency_key: key.to_owned(),
    };
    cell.caller.record(Some(norn_cell_do::policy::Caller {
        subject_id: "approver@steps".to_owned(),
        roles: vec!["admin".to_owned()],
        actor_kind: "user",
        role_table: norn_cell_do::principal::FIXTURE_ROLES,
        accountable_person: None,
    }));
    let decided = norn_api::decisions::decide(
        &cell.host,
        &norn_api::decisions::DecideCall {
            scope: &scope,
            actor: Some("approver@steps"),
            id: decision_id,
            body: &body,
            now: norn_kernel::LogicalTime::from_millis(now_ms),
        },
        &reads,
        &cell.handles,
        as_requester,
    )
    .expect("decision approves");
    cell.caller.record(None);
    decided
}

fn decide_ask_review_without_driving(cell: &Cell, decision_id: &str, key: &str) {
    let no_requester = |_p: &norn_cell::flow_run::RunStarter,
                        _c: &mut dyn FnMut() -> Result<
        norn_api::actions::ActionAnswer,
        norn_api::refusal::Refusal,
    >| None;
    let _ = decide_as_approver(
        cell,
        decision_id,
        key,
        step_cell::NOW.saturating_mul(1000),
        &no_requester,
    );
}

/// STEP-PIN-1 retry recovery: when a parked action step commits its action and
/// the process stops before settling the step, a later definition change must
/// not turn the retry into a pin refusal; retry recovers the attempt-owned
/// committed outcome and completes the run.
#[test]
fn retry_after_commit_and_definition_change_recovers_committed_outcome() {
    let cell = cell();
    let (status, opened) = call(
        &cell,
        "POST",
        "/v1/runs",
        OPERATOR,
        &json!({ "flow": FLOW_ID, "runId": "melt-run-recover" }).to_string(),
    );
    assert_eq!(status, 201, "{opened}");
    let pending = format!("{SCOPE}&state=pending");
    let (_, page) = call_with(&cell, "GET", "/v1/decisions", &pending, APPROVER, "");
    let id = page["items"][0]["id"].as_str().unwrap().to_owned();

    // Decide `ask_review` directly without driving `convert`, leaving it parked.
    decide_ask_review_without_driving(&cell, &id, "k-recover");

    let now_ms = step_cell::NOW.saturating_mul(1000);
    let no_requester = |_p: &norn_cell::flow_run::RunStarter,
                        _c: &mut dyn FnMut() -> Result<
        norn_api::actions::ActionAnswer,
        norn_api::refusal::Refusal,
    >| None;
    // First drive commits the action, then simulates an interruption before `settle`.
    let crash_after_commit = |starter: &norn_cell::flow_run::RunStarter,
                              dispatch: &mut dyn FnMut() -> Result<
        norn_api::actions::ActionAnswer,
        norn_api::refusal::Refusal,
    >| {
        cell.caller.record(Some(starter_caller(starter)));
        let committed = dispatch();
        cell.caller.record(None);
        assert!(committed.is_ok(), "action must commit: {committed:?}");
        Some(Err(norn_api::refusal::Refusal::host("simulated crash")))
    };
    assert!(
        norn_api::flow_driver::drive_run(
            &cell.host,
            "melt-run-recover",
            &crash_after_commit,
            &no_requester,
            &cell.handles,
            norn_kernel::LogicalTime::from_millis(now_ms),
        )
        .is_err()
    );
    assert_eq!(melt(&cell), 200, "action side effect is already committed");
    let (_, mid_run) = call(&cell, "GET", "/v1/runs/melt-run-recover", OPERATOR, "");
    assert_eq!(mid_run["state"], json!("awaiting_perform_convert"));

    // Mutate action definition before retry; retry recovers committed outcome.
    revise_action_to_omits_loss(&cell);
    let as_starter = |starter: &norn_cell::flow_run::RunStarter,
                      dispatch: &mut dyn FnMut() -> Result<
        norn_api::actions::ActionAnswer,
        norn_api::refusal::Refusal,
    >| {
        cell.caller.record(Some(starter_caller(starter)));
        let out = dispatch();
        cell.caller.record(None);
        Some(out)
    };
    norn_api::flow_driver::drive_run(
        &cell.host,
        "melt-run-recover",
        &as_starter,
        &no_requester,
        &cell.handles,
        norn_kernel::LogicalTime::from_millis(now_ms.saturating_add(2)),
    )
    .expect("retry settles committed step cleanly");
    let (_, final_run) = call(&cell, "GET", "/v1/runs/melt-run-recover", OPERATOR, "");
    assert_eq!(final_run["state"], json!("completed"), "{final_run}");
}

/// STEP-PIN-1 through approval dispatch: when a parked step with an explicit
/// `definitionsVersion` pin parks for approval and definitions advance before
/// the approved call executes, approval dispatch records and settles
/// `action_step.pin_mismatch` rather than `defs.stale`.
#[test]
fn an_approved_step_preserves_pin_mismatch_when_definitions_version_drifts() {
    let cell = step_cell::cell_on(
        norn_cell::engine::NativeEngine::open_in_memory().unwrap(),
        false,
    );
    install_melt_approval_gate(&cell);
    let pinned_version = cell
        .host
        .active_definitions_version()
        .unwrap()
        .unwrap()
        .saturating_add(1);
    install_flow_with_pin(
        &cell,
        &format!(
            "{{artifactId: 'constitution.melt.convert.action', version: '1.0.0', definitionsVersion: {pinned_version}}}"
        ),
    );
    assert_eq!(
        cell.host.active_definitions_version().unwrap(),
        Some(pinned_version)
    );
    let decided_review = start_and_approve(&cell, OPERATOR, "melt-run-approved-pin");
    assert_eq!(
        decided_review["run"]["state"],
        json!("awaiting_perform_convert"),
        "{decided_review}"
    );
    let pending = format!("{SCOPE}&state=pending");
    let (_, page) = call_with(&cell, "GET", "/v1/decisions", &pending, APPROVER, "");
    assert_eq!(page["total"], json!(1), "{page}");
    let approval_id = page["items"][0]["id"].as_str().unwrap().to_owned();

    // Advance definitions version past `pinned_version` while keeping the action present.
    revise_action_to_omits_loss(&cell);

    let as_requester = |starter: &norn_cell::flow_run::RunStarter,
                        dispatch: &mut dyn FnMut() -> Result<
        norn_api::actions::ActionAnswer,
        norn_api::refusal::Refusal,
    >| {
        let previous = cell.caller.replace(Some(starter_caller(starter)), false);
        let out = dispatch();
        cell.caller.replace(previous.0, previous.1);
        Some(out)
    };
    let now_ms = step_cell::NOW.saturating_mul(1000).saturating_add(2);
    let decided = decide_as_approver(&cell, &approval_id, "k-approve-pin", now_ms, &as_requester);
    assert_eq!(
        decided.call.as_ref().and_then(|c| c.code.as_deref()),
        Some(norn_api::step_pin::step_pin_codes::PIN_MISMATCH),
        "{decided:?}"
    );
    let no_requester = |_p: &norn_cell::flow_run::RunStarter,
                        _c: &mut dyn FnMut() -> Result<
        norn_api::actions::ActionAnswer,
        norn_api::refusal::Refusal,
    >| None;
    norn_api::flow_driver::drive_run(
        &cell.host,
        "melt-run-approved-pin",
        &no_requester,
        &no_requester,
        &cell.handles,
        norn_kernel::LogicalTime::from_millis(now_ms.saturating_add(1)),
    )
    .expect("drive settles recorded approval refusal");
    let (_, final_run) = call(&cell, "GET", "/v1/runs/melt-run-approved-pin", OPERATOR, "");
    assert_eq!(final_run["state"], json!("failed_refused"), "{final_run}");
    assert_eq!(
        final_run["steps"][1]["error"]["code"],
        json!(norn_api::step_pin::step_pin_codes::PIN_MISMATCH),
        "{final_run}"
    );
    assert_eq!(melt(&cell), 404);
}
