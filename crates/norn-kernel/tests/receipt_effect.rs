#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    reason = "integration-test crate: fixture helpers unwrap and index"
)]

//! RECEIPT-EFFECT-1: a `fire_transition` with `acceptReceipt` consumes one
//! authenticated, accepted `FunctionReceipt` and fires the run's resolving
//! transition in one commit. Replay, a receipt of another run or generation,
//! and a refused transition each leave the run, its delivery and the ledger
//! unchanged.

use std::collections::BTreeMap;
use std::convert::Infallible;

use norn_ir::function::{
    CacheOrigin, Digest, ExecutionProvenance, FunctionOutcome, FunctionReceipt, ResourceUse,
    RunRef, SuccessOutcome,
};
use norn_ir::{ActionRoot, ObjectTypeDef, RuntimeValue};
use norn_kernel::cell_host::{JsonObject, JsonValue};
use norn_kernel::receipt_binding::{
    AcceptedReceiptStore, ReceiptBinding, ReceiptBindingError, ReceiptReference, ReceiptResolution,
    bind_receipt,
};
use norn_kernel::result_reconciliation::{Acceptance, AcceptedResult, CommandState, ResultRecord};
use norn_kernel::{
    ActionAuthorizer, ActionAuthzRequest, ActionCall, ActionDenial, ActionOutcome, CellCommit,
    CellHost, Command, CommandId, CommitError, CommitReceipt, Consumed, DecisionTurn, DueTimer,
    Effect, FactEvent, FactKey, FactMutation, FactState, FactVersion, FactWrite, FreshnessPolicy,
    Generation, IdempotencyKey, LinkKey, LinkState, LogicalTime, Observed, Performed, TypeCatalog,
    World, commit_action,
};
use norn_testkit::cell_host::MemoryCell;
use serde_json::json;

type Cell = MemoryCell<FactEvent, JsonObject, JsonObject>;

const RUN: &str = "flow.DecisionRun";

fn manifest() -> Digest {
    Digest(format!("sha256:{}", "d".repeat(64)))
}

fn run_types() -> Vec<ObjectTypeDef> {
    vec![serde_json::from_value(json!({
        "name": RUN,
        "identity": [{ "name": "primary", "properties": ["run_id"] }],
        "properties": [
            { "id": "run_id", "type": { "kind": "text" } },
            { "id": "status", "type": { "kind": "enum", "values": [
                { "value": "started" }, { "value": "awaiting_perform_ask" }, { "value": "decided" }
            ] }, "default": "started" },
            { "id": "choice", "type": { "kind": "text" }, "constraints": { "nullable": true } },
            { "id": "receipt", "type": { "kind": "text" }, "constraints": { "nullable": true } }
        ],
        "lifecycles": [{ "property": "status", "initial": ["started"], "terminal": ["decided"],
            "transitions": [
                { "id": "ask", "from": ["started"], "to": "awaiting_perform_ask" },
                { "id": "decide", "from": ["awaiting_perform_ask"], "to": "decided" }
            ] }]
    }))
    .unwrap()]
}

fn decide_action() -> ActionRoot {
    serde_json::from_value(json!({
        "actionId": "flow.decision_run.decide",
        "title": "Decide",
        "scopeRequired": ["orgId", "environmentId"],
        "idempotent": false,
        "ledgered": true,
        "inputSchema": { "type": "object",
            "properties": { "run_id": { "type": "string" }, "receipt": { "type": "string" } },
            "required": ["run_id", "receipt"], "additionalProperties": false },
        "outputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
        "writes": [{ "entityKind": RUN, "change": "modified" }],
        "effectProgram": {
            "kind": "fire_transition", "objectIdField": "run_id", "objectKind": RUN,
            "entityArtifactId": "flow.DecisionRun.entity", "entityArtifactVersion": "1.0.0",
            "lifecycle": "status", "transition": "decide",
            "acceptReceipt": {
                "referenceField": "receipt", "manifestDigest": manifest().0,
                "digestProperty": "receipt", "resultBindings": { "choice": "choice" }
            }
        }
    }))
    .unwrap()
}

struct Allow;

