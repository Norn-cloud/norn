//! RECEIPT-CONSUME-1 on the durable host: a receipt's delivery is consumed in
//! the transition's own SQLite commit. A crash before that commit leaves the
//! delivery pending and the run waiting; after it, a reopened cell holds the
//! transition and refuses a second consumption. The in-memory host's tests
//! live in `norn-kernel/tests/receipt_effect.rs`.
#![cfg(feature = "native")]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    missing_docs,
    reason = "durable parity assertions"
)]

mod common;

use std::collections::BTreeMap;
use std::path::Path;

use norn_cell::{CellError, Model, SqliteCell, StoreOptions};
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
use serde_json::json;

const RUN: &str = "flow.DecisionRun";
const SCOPE: &str = "org_demo/dev";

type Sqlite = SqliteCell<FactEvent, JsonObject, JsonObject>;

fn manifest() -> Digest {
    Digest(format!("sha256:{}", "d".repeat(64)))
}

fn run_types() -> Vec<ObjectTypeDef> {
    vec![
        serde_json::from_value(json!({
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
        .unwrap(),
    ]
}

/// The run type and the kernel's action-call record type, which every
/// durable action commit writes (as `tests/synchronous_action.rs` declares it).
fn definitions() -> Vec<ObjectTypeDef> {
    let text = |nullable: bool| {
        move |id: &&str| {
            if nullable {
                json!({"id": id, "type": {"kind": "text"}, "constraints": {"nullable": true}})
            } else {
                json!({"id": id, "type": {"kind": "text"}})
            }
        }
    };
    let mut fields: Vec<serde_json::Value> =
        ["actionId", "actor", "turn", "transition", "from", "to"]
            .iter()
            .map(text(false))
            .collect();
    fields.extend(
        [
            "actionArtifactId",
            "actionVersion",
            "effectKind",
            "ruleBinding",
        ]
        .iter()
        .map(text(true)),
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
    definitions.push(
        serde_json::from_value(
            json!({"name": norn_kernel::ACTION_CALL_TYPE, "properties": fields}),
        )
        .unwrap(),
    );
    definitions
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

/// The issued receipt and its acceptance, as the cell's receipt store keeps
/// them; consumption is the durable cell's own pending-delivery state.
struct Issued {
    receipt: FunctionReceipt,
    acceptance: Acceptance,
}

struct DurableReceiptCell {
    cell: Sqlite,
    issued: BTreeMap<String, Issued>,
}

struct Store<'a>(&'a DurableReceiptCell, &'a Digest);

impl AcceptedReceiptStore for Store<'_> {
    type Error = CellError;
    fn resolve(&self, reference: &Digest) -> Result<ReceiptResolution<'_>, CellError> {
        Ok(match self.0.issued.get(&reference.0) {
            Some(issued) if &issued.receipt.manifest == self.1 => ReceiptResolution::Accepted {
                acceptance: &issued.acceptance,
                receipt: &issued.receipt,
            },
            _ => ReceiptResolution::Unknown,
        })
    }
}

impl CellHost for DurableReceiptCell {
    type Event = FactEvent;
    type Command = JsonObject;
    type Input = JsonObject;
    type Error = CellError;

    fn load(&self, key: &FactKey) -> Result<FactState, CellError> {
        self.cell.load(key)
    }
    // The durable host's own read and commit paths, forwarded unchanged.
    fn supports_reference_reads(&self) -> bool {
        self.cell.supports_reference_reads()
    }
    fn load_reference(
        &self,
        key: &FactKey,
        max_bytes: u64,
    ) -> Result<Option<norn_kernel::BoundedFact>, CellError> {
        self.cell.load_reference(key, max_bytes)
    }
    fn load_bounded(
        &self,
        key: &FactKey,
        max_bytes: u64,
    ) -> Result<Option<norn_kernel::BoundedFact>, CellError> {
        self.cell.load_bounded(key, max_bytes)
    }
    fn commit_action_references(
        &mut self,
        commit: CellCommit<FactEvent, JsonObject, JsonObject>,
        proof: norn_kernel::action_commit::ActionReferenceReads,
        quantity: Option<(&norn_ir::quantity_ledger::QuantityRequest, &str)>,
    ) -> Result<CommitReceipt, CommitError<CellError>> {
        self.cell.commit_action_references(commit, proof, quantity)
    }
    fn resolve_accepted_receipt(
        &self,
        reference: &ReceiptReference,
        manifest: &Digest,
    ) -> Result<Option<ReceiptBinding<JsonObject>>, ReceiptBindingError<CellError>> {
        bind_receipt(&Store(self, manifest), reference).map(Some)
    }
    fn load_link(&self, key: &LinkKey) -> Result<LinkState, CellError> {
        self.cell.load_link(key)
    }
    fn commit(
        &mut self,
        commit: CellCommit<FactEvent, JsonObject, JsonObject>,
    ) -> Result<CommitReceipt, CommitError<CellError>> {
        self.cell.commit(commit)
    }
    fn perform(&mut self) -> Result<Option<Performed<JsonObject>>, CellError> {
        self.cell.perform()
    }
    fn observe(&mut self) -> Result<Option<Observed<JsonObject>>, CellError> {
        self.cell.observe()
    }
    fn clock(&mut self) -> Result<Option<DueTimer<JsonObject>>, CellError> {
        self.cell.clock()
    }
    fn publish(&mut self, receipt: &CommitReceipt) -> Result<(), CellError> {
        self.cell.publish(receipt)
    }
}

fn open(path: &Path, issued: BTreeMap<String, Issued>) -> DurableReceiptCell {
    let cell = Sqlite::open(
        path,
        Model::from_definitions(&definitions(), &[]).expect("models the run type"),
        StoreOptions::default(),
        SCOPE,
        |command: &Command<JsonObject>| command.payload.clone(),
    )
    .expect("opens the durable cell");
    DurableReceiptCell { cell, issued }
}

/// A run waiting on its performed `ask` command (its result delivery pending).
fn waiting_run(cell: &mut DurableReceiptCell, run: &str) {
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
                generation: Generation(1),
                idempotency_key: IdempotencyKey(format!("{run}/ask/1")),
                freshness: FreshnessPolicy::AppendOnly,
                deadline: None,
                payload: JsonObject::new(),
            })],
            ..CellCommit::empty()
        })
        .unwrap();
    assert!(cell.cell.perform().unwrap().is_some());
}

