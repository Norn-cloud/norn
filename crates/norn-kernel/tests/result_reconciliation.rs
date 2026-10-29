#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::too_many_lines,
    missing_docs,
    reason = "integration-test crate"
)]

//! Result reconciliation (T2-RES-1, #2777): the ruling 2026-09-27 §4
//! fixtures over the T1-FN-1 records in `fixtures/function`.
//!
//! Every case asserts the decision and that no refused result advances the
//! transition: unrelated-commit tolerance, a relevant-state change, a phantom
//! insert and delete under a predicate token, a collection epoch, revoked
//! authorization between dispatch and completion, cancellation, a late
//! attempt after supersession, duplicate and out-of-order completions, the
//! image-A-versus-document-now-B case, the deadline policy, dispatch binding
//! and provenance, bounded rescheduling, CAS in the accepting commit on the
//! in-memory reference host, and fan-in under all/any/quorum.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::path::Path;

use norn_ir::RuntimeObject;
use norn_ir::RuntimeValue;
use norn_ir::function::{
    CollectionPin, Digest, FunctionInvocation, FunctionManifest, FunctionOutcome, FunctionReceipt,
    MemberPin, PredicatePin, output_manifest_digest,
};
use norn_kernel::result_reconciliation::{
    Acceptance, Classification, CommandState, Completion, DispatchedCommand, FanIn, FanInError,
    FanInIgnored, FanInPolicy, FanInState, FanInStep, InvalidReceipt, ReceiptValidator,
    ReconcileContext, ReconcilePolicy, Reconciliation, RefusalReason, Refused, ResultAuthorizer,
    ResultAuthzRequest, ResultChecks, ResultDenial, StateView, reconcile,
};
use norn_kernel::{
    CellHost, Command, CommandId, CommitError, Effect, FactKey, FactMutation, FactVersion,
    FactWrite, Generation, IdempotencyKey, LogicalTime,
};
use norn_testkit::cell_host::MemoryCell;

const TENANT: &str = "tenant_acme";
const NOW: u64 = 1_790_000_000_000;

fn fixture<T: serde::de::DeserializeOwned>(name: &str) -> T {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/function")
        .join(name);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("{name}: {error}"))
}

/// One dispatched command, its receipt, and the state it was dispatched
/// against.
#[derive(Clone)]
struct Case {
    dispatched: DispatchedCommand,
    receipt: FunctionReceipt,
    state: CommandState,
    view: View,
    contract: Contract,
}

fn case(profile: &str) -> Case {
    let manifest: FunctionManifest = fixture(&format!("manifest.{profile}.json"));
    let invocation: FunctionInvocation = fixture(&format!("invocation.{profile}.json"));
    let receipt: FunctionReceipt = fixture(&format!("receipt.{profile}.json"));
    let dispatched = DispatchedCommand::pin(
        invocation,
        manifest.execution_profile_digest().expect("profile"),
    )
    .expect("pin");
    assert_eq!(
        receipt.dispatch, dispatched.dispatch,
        "{profile} fixture binds"
    );
    let state = CommandState::dispatched(&dispatched);
    let view = View::at_dispatch(&dispatched);
    Case {
        dispatched,
        receipt,
        state,
        view,
        contract: Contract {
            runtime: manifest.runtime.digest,
        },
    }
}

/// The receipt contract as a host supplies it. Hosts adapt `norn-checker`'s
/// `check_function_receipt` (which the kernel may not depend on); this stub
/// applies three of its rules so the refusal path is exercised here.
#[derive(Clone)]
struct Contract {
    runtime: Digest,
}

impl ReceiptValidator for Contract {
    fn validate(
        &self,
        dispatched: &DispatchedCommand,
        receipt: &FunctionReceipt,
    ) -> Result<(), InvalidReceipt> {
        let invalid = |reason: &str| {
            Err(InvalidReceipt {
                reason: reason.to_owned(),
            })
        };
        if receipt.attempt == 0 || receipt.attempt > dispatched.invocation.retry.max_attempts {
            return invalid("function.receipt.attempt_out_of_range");
        }
        if receipt.provenance.runtime != self.runtime {
            return invalid("function.receipt.dispatch_mismatch");
        }
        if let FunctionOutcome::Success(success) = &receipt.outcome
            && output_manifest_digest(&success.outputs, &success.summary).ok()
                != Some(success.output_manifest.clone())
        {
            return invalid("function.receipt.output_manifest_mismatch");
        }
        Ok(())
    }
}

/// Re-pin `case` after editing its invocation, re-binding the receipt.
fn repin(case: &mut Case, edit: impl FnOnce(&mut FunctionInvocation)) {
    let mut invocation = case.dispatched.invocation.clone();
    edit(&mut invocation);
    case.dispatched =
        DispatchedCommand::pin(invocation, case.dispatched.execution_profile.clone()).expect("pin");
    case.receipt.dispatch = case.dispatched.dispatch.clone();
    case.receipt.input_identity = case.dispatched.input_identity.clone();
    case.receipt.dependency_token = case.dispatched.dependency_token.clone();
    case.receipt.run = case.dispatched.invocation.run.clone();
    case.state = CommandState::dispatched(&case.dispatched);
    case.view = View::at_dispatch(&case.dispatched);
}

/// Current state as the host would read it.
#[derive(Clone, Default)]
struct View {
    facts: BTreeMap<FactKey, u64>,
    predicates: BTreeMap<(String, String), Vec<MemberPin>>,
    epochs: BTreeMap<String, u64>,
}

