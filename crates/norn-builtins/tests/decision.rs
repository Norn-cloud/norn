//! DECISION-1 / DECISION-ISSUED-1: only the cell resolves `norn.decision@1`,
//! by issuing the receipt for a verified decider. Reconciliation refuses any
//! other receipt, however well-formed.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test assertions"
)]

use std::collections::{BTreeMap, BTreeSet};
use std::convert::Infallible;

use norn_builtins::decision::{
    DeciderEvidence, Decision, DecisionRefusal, IssuedOnly, REQUEST_BLOB, expire, issue,
    parse_request, withdraw,
};
use norn_ir::function::{
    Authority, AuthorityKind, BlobRef, Digest, Fencing, FreshnessMode, FunctionInvocation,
    FunctionManifest, FunctionOutcome, FunctionReceipt, MemberPin, PredicatePin,
    RelevantStateToken, RetryCeiling, RunRef,
};
use norn_kernel::result_reconciliation::{
    Completion, DispatchedCommand, InvalidReceipt, ReceiptValidator, ReconcileContext,
    ReconcilePolicy, Reconciliation, ResultAuthorizer, ResultAuthzRequest, ResultChecks,
    ResultDenial, StateView, reconcile,
};
use norn_kernel::{CommandId, FactKey, FactVersion, Generation, LogicalTime};
use norn_testkit::proptest::prelude::*;
use serde_json::json;

const TENANT: &str = "org_demo";
const DEADLINE: u64 = 10_000;

fn manifest() -> FunctionManifest {
    norn_builtins::decision_manifest().unwrap()
}

fn request(extra: &serde_json::Value) -> Vec<u8> {
    let mut body = json!({
        "question": "Publish the Q3 dashboard?",
        "risk": "high",
        "options": [
            { "id": "publish", "label": "Publish", "effect": { "action": "dash.publish" }, "scope": "Public" },
            { "id": "hold", "label": "Hold", "effect": { "binding": "dash.hold" }, "scope": "Nobody" }
        ],
        "agentPick": "hold",
        "decider": { "roles": ["finance.approver"], "law": "law.two_person" },
        "expiresInMs": 86_400_000_u64
    });
    if let (Some(base), Some(extra)) = (body.as_object_mut(), extra.as_object()) {
        for (key, value) in extra {
            base.insert(key.clone(), value.clone());
        }
    }
    serde_json::to_vec(&body).unwrap()
}

fn invocation(manifest: &FunctionManifest, request: &[u8]) -> FunctionInvocation {
    FunctionInvocation {
        tenant: TENANT.to_owned(),
        world: "main".to_owned(),
        authority: Authority {
            kind: AuthorityKind::Service,
            id: "flow:publish".to_owned(),
        },
        command_id: "run-1/ask_publish".to_owned(),
        run: RunRef {
            id: "run-1".to_owned(),
            generation: 1,
        },
        manifest: manifest.content_digest().unwrap(),
        inputs: BTreeMap::new(),
        blobs: BTreeMap::from([(
            REQUEST_BLOB.to_owned(),
            BlobRef {
                digest: Digest::of_json_document(request).unwrap(),
                version: None,
                size_bytes: u64::try_from(request.len()).unwrap(),
            },
        )]),
        provenance_commit: 7,
        dependency: RelevantStateToken::default(),
        freshness: FreshnessMode::AppendOnly,
        destination: None,
        deadline_millis: DEADLINE,
        retry: RetryCeiling {
            max_attempts: 1,
            max_cost_units: 0,
        },
        fencing: Fencing {
            token: "run-1/ask_publish/1".to_owned(),
            cancellation_generation: 0,
        },
    }
}

fn ada() -> Authority {
    Authority {
        kind: AuthorityKind::Actor,
        id: "user:ada".to_owned(),
    }
}

fn roles(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|name| (*name).to_owned()).collect()
}

fn decide(choice: &str) -> Decision {
    Decision {
        choice: choice.to_owned(),
        note: Some("Numbers reconciled.".to_owned()),
    }
}

