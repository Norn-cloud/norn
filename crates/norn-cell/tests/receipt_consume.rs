//! RECEIPT-STORE-1 and RECEIPT-CONSUME-1 on the durable host. A carrier
//! delivery confirmation runs as a `norn.function/v1` command; its recorded
//! receipt is retained only through authenticated reconciliation
//! (`SqliteCell::retain_function_receipt`), and `SqliteCell` itself resolves
//! it for an action that names the receipt's digest. The delivery is
//! consumed in the transition's own SQLite commit: a crash before that
//! commit leaves the delivery pending and the run waiting; after it, a
//! reopened cell holds the transition and refuses a second consumption. The
//! in-memory host's tests live in `norn-kernel/tests/receipt_effect.rs`.
#![cfg(feature = "native")]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    missing_docs,
    reason = "durable receipt scenarios"
)]

mod common;

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::Path;

use norn_cell::export::Exporter;
use norn_cell::function_receipt::{
    FUNCTION_COMMAND_TYPE, FUNCTION_PAYLOAD_TYPE, ReceiptIntakeError,
};
use norn_cell::{CellError, Model, SqliteCell, StoreOptions};
use norn_ir::function::{
    CacheOrigin, Digest, ExecutionProvenance, FunctionInvocation, FunctionOutcome, FunctionReceipt,
    ResourceUse, SuccessOutcome,
};
use norn_ir::{ActionRoot, ObjectTypeDef, PropertyId, RuntimeValue, TransitionId};
use norn_kernel::cell_host::{JsonObject, JsonValue};
use norn_kernel::receipt_binding::{ReceiptBinding, ReceiptBindingError, ReceiptReference};
use norn_kernel::result_reconciliation::{
    Acceptance, CommandState, Completion, DispatchedCommand, InvalidReceipt, ReceiptValidator,
    RefusalReason, ResultAuthorizer, ResultAuthzRequest, ResultChecks, ResultDenial,
};
use norn_kernel::{
    ActionAuthorizer, ActionAuthzRequest, ActionCall, ActionDenial, ActionOutcome, CellCommit,
    CellHost, Command, CommandId, CommitError, CommitReceipt, Consumed, DecisionTurn, DueTimer,
    Effect, FactEvent, FactKey, FactMutation, FactState, FactVersion, FactWrite, FreshnessPolicy,
    Generation, IdempotencyKey, LedgerSeq, LinkKey, LinkState, LogicalTime, Observed, Performed,
    TimerId, TypeCatalog, World, commit_action,
};
use serde_json::json;

const RUN: &str = "flow.DeliveryCheck";
const SHIPMENT: &str = "flow.Shipment";
const SCOPE: &str = "org_demo/dev";
const OTHER_SCOPE: &str = "org_other/dev";
const EXECUTOR: &str = "pool.carrier-confirmations";
const DEADLINE: u64 = 1_000_000;
const PAST_DEADLINE: u64 = 1_000_001;
const WITHIN_DEADLINE: u64 = 500_000;

type Sqlite = SqliteCell<FactEvent, JsonObject, JsonObject>;

fn sha(fill: char) -> Digest {
    Digest(format!("sha256:{}", fill.to_string().repeat(64)))
}

fn manifest() -> Digest {
    sha('d')
}

fn text(id: &str, nullable: bool) -> serde_json::Value {
    if nullable {
        json!({"id": id, "type": {"kind": "text"}, "constraints": {"nullable": true}})
    } else {
        json!({"id": id, "type": {"kind": "text"}})
    }
}

/// The confirmation run, under a governing law: a held delivery is never
/// published.
fn run_types() -> Vec<ObjectTypeDef> {
    vec![
        serde_json::from_value(json!({
            "name": RUN,
            "identity": [{ "name": "primary", "properties": ["run_id"] }],
            "properties": [
                text("run_id", false),
                { "id": "status", "type": { "kind": "enum", "values": [
                    { "value": "started" }, { "value": "awaiting_perform_confirm" },
                    { "value": "confirmed" }
                ] }, "default": "started" },
                text("outcome", true),
                text("receipt", true)
            ],
            "lifecycles": [{ "property": "status", "initial": ["started"], "terminal": ["confirmed"],
                "transitions": [
                    { "id": "ask", "from": ["started"], "to": "awaiting_perform_confirm" },
                    { "id": "confirm", "from": ["awaiting_perform_confirm"], "to": "confirmed" }
                ] }],
            "invariants": [{
                "id": "never_publish_held",
                "expression": "(self.outcome ?? '') != 'held'",
                "message": "a held delivery is never published"
            }]
        }))
        .unwrap(),
    ]
}

/// The run type, the shipment its result is pinned to, the Function command
/// state, and the kernel's action-call record type, which every durable
/// action commit writes.
fn definitions() -> Vec<ObjectTypeDef> {
    let mut fields: Vec<serde_json::Value> =
        ["actionId", "actor", "turn", "transition", "from", "to"]
            .iter()
            .map(|id| text(id, false))
            .collect();
    fields.extend(
        [
            "actionArtifactId",
            "actionVersion",
            "effectKind",
            "ruleBinding",
        ]
        .iter()
        .map(|id| text(id, true)),
    );
    fields.extend(
        [
            "object",
            "input",
            "effectiveInput",
            "derivedValues",
            "preconditionOutcomes",
            "rowOperations",
        ]
        .iter()
        .map(
            |id| json!({"id": id, "type": {"kind": "json-ext"}, "constraints": {"nullable": true}}),
        ),
    );
    let mut definitions = run_types();
    definitions.extend(
        [
            json!({"name": SHIPMENT, "properties": [text("note", true)]}),
            json!({"name": FUNCTION_COMMAND_TYPE, "properties": [{"id": "state", "type": {"kind": "json-ext"}}]}),
            json!({"name": norn_kernel::ACTION_CALL_TYPE, "properties": fields}),
        ]
        .into_iter()
        .map(|definition| serde_json::from_value::<ObjectTypeDef>(definition).unwrap()),
    );
    definitions
}