impl View {
    /// Exactly the state the token pinned, plus the destination at its
    /// expected owner version.
    fn at_dispatch(dispatched: &DispatchedCommand) -> Self {
        let token = &dispatched.invocation.dependency;
        let mut view = Self::default();
        for pin in &token.facts {
            view.facts.insert(
                FactKey::new(pin.object_type.clone(), pin.key.clone()),
                pin.version,
            );
        }
        for pin in &token.predicates {
            view.predicates.insert(
                (pin.object_type.clone(), pin.predicate.clone()),
                pin.members.clone(),
            );
        }
        for pin in &token.collections {
            view.epochs.insert(pin.object_type.clone(), pin.epoch);
        }
        if let Some(destination) = &dispatched.invocation.destination {
            view.facts.insert(
                FactKey::new(destination.object_type.clone(), destination.key.clone()),
                destination.expected_owner_version,
            );
        }
        view
    }

    fn set(&mut self, object_type: &str, key: &str, version: u64) {
        self.facts.insert(FactKey::new(object_type, key), version);
    }
}

impl StateView for View {
    type Error = Infallible;

    fn fact_version(&self, key: &FactKey) -> Result<FactVersion, Infallible> {
        Ok(FactVersion(self.facts.get(key).copied().unwrap_or(0)))
    }

    fn predicate_members(&self, predicate: &PredicatePin) -> Result<Vec<MemberPin>, Infallible> {
        Ok(self
            .predicates
            .get(&(predicate.object_type.clone(), predicate.predicate.clone()))
            .cloned()
            .unwrap_or_default())
    }

    fn collection_epoch(&self, object_type: &str) -> Result<u64, Infallible> {
        Ok(self.epochs.get(object_type).copied().unwrap_or(0))
    }
}

/// The authorizer, with the command's authority revocable.
struct Authz {
    revoked: bool,
}

impl ResultAuthorizer for Authz {
    fn authorize(&self, request: &ResultAuthzRequest<'_>) -> Result<(), ResultDenial> {
        assert_eq!(request.tenant, TENANT);
        if self.revoked {
            Err(ResultDenial {
                reason: "authority.revoked".to_owned(),
            })
        } else {
            Ok(())
        }
    }
}

const ALLOW: Authz = Authz { revoked: false };

fn context(policy: ReconcilePolicy) -> ReconcileContext<'static> {
    ReconcileContext {
        tenant: TENANT,
        world: "main",
        now: LogicalTime::from_millis(NOW),
        policy,
    }
}

fn run_with(
    case: &Case,
    receipt: &FunctionReceipt,
    context: &ReconcileContext<'_>,
    authz: &Authz,
) -> Reconciliation {
    let command = CommandId(receipt.command_id.clone());
    let completion = Completion {
        receipt,
        command: &command,
        generation: Generation(receipt.run.generation),
        authenticated_executor: Some(receipt.provenance.executor.as_str()),
    };
    reconcile(
        context,
        &case.dispatched,
        &case.state,
        &completion,
        &ResultChecks {
            authorizer: authz,
            validator: &case.contract,
        },
        &case.view,
    )
    .expect("reconciles")
}

fn run(case: &Case) -> Reconciliation {
    run_with(
        case,
        &case.receipt,
        &context(ReconcilePolicy::DEFAULT),
        &ALLOW,
    )
}

fn accepted(reconciliation: Reconciliation) -> Acceptance {
    match reconciliation {
        Reconciliation::Accepted(acceptance) => acceptance,
        Reconciliation::Refused(refused) => panic!("refused: {refused:?}"),
    }
}

/// A refusal never carries an acceptance; its state never records one.
fn refused(reconciliation: Reconciliation) -> Refused {
    match reconciliation {
        Reconciliation::Refused(refused) => {
            assert_eq!(
                refused.record.classification,
                Some(refused.classification),
                "the record carries the classification"
            );
            refused
        }
        Reconciliation::Accepted(acceptance) => panic!("accepted: {acceptance:?}"),
    }
}

fn expect(fact: (&str, &str, u64)) -> norn_kernel::FactExpectation {
    norn_kernel::FactExpectation {
        key: FactKey::new(fact.0, fact.1),
        expected: FactVersion(fact.2),
    }
}

// ---------------------------------------------------------------- freshness

#[test]
fn result_reconciliation_accepts_a_fresh_result_with_its_cas() {
    let case = case("wasm");
    let acceptance = accepted(run(&case));
    assert_eq!(
        acceptance.reads,
        vec![
            expect(("crm.Conversation", "conv-42", 7)),
            expect(("crm.Message", "msg-42", 1)),
        ]
    );
    assert!(acceptance.requires_cell_lane, "a predicate pin");
    assert_eq!(acceptance.destination, None);
    let recorded = acceptance.state.accepted.expect("accepted");
    assert_eq!(
        recorded.result_identity,
        case.receipt.result_identity().expect("identity")
    );
    assert_eq!(acceptance.record.classification, None);
}

#[test]
fn result_reconciliation_tolerates_unrelated_commits() {
    let mut case = case("wasm");
    case.view.set("crm.Conversation", "conv-99", 40);
    case.view.set("crm.Message", "msg-7", 3);
    case.view.epochs.insert("crm.Message".to_owned(), 912);
    case.view.predicates.insert(
        ("crm.Message".to_owned(), "conversation-archived".to_owned()),
        vec![MemberPin {
            key: "msg-1".to_owned(),
            version: 2,
        }],
    );
    accepted(run(&case));
}