impl ActionAuthorizer for Allow {
    fn authorize(&self, _: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }
}

/// The issued receipt and its persisted acceptance, as a host keeps them.
#[derive(Clone)]
struct Issued {
    receipt: FunctionReceipt,
    acceptance: Acceptance,
}

/// A run cell whose receipt store authenticates by digest; consumption is the
/// cell's own pending-delivery state, never the store's.
struct ReceiptCell {
    cell: Cell,
    issued: BTreeMap<String, Issued>,
}

struct Store<'a>(&'a ReceiptCell, &'a Digest);

impl AcceptedReceiptStore for Store<'_> {
    type Error = Infallible;
    fn resolve(&self, reference: &Digest) -> Result<ReceiptResolution<'_>, Infallible> {
        Ok(match self.0.issued.get(&reference.0) {
            Some(issued) if &issued.receipt.manifest == self.1 => ReceiptResolution::Accepted {
                acceptance: &issued.acceptance,
                receipt: &issued.receipt,
            },
            _ => ReceiptResolution::Unknown,
        })
    }
}

impl CellHost for ReceiptCell {
    type Event = FactEvent;
    type Command = JsonObject;
    type Input = JsonObject;
    type Error = Infallible;

    fn load(&self, key: &FactKey) -> Result<FactState, Infallible> {
        self.cell.load(key)
    }
    fn resolve_accepted_receipt(
        &self,
        reference: &ReceiptReference,
        manifest: &Digest,
    ) -> Result<Option<ReceiptBinding<JsonObject>>, ReceiptBindingError<Infallible>> {
        bind_receipt(&Store(self, manifest), reference).map(Some)
    }
    fn load_link(&self, key: &LinkKey) -> Result<LinkState, Infallible> {
        self.cell.load_link(key)
    }
    fn commit(
        &mut self,
        commit: CellCommit<FactEvent, JsonObject, JsonObject>,
    ) -> Result<CommitReceipt, CommitError<Infallible>> {
        self.cell.commit(commit)
    }
    fn perform(&mut self) -> Result<Option<Performed<JsonObject>>, Infallible> {
        self.cell.perform()
    }
    fn observe(&mut self) -> Result<Option<Observed<JsonObject>>, Infallible> {
        self.cell.observe()
    }
    fn clock(&mut self) -> Result<Option<DueTimer<JsonObject>>, Infallible> {
        self.cell.clock()
    }
    fn publish(&mut self, receipt: &CommitReceipt) -> Result<(), Infallible> {
        self.cell.publish(receipt)
    }
}

fn scope() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("orgId".to_owned(), "org_demo".to_owned()),
        ("environmentId".to_owned(), "dev".to_owned()),
    ])
}

fn decide<'a>(action: &'a ActionRoot, run: &str, receipt: &str, key: &str) -> ActionCall<'a> {
    ActionCall {
        action,
        action_artifact_id: "flow.decision_run.decide.action",
        action_version: "1.0.0",
        scope: scope(),
        actor: "user:ada".to_owned(),
        input: JsonObject::from([
            ("run_id".to_owned(), JsonValue::String(run.to_owned())),
            ("receipt".to_owned(), JsonValue::String(receipt.to_owned())),
        ]),
        idempotency_key: key.to_owned(),
        object_type: None,
        turn: DecisionTurn {
            id: format!("turn-{key}"),
            now: LogicalTime::from_millis(1_000),
            world: World::Main,
            consumes: None,
        },
    }
}

/// A run waiting in `awaiting_perform_ask` with its decision command performed
/// (its result delivery pending) under `generation`.
fn waiting_run(cell: &mut ReceiptCell, run: &str, generation: u64) {
    cell.cell
        .commit(CellCommit {
            writes: vec![FactWrite {
                key: FactKey::new(RUN, run),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put {
                    values: JsonObject::from([
                        ("run_id".to_owned(), JsonValue::String(run.to_owned())),
                        (
                            "status".to_owned(),
                            JsonValue::String("awaiting_perform_ask".to_owned()),
                        ),
                    ]),
                },
            }],
            effects: vec![Effect::Perform(Command {
                id: CommandId(format!("{run}/ask")),
                generation: Generation(generation),
                idempotency_key: IdempotencyKey(format!("{run}/ask/{generation}")),
                freshness: FreshnessPolicy::AppendOnly,
                deadline: None,
                payload: JsonObject::new(),
            })],
            ..CellCommit::empty()
        })
        .unwrap();
    assert!(cell.cell.perform().unwrap().is_some());
}

