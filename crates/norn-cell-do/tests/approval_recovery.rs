//! APPROVAL-RECOVERY-1 (#1481, PARKED-CRASH for hosted approvals): the host
//! dies after an approval is recorded but before the approved call commits,
//! or after the call commits but before its run takes the outcome. Nothing
//! runs a call without the requester's own request: the call stays visibly
//! approved and not executed until the requester's retry runs it, exactly
//! once, under the requester's current authority, and a run takes a
//! recorded outcome from the wake or the drive without the action being
//! called again. A refusal is recorded on the ledger, with its category, and
//! stays final, also on a restored cell and after an in-place upgrade.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "dispatch correspondence"
)]

use std::path::{Path, PathBuf};

use super::step_cell::{
    APPROVER, Cell, DERIVES_LOSS, FLOW_ID, MELT_ACTION, NAMES, NOW, OPERATOR, REVOKED, SCOPE, call,
    call_with, cell_on, melt_body, melt_store_on,
};
use norn_api::timers::{DueWork, next_due, run_due};
use norn_cell::engine::{
    EngineFault, EngineFaultKind, FaultEngine, FaultSwitch, NativeEngine, SqlEngine,
};
use norn_cell::export::Exporter;
use norn_cell::{CellStore, MAIN_WORLD, StoreOptions};
use norn_cell_do::principal::FixturePrincipals;
use norn_kernel::LogicalTime;
use serde_json::{Value, json};

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

type Faulty = FaultEngine<NativeEngine>;

/// What `act` on the cell crashed at transaction `at` left on disk: whether
/// the approval of `id` is recorded, and whether the melt committed; `None`
/// once `at` lies past `act`'s last transaction.
fn crash_at(
    path: &Path,
    id: &str,
    at: u32,
    act: &dyn Fn(&Cell<Faulty>, &str),
) -> Option<(bool, bool)> {
    let faults = FaultSwitch::default();
    let cell = cell_on(FaultEngine::new(file(path), faults.clone()), false);
    faults.arm(EngineFault {
        kind: EngineFaultKind::Crash,
        transaction: at,
        at_statement: 0,
    });
    act(&cell, id);
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

/// Every crash point of `act` whose aftermath is `want` (approval recorded,
/// melt committed), each on a fresh copy of the cell `park` leaves: the
/// cell and the decision.
fn crashes_leaving(
    directory: &Path,
    want: (bool, bool),
    park: fn(&Path) -> String,
    act: &dyn Fn(&Cell<Faulty>, &str),
) -> Vec<(PathBuf, String)> {
    let mut found = Vec::new();
    for at in 0..64 {
        let path = directory.join(format!("cell-{at}.sqlite"));
        let id = park(&path);
        match crash_at(&path, &id, at, act) {
            Some(left) if left == want => found.push((path, id)),
            Some(_) => {}
            None => break,
        }
    }
    assert_ne!(found, [], "no crash point leaves {want:?}");
    found
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

/// A gated cell on `path` with the operator's melt call parked and
/// approved: the approval is recorded, and nothing ran. The decision id.
fn approved(path: &Path) -> String {
    let id = parked(path);
    let cell = cell_on(file(path), false);
    let (status, decided) = approve(&cell, &id);
    assert_eq!(status, 200, "{decided}");
    assert_eq!(decided["call"], not_executed(), "{decided}");
    id
}

/// APPROVAL-RECOVERY-BEFORE: the host stops after the approval is recorded
/// and before anything runs the call. Before #1481 the call was lost
/// silently. The reopened cell shows it approved and not executed; the wake
/// runs nothing; the requester's retry commits it once, under its own
/// authority, and every later look and replay answers the same commit.
#[test]
fn an_approved_call_lost_to_a_restart_is_visible_and_the_retry_commits_it_once() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cell.sqlite");
    let id = approved(&path);
    let reopened = cell_on(file(&path), false);
    assert_eq!(melt(&reopened).0, 404);
    assert_eq!(standing(&reopened, &id), not_executed());
    // Nothing is due: no wake ever calls an action as anyone.
    assert_eq!(next_due(&reopened.host).unwrap(), None);
    assert_eq!(wake(&reopened, NOW * 1000 + 1), DueWork::default());
    assert_eq!(melt(&reopened).0, 404);

    let (status, receipt) = retry(&reopened);
    assert_eq!(status, 200, "{receipt}");
    assert_eq!(receipt["outcome"], json!("committed"), "{receipt}");
    assert_eq!(standing(&reopened, &id), committed());
    let before = commit_count(&path);
    let (status, replayed) = approve(&reopened, &id);
    assert_eq!(status, 200, "{replayed}");
    assert_eq!(replayed["outcome"], json!("replayed"), "{replayed}");
    assert_eq!(replayed["call"], committed(), "{replayed}");
    let (status, again) = retry(&reopened);
    assert_eq!(status, 200, "{again}");
    assert_eq!(again["outcome"], json!("replayed"), "{again}");
    assert_eq!(commit_count(&path), before, "nothing new");
}

/// APPROVAL-RECOVERY-REPLAY (S2): the approver's replay of the decide never
/// runs the call: the approver's request cannot carry the requester's
/// current authority. The call stays approved and not executed until the
/// requester's own retry commits it, once.
#[test]
fn the_approvers_replay_never_runs_an_approved_call() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cell.sqlite");
    let id = approved(&path);
    let reopened = cell_on(file(&path), false);
    let (status, replayed) = approve(&reopened, &id);
    assert_eq!(status, 200, "{replayed}");
    assert_eq!(replayed["outcome"], json!("replayed"), "{replayed}");
    assert_eq!(replayed["call"], not_executed(), "{replayed}");
    assert_eq!(melt(&reopened).0, 404);
    let (status, receipt) = retry(&reopened);
    assert_eq!(status, 200, "{receipt}");
    let before = commit_count(&path);
    let (status, again) = approve(&reopened, &id);
    assert_eq!(status, 200, "{again}");
    assert_eq!(again["call"], committed(), "{again}");
    assert_eq!(commit_count(&path), before, "never twice");
}

