//! APPROVAL-REVISION-1 conformance: the approval fencing model's ITF traces
//! (verification/hosted-timers-v1, `stepConform`) are the oracle, replayed
//! through the real DO dispatch: a call parks (`POST /v1/actions`), a
//! definitions revision installs (changing the action's writer, or
//! reinstalling it unchanged), and an approval (`:decide`) either executes
//! the call or answers `approval.definition_changed` and runs nothing,
//! exactly as the model's digest comparison says.
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
use step_cell::{APPROVER, Cell, MELT_ACTION, NOW, OPERATOR, call, cell, melt_body};

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

fn melt_status(cell: &Cell, c: i64) -> u16 {
    call(
        cell,
        "GET",
        &format!("/v1/objects/constitution.Melt/melt-{c}"),
        OPERATOR,
        "",
    )
    .0
}

fn states(state: &Value) -> BTreeMap<i64, String> {
    map(state, "state")
        .into_iter()
        .map(|(c, v)| (c, v.as_str().expect("state").to_owned()))
        .collect()
}

#[test]
fn approval_fencing_quint_traces() {
    let (mut executed, mut refused, mut revisions) = (0_usize, 0_usize, 0_usize);
    for (file, trace) in traces("approval-fencing") {
        let name = file.display();
        let cell = cell();
        gate(&cell);
        let mut decision: BTreeMap<i64, String> = BTreeMap::new();
        let mut answered: BTreeMap<i64, bool> = BTreeMap::new();
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
            let (old, new) = (states(before), states(after));
            let (&c, now) = new
                .iter()
                .find(|(c, s)| old[c] != **s)
                .unwrap_or_else(|| panic!("{name} step {step}: no transition"));
            match (old[&c].as_str(), now.as_str()) {
                ("none" | "refused", "parked") => {
                    let (status, parked) = call(
                        &cell,
                        "POST",
                        &format!("/v1/actions/{MELT_ACTION}"),
                        OPERATOR,
                        &body(c),
                    );
                    assert_eq!(status, 202, "{name} step {step}: call {c} parks: {parked}");
                    let id = parked["decision"]["id"]
                        .as_str()
                        .expect("decision id")
                        .to_owned();
                    assert_ne!(
                        decision.get(&c),
                        Some(&id),
                        "{name} step {step}: a fresh decision"
                    );
                    decision.insert(c, id);
                }
                ("parked", "checking") => {
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
                    let matched = map(after, "matched")[&c].as_bool().expect("matched");
                    let expected = if matched {
                        json!({ "committed": true })
                    } else {
                        json!({ "committed": false, "code": "approval.definition_changed" })
                    };
                    assert_eq!(decided["call"], expected, "{name} step {step}: call {c}");
                    answered.insert(c, matched);
                }
                ("checking", outcome) => {
                    let ran = outcome == "executed";
                    assert_eq!(
                        answered.remove(&c),
                        Some(ran),
                        "{name} step {step}: call {c} {outcome}"
                    );
                    assert_eq!(
                        melt_status(&cell, c),
                        if ran { 200 } else { 404 },
                        "{name} step {step}: call {c}"
                    );
                    if ran { executed += 1 } else { refused += 1 }
                }
                (from, to) => panic!("{name} step {step}: unexpected {from} -> {to}"),
            }
        }
    }
    assert!(
        executed >= 5 && refused >= 2 && revisions >= 5,
        "{executed} executed, {refused} refused, {revisions} revisions"
    );
}
