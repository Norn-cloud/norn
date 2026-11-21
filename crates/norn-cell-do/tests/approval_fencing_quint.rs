//! APPROVAL-REVISION-1 under APPROVAL-RECOVERY-1 conformance: the approval
//! fencing model's ITF traces (verification/hosted-timers-v1, `stepConform`)
//! are the oracle, replayed through the real DO dispatch: a call parks
//! (`POST /v1/actions`), a definitions revision installs (changing the
//! action's writer, or reinstalling it unchanged), the requester gets a new
//! token carrying the issuer's grant (an issuer revocation alone changes
//! nothing the cell can see), and the approver's `:decide` records the
//! approval and runs nothing (`not_executed`). Only the requester's retry of
//! the call runs it, exactly as the model's command comparison and the
//! retry's own token say: it commits, parks the call anew under the changed
//! definition, or is denied `authz.denied` and leaves the call approved and
//! not executed.
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

use norn_cell_do::principal::FixturePrincipals;
use quint_itf::{int, map, str_map, traces};
use serde_json::{Value, json};
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

/// One trace replayed against one cell.
struct Replay {
    cell: Cell,
    decision: BTreeMap<i64, String>,
    retried: BTreeMap<i64, Retried>,
    counts: Counts,
}

#[derive(Default)]
struct Counts {
    executed: usize,
    reparked: usize,
    denied: usize,
    revisions: usize,
    revocations: usize,
    issuer_changes: usize,
    /// Executions after the issuer revoked the grant, under a token verified
    /// before: authority is as of the request's token.
    outstanding: usize,
}

impl Replay {
    fn new(counts: Counts) -> Self {
        let cell = cell();
        gate(&cell);
        Self {
            cell,
            decision: BTreeMap::new(),
            retried: BTreeMap::new(),
            counts,
        }
    }

    /// The model's step from `before` to `after`, `at` naming it.
    fn step(&mut self, at: &str, before: &Value, after: &Value) {
        if int(after, "revisions") > int(before, "revisions") {
            // The writer the model's action version names: a revision that
            // changes the action installs the other writer, any other one
            // reinstalls the current writer unchanged.
            let action = str_map(&after["active"])["action"]
                .as_i64()
                .expect("version");
            revise(
                &self.cell,
                if action == 0 {
                    step_cell::DERIVES_LOSS
                } else {
                    step_cell::action_cell::OMITS_LOSS
                },
            );
            self.counts.revisions = self.counts.revisions.saturating_add(1);
            return;
        }
        if int(after, "issuerChanges") > int(before, "issuerChanges") {
            // The issuer's grant alone: the cell keeps no membership store,
            // so nothing changes until the requester's next token.
            self.counts.issuer_changes = self.counts.issuer_changes.saturating_add(1);
            return;
        }
        if int(after, "tokenIssues") > int(before, "tokenIssues") {
            // The requester's next token: the issuer's grant as of now. A
            // revoked one keeps the same subject, verified with no role
            // (APPROVAL-RECOVERY-1, S2).
            let grant = after["token"]["grant"].as_bool().expect("grant");
            self.cell.principals = FixturePrincipals::new(
                if grant { NAMES } else { REVOKED },
                std::sync::Arc::clone(&self.cell.caller),
            );
            if !grant {
                self.counts.revocations = self.counts.revocations.saturating_add(1);
            }
            return;
        }
        let (old, new) = (states(before), states(after));
        let (&c, now) = new
            .iter()
            .find(|(c, s)| old[c] != **s)
            .unwrap_or_else(|| panic!("{at}: no transition"));
        let at = format!("{at}: call {c}");
        match (old[&c].as_str(), now.as_str()) {
            ("none", "parked") => {
                let (status, parked) = request(&self.cell, c);
                assert_eq!(status, 202, "{at} parks: {parked}");
                self.decision.insert(c, decision_id(&parked));
            }
            ("parked", "approved") => self.approve(&at, c),
            ("approved", "checking") => self.retry(&at, c, after),
            ("checking", outcome) => self.settle(&at, c, outcome, after),
            (from, to) => panic!("{at}: unexpected {from} -> {to}"),
        }
    }

