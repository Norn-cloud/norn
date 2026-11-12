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
mod step_cell;
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
