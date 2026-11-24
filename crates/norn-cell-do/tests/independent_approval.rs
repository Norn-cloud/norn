//! AUTH-INDEPENDENT-PERSON (#618) through the DO dispatch: an approval needs
//! two accountable people (APPROVAL-INDEPENDENT-1), runs only the commit it
//! was granted for (APPROVAL-INTENT-1), and the requester's authority is
//! decided again when its retry commits.

#[path = "support/step_cell.rs"]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "the shared step cell's fixture setup unwraps"
)]
mod step_cell;

use std::error::Error;
use std::path::Path;
use std::sync::Arc;

use norn_api::actions::{ActionAnswer, ActionQuery};
use norn_api::decisions::{DecideCall, DecideRequest, DecisionsQuery};
use norn_api::scope::ScopeQuery;
use norn_cell::StoreOptions;
use norn_cell::engine::{NativeEngine, SqlEngine};
use norn_cell_do::policy::Caller;
use norn_cell_do::principal::{FIXTURE_ROLES, FixturePrincipals};
use norn_kernel::LogicalTime;
use serde_json::{Value, json};
use step_cell::{APPROVER, Cell, MELT_ACTION, OPERATOR, REVOKED, call, cell, cell_on, melt_body};

type Checked = Result<(), Box<dyn Error>>;

const AT: u64 = 1_800_000_000_000;

/// Install the approval gating the melt action, deciders `admin`.
fn gate<E: SqlEngine>(cell: &Cell<E>) -> Checked {
    let payload = json!({ "action": MELT_ACTION, "decider": { "roles": ["admin"] },
        "question": "Convert this melt?", "risk": "high" });
    let body = json!({ "kind": "approval", "id": "ops.melt_approval", "payload": &payload });
    let (status, admitted) = call(
        cell,
        "POST",
        "/v1/definitions:admit",
        OPERATOR,
        &body.to_string(),
    );
    assert_eq!(status, 200, "{admitted}");
    let restatement = admitted
        .get("restatementSha256")
        .cloned()
        .ok_or("no restatement")?;
    let signed = json!({ "kind": "approval", "id": "ops.melt_approval", "payload": payload,
        "signoff": { "restatementSha256": restatement } });
    let (status, installed) = call(
        cell,
        "POST",
        "/v1/definitions:install",
        OPERATOR,
        &signed.to_string(),
    );
    assert_eq!(status, 200, "{installed}");
    Ok(())
}

/// The operator's melt call under `key`.
fn melt<E: SqlEngine>(cell: &Cell<E>, key: &str) -> (u16, Value) {
    let body = melt_body("melt-7", "12", "10", key);
    call(
        cell,
        "POST",
        &format!("/v1/actions/{MELT_ACTION}"),
        OPERATOR,
        &body,
    )
}

fn decision_id(parked: &Value) -> Result<String, Box<dyn Error>> {
    let id = parked.pointer("/decision/id").and_then(Value::as_str);
    Ok(id.ok_or("no pending decision")?.to_owned())
}

/// The approver approves `id`.
fn approve<E: SqlEngine>(cell: &Cell<E>, id: &str) {
    let body = json!({ "choice": "approve", "idempotencyKey": "approve-1" }).to_string();
    let (status, decided) = call(
        cell,
        "POST",
        &format!("/v1/decisions/{id}:decide"),
        APPROVER,
        &body,
    );
    assert_eq!(status, 200, "{decided}");
}

fn melt_written<E: SqlEngine>(cell: &Cell<E>) -> bool {
    call(
        cell,
        "GET",
        "/v1/objects/constitution.Melt/melt-7",
        APPROVER,
        "",
    )
    .0 == 200
}

/// A gated cell on `path` whose operator call `operator-1` is parked and
/// approved: the decision id. The cell is closed before it returns.
fn approved_on(path: &Path) -> Result<String, Box<dyn Error>> {
    let cell = cell_on(NativeEngine::open(path, StoreOptions::default())?, true);
    gate(&cell)?;
    let (status, parked) = melt(&cell, "operator-1");
    assert_eq!(status, 202, "{parked}");
    let id = decision_id(&parked)?;
    approve(&cell, &id);
    Ok(id)
}