/// The cell issues an accepted decision receipt for `run`'s command.
fn issue(cell: &mut ReceiptCell, run: &str, generation: u64, choice: &str) -> String {
    let summary = BTreeMap::from([("choice".to_owned(), RuntimeValue::String(choice.to_owned()))]);
    issue_summary(cell, run, generation, summary)
}

fn issue_summary(
    cell: &mut ReceiptCell,
    run: &str,
    generation: u64,
    summary: norn_ir::RuntimeObject,
) -> String {
    let receipt = FunctionReceipt {
        command_id: format!("{run}/ask"),
        run: RunRef {
            id: run.to_owned(),
            generation,
        },
        attempt: 1,
        dispatch: Digest(format!("sha256:{}", "1".repeat(64))),
        manifest: manifest(),
        execution_profile: Digest(format!("sha256:{}", "2".repeat(64))),
        input_identity: Digest(format!("sha256:{}", "3".repeat(64))),
        dependency_token: Digest(format!("sha256:{}", "4".repeat(64))),
        outcome: FunctionOutcome::Success(SuccessOutcome {
            output_manifest: norn_ir::function::output_manifest_digest(&[], &summary).unwrap(),
            outputs: Vec::new(),
            summary,
        }),
        provenance: ExecutionProvenance {
            executor: "cell".to_owned(),
            placement: "cell".to_owned(),
            runtime: Digest(format!("sha256:{}", "5".repeat(64))),
        },
        resources: ResourceUse {
            cpu_millis: 0,
            wall_millis: 0,
            peak_memory_bytes: 0,
            output_bytes: 0,
            fuel: None,
        },
        cache: CacheOrigin::Miss {},
    };
    let digest = receipt.content_digest().unwrap();
    let result_identity = receipt.result_identity().unwrap();
    let acceptance = Acceptance {
        consumed: Consumed::Result {
            command: CommandId(format!("{run}/ask")),
            generation: Generation(generation),
        },
        reads: Vec::new(),
        destination: None,
        requires_cell_lane: false,
        record: ResultRecord {
            command: CommandId(format!("{run}/ask")),
            generation: Generation(generation),
            attempt: 1,
            receipt: digest.clone(),
            result_identity: result_identity.clone(),
            classification: None,
            reason: None,
        },
        state: CommandState {
            generation: Generation(generation),
            cancelled: false,
            accepted: Some(AcceptedResult {
                generation: Generation(generation),
                result_identity,
                receipt: digest.clone(),
            }),
            reschedules: 0,
        },
    };
    cell.issued.insert(
        digest.0.clone(),
        Issued {
            receipt,
            acceptance,
        },
    );
    digest.0
}

fn fresh() -> ReceiptCell {
    ReceiptCell {
        cell: Cell::new(|command: &Command<JsonObject>| command.payload.clone()),
        issued: BTreeMap::new(),
    }
}

fn value(cell: &ReceiptCell, run: &str, property: &str) -> Option<JsonValue> {
    cell.load(&FactKey::new(RUN, run))
        .unwrap()
        .values
        .and_then(|values| values.get(property).cloned())
}

fn is_pending(cell: &mut ReceiptCell, run: &str) -> bool {
    cell.cell
        .perform()
        .unwrap()
        .is_some_and(|performed| performed.command.0 == format!("{run}/ask"))
}