fn confirm_action(manifest: &Digest) -> ActionRoot {
    serde_json::from_value(json!({
        "actionId": "flow.delivery_check.confirm",
        "title": "Confirm delivery",
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
            "entityArtifactId": "flow.DeliveryCheck.entity", "entityArtifactVersion": "1.0.0",
            "lifecycle": "status", "transition": "confirm",
            "acceptReceipt": {
                "referenceField": "receipt", "manifestDigest": manifest.0,
                "digestProperty": "receipt", "resultBindings": { "outcome": "outcome" }
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

/// The caller no longer holds authority over the run.
struct Deny;

impl ActionAuthorizer for Deny {
    fn authorize(&self, _: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Err(ActionDenial::Invisible)
    }
}

/// The receipt contract and the command authority's recheck at intake.
struct Contract;

impl ResultAuthorizer for Contract {
    fn authorize(&self, request: &ResultAuthzRequest<'_>) -> Result<(), ResultDenial> {
        if request.authority.id == "svc.delivery-confirmation" {
            Ok(())
        } else {
            Err(ResultDenial {
                reason: "authority revoked".to_owned(),
            })
        }
    }
}

impl ReceiptValidator for Contract {
    fn validate(
        &self,
        _dispatched: &DispatchedCommand,
        receipt: &FunctionReceipt,
    ) -> Result<(), InvalidReceipt> {
        if receipt.attempt == 1 && matches!(receipt.outcome, FunctionOutcome::Success(_)) {
            Ok(())
        } else {
            Err(InvalidReceipt {
                reason: "receipt contract".to_owned(),
            })
        }
    }
}

/// The command's authority was revoked.
struct Revoked;

impl ResultAuthorizer for Revoked {
    fn authorize(&self, _: &ResultAuthzRequest<'_>) -> Result<(), ResultDenial> {
        Err(ResultDenial {
            reason: "authority revoked".to_owned(),
        })
    }
}

/// A dispatch of the confirmation Function for `run`, pinned to `ship-1`
/// at version 1, under `tenant` and `world`.
fn dispatch(run: &str, tenant: &str, world: &str) -> DispatchedCommand {
    let invocation: FunctionInvocation = serde_json::from_value(json!({
        "tenant": tenant,
        "world": world,
        "authority": { "id": "svc.delivery-confirmation", "kind": "service" },
        "commandId": format!("{run}/confirm"),
        "run": { "id": run, "generation": 1 },
        "manifest": manifest().0,
        "inputs": { "trackingRef": format!("TRK-{run}") },
        "blobs": {},
        "provenanceCommit": 1,
        "dependency": {
            "facts": [{ "objectType": SHIPMENT, "key": "ship-1", "version": 1 }],
            "predicates": [],
            "collections": []
        },
        "freshness": "relevant_state",
        "destination": null,
        "deadlineMillis": DEADLINE,
        "retry": { "maxAttempts": 2, "maxCostUnits": 100 },
        "fencing": { "cancellationGeneration": 0, "token": format!("fence-{run}") }
    }))
    .unwrap();
    DispatchedCommand::pin(invocation, sha('7')).unwrap()
}

/// The performer's receipt for `dispatched`, reporting `outcome`.
fn receipt_for(dispatched: &DispatchedCommand, outcome: &str) -> FunctionReceipt {
    let summary = BTreeMap::from([(
        "outcome".to_owned(),
        RuntimeValue::String(outcome.to_owned()),
    )]);
    FunctionReceipt {
        command_id: dispatched.invocation.command_id.clone(),
        run: dispatched.invocation.run.clone(),
        attempt: 1,
        dispatch: dispatched.dispatch.clone(),
        manifest: dispatched.invocation.manifest.clone(),
        execution_profile: dispatched.execution_profile.clone(),
        input_identity: dispatched.input_identity.clone(),
        dependency_token: dispatched.dependency_token.clone(),
        outcome: FunctionOutcome::Success(SuccessOutcome {
            output_manifest: norn_ir::function::output_manifest_digest(&[], &summary).unwrap(),
            outputs: Vec::new(),
            summary,
        }),
        provenance: ExecutionProvenance {
            executor: EXECUTOR.to_owned(),
            placement: "carrier-pool/node-1".to_owned(),
            runtime: sha('5'),
        },
        resources: ResourceUse {
            cpu_millis: 1,
            wall_millis: 1,
            peak_memory_bytes: 0,
            output_bytes: 0,
            fuel: None,
        },
        cache: CacheOrigin::Miss {},
    }
}

fn object(receipt: &FunctionReceipt) -> JsonObject {
    let JsonValue::Object(object) = norn_ir::runtime::encode(receipt).unwrap() else {
        panic!("a receipt encodes as an object");
    };
    object
}

/// A cell whose outside world answers each command with its receipt, and
/// which rechecks command authority when it resolves a retained receipt.
fn open(path: &Path, scope: &str, performed: &[&FunctionReceipt]) -> Sqlite {
    let answers: BTreeMap<String, JsonObject> = performed
        .iter()
        .map(|receipt| (receipt.command_id.clone(), object(receipt)))
        .collect();
    let mut cell = Sqlite::open(
        path,
        Model::from_definitions(&definitions(), &[]).expect("models the run"),
        StoreOptions::default(),
        scope,
        move |command: &Command<JsonObject>| {
            answers
                .get(&command.id.0)
                .cloned()
                .unwrap_or_else(|| panic!("no answer for {}", command.id.0))
        },
    )
    .expect("opens the durable cell");
    cell.set_result_authorizer(Some(Box::new(Contract)));
    cell
}

fn put(key: FactKey, expected: FactVersion, values: &[(&str, &str)]) -> FactWrite {
    FactWrite {
        key,
        expected,
        mutation: FactMutation::Put {
            values: values
                .iter()
                .map(|(name, value)| ((*name).to_owned(), JsonValue::String((*value).to_owned())))
                .collect(),
        },
    }
}

/// A new run waiting on its confirmation: created in its initial state and
/// moved by `ask` in the same commit, so the recorded transition admits its
/// first, non-initial state (GRAPH-STATE-1).
fn awaiting_run(run: &str) -> (FactWrite, [FactEvent; 2]) {
    let key = FactKey::new(RUN, run);
    let events = [
        FactEvent::Created {
            key: key.clone(),
            values: JsonObject::from([
                ("run_id".to_owned(), JsonValue::String(run.to_owned())),
                ("status".to_owned(), JsonValue::String("started".to_owned())),
            ]),
        },
        FactEvent::Transitioned {
            key: key.clone(),
            lifecycle: PropertyId("status".into()),
            transition: TransitionId("ask".into()),
            from: "started".into(),
            to: "awaiting_perform_confirm".into(),
        },
    ];
    let write = put(
        key,
        FactVersion::ABSENT,
        &[("run_id", run), ("status", "awaiting_perform_confirm")],
    );
    (write, events)
}

/// The pinned shipment and runs waiting on their confirmation.
fn seed(cell: &mut Sqlite, runs: &[&str]) {
    let mut writes = vec![put(
        FactKey::new(SHIPMENT, "ship-1"),
        FactVersion::ABSENT,
        &[("note", "in transit")],
    )];
    let mut events = Vec::new();
    for &run in runs {
        let (write, recorded) = awaiting_run(run);
        writes.push(write);
        events.extend(recorded);
    }
    cell.commit(CellCommit {
        writes,
        events,
        ..CellCommit::empty()
    })
    .unwrap();
}

/// Enqueue `dispatched` as a `norn.function/v1` command with its command
/// state, then perform it: its receipt becomes the pending delivery.
fn perform(cell: &mut Sqlite, dispatched: &DispatchedCommand) {
    let state = norn_ir::runtime::encode(&CommandState::dispatched(dispatched)).unwrap();
    let invocation = norn_ir::runtime::encode(&dispatched.invocation).unwrap();
    cell.commit(CellCommit {
        writes: vec![FactWrite {
            key: FactKey::new(
                FUNCTION_COMMAND_TYPE,
                dispatched.invocation.command_id.as_str(),
            ),
            expected: FactVersion::ABSENT,
            mutation: FactMutation::Put {
                values: JsonObject::from([("state".to_owned(), state)]),
            },
        }],
        effects: vec![Effect::Perform(Command {
            id: dispatched.command_id(),
            generation: dispatched.generation(),
            idempotency_key: IdempotencyKey(format!("{}/1", dispatched.invocation.command_id)),
            freshness: dispatched.freshness().unwrap(),
            deadline: None,
            payload: JsonObject::from([
                (
                    "type".to_owned(),
                    JsonValue::String(FUNCTION_PAYLOAD_TYPE.to_owned()),
                ),
                ("invocation".to_owned(), invocation),
            ]),
        })],
        ..CellCommit::empty()
    })
    .unwrap();
    assert!(cell.perform().unwrap().is_some());
}

fn retain(
    cell: &mut Sqlite,
    dispatched: &DispatchedCommand,
    receipt: &FunctionReceipt,
) -> Result<Digest, ReceiptIntakeError> {
    retain_as(cell, dispatched, receipt, Some(EXECUTOR), &Contract)
}

/// Intake as a sender the host authenticated as `executor`, under
/// `authority`'s recheck.
fn retain_as(
    cell: &mut Sqlite,
    dispatched: &DispatchedCommand,
    receipt: &FunctionReceipt,
    executor: Option<&str>,
    authority: &dyn ResultAuthorizer,
) -> Result<Digest, ReceiptIntakeError> {
    let command = dispatched.command_id();
    cell.retain_function_receipt(
        dispatched,
        &Completion {
            receipt,
            command: &command,
            generation: dispatched.generation(),
            authenticated_executor: executor,
        },
        &ResultChecks {
            authorizer: authority,
            validator: &Contract,
        },
    )
}

/// One run, its pending confirmation and its performer's receipt.
struct Fixture {
    cell: Sqlite,
    dispatched: DispatchedCommand,
    receipt: FunctionReceipt,
}

fn fixture(path: &Path, outcome: &str) -> Fixture {
    let dispatched = dispatch("run-1", SCOPE, "main");
    let receipt = receipt_for(&dispatched, outcome);
    let mut cell = open(path, SCOPE, &[&receipt]);
    seed(&mut cell, &["run-1"]);
    perform(&mut cell, &dispatched);
    Fixture {
        cell,
        dispatched,
        receipt,
    }
}

fn confirm<'a>(action: &'a ActionRoot, run: &str, receipt: &str, key: &str) -> ActionCall<'a> {
    ActionCall {
        action,
        action_artifact_id: "flow.delivery_check.confirm.action",
        action_version: "1.0.0",
        scope: BTreeMap::from([
            ("orgId".to_owned(), "org_demo".to_owned()),
            ("environmentId".to_owned(), "dev".to_owned()),
        ]),
        actor: "user:dispatcher".to_owned(),
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
        trace: None,
    }
}

fn status<H: CellHost<Error = CellError>>(
    cell: &H,
    run: &str,
) -> (Option<JsonValue>, Option<JsonValue>) {
    let values = cell
        .load(&FactKey::new(RUN, run))
        .unwrap()
        .values
        .unwrap_or_default();
    (
        values.get("status").cloned(),
        // The durable host stores an unset nullable property as null.
        values
            .get("outcome")
            .cloned()
            .filter(|outcome| *outcome != JsonValue::Null),
    )
}

fn waiting() -> (Option<JsonValue>, Option<JsonValue>) {
    (
        Some(JsonValue::String("awaiting_perform_confirm".to_owned())),
        None,
    )
}

fn confirmed() -> (Option<JsonValue>, Option<JsonValue>) {
    (
        Some(JsonValue::String("confirmed".to_owned())),
        Some(JsonValue::String("delivered".to_owned())),
    )
}

fn reference(digest: &Digest) -> ReceiptReference {
    ReceiptReference::parse(&JsonValue::String(digest.0.clone())).unwrap()
}

/// Whether every commit after the first `base` consumed and wrote nothing.
fn effect_free_since(cell: &Sqlite, base: usize) -> bool {
    cell.commits().unwrap()[base..].iter().all(|commit| {
        commit.consumed.is_none() && commit.written.is_empty() && commit.linked.is_empty()
    })
}

/// Whether the delivery is still pending and its evidence still accepted.
fn still_accepted(cell: &Sqlite, digest: &Digest) -> bool {
    matches!(
        cell.resolve_accepted_receipt(&reference(digest), &manifest()),
        Ok(Some(_))
    )
}

#[test]
fn retained_receipt_commits_its_transition_and_consumption_atomically_across_a_crash() {
    let types = run_types();
    let catalog = TypeCatalog::new(&types);
    let action = confirm_action(&manifest());
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("cell.sqlite");
    let before_dir = tempfile::tempdir().expect("tempdir");
    let before = before_dir.path().join("cell.sqlite");
    let after_dir = tempfile::tempdir().expect("tempdir");
    let after = after_dir.path().join("cell.sqlite");
    let Fixture {
        mut cell,
        dispatched,
        receipt,
    } = fixture(&path, "delivered");
    let digest = retain(&mut cell, &dispatched, &receipt).expect("intake accepts the receipt");
    assert_eq!(digest, receipt.content_digest().unwrap());
    // An exact intake retry is a no-op that names the same evidence.
    assert_eq!(retain(&mut cell, &dispatched, &receipt).unwrap(), digest);
    let base = cell.commits().unwrap().len();
    // The disk image a crash just before the consuming commit leaves.
    common::crash_image(&path, &before);
    let call = confirm(&action, "run-1", &digest.0, "k1");
    let outcome = commit_action(&mut cell, &catalog, &call, &Allow)
        .unwrap_or_else(|refusal| panic!("the accepted receipt commits: {refusal:?}"));
    assert!(matches!(outcome, ActionOutcome::Committed { .. }));
    assert_eq!(status(&cell, "run-1"), confirmed());
    let values = cell
        .load(&FactKey::new(RUN, "run-1"))
        .unwrap()
        .values
        .unwrap();
    assert_eq!(
        values.get("receipt"),
        Some(&JsonValue::String(digest.0.clone()))
    );
    assert_eq!(
        cell.commits().unwrap().len(),
        base.saturating_add(1),
        "one consuming commit"
    );
    // RECEIPT-CONSUME-ONCE: a lost acknowledgement's exact retry replays the
    // original outcome and consumes nothing.
    let retried = commit_action(&mut cell, &catalog, &call, &Allow).expect("exact retry");
    assert!(matches!(retried, ActionOutcome::Replayed { .. }));
    assert_eq!(cell.commits().unwrap().len(), base.saturating_add(1));
    // The disk image a crash just after it leaves (WAL included).
    common::crash_image(&path, &after);
    drop(cell);

    // Before the commit: neither the transition nor the consumption
    // survived, and the retained evidence still resolves the run once.
    let mut crashed = open(&before, SCOPE, &[&receipt]);
    assert_eq!(status(&crashed, "run-1"), waiting());
    assert_eq!(crashed.commits().unwrap().len(), base);
    commit_action(&mut crashed, &catalog, &call, &Allow)
        .expect("the pending delivery is consumed after recovery");
    assert_eq!(status(&crashed, "run-1"), confirmed());

    // After the commit, crashed or cleanly closed: both effects survived,
    // and a second consumption under a new key is refused.
    for image in [&after, &path] {
        let mut reopened = open(image, SCOPE, &[&receipt]);
        assert_eq!(
            status(&reopened, "run-1"),
            confirmed(),
            "{}",
            image.display()
        );
        let commits = reopened.commits().unwrap().len();
        assert_eq!(commits, base.saturating_add(1), "one consuming commit");
        // Put the run back in its waiting state: only the fence stops a replay.
        let version = reopened.load(&FactKey::new(RUN, "run-1")).unwrap().version;
        reopened
            .commit(CellCommit {
                writes: vec![put(
                    FactKey::new(RUN, "run-1"),
                    version,
                    &[("run_id", "run-1"), ("status", "awaiting_perform_confirm")],
                )],
                ..CellCommit::empty()
            })
            .unwrap();
        let refused = commit_action(
            &mut reopened,
            &catalog,
            &confirm(&action, "run-1", &digest.0, "k2"),
            &Allow,
        )
        .unwrap_err();
        assert_eq!(refused.code(), "action.receipt_replayed", "{refused}");
        assert_eq!(status(&reopened, "run-1"), waiting());
        assert_eq!(reopened.commits().unwrap().len(), commits.saturating_add(1));
    }
}

#[test]
fn foreign_scope_or_world_evidence_is_never_retained_or_resolved() {
    let types = run_types();
    let catalog = TypeCatalog::new(&types);
    let action = confirm_action(&manifest());
    for (tenant, world) in [(OTHER_SCOPE, "main"), (SCOPE, "fork-1")] {
        let dir = tempfile::tempdir().expect("tempdir");
        let dispatched = dispatch("run-1", tenant, world);
        let receipt = receipt_for(&dispatched, "delivered");
        let mut cell = open(&dir.path().join("cell.sqlite"), SCOPE, &[&receipt]);
        seed(&mut cell, &["run-1"]);
        perform(&mut cell, &dispatched);
        let base = cell.commits().unwrap().len();
        let refused = retain(&mut cell, &dispatched, &receipt).unwrap_err();
        assert!(
            matches!(
                refused,
                ReceiptIntakeError::Refused {
                    reason: RefusalReason::OutOfScope { .. },
                    ..
                }
            ),
            "{tenant} {world}: {refused:?}"
        );
        let digest = receipt.content_digest().unwrap();
        let refused = commit_action(
            &mut cell,
            &catalog,
            &confirm(&action, "run-1", &digest.0, "k1"),
            &Allow,
        )
        .unwrap_err();
        assert_eq!(refused.code(), "action.receipt_unknown", "{refused}");
        assert_eq!(status(&cell, "run-1"), waiting());
        assert_eq!(cell.commits().unwrap().len(), base);
    }

    // Evidence retained by one tenant's cell is invisible to another's.
    let home_dir = tempfile::tempdir().expect("tempdir");
    let Fixture {
        mut cell,
        dispatched,
        receipt,
    } = fixture(&home_dir.path().join("cell.sqlite"), "delivered");
    let digest = retain(&mut cell, &dispatched, &receipt).unwrap();
    let foreign_dir = tempfile::tempdir().expect("tempdir");
    let foreign_dispatch = dispatch("run-1", OTHER_SCOPE, "main");
    let foreign_receipt = receipt_for(&foreign_dispatch, "delivered");
    let mut foreign = open(
        &foreign_dir.path().join("cell.sqlite"),
        OTHER_SCOPE,
        &[&foreign_receipt],
    );
    seed(&mut foreign, &["run-1"]);
    perform(&mut foreign, &foreign_dispatch);
    let base = foreign.commits().unwrap().len();
    let refused = commit_action(
        &mut foreign,
        &catalog,
        &confirm(&action, "run-1", &digest.0, "k1"),
        &Allow,
    )
    .unwrap_err();
    assert_eq!(refused.code(), "action.receipt_unknown", "{refused}");
    assert_eq!(status(&foreign, "run-1"), waiting());
    assert_eq!(foreign.commits().unwrap().len(), base);
    assert!(still_accepted(&cell, &digest));
}

#[test]
fn wrong_manifest_invocation_or_tampered_bindings_refuse_without_effects() {
    let types = run_types();
    let catalog = TypeCatalog::new(&types);
    let dir = tempfile::tempdir().expect("tempdir");
    let Fixture {
        mut cell,
        dispatched,
        receipt,
    } = fixture(&dir.path().join("cell.sqlite"), "delivered");
    let (write, events) = awaiting_run("run-2");
    cell.commit(CellCommit {
        writes: vec![write],
        events: events.into(),
        ..CellCommit::empty()
    })
    .unwrap();
    let base = cell.commits().unwrap().len();

    // Another invocation than the one enqueued, and pins that do not
    // re-derive from the enqueued invocation.
    let mut other = dispatched.invocation.clone();
    other.inputs.insert(
        "trackingRef".to_owned(),
        RuntimeValue::String("TRK-x".to_owned()),
    );
    let other = DispatchedCommand::pin(other, dispatched.execution_profile.clone()).unwrap();
    assert!(matches!(
        retain(&mut cell, &other, &receipt_for(&other, "delivered")),
        Err(ReceiptIntakeError::DispatchMismatch)
    ));
    let mut forged = dispatched.clone();
    forged.input_identity = sha('9');
    assert!(matches!(
        retain(&mut cell, &forged, &receipt),
        Err(ReceiptIntakeError::DispatchMismatch)
    ));
    // A result other than the one the cell recorded.
    let tampered = receipt_for(&dispatched, "delivered-elsewhere");
    assert!(matches!(
        retain(&mut cell, &dispatched, &tampered),
        Err(ReceiptIntakeError::DispatchMismatch)
    ));
    let tampered_digest = tampered.content_digest().unwrap();
    let action = confirm_action(&manifest());
    let refused = commit_action(
        &mut cell,
        &catalog,
        &confirm(&action, "run-1", &tampered_digest.0, "k-tampered"),
        &Allow,
    )
    .unwrap_err();
    assert_eq!(refused.code(), "action.receipt_unknown", "{refused}");

    // Accepted evidence under another declared manifest is not visible.
    let digest = retain(&mut cell, &dispatched, &receipt).unwrap();
    let elsewhere = confirm_action(&sha('e'));
    let refused = commit_action(
        &mut cell,
        &catalog,
        &confirm(&elsewhere, "run-1", &digest.0, "k-manifest"),
        &Allow,
    )
    .unwrap_err();
    assert_eq!(refused.code(), "action.receipt_unknown", "{refused}");
    // RUN-BIND-1: the receipt resolves only the run it was dispatched under.
    let refused = commit_action(
        &mut cell,
        &catalog,
        &confirm(&action, "run-2", &digest.0, "k-run"),
        &Allow,
    )
    .unwrap_err();
    assert_eq!(refused.code(), "action.receipt_run_mismatch", "{refused}");
    assert_eq!(status(&cell, "run-2"), waiting());
    assert_eq!(status(&cell, "run-1"), waiting());
    assert_eq!(cell.commits().unwrap().len(), base);
    assert!(still_accepted(&cell, &digest));
}

#[test]
fn a_performer_reporting_other_inputs_is_never_retained() {
    let types = run_types();
    let catalog = TypeCatalog::new(&types);
    let action = confirm_action(&manifest());
    let dispatched = dispatch("run-1", SCOPE, "main");
    let tampered_dir = tempfile::tempdir().expect("tempdir");
    let mut inputs_differ = receipt_for(&dispatched, "delivered");
    inputs_differ.input_identity = sha('8');
    let mut cell = open(
        &tampered_dir.path().join("cell.sqlite"),
        SCOPE,
        &[&inputs_differ],
    );
    seed(&mut cell, &["run-1"]);
    perform(&mut cell, &dispatched);
    let refused = retain(&mut cell, &dispatched, &inputs_differ).unwrap_err();
    assert!(
        matches!(
            refused,
            ReceiptIntakeError::Refused {
                reason: RefusalReason::InputsDiffer,
                ..
            }
        ),
        "{refused:?}"
    );
    let refused = commit_action(
        &mut cell,
        &catalog,
        &confirm(
            &action,
            "run-1",
            &inputs_differ.content_digest().unwrap().0,
            "k1",
        ),
        &Allow,
    )
    .unwrap_err();
    assert_eq!(refused.code(), "action.receipt_unknown", "{refused}");
    assert_eq!(status(&cell, "run-1"), waiting());
}

#[test]
fn stale_or_expired_evidence_refuses_and_leaves_the_delivery_pending() {
    let types = run_types();
    let catalog = TypeCatalog::new(&types);
    let action = confirm_action(&manifest());

    // Stale: the pinned shipment moved after the result was accepted.
    let dir = tempfile::tempdir().expect("tempdir");
    let Fixture {
        mut cell,
        dispatched,
        receipt,
    } = fixture(&dir.path().join("cell.sqlite"), "delivered");
    let digest = retain(&mut cell, &dispatched, &receipt).unwrap();
    cell.commit(CellCommit {
        writes: vec![put(
            FactKey::new(SHIPMENT, "ship-1"),
            FactVersion(1),
            &[("note", "rerouted")],
        )],
        ..CellCommit::empty()
    })
    .unwrap();
    let base = cell.commits().unwrap().len();
    let refused = commit_action(
        &mut cell,
        &catalog,
        &confirm(&action, "run-1", &digest.0, "k1"),
        &Allow,
    )
    .unwrap_err();
    assert_eq!(refused.code(), "action.receipt_not_accepted", "{refused}");
    assert_eq!(status(&cell, "run-1"), waiting());
    assert_eq!(cell.commits().unwrap().len(), base);
    assert_eq!(
        cell.pending_commands().unwrap().len(),
        1,
        "the delivery stays pending"
    );
    assert!(!still_accepted(&cell, &digest));

    // Expired: the cell's clock passed the dispatch deadline.
    let dir = tempfile::tempdir().expect("tempdir");
    let Fixture {
        mut cell,
        dispatched,
        receipt,
    } = fixture(&dir.path().join("cell.sqlite"), "delivered");
    let digest = retain(&mut cell, &dispatched, &receipt).unwrap();
    assert!(still_accepted(&cell, &digest));
    cell.advance_to(LogicalTime::from_millis(PAST_DEADLINE));
    let base = cell.commits().unwrap().len();
    let refused = commit_action(
        &mut cell,
        &catalog,
        &confirm(&action, "run-1", &digest.0, "k1"),
        &Allow,
    )
    .unwrap_err();
    assert_eq!(refused.code(), "action.receipt_not_accepted", "{refused}");
    assert_eq!(status(&cell, "run-1"), waiting());
    assert_eq!(cell.commits().unwrap().len(), base);
    assert_eq!(
        cell.pending_commands().unwrap().len(),
        1,
        "the delivery stays pending"
    );
}

/// A host over the durable cell whose `race` commits a competing action
/// between this call's resolution and its commit.
struct Host<'a> {
    cell: RefCell<Sqlite>,
    race: RefCell<Option<Race<'a>>>,
}

/// A competing consumer that commits first.
type Race<'a> = Box<dyn FnOnce(&mut Sqlite) + 'a>;

impl CellHost for Host<'_> {
    type Event = FactEvent;
    type Command = JsonObject;
    type Input = JsonObject;
    type Error = CellError;

    fn load(&self, key: &FactKey) -> Result<FactState, CellError> {
        self.cell.borrow().load(key)
    }
    // The durable host's own read and commit paths, forwarded unchanged.
    fn supports_reference_reads(&self) -> bool {
        self.cell.borrow().supports_reference_reads()
    }
    fn load_reference(
        &self,
        key: &FactKey,
        max_bytes: u64,
    ) -> Result<Option<norn_kernel::BoundedFact>, CellError> {
        self.cell.borrow().load_reference(key, max_bytes)
    }
    fn load_bounded(
        &self,
        key: &FactKey,
        max_bytes: u64,
    ) -> Result<Option<norn_kernel::BoundedFact>, CellError> {
        self.cell.borrow().load_bounded(key, max_bytes)
    }
    fn commit_action_references(
        &mut self,
        commit: CellCommit<FactEvent, JsonObject, JsonObject>,
        proof: norn_kernel::action_commit::ActionReferenceReads,
        quantity: Option<(&norn_ir::quantity_ledger::QuantityRequest, &str)>,
    ) -> Result<CommitReceipt, CommitError<CellError>> {
        self.cell
            .get_mut()
            .commit_action_references(commit, proof, quantity)
    }
    fn resolve_accepted_receipt(
        &self,
        reference: &ReceiptReference,
        manifest: &Digest,
    ) -> Result<Option<ReceiptBinding<JsonObject>>, ReceiptBindingError<CellError>> {
        let binding = self
            .cell
            .borrow()
            .resolve_accepted_receipt(reference, manifest);
        let race = self.race.borrow_mut().take();
        if let Some(race) = race {
            let mut cell = self.cell.borrow_mut();
            race(&mut cell);
        }
        binding
    }
    fn load_link(&self, key: &LinkKey) -> Result<LinkState, CellError> {
        self.cell.borrow().load_link(key)
    }
    fn commit(
        &mut self,
        commit: CellCommit<FactEvent, JsonObject, JsonObject>,
    ) -> Result<CommitReceipt, CommitError<CellError>> {
        self.cell.get_mut().commit(commit)
    }
    fn perform(&mut self) -> Result<Option<Performed<JsonObject>>, CellError> {
        self.cell.get_mut().perform()
    }
    fn observe(&mut self) -> Result<Option<Observed<JsonObject>>, CellError> {
        self.cell.get_mut().observe()
    }
    fn clock(&mut self) -> Result<Option<DueTimer<JsonObject>>, CellError> {
        self.cell.get_mut().clock()
    }
    fn publish(&mut self, receipt: &CommitReceipt) -> Result<(), CellError> {
        self.cell.get_mut().publish(receipt)
    }
}

#[test]
fn concurrent_consumptions_have_exactly_one_winner() {
    let types = run_types();
    let catalog = TypeCatalog::new(&types);
    let action = confirm_action(&manifest());
    let dir = tempfile::tempdir().expect("tempdir");
    let Fixture {
        mut cell,
        dispatched,
        receipt,
    } = fixture(&dir.path().join("cell.sqlite"), "delivered");
    let digest = retain(&mut cell, &dispatched, &receipt).unwrap();
    let base = cell.commits().unwrap().len();
    let winner = confirm(&action, "run-1", &digest.0, "winner");
    let race: Race<'_> = Box::new(|cell: &mut Sqlite| {
        let outcome = commit_action(cell, &catalog, &winner, &Allow)
            .unwrap_or_else(|refusal| panic!("the first consumer commits: {refusal:?}"));
        assert!(matches!(outcome, ActionOutcome::Committed { .. }));
    });
    let mut host = Host {
        cell: RefCell::new(cell),
        race: RefCell::new(Some(race)),
    };
    // Both calls resolved the same pending delivery; the loser's commit
    // finds it consumed and applies nothing.
    let refused = commit_action(
        &mut host,
        &catalog,
        &confirm(&action, "run-1", &digest.0, "loser"),
        &Allow,
    )
    .unwrap_err();
    assert_eq!(refused.code(), "action.receipt_not_pending", "{refused}");
    let mut cell = host.cell.into_inner();
    assert_eq!(status(&cell, "run-1"), confirmed());
    assert_eq!(
        cell.commits().unwrap().len(),
        base.saturating_add(1),
        "exactly one winner"
    );
    // The winner's exact retry still replays its own outcome.
    let retried = commit_action(&mut cell, &catalog, &winner, &Allow).unwrap();
    assert!(matches!(retried, ActionOutcome::Replayed { .. }));
    assert_eq!(cell.commits().unwrap().len(), base.saturating_add(1));
}