/// The requester's melt reads, as a holder of a grant.
fn melt_as_approver<E: SqlEngine>(cell: &Cell<E>) -> u16 {
    call(
        cell,
        "GET",
        "/v1/objects/constitution.Melt/melt-7",
        APPROVER,
        "",
    )
    .0
}

/// APPROVAL-RECOVERY-REVOKED (S2): the requester's roles are revoked after
/// the call parked. Neither the approval nor its replay runs the call with
/// the roles recorded when it parked, and the requester's own retry is
/// decided under its current authority: denied, and not recorded, so the
/// call stays approved and not executed.
#[test]
fn a_requester_revoked_after_parking_never_gets_the_call_run() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cell.sqlite");
    let id = parked(&path);
    let mut cell = cell_on(file(&path), false);
    cell.principals = FixturePrincipals::new(REVOKED, std::sync::Arc::clone(&cell.caller));
    let (status, decided) = approve(&cell, &id);
    assert_eq!(status, 200, "{decided}");
    assert_eq!(decided["call"], not_executed(), "{decided}");
    let (status, replayed) = approve(&cell, &id);
    assert_eq!(status, 200, "{replayed}");
    assert_eq!(replayed["call"], not_executed(), "{replayed}");
    assert_eq!(melt_as_approver(&cell), 404, "nothing ran on the old roles");
    let (status, denied) = retry(&cell);
    assert_eq!(status, 403, "{denied}");
    assert_eq!(melt_as_approver(&cell), 404);
    assert_eq!(standing(&cell, &id), not_executed());
}

/// APPROVAL-RECOVERY-CONFLICT: another call committed under the approved
/// call's idempotency key (same requester and key, other input, approved on
/// its own). The approved call never reads as committed: it is an
/// idempotency conflict, and nothing runs.
#[test]
fn another_call_under_the_approved_key_is_a_conflict_never_a_commit() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cell.sqlite");
    let id = approved(&path);
    let reopened = cell_on(file(&path), false);
    let (status, other) = melt_call(&reopened, "11", "operator-1");
    assert_eq!(status, 202, "{other}");
    let other_id = other["decision"]["id"].as_str().unwrap().to_owned();
    assert_ne!(other_id, id);
    let (status, decided) = decide_with(&reopened, &other_id, "approve-other");
    assert_eq!(status, 200, "{decided}");
    let (status, receipt) = melt_call(&reopened, "11", "operator-1");
    assert_eq!(status, 200, "{receipt}");

    let conflict = json!({ "committed": false, "code": "action.idempotency_conflict",
        "state": "refused" });
    assert_eq!(standing(&reopened, &id), conflict);
    let before = commit_count(&path);
    let (status, replayed) = approve(&reopened, &id);
    assert_eq!(status, 200, "{replayed}");
    assert_eq!(replayed["call"], conflict, "{replayed}");
    assert_eq!(commit_count(&path), before, "nothing ran");
    let (_status, object) = melt(&reopened);
    assert_eq!(object["values"]["input_fine_g"], json!(11), "{object}");
}