struct Allow;
impl ResultAuthorizer for Allow {
    fn authorize(&self, _: &ResultAuthzRequest<'_>) -> Result<(), ResultDenial> {
        Ok(())
    }
}

struct Contract;
impl ReceiptValidator for Contract {
    fn validate(&self, _: &DispatchedCommand, _: &FunctionReceipt) -> Result<(), InvalidReceipt> {
        Ok(())
    }
}

struct Unchanged;
impl StateView for Unchanged {
    type Error = Infallible;
    fn fact_version(&self, _: &FactKey) -> Result<FactVersion, Infallible> {
        Ok(FactVersion(0))
    }
    fn predicate_members(&self, _: &PredicatePin) -> Result<Vec<MemberPin>, Infallible> {
        Ok(Vec::new())
    }
    fn collection_epoch(&self, _: &str) -> Result<u64, Infallible> {
        Ok(0)
    }
}

/// Reconcile `receipt` as the perform's result under the issued-only contract.
fn deliver(
    manifest: &FunctionManifest,
    invocation: &FunctionInvocation,
    issued: &BTreeSet<Digest>,
    receipt: &FunctionReceipt,
) -> Reconciliation {
    let dispatched = DispatchedCommand::pin(
        invocation.clone(),
        manifest.execution_profile_digest().unwrap(),
    )
    .unwrap();
    let state = norn_kernel::result_reconciliation::CommandState::dispatched(&dispatched);
    let digest = manifest.content_digest().unwrap();
    let validator = IssuedOnly {
        manifest: &digest,
        issued,
        inner: &Contract,
    };
    reconcile(
        &ReconcileContext {
            tenant: TENANT,
            world: "main",
            now: LogicalTime::from_millis(5_000),
            policy: ReconcilePolicy::DEFAULT,
        },
        &dispatched,
        &state,
        &Completion {
            receipt,
            command: &CommandId(invocation.command_id.clone()),
            generation: Generation(1),
            authenticated_executor: Some(receipt.provenance.executor.as_str()),
        },
        &ResultChecks {
            authorizer: &Allow,
            validator: &validator,
        },
        &Unchanged,
    )
    .unwrap()
}

#[test]
fn a_cell_issued_receipt_resolves_the_perform_and_routes_on_choice() {
    let manifest = manifest();
    let body = request(&json!({}));
    let invocation = invocation(&manifest, &body);
    let held = roles(&["finance.approver"]);
    let laws = roles(&["law.two_person"]);
    let issued = issue(
        &manifest,
        &invocation,
        &body,
        &DeciderEvidence {
            actor: &ada(),
            roles: &held,
            satisfied_laws: &laws,
        },
        &decide("publish"),
        5_000,
    )
    .unwrap();
    let receipt = issued.receipt();
    assert_eq!(
        receipt.provenance.executor, "user:ada",
        "the deciding Actor"
    );
    assert_eq!(issued.digest(), &receipt.content_digest().unwrap());
    let FunctionOutcome::Success(success) = &receipt.outcome else {
        panic!("a decision is a success")
    };
    assert_eq!(
        success.summary.get("choice"),
        Some(&norn_ir::RuntimeValue::String("publish".to_owned()))
    );
    let ledger = BTreeSet::from([issued.digest().clone()]);
    let Reconciliation::Accepted(acceptance) = deliver(&manifest, &invocation, &ledger, receipt)
    else {
        panic!("the cell-issued receipt is accepted")
    };
    assert_eq!(&acceptance.record.receipt, issued.digest());
}