#[test]
fn governing_law_and_caller_authority_still_decide_publication() {
    let types = run_types();
    let catalog = TypeCatalog::new(&types);
    let action = confirm_action(&manifest());

    // An accepted, authentic receipt reporting a held delivery.
    let dir = tempfile::tempdir().expect("tempdir");
    let Fixture {
        mut cell,
        dispatched,
        receipt,
    } = fixture(&dir.path().join("cell.sqlite"), "held");
    let digest = retain(&mut cell, &dispatched, &receipt).unwrap();
    let base = cell.commits().unwrap().len();
    let refused = commit_action(
        &mut cell,
        &catalog,
        &confirm(&action, "run-1", &digest.0, "k1"),
        &Allow,
    )
    .unwrap_err();
    assert!(!refused.code().starts_with("action.receipt_"), "{refused}");
    assert_eq!(status(&cell, "run-1"), waiting());
    assert!(effect_free_since(&cell, base), "only a refusal record");
    assert!(still_accepted(&cell, &digest), "the delivery stays pending");

    // A caller without authority over the run publishes nothing either.
    let dir = tempfile::tempdir().expect("tempdir");
    let Fixture {
        mut cell,
        dispatched,
        receipt,
    } = fixture(&dir.path().join("cell.sqlite"), "delivered");
    let digest = retain(&mut cell, &dispatched, &receipt).unwrap();
    let base = cell.commits().unwrap().len();
    let refused = commit_action(
        &mut cell,
        &catalog,
        &confirm(&action, "run-1", &digest.0, "k1"),
        &Deny,
    )
    .unwrap_err();
    assert!(!refused.code().starts_with("action.receipt_"), "{refused}");
    assert_eq!(status(&cell, "run-1"), waiting());
    assert!(effect_free_since(&cell, base));
    assert!(still_accepted(&cell, &digest));
    commit_action(
        &mut cell,
        &catalog,
        &confirm(&action, "run-1", &digest.0, "k2"),
        &Allow,
    )
    .expect("an authorized caller publishes the same evidence");
    assert_eq!(status(&cell, "run-1"), confirmed());
}

