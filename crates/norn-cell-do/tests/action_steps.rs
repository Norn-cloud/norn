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