#[test]
fn result_reconciliation_refuses_a_relevant_state_change_as_stale() {
    let mut case = case("wasm");
    case.view.set("crm.Conversation", "conv-42", 8);
    let refused = refused(run(&case));
    assert_eq!(refused.classification, Classification::Stale);
    assert_eq!(
        refused.reason,
        RefusalReason::RelevantStateChanged {
            changed: vec!["crm.Conversation/conv-42".to_owned()],
        }
    );
    assert_eq!(refused.reschedule, Some(Generation(2)));
    assert_eq!(refused.state.generation, Generation(2));
    assert_eq!(refused.state.reschedules, 1);
    assert_eq!(refused.state.accepted, None);
}

#[test]
fn result_reconciliation_detects_phantom_insert_and_delete_under_a_predicate() {
    let key = (
        "crm.Message".to_owned(),
        "conversation-unclassified".to_owned(),
    );
    let mut inserted = case("wasm");
    inserted
        .view
        .predicates
        .get_mut(&key)
        .expect("pinned")
        .push(MemberPin {
            key: "msg-43".to_owned(),
            version: 1,
        });
    let mut deleted = case("wasm");
    deleted
        .view
        .predicates
        .get_mut(&key)
        .expect("pinned")
        .clear();
    for (name, case) in [("insert", inserted), ("delete", deleted)] {
        let refused = refused(run(&case));
        assert_eq!(refused.classification, Classification::Stale, "{name}");
        assert_eq!(
            refused.reason,
            RefusalReason::RelevantStateChanged {
                changed: vec!["predicate:crm.Message/conversation-unclassified".to_owned()],
            },
            "{name}: the row versions are unchanged; only membership moved"
        );
    }
}

#[test]
fn result_reconciliation_honours_a_conservative_collection_epoch() {
    let mut case = case("wasm");
    repin(&mut case, |invocation| {
        invocation.dependency.collections.push(CollectionPin {
            object_type: "crm.Contact".to_owned(),
            epoch: 31,
        });
    });
    accepted(run(&case));
    case.view.epochs.insert("crm.Contact".to_owned(), 32);
    let refused = refused(run(&case));
    assert_eq!(refused.classification, Classification::Stale);
    assert_eq!(
        refused.reason,
        RefusalReason::RelevantStateChanged {
            changed: vec!["collection:crm.Contact".to_owned()],
        }
    );
}

#[test]
fn result_reconciliation_reschedules_stale_results_boundedly() {
    let mut case = case("wasm");
    case.view.set("crm.Conversation", "conv-42", 8);
    let policy = ReconcilePolicy {
        max_reschedules: 1,
        ..ReconcilePolicy::DEFAULT
    };
    let first = refused(run_with(&case, &case.receipt, &context(policy), &ALLOW));
    assert_eq!(first.reschedule, Some(Generation(2)));

    // The duplicate stale delivery of generation 1 coalesces: it is fenced
    // by the reschedule, not rescheduled again.
    let mut after = case.clone();
    after.state = first.state;
    let duplicate = refused(run_with(&after, &after.receipt, &context(policy), &ALLOW));
    assert_eq!(duplicate.classification, Classification::Superseded);
    assert_eq!(
        duplicate.reason,
        RefusalReason::Fenced {
            current: Generation(2)
        }
    );
    assert_eq!(duplicate.reschedule, None);

    // The rescheduled generation is stale again: the budget is spent.
    repin(&mut after, |invocation| invocation.run.generation = 2);
    after.state.reschedules = 1;
    after.view.set("crm.Conversation", "conv-42", 9);
    let exhausted = refused(run_with(&after, &after.receipt, &context(policy), &ALLOW));
    assert_eq!(exhausted.classification, Classification::Stale);
    assert_eq!(exhausted.reschedule, None, "never an unbounded retry loop");
    assert_eq!(exhausted.state, after.state);
}

// ---------------------------------------------------- authorization, fences

#[test]
fn result_reconciliation_rechecks_authorization_at_completion() {
    let case = case("wasm");
    let refused = refused(run_with(
        &case,
        &case.receipt,
        &context(ReconcilePolicy::DEFAULT),
        &Authz { revoked: true },
    ));
    assert_eq!(refused.classification, Classification::Rejected);
    assert_eq!(
        refused.reason,
        RefusalReason::Unauthorized {
            reason: "authority.revoked".to_owned()
        }
    );
    assert_eq!(refused.reschedule, None);
    assert_eq!(refused.state, case.state, "nothing advances");
}

#[test]
fn result_reconciliation_fences_cancelled_and_superseded_generations() {
    let mut cancelled = case("wasm");
    cancelled.state = cancelled.state.cancel().expect("cancel");
    let refused_cancel = refused(run(&cancelled));
    assert_eq!(refused_cancel.classification, Classification::Superseded);
    assert_eq!(refused_cancel.reason, RefusalReason::Cancelled);

    // A late attempt of generation 1 after the command was re-issued at 2.
    let mut reissued = case("wasm");
    let late = reissued.receipt.clone();
    repin(&mut reissued, |invocation| invocation.run.generation = 2);
    let refused_late = refused(run_with(
        &reissued,
        &late,
        &context(ReconcilePolicy::DEFAULT),
        &ALLOW,
    ));
    assert_eq!(refused_late.classification, Classification::Superseded);
    assert_eq!(
        refused_late.reason,
        RefusalReason::Fenced {
            current: Generation(2)
        }
    );
    // The current generation's own result is still accepted.
    accepted(run(&reissued));

    // A generation that was never dispatched is not this command's.
    let mut future = case("wasm");
    future.receipt.run.generation = 5;
    let refused_future = refused(run(&future));
    assert_eq!(refused_future.reason, RefusalReason::NotOwned);
}