#[test]
fn a_host_without_receipt_storage_stays_fail_closed() {
    let types = run_types();
    let catalog = TypeCatalog::new(&types);
    let action = confirm_action(&manifest());
    let dir = tempfile::tempdir().expect("tempdir");
    let Fixture {
        mut cell,
        dispatched,
        receipt,
    } = fixture(&dir.path().join("cell.sqlite"), "delivered");
    let digest = retain(&mut cell, &dispatched, &receipt).unwrap();
    let base = cell.commits().unwrap().len();
    // A cell with no command-authority recheck serves no receipt effect.
    cell.set_result_authorizer(None);
    let refused = commit_action(
        &mut cell,
        &catalog,
        &confirm(&action, "run-1", &digest.0, "k1"),
        &Allow,
    )
    .unwrap_err();
    assert_eq!(
        refused.code(),
        "action.receipt_effect_unsupported",
        "{refused}"
    );
    assert_eq!(status(&cell, "run-1"), waiting());
    assert_eq!(cell.commits().unwrap().len(), base);
    cell.set_result_authorizer(Some(Box::new(Contract)));
    assert!(still_accepted(&cell, &digest));
    // The intake itself refuses unqualified freshness pins.
    let dir = tempfile::tempdir().expect("tempdir");
    let mut destination = dispatch("run-1", SCOPE, "main").invocation;
    destination.freshness = norn_ir::function::FreshnessMode::DestinationCas;
    let destination = DispatchedCommand::pin(destination, sha('7')).unwrap();
    let mut cell = open(&dir.path().join("cell.sqlite"), SCOPE, &[]);
    assert!(matches!(
        retain(
            &mut cell,
            &destination,
            &receipt_for(&destination, "delivered")
        ),
        Err(ReceiptIntakeError::Unsupported)
    ));
}