#[test]
fn accepted_receipt_and_transition_commit_together_once() {
    let types = run_types();
    let catalog = TypeCatalog::new(&types);
    let action = decide_action();
    let mut cell = fresh();
    waiting_run(&mut cell, "run-1", 1);
    let receipt = issue(&mut cell, "run-1", 1, "approve");
    let ledger = cell.cell.ledger().len();

    let outcome = commit_action(
        &mut cell,
        &catalog,
        &decide(&action, "run-1", &receipt, "k1"),
        &Allow,
    )
    .unwrap();
    assert!(matches!(outcome, ActionOutcome::Committed { .. }));
    assert_eq!(
        value(&cell, "run-1", "status"),
        Some(JsonValue::String("decided".to_owned()))
    );
    assert_eq!(
        value(&cell, "run-1", "choice"),
        Some(JsonValue::String("approve".to_owned()))
    );
    assert_eq!(
        value(&cell, "run-1", "receipt"),
        Some(JsonValue::String(receipt.clone()))
    );
    assert!(
        !is_pending(&mut cell, "run-1"),
        "the delivery was consumed by the transition's commit"
    );
    assert_eq!(cell.cell.ledger().len(), ledger + 1, "one commit");

    // An exact retry replays the original outcome; nothing new commits.
    let retry = commit_action(
        &mut cell,
        &catalog,
        &decide(&action, "run-1", &receipt, "k1"),
        &Allow,
    )
    .unwrap();
    assert!(matches!(retry, ActionOutcome::Replayed { .. }));
    assert_eq!(cell.cell.ledger().len(), ledger + 1);
}

#[test]
fn a_consumed_receipt_is_refused_under_a_new_call() {
    let types = run_types();
    let catalog = TypeCatalog::new(&types);
    let action = decide_action();
    let mut cell = fresh();
    waiting_run(&mut cell, "run-1", 1);
    let receipt = issue(&mut cell, "run-1", 1, "approve");
    commit_action(
        &mut cell,
        &catalog,
        &decide(&action, "run-1", &receipt, "k1"),
        &Allow,
    )
    .unwrap();
    let ledger = cell.cell.ledger().len();
    let refused = commit_action(
        &mut cell,
        &catalog,
        &decide(&action, "run-1", &receipt, "k2"),
        &Allow,
    )
    .unwrap_err();
    // The run already left its waiting state; nothing commits.
    assert_ne!(refused.code(), "action.unsupported_effect", "{refused}");
    assert_eq!(cell.cell.ledger().len(), ledger);
}

#[test]
fn replay_against_a_reissued_waiting_run_is_refused_by_the_delivery_fence() {
    // The run is back in its waiting state (as after a retry step), but the
    // receipt's delivery was consumed: the host refuses the second consumption
    // and the transition does not fire.
    let types = run_types();
    let catalog = TypeCatalog::new(&types);
    let action = decide_action();
    let mut cell = fresh();
    waiting_run(&mut cell, "run-1", 1);
    let receipt = issue(&mut cell, "run-1", 1, "approve");
    commit_action(
        &mut cell,
        &catalog,
        &decide(&action, "run-1", &receipt, "k1"),
        &Allow,
    )
    .unwrap();
    let version = cell.load(&FactKey::new(RUN, "run-1")).unwrap().version;
    cell.cell
        .commit(CellCommit {
            writes: vec![FactWrite {
                key: FactKey::new(RUN, "run-1"),
                expected: version,
                mutation: FactMutation::Put {
                    values: JsonObject::from([
                        ("run_id".to_owned(), JsonValue::String("run-1".to_owned())),
                        (
                            "status".to_owned(),
                            JsonValue::String("awaiting_perform_ask".to_owned()),
                        ),
                    ]),
                },
            }],
            ..CellCommit::empty()
        })
        .unwrap();
    let ledger = cell.cell.ledger().len();
    let refused = commit_action(
        &mut cell,
        &catalog,
        &decide(&action, "run-1", &receipt, "k2"),
        &Allow,
    )
    .unwrap_err();
    assert_eq!(refused.code(), "action.receipt_not_pending", "{refused}");
    assert_eq!(cell.cell.ledger().len(), ledger);
    assert_eq!(
        value(&cell, "run-1", "status"),
        Some(JsonValue::String("awaiting_perform_ask".to_owned()))
    );
    assert_eq!(value(&cell, "run-1", "choice"), None);
}