/// Rewrite the parked call's command on `path` with the SQL expression `set`.
fn rewrite_parked(path: &Path, set: &str) -> Checked {
    NativeEngine::open(path, StoreOptions::default())?.execute_batch(&format!(
        r#"UPDATE "_norn_outbox" SET "command"={set} WHERE json_extract("command",'$.payload.approval') IS NOT NULL"#
    ))?;
    Ok(())
}

/// A caller for `subject` acting for `person`, as a verified token names it.
fn caller(subject: &str, kind: &'static str, person: Option<&str>) -> Caller {
    Caller {
        subject_id: subject.to_owned(),
        roles: vec!["admin".to_owned()],
        actor_kind: kind,
        role_table: FIXTURE_ROLES,
        accountable_person: person.map(str::to_owned),
    }
}

fn scope() -> ScopeQuery {
    ScopeQuery {
        org_id: Some("org_demo".into()),
        environment_id: Some("dev".into()),
    }
}

/// `as_caller` calls the melt action with `key`.
fn melt_as(
    cell: &Cell,
    as_caller: Caller,
    key: &str,
    now: u64,
) -> Result<ActionAnswer, Box<dyn Error>> {
    let subject = as_caller.subject_id.clone();
    let request = norn_api::actions::decode_action_request(&melt_body("melt-7", "12", "10", key))
        .map_err(|refusal| refusal.code)?;
    let query = ActionQuery {
        org_id: Some("org_demo".into()),
        environment_id: Some("dev".into()),
        version: None,
    };
    cell.caller.record(Some(as_caller));
    let answer = norn_api::actions::call_action(
        &cell.host,
        &query,
        Some(&subject),
        MELT_ACTION,
        &request,
        &cell.handles,
        LogicalTime::from_millis(now),
    );
    cell.caller.record(None);
    Ok(answer.map_err(|refusal| refusal.code)?)
}

/// How many calls wait on a decider, as `as_caller` lists them.
fn pending_for(cell: &Cell, as_caller: Caller, now: u64) -> Result<u64, Box<dyn Error>> {
    let subject = as_caller.subject_id.clone();
    let query = DecisionsQuery {
        org_id: Some("org_demo".into()),
        environment_id: Some("dev".into()),
        state: Some("pending".into()),
        flow: None,
        cursor: None,
        limit: None,
    };
    cell.caller.record(Some(as_caller));
    let listed = norn_api::decisions::list_decisions(
        &cell.host,
        &query,
        Some(&subject),
        LogicalTime::from_millis(now),
    );
    cell.caller.record(None);
    Ok(listed.map_err(|refusal| refusal.code)?.total)
}

/// `as_caller` approves `id`: the refusal's status, if refused.
fn decide_as(cell: &Cell, as_caller: Caller, id: &str, now: u64) -> Option<u16> {
    let subject = as_caller.subject_id.clone();
    let reads = |capability: norn_api::authz::ReadCapability, object_type: &str| {
        norn_cell_do::policy::collection_access(
            cell.host.scope(),
            &subject,
            capability,
            object_type,
        )
    };
    let body = DecideRequest {
        choice: "approve".into(),
        note: None,
        idempotency_key: format!("{subject}-{now}"),
    };
    cell.caller.record(Some(as_caller));
    let decided = norn_api::decisions::decide(
        &cell.host,
        &DecideCall { scope: &scope(), actor: Some(&subject), id, body: &body, now: LogicalTime::from_millis(now) },
        &reads,
        &cell.handles,
        &|_requester: &norn_cell::flow_run::RunStarter,
          _dispatch: &mut dyn FnMut() -> Result<ActionAnswer, norn_api::refusal::Refusal>| None,
    );
    cell.caller.record(None);
    decided.err().map(|refused| refused.refusal.status)
}