/// Export every commit of the cell on `path` and restore it to `target`.
fn restore(path: &Path, target: &Path, directory: &Path) {
    let source = store(path);
    let model = source.model().clone();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    runtime.block_on(async {
        let export = directory.join(format!(
            "export-{}",
            target.file_stem().unwrap().to_string_lossy()
        ));
        let mut exporter = Exporter::local(&export).unwrap();
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

/// APPROVAL-RECOVERY-REFUSED (S11): the approved call is refused when the
/// requester's retry runs it (another call created the melt first), and the
/// refusal is recorded with the category it was served with: every later
/// retry, and a cell restored from the export, answers exactly that
/// refusal, its status, code and category, and never runs the call.
#[test]
fn a_recorded_refusal_keeps_its_category_and_stays_final_after_a_restore() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cell.sqlite");
    let id = approved(&path);
    let cell = cell_on(file(&path), false);
    // Another call (its own key and input) creates the melt first.
    let (status, other) = melt_call(&cell, "11", "operator-other");
    assert_eq!(status, 202, "{other}");
    let other_id = other["decision"]["id"].as_str().unwrap().to_owned();
    let (status, _decided) = decide_with(&cell, &other_id, "approve-other");
    assert_eq!(status, 200);
    let (status, created) = melt_call(&cell, "11", "operator-other");
    assert_eq!(status, 200, "{created}");

    let (status, refused) = retry(&cell);
    assert!(status >= 400, "{refused}");
    let code = refused["code"].clone();
    let category = refused["category"].clone();
    let recorded = json!({ "committed": false, "code": code, "state": "refused" });
    assert_eq!(standing(&cell, &id), recorded);
    let (again_status, again) = retry(&cell);
    assert_eq!(
        (again_status, &again["code"], &again["category"]),
        (status, &code, &category),
        "{again}"
    );
    drop(cell);

    let target = directory.path().join("restored.sqlite");
    restore(&path, &target, directory.path());
    let restored = cell_on(file(&target), false);
    assert_eq!(standing(&restored, &id), recorded);
    let (restored_status, replayed) = retry(&restored);
    assert_eq!(
        (restored_status, &replayed["code"], &replayed["category"]),
        (status, &code, &category),
        "{replayed}"
    );
    let (_status, object) = melt(&restored);
    assert_eq!(
        object["values"]["input_fine_g"],
        json!(11),
        "the call never ran"
    );
    assert_eq!(next_due(&restored.host).unwrap(), None);
}

/// The gate's next revision: the same approval, asking another question.
fn revise_gate<E: SqlEngine>(cell: &Cell<E>) {
    revise_gate_from(cell, 1);
}

/// The gate's revision after `revision`, asking a question of its own.
fn revise_gate_from<E: SqlEngine>(cell: &Cell<E>, revision: u64) {
    let question = if revision == 1 {
        "Convert this melt now?".to_owned()
    } else {
        format!("Convert this melt now ({revision})?")
    };
    let mut body = json!({ "kind": "approval", "id": APPROVAL_ID, "payload": {
        "action": MELT_ACTION, "decider": { "roles": ["admin"] },
        "question": question, "risk": "high" } });
    let (status, admitted) = call(
        cell,
        "POST",
        "/v1/definitions:admit",
        OPERATOR,
        &body.to_string(),
    );
    assert_eq!(status, 200, "{admitted}");
    body["signoff"] = json!({ "restatementSha256": admitted["restatementSha256"] });
    body["expectedVersion"] = json!(revision);
    let (status, installed) = call(
        cell,
        "POST",
        "/v1/definitions:install",
        OPERATOR,
        &body.to_string(),
    );
    assert_eq!(status, 200, "{installed}");
}

/// APPROVAL-RECOVERY-UPGRADE (S9): a cell whose approval was recorded
/// before approved calls were (its file has no `_norn_approved_call`) is
/// upgraded in place when it opens: the approval carries its call exactly
/// as an export restore of the same ledger rebuilds it, before and after
/// the gate's next revision, and the revision's fence still holds (the
/// requester's retry parks anew instead of running under the old approval).
#[test]
fn an_in_place_upgrade_backfills_approved_calls_as_a_restore_does() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cell.sqlite");
    let id = approved(&path);
    // The file as a host before APPROVAL-RECOVERY-1 left it.
    file(&path)
        .execute_batch(r#"DROP TABLE "_norn_approved_call""#)
        .unwrap();
    let upgraded = cell_on(file(&path), false);
    let target = directory.path().join("restored.sqlite");
    restore(&path, &target, directory.path());
    let restored = cell_on(file(&target), false);
    assert_eq!(standing(&upgraded, &id), not_executed());
    assert_eq!(standing(&restored, &id), standing(&upgraded, &id));

    revise_gate(&upgraded);
    revise_gate(&restored);
    for cell in [&upgraded, &restored] {
        assert_eq!(standing(cell, &id), not_executed());
        let (status, again) = retry(cell);
        assert_eq!(status, 202, "{again}");
        assert_ne!(again["decision"]["id"], json!(id), "{again}");
        assert_eq!(melt(cell).0, 404, "never under the old gate revision");
    }
}

/// A gated cell on `path` whose melt run's convert step parked its call
/// (its ask approved); the call's decision id.
fn parked_step(path: &Path) -> String {
    let cell = cell_on(file(path), true);
    gate(&cell);
    let (status, opened) = start(&cell);
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

/// [`parked_step`] with the step's call approved: nothing ran.
fn approved_step(path: &Path) -> String {
    let id = parked_step(path);
    let cell = cell_on(file(path), false);
    let (status, decided) = approve(&cell, &id);
    assert_eq!(status, 200, "{decided}");
    assert_eq!(decided["call"], not_executed(), "{decided}");
    assert_eq!(decided["run"]["state"], json!("awaiting_perform_convert"));
    id
}

/// The operator starts the melt run, or retries that start: the exact
/// retry drives the run under the starter's own authority.
fn start<E: SqlEngine>(cell: &Cell<E>) -> (u16, Value) {
    call(
        cell,
        "POST",
        "/v1/runs",
        OPERATOR,
        &json!({ "flow": FLOW_ID, "runId": "melt-run" }).to_string(),
    )
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
    let (status, run) = call_with(cell, "GET", "/v1/runs/melt-run", SCOPE, APPROVER, "");
    assert_eq!(status, 200, "{run}");
    run["state"].clone()
}

/// APPROVAL-RECOVERY-STEP-BEFORE: a flow step's approved call waits for its
/// requester (the run's starter) across a restart. The wake calls nothing,
/// the approver's replay runs nothing, and the starter's retry of the run
/// runs it once and the run completes.
#[test]
fn an_approved_step_call_runs_once_on_the_starters_retry() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cell.sqlite");
    let id = approved_step(&path);
    let reopened = cell_on(file(&path), false);
    assert_eq!(run_state(&reopened), json!("awaiting_perform_convert"));
    assert_eq!(standing(&reopened, &id), not_executed());
    assert_eq!(next_due(&reopened.host).unwrap(), None);
    assert_eq!(wake(&reopened, NOW * 1000 + 1), DueWork::default());
    let (status, replayed) = approve(&reopened, &id);
    assert_eq!(status, 200, "{replayed}");
    assert_eq!(replayed["call"], not_executed(), "{replayed}");
    assert_eq!(melt(&reopened).0, 404);

    let (status, run) = start(&reopened);
    assert_eq!(status, 200, "{run}");
    assert_eq!(run["state"], json!("completed"), "{run}");
    assert_eq!(melt(&reopened).0, 200);
    assert_eq!(standing(&reopened, &id), committed());
}

/// The starter's retry, crashed: the action for [`crashes_leaving`].
fn restart_crashing(cell: &Cell<Faulty>, _id: &str) {
    let _answer = start(cell);
}

/// The crash points of the starter's retry that leave the step's call
/// committed and its run not yet past it: before the commit that records and
/// consumes the step's result (P3: one transaction, so no crash point lies
/// between recording and consuming it).
fn undelivered(directory: &Path) -> Vec<(PathBuf, String)> {
    let crashes = crashes_leaving(directory, (true, true), approved_step, &restart_crashing);
    let undelivered: Vec<_> = crashes
        .into_iter()
        .filter(|(path, _id)| run_state(&cell_on(file(path), false)) != json!("completed"))
        .collect();
    assert!(
        !undelivered.is_empty(),
        "{} crash points leave the outcome undelivered",
        undelivered.len()
    );
    undelivered
}

/// APPROVAL-RECOVERY-STEP-AFTER: a flow step's approved call committed, and
/// the host died before its run took the outcome (before the result was
/// recorded, or after it was recorded and before the run consumed it). The
/// wake delivers the recorded outcome, durably, without calling the action:
/// one new commit, the run's result turn.
#[test]
fn a_committed_step_call_lost_before_its_delivery_moves_its_run_on_the_wake() {
    let directory = tempfile::tempdir().unwrap();
    for (path, id) in undelivered(directory.path()) {
        let reopened = cell_on(file(&path), false);
        assert_eq!(standing(&reopened, &id), committed());
        let (_status, before) = melt(&reopened);
        let due = next_due(&reopened.host)
            .unwrap()
            .expect("the outcome is due");
        let count = commit_count(&path);
        let woke = wake(&reopened, due.max(NOW * 1000));
        assert_eq!(woke.steps, 1, "{woke:?}");
        assert_eq!(woke.failed.len(), 0, "{woke:?}");
        assert_eq!(run_state(&reopened), json!("completed"));
        assert_eq!(melt(&reopened).1, before, "the action never ran again");
        assert_eq!(commit_count(&path), count + 1);
        assert_eq!(next_due(&reopened.host).unwrap(), None);
        assert_eq!(wake(&reopened, due.max(NOW * 1000)), DueWork::default());
    }
}

/// S5b: the drive delivers a step's outcome the same way the wake does,
/// also a result recorded before the crash and not yet consumed: the
/// approver's replay drives the run to completion without calling the
/// action again.
#[test]
fn a_drive_delivers_a_step_outcome_recorded_before_a_crash() {
    let directory = tempfile::tempdir().unwrap();
    for (path, id) in undelivered(directory.path()) {
        let reopened = cell_on(file(&path), false);
        let (_status, before) = melt(&reopened);
        let count = commit_count(&path);
        let (status, replayed) = approve(&reopened, &id);
        assert_eq!(status, 200, "{replayed}");
        assert_eq!(replayed["call"], committed(), "{replayed}");
        assert_eq!(replayed["run"]["state"], json!("completed"), "{replayed}");
        assert_eq!(melt(&reopened).1, before, "the action never ran again");
        assert_eq!(commit_count(&path), count + 1, "one result turn");
        assert_eq!(next_due(&reopened.host).unwrap(), None);
    }
}

/// The parked step command and the approved call's idempotency key, read
/// from the cell's outbox.
fn step_and_key(path: &Path) -> ((String, u64), String) {
    let store = store(path);
    let mut step = None;
    let mut key = None;
    for pending in store.read().parked_commands().unwrap() {
        let payload = pending.command.command.get("payload").unwrap();
        if payload
            .get("function")
            .and_then(norn_ir::RuntimeValue::as_str)
            == Some(norn_cell::flow_run::ACTION_FUNCTION)
        {
            step = Some((
                pending.command.command_id.clone(),
                pending.command.generation,
            ));
        }
        if let Some(found) = payload
            .get("approval")
            .and_then(|approval| approval.get("idempotencyKey"))
            .and_then(norn_ir::RuntimeValue::as_str)
        {
            key = Some(found.to_owned());
        }
    }
    (step.unwrap(), key.unwrap())
}

/// S5a: a flow step's call key is the host's own identity for that step
/// attempt's call. A direct call under it (the starter's, with the step's
/// exact input, unpinned or pinned, or with another input) is refused before
/// anything parks, consumes an approval or commits, before and after the
/// step's call is approved: it never supplies the step's outcome. The step's
/// own call, run by its requester's retry of the run, does.
#[test]
fn a_direct_call_with_a_steps_key_never_supplies_the_steps_outcome() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cell.sqlite");
    let id = parked_step(&path);
    let (_step, key) = step_and_key(&path);
    let cell = cell_on(file(&path), false);
    let version = cell.host.active_definitions_version().unwrap().unwrap();
    let pinned = format!("{SCOPE}&version={version}");
    let refused_every_way = || {
        for (query, input) in [(SCOPE, "12"), (pinned.as_str(), "12"), (SCOPE, "11")] {
            let (status, refused) = call_with(
                &cell,
                "POST",
                &format!("/v1/actions/{MELT_ACTION}"),
                query,
                OPERATOR,
                &melt_body("melt-7", input, "10", &key),
            );
            assert_eq!(status, 400, "{refused}");
            assert_eq!(refused["code"], json!("request.invalid"), "{refused}");
        }
    };
    refused_every_way();
    assert_eq!(pending_ids(&cell), vec![id.clone()], "nothing parked");
    let (status, decided) = approve(&cell, &id);
    assert_eq!(status, 200, "{decided}");
    assert_eq!(decided["call"], not_executed(), "{decided}");
    refused_every_way();
    assert_eq!(melt(&cell).0, 404, "nothing committed");
    assert_eq!(standing(&cell, &id), not_executed());
    assert_eq!(wake(&cell, NOW * 1000 + 1), DueWork::default());
    assert_eq!(run_state(&cell), json!("awaiting_perform_convert"));

    let (status, run) = start(&cell);
    assert_eq!(status, 200, "{run}");
    assert_eq!(run["state"], json!("completed"), "{run}");
    assert_eq!(standing(&cell, &id), committed());
}

/// The melt run started on an ungated cell on `path`, its ask open: the
/// ask's decision id.
fn asked(path: &Path) -> String {
    let cell = cell_on(file(path), true);
    let (status, opened) = start(&cell);
    assert_eq!(status, 201, "{opened}");
    let ask = pending_ids(&cell);
    assert_eq!(ask.len(), 1, "{ask:?}");
    ask[0].clone()
}

/// The approver approves the ask, crashed: the action for
/// [`crashes_leaving`].
fn approve_crashing(cell: &Cell<Faulty>, id: &str) {
    let _answer = approve(cell, id);
}

/// Whether the cell on `path` recorded a result for its run's action step.
fn step_result_recorded(path: &Path) -> bool {
    store(path)
        .read()
        .parked_commands()
        .unwrap()
        .into_iter()
        .find(|pending| {
            pending
                .command
                .command
                .get("payload")
                .and_then(|payload| payload.get("function"))
                .and_then(norn_ir::RuntimeValue::as_str)
                == Some(norn_cell::flow_run::ACTION_FUNCTION)
        })
        .is_some_and(|step| step.result.is_some())
}

/// The file on `path` as a host before step ownership was recorded left
/// it: no commit names the step attempt that made it, so the record under
/// the step's call key is exactly what a direct call under that key, made
/// before the key was reserved, would have left.
fn before_ownership(path: &Path) {
    file(path)
        .execute_batch(r#"DROP TABLE IF EXISTS "_norn_step_call""#)
        .unwrap();
}

/// The melt run's convert step as the approver reads it: its status.
fn convert_status<E: SqlEngine>(cell: &Cell<E>) -> Value {
    let (status, run) = call_with(cell, "GET", "/v1/runs/melt-run", SCOPE, APPROVER, "");
    assert_eq!(status, 200, "{run}");
    assert_eq!(run["steps"][1]["id"], json!("convert"), "{run}");
    run["steps"][1]["status"].clone()
}

/// S5a: a record under a step's call key is the step's outcome only when
/// that step attempt's own dispatch committed it, as the ledger records it.
/// The step's call committed and the host died before the step took the
/// outcome, in a file from before ownership was recorded, which a direct
/// call under the step's key would have left the same way. The starter's
/// retry of the run never replays that record as the step's output: the
/// step fails `Refused`, the run never completes on it, and the action
/// never runs again.
#[test]
fn a_record_under_a_steps_key_its_attempt_never_committed_is_never_replayed_as_its_outcome() {
    let directory = tempfile::tempdir().unwrap();
    let crashes = crashes_leaving(directory.path(), (true, true), asked, &approve_crashing);
    let mut tried = 0;
    for (path, _ask) in crashes {
        if run_state(&cell_on(file(&path), false)) == json!("completed")
            || step_result_recorded(&path)
        {
            continue;
        }
        tried += 1;
        before_ownership(&path);
        let cell = cell_on(file(&path), false);
        let (_status, before) = melt(&cell);
        let (status, run) = start(&cell);
        assert_eq!(status, 200, "{run}");
        assert_ne!(run_state(&cell), json!("completed"));
        assert_eq!(convert_status(&cell), json!("failed"));
        assert_eq!(melt(&cell).1, before, "the action never ran again");
    }
    assert!(
        tried > 0,
        "no crash point leaves the call committed and its step pending"
    );
}

/// S5a: an approved step call's commit is the step's outcome on recovery
/// only when the step attempt's own dispatch committed it. The same file
/// from before ownership was recorded: the wake delivers no output for the
/// unowned record; the step fails `Refused` and the action never runs again.
#[test]
fn an_unowned_record_under_a_steps_key_is_never_its_recovered_outcome() {
    let directory = tempfile::tempdir().unwrap();
    let mut tried = 0;
    for (path, _id) in undelivered(directory.path()) {
        if step_result_recorded(&path) {
            continue;
        }
        tried += 1;
        before_ownership(&path);
        let cell = cell_on(file(&path), false);
        let (_status, before) = melt(&cell);
        let due = next_due(&cell.host).unwrap().expect("the outcome is due");
        let woke = wake(&cell, due.max(NOW * 1000));
        assert_eq!(woke.steps, 1, "{woke:?}");
        assert_ne!(run_state(&cell), json!("completed"));
        assert_eq!(convert_status(&cell), json!("failed"));
        assert_eq!(melt(&cell).1, before, "the action never ran again");
    }
    assert!(
        tried > 0,
        "no crash point leaves the step's result unrecorded"
    );
}

/// The step call parked on `path` as a host before attempts were recorded
/// left it: its parked call carries no step attempt.
fn without_attempt(path: &Path) {
    let engine = file(path);
    engine
        .execute_batch(
            r#"UPDATE "_norn_outbox" SET "command"=json_remove("command",'$.payload.approval.attempt') WHERE json_extract("command",'$.payload.approval') IS NOT NULL"#,
        )
        .unwrap();
    let rows = engine
        .query(
            r#"SELECT json_extract("command",'$.payload.approval.attempt') IS NULL FROM "_norn_outbox" WHERE json_extract("command",'$.payload.approval') IS NOT NULL"#,
            &[],
        )
        .unwrap();
    assert_eq!(rows.len(), 1, "one parked call");
    assert_eq!(
        rows[0],
        vec![norn_cell::engine::SqlValue::Integer(1)],
        "its attempt is gone"
    );
}

/// S5a: a direct call held under a step's call key before the key was
/// reserved (its parked call carries no step attempt) never commits
/// through its approval's dispatch, even as its present requester: not
/// while no step waits on the key (the refusal is recorded for the
/// approval), and not while a step does (only that step's own dispatch
/// runs it).
#[test]
fn a_held_direct_call_under_a_steps_key_never_commits_through_its_approval() {
    let directory = tempfile::tempdir().unwrap();
    // No step waits on the key: a direct call's approval.
    let path = directory.path().join("ended.sqlite");
    let id = parked_step(&path);
    let (step, _key) = step_and_key(&path);
    without_attempt(&path);
    end_attempt(&path, &step);
    let cell = cell_on(file(&path), false);
    let dispatched = approve_dispatching(&cell, &id, &|| {});
    let reserved = json!({ "committed": false, "code": "request.invalid", "state": "refused" });
    assert_eq!(dispatched, reserved);
    assert_eq!(melt(&cell).0, 404, "nothing committed");
    assert_eq!(standing(&cell, &id), reserved, "final");

    // A step waits on the key: only its own dispatch runs the call.
    let path = directory.path().join("waiting.sqlite");
    let id = parked_step(&path);
    without_attempt(&path);
    let cell = cell_on(file(&path), false);
    let dispatched = approve_dispatching(&cell, &id, &|| {});
    assert_eq!(dispatched["committed"], json!(false), "{dispatched}");
    assert_eq!(dispatched["state"], json!("not_executed"), "{dispatched}");
    assert_eq!(melt(&cell).0, 404, "nothing committed");
    assert_eq!(standing(&cell, &id), not_executed());
}

/// S6b: a flow step's approval recorded before attempts were (its parked
/// call carries none) is bound, under the dispatch's hold, to its recorded
/// run's pending attempt that makes exactly that call: the starter's
/// ordinary retry of the run consumes it and the run completes. Recorded
/// for another run, no retry of this run consumes it.
#[test]
fn a_step_approval_from_before_attempts_is_consumed_by_its_own_runs_retry_only() {
    let directory = tempfile::tempdir().unwrap();
    for elsewhere in [false, true] {
        let path = directory.path().join(format!("cell-{elsewhere}.sqlite"));
        let id = parked_step(&path);
        without_attempt(&path);
        {
            let cell = cell_on(file(&path), false);
            let (status, decided) = approve(&cell, &id);
            assert_eq!(status, 200, "{decided}");
            assert_eq!(decided["call"], not_executed(), "{decided}");
            assert_eq!(decided["run"]["id"], json!("melt-run"), "{decided}");
        }
        if elsewhere {
            file(&path)
                .execute(
                    r#"UPDATE "_norn_approved_call" SET "run_id"='melt-run-elsewhere' WHERE "decision_id"=?1"#,
                    &[id.as_str().into()],
                )
                .unwrap();
        }
        let cell = cell_on(file(&path), false);
        let (status, run) = start(&cell);
        assert_eq!(status, 200, "{run}");
        if elsewhere {
            assert_ne!(run["state"], json!("completed"), "{run}");
            assert_eq!(melt(&cell).0, 404, "never by another run's retry");
            assert_eq!(standing(&cell, &id), not_executed());
        } else {
            assert_eq!(run["state"], json!("completed"), "{run}");
            assert_eq!(melt(&cell).0, 200);
            assert_eq!(standing(&cell, &id), committed());
        }
    }
}

/// S6b: a denial of authority is decided anew on every try, so it is never
/// the step's outcome while its approval stays executable: the starter's
/// retry under revoked roles leaves the step waiting, and the run moves once
/// the starter's authority holds again.
#[test]
fn a_denied_try_of_an_approved_step_call_leaves_the_step_pending() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cell.sqlite");
    let id = approved_step(&path);
    let mut cell = cell_on(file(&path), false);
    cell.principals = FixturePrincipals::new(REVOKED, std::sync::Arc::clone(&cell.caller));
    let (status, run) = start(&cell);
    assert_eq!(status, 200, "{run}");
    assert_eq!(run_state(&cell), json!("awaiting_perform_convert"));
    assert_eq!(standing(&cell, &id), not_executed());
    drop(cell);

    let cell = cell_on(file(&path), false);
    let (status, run) = start(&cell);
    assert_eq!(status, 200, "{run}");
    assert_eq!(run["state"], json!("completed"), "{run}");
}

/// S6b: an approval runs a step's call only while that exact step attempt
/// waits on it. Once the attempt has its result (here recorded by an
/// earlier host), the approved call reads `approval.attempt_ended`, for
/// good, and nothing runs it: not the starter's retry of the run, not the
/// approver's replay.
#[test]
fn an_approved_step_call_never_runs_after_its_attempt_ended() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cell.sqlite");
    let id = parked_step(&path);
    let (step, _key) = step_and_key(&path);
    {
        let cell = cell_on(file(&path), false);
        let (status, decided) = approve(&cell, &id);
        assert_eq!(status, 200, "{decided}");
        assert_eq!(decided["call"], not_executed(), "{decided}");
    }
    end_attempt(&path, &step);
    let cell = cell_on(file(&path), false);
    assert_eq!(standing(&cell, &id), attempt_ended());
    let _run = start(&cell);
    let (status, replayed) = approve(&cell, &id);
    assert_eq!(status, 200, "{replayed}");
    assert_eq!(replayed["call"], attempt_ended(), "{replayed}");
    assert_eq!(melt(&cell).0, 404);
    assert_eq!(standing(&cell, &id), attempt_ended());
}