#[test]
fn result_reconciliation_accepts_one_terminal_result_under_duplicates_and_reordering() {
    let case = case("wasm");
    let mut second_attempt = case.receipt.clone();
    second_attempt.attempt = 2;
    second_attempt.resources.cpu_millis = 5;
    second_attempt.provenance.placement = "pool-eu-1/node-c".to_owned();
    let mut divergent = case.receipt.clone();
    divergent.attempt = 2;
    if let FunctionOutcome::Success(success) = &mut divergent.outcome {
        success.summary.insert(
            "intent".to_owned(),
            RuntimeValue::String("order.cancel".to_owned()),
        );
        success.output_manifest =
            output_manifest_digest(&success.outputs, &success.summary).expect("digest");
    }

    let deliveries = [&case.receipt, &second_attempt, &divergent, &case.receipt];
    // Every arrival order: exactly one accepted terminal result, and the
    // rest are duplicates (same result identity) or superseded (another).
    let orders: [[usize; 4]; 6] = [
        [0, 1, 2, 3],
        [1, 0, 3, 2],
        [2, 0, 1, 3],
        [3, 2, 1, 0],
        [1, 2, 0, 3],
        [2, 3, 0, 1],
    ];
    for order in orders {
        let mut state = case.state.clone();
        let mut accepted_count = 0_u32;
        let mut winner = None;
        for index in order {
            let receipt = deliveries[index];
            let current = Case {
                state: state.clone(),
                ..case.clone()
            };
            match run_with(
                &current,
                receipt,
                &context(ReconcilePolicy::DEFAULT),
                &ALLOW,
            ) {
                Reconciliation::Accepted(acceptance) => {
                    accepted_count = accepted_count.saturating_add(1);
                    winner = Some(receipt.result_identity().expect("identity"));
                    state = acceptance.state;
                }
                Reconciliation::Refused(refused) => {
                    assert_eq!(refused.reason, RefusalReason::AlreadyAccepted, "{order:?}");
                    let same =
                        winner.as_ref() == Some(&receipt.result_identity().expect("identity"));
                    let expected = if same {
                        Classification::Duplicate
                    } else {
                        Classification::Superseded
                    };
                    assert_eq!(refused.classification, expected, "{order:?}");
                    assert_eq!(refused.state, state, "a refusal never changes the state");
                }
            }
        }
        assert_eq!(accepted_count, 1, "{order:?}");
    }
}

// ------------------------------------------------ destination CAS (image A/B)

#[test]
fn result_reconciliation_supersedes_image_a_when_the_document_now_holds_b() {
    let mut case = case("container");
    // The document moved from image A (owner version 5) to image B (6)
    // while the thumbnail of A was being computed.
    case.view.set("dam.Document", "doc-9", 6);
    let refused = refused(run(&case));
    assert_eq!(refused.classification, Classification::Superseded);
    assert_eq!(
        refused.reason,
        RefusalReason::DestinationMoved {
            expected: 5,
            current: 6
        }
    );
    assert_eq!(
        refused.reschedule, None,
        "the new image has its own command"
    );
}

#[test]
fn result_reconciliation_applies_destination_cas_in_the_accepting_commit() {
    let case = case("container");
    let acceptance = accepted(run(&case));
    assert_eq!(
        acceptance.destination,
        Some(expect(("dam.Document", "doc-9", 5)))
    );
    assert!(acceptance.reads.is_empty());
    assert!(!acceptance.requires_cell_lane);

    let thumbnail = |expected: u64| FactWrite {
        key: FactKey::new("dam.Document", "doc-9"),
        expected: FactVersion(expected),
        mutation: FactMutation::Put {
            values: RuntimeObject::new(),
        },
    };
    let commit = acceptance
        .commit::<(), (), ()>(vec![thumbnail(5)], Vec::new(), Vec::new())
        .expect("the write carries the CAS");
    assert!(commit.reads.is_empty(), "the write itself is the CAS");
    assert_eq!(commit.writes[0].expected, FactVersion(5));
    let conflict = acceptance
        .commit::<(), (), ()>(vec![thumbnail(6)], Vec::new(), Vec::new())
        .expect_err("a write at another version contradicts the reconciliation");
    assert_eq!(conflict.pinned, FactVersion(5));

    // Without a write the expectation stays a read.
    let commit = acceptance
        .commit::<(), (), ()>(Vec::new(), Vec::new(), Vec::new())
        .expect("commit");
    assert_eq!(commit.reads, vec![expect(("dam.Document", "doc-9", 5))]);
}

#[test]
fn result_reconciliation_refuses_a_receipt_of_other_inputs() {
    let mut case = case("container");
    case.receipt.input_identity = Digest(format!("sha256:{}", "a".repeat(64)));
    let refused = refused(run(&case));
    assert_eq!(refused.classification, Classification::Rejected);
    assert_eq!(refused.reason, RefusalReason::InputsDiffer);
}

#[test]
fn result_reconciliation_keeps_append_only_observations() {
    let mut case = case("effectful");
    case.view.set("crm.Conversation", "conv-42", 99);
    case.view.epochs.insert("crm.Message".to_owned(), 1_000);
    let acceptance = accepted(run(&case));
    assert!(acceptance.reads.is_empty() && acceptance.destination.is_none());
}