#[test]
fn a_receipt_of_another_run_or_generation_is_refused() {
    let types = run_types();
    let catalog = TypeCatalog::new(&types);
    let action = decide_action();
    let mut cell = fresh();
    waiting_run(&mut cell, "run-1", 1);
    let other_run = issue(&mut cell, "run-2", 1, "approve");
    let stale = issue(&mut cell, "run-1", 7, "approve");
    let ledger = cell.cell.ledger().len();
    for (receipt, code) in [
        (&other_run, "action.receipt_run_mismatch"),
        (&stale, "action.receipt_not_pending"),
    ] {
        let refused = commit_action(
            &mut cell,
            &catalog,
            &decide(&action, "run-1", receipt, receipt),
            &Allow,
        )
        .unwrap_err();
        assert_eq!(refused.code(), code, "{refused}");
        assert_eq!(cell.cell.ledger().len(), ledger);
        assert!(
            is_pending(&mut cell, "run-1"),
            "the run's delivery stays pending"
        );
        assert_eq!(value(&cell, "run-1", "choice"), None);
    }
    // A receipt whose acceptance disagrees with its own run generation.
    let digest = issue(&mut cell, "run-1", 1, "approve");
    cell.issued
        .get_mut(&digest)
        .unwrap()
        .acceptance
        .state
        .generation = Generation(2);
    let refused = commit_action(
        &mut cell,
        &catalog,
        &decide(&action, "run-1", &digest, "k-gen"),
        &Allow,
    )
    .unwrap_err();
    assert_eq!(refused.code(), "action.receipt_binding_mismatch");
    assert_eq!(cell.cell.ledger().len(), ledger);
}

#[test]
fn no_transition_without_consumption_and_no_consumption_without_transition() {
    let types = run_types();
    let catalog = TypeCatalog::new(&types);
    let action = decide_action();

    // Transition refused (run not waiting): the delivery stays pending.
    let mut cell = fresh();
    waiting_run(&mut cell, "run-1", 1);
    let receipt = issue(&mut cell, "run-1", 1, "approve");
    let version = cell.load(&FactKey::new(RUN, "run-1")).unwrap().version;
    cell.cell
        .commit(CellCommit {
            writes: vec![FactWrite {
                key: FactKey::new(RUN, "run-1"),
                expected: version,
                mutation: FactMutation::Put {
                    values: JsonObject::from([
                        ("run_id".to_owned(), JsonValue::String("run-1".to_owned())),
                        ("status".to_owned(), JsonValue::String("started".to_owned())),
                    ]),
                },
            }],
            ..CellCommit::empty()
        })
        .unwrap();
    let ledger = cell.cell.ledger().len();
    assert!(
        commit_action(
            &mut cell,
            &catalog,
            &decide(&action, "run-1", &receipt, "k1"),
            &Allow
        )
        .is_err()
    );
    assert_eq!(cell.cell.ledger().len(), ledger);
    assert!(
        is_pending(&mut cell, "run-1"),
        "no consumption without the transition"
    );

    // Unknown receipt, a body instead of a reference, and a result that does
    // not fit the bindings: no transition fires.
    let mut cell = fresh();
    waiting_run(&mut cell, "run-1", 1);
    let ledger = cell.cell.ledger().len();
    let unknown = format!("sha256:{}", "e".repeat(64));
    let refused = commit_action(
        &mut cell,
        &catalog,
        &decide(&action, "run-1", &unknown, "k-unknown"),
        &Allow,
    )
    .unwrap_err();
    assert_eq!(refused.code(), "action.receipt_unknown");
    let mut body = decide(&action, "run-1", "", "k-body");
    body.input.insert(
        "receipt".to_owned(),
        JsonValue::Object(JsonObject::from([(
            "choice".to_owned(),
            JsonValue::String("approve".to_owned()),
        )])),
    );
    assert!(commit_action(&mut cell, &catalog, &body, &Allow).is_err());
    let digest = Digest(issue_summary(&mut cell, "run-1", 1, BTreeMap::new()));
    let refused = commit_action(
        &mut cell,
        &catalog,
        &decide(&action, "run-1", &digest.0, "k-type"),
        &Allow,
    )
    .unwrap_err();
    assert_eq!(refused.code(), "action.receipt_result_type");
    assert_eq!(cell.cell.ledger().len(), ledger);
    assert!(is_pending(&mut cell, "run-1"));
    assert_eq!(
        value(&cell, "run-1", "status"),
        Some(JsonValue::String("awaiting_perform_ask".to_owned()))
    );
}