#[test]
fn an_intake_retry_reruns_every_check() {
    let dir = tempfile::tempdir().expect("tempdir");
    let Fixture {
        mut cell,
        dispatched,
        receipt,
    } = fixture(&dir.path().join("cell.sqlite"), "delivered");
    let digest = retain(&mut cell, &dispatched, &receipt).unwrap();
    assert_eq!(retain(&mut cell, &dispatched, &receipt).unwrap(), digest);
    // The same receipt again, but from a sender nobody authenticated, under
    // revoked authority, or under pins that do not re-derive.
    let refused = retain_as(&mut cell, &dispatched, &receipt, None, &Contract).unwrap_err();
    assert!(
        matches!(
            refused,
            ReceiptIntakeError::Refused {
                reason: RefusalReason::Unauthenticated,
                ..
            }
        ),
        "{refused:?}"
    );
    let refused =
        retain_as(&mut cell, &dispatched, &receipt, Some(EXECUTOR), &Revoked).unwrap_err();
    assert!(
        matches!(
            refused,
            ReceiptIntakeError::Refused {
                reason: RefusalReason::Unauthorized { .. },
                ..
            }
        ),
        "{refused:?}"
    );
    let mut forged = dispatched.clone();
    forged.dependency_token = sha('9');
    assert!(matches!(
        retain(&mut cell, &forged, &receipt),
        Err(ReceiptIntakeError::DispatchMismatch)
    ));
    let other = receipt_for(&dispatched, "delivered-elsewhere");
    assert!(matches!(
        retain(&mut cell, &dispatched, &other),
        Err(ReceiptIntakeError::DispatchMismatch)
    ));
    assert!(still_accepted(&cell, &digest));
}