// ------------------------------------------- deadline, binding, provenance

#[test]
fn result_reconciliation_applies_the_deadline_policy() {
    let case = case("wasm");
    let late = ReconcileContext {
        now: LogicalTime::from_millis(1_790_000_005_001),
        ..context(ReconcilePolicy::DEFAULT)
    };
    let refused = refused(run_with(&case, &case.receipt, &late, &ALLOW));
    assert_eq!(refused.classification, Classification::Superseded);
    assert_eq!(
        refused.reason,
        RefusalReason::DeadlinePassed {
            deadline: 1_790_000_005_000
        }
    );
    let tolerant = ReconcileContext {
        policy: ReconcilePolicy {
            accept_late: true,
            ..ReconcilePolicy::DEFAULT
        },
        ..late
    };
    accepted(run_with(&case, &case.receipt, &tolerant, &ALLOW));
}

#[test]
fn result_reconciliation_rejects_unbound_or_unauthenticated_receipts() {
    let case = case("wasm");
    let mut forged = case.receipt.clone();
    forged.dispatch = Digest(format!("sha256:{}", "b".repeat(64)));
    let binding = refused(run_with(
        &case,
        &forged,
        &context(ReconcilePolicy::DEFAULT),
        &ALLOW,
    ));
    assert_eq!(binding.classification, Classification::Rejected);
    assert_eq!(
        binding.reason,
        RefusalReason::BindingMismatch {
            member: "dispatch".to_owned()
        }
    );

    let command = CommandId(case.receipt.command_id.clone());
    for executor in [None, Some("pool.rogue")] {
        let completion = Completion {
            receipt: &case.receipt,
            command: &command,
            generation: Generation(1),
            authenticated_executor: executor,
        };
        let outcome = reconcile(
            &context(ReconcilePolicy::DEFAULT),
            &case.dispatched,
            &case.state,
            &completion,
            &ResultChecks {
                authorizer: &ALLOW,
                validator: &case.contract,
            },
            &case.view,
        )
        .expect("reconciles");
        assert_eq!(refused(outcome).reason, RefusalReason::Unauthenticated);
    }

    let other_tenant = ReconcileContext {
        tenant: "tenant_globex",
        ..context(ReconcilePolicy::DEFAULT)
    };
    let scope = refused(run_with(&case, &case.receipt, &other_tenant, &ALLOW));
    assert!(matches!(scope.reason, RefusalReason::OutOfScope { .. }));
    let fork = ReconcileContext {
        world: "fork-what-if-1",
        ..context(ReconcilePolicy::DEFAULT)
    };
    let in_fork = refused(run_with(&case, &case.receipt, &fork, &ALLOW));
    assert!(matches!(in_fork.reason, RefusalReason::OutOfScope { .. }));
}

#[test]
fn result_reconciliation_rejects_receipts_that_break_their_contract() {
    let case = case("wasm");
    let mut zero_attempt = case.receipt.clone();
    zero_attempt.attempt = 0;
    let mut other_runtime = case.receipt.clone();
    other_runtime.provenance.runtime = Digest(format!("sha256:{}", "c".repeat(64)));
    let mut tampered = case.receipt.clone();
    if let FunctionOutcome::Success(success) = &mut tampered.outcome {
        success.summary.insert(
            "intent".to_owned(),
            RuntimeValue::String("refund".to_owned()),
        );
    }
    for (receipt, reason) in [
        (&zero_attempt, "function.receipt.attempt_out_of_range"),
        (&other_runtime, "function.receipt.dispatch_mismatch"),
        (&tampered, "function.receipt.output_manifest_mismatch"),
    ] {
        let refused = refused(run_with(
            &case,
            receipt,
            &context(ReconcilePolicy::DEFAULT),
            &ALLOW,
        ));
        assert_eq!(refused.classification, Classification::Rejected);
        assert_eq!(
            refused.reason,
            RefusalReason::InvalidReceipt {
                reason: reason.to_owned()
            }
        );
        assert_eq!(refused.state, case.state, "nothing advances");
    }
}

// --------------------------------------- CAS on the in-memory reference host

type Cell = MemoryCell<String, FunctionInvocation, FunctionReceipt>;

/// A host view over a [`MemoryCell`]: objects from the cell, predicate
/// membership supplied by the test (the cell has no query engine).
struct HostView<'a> {
    cell: &'a Cell,
    members: Vec<MemberPin>,
}

impl StateView for HostView<'_> {
    type Error = Infallible;

    fn fact_version(&self, key: &FactKey) -> Result<FactVersion, Infallible> {
        Ok(self.cell.load(key)?.version)
    }

    fn predicate_members(&self, _predicate: &PredicatePin) -> Result<Vec<MemberPin>, Infallible> {
        Ok(self.members.clone())
    }

    fn collection_epoch(&self, _object_type: &str) -> Result<u64, Infallible> {
        Ok(0)
    }
}

fn put(object_type: &str, key: &str, expected: u64, label: &str) -> FactWrite {
    let mut values = RuntimeObject::new();
    values.insert("label".to_owned(), RuntimeValue::String(label.to_owned()));
    FactWrite {
        key: FactKey::new(object_type, key),
        expected: FactVersion(expected),
        mutation: FactMutation::Put { values },
    }
}

