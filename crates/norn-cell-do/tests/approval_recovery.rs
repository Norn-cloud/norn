//! APPROVAL-RECOVERY-1 (#1481, PARKED-CRASH for hosted approvals): the host
//! dies after an approval is recorded but before the approved call commits,
//! or after the call commits but before its run takes the outcome. Nothing
//! runs a call without a request: the call stays visibly approved and not
//! executed until the requester's retry or the approver's replay runs it,
//! exactly once, and a run takes a recorded outcome from the wake without
//! the action being called again. A refusal is recorded on the ledger and
//! stays final, also on a restored cell.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "dispatch correspondence"
)]

#[path = "support/step_cell.rs"]
mod step_cell;

use std::path::{Path, PathBuf};

use norn_api::timers::{DueWork, next_due, run_due};
use norn_cell::engine::{
    EngineFault, EngineFaultKind, FaultEngine, FaultSwitch, NativeEngine, SqlEngine,
};
use norn_cell::export::Exporter;
use norn_cell::{CellStore, MAIN_WORLD, StoreOptions};
use norn_kernel::LogicalTime;
use serde_json::{Value, json};
use step_cell::{
    APPROVER, Cell, DERIVES_LOSS, FLOW_ID, MELT_ACTION, NOW, OPERATOR, SCOPE, call, call_with,
    cell_on, melt_body, melt_store_on,
};

const APPROVAL_ID: &str = "ops.melt_approval";

fn file(path: &Path) -> NativeEngine {
    NativeEngine::open(path, StoreOptions::default()).unwrap()
}

/// The melt cell's store on `path`, opened alone.
fn store(path: &Path) -> CellStore<NativeEngine> {
    melt_store_on(
        file(path),
        DERIVES_LOSS,
        norn_api::action_admission::prepare_action_model,
    )
}