#[test]
fn revoked_command_authority_refuses_consumption() {
    let types = run_types();
    let catalog = TypeCatalog::new(&types);
    let action = confirm_action(&manifest());
    let dir = tempfile::tempdir().expect("tempdir");
    let Fixture {
        mut cell,
        dispatched,
        receipt,
    } = fixture(&dir.path().join("cell.sqlite"), "delivered");
    let digest = retain(&mut cell, &dispatched, &receipt).unwrap();
    let base = cell.commits().unwrap().len();
    // T2-RES-1 step 8: authority is rechecked when the receipt is consumed.
    cell.set_result_authorizer(Some(Box::new(Revoked)));
    let refused = commit_action(
        &mut cell,
        &catalog,
        &confirm(&action, "run-1", &digest.0, "k1"),
        &Allow,
    )
    .unwrap_err();
    assert_eq!(refused.code(), "action.receipt_not_accepted", "{refused}");
    assert_eq!(status(&cell, "run-1"), waiting());
    assert_eq!(cell.commits().unwrap().len(), base);
    assert_eq!(cell.pending_commands().unwrap().len(), 1);
    cell.set_result_authorizer(Some(Box::new(Contract)));
    commit_action(
        &mut cell,
        &catalog,
        &confirm(&action, "run-1", &digest.0, "k2"),
        &Allow,
    )
    .expect("restored authority consumes the same evidence");
    assert_eq!(status(&cell, "run-1"), confirmed());
}

#[test]
fn a_fork_never_retains_or_resolves_evidence() {
    let types = run_types();
    let catalog = TypeCatalog::new(&types);
    let action = confirm_action(&manifest());
    let dir = tempfile::tempdir().expect("tempdir");
    let Fixture {
        mut cell,
        dispatched,
        receipt,
    } = fixture(&dir.path().join("cell.sqlite"), "delivered");
    let digest = retain(&mut cell, &dispatched, &receipt).unwrap();
    let base = cell.store().verified_head().expect("head");
    let store = cell.store().fork("what-if", &base).expect("forks");
    let mut fork = Sqlite::new(
        store,
        SCOPE,
        |_command: &Command<JsonObject>| -> JsonObject { panic!("a fork never performs") },
    )
    .expect("serves the fork");
    fork.set_result_authorizer(Some(Box::new(Contract)));
    let before = fork.store().image().expect("image");
    assert!(matches!(
        retain(&mut fork, &dispatched, &receipt),
        Err(ReceiptIntakeError::Unsupported)
    ));
    let refused = commit_action(
        &mut fork,
        &catalog,
        &confirm(&action, "run-1", &digest.0, "k1"),
        &Allow,
    )
    .unwrap_err();
    assert_eq!(refused.code(), "action.receipt_unknown", "{refused}");
    assert_eq!(status(&fork, "run-1"), waiting());
    assert_eq!(fork.store().image().expect("image"), before);
    assert!(still_accepted(&cell, &digest));
}

#[tokio::test]
async fn a_restored_cell_holds_no_retained_evidence() {
    let types = run_types();
    let catalog = TypeCatalog::new(&types);
    let action = confirm_action(&manifest());
    let dir = tempfile::tempdir().expect("tempdir");
    let Fixture {
        mut cell,
        dispatched,
        receipt,
    } = fixture(&dir.path().join("cell.sqlite"), "delivered");
    let digest = retain(&mut cell, &dispatched, &receipt).unwrap();
    let mut exporter = Exporter::local(&dir.path().join("export")).expect("exporter");
    let start = exporter
        .verify()
        .await
        .expect("verify")
        .head_commit_seq
        .saturating_add(1);
    for seq in start..=cell.store().export_commit_count().expect("count") {
        exporter
            .append(
                &cell
                    .store()
                    .export_record(seq)
                    .expect("journal")
                    .expect("record"),
            )
            .await
            .expect("append");
    }
    let manifest = exporter.verify().await.expect("verified export");
    let restored_path = dir.path().join("restored.sqlite");
    norn_cell::CellStore::restore_from(
        &mut exporter,
        &restored_path,
        Model::from_definitions(&definitions(), &[]).expect("models the run"),
        SCOPE,
        &manifest.bindings,
        StoreOptions::default(),
    )
    .await
    .expect("restores the exported history");
    // Export carries ledger history, never private receipt evidence: the
    // restored cell refuses the reference until intake runs again.
    let mut restored = open(&restored_path, SCOPE, &[&receipt]);
    assert_eq!(status(&restored, "run-1"), waiting());
    let refused = commit_action(
        &mut restored,
        &catalog,
        &confirm(&action, "run-1", &digest.0, "k1"),
        &Allow,
    )
    .unwrap_err();
    assert_eq!(refused.code(), "action.receipt_unknown", "{refused}");
    assert_eq!(status(&restored, "run-1"), waiting());
    assert!(still_accepted(&cell, &digest));
}

