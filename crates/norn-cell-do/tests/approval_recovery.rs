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

use norn_api::timers::next_due;
use norn_cell::engine::{
    EngineFault, EngineFaultKind, FaultEngine, FaultSwitch, NativeEngine, SqlEngine,
};
use norn_cell::{MAIN_WORLD, StoreOptions};
use norn_kernel::LogicalTime;
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
/// approval is recorded, and whether the melt committed; `None` once `at`
/// lies past the decide's last transaction.
fn crash_at(path: &Path, id: &str, at: u32) -> Option<(bool, bool)> {
    let faults = FaultSwitch::default();
    let cell = cell_on(FaultEngine::new(file(path), faults.clone()), false);
    faults.arm(EngineFault {
        kind: EngineFaultKind::Crash,
        transaction: at,
        at_statement: 0,
    });
    let _answer = approve(&cell, id);
    if !faults.disarm() {
        return None;
    }
    drop(cell);
    let reopened = cell_on(file(path), false);
    Some((resolved(path, id), melt_status(&reopened) == 200))
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

/// The approved call's settlement as the cell on `path` records it.
fn settlement(path: &Path, id: &str) -> Option<(String, Option<String>)> {
    let rows = file(path)
        .query(
            r#"SELECT "state","code" FROM "_norn_approved_call" WHERE "world"=?1 AND "decision_id"=?2"#,
            &[MAIN_WORLD.into(), id.into()],
        )
        .unwrap();
    rows.into_iter().next().map(|row| {
        let row = norn_cell::engine::SqlRow(row);
        (row.get(0).unwrap(), row.get(1).unwrap())
    })
}

/// The first crash point whose aftermath is `want` (approval recorded, melt
/// committed), on a fresh copy of the parked cell each try; the cell and the
/// decision.
fn crash_leaving(directory: &Path, want: (bool, bool)) -> (std::path::PathBuf, String) {
    for at in 0..64 {
        let path = directory.join(format!("cell-{at}.sqlite"));
        let id = parked(&path);
        match crash_at(&path, &id, at) {
            Some(left) if left == want => return (path, id),
            Some(_) => {}
            None => break,
        }
    }
    panic!("no crash point of the decide leaves {want:?}");
}

/// The alarm's due work at `at` (logical milliseconds), as the hosted cell
/// runs it.
fn wake<E: SqlEngine>(cell: &Cell<E>, at: u64) -> norn_api::timers::DueWork {
    norn_cell_do::server::run_due(
        &cell.host,
        &cell.principals,
        Some(&cell.handles),
        LogicalTime::from_millis(at),
    )
    .unwrap()
}

/// The requester calls again with the same key: what the cell answers.
fn retry<E: SqlEngine>(cell: &Cell<E>) -> (u16, Value) {
    call(
        cell,
        "POST",
        &format!("/v1/actions/{MELT_ACTION}"),
        OPERATOR,
        &melt_body("melt-7", "12", "10", "operator-1"),
    )
}

/// APPROVAL-RECOVERY-1, crash before the commit: the host dies between
/// recording the approval and committing the call. On main this lost the
/// call: nothing was due after the reopen. Now the approval leaves the call
/// due: the reopened cell wakes at once, a wake that cannot act as the
/// requester keeps it due on its backoff, and the alarm commits it exactly
/// once as its requester; a repeated wake and the requester's retry replay.
#[test]
fn an_approved_call_lost_to_a_crash_commits_once_after_a_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let (path, id) = crash_leaving(directory.path(), (true, false));
    let reopened = cell_on(file(&path), false);
    assert_eq!(melt_status(&reopened), 404, "the crash lost the commit");
    assert_eq!(settlement(&path, &id), Some(("due".to_owned(), None)));
    let due = next_due(&reopened.host)
        .unwrap()
        .expect("the approved call is due");
    let at = due.max(NOW * 1000);

    // A wake with no principal to act as commits nothing and backs off.
    let alone = norn_api::timers::run_due(&reopened.host, LogicalTime::from_millis(at)).unwrap();
    assert_eq!(alone.failed.len(), 1, "{alone:?}");
    assert!(!alone.committed());
    assert_eq!(melt_status(&reopened), 404);
    let retry_at = at + norn_api::timers::STUCK_RETRY_MS;
    assert_eq!(next_due(&reopened.host).unwrap(), Some(retry_at));

    let woke = wake(&reopened, retry_at);
    assert_eq!(woke.approved_calls, 1, "{woke:?}");
    assert!(woke.failed.is_empty(), "{woke:?}");
    assert_eq!(melt_status(&reopened), 200);
    assert_eq!(settlement(&path, &id), Some(("committed".to_owned(), None)));
    assert_eq!(next_due(&reopened.host).unwrap(), None);
    // Delivered again, the wake finds nothing; the requester's retry replays.
    assert_eq!(
        wake(&reopened, retry_at),
        norn_api::timers::DueWork::default()
    );
    let (status, replayed) = retry(&reopened);
    assert_eq!(status, 200, "{replayed}");
    assert_eq!(replayed["outcome"], json!("replayed"), "{replayed}");
    assert_eq!(
        reopened.caller.current(),
        None,
        "no caller outlives the wake"
    );
}