/// A cell holding the conversation and message at version 1, with the
/// classify command dispatched and its result delivered.
fn dispatched_cell() -> (Cell, Case) {
    let mut case = case("wasm");
    repin(&mut case, |invocation| {
        invocation.dependency.facts[0].version = 1;
    });
    let receipt = case.receipt.clone();
    let mut cell: Cell = MemoryCell::new(move |_| receipt.clone());
    cell.commit(norn_kernel::CellCommit {
        writes: vec![
            put("crm.Conversation", "conv-42", 0, "open"),
            put("crm.Message", "msg-42", 0, "unclassified"),
        ],
        events: vec!["seeded".to_owned()],
        ..norn_kernel::CellCommit::empty()
    })
    .expect("seed");
    let command = Command {
        id: case.dispatched.command_id(),
        generation: case.dispatched.generation(),
        idempotency_key: IdempotencyKey(case.dispatched.dispatch.0.clone()),
        freshness: case.dispatched.freshness().expect("freshness"),
        deadline: Some(LogicalTime::from_millis(
            case.dispatched.invocation.deadline_millis,
        )),
        payload: case.dispatched.invocation.clone(),
    };
    cell.commit(norn_kernel::CellCommit {
        events: vec!["dispatched".to_owned()],
        effects: vec![Effect::Perform(command)],
        ..norn_kernel::CellCommit::empty()
    })
    .expect("dispatch");
    let performed = cell.perform().expect("perform").expect("a result");
    assert_eq!(performed.input, case.receipt);
    (cell, case)
}

fn reconcile_in(cell: &Cell, case: &Case) -> Reconciliation {
    let view = HostView {
        cell,
        members: case.dispatched.invocation.dependency.predicates[0]
            .members
            .clone(),
    };
    let command = case.dispatched.command_id();
    let completion = Completion {
        receipt: &case.receipt,
        command: &command,
        generation: Generation(1),
        authenticated_executor: Some("pool.wasm.eu-1"),
    };
    reconcile(
        &context(ReconcilePolicy::DEFAULT),
        &case.dispatched,
        &case.state,
        &completion,
        &ResultChecks {
            authorizer: &ALLOW,
            validator: &case.contract,
        },
        &view,
    )
    .expect("reconciles")
}

#[test]
fn result_reconciliation_commits_the_cas_with_the_accepted_outcome() {
    let (mut cell, case) = dispatched_cell();
    let acceptance = accepted(reconcile_in(&cell, &case));
    let commit = acceptance
        .commit(
            vec![put("crm.Message", "msg-42", 1, "order.modify")],
            vec!["accepted".to_owned()],
            Vec::new(),
        )
        .expect("the write expects the pinned version");
    assert_eq!(
        commit.reads,
        vec![expect(("crm.Conversation", "conv-42", 1))]
    );
    cell.commit(commit).expect("accepted");
    assert_eq!(
        cell.load(&FactKey::new("crm.Message", "msg-42"))
            .expect("load")
            .version,
        FactVersion(2)
    );
    assert!(
        cell.perform().expect("perform").is_none(),
        "the delivery is consumed"
    );
}

#[test]
fn result_reconciliation_cas_refuses_a_change_between_check_and_commit() {
    let (mut cell, case) = dispatched_cell();
    let acceptance = accepted(reconcile_in(&cell, &case));
    let commit = acceptance
        .commit(
            vec![put("crm.Message", "msg-42", 1, "order.modify")],
            vec!["accepted".to_owned()],
            Vec::new(),
        )
        .expect("commit");
    // A concurrent writer changes the conversation the token pinned.
    cell.commit(norn_kernel::CellCommit {
        writes: vec![put("crm.Conversation", "conv-42", 1, "closed")],
        events: vec!["closed".to_owned()],
        ..norn_kernel::CellCommit::empty()
    })
    .expect("concurrent write");
    let error = cell.commit(commit).expect_err("the CAS refuses");
    assert!(matches!(error, CommitError::Conflict { .. }), "{error:?}");
    assert_eq!(
        cell.load(&FactKey::new("crm.Message", "msg-42"))
            .expect("load")
            .version,
        FactVersion(1),
        "no transition from the stale result"
    );

    // Reconciled again, the result is stale: recorded, consumed, and the
    // command is re-issued at the next generation in the same commit.
    let refused = refused(reconcile_in(&cell, &case));
    assert_eq!(refused.classification, Classification::Stale);
    let next = refused.reschedule.expect("reschedule");
    let reissue = Command {
        id: case.dispatched.command_id(),
        generation: next,
        idempotency_key: IdempotencyKey(format!("{}#{}", case.dispatched.dispatch, next.0)),
        freshness: case.dispatched.freshness().expect("freshness"),
        deadline: None,
        payload: case.dispatched.invocation,
    };
    cell.commit(refused.commit(
        vec!["result.stale".to_owned()],
        vec![Effect::Perform(reissue)],
    ))
    .expect("recorded");
    assert_eq!(cell.outbox().len(), 1);
    assert_eq!(cell.outbox()[0].generation, Generation(2));
}

// ------------------------------------------------------------------ fan-in

fn children() -> BTreeMap<CommandId, Generation> {
    ["a", "b", "c"]
        .into_iter()
        .map(|id| (CommandId(id.to_owned()), Generation(1)))
        .collect()
}

/// Every permutation of `items`.
fn permutations(items: &[usize]) -> Vec<Vec<usize>> {
    if items.len() <= 1 {
        return vec![items.to_vec()];
    }
    let mut out = Vec::new();
    for (index, first) in items.iter().enumerate() {
        let mut rest = items.to_vec();
        rest.remove(index);
        for mut tail in permutations(&rest) {
            tail.insert(0, *first);
            out.push(tail);
        }
    }
    out
}