/// End the step attempt the parked call on `path` was made for: an earlier
/// host (an engine before P3) recorded its result apart from any consuming
/// commit.
fn end_attempt(path: &Path, (command, generation): &(String, u64)) {
    let ended = norn_ir::RuntimeValue::from_json(json!({ "_tag": "Denied" })).unwrap();
    assert!(
        store(path)
            .record_parked_apart(command, *generation, &ended)
            .unwrap()
    );
}

fn attempt_ended() -> Value {
    json!({ "committed": false, "code": "approval.attempt_ended", "state": "refused" })
}

/// S2: an approval is consumed only by its requester's own authenticated
/// principal, compared under the dispatch's hold: the same subject verified
/// from another role table is another principal, denied without anything
/// recorded, and the call stays approved for the requester.
#[test]
fn a_requester_verified_from_another_role_table_never_consumes_the_approval() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cell.sqlite");
    let id = approved(&path);
    let mut cell = cell_on(file(&path), false);
    cell.principals = FixturePrincipals::new(NAMES, std::sync::Arc::clone(&cell.caller))
        .with_role_table("fixtures/2");
    let (status, denied) = retry(&cell);
    assert_eq!(status, 403, "{denied}");
    assert_eq!(melt(&cell).0, 404, "nothing ran");
    assert_eq!(standing(&cell, &id), not_executed());
    drop(cell);

    let cell = cell_on(file(&path), false);
    let (status, receipt) = retry(&cell);
    assert_eq!(status, 200, "{receipt}");
    assert_eq!(standing(&cell, &id), committed());
}