/// APPROVAL-RECOVERY-1, crash after the commit: the call committed but the
/// host died before recording so. The wake settles it from the call's
/// idempotency record and dispatches nothing: no second commit.
#[test]
fn an_approved_call_committed_before_a_crash_never_commits_twice() {
    let directory = tempfile::tempdir().unwrap();
    let (path, id) = crash_leaving(directory.path(), (true, true));
    assert_eq!(settlement(&path, &id), Some(("due".to_owned(), None)));
    let reopened = cell_on(file(&path), false);
    let due = next_due(&reopened.host)
        .unwrap()
        .expect("the settlement is due");
    let woke = wake(&reopened, due.max(NOW * 1000));
    assert_eq!(woke.approved_calls, 0, "nothing was dispatched: {woke:?}");
    assert!(
        woke.failed.is_empty() && woke.refused_calls.is_empty(),
        "{woke:?}"
    );
    assert_eq!(settlement(&path, &id), Some(("committed".to_owned(), None)));
    assert_eq!(next_due(&reopened.host).unwrap(), None);
    let (status, replayed) = retry(&reopened);
    assert_eq!(status, 200, "{replayed}");
    assert_eq!(replayed["outcome"], json!("replayed"), "{replayed}");
}

/// APPROVAL-RECOVERY-1 with APPROVAL-REVISION-1: the action changed while
/// the host was down. The wake never runs the call under the changed
/// action: it settles `approval.definition_changed`, which the approver's
/// replay shows, and the requester's call asks for a fresh approval. The
/// decision is never silently consumed.
#[test]
fn an_approved_call_whose_action_changed_while_down_is_refused_visibly() {
    let directory = tempfile::tempdir().unwrap();
    let (path, id) = crash_leaving(directory.path(), (true, false));
    let reopened = cell_on(file(&path), false);
    let version = reopened.host.active_definitions_version().unwrap();
    let (model, input) = step_cell::action_cell::melt_install(
        step_cell::action_cell::OMITS_LOSS,
        norn_api::action_admission::prepare_action_model,
        NOW * 1000 + 1,
    );
    reopened
        .host
        .install_definitions(model, input, version)
        .expect("the next revision installs");

    let due = next_due(&reopened.host)
        .unwrap()
        .expect("the approved call is due");
    let woke = wake(&reopened, due.max(NOW * 1000 + 1));
    assert_eq!(woke.approved_calls, 0, "{woke:?}");
    assert_eq!(woke.refused_calls.len(), 1, "{woke:?}");
    assert!(
        woke.refused_calls[0].ends_with("approval.definition_changed"),
        "{woke:?}"
    );
    assert_eq!(
        melt_status(&reopened),
        404,
        "nothing ran under the new action"
    );
    assert_eq!(
        settlement(&path, &id),
        Some((
            "refused".to_owned(),
            Some("approval.definition_changed".to_owned())
        ))
    );
    assert_eq!(next_due(&reopened.host).unwrap(), None);

    let (status, replayed) = approve(&reopened, &id);
    assert_eq!(status, 200, "{replayed}");
    assert_eq!(replayed["outcome"], json!("replayed"), "{replayed}");
    assert_eq!(
        replayed["call"],
        json!({ "committed": false, "code": "approval.definition_changed" }),
        "{replayed}"
    );
    let (status, again) = retry(&reopened);
    assert_eq!(status, 202, "{again}");
    assert_ne!(again["decision"]["id"], json!(id), "{again}");
    assert_eq!(melt_status(&reopened), 404);
}