#[test]
fn result_reconciliation_fan_in_completes_exactly_once_in_any_order() {
    let deliveries = [
        (CommandId("a".to_owned()), Generation(1)),
        (CommandId("b".to_owned()), Generation(1)),
        (CommandId("c".to_owned()), Generation(1)),
        (CommandId("a".to_owned()), Generation(1)),
        (CommandId("x".to_owned()), Generation(1)),
        (CommandId("b".to_owned()), Generation(2)),
    ];
    for (policy, required) in [
        (FanInPolicy::All, 3),
        (FanInPolicy::Any, 1),
        (FanInPolicy::Quorum { required: 2 }, 2),
    ] {
        let fan_in = FanIn::new(children(), policy).expect("fan-in");
        assert_eq!(fan_in.required(), required);
        for order in permutations(&[0, 1, 2, 3, 4, 5]) {
            let mut state = FanInState::default();
            let mut completions = 0_u32;
            for index in &order {
                let (child, generation) = &deliveries[*index];
                match fan_in.count(&state, child, *generation) {
                    FanInStep::Counted { state: next } => state = next,
                    FanInStep::Complete { state: next } => {
                        completions = completions.saturating_add(1);
                        assert_eq!(next.accepted.len(), required, "{policy:?} {order:?}");
                        state = next;
                    }
                    FanInStep::Ignored { reason } => match child.0.as_str() {
                        "x" => assert_eq!(reason, FanInIgnored::UnknownChild),
                        _ if *generation == Generation(2) => {
                            assert_eq!(reason, FanInIgnored::OtherGeneration);
                        }
                        _ => assert!(
                            matches!(
                                reason,
                                FanInIgnored::Duplicate | FanInIgnored::AlreadyComplete
                            ),
                            "{reason:?}"
                        ),
                    },
                }
            }
            assert_eq!(completions, 1, "{policy:?} {order:?}");
            assert!(state.complete);
        }
    }
}

#[test]
fn result_reconciliation_fan_in_refuses_impossible_policies() {
    assert_eq!(
        FanIn::new(BTreeMap::new(), FanInPolicy::All),
        Err(FanInError::NoChildren)
    );
    for required in [0, 4] {
        assert_eq!(
            FanIn::new(children(), FanInPolicy::Quorum { required }),
            Err(FanInError::Quorum {
                required,
                children: 3
            })
        );
    }
}

// ------------------------------------------- the vertical flow (T7-EFX-3)

/// A JSON fixture value as an exact Norn value (fixture numbers are short
/// decimals that `serde_json`'s shortest text reproduces exactly).
fn exact(json: &serde_json::Value) -> norn_expr::Value {
    use norn_expr::Value;
    match json {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(flag) => Value::Bool(*flag),
        serde_json::Value::Number(number) => {
            Value::Number(number.to_string().parse().expect("exact number"))
        }
        serde_json::Value::String(text) => Value::String(text.clone()),
        serde_json::Value::Array(items) => Value::Array(items.iter().map(exact).collect()),
        serde_json::Value::Object(entries) => Value::Object(
            entries
                .iter()
                .map(|(key, item)| (key.clone(), exact(item)))
                .collect(),
        ),
    }
}

fn repo_json(path: &str) -> serde_json::Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(path);
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// One evaluation of the vertical flow's decide rule: its facts and commands
/// as JSON.
fn decide(
    decision: &norn_decisions::AdmittedDecision,
    run: Option<&serde_json::Value>,
    event: &serde_json::Value,
) -> (Vec<serde_json::Value>, Vec<serde_json::Value>) {
    let slice = match run {
        Some(run) => serde_json::json!({ "run": run }),
        None => serde_json::json!({}),
    };
    let slice = decision.validate_slice(exact(&slice)).expect("slice");
    let event = decision.type_event(exact(event)).expect("event");
    let evaluated = norn_decisions::evaluate(
        decision,
        &slice,
        &event,
        norn_expr::EvaluationBudget::default(),
    )
    .expect("evaluates");
    let encoded = norn_ir::runtime::encode(&evaluated.output).expect("encodes");
    let output: serde_json::Value =
        serde_json::from_str(&serde_json::to_string(&encoded).expect("text")).expect("json");
    let list = |name: &str| output[name].as_array().cloned().unwrap_or_default();
    (list("facts"), list("commands"))
}

/// Apply the generated Run facts (`create`, `set`, `to_<state>` transitions).
fn apply_run_facts(run: &mut Option<serde_json::Value>, facts: &[serde_json::Value]) {
    for fact in facts {
        match fact["op"].as_str().expect("op") {
            "create" => *run = Some(fact["values"].clone()),
            "set" => {
                let state = run.as_mut().expect("run");
                state[fact["property"].as_str().expect("property")] = fact["value"].clone();
            }
            "transition" => {
                let state = run.as_mut().expect("run");
                let to = fact["transition"].as_str().expect("transition");
                state["status"] =
                    serde_json::Value::String(to.strip_prefix("to_").expect("to_").to_owned());
            }
            other => panic!("unexpected fact {other}"),
        }
    }
}