/// S2, the flow's dispatch: the drive reads the step unapproved and tries
/// its call under the starter's recorded authority; an approval recorded in
/// between is judged under the dispatch's own hold, where that try carries
/// no authenticated principal. It never consumes the approval: nothing
/// runs, and the step stays pending for its requester.
#[test]
fn an_approval_recorded_while_a_step_dispatches_on_recorded_authority_is_never_consumed() {
    use norn_api::actions::ActionAnswer;
    use norn_api::refusal::Refusal;
    use norn_cell::flow_run::RunStarter;
    use norn_cell_do::policy::Caller;
    type Dispatch<'a> = &'a mut dyn FnMut() -> Result<ActionAnswer, Refusal>;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cell.sqlite");
    let id = parked_step(&path);
    let cell = cell_on(file(&path), false);
    let raced = std::cell::Cell::new(false);
    let as_starter = |starter: &RunStarter, dispatch: Dispatch<'_>| {
        // After the drive read the step, before its dispatch holds the cell.
        let (status, decided) = approve(&cell, &id);
        assert_eq!(status, 200, "{decided}");
        raced.set(true);
        cell.caller.record(Some(Caller {
            subject_id: starter.subject.clone(),
            roles: starter.roles.clone(),
            actor_kind: "user",
            role_table: norn_cell_do::principal::FIXTURE_ROLES,
            accountable_person: starter.accountable_person.clone(),
        }));
        let answer = dispatch();
        cell.caller.record(None);
        Some(answer)
    };
    let as_requester = |_requester: &RunStarter, _dispatch: Dispatch<'_>| None;
    norn_api::flow_driver::drive_run(
        &cell.host,
        "melt-run",
        &as_starter,
        &as_requester,
        &cell.handles,
        LogicalTime::from_millis(NOW * 1000 + 5),
    )
    .unwrap();
    assert!(raced.get());
    assert_eq!(melt(&cell).0, 404, "never on the starter's recorded roles");
    assert_eq!(run_state(&cell), json!("awaiting_perform_convert"));
    assert_eq!(standing(&cell, &id), not_executed());
}