#[test]
fn an_unsupported_host_refuses_receipt_effects() {
    let types = run_types();
    let catalog = TypeCatalog::new(&types);
    let action = decide_action();
    let mut cell = Cell::new(|command: &Command<JsonObject>| command.payload.clone());
    let refused = commit_action(
        &mut cell,
        &catalog,
        &decide(&action, "run-1", &format!("sha256:{}", "a".repeat(64)), "k"),
        &Allow,
    )
    .unwrap_err();
    assert!(matches!(
        refused.code(),
        "action.receipt_effect_unsupported" | "action.target_not_found"
    ));
    assert!(cell.ledger().is_empty());
}

#[test]
fn receipt_refusals_have_distinct_codes_and_stable_categories() {
    use norn_kernel::RefusalCategory;
    use norn_kernel::receipt_binding::ReceiptRefusal::{self, *};
    let all: [ReceiptRefusal; 10] = [
        BodyForbidden,
        ReferenceInvalid,
        EffectUnsupported,
        Unknown,
        Replayed,
        BindingMismatch,
        NotAccepted,
        ResultType,
        RunMismatch,
        NotPending,
    ];
    let codes: std::collections::BTreeSet<_> = all.iter().map(|reason| reason.code()).collect();
    assert_eq!(codes.len(), all.len(), "every refusal has its own code");
    assert!(codes.iter().all(|code| code.starts_with("action.receipt_")));
    assert_eq!(Unknown.category(), RefusalCategory::Visibility);
    assert_eq!(Replayed.category(), RefusalCategory::Conflict);
    assert_eq!(NotPending.category(), RefusalCategory::Conflict);
    for reason in all {
        let refusal: norn_kernel::ActionRefusal<Infallible> =
            norn_kernel::ActionRefusal::ReceiptRefused { reason };
        assert_eq!(refusal.code(), reason.code());
        assert_eq!(refusal.category(), reason.category());
    }
}

#[test]
fn evidence_never_consumes_a_delivery_other_than_its_own_command_and_generation() {
    // Sol S1 on #1043: generation-1 evidence whose `acceptance.consumed` names
    // the run's pending generation-2 delivery must not commit against it.
    let types = run_types();
    let catalog = TypeCatalog::new(&types);
    let action = decide_action();
    let mut cell = fresh();
    waiting_run(&mut cell, "run-1", 2);
    let ledger = cell.cell.ledger().len();
    let retargets = [
        Consumed::Result {
            command: CommandId("run-1/ask".to_owned()),
            generation: Generation(2),
        },
        Consumed::Result {
            command: CommandId("run-1/other".to_owned()),
            generation: Generation(1),
        },
    ];
    for (index, consumed) in retargets.into_iter().enumerate() {
        let digest = issue(&mut cell, "run-1", 1, "approve");
        cell.issued.get_mut(&digest).unwrap().acceptance.consumed = consumed;
        let refused = commit_action(
            &mut cell,
            &catalog,
            &decide(&action, "run-1", &digest, &format!("k-retarget-{index}")),
            &Allow,
        )
        .unwrap_err();
        assert_eq!(
            refused.code(),
            "action.receipt_binding_mismatch",
            "{refused}"
        );
        assert_eq!(cell.cell.ledger().len(), ledger, "nothing committed");
        assert_eq!(value(&cell, "run-1", "choice"), None);
        assert!(
            is_pending(&mut cell, "run-1"),
            "the generation-2 delivery stays pending"
        );
    }
    // The run's own generation-2 evidence still resolves it.
    let own = issue(&mut cell, "run-1", 2, "approve");
    commit_action(
        &mut cell,
        &catalog,
        &decide(&action, "run-1", &own, "k-own"),
        &Allow,
    )
    .unwrap();
    assert_eq!(
        value(&cell, "run-1", "status"),
        Some(JsonValue::String("decided".to_owned()))
    );
}