/// Install the approval gating the melt action, deciders `admin`.
fn gate<E: SqlEngine>(cell: &Cell<E>) {
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

/// The melt object as the operator reads it: its status and document.
fn melt<E: SqlEngine>(cell: &Cell<E>) -> (u16, Value) {
    call(
        cell,
        "GET",
        "/v1/objects/constitution.Melt/melt-7",
        OPERATOR,
        "",
    )
}

/// The operator's melt call with key `key` and fine input `input`.
fn melt_call<E: SqlEngine>(cell: &Cell<E>, input: &str, key: &str) -> (u16, Value) {
    call(
        cell,
        "POST",
        &format!("/v1/actions/{MELT_ACTION}"),
        OPERATOR,
        &melt_body("melt-7", input, "10", key),
    )
}

/// The requester calls again with the same key and input: what the cell
/// answers.
fn retry<E: SqlEngine>(cell: &Cell<E>) -> (u16, Value) {
    melt_call(cell, "12", "operator-1")
}

/// A gated cell on `path` with the operator's melt call parked; its
/// decision id.
fn parked(path: &Path) -> String {
    let cell = cell_on(file(path), true);
    gate(&cell);
    let (status, parked) = retry(&cell);
    assert_eq!(status, 202, "{parked}");
    parked["decision"]["id"].as_str().unwrap().to_owned()
}

fn decide_with<E: SqlEngine>(cell: &Cell<E>, id: &str, key: &str) -> (u16, Value) {
    call(
        cell,
        "POST",
        &format!("/v1/decisions/{id}:decide"),
        APPROVER,
        &json!({ "choice": "approve", "idempotencyKey": key }).to_string(),
    )
}

/// The approver approves `id` (or replays that approval).
fn approve<E: SqlEngine>(cell: &Cell<E>, id: &str) -> (u16, Value) {
    decide_with(cell, id, "approve-1")
}

/// Where the approved call of `id` stands, read only: the approver's decide
/// with another key answers `decision.resolved` with it, and runs nothing.
fn standing<E: SqlEngine>(cell: &Cell<E>, id: &str) -> Value {
    let (status, resolved) = decide_with(cell, id, "look-again");
    assert_eq!(status, 409, "{resolved}");
    assert_eq!(resolved["code"], json!("decision.resolved"), "{resolved}");
    resolved["resolution"]["call"].clone()
}

/// How many commits the cell on `path` holds, read from the file.
fn commit_count(path: &Path) -> usize {
    file(path)
        .query(r#"SELECT 1 FROM "_norn_receipt""#, &[])
        .unwrap()
        .len()
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
    Some((resolved(path, id), melt(&reopened).0 == 200))
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

/// Every crash point of the decide whose aftermath is `want` (approval
/// recorded, melt committed), each on a fresh copy of the call parked by
/// `park`: the cell and the decision.
fn crashes_leaving(
    directory: &Path,
    want: (bool, bool),
    park: fn(&Path) -> String,
) -> Vec<(PathBuf, String)> {
    let mut found = Vec::new();
    for at in 0..64 {
        let path = directory.join(format!("cell-{at}.sqlite"));
        let id = park(&path);
        match crash_at(&path, &id, at) {
            Some(left) if left == want => found.push((path, id)),
            Some(_) => {}
            None => break,
        }
    }
    assert!(
        !found.is_empty(),
        "no crash point of the decide leaves {want:?}"
    );
    found
}

/// The first crash point leaving `want` for a direct call.
fn crash_leaving(directory: &Path, want: (bool, bool)) -> (PathBuf, String) {
    crashes_leaving(directory, want, parked).swap_remove(0)
}

fn wake<E: SqlEngine>(cell: &Cell<E>, at: u64) -> DueWork {
    run_due(&cell.host, LogicalTime::from_millis(at)).unwrap()
}

fn not_executed() -> Value {
    json!({ "committed": false, "state": "not_executed" })
}

fn committed() -> Value {
    json!({ "committed": true, "state": "committed" })
}

/// APPROVAL-RECOVERY-BEFORE: crash before the commit. Before #1481 this lost
/// the call silently. Now the reopened cell shows it approved and not
/// executed; the wake runs nothing; the requester's retry commits it once,
/// under its own authority, and every later look and replay answers the
/// same commit.
#[test]
fn an_approved_call_lost_to_a_crash_is_visible_and_the_retry_commits_it_once() {
    let directory = tempfile::tempdir().unwrap();
    let (path, id) = crash_leaving(directory.path(), (true, false));
    let reopened = cell_on(file(&path), false);
    assert_eq!(melt(&reopened).0, 404, "the crash lost the commit");
    assert_eq!(standing(&reopened, &id), not_executed());
    // Nothing is due: no wake ever calls an action as anyone.
    assert_eq!(next_due(&reopened.host).unwrap(), None);
    assert_eq!(wake(&reopened, NOW * 1000 + 1), DueWork::default());
    assert_eq!(melt(&reopened).0, 404);

    let (status, receipt) = retry(&reopened);
    assert_eq!(status, 200, "{receipt}");
    assert_eq!(receipt["outcome"], json!("committed"), "{receipt}");
    assert_eq!(standing(&reopened, &id), committed());
    let before = commit_count_of(&reopened, &path);
    let (status, replayed) = approve(&reopened, &id);
    assert_eq!(status, 200, "{replayed}");
    assert_eq!(replayed["outcome"], json!("replayed"), "{replayed}");
    assert_eq!(replayed["call"], committed(), "{replayed}");
    let (status, again) = retry(&reopened);
    assert_eq!(status, 200, "{again}");
    assert_eq!(again["outcome"], json!("replayed"), "{again}");
    assert_eq!(commit_count_of(&reopened, &path), before, "nothing new");
}

/// [`commit_count`] while `cell` is open on `path`.
fn commit_count_of<E: SqlEngine>(_open: &Cell<E>, path: &Path) -> usize {
    commit_count(path)
}

/// APPROVAL-RECOVERY-REPLAY: crash before the commit; the approver's replay
/// of the decide runs the call once, as its requester, through the
/// approval's fence; the requester's retry then replays it.
#[test]
fn the_approvers_replay_commits_an_approved_call_lost_to_a_crash_once() {
    let directory = tempfile::tempdir().unwrap();
    let (path, id) = crash_leaving(directory.path(), (true, false));
    let reopened = cell_on(file(&path), false);
    let (status, replayed) = approve(&reopened, &id);
    assert_eq!(status, 200, "{replayed}");
    assert_eq!(replayed["outcome"], json!("replayed"), "{replayed}");
    assert_eq!(replayed["call"], committed(), "{replayed}");
    assert_eq!(melt(&reopened).0, 200);
    let before = commit_count_of(&reopened, &path);
    let (status, again) = approve(&reopened, &id);
    assert_eq!(status, 200, "{again}");
    assert_eq!(again["call"], committed(), "{again}");
    let (status, receipt) = retry(&reopened);
    assert_eq!(status, 200, "{receipt}");
    assert_eq!(receipt["outcome"], json!("replayed"), "{receipt}");
    assert_eq!(commit_count_of(&reopened, &path), before, "never twice");
}

/// APPROVAL-RECOVERY-CONFLICT: another call committed under the approved
/// call's idempotency key (same requester and key, other input, approved on
/// its own). The approved call never reads as committed: it is an
/// idempotency conflict, and nothing runs.
#[test]
fn another_call_under_the_approved_key_is_a_conflict_never_a_commit() {
    let directory = tempfile::tempdir().unwrap();
    let (path, id) = crash_leaving(directory.path(), (true, false));
    let reopened = cell_on(file(&path), false);
    let (status, other) = melt_call(&reopened, "11", "operator-1");
    assert_eq!(status, 202, "{other}");
    let other_id = other["decision"]["id"].as_str().unwrap().to_owned();
    assert_ne!(other_id, id);
    let (status, decided) = decide_with(&reopened, &other_id, "approve-other");
    assert_eq!(status, 200, "{decided}");
    assert_eq!(decided["call"], committed(), "{decided}");

    let conflict = json!({ "committed": false, "code": "action.idempotency_conflict",
        "state": "refused" });
    assert_eq!(standing(&reopened, &id), conflict);
    let before = commit_count_of(&reopened, &path);
    let (status, replayed) = approve(&reopened, &id);
    assert_eq!(status, 200, "{replayed}");
    assert_eq!(replayed["call"], conflict, "{replayed}");
    assert_eq!(commit_count_of(&reopened, &path), before, "nothing ran");
    let (_status, object) = melt(&reopened);
    assert_eq!(object["values"]["input_fine_g"], json!(11), "{object}");
}

/// Install the melt definitions `set` as the next version at `at`.
fn install_set<E: SqlEngine>(cell: &Cell<E>, set: &str, at: u64) {
    let version = cell.host.active_definitions_version().unwrap();
    let (model, input) = step_cell::action_cell::melt_install(
        set,
        norn_api::action_admission::prepare_action_model,
        at,
    );
    cell.host
        .install_definitions(model, input, version)
        .expect("the next revision installs");
}

/// Export every commit of the cell on `path` and restore it to `target`.
fn restore(path: &Path, target: &Path, directory: &Path) {
    let source = store(path);
    let model = source.model().clone();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    runtime.block_on(async {
        let mut exporter = Exporter::local(&directory.join("export")).unwrap();
        for seq in 1..=source.export_commit_count().unwrap() {
            exporter
                .append(&source.export_record(seq).unwrap().unwrap())
                .await
                .unwrap();
        }
        let manifest = exporter.verify().await.unwrap();
        CellStore::restore_from(
            &mut exporter,
            target,
            model,
            "org_demo/dev",
            &manifest.bindings,
            StoreOptions::default(),
        )
        .await
        .unwrap();
    });
}

/// APPROVAL-RECOVERY-REFUSED (with APPROVAL-REVISION-1): the approved call
/// is refused when it runs (its action changed after it parked), and the
/// refusal is final: once the action is back as it was, neither the
/// requester's retry nor the approver's replay runs it, and a cell restored
/// from the export still holds the refusal.
#[test]
fn a_recorded_refusal_stays_final_after_the_cause_clears_and_after_a_restore() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cell.sqlite");
    let id = parked(&path);
    let cell = cell_on(file(&path), false);
    install_set(&cell, step_cell::action_cell::OMITS_LOSS, NOW * 1000 + 1);
    let refused = json!({ "committed": false, "code": "approval.definition_changed",
        "state": "refused" });
    let (status, decided) = approve(&cell, &id);
    assert_eq!(status, 200, "{decided}");
    assert_eq!(decided["call"], refused, "{decided}");

    // The cause clears: the action is defined as it was when the call parked.
    install_set(&cell, DERIVES_LOSS, NOW * 1000 + 2);
    let (status, again) = retry(&cell);
    assert_eq!(status, 409, "{again}");
    assert_eq!(
        again["code"],
        json!("approval.definition_changed"),
        "{again}"
    );
    let (status, replayed) = approve(&cell, &id);
    assert_eq!(status, 200, "{replayed}");
    assert_eq!(replayed["call"], refused, "{replayed}");
    assert_eq!(melt(&cell).0, 404);
    drop(cell);

    let target = directory.path().join("restored.sqlite");
    restore(&path, &target, directory.path());
    let restored = cell_on(file(&target), false);
    assert_eq!(standing(&restored, &id), refused);
    let (status, replayed) = approve(&restored, &id);
    assert_eq!(status, 200, "{replayed}");
    assert_eq!(replayed["call"], refused, "{replayed}");
    let (status, again) = retry(&restored);
    assert_eq!(status, 409, "{again}");
    assert_eq!(
        again["code"],
        json!("approval.definition_changed"),
        "{again}"
    );
    assert_eq!(melt(&restored).0, 404, "the restore never runs it");
    assert_eq!(next_due(&restored.host).unwrap(), None);
}

/// A gated cell on `path` whose melt run's convert step parked its call
/// (its ask approved); the call's decision id.
fn parked_step(path: &Path) -> String {
    let cell = cell_on(file(path), true);
    gate(&cell);
    let (status, opened) = call(
        &cell,
        "POST",
        "/v1/runs",
        OPERATOR,
        &json!({ "flow": FLOW_ID, "runId": "melt-run" }).to_string(),
    );
    assert_eq!(status, 201, "{opened}");
    let ask = pending_ids(&cell);
    assert_eq!(ask.len(), 1, "{ask:?}");
    let (status, decided) = approve(&cell, &ask[0]);
    assert_eq!(status, 200, "{decided}");
    assert_eq!(decided["run"]["state"], json!("awaiting_perform_convert"));
    let approval = pending_ids(&cell);
    assert_eq!(approval.len(), 1, "{approval:?}");
    approval[0].clone()
}

/// The decisions the approver may decide.
fn pending_ids<E: SqlEngine>(cell: &Cell<E>) -> Vec<String> {
    let query = format!("{SCOPE}&state=pending");
    let (status, page) = call_with(cell, "GET", "/v1/decisions", &query, APPROVER, "");
    assert_eq!(status, 200, "{page}");
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["id"].as_str().unwrap().to_owned())
        .collect()
}

fn run_state<E: SqlEngine>(cell: &Cell<E>) -> Value {
    let (status, run) = call_with(cell, "GET", "/v1/runs/melt-run", SCOPE, OPERATOR, "");
    assert_eq!(status, 200, "{run}");
    run["state"].clone()
}

/// APPROVAL-RECOVERY-STEP-BEFORE: a flow step's approved call is lost to a
/// crash before it commits. The step waits, approved and not executed, and
/// the wake calls nothing; the approver's replay runs it once and the run
/// completes.
#[test]
fn an_approved_step_call_lost_before_its_commit_runs_once_on_the_approvers_replay() {
    let directory = tempfile::tempdir().unwrap();
    let (path, id) = crashes_leaving(directory.path(), (true, false), parked_step).swap_remove(0);
    let reopened = cell_on(file(&path), false);
    assert_eq!(run_state(&reopened), json!("awaiting_perform_convert"));
    assert_eq!(standing(&reopened, &id), not_executed());
    assert_eq!(next_due(&reopened.host).unwrap(), None);
    assert_eq!(wake(&reopened, NOW * 1000 + 1), DueWork::default());
    assert_eq!(melt(&reopened).0, 404);

    let (status, replayed) = approve(&reopened, &id);
    assert_eq!(status, 200, "{replayed}");
    assert_eq!(replayed["call"], committed(), "{replayed}");
    assert_eq!(melt(&reopened).0, 200);
    assert_eq!(run_state(&reopened), json!("completed"));
}

/// APPROVAL-RECOVERY-STEP-AFTER: a flow step's approved call committed, and
/// the host died before its run took the outcome (before the result was
/// recorded, or after it was recorded and before the run consumed it). The
/// wake delivers the recorded outcome, durably, without calling the action:
/// one new commit, the run's result turn.
#[test]
fn a_committed_step_call_lost_before_its_delivery_moves_its_run_on_the_wake() {
    let directory = tempfile::tempdir().unwrap();
    let crashes = crashes_leaving(directory.path(), (true, true), parked_step);
    let mut delivered = 0;
    for (path, id) in crashes {
        let reopened = cell_on(file(&path), false);
        if run_state(&reopened) == json!("completed") {
            continue;
        }
        delivered += 1;
        assert_eq!(standing(&reopened, &id), committed());
        let (_status, before) = melt(&reopened);
        let due = next_due(&reopened.host)
            .unwrap()
            .expect("the outcome is due");
        let count = commit_count_of(&reopened, &path);
        let woke = wake(&reopened, due.max(NOW * 1000));
        assert_eq!(woke.steps, 1, "{woke:?}");
        assert!(woke.failed.is_empty(), "{woke:?}");
        assert_eq!(run_state(&reopened), json!("completed"));
        assert_eq!(melt(&reopened).1, before, "the action never ran again");
        assert_eq!(commit_count_of(&reopened, &path), count + 1);
        assert_eq!(next_due(&reopened.host).unwrap(), None);
        assert_eq!(wake(&reopened, due.max(NOW * 1000)), DueWork::default());
    }
    // Both boundaries: before the step's result is recorded, and after it
    // is recorded but before the run consumed it.
    assert!(
        delivered >= 2,
        "{delivered} crash points leave the outcome undelivered"
    );
}
