//! APPROVAL-RECOVERY-1 (#1481, PARKED-CRASH for hosted approvals): the host
//! dies after an approval is recorded but before the approved call commits.
//! After a reopen the call commits exactly once, or its refusal is visible on
//! the decision; it is never silently dropped.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "dispatch correspondence"
)]

#[path = "support/step_cell.rs"]
mod step_cell;

use std::path::Path;

use norn_cell::engine::{
    EngineFault, EngineFaultKind, FaultEngine, FaultSwitch, NativeEngine, SqlEngine,
};
use norn_cell::{MAIN_WORLD, StoreOptions};
use serde_json::{Value, json};
use step_cell::{APPROVER, Cell, MELT_ACTION, NOW, OPERATOR, call, cell_on, melt_body};

const APPROVAL_ID: &str = "ops.melt_approval";

fn file(path: &Path) -> NativeEngine {
    NativeEngine::open(path, StoreOptions::default()).unwrap()
}

/// Install the approval gating the melt action, deciders `admin`.
fn gate<E: norn_cell::engine::SqlEngine>(cell: &Cell<E>) {
    let mut body = json!({ "kind": "approval", "id": APPROVAL_ID, "payload": {
        "action": MELT_ACTION, "decider": { "roles": ["admin"] },
        "question": "Convert this melt?", "risk": "high" } });
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

fn melt_status<E: norn_cell::engine::SqlEngine>(cell: &Cell<E>) -> u16 {
    call(
        cell,
        "GET",
        "/v1/objects/constitution.Melt/melt-7",
        OPERATOR,
        "",
    )
    .0
}

/// A gated cell on `path` with the operator's melt call parked; its
/// decision id.
fn parked(path: &Path) -> String {
    let cell = cell_on(file(path), true);
    gate(&cell);
    let (status, parked) = call(
        &cell,
        "POST",
        &format!("/v1/actions/{MELT_ACTION}"),
        OPERATOR,
        &melt_body("melt-7", "12", "10", "operator-1"),
    );
    assert_eq!(status, 202, "{parked}");
    parked["decision"]["id"].as_str().unwrap().to_owned()
}

fn approve<E: norn_cell::engine::SqlEngine>(cell: &Cell<E>, id: &str) -> (u16, Value) {
    call(
        cell,
        "POST",
        &format!("/v1/decisions/{id}:decide"),
        APPROVER,
        &json!({ "choice": "approve", "idempotencyKey": "approve-1" }).to_string(),
    )
}

/// What a decide that crashed at transaction `at` left on disk: whether the
/// approval is recorded, and whether the melt committed.
fn crash_at(path: &Path, id: &str, at: u32) -> (bool, bool) {
    let faults = FaultSwitch::default();
    let cell = cell_on(FaultEngine::new(file(path), faults.clone()), false);
    faults.arm(EngineFault {
        kind: EngineFaultKind::Crash,
        transaction: at,
        at_statement: 0,
    });
    let _ = approve(&cell, id);
    assert!(faults.disarm(), "the crash at {at} fired");
    drop(cell);
    let reopened = cell_on(file(path), false);
    (resolved(path, id), melt_status(&reopened) == 200)
}

/// Whether the cell on `path` recorded the resolution of `id`.
fn resolved(path: &Path, id: &str) -> bool {
    !file(path)
        .query(
            r#"SELECT 1 FROM "_norn_decision" WHERE "world"=?1 AND "decision_id"=?2"#,
            &[MAIN_WORLD.into(), id.into()],
        )
        .unwrap()
        .is_empty()
}

/// The first crash point after the approval is recorded and before the call
/// commits, on a fresh copy of the parked cell each try.
fn crash_between(directory: &Path) -> (std::path::PathBuf, String) {
    for at in 0..64 {
        let path = directory.join(format!("cell-{at}.sqlite"));
        let id = parked(&path);
        if crash_at(&path, &id, at) == (true, false) {
            return (path, id);
        }
    }
    panic!("no crash point lies between the approval and the call");
}

/// The host dies between recording the approval and committing the call.
/// After a reopen the alarm's due work runs: the call commits, once.
#[test]
fn an_approved_call_lost_to_a_crash_commits_after_a_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let (path, _id) = crash_between(directory.path());
    let reopened = cell_on(file(&path), false);
    assert_eq!(melt_status(&reopened), 404, "the crash lost the commit");
    let due = norn_api::timers::next_due(&reopened.host).unwrap();
    let ran = norn_api::timers::run_due(
        &reopened.host,
        norn_kernel::LogicalTime::from_millis(due.unwrap_or(NOW * 1000)),
    )
    .unwrap();
    assert_eq!(melt_status(&reopened), 200, "{due:?} {ran:?}");
}