#[test]
fn a_client_supplied_result_is_refused_however_well_formed() {
    let manifest = manifest();
    let body = request(&json!({}));
    let invocation = invocation(&manifest, &body);
    let held = roles(&["finance.approver"]);
    let laws = roles(&["law.two_person"]);
    let issued = issue(
        &manifest,
        &invocation,
        &body,
        &DeciderEvidence {
            actor: &ada(),
            roles: &held,
            satisfied_laws: &laws,
        },
        &decide("hold"),
        5_000,
    )
    .unwrap();
    // A client copies the issued shape but chooses "publish".
    let mut forged = issued.receipt().clone();
    if let FunctionOutcome::Success(success) = &mut forged.outcome {
        success.summary.insert(
            "choice".to_owned(),
            norn_ir::RuntimeValue::String("publish".to_owned()),
        );
        success.output_manifest =
            norn_ir::function::output_manifest_digest(&success.outputs, &success.summary).unwrap();
    }
    let ledger = BTreeSet::from([issued.digest().clone()]);
    for (receipt, ledger) in [
        (&forged, ledger),
        // Even the genuine bytes are refused when the cell has no record of issuing them.
        (issued.receipt(), BTreeSet::new()),
    ] {
        let Reconciliation::Refused(refused) = deliver(&manifest, &invocation, &ledger, receipt)
        else {
            panic!("a receipt the cell did not issue is refused")
        };
        assert!(
            format!("{:?}", refused.record.reason).contains("decision.receipt_not_cell_issued"),
            "{:?}",
            refused.record.reason
        );
    }
}

#[test]
fn a_non_decider_is_refused_before_any_receipt_exists() {
    let manifest = manifest();
    let body = request(&json!({}));
    let invocation = invocation(&manifest, &body);
    let laws = roles(&["law.two_person"]);
    let none = BTreeSet::new();
    let approver = roles(&["finance.approver"]);
    let agent = Authority {
        kind: AuthorityKind::Service,
        id: "agent:copilot".to_owned(),
    };
    for (actor, held, satisfied, refusal) in [
        (
            ada(),
            roles(&["finance.viewer"]),
            laws.clone(),
            DecisionRefusal::NotADecider,
        ),
        (agent, approver.clone(), laws, DecisionRefusal::NotADecider),
        (ada(), approver, none, DecisionRefusal::LawNotSatisfied),
    ] {
        let refused = issue(
            &manifest,
            &invocation,
            &body,
            &DeciderEvidence {
                actor: &actor,
                roles: &held,
                satisfied_laws: &satisfied,
            },
            &decide("publish"),
            5_000,
        )
        .unwrap_err();
        assert_eq!(refused, refusal, "{}", refused.code());
    }
}

#[test]
fn answers_and_requests_are_bounded() {
    let manifest = manifest();
    let body = request(&json!({}));
    let invocation = invocation(&manifest, &body);
    let held = roles(&["finance.approver"]);
    let laws = roles(&["law.two_person"]);
    let evidence = DeciderEvidence {
        actor: &ada(),
        roles: &held,
        satisfied_laws: &laws,
    };
    let answer =
        |decision: &Decision, now| issue(&manifest, &invocation, &body, &evidence, decision, now);
    assert_eq!(
        answer(&decide("ship"), 5_000),
        Err(DecisionRefusal::UnknownChoice)
    );
    let long = Decision {
        choice: "hold".to_owned(),
        note: Some("x".repeat(501)),
    };
    assert_eq!(answer(&long, 5_000), Err(DecisionRefusal::NoteTooLong));
    assert_eq!(
        answer(&decide("hold"), DEADLINE + 1),
        Err(DecisionRefusal::Expired)
    );
    // Another request than the dispatched blob does not bind.
    let other = request(&json!({ "question": "Something else?" }));
    assert_eq!(
        issue(
            &manifest,
            &invocation,
            &other,
            &evidence,
            &decide("hold"),
            5_000
        ),
        Err(DecisionRefusal::NotThisCommand)
    );

    let option =
        |id: &str| json!({ "id": id, "label": "L", "effect": { "action": "a" }, "scope": "S" });
    for (extra, code) in [
        (
            json!({ "question": "q".repeat(501) }),
            "decision.question_length",
        ),
        (json!({ "options": [] }), "decision.option_count"),
        (
            json!({ "options": (0..10).map(|n| option(&format!("o{n}"))).collect::<Vec<_>>() }),
            "decision.option_count",
        ),
        (json!({ "options": [option("Bad")] }), "decision.option_id"),
        (
            json!({ "options": [option("a"), option("a")] }),
            "decision.option_id",
        ),
        (
            json!({ "options": [{ "id": "a", "label": "L", "effect": {}, "scope": "S" }] }),
            "decision.option_effect",
        ),
        (
            json!({ "options": [{ "id": "a", "label": "L", "effect": { "action": "x", "binding": "y" }, "scope": "S" }] }),
            "decision.option_effect",
        ),
        (json!({ "agentPick": "ship" }), "decision.agent_pick"),
        (
            json!({ "decider": { "roles": [] } }),
            "decision.decider_roles",
        ),
        (
            json!({ "expiresInMs": 7 * 24 * 60 * 60 * 1000 + 1 }),
            "decision.expires_in_ms",
        ),
        (json!({ "risk": "extreme" }), "decision.request_shape"),
        (json!({ "context": 42 }), "decision.request_shape"),
    ] {
        assert_eq!(
            parse_request(&request(&extra))
                .map(|_| ())
                .unwrap_err()
                .code(),
            code,
            "{extra}"
        );
    }
}

