//! APPROVAL-REVISION-1 under APPROVAL-RECOVERY-1 conformance: the approval
//! fencing model's ITF traces (verification/hosted-timers-v1, `stepConform`)
//! are the oracle, replayed through the real DO dispatch: a call parks
//! (`POST /v1/actions`), a definitions revision installs (changing the
//! action's writer, or reinstalling it unchanged), the requester's grant is
//! revoked or restored, and the approver's `:decide` records the approval
//! and runs nothing (`not_executed`). Only the requester's retry of the call
//! runs it, exactly as the model's command comparison and authority decision
//! say: it commits, parks the call anew under the changed definition, or is
//! denied and leaves the call approved and not executed.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "trace correspondence: a divergence must fail loudly"
)]

#[path = "support/quint_itf.rs"]
mod quint_itf;
#[path = "support/step_cell.rs"]
mod step_cell;

use std::collections::BTreeMap;

use quint_itf::{int, map, str_map, traces};
use serde_json::{Value, json};
use norn_cell_do::principal::FixturePrincipals;
use step_cell::{
    APPROVER, Cell, MELT_ACTION, NAMES, NOW, OPERATOR, REVOKED, call, cell, melt_body,
};

const APPROVAL_ID: &str = "ops.melt_approval";

/// The approval gating the melt action, deciders `admin`.
fn gate(cell: &Cell) {
    let mut body = json!({ "kind": "approval", "id": APPROVAL_ID, "payload": {
        "action": MELT_ACTION,
        "decider": { "roles": ["admin"] },
        "question": "Convert this melt?",
        "risk": "high",
    }});
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

/// Install `set` as the next definitions revision.
fn revise(cell: &Cell, set: &str) {
    let version = cell.host.active_definitions_version().unwrap();
    let (model, input) = step_cell::action_cell::melt_install(
        set,
        norn_api::action_admission::prepare_action_model,
        NOW * 1000 + 1,
    );
    cell.host
        .install_definitions(model, input, version)
        .expect("the revision installs");
    assert_ne!(cell.host.active_definitions_version().unwrap(), version);
}

/// Call `c`'s request. It loses nothing, so the fine-mass law holds under
/// both writers (the one that omits the loss keeps its default of 0); the
/// writers' digests still differ.
fn body(c: i64) -> String {
    melt_body(&format!("melt-{c}"), "10", "10", &format!("call-{c}"))
}

/// Whether call `c`'s melt exists, read by a holder of the grant (the
/// requester may be revoked).
fn melt_status(cell: &Cell, c: i64) -> u16 {
    call(
        cell,
        "GET",
        &format!("/v1/objects/constitution.Melt/melt-{c}"),
        APPROVER,
        "",
    )
    .0
}

/// The requester makes call `c` (or retries it).
fn request(cell: &Cell, c: i64) -> (u16, Value) {
    call(
        cell,
        "POST",
        &format!("/v1/actions/{MELT_ACTION}"),
        OPERATOR,
        &body(c),
    )
}

fn decision_id(parked: &Value) -> String {
    parked["decision"]["id"]
        .as_str()
        .expect("decision id")
        .to_owned()
}

/// Where decision `id`'s approved call stands, read only: the approver's
/// decide with another key answers `decision.resolved` with it, and runs
/// nothing.
fn standing(cell: &Cell, id: &str) -> Value {
    let (status, resolved) = call(
        cell,
        "POST",
        &format!("/v1/decisions/{id}:decide"),
        APPROVER,
        &json!({ "choice": "approve", "idempotencyKey": format!("look-{id}") }).to_string(),
    );
    assert_eq!(status, 409, "{resolved}");
    assert_eq!(resolved["code"], json!("decision.resolved"), "{resolved}");
    resolved["resolution"]["call"].clone()
}

fn not_executed() -> Value {
    json!({ "committed": false, "state": "not_executed" })
}

fn states(state: &Value) -> BTreeMap<i64, String> {
    map(state, "state")
        .into_iter()
        .map(|(c, v)| (c, v.as_str().expect("state").to_owned()))
        .collect()
}

fn flag(state: &Value, var: &str, c: i64) -> bool {
    map(state, var)[&c].as_bool().expect(var)
}

/// What the requester's retry answered: its status and, for a call parked
/// anew, the fresh decision.
struct Retried {
    status: u16,
    decision: Option<String>,
}

#[test]
fn approval_fencing_quint_traces() {
    let (mut executed, mut reparked, mut denied, mut revisions, mut revocations) =
        (0_usize, 0_usize, 0_usize, 0_usize, 0_usize);
    for (file, trace) in traces("approval-fencing") {
        let name = file.display();
        let mut cell = cell();
        gate(&cell);
        let mut decision: BTreeMap<i64, String> = BTreeMap::new();
        let mut retried: BTreeMap<i64, Retried> = BTreeMap::new();
        for (step, pair) in trace.windows(2).enumerate() {
            let (before, after) = (&pair[0], &pair[1]);
            if int(after, "revisions") > int(before, "revisions") {
                // The writer the model's action version names: a revision
                // that changes the action installs the other writer, any
                // other one reinstalls the current writer unchanged.
                let action = str_map(&after["active"])["action"]
                    .as_i64()
                    .expect("version");
                revise(
                    &cell,
                    if action == 0 {
                        step_cell::DERIVES_LOSS
                    } else {
                        step_cell::action_cell::OMITS_LOSS
                    },
                );
                revisions += 1;
                continue;
            }
            if int(after, "authorityChanges") > int(before, "authorityChanges") {
                // The requester's grant: revoked keeps the same subject,
                // verified now with no role (APPROVAL-RECOVERY-1, S2).
                let authorized = after["authorized"].as_bool().expect("authorized");
                cell.principals = FixturePrincipals::new(
                    if authorized { NAMES } else { REVOKED },
                    std::sync::Arc::clone(&cell.caller),
                );
                if !authorized {
                    revocations += 1;
                }
                continue;
            }
            let (old, new) = (states(before), states(after));
            let (&c, now) = new
                .iter()
                .find(|(c, s)| old[c] != **s)
                .unwrap_or_else(|| panic!("{name} step {step}: no transition"));
            match (old[&c].as_str(), now.as_str()) {
                ("none", "parked") => {
                    let (status, parked) = request(&cell, c);
                    assert_eq!(status, 202, "{name} step {step}: call {c} parks: {parked}");
                    decision.insert(c, decision_id(&parked));
                }
                ("parked", "approved") => {
                    let id = &decision[&c];
                    let (status, decided) = call(
                        &cell,
                        "POST",
                        &format!("/v1/decisions/{id}:decide"),
                        APPROVER,
                        &json!({ "choice": "approve", "idempotencyKey": format!("approve-{id}") })
                            .to_string(),
                    );
                    assert_eq!(status, 200, "{name} step {step}: {decided}");
                    assert_eq!(
                        decided["call"],
                        not_executed(),
                        "{name} step {step}: call {c}: the decide runs nothing"
                    );
                    assert_eq!(melt_status(&cell, c), 404, "{name} step {step}: call {c}");
                }
                ("approved", "checking") => {
                    let (status, answer) = request(&cell, c);
                    let (allowed, matched) = (flag(after, "allowed", c), flag(after, "matched", c));
                    let expected = match (allowed, matched) {
                        (false, _) => 403,
                        (true, true) => 200,
                        (true, false) => 202,
                    };
                    assert_eq!(status, expected, "{name} step {step}: call {c}: {answer}");
                    let fresh = (status == 202).then(|| decision_id(&answer));
                    if let Some(fresh) = &fresh {
                        assert_ne!(
                            fresh, &decision[&c],
                            "{name} step {step}: call {c} parks a fresh decision"
                        );
                    }
                    retried.insert(c, Retried { status, decision: fresh });
                }
                ("checking", outcome) => {
                    let Retried { status, decision: fresh } = retried
                        .remove(&c)
                        .unwrap_or_else(|| panic!("{name} step {step}: call {c} not retried"));
                    let id = decision[&c].clone();
                    match (outcome, status) {
                        ("executed", 200) => {
                            assert_eq!(melt_status(&cell, c), 200, "{name} step {step}: call {c}");
                            assert_eq!(
                                standing(&cell, &id),
                                json!({ "committed": true, "state": "committed" }),
                                "{name} step {step}: call {c}"
                            );
                            executed += 1;
                        }
                        ("approved", 403) => {
                            assert_eq!(melt_status(&cell, c), 404, "{name} step {step}: call {c}");
                            assert_eq!(
                                standing(&cell, &id),
                                not_executed(),
                                "{name} step {step}: call {c}: the denial records nothing"
                            );
                            denied += 1;
                        }
                        ("parked", 202) => {
                            assert_eq!(melt_status(&cell, c), 404, "{name} step {step}: call {c}");
                            assert_eq!(
                                standing(&cell, &id),
                                not_executed(),
                                "{name} step {step}: call {c}: the stale approval is never consumed"
                            );
                            decision.insert(c, fresh.expect("the fresh decision"));
                            reparked += 1;
                        }
                        (outcome, status) => {
                            panic!("{name} step {step}: call {c} {outcome} after {status}")
                        }
                    }
                }
                (from, to) => panic!("{name} step {step}: unexpected {from} -> {to}"),
            }
        }
    }
    assert!(
        executed >= 5 && reparked >= 2 && denied >= 2 && revisions >= 5 && revocations >= 3,
        "{executed} executed, {reparked} reparked, {denied} denied, {revisions} revisions, \
         {revocations} revocations"
    );
}