#[test]
fn a_sibling_command_state_change_after_resolution_refuses_consumption() {
    let types = run_types();
    let catalog = TypeCatalog::new(&types);
    let action = confirm_action(&manifest());
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("cell.sqlite");
    let Fixture {
        mut cell,
        dispatched,
        receipt,
    } = fixture(&path, "delivered");
    let digest = retain(&mut cell, &dispatched, &receipt).unwrap();
    let base = cell.commits().unwrap().len();
    // Another handle cancels the command after this call resolved the
    // receipt and before its commit; the delivery itself stays pending.
    let mut sibling = open(&path, SCOPE, &[&receipt]);
    let cancelled =
        norn_ir::runtime::encode(&CommandState::dispatched(&dispatched).cancel().unwrap()).unwrap();
    let race: Race<'_> = Box::new(move |_cell: &mut Sqlite| {
        sibling
            .commit(CellCommit {
                writes: vec![FactWrite {
                    key: FactKey::new(FUNCTION_COMMAND_TYPE, "run-1/confirm"),
                    expected: FactVersion(1),
                    mutation: FactMutation::Put {
                        values: JsonObject::from([("state".to_owned(), cancelled)]),
                    },
                }],
                ..CellCommit::empty()
            })
            .expect("the sibling cancels the command");
    });
    let mut host = Host {
        cell: RefCell::new(cell),
        race: RefCell::new(Some(race)),
    };
    let refused = commit_action(
        &mut host,
        &catalog,
        &confirm(&action, "run-1", &digest.0, "k1"),
        &Allow,
    )
    .unwrap_err();
    assert_eq!(refused.code(), "decision.stale_state", "{refused}");
    let mut cell = host.cell.into_inner();
    assert_eq!(status(&cell, "run-1"), waiting());
    assert_eq!(cell.pending_commands().unwrap().len(), 1);
    assert_eq!(
        cell.commits().unwrap().len(),
        base.saturating_add(1),
        "only the cancel"
    );
    // A retry resolves against the cancelled command and is refused.
    let refused = commit_action(
        &mut cell,
        &catalog,
        &confirm(&action, "run-1", &digest.0, "k2"),
        &Allow,
    )
    .unwrap_err();
    assert_eq!(refused.code(), "action.receipt_not_accepted", "{refused}");
    assert_eq!(status(&cell, "run-1"), waiting());
}

#[test]
fn authority_revoked_between_resolution_and_commit_refuses() {
    let types = run_types();
    let catalog = TypeCatalog::new(&types);
    let action = confirm_action(&manifest());
    let dir = tempfile::tempdir().expect("tempdir");
    let Fixture {
        mut cell,
        dispatched,
        receipt,
    } = fixture(&dir.path().join("cell.sqlite"), "delivered");
    let digest = retain(&mut cell, &dispatched, &receipt).unwrap();
    let base = cell.commits().unwrap().len();
    let race: Race<'_> = Box::new(|cell: &mut Sqlite| {
        cell.set_result_authorizer(Some(Box::new(Revoked)));
    });
    let mut host = Host {
        cell: RefCell::new(cell),
        race: RefCell::new(Some(race)),
    };
    // The commit itself rechecks authority under its write lock.
    let refused = commit_action(
        &mut host,
        &catalog,
        &confirm(&action, "run-1", &digest.0, "k1"),
        &Allow,
    )
    .unwrap_err();
    assert_eq!(refused.code(), "action.receipt_not_accepted", "{refused}");
    let mut cell = host.cell.into_inner();
    assert_eq!(status(&cell, "run-1"), waiting());
    assert_eq!(cell.commits().unwrap().len(), base);
    assert_eq!(cell.pending_commands().unwrap().len(), 1);
    cell.set_result_authorizer(Some(Box::new(Contract)));
    assert!(still_accepted(&cell, &digest));
}

/// Another handle on the same file commits at logical time `millis`.
fn advance_sibling(path: &Path, receipt: &FunctionReceipt, millis: u64) {
    let mut sibling = open(path, SCOPE, &[receipt]);
    sibling.advance_to(LogicalTime::from_millis(millis));
    let run = format!("run-{millis}");
    sibling
        .commit(CellCommit {
            writes: vec![put(
                FactKey::new(RUN, run.as_str()),
                FactVersion::ABSENT,
                &[
                    ("run_id", run.as_str()),
                    ("status", "awaiting_perform_confirm"),
                ],
            )],
            ..CellCommit::empty()
        })
        .expect("the sibling commits at its clock");
}

#[test]
fn a_sibling_clock_past_the_deadline_fails_intake_and_resolution() {
    let types = run_types();
    let catalog = TypeCatalog::new(&types);
    let action = confirm_action(&manifest());

    // Resolution: retained before another handle's clock passed the deadline.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("cell.sqlite");
    let Fixture {
        mut cell,
        dispatched,
        receipt,
    } = fixture(&path, "delivered");
    let digest = retain(&mut cell, &dispatched, &receipt).unwrap();
    advance_sibling(&path, &receipt, PAST_DEADLINE);
    assert!(
        cell.now() < LogicalTime::from_millis(DEADLINE),
        "a stale handle clock"
    );
    let base = cell.commits().unwrap().len();
    let refused = commit_action(
        &mut cell,
        &catalog,
        &confirm(&action, "run-1", &digest.0, "k1"),
        &Allow,
    )
    .unwrap_err();
    assert_eq!(refused.code(), "action.receipt_not_accepted", "{refused}");
    assert_eq!(status(&cell, "run-1"), waiting());
    assert_eq!(cell.commits().unwrap().len(), base);

    // Intake: the clock passed the deadline before the receipt was offered.
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("cell.sqlite");
    let Fixture {
        mut cell,
        dispatched,
        receipt,
    } = fixture(&path, "delivered");
    advance_sibling(&path, &receipt, PAST_DEADLINE);
    let refused = retain(&mut cell, &dispatched, &receipt).unwrap_err();
    assert!(
        matches!(
            refused,
            ReceiptIntakeError::Refused {
                reason: RefusalReason::DeadlinePassed { .. },
                ..
            }
        ),
        "{refused:?}"
    );
    let refused = commit_action(
        &mut cell,
        &catalog,
        &confirm(&action, "run-1", &receipt.content_digest().unwrap().0, "k1"),
        &Allow,
    )
    .unwrap_err();
    assert_eq!(refused.code(), "action.receipt_unknown", "{refused}");
}

#[test]
fn a_sibling_clock_moving_between_resolution_and_commit() {
    let types = run_types();
    let catalog = TypeCatalog::new(&types);
    let action = confirm_action(&manifest());
    // The commit follows the durable clock: still valid within the deadline,
    // refused past it, and never a clock regression of this handle.
    for (millis, accepted) in [(WITHIN_DEADLINE, true), (PAST_DEADLINE, false)] {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cell.sqlite");
        let Fixture {
            mut cell,
            dispatched,
            receipt,
        } = fixture(&path, "delivered");
        let digest = retain(&mut cell, &dispatched, &receipt).unwrap();
        let base = cell.commits().unwrap().len();
        let race: Race<'_> = Box::new(move |_cell: &mut Sqlite| {
            advance_sibling(&path, &receipt, millis);
        });
        let mut host = Host {
            cell: RefCell::new(cell),
            race: RefCell::new(Some(race)),
        };
        let outcome = commit_action(
            &mut host,
            &catalog,
            &confirm(&action, "run-1", &digest.0, "k1"),
            &Allow,
        );
        let cell = host.cell.into_inner();
        if accepted {
            assert!(
                matches!(outcome, Ok(ActionOutcome::Committed { .. })),
                "{millis}"
            );
            assert_eq!(status(&cell, "run-1"), confirmed());
            assert_eq!(cell.pending_commands().unwrap().len(), 0);
        } else {
            let refused = outcome.unwrap_err();
            assert_eq!(refused.code(), "action.receipt_not_accepted", "{refused}");
            assert_eq!(status(&cell, "run-1"), waiting());
            assert_eq!(cell.commits().unwrap().len(), base.saturating_add(1));
        }
    }
}