/// S6b: a step's call carries its exact attempt from the moment it parks.
/// An attempt that ended before anyone decided still bounds the approval:
/// the approved call reads `approval.attempt_ended`, for good, and never
/// runs.
#[test]
fn an_attempt_that_ended_before_the_decision_never_gets_its_call_run() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cell.sqlite");
    let id = parked_step(&path);
    let (step, _key) = step_and_key(&path);
    end_attempt(&path, &step);
    let cell = cell_on(file(&path), false);
    let (status, decided) = approve(&cell, &id);
    assert_eq!(status, 200, "{decided}");
    assert_eq!(decided["call"], attempt_ended(), "{decided}");
    let _run = start(&cell);
    assert_eq!(melt(&cell).0, 404);
    assert_eq!(standing(&cell, &id), attempt_ended());
}

/// The approver approves `id` through the decide service, and `before` runs
/// after the approval is recorded and before its call dispatches as its
/// requester: what the decide answers for the call.
fn approve_dispatching<E: SqlEngine>(cell: &Cell<E>, id: &str, before: &dyn Fn()) -> Value {
    use norn_api::actions::ActionAnswer;
    use norn_api::refusal::Refusal;
    use norn_cell::flow_run::RunStarter;
    use norn_cell_do::policy::Caller;
    let as_caller = |subject: &str, roles: &[String]| Caller {
        subject_id: subject.to_owned(),
        roles: roles.to_vec(),
        actor_kind: "user",
        role_table: norn_cell_do::principal::FIXTURE_ROLES,
        accountable_person: Some(subject.to_owned()),
    };
    cell.caller
        .record(Some(as_caller("approver@steps", &["admin".to_owned()])));
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
    let as_requester =
        |requester: &RunStarter, dispatch: &mut dyn FnMut() -> Result<ActionAnswer, Refusal>| {
            before();
            cell.caller
                .record(Some(as_caller(&requester.subject, &requester.roles)));
            Some(dispatch())
        };
    let decided = norn_api::decisions::decide(
        &cell.host,
        &norn_api::decisions::DecideCall {
            scope: &scope,
            actor: Some("approver@steps"),
            id,
            body: &body,
            now: LogicalTime::from_millis(NOW * 1000),
        },
        &reads,
        &cell.handles,
        &as_requester,
    )
    .unwrap();
    cell.caller.record(None);
    serde_json::to_value(decided.call.expect("the approved call was dispatched")).unwrap()
}