#[test]
fn expiry_and_withdrawal_are_typed_failures_the_cell_issues() {
    let manifest = manifest();
    let body = request(&json!({}));
    let invocation = invocation(&manifest, &body);
    let owner = invocation.authority.clone();
    let none = BTreeSet::new();
    let by_owner = DeciderEvidence {
        actor: &owner,
        roles: &none,
        satisfied_laws: &none,
    };
    assert_eq!(
        expire(&manifest, &invocation, &body, DEADLINE - 1),
        Err(DecisionRefusal::NotExpired)
    );
    let expired = expire(&manifest, &invocation, &body, DEADLINE).unwrap();
    let withdrawn = withdraw(&manifest, &invocation, &body, &by_owner, DEADLINE - 1).unwrap();
    for (issued, tag) in [(expired, "Expired"), (withdrawn, "Withdrawn")] {
        let FunctionOutcome::Failure(failure) = &issued.receipt().outcome else {
            panic!("{tag} is a typed failure")
        };
        assert_eq!(failure.code, tag);
        assert_eq!(
            failure.detail.get("_tag"),
            Some(&norn_ir::RuntimeValue::String(tag.to_owned()))
        );
    }
}

#[test]
fn a_withdrawal_refuses_at_the_deadline_and_for_an_unknown_withdrawer() {
    let manifest = manifest();
    let body = request(&json!({}));
    let invocation = invocation(&manifest, &body);
    let none = BTreeSet::new();
    let approver = roles(&["finance.approver"]);
    let owner = invocation.authority.clone();
    let empty = Authority {
        kind: AuthorityKind::Actor,
        id: String::new(),
    };
    let stranger = Authority {
        kind: AuthorityKind::Service,
        id: "agent:copilot".to_owned(),
    };
    let withdraw_as = |actor: &Authority, held: &BTreeSet<String>, now| {
        withdraw(
            &manifest,
            &invocation,
            &body,
            &DeciderEvidence {
                actor,
                roles: held,
                satisfied_laws: &none,
            },
            now,
        )
    };
    // At or after the deadline only Expired resolves the decision.
    for now in [DEADLINE, DEADLINE + 1] {
        assert_eq!(
            withdraw_as(&owner, &none, now),
            Err(DecisionRefusal::Expired)
        );
    }
    for (actor, held) in [
        (&empty, &approver),
        (&stranger, &none),
        (&stranger, &approver),
        (&ada(), &none),
    ] {
        assert_eq!(
            withdraw_as(actor, held, 5_000),
            Err(DecisionRefusal::NotADecider),
            "{actor:?}"
        );
    }
    // The run's own authority, or a decider, may withdraw before the deadline.
    assert!(withdraw_as(&owner, &none, 5_000).is_ok());
    assert!(withdraw_as(&ada(), &approver, 5_000).is_ok());
}