#[test]
fn an_embedded_acceptance_rechecks_authority_and_only_a_bare_refusal_record_skips_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let Fixture {
        mut cell,
        dispatched,
        receipt,
    } = fixture(&dir.path().join("cell.sqlite"), "delivered");
    let digest = retain(&mut cell, &dispatched, &receipt).unwrap();
    // An embedded host prepares the acceptance itself, outside any action.
    let (_, _, acceptance) = cell
        .resolve_accepted_receipt(&reference(&digest), &manifest())
        .unwrap()
        .expect("resolves")
        .into_parts();
    let apply = |acceptance: &Acceptance| {
        acceptance
            .commit::<FactEvent, JsonObject, JsonObject>(
                vec![put(
                    FactKey::new(RUN, "run-1"),
                    FactVersion(1),
                    &[
                        ("run_id", "run-1"),
                        ("status", "awaiting_perform_confirm"),
                        ("outcome", "delivered"),
                    ],
                )],
                Vec::new(),
                Vec::new(),
            )
            .expect("no pinned write")
    };
    let base = cell.commits().unwrap().len();
    // Authority revoked between preparation and commit.
    cell.set_result_authorizer(Some(Box::new(Revoked)));
    let refused = cell.commit(apply(&acceptance)).unwrap_err();
    assert!(
        matches!(refused, CommitError::ReceiptNotAccepted { .. }),
        "{refused}"
    );
    // Even a read-only acceptance, which writes nothing, is rechecked: only
    // the refusal entry point skips it.
    let read_only = acceptance
        .commit::<FactEvent, JsonObject, JsonObject>(Vec::new(), Vec::new(), Vec::new())
        .expect("no pinned write");
    let refused = cell.commit(read_only).unwrap_err();
    assert!(
        matches!(refused, CommitError::ReceiptNotAccepted { .. }),
        "{refused}"
    );
    // So are event-only and effect-only commits.
    let event = FactEvent::CommandEnqueued {
        command_id: "run-1/other".to_owned(),
        generation: 1,
        idempotency_key: "other".to_owned(),
        action_call: "none".to_owned(),
        payload: JsonObject::new(),
    };
    let unrelated = Effect::CancelTimer(TimerId("unrelated".to_owned()));
    for (events, effects) in [(vec![event], Vec::new()), (Vec::new(), vec![unrelated])] {
        let refused = cell
            .commit(CellCommit {
                consumes: Some(acceptance.consumed.clone()),
                events,
                effects,
                ..CellCommit::empty()
            })
            .unwrap_err();
        assert!(
            matches!(refused, CommitError::ReceiptNotAccepted { .. }),
            "{refused}"
        );
    }
    // A refusal record must name a pending Function delivery.
    for other in [
        Consumed::Result {
            command: CommandId("run-1/unknown".to_owned()),
            generation: Generation(1),
        },
        Consumed::Timer {
            id: TimerId("unrelated".to_owned()),
            incarnation: LedgerSeq(1),
        },
    ] {
        assert!(matches!(
            cell.commit_result_refusal(other),
            Err(CommitError::ReceiptNotAccepted { .. })
        ));
    }
    assert_eq!(status(&cell, "run-1"), waiting());
    assert_eq!(cell.commits().unwrap().len(), base);
    assert_eq!(cell.pending_commands().unwrap().len(), 1);
    // Bookkeeping events the host attaches join the final batch, so the
    // refusal entry point refuses them too.
    let attached = cell
        .attributed(
            vec![norn_ir::runtime_object! { "type" => "test.bookkeeping" }],
            |cell| cell.commit_result_refusal(acceptance.consumed.clone()),
        )
        .unwrap_err();
    assert!(
        matches!(attached, CommitError::ReceiptNotAccepted { .. }),
        "{attached}"
    );
    assert_eq!(cell.pending_commands().unwrap().len(), 1);
    // The refusal record consumes the delivery without the recheck and
    // applies nothing.
    cell.commit_result_refusal(acceptance.consumed)
        .expect("the refusal record consumes the delivery");
    assert_eq!(cell.pending_commands().unwrap().len(), 0);
    assert_eq!(status(&cell, "run-1"), waiting());
}

#[test]
fn a_losing_consumer_behind_a_later_sibling_clock_is_not_pending() {
    let types = run_types();
    let catalog = TypeCatalog::new(&types);
    let action = confirm_action(&manifest());
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("cell.sqlite");
    let Fixture {
        mut cell,
        dispatched,
        receipt,
    } = fixture(&path, "delivered");
    let digest = retain(&mut cell, &dispatched, &receipt).unwrap();
    let base = cell.commits().unwrap().len();
    // Another handle, its clock later than this one's, consumes the
    // delivery after this call resolved it and before this call commits.
    let mut sibling = open(&path, SCOPE, &[&receipt]);
    sibling.advance_to(LogicalTime::from_millis(WITHIN_DEADLINE));
    let winner = confirm(&action, "run-1", &digest.0, "winner");
    let shared = &catalog;
    let race: Race<'_> = Box::new(move |_cell: &mut Sqlite| {
        let outcome = commit_action(&mut sibling, shared, &winner, &Allow)
            .unwrap_or_else(|refusal| panic!("the sibling consumes first: {refusal:?}"));
        assert!(matches!(outcome, ActionOutcome::Committed { .. }));
    });
    let mut host = Host {
        cell: RefCell::new(cell),
        race: RefCell::new(Some(race)),
    };
    let refused = commit_action(
        &mut host,
        &catalog,
        &confirm(&action, "run-1", &digest.0, "loser"),
        &Allow,
    )
    .unwrap_err();
    assert_eq!(refused.code(), "action.receipt_not_pending", "{refused}");
    let cell = host.cell.into_inner();
    assert_eq!(status(&cell, "run-1"), confirmed());
    assert_eq!(
        cell.commits().unwrap().len(),
        base.saturating_add(1),
        "only the winner"
    );
}

#[test]
fn the_floor_classifier_meets_stale_generations_and_non_text_types() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("cell.sqlite");
    let mut answer = receipt_for(&dispatch("run-1", SCOPE, "main"), "delivered");
    answer.command_id = "plain-1".to_owned();
    let mut cell = open(&path, SCOPE, &[&answer]);
    let plain = CommandId("plain-1".to_owned());
    // An ordinary command whose `payload.type` is a number.
    cell.commit(CellCommit {
        effects: vec![Effect::Perform(Command {
            id: plain.clone(),
            generation: Generation(1),
            idempotency_key: IdempotencyKey("plain-1/1".to_owned()),
            freshness: FreshnessPolicy::AppendOnly,
            deadline: None,
            payload: JsonObject::from([(
                "type".to_owned(),
                norn_ir::runtime::encode(&7_u32).unwrap(),
            )]),
        })],
        ..CellCommit::empty()
    })
    .unwrap();
    assert!(cell.perform().unwrap().is_some());
    // Another handle's clock passes this one's.
    advance_sibling(&path, &answer, WITHIN_DEADLINE);
    // A stale generation, including one no SQL integer can store, meets the
    // delivery check, not a clock regression or a range error.
    for generation in [9, 9_007_199_254_740_993, u64::MAX] {
        let stale = cell
            .commit(CellCommit {
                consumes: Some(Consumed::Result {
                    command: plain.clone(),
                    generation: Generation(generation),
                }),
                ..CellCommit::empty()
            })
            .unwrap_err();
        assert!(
            matches!(stale, CommitError::NotPending { .. }),
            "{generation}: {stale}"
        );
    }
    // A non-text `payload.type` is an ordinary delivery, consumed normally.
    cell.advance_to(LogicalTime::from_millis(WITHIN_DEADLINE));
    cell.commit(CellCommit {
        consumes: Some(Consumed::Result {
            command: plain,
            generation: Generation(1),
        }),
        ..CellCommit::empty()
    })
    .expect("an ordinary delivery is consumed");
    assert_eq!(cell.pending_commands().unwrap().len(), 0);
}