/// An accepted generation-1 decision receipt for `run`'s `ask` command.
fn issue(run: &str, choice: &str) -> (String, Issued) {
    let summary = BTreeMap::from([("choice".to_owned(), RuntimeValue::String(choice.to_owned()))]);
    let receipt = FunctionReceipt {
        command_id: format!("{run}/ask"),
        run: RunRef {
            id: run.to_owned(),
            generation: 1,
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
            generation: Generation(1),
        },
        reads: Vec::new(),
        destination: None,
        requires_cell_lane: false,
        record: ResultRecord {
            command: CommandId(format!("{run}/ask")),
            generation: Generation(1),
            attempt: 1,
            receipt: digest.clone(),
            result_identity: result_identity.clone(),
            classification: None,
            reason: None,
        },
        state: CommandState {
            generation: Generation(1),
            cancelled: false,
            accepted: Some(AcceptedResult {
                generation: Generation(1),
                result_identity,
                receipt: digest.clone(),
            }),
            reschedules: 0,
        },
    };
    (
        digest.0,
        Issued {
            receipt,
            acceptance,
        },
    )
}

fn decide<'a>(action: &'a ActionRoot, receipt: &str, key: &str) -> ActionCall<'a> {
    ActionCall {
        action,
        action_artifact_id: "flow.decision_run.decide.action",
        action_version: "1.0.0",
        scope: BTreeMap::from([
            ("orgId".to_owned(), "org_demo".to_owned()),
            ("environmentId".to_owned(), "dev".to_owned()),
        ]),
        actor: "user:ada".to_owned(),
        input: JsonObject::from([
            ("run_id".to_owned(), JsonValue::String("run-1".to_owned())),
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

fn status(cell: &DurableReceiptCell) -> (Option<JsonValue>, Option<JsonValue>) {
    let values = cell
        .load(&FactKey::new(RUN, "run-1"))
        .unwrap()
        .values
        .unwrap_or_default();
    (
        values.get("status").cloned(),
        // The durable host stores an unset nullable property as null.
        values
            .get("choice")
            .cloned()
            .filter(|choice| *choice != JsonValue::Null),
    )
}

fn waiting() -> (Option<JsonValue>, Option<JsonValue>) {
    (
        Some(JsonValue::String("awaiting_perform_ask".to_owned())),
        None,
    )
}

fn decided() -> (Option<JsonValue>, Option<JsonValue>) {
    (
        Some(JsonValue::String("decided".to_owned())),
        Some(JsonValue::String("approve".to_owned())),
    )
}

#[test]
fn receipt_consumption_is_atomic_across_a_crash_and_a_reopen() {
    let types = run_types();
    let catalog = TypeCatalog::new(&types);
    let action = decide_action();
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("cell.sqlite");
    let before_dir = tempfile::tempdir().expect("tempdir");
    let before = before_dir.path().join("cell.sqlite");
    let after_dir = tempfile::tempdir().expect("tempdir");
    let after = after_dir.path().join("cell.sqlite");
    let (digest, _) = issue("run-1", "approve");
    let store = || BTreeMap::from([issue("run-1", "approve")]);

    let mut cell = open(&path, store());
    waiting_run(&mut cell, "run-1");
    let base = cell.cell.commits().unwrap().len();
    // The disk image a crash just before the consuming commit leaves.
    common::crash_image(&path, &before);
    let outcome = commit_action(&mut cell, &catalog, &decide(&action, &digest, "k1"), &Allow)
        .unwrap_or_else(|refusal| panic!("error: the accepted receipt commits: {refusal:?}"));
    assert!(matches!(outcome, ActionOutcome::Committed { .. }));
    // The disk image a crash just after it leaves (WAL included).
    common::crash_image(&path, &after);
    drop(cell);

    // Before the commit: neither the transition nor the consumption survived,
    // so the same receipt still resolves the run exactly once.
    let mut crashed = open(&before, store());
    assert_eq!(status(&crashed), waiting());
    assert_eq!(crashed.cell.commits().unwrap().len(), base);
    commit_action(
        &mut crashed,
        &catalog,
        &decide(&action, &digest, "k1"),
        &Allow,
    )
    .expect("the pending delivery is consumed after recovery");
    assert_eq!(status(&crashed), decided());

    // After the commit, crashed or cleanly closed: both effects survived, and
    // a second consumption is refused by the durable delivery fence.
    for image in [&after, &path] {
        let mut reopened = open(image, store());
        assert_eq!(status(&reopened), decided(), "{}", image.display());
        let commits = reopened.cell.commits().unwrap().len();
        assert_eq!(commits, base + 1, "one consuming commit");
        // Put the run back in its waiting state: only the fence stops a replay.
        let version = reopened.load(&FactKey::new(RUN, "run-1")).unwrap().version;
        reopened
            .cell
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
        let refused = commit_action(
            &mut reopened,
            &catalog,
            &decide(&action, &digest, "k2"),
            &Allow,
        )
        .unwrap_err();
        assert_eq!(refused.code(), "action.receipt_not_pending", "{refused}");
        assert_eq!(status(&reopened), waiting());
    }
}