/// One cell-side attempt to resolve the decision at a logical time.
#[derive(Debug, Clone)]
enum Attempt {
    Decide(usize),
    Withdraw,
    Expire,
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]
    /// DECISION-TERMINAL-1: for any interleaving of decide, withdraw and
    /// expire, delivered in order to reconciliation, exactly one terminal
    /// receipt is accepted per decision: the first one issued.
    #[test]
    fn any_interleaving_accepts_exactly_one_terminal_receipt(
        attempts in prop::collection::vec(
            (
                prop_oneof![
                    (0_usize..3).prop_map(Attempt::Decide),
                    Just(Attempt::Withdraw),
                    Just(Attempt::Expire),
                ],
                0_u64..2 * DEADLINE,
            ),
            1..12,
        ),
    ) {
        let manifest = manifest();
        let body = request(&json!({}));
        let invocation = invocation(&manifest, &body);
        let held = roles(&["finance.approver"]);
        let laws = roles(&["law.two_person"]);
        let evidence = DeciderEvidence { actor: &ada(), roles: &held, satisfied_laws: &laws };
        let choices = ["publish", "hold", "ship"];
        let mut attempts = attempts;
        attempts.sort_by_key(|(_, at)| *at);
        // The cell's alarm always expires an unresolved decision eventually.
        attempts.push((Attempt::Expire, 2 * DEADLINE));
        let dispatched = DispatchedCommand::pin(
            invocation.clone(),
            manifest.execution_profile_digest().unwrap(),
        )
        .unwrap();
        let digest = manifest.content_digest().unwrap();
        let mut state = norn_kernel::result_reconciliation::CommandState::dispatched(&dispatched);
        let mut ledger = BTreeSet::new();
        let mut first_issued = None;
        let mut accepted = Vec::new();
        for (attempt, at) in attempts {
            let issued = match attempt {
                Attempt::Decide(choice) => issue(&manifest, &invocation, &body, &evidence, &decide(choices[choice]), at),
                Attempt::Withdraw => withdraw(&manifest, &invocation, &body, &evidence, at),
                Attempt::Expire => expire(&manifest, &invocation, &body, at),
            };
            let Ok(issued) = issued else { continue };
            // The issuers' windows never overlap: before the deadline a
            // decision or withdrawal, at or after it only expiry.
            let expiry = matches!(&issued.receipt().outcome, FunctionOutcome::Failure(failure) if failure.code == "Expired");
            prop_assert_eq!(expiry, at >= DEADLINE);
            ledger.insert(issued.digest().clone());
            first_issued.get_or_insert_with(|| issued.digest().clone());
            let validator = IssuedOnly { manifest: &digest, issued: &ledger, inner: &Contract };
            let outcome = reconcile(
                &ReconcileContext {
                    tenant: TENANT,
                    world: "main",
                    now: LogicalTime::from_millis(at),
                    // The cell issues every decision receipt itself, so the
                    // issuers, not the delivery time, enforce the deadline.
                    policy: ReconcilePolicy { accept_late: true, ..ReconcilePolicy::DEFAULT },
                },
                &dispatched,
                &state,
                &Completion {
                    receipt: issued.receipt(),
                    command: &CommandId(invocation.command_id.clone()),
                    generation: Generation(1),
                    authenticated_executor: Some(issued.receipt().provenance.executor.as_str()),
                },
                &ResultChecks { authorizer: &Allow, validator: &validator },
                &Unchanged,
            )
            .unwrap();
            match outcome {
                Reconciliation::Accepted(acceptance) => {
                    accepted.push(acceptance.record.receipt.clone());
                    state = acceptance.state;
                }
                Reconciliation::Refused(refused) => state = refused.state,
            }
        }
        prop_assert_eq!(accepted.len(), 1, "exactly one terminal receipt");
        prop_assert_eq!(Some(&accepted[0]), first_issued.as_ref());
    }
}