    /// The approver's decide records the approval and runs nothing.
    fn approve(&self, at: &str, c: i64) {
        let id = &self.decision[&c];
        let (status, decided) = call(
            &self.cell,
            "POST",
            &format!("/v1/decisions/{id}:decide"),
            APPROVER,
            &json!({ "choice": "approve", "idempotencyKey": format!("approve-{id}") }).to_string(),
        );
        assert_eq!(status, 200, "{at}: {decided}");
        assert_eq!(
            decided["call"],
            not_executed(),
            "{at}: the decide runs nothing"
        );
        assert_eq!(melt_status(&self.cell, c), 404, "{at}");
    }

    /// The requester's retry answers as the model's authority decision and
    /// command comparison say: a denial is the opaque `authz.denied`, a
    /// reparked call names a fresh decision.
    fn retry(&mut self, at: &str, c: i64, after: &Value) {
        let (status, answer) = request(&self.cell, c);
        let expected = match (flag(after, "allowed", c), flag(after, "matched", c)) {
            (false, _) => 403,
            (true, true) => 200,
            (true, false) => 202,
        };
        assert_eq!(status, expected, "{at}: {answer}");
        if status == 403 {
            assert_eq!(answer["code"], json!("authz.denied"), "{at}: {answer}");
        }
        let fresh = (status == 202).then(|| decision_id(&answer));
        if let Some(fresh) = &fresh {
            assert_ne!(fresh, &self.decision[&c], "{at} parks a fresh decision");
        }
        self.retried.insert(
            c,
            Retried {
                status,
                decision: fresh,
            },
        );
    }

    /// Where the retried call stands afterwards.
    fn settle(&mut self, at: &str, c: i64, outcome: &str, after: &Value) {
        let Retried {
            status,
            decision: fresh,
        } = self
            .retried
            .remove(&c)
            .unwrap_or_else(|| panic!("{at} not retried"));
        let id = self.decision[&c].clone();
        let (melt, standing_is) = match (outcome, status) {
            ("executed", 200) => {
                self.counts.executed = self.counts.executed.saturating_add(1);
                if !flag(after, "issuerAtRun", c) {
                    self.counts.outstanding = self.counts.outstanding.saturating_add(1);
                }
                (200, json!({ "committed": true, "state": "committed" }))
            }
            // The denial records nothing: the approval stays not executed
            // and the melt absent.
            ("approved", 403) => {
                self.counts.denied = self.counts.denied.saturating_add(1);
                (404, not_executed())
            }
            // The stale approval is never consumed: it stays not executed,
            // the melt absent, and the call waits on the fresh decision.
            ("parked", 202) => {
                self.decision.insert(c, fresh.expect("the fresh decision"));
                self.counts.reparked = self.counts.reparked.saturating_add(1);
                (404, not_executed())
            }
            (outcome, status) => panic!("{at} {outcome} after {status}"),
        };
        assert_eq!(melt_status(&self.cell, c), melt, "{at} {outcome}");
        assert_eq!(standing(&self.cell, &id), standing_is, "{at} {outcome}");
    }
}

#[test]
fn approval_fencing_quint_traces() {
    let mut counts = Counts::default();
    for (file, trace) in traces("approval-fencing") {
        let mut replay = Replay::new(counts);
        for (step, pair) in trace.windows(2).enumerate() {
            replay.step(
                &format!("{} step {step}", file.display()),
                &pair[0],
                &pair[1],
            );
        }
        counts = replay.counts;
    }
    let Counts {
        executed,
        reparked,
        denied,
        revisions,
        revocations,
        issuer_changes,
        outstanding,
    } = counts;
    assert!(
        executed >= 5
            && reparked >= 2
            && denied >= 2
            && revisions >= 5
            && revocations >= 3
            && issuer_changes >= 3
            && outstanding >= 1,
        "{executed} executed, {reparked} reparked, {denied} denied, {revisions} revisions, \
         {revocations} revocations, {issuer_changes} issuer changes, {outstanding} under an \
         outstanding token"
    );
}