/// The host's binding of a reconciled receipt to the flow: only an accepted
/// result becomes the decide rule's `result` event (the receipt's attempt and
/// its success summary as the step's output); a refused one is never bound.
fn bind(
    reconciliation: &Reconciliation,
    receipt: &FunctionReceipt,
    run_id: &str,
) -> Option<serde_json::Value> {
    let Reconciliation::Accepted(_) = reconciliation else {
        return None;
    };
    let FunctionOutcome::Success(success) = &receipt.outcome else {
        return None;
    };
    let summary: serde_json::Value =
        serde_json::from_str(&serde_json::to_string(&success.summary).expect("text"))
            .expect("json");
    Some(serde_json::json!({
        "kind": "result",
        "runId": run_id,
        "step": "classify",
        "attempt": receipt.attempt,
        "results": { "classify": summary },
    }))
}

/// T7-EFX-3 (#2786): a result of the vertical flow's `classify` perform is
/// bound into the Run only when reconciliation accepts it. The flow's command
/// is the T1-FN-1 invocation of `fixtures/function` (same Function, inputs,
/// run, and attempt bound); the receipt is reconciled once (accepted), then
/// redelivered (duplicate) and delivered against moved relevant state
/// (stale); only the accepted receipt becomes the rule's `result` event,
/// which completes the Run on its route.
#[test]
fn result_reconciliation_binds_only_accepted_results_into_the_vertical_flow() {
    let flow = repo_json("tools/norn-ts/fixtures/vertical/route-message.ir.json");
    let rule = flow
        .as_array()
        .expect("artifacts")
        .iter()
        .find(|artifact| artifact["kind"] == "rule")
        .expect("decide rule");
    let root: norn_ir::RuleRoot = serde_json::from_value(rule["payload"].clone()).expect("rule");
    let decision = norn_decisions::AdmittedDecision::admit(
        &root,
        &BTreeMap::new(),
        norn_decisions::DecisionBinding {
            profile_id: "norn-zen/6".to_owned(),
            environment_hash: "sha256:test".to_owned(),
        },
    )
    .expect("admitted");
    let manifest = repo_json("fixtures/function/manifest.wasm.json");
    let invocation = repo_json("fixtures/function/invocation.wasm.json");

    // Start: the flow dispatches the Function exactly as the invocation pins it.
    let case = case("wasm");
    let run_id = invocation["run"]["id"].as_str().expect("run id");
    let mut run_state = None;
    let (facts, commands) = decide(
        &decision,
        None,
        &serde_json::json!({ "kind": "start", "runId": run_id }),
    );
    apply_run_facts(&mut run_state, &facts);
    assert_eq!(commands.len(), 1);
    let command = &commands[0];
    assert_eq!(command["function"], manifest["functionId"]);
    assert_eq!(
        command["functionVersion"],
        serde_json::json!(manifest["version"].to_string())
    );
    assert_eq!(command["input"]["classify"], invocation["inputs"]);
    assert_eq!(command["attempt"], serde_json::json!(case.receipt.attempt));
    assert!(
        case.receipt.attempt <= case.dispatched.invocation.retry.max_attempts,
        "the flow's finite retry stays within the invocation's attempt bound"
    );
    let waiting = run_state.clone();

    // One delivery path for every receipt: reconcile, bind only an accepted
    // result as the rule's `result` event, and deliver it to the Run.
    let deliver = |reconciliation: &Reconciliation,
                   receipt: &FunctionReceipt,
                   run_state: &mut Option<serde_json::Value>|
     -> (Vec<serde_json::Value>, Vec<serde_json::Value>) {
        let Some(event) = bind(reconciliation, receipt, run_id) else {
            return (Vec::new(), Vec::new());
        };
        let (facts, commands) = decide(&decision, run_state.as_ref(), &event);
        apply_run_facts(run_state, &facts);
        (facts, commands)
    };

    // A same-attempt proposal refused for freshness, delivered while the Run
    // awaits it: classified stale, never bound, the Run unchanged.
    let mut moved = case.clone();
    moved.view.set("crm.Conversation", "conv-42", 8);
    let stale = run(&moved);
    assert!(
        matches!(&stale, Reconciliation::Refused(r) if r.classification == Classification::Stale)
    );
    let (facts, commands) = deliver(&stale, &case.receipt, &mut run_state);
    assert!(facts.is_empty() && commands.is_empty());
    assert_eq!(
        run_state, waiting,
        "a stale proposal does not advance the Run"
    );

    // The fresh delivery is accepted and bound: the Run completes on its route.
    let fresh = run_with(
        &case,
        &case.receipt,
        &context(ReconcilePolicy::DEFAULT),
        &ALLOW,
    );
    let accepted_state = match &fresh {
        Reconciliation::Accepted(acceptance) => acceptance.state.clone(),
        Reconciliation::Refused(refused) => panic!("refused: {refused:?}"),
    };
    let (_, commands) = deliver(&fresh, &case.receipt, &mut run_state);
    assert!(commands.is_empty());
    let completed = run_state.clone().expect("run");
    assert_eq!(completed["status"], "completed");
    assert_eq!(
        completed["slots"]["result"],
        serde_json::json!({ "route": "orders", "intent": "order.modify" })
    );

    // The redelivery is a duplicate: never bound, the Run unchanged.
    let mut redelivered = case.clone();
    redelivered.state = accepted_state;
    let duplicate = run(&redelivered);
    assert!(
        matches!(&duplicate, Reconciliation::Refused(r) if r.classification == Classification::Duplicate)
    );
    let (facts, commands) = deliver(&duplicate, &case.receipt, &mut run_state);
    assert!(facts.is_empty() && commands.is_empty());
    assert_eq!(run_state, Some(completed));
}