/// The approver approves `id` through the decide service, and the gate is
/// revised after the approval is recorded and before its call dispatches as
/// its requester: the approval is refused `approval.definition_changed`.
fn approve_racing_a_revision<E: SqlEngine>(cell: &Cell<E>, id: &str) {
    let dispatched = approve_dispatching(cell, id, &|| revise_gate(cell));
    assert_eq!(
        dispatched["code"],
        json!("approval.definition_changed"),
        "{dispatched}"
    );
}

/// S5c: of two approvals of a step's exact call, the one the ledger
/// committed last binds the step, whatever their decision ids. The first
/// approval was superseded (the gate changed under its dispatch); the
/// starter's retry of the run parks the step's call anew, and the second
/// approval, recorded later at the same logical time, has an id that sorts
/// first: its commit, run by the starter's next retry, is the step's outcome.
#[test]
fn of_two_approvals_of_a_steps_call_the_later_commit_binds() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cell.sqlite");
    let first = parked_step(&path);
    let cell = cell_on(file(&path), false);
    approve_racing_a_revision(&cell, &first);
    // Park the step's call anew until its id sorts before the first one's.
    let mut seen = vec![first.clone()];
    let mut later = None;
    for revision in 2..=17 {
        let (status, run) = start(&cell);
        assert_eq!(status, 200, "{run}");
        let parked: Vec<String> = pending_ids(&cell)
            .into_iter()
            .filter(|id| !seen.contains(id))
            .collect();
        assert_eq!(parked.len(), 1, "{parked:?}");
        let id = parked[0].clone();
        if id < first {
            later = Some(id);
            break;
        }
        seen.push(id);
        revise_gate_from(&cell, revision);
    }
    let later = later.expect("a revision whose call id sorts first");
    let (status, decided) = decide_with(&cell, &later, "approve-later");
    assert_eq!(status, 200, "{decided}");
    assert_eq!(decided["call"], not_executed(), "{decided}");
    let (status, run) = start(&cell);
    assert_eq!(status, 200, "{run}");
    assert_eq!(run["state"], json!("completed"), "{run}");
    assert_eq!(standing(&cell, &later), committed());
}