/// APPROVAL-INDEPENDENT-1: two sessions of one person, or a principal naming
/// no person, never approve the call or see it waiting; a requester naming no
/// person parks nothing; another person approves, and the requester's own
/// retry commits it.
#[test]
fn two_sessions_of_one_person_never_approve_a_call() -> Checked {
    let cell = cell();
    gate(&cell)?;
    let nobody = caller("operator@steps", "user", None);
    assert!(melt_as(&cell, nobody, "unattributed", AT).is_err());
    let alice_web = caller("operator@steps", "user", Some("alice"));
    let ActionAnswer::Pending(parked) = melt_as(&cell, alice_web.clone(), "alice-1", AT)? else {
        return Err("the call did not park".into());
    };
    let id = parked.decision.id;
    let alice_phone = caller("approver@steps", "user", Some("alice"));
    assert_eq!(pending_for(&cell, alice_phone.clone(), AT)?, 0);
    assert_eq!(decide_as(&cell, alice_phone, &id, AT), Some(404));
    assert_eq!(
        decide_as(&cell, caller("approver@steps", "service", None), &id, AT),
        Some(404)
    );
    let bob = caller("approver@steps", "user", Some("bob"));
    assert_eq!(pending_for(&cell, bob.clone(), AT)?, 1);
    assert_eq!(decide_as(&cell, bob, &id, AT), None);
    assert!(matches!(
        melt_as(&cell, alice_web, "alice-1", AT)?,
        ActionAnswer::Receipt(_)
    ));
    assert!(melt_written(&cell));
    Ok(())
}

/// APPROVAL-INTENT-1: the parked approval names another prepared commit than
/// the one the requester's retry would make. The retry commits nothing and
/// the refusal is final; a new call asks for a fresh approval.
#[test]
fn an_approval_runs_only_the_commit_it_was_granted_for() -> Checked {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("cell.sqlite");
    approved_on(&path)?;
    let other = format!("'sha256:{}'", "0".repeat(64));
    rewrite_parked(
        &path,
        &format!(r#"json_set("command",'$.payload.approval.intentDigest',{other})"#),
    )?;
    let cell = cell_on(NativeEngine::open(&path, StoreOptions::default())?, false);
    for _ in 0..2 {
        let (status, refused) = melt(&cell, "operator-1");
        assert_eq!(
            (status, refused.get("code")),
            (409, Some(&json!("action.approved_intent_changed"))),
            "{refused}"
        );
    }
    assert!(!melt_written(&cell));
    assert_eq!(melt(&cell, "operator-2").0, 202);
    Ok(())
}

/// A call parked and approved before intents and people were recorded: its
/// requester's retry still names the requester, but the approval covers no
/// commit, so nothing runs and that is final.
#[test]
fn an_approval_recorded_before_intents_were_bound_runs_nothing() -> Checked {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("cell.sqlite");
    approved_on(&path)?;
    rewrite_parked(
        &path,
        r#"json_remove("command",'$.payload.approval.intentDigest','$.payload.approval.requester.accountablePerson')"#,
    )?;
    let cell = cell_on(NativeEngine::open(&path, StoreOptions::default())?, false);
    for _ in 0..2 {
        let (status, refused) = melt(&cell, "operator-1");
        assert_eq!(
            (status, refused.get("code")),
            (409, Some(&json!("approval.definition_changed"))),
            "{refused}"
        );
    }
    assert!(!melt_written(&cell));
    Ok(())
}

/// The requester's roles are revoked after the approval is recorded: its
/// retry is decided under its authority now, denied, and commits nothing.
#[test]
fn a_requester_revoked_after_the_approval_commits_nothing() -> Checked {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("cell.sqlite");
    approved_on(&path)?;
    let mut cell = cell_on(NativeEngine::open(&path, StoreOptions::default())?, false);
    cell.principals = FixturePrincipals::new(REVOKED, Arc::clone(&cell.caller));
    assert_eq!(melt(&cell, "operator-1").0, 403);
    assert!(!melt_written(&cell));
    Ok(())
}