/// S6c: each commit is attributed to the approval it consumed. A refusal
/// recorded for one approval (the gate changed under its dispatch) stays
/// final when the same call later commits under the revised gate's own
/// approval.
#[test]
fn a_recorded_refusal_stays_final_when_the_call_commits_under_another_approval() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cell.sqlite");
    let id = parked(&path);
    let cell = cell_on(file(&path), false);
    approve_racing_a_revision(&cell, &id);
    let changed = json!({ "committed": false, "code": "approval.definition_changed",
        "state": "refused" });
    assert_eq!(standing(&cell, &id), changed);

    let (status, parked) = retry(&cell);
    assert_eq!(status, 202, "{parked}");
    let other = parked["decision"]["id"].as_str().unwrap().to_owned();
    assert_ne!(other, id);
    let (status, decided) = decide_with(&cell, &other, "approve-other");
    assert_eq!(status, 200, "{decided}");
    let (status, receipt) = retry(&cell);
    assert_eq!(status, 200, "{receipt}");
    assert_eq!(standing(&cell, &other), committed());
    assert_eq!(standing(&cell, &id), changed, "final");
}

/// Retire the gate (its first revision): the melt action is ungated.
fn retire_gate<E: SqlEngine>(cell: &Cell<E>) {
    let (status, retired) = call(
        cell,
        "POST",
        &format!("/v1/definitions/{APPROVAL_ID}:retire"),
        OPERATOR,
        &json!({ "expectedVersion": 1 }).to_string(),
    );
    assert_eq!(status, 200, "{retired}");
}

/// S6c: whether an approved call committed is read from the ledger alone,
/// the same on every path. The gate is retired and the requester's exact
/// call commits ungated, so its commit consumes no approval, as a commit
/// made before consumption was recorded does; the file is then opened as a
/// host before APPROVAL-RECOVERY-1 left it (no `_norn_approved_call`). The
/// opened file and a restore of its export answer alike: committed, and the
/// approver's replay runs nothing on either.
#[test]
fn an_opened_file_and_its_restored_export_attribute_a_commit_alike() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cell.sqlite");
    let id = approved(&path);
    {
        let cell = cell_on(file(&path), false);
        retire_gate(&cell);
        let (status, receipt) = retry(&cell);
        assert_eq!(status, 200, "{receipt}");
    }
    file(&path)
        .execute_batch(r#"DROP TABLE "_norn_approved_call""#)
        .unwrap();
    let opened = cell_on(file(&path), false);
    let target = directory.path().join("restored.sqlite");
    restore(&path, &target, directory.path());
    let restored = cell_on(file(&target), false);
    assert_eq!(standing(&opened, &id), committed());
    assert_eq!(standing(&restored, &id), standing(&opened, &id));
    for (cell, at) in [(&opened, &path), (&restored, &target)] {
        let before = commit_count(at);
        let (status, replayed) = approve(cell, &id);
        assert_eq!(status, 200, "{replayed}");
        assert_eq!(replayed["call"], committed(), "{replayed}");
        assert_eq!(commit_count(at), before, "nothing ran");
    }
}

/// S7: a stale drive of a step (it read the step before its call was
/// approved) tries the call on the starter's recorded authority. Meanwhile
/// the call is approved and its requester's own request commits it, and
/// nothing has delivered that outcome yet. The stale try is denied and goes
/// last: under the one cell hold it records under, the step takes the
/// committed outcome, never the denial.
#[test]
fn a_stale_drive_denied_after_the_requesters_commit_delivers_the_commit() {
    use norn_api::actions::ActionAnswer;
    use norn_api::refusal::Refusal;
    use norn_cell::flow_run::RunStarter;
    use norn_cell_do::policy::Caller;
    type Dispatch<'a> = &'a mut dyn FnMut() -> Result<ActionAnswer, Refusal>;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cell.sqlite");
    let id = parked_step(&path);
    let cell = cell_on(file(&path), false);
    let raced = std::cell::Cell::new(false);
    let as_starter = |starter: &RunStarter, dispatch: Dispatch<'_>| {
        // After the drive read the step, before its dispatch holds the cell:
        // approved, and committed by the requester's own request.
        let decided = approve_dispatching(&cell, &id, &|| {});
        assert_eq!(decided, committed(), "{decided}");
        assert_eq!(run_state(&cell), json!("awaiting_perform_convert"));
        raced.set(true);
        cell.caller.record(Some(Caller {
            subject_id: starter.subject.clone(),
            roles: starter.roles.clone(),
            actor_kind: "user",
            role_table: norn_cell_do::principal::FIXTURE_ROLES,
            accountable_person: starter.accountable_person.clone(),
        }));
        let answer = dispatch();
        cell.caller.record(None);
        assert!(
            matches!(&answer, Err(refused) if refused.status == 403),
            "the stale try is denied: {answer:?}"
        );
        Some(answer)
    };
    let as_requester = |_requester: &RunStarter, _dispatch: Dispatch<'_>| None;
    norn_api::flow_driver::drive_run(
        &cell.host,
        "melt-run",
        &as_starter,
        &as_requester,
        &cell.handles,
        LogicalTime::from_millis(NOW * 1000 + 5),
    )
    .unwrap();
    assert!(raced.get());
    assert_eq!(run_state(&cell), json!("completed"));
    assert_eq!(melt(&cell).0, 200);
    assert_eq!(standing(&cell, &id), committed());
}
