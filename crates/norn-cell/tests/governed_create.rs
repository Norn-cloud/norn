#![cfg(feature = "native")]
//! CREATE-WRITER-1 / ACTOR-STAMP-1 / ACTOR-INDEPENDENT-1 (#1101, #1091).
//!
//! Through the production SQLite host: a governed create stamps its creator
//! from the verified principal, a writer computes the new record's remaining
//! fields and a second object in the same receipt, and only a different,
//! currently authorized principal decides it. Two neutral domains: an
//! Accounting invoice submitter and payment approver, and a Supply Chain
//! transfer requester and releasing custodian. The fixture is generated from
//! `tools/norn-ts/fixtures/decisions/governed-create.source.ts`.

mod quantity_support;

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Mutex;

use norn_cell::{Model, SqliteCell, StoreOptions};
use norn_ir::{ActionRoot, ArtifactEnvelope, ArtifactKind, ObjectTypeDef, RuntimeValue};
use norn_kernel::cell_host::JsonObject;
use norn_kernel::{
    ActionAuthorizer, ActionAuthzRequest, ActionCall, ActionDenial, ActionOutcome,
    ActionReadAuthzRequest, ActionReadTypeAuthzRequest, CellCommit, CellHost, Command,
    DecisionTurn, FactEvent, FactKey, FactMutation, FactVersion, FactWrite, LogicalTime,
    SynchronousRule, TransitionGuard, TypeCatalog, World, actor_derived_fields, commit_action,
    commit_action_with_limits, commit_action_with_preparation, prepare_action_input_for_call,
    transition_guards,
};
use serde_json::{Value, json};

type Cell = SqliteCell<FactEvent, JsonObject, JsonObject>;

const FIXTURE: &str = include_str!("fixtures/governed-create.json");
const INVOICE: &str = "ledger.Invoice";
const POSTING: &str = "ledger.Posting";
const TAX_CODE: &str = "ledger.TaxCode";
const TRANSFER: &str = "supply.Transfer";
const PAYABLE: &str = "ledger.Payable";
const RECEIPT: &str = "ledger.GoodsReceipt";
const GUARDED: &str = "a writer cannot fire a transition guarded by independentOf";
const SUBMITTER: &str = "person:submitter";
const APPROVER: &str = "person:approver";
const INVOICE_WRITER: &str = "ledger.invoice.writer.rule";

/// The scoped authority mapper's current grants. A principal acts only while
/// it holds membership, and every check reads the grants as they are now, so
/// a revocation takes effect at the next check, including the commit's.
struct Members(Mutex<BTreeSet<String>>);

#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "integration-test fixtures and assertions"
)]
impl Members {
    fn of(principals: &[&str]) -> Self {
        Self(Mutex::new(
            principals.iter().map(|name| (*name).to_owned()).collect(),
        ))
    }

    fn revoke(&self, principal: &str) {
        self.0.lock().unwrap().remove(principal);
    }

    fn member(&self, principal: &str) -> Result<(), ActionDenial> {
        if self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .contains(principal)
        {
            Ok(())
        } else {
            Err(ActionDenial::Forbidden {
                reason: "membership revoked".into(),
            })
        }
    }
}

impl ActionAuthorizer for Members {
    fn authorize(&self, request: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        self.member(request.actor)
    }
    fn authorize_read(&self, _: &ActionReadAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }
    fn authorize_read_type(&self, _: &ActionReadTypeAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }
}

#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "integration-test fixtures and assertions"
)]
fn documents() -> Vec<Value> {
    serde_json::from_str(FIXTURE).unwrap()
}

#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "integration-test fixtures and assertions"
)]
fn types() -> Vec<ObjectTypeDef> {
    let mut definitions: Vec<ObjectTypeDef> = documents()
        .iter()
        .filter_map(|document| document["payload"].get("objectType"))
        .map(|definition| serde_json::from_value(definition.clone()).unwrap())
        .collect();
    let mut fields: Vec<Value> = ["actionId", "actor", "turn", "transition", "from", "to"]
        .iter()
        .map(|id| json!({"id":id,"type":{"kind":"text"}}))
        .collect();
    fields.extend(
        [
            "actionArtifactId",
            "actionVersion",
            "effectKind",
            "ruleBinding",
        ]
        .iter()
        .map(|id| json!({"id":id,"type":{"kind":"text"},"constraints":{"nullable":true}})),
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
        .map(|id| json!({"id":id,"type":{"kind":"json-ext"},"constraints":{"nullable":true}})),
    );
    definitions.push(
        serde_json::from_value(json!({"name":"norn.ActionCall","properties":fields})).unwrap(),
    );
    definitions
}

/// Every generated artifact; the cell and catalog retain its laws.
#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "integration-test fixtures and assertions"
)]
fn artifacts() -> Vec<ArtifactEnvelope> {
    let artifacts: Vec<ArtifactEnvelope> = serde_json::from_str(FIXTURE).unwrap();
    assert!(
        artifacts
            .iter()
            .any(|artifact| artifact.kind() == ArtifactKind::Law)
    );
    artifacts
}

#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "integration-test fixtures and assertions"
)]
fn writer() -> SynchronousRule {
    admit_rule(&rule_document(INVOICE_WRITER))
}

#[allow(
    clippy::unwrap_used,
    reason = "integration-test fixtures and assertions"
)]
fn rule_document(id: &str) -> Value {
    documents()
        .into_iter()
        .find(|document| document["artifactId"] == id)
        .unwrap()
}

/// The generated invoice writer with its fact output replaced by `facts`,
/// and its closed output schema widened to name the protected creator and to
/// fire transitions, so a test can drive a writer that breaks the create
/// contract through the kernel.
#[allow(
    clippy::indexing_slicing,
    reason = "integration-test fixtures and assertions"
)]
fn writer_emitting(facts: &str) -> SynchronousRule {
    let mut document = rule_document(INVOICE_WRITER);
    document["payload"]["graph"]["nodes"][1]["content"]["rules"][0]["out_facts"] = json!(facts);
    let item = &mut document["payload"]["outputSchema"]["properties"]["facts"]["items"];
    for member in ["values", "patch"] {
        item["properties"][member]["properties"]["submitted_by"] = json!({"type": "string"});
    }
    item["properties"]["op"]["enum"] = json!(["create", "patch", "transition"]);
    for member in ["lifecycle", "transition"] {
        item["properties"][member] = json!({"type": "string"});
    }
    admit_rule(&document)
}

#[allow(
    clippy::unwrap_used,
    reason = "integration-test fixtures and assertions"
)]
fn admit_rule(document: &Value) -> SynchronousRule {
    SynchronousRule::admit(
        norn_ir::ActionRuleRef {
            artifact_id: document["artifactId"].as_str().unwrap().into(),
            version: "1.0.0".into(),
        },
        &serde_json::from_value(document["payload"].clone()).unwrap(),
        &BTreeMap::new(),
        norn_kernel::DecisionBinding {
            profile_id: "norn-zen/7".into(),
            environment_hash: "sqlite-action".into(),
        },
    )
    .unwrap()
}

/// ACTOR-INDEPENDENT-1: the transition guards of every fixture action, as a
/// host binds them from its admitted actions.
fn guards() -> Vec<TransitionGuard> {
    let actions: Vec<ActionRoot> = documents()
        .iter()
        .filter(|document| document["kind"] == "action")
        .filter_map(|document| serde_json::from_value(document["payload"].clone()).ok())
        .collect();
    transition_guards(&actions)
}

#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "integration-test fixtures and assertions"
)]
fn action(id: &str) -> ActionRoot {
    let document = documents()
        .into_iter()
        .find(|document| document["kind"] == "action" && document["payload"]["actionId"] == id)
        .unwrap();
    serde_json::from_value(document["payload"].clone()).unwrap()
}

#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "integration-test fixtures and assertions"
)]
fn call<'a>(action: &'a ActionRoot, actor: &str, input: Value, key: &str) -> ActionCall<'a> {
    ActionCall {
        action,
        action_artifact_id: "governed.create.fixture",
        action_version: "1.0.0",
        scope: BTreeMap::from([
            ("orgId".into(), "org_demo".into()),
            ("environmentId".into(), "dev".into()),
        ]),
        actor: actor.into(),
        input: serde_json::from_value(input).unwrap(),
        idempotency_key: key.into(),
        object_type: None,
        turn: DecisionTurn {
            id: key.into(),
            now: LogicalTime::from_millis(0),
            world: World::Main,
            consumes: None,
        },
        trace: None,
    }
}

#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "integration-test fixtures and assertions"
)]
fn open(path: &Path, definitions: &[ObjectTypeDef], artifacts: &[ArtifactEnvelope]) -> Cell {
    Cell::open(
        path,
        Model::from_definitions(definitions, &[])
            .unwrap()
            .with_governing_laws(artifacts)
            .unwrap(),
        StoreOptions::default(),
        "org_demo/dev",
        |_: &Command<JsonObject>| panic!("a governed create enqueues no command"),
    )
    .unwrap()
}

#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "integration-test fixtures and assertions"
)]
fn seed_tax_codes(cell: &mut Cell) {
    let mut proposal = CellCommit::empty();
    for (code, fee) in [("STD", 5_i64), ("NEG", -5_i64)] {
        proposal.writes.push(FactWrite {
            key: FactKey::new(TAX_CODE, code),
            expected: FactVersion::ABSENT,
            mutation: FactMutation::Put {
                values: serde_json::from_value(json!({"code": code, "flat_fee": fee})).unwrap(),
            },
        });
    }
    cell.commit(proposal).unwrap();
}

#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "integration-test fixtures and assertions"
)]
fn values(cell: &Cell, kind: &str, key: &str) -> Option<JsonObject> {
    cell.load(&FactKey::new(kind, key)).unwrap().values
}

#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "integration-test fixtures and assertions"
)]
fn text<'a>(values: &'a JsonObject, property: &str) -> Option<&'a str> {
    values.get(property).and_then(RuntimeValue::as_str)
}

#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "integration-test fixtures and assertions"
)]
fn integer(values: &JsonObject, property: &str) -> i64 {
    match serde_json::to_value(&values[property]).unwrap() {
        Value::Number(number) => number.as_i64().unwrap(),
        Value::String(text) => text.parse().unwrap(),
        other @ (Value::Null | Value::Bool(_) | Value::Array(_) | Value::Object(_)) => {
            panic!("{property} is not an integer: {other}")
        }
    }
}

#[allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "integration-test fixtures and assertions"
)]
fn submit(invoice: &str, tax_code: &str, amount: i64) -> Value {
    json!({"invoice_id": invoice, "tax_code": tax_code, "amount": amount})
}

/// CREATE-WRITER-1 + ACTOR-STAMP-1 + ACTOR-INDEPENDENT-1 (Accounting): the
/// submitter's create commits once with its stamped creator, defaults, initial
/// state, writer-computed fields and posting; the submitter cannot approve
/// their own invoice, and a second authorized principal can.
#[test]
fn submitter_creates_and_only_an_independent_approver_decides() {
    let directory = tempfile::tempdir().unwrap();
    let definitions = types();
    let rules = [writer()];
    let artifacts = artifacts();
    let guards = guards();
    let unbound = TypeCatalog::new(&definitions)
        .with_action_rules(&rules)
        .with_governing_laws(&artifacts);
    let catalog = TypeCatalog::new(&definitions)
        .with_action_rules(&rules)
        .with_governing_laws(&artifacts)
        .with_transition_guards(&guards);
    let mut cell = open(
        &directory.path().join("ledger.sqlite"),
        &definitions,
        &artifacts,
    );
    seed_tax_codes(&mut cell);
    let members = Members::of(&[SUBMITTER, APPROVER]);
    let submit_action = action("ledger.invoice.submit");
    let approve_action = action("ledger.invoice.approve");

    let created = call(
        &submit_action,
        SUBMITTER,
        submit("INV-1", "STD", 100),
        "s-1",
    );
    assert!(matches!(
        commit_action(&mut cell, &catalog, &created, &members).unwrap(),
        ActionOutcome::Committed { .. }
    ));
    let invoice = values(&cell, INVOICE, "INV-1").expect("created invoice");
    assert_eq!(text(&invoice, "submitted_by"), Some(SUBMITTER));
    assert_eq!(text(&invoice, "status"), Some("submitted"));
    assert_eq!(integer(&invoice, "amount"), 100);
    assert_eq!(integer(&invoice, "tax"), 5);
    assert_eq!(integer(&invoice, "total"), 105);
    let posting = values(&cell, POSTING, "INV-1").expect("posting in the same receipt");
    assert_eq!(integer(&posting, "amount"), 105);
    // TXN-IDEMPOTENT-1: an exact retry replays and writes nothing new.
    let dump = cell.store().read().dump().unwrap();
    assert!(matches!(
        commit_action(&mut cell, &catalog, &created, &members).unwrap(),
        ActionOutcome::Replayed { .. }
    ));
    assert_eq!(cell.store().read().dump().unwrap(), dump);

    let own = call(
        &approve_action,
        SUBMITTER,
        json!({"invoice_id": "INV-1"}),
        "a-own",
    );
    let refusal = commit_action(&mut cell, &catalog, &own, &members).unwrap_err();
    assert_eq!(refusal.code(), "action.permission_denied", "{refusal}");
    assert_eq!(cell.store().read().dump().unwrap(), dump);

    let independent = call(
        &approve_action,
        APPROVER,
        json!({"invoice_id": "INV-1"}),
        "a-1",
    );
    // An embedded caller that bound no transition guards fails closed on a
    // type with protected actors: sibling guards are unknown, not absent.
    let refusal = commit_action(&mut cell, &unbound, &independent, &members).unwrap_err();
    assert_eq!(refusal.code(), "action.permission_denied", "{refusal}");
    assert!(refusal.to_string().contains("not bound"), "{refusal}");
    assert_eq!(cell.store().read().dump().unwrap(), dump);
    assert!(matches!(
        commit_action(&mut cell, &catalog, &independent, &members).unwrap(),
        ActionOutcome::Committed { .. }
    ));
    let invoice = values(&cell, INVOICE, "INV-1").unwrap();
    assert_eq!(text(&invoice, "status"), Some("approved"));
    // The historical creator is unchanged by the approval.
    assert_eq!(text(&invoice, "submitted_by"), Some(SUBMITTER));
}

/// CREATE-WRITER-1: an existing id refuses (no silent upsert), and a law
/// violated by the writer-computed create refuses the whole proposal.
#[test]
fn writer_create_refuses_an_existing_target_and_a_violated_law_atomically() {
    let directory = tempfile::tempdir().unwrap();
    let definitions = types();
    let rules = [writer()];
    let artifacts = artifacts();
    let catalog = TypeCatalog::new(&definitions)
        .with_action_rules(&rules)
        .with_governing_laws(&artifacts);
    let mut cell = open(
        &directory.path().join("ledger.sqlite"),
        &definitions,
        &artifacts,
    );
    seed_tax_codes(&mut cell);
    let members = Members::of(&[SUBMITTER]);
    let submit_action = action("ledger.invoice.submit");
    let first = call(
        &submit_action,
        SUBMITTER,
        submit("INV-1", "STD", 100),
        "s-1",
    );
    commit_action(&mut cell, &catalog, &first, &members).unwrap();
    let dump = cell.store().read().dump().unwrap();

    let again = call(
        &submit_action,
        SUBMITTER,
        submit("INV-1", "STD", 900),
        "s-2",
    );
    let refusal = commit_action(&mut cell, &catalog, &again, &members).unwrap_err();
    assert_eq!(refusal.code(), "action.write_mode_failed", "{refusal}");
    assert_eq!(cell.store().read().dump().unwrap(), dump);

    // A negative fee makes the computed total fall below the amount.
    let violating = call(
        &submit_action,
        SUBMITTER,
        submit("INV-2", "NEG", 100),
        "s-3",
    );
    let refusal = commit_action(&mut cell, &catalog, &violating, &members).unwrap_err();
    assert_eq!(refusal.code(), "core.laws.violated", "{refusal}");
    assert_eq!(cell.store().read().dump().unwrap(), dump);
    assert!(values(&cell, INVOICE, "INV-2").is_none());
    assert!(values(&cell, POSTING, "INV-2").is_none());
}

/// ACTION-ACTOR-1 / ACTOR-STAMP-1: the creator is never caller input. The
/// stamped field is what a public catalog strips, and a supplied value (even
/// the caller's own id) refuses before anything is prepared.
#[test]
fn caller_supplied_creator_is_refused_and_never_offered() {
    let directory = tempfile::tempdir().unwrap();
    let definitions = types();
    let rules = [writer()];
    let catalog = TypeCatalog::new(&definitions).with_action_rules(&rules);
    let mut cell = open(&directory.path().join("ledger.sqlite"), &definitions, &[]);
    seed_tax_codes(&mut cell);
    let members = Members::of(&[SUBMITTER]);
    let submit_action = action("ledger.invoice.submit");
    assert_eq!(
        actor_derived_fields(&submit_action),
        BTreeSet::from(["submitted_by".to_owned()])
    );
    assert_eq!(
        submit_action.actor_stamped_fields(),
        BTreeSet::from(["submitted_by".to_owned()])
    );
    let dump = cell.store().read().dump().unwrap();
    for supplied in ["person:someone-else", SUBMITTER] {
        let mut input = submit("INV-1", "STD", 100);
        input["submitted_by"] = json!(supplied);
        let spoofed = call(&submit_action, SUBMITTER, input, "s-spoof");
        let refusal = commit_action(&mut cell, &catalog, &spoofed, &members).unwrap_err();
        assert_eq!(refusal.code(), "action.invalid_input", "{refusal}");
        assert_eq!(cell.store().read().dump().unwrap(), dump);
    }
}

/// ACTOR-STAMP-1 at commit (defense in depth behind the Ring-0 checker): an
/// update, a create from caller input and a delegated stamp that names a
/// different principal all refuse; the recorded creator never changes.
#[test]
fn protected_creator_cannot_be_spoofed_or_overwritten_at_commit() {
    let directory = tempfile::tempdir().unwrap();
    let definitions = types();
    let rules = [writer()];
    let catalog = TypeCatalog::new(&definitions).with_action_rules(&rules);
    let mut cell = open(&directory.path().join("ledger.sqlite"), &definitions, &[]);
    seed_tax_codes(&mut cell);
    let members = Members::of(&[SUBMITTER, APPROVER]);
    let submit_action = action("ledger.invoice.submit");
    let first = call(
        &submit_action,
        SUBMITTER,
        submit("INV-1", "STD", 100),
        "s-1",
    );
    commit_action(&mut cell, &catalog, &first, &members).unwrap();
    let dump = cell.store().read().dump().unwrap();

    // An admission-refused shape forced through the kernel: caller-written
    // protected creator, as an update and as a create.
    let mut forged = submit_action.clone();
    forged.derive.clear();
    forged.synchronous_rule = None;
    forged.slice.clear();
    forged.writes = vec![
        norn_ir::ActionWrite {
            entity_kind: INVOICE.into(),
            change: norn_ir::WriteChange::Created,
        },
        norn_ir::ActionWrite {
            entity_kind: INVOICE.into(),
            change: norn_ir::WriteChange::Modified,
        },
    ];
    for (mode, invoice) in [
        (norn_ir::UpsertWriteMode::Upsert, "INV-1"),
        (norn_ir::UpsertWriteMode::CreateOnly, "INV-9"),
    ] {
        let Some(norn_ir::ActionEffectProgram::UpsertInputObject { write_mode, .. }) =
            forged.effect_program.as_mut()
        else {
            panic!("the submit action upserts");
        };
        *write_mode = mode;
        let mut input = submit(invoice, "STD", 100);
        input["submitted_by"] = json!(APPROVER);
        let spoofed = call(&forged, APPROVER, input, &format!("forged-{invoice}"));
        let refusal = commit_action(&mut cell, &catalog, &spoofed, &members).unwrap_err();
        assert_eq!(
            refusal.code(),
            "decision.invalid_output",
            "{invoice}: {refusal}"
        );
        assert_eq!(cell.store().read().dump().unwrap(), dump);
    }
    // A stamp derivation that a second derivation rewrites is not a stamp.
    let mut rewritten = submit_action.clone();
    rewritten.derive.push(norn_ir::ActionInputDerivation {
        field: "submitted_by".into(),
        expression: norn_ir::ZenExpression("'person:someone-else'".into()),
        input_required: false,
    });
    assert!(rewritten.actor_stamped_fields().is_empty());
    let laundered = call(
        &rewritten,
        SUBMITTER,
        submit("INV-8", "STD", 100),
        "s-laundered",
    );
    let refusal = commit_action(&mut cell, &catalog, &laundered, &members).unwrap_err();
    assert_eq!(refusal.code(), "decision.invalid_output", "{refusal}");
    assert_eq!(cell.store().read().dump().unwrap(), dump);
    assert_eq!(
        text(&values(&cell, INVOICE, "INV-1").unwrap(), "submitted_by"),
        Some(SUBMITTER)
    );
}

/// AUTH-REVOKE / current authority (Supply Chain): the historical requester
/// stays on the record, while the releasing custodian's authority is the
/// mapper's at commit. Membership revoked between preparation and commit
/// refuses; the requester never releases their own transfer.
#[test]
fn custodian_authority_is_current_at_commit_and_independent_of_the_requester() {
    const REQUESTER: &str = "person:requester";
    const CUSTODIAN: &str = "person:custodian";
    let directory = tempfile::tempdir().unwrap();
    let definitions = types();
    let guards = guards();
    let catalog = TypeCatalog::new(&definitions).with_transition_guards(&guards);
    let mut cell = open(&directory.path().join("supply.sqlite"), &definitions, &[]);
    let members = Members::of(&[REQUESTER, CUSTODIAN]);
    let request_action = action("supply.transfer.request");
    let release_action = action("supply.transfer.release");
    let requested = call(
        &request_action,
        REQUESTER,
        json!({"transfer_id": "TR-1", "sku": "SKU-7", "quantity": 12}),
        "r-1",
    );
    commit_action(&mut cell, &catalog, &requested, &members).unwrap();
    let transfer = values(&cell, TRANSFER, "TR-1").unwrap();
    assert_eq!(text(&transfer, "requested_by"), Some(REQUESTER));
    assert_eq!(text(&transfer, "status"), Some("requested"));
    let dump = cell.store().read().dump().unwrap();

    let own = call(
        &release_action,
        REQUESTER,
        json!({"transfer_id": "TR-1"}),
        "x-own",
    );
    let refusal = commit_action(&mut cell, &catalog, &own, &members).unwrap_err();
    assert_eq!(refusal.code(), "action.permission_denied", "{refusal}");
    assert_eq!(cell.store().read().dump().unwrap(), dump);

    let release = call(
        &release_action,
        CUSTODIAN,
        json!({"transfer_id": "TR-1"}),
        "x-1",
    );
    let preparation = prepare_action_input_for_call::<norn_cell::CellError>(
        &release_action,
        &release.input,
        release.action_artifact_id,
        release.action_version,
        &release.actor,
    )
    .unwrap();
    members.revoke(CUSTODIAN);
    let refusal =
        commit_action_with_preparation(&mut cell, &catalog, &release, &members, preparation)
            .unwrap_err();
    assert_eq!(refusal.code(), "action.permission_denied", "{refusal}");
    assert_eq!(cell.store().read().dump().unwrap(), dump);

    let restored = Members::of(&[CUSTODIAN]);
    assert!(matches!(
        commit_action(&mut cell, &catalog, &release, &restored).unwrap(),
        ActionOutcome::Committed { .. }
    ));
    let transfer = values(&cell, TRANSFER, "TR-1").unwrap();
    assert_eq!(text(&transfer, "status"), Some("released"));
    assert_eq!(text(&transfer, "requested_by"), Some(REQUESTER));
}

/// CREATE-WRITER-1 / ACTOR-STAMP-1: a governed-create writer only `set`s the
/// fields of the record it creates. Writing its protected creator refuses
/// when the proposal is prepared, even with the caller's own id (a stamp is
/// the kernel's create, never a writer's `set`), and creating the primary
/// record itself refuses the closed writer contract. Nothing commits.
#[test]
fn create_writer_cannot_write_the_creator_or_create_its_target() {
    let directory = tempfile::tempdir().unwrap();
    let definitions = types();
    let submit_action = action("ledger.invoice.submit");
    let members = Members::of(&[SUBMITTER]);
    for (index, (facts, code)) in [
        (
            "[{op: 'patch', objectType: 'ledger.Invoice', key: event.input.invoice_id, patch: {submitted_by: 'person:someone-else'}}]",
            "decision.invalid_output",
        ),
        (
            "[{op: 'patch', objectType: 'ledger.Invoice', key: event.input.invoice_id, patch: {submitted_by: event.actor}}]",
            "decision.invalid_output",
        ),
        (
            "[{op: 'create', objectType: 'ledger.Invoice', key: event.input.invoice_id, values: {tax: 1, total: 101}}]",
            "core.txn.unchecked_writer",
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let rules = [writer_emitting(facts)];
        let catalog = TypeCatalog::new(&definitions).with_action_rules(&rules);
        let mut cell = open(
            &directory.path().join(format!("writer-{index}.sqlite")),
            &definitions,
            &[],
        );
        seed_tax_codes(&mut cell);
        let dump = cell.store().read().dump().unwrap();
        let created = call(&submit_action, SUBMITTER, submit("INV-1", "STD", 100), "s-1");
        let refusal = commit_action(&mut cell, &catalog, &created, &members).unwrap_err();
        assert_eq!(refusal.code(), code, "{facts}: {refusal}");
        assert_eq!(cell.store().read().dump().unwrap(), dump);
        assert!(values(&cell, INVOICE, "INV-1").is_none());
    }
}

/// ACTOR-STAMP-1 at shared host admission (Astra blocker 2): a direct
/// `CellHost::commit` cannot create an object with a protected creator,
/// change or clear an existing one, or delete an object that carries one.
#[test]
fn direct_host_commits_cannot_write_or_delete_a_protected_creator() {
    let directory = tempfile::tempdir().unwrap();
    let definitions = types();
    let rules = [writer()];
    let catalog = TypeCatalog::new(&definitions).with_action_rules(&rules);
    let mut cell = open(&directory.path().join("ledger.sqlite"), &definitions, &[]);
    seed_tax_codes(&mut cell);
    let members = Members::of(&[SUBMITTER]);
    let submit_action = action("ledger.invoice.submit");
    let created = call(
        &submit_action,
        SUBMITTER,
        submit("INV-1", "STD", 100),
        "s-1",
    );
    commit_action(&mut cell, &catalog, &created, &members).unwrap();
    let dump = cell.store().read().dump().unwrap();
    let key = FactKey::new(INVOICE, "INV-1");
    let stored = cell.load(&key).unwrap();
    let mut changed = stored.values.clone().unwrap();
    changed.insert("submitted_by".into(), RuntimeValue::String(APPROVER.into()));
    let mut cleared = stored.values.clone().unwrap();
    cleared.remove("submitted_by");
    let mut forged = changed.clone();
    forged.insert("invoice_id".into(), RuntimeValue::String("INV-2".into()));
    for write in [
        FactWrite {
            key: FactKey::new(INVOICE, "INV-2"),
            expected: FactVersion::ABSENT,
            mutation: FactMutation::Put { values: forged },
        },
        FactWrite {
            key: key.clone(),
            expected: stored.version,
            mutation: FactMutation::Put { values: changed },
        },
        FactWrite {
            key: key.clone(),
            expected: stored.version,
            mutation: FactMutation::Put { values: cleared },
        },
        FactWrite {
            key,
            expected: stored.version,
            mutation: FactMutation::Delete,
        },
    ] {
        let mut proposal = CellCommit::empty();
        proposal.writes.push(write);
        let refusal = cell.commit(proposal).unwrap_err();
        assert_eq!(refusal.code(), "commit.immutable_object", "{refusal}");
        assert_eq!(cell.store().read().dump().unwrap(), dump);
    }
}

#[allow(
    clippy::unwrap_used,
    reason = "integration-test fixtures and assertions"
)]
fn seed_receipt(cell: &mut Cell) {
    let mut proposal = CellCommit::empty();
    proposal.writes.push(FactWrite {
        key: FactKey::new(RECEIPT, "GR-1"),
        expected: FactVersion::ABSENT,
        mutation: FactMutation::Put {
            values: serde_json::from_value(json!({"receipt_id": "GR-1", "status": "received"}))
                .unwrap(),
        },
    });
    cell.commit(proposal).unwrap();
}

/// CREATE-WRITER-1 (#1101) with an unguarded secondary transition: raising a
/// payable stamps its submitter and matches its goods receipt in one commit.
/// A host that binds no transition guards lets no writer fire a transition.
#[test]
fn create_writer_fires_an_unguarded_secondary_transition_atomically() {
    let directory = tempfile::tempdir().unwrap();
    let definitions = types();
    let rules = [admit_rule(&rule_document(
        "ledger.payable.match_writer.rule",
    ))];
    let guards = guards();
    let raise = action("ledger.payable.raise");
    let members = Members::of(&[SUBMITTER]);
    let input = json!({"payable_id": "PAY-1", "receipt_id": "GR-1", "amount": 50});

    let unbound = TypeCatalog::new(&definitions).with_action_rules(&rules);
    let mut cell = open(&directory.path().join("unbound.sqlite"), &definitions, &[]);
    seed_receipt(&mut cell);
    let dump = cell.store().read().dump().unwrap();
    let request = call(&raise, SUBMITTER, input.clone(), "p-1");
    let refusal = commit_action(&mut cell, &unbound, &request, &members).unwrap_err();
    assert_eq!(refusal.code(), "core.txn.unchecked_writer", "{refusal}");
    assert_eq!(cell.store().read().dump().unwrap(), dump);

    let catalog = TypeCatalog::new(&definitions)
        .with_action_rules(&rules)
        .with_transition_guards(&guards);
    let mut cell = open(&directory.path().join("bound.sqlite"), &definitions, &[]);
    seed_receipt(&mut cell);
    let request = call(&raise, SUBMITTER, input, "p-1");
    assert!(matches!(
        commit_action(&mut cell, &catalog, &request, &members).unwrap(),
        ActionOutcome::Committed { .. }
    ));
    let payable = values(&cell, PAYABLE, "PAY-1").expect("created payable");
    assert_eq!(text(&payable, "submitted_by"), Some(SUBMITTER));
    let receipt = values(&cell, RECEIPT, "GR-1").unwrap();
    assert_eq!(text(&receipt, "status"), Some("matched"));
}

/// ACTOR-INDEPENDENT-1 at commit (Astra ruling): the guard covers the
/// effective transition. A writer firing the guarded approval on another
/// invoice refuses, and a sibling action that omits `independentOf` is held
/// to the guard bound from the admitted actions.
#[test]
fn guarded_transition_holds_for_writers_and_sibling_actions() {
    let directory = tempfile::tempdir().unwrap();
    let definitions = types();
    let guards = guards();
    let members = Members::of(&[SUBMITTER, APPROVER]);
    let submit_action = action("ledger.invoice.submit");

    let rules = [writer()];
    let catalog = TypeCatalog::new(&definitions)
        .with_action_rules(&rules)
        .with_transition_guards(&guards);
    let mut cell = open(&directory.path().join("sibling.sqlite"), &definitions, &[]);
    seed_tax_codes(&mut cell);
    let created = call(
        &submit_action,
        SUBMITTER,
        submit("INV-1", "STD", 100),
        "s-1",
    );
    commit_action(&mut cell, &catalog, &created, &members).unwrap();
    let dump = cell.store().read().dump().unwrap();
    let mut sibling = action("ledger.invoice.approve");
    sibling.action_id = "ledger.invoice.fast_approve".into();
    sibling.independent_of.clear();
    let own = call(&sibling, SUBMITTER, json!({"invoice_id": "INV-1"}), "f-own");
    let refusal = commit_action(&mut cell, &catalog, &own, &members).unwrap_err();
    assert_eq!(refusal.code(), "action.permission_denied", "{refusal}");
    assert_eq!(cell.store().read().dump().unwrap(), dump);

    let rules = [writer_emitting(
        "[{op: 'transition', objectType: 'ledger.Invoice', key: 'INV-1', lifecycle: 'status', transition: 'approve'}]",
    )];
    let catalog = TypeCatalog::new(&definitions)
        .with_action_rules(&rules)
        .with_transition_guards(&guards);
    let routed = call(
        &submit_action,
        SUBMITTER,
        submit("INV-2", "STD", 100),
        "s-2",
    );
    let refusal = commit_action(&mut cell, &catalog, &routed, &members).unwrap_err();
    assert_eq!(refusal.code(), "core.txn.unchecked_writer", "{refusal}");
    assert!(refusal.to_string().contains(GUARDED), "{refusal}");
    assert_eq!(cell.store().read().dump().unwrap(), dump);

    // The same guard on an object the writer captured in its slice: the
    // refusal is the guard's, not an out-of-slice write.
    let rules = [admit_rule(&rule_document(
        "ledger.payable.match_writer.rule",
    ))];
    let guarded = [TransitionGuard {
        object_type: norn_ir::ObjectTypeName(RECEIPT.into()),
        lifecycle: norn_ir::PropertyId("status".into()),
        transition: norn_ir::TransitionId("match".into()),
        independent_of: vec![norn_ir::PropertyId("receipt_id".into())],
    }];
    let catalog = TypeCatalog::new(&definitions)
        .with_action_rules(&rules)
        .with_transition_guards(&guarded);
    seed_receipt(&mut cell);
    let dump = cell.store().read().dump().unwrap();
    let raise = action("ledger.payable.raise");
    let input = json!({"payable_id": "PAY-1", "receipt_id": "GR-1", "amount": 50});
    let request = call(&raise, SUBMITTER, input, "p-1");
    let refusal = commit_action(&mut cell, &catalog, &request, &members).unwrap_err();
    assert_eq!(refusal.code(), "core.txn.unchecked_writer", "{refusal}");
    assert!(refusal.to_string().contains(GUARDED), "{refusal}");
    assert_eq!(cell.store().read().dump().unwrap(), dump);
}

/// ACTOR-STAMP-1 with a quantity effect (Astra round 2): the stamped create
/// and its posting commit in one transaction under the stamp's admission.
#[test]
fn stamped_create_with_a_quantity_effect_commits_atomically() {
    use norn_ir::quantity_ledger::{QuantityOperation, QuantityResolution};
    let directory = tempfile::tempdir().unwrap();
    let mut definitions = types();
    let transfer = definitions
        .iter_mut()
        .find(|definition| definition.name.as_str() == TRANSFER)
        .unwrap();
    transfer.properties.extend(
        serde_json::from_value::<Vec<norn_ir::Property>>(json!([
            {"id": "reservation_id", "type": {"kind": "text"}},
            {"id": "posting_id", "type": {"kind": "text"}}
        ]))
        .unwrap(),
    );
    let catalog = TypeCatalog::new(&definitions);
    let mut cell = open(&directory.path().join("quantity.sqlite"), &definitions, &[]);
    let mut request_action = action("supply.transfer.request");
    request_action.quantity_effect = Some(norn_ir::ActionQuantityEffect {
        ledger_id: "stock".into(),
        balance_versions_field: "balance_versions".into(),
        operation: norn_ir::ActionQuantityOperation::PostReservation {
            reservation_id_field: "reservation_id".into(),
            posting_id_field: "posting_id".into(),
        },
    });
    let mut schema = serde_json::to_value(&request_action.input_schema).unwrap();
    schema["properties"]["reservation_id"] = json!({"type": "string"});
    schema["properties"]["posting_id"] = json!({"type": "string"});
    schema["properties"]["balance_versions"] = json!({"type": "array", "items": {
        "type": "object", "additionalProperties": false,
        "properties": {"account": {"type": "string"}, "unit": {"type": "string"},
            "layer": {"type": "string"}, "version": {"type": "integer"}},
        "required": ["account", "unit", "layer", "version"]}});
    for field in ["reservation_id", "posting_id", "balance_versions"] {
        schema["required"]
            .as_array_mut()
            .unwrap()
            .push(json!(field));
    }
    request_action.input_schema = serde_json::from_value(schema).unwrap();
    for (key, mut request) in [
        ("define", quantity_support::define()),
        ("fund", quantity_support::post(0, "fund", "fund", 20)),
        ("reserve", quantity_support::reserve(0, "r", 10, 100)),
    ] {
        request.balance_versions = cell
            .store()
            .read()
            .quantity_balance_versions("main", &request)
            .unwrap();
        cell.commit_with_partition_postings(CellCommit::empty(), &[request], key)
            .unwrap();
    }
    let posting = quantity_support::request(
        0,
        QuantityOperation::PostReservation {
            reservation_id: "r".into(),
            posting_id: "shipment".into(),
            resolution: QuantityResolution::All,
        },
    );
    let fences = cell
        .store()
        .read()
        .quantity_balance_versions("main", &posting)
        .unwrap();
    let requested = call(
        &request_action,
        "person:requester",
        json!({"transfer_id": "TR-1", "sku": "SKU-7", "quantity": 12,
            "reservation_id": "r", "posting_id": "shipment", "balance_versions": fences}),
        "q-1",
    );
    let members = Members::of(&["person:requester"]);
    assert!(matches!(
        commit_action(&mut cell, &catalog, &requested, &members).unwrap(),
        ActionOutcome::Committed { .. }
    ));
    let transfer = values(&cell, TRANSFER, "TR-1").expect("created transfer");
    assert_eq!(text(&transfer, "requested_by"), Some("person:requester"));
    assert!(
        cell.store()
            .read()
            .quantity_posting("main", "stock", "shipment")
            .unwrap()
            .is_some()
    );
}

/// ACTOR-STAMP-1 (Astra round 2): a protected default is compared as hosts
/// store it, so an ordinary create of a type with a protected timestamp
/// default given with an offset admits its canonical UTC value.
#[test]
fn protected_default_is_compared_in_its_canonical_form() {
    let directory = tempfile::tempdir().unwrap();
    let mut definitions = types();
    definitions.push(
        serde_json::from_value(json!({"name": "ledger.AuditNote", "properties": [
            {"id": "note_id", "type": {"kind": "text"}},
            {"id": "recorded_at", "type": {"kind": "timestamp", "timezone": "utc"},
                "protected": true, "default": "2026-01-01T02:00:00+02:00"}
        ]}))
        .unwrap(),
    );
    let mut cell = open(&directory.path().join("note.sqlite"), &definitions, &[]);
    for (key, recorded_at) in [
        ("N-1", "2026-01-01T00:00:00Z"),
        ("N-2", "2026-01-01T02:00:00+02:00"),
    ] {
        let mut proposal = CellCommit::empty();
        proposal.writes.push(FactWrite {
            key: FactKey::new("ledger.AuditNote", key),
            expected: FactVersion::ABSENT,
            mutation: FactMutation::Put {
                values: serde_json::from_value(json!({"note_id": key, "recorded_at": recorded_at}))
                    .unwrap(),
            },
        });
        cell.commit(proposal).unwrap();
    }
    let mut forged = CellCommit::empty();
    forged.writes.push(FactWrite {
        key: FactKey::new("ledger.AuditNote", "N-3"),
        expected: FactVersion::ABSENT,
        mutation: FactMutation::Put {
            values: serde_json::from_value(
                json!({"note_id": "N-3", "recorded_at": "2026-01-02T00:00:00Z"}),
            )
            .unwrap(),
        },
    });
    let refusal = cell.commit(forged).unwrap_err();
    assert_eq!(refusal.code(), "commit.immutable_object", "{refusal}");
}

/// ACTOR-STAMP-1 ordering (Astra round 2): a create that lost a race to a
/// concurrent create by another principal is the retryable version
/// `Conflict`, never a permanent protected refusal.
#[test]
fn a_create_that_lost_a_race_is_a_conflict() {
    let directory = tempfile::tempdir().unwrap();
    let definitions = types();
    let rules = [writer()];
    let catalog = TypeCatalog::new(&definitions).with_action_rules(&rules);
    let mut cell = open(&directory.path().join("race.sqlite"), &definitions, &[]);
    seed_tax_codes(&mut cell);
    let members = Members::of(&[SUBMITTER]);
    let submit_action = action("ledger.invoice.submit");
    let created = call(
        &submit_action,
        SUBMITTER,
        submit("INV-1", "STD", 100),
        "s-1",
    );
    commit_action(&mut cell, &catalog, &created, &members).unwrap();
    let dump = cell.store().read().dump().unwrap();
    let mut late = values(&cell, INVOICE, "INV-1").unwrap();
    late.insert("submitted_by".into(), RuntimeValue::String(APPROVER.into()));
    let mut proposal = CellCommit::empty();
    proposal.writes.push(FactWrite {
        key: FactKey::new(INVOICE, "INV-1"),
        expected: FactVersion::ABSENT,
        mutation: FactMutation::Put { values: late },
    });
    let refusal = cell.commit(proposal).unwrap_err();
    assert_eq!(refusal.code(), "commit.conflict", "{refusal}");
    assert_eq!(cell.store().read().dump().unwrap(), dump);
}

/// A perimeter host that forwards to the production cell, optionally
/// dropping the prepared links of a stamped create before admission.
struct Tamper<'a> {
    inner: &'a mut Cell,
    drop_links: bool,
}

impl CellHost for Tamper<'_> {
    type Event = FactEvent;
    type Command = JsonObject;
    type Input = JsonObject;
    type Error = norn_cell::CellError;
    fn load(&self, key: &FactKey) -> Result<norn_kernel::FactState, Self::Error> {
        self.inner.load(key)
    }
    fn supports_reference_reads(&self) -> bool {
        self.inner.supports_reference_reads()
    }
    fn load_reference(
        &self,
        key: &FactKey,
        max: u64,
    ) -> Result<Option<norn_kernel::BoundedFact>, Self::Error> {
        self.inner.load_reference(key, max)
    }
    fn load_bounded(
        &self,
        key: &FactKey,
        max: u64,
    ) -> Result<Option<norn_kernel::BoundedFact>, Self::Error> {
        self.inner.load_bounded(key, max)
    }
    fn load_link(&self, key: &norn_kernel::LinkKey) -> Result<norn_kernel::LinkState, Self::Error> {
        self.inner.load_link(key)
    }
    fn read_link_targets(
        &self,
        source: &FactKey,
        link: &str,
        max_targets: usize,
        max_key_bytes: u64,
    ) -> Result<Option<Vec<norn_kernel::LinkReadTarget>>, Self::Error> {
        self.inner
            .read_link_targets(source, link, max_targets, max_key_bytes)
    }
    fn commit(
        &mut self,
        commit: CellCommit<FactEvent, JsonObject, JsonObject>,
    ) -> Result<norn_kernel::CommitReceipt, norn_kernel::CommitError<Self::Error>> {
        self.inner.commit(commit)
    }
    fn commit_stamped(
        &mut self,
        mut commit: CellCommit<FactEvent, JsonObject, JsonObject>,
        proof: norn_kernel::CreatorStampProof,
        quantity: Option<(&norn_ir::quantity_ledger::QuantityRequest, &str)>,
    ) -> Result<norn_kernel::CommitReceipt, norn_kernel::CommitError<Self::Error>> {
        if self.drop_links {
            commit.links.clear();
        }
        self.inner.commit_stamped(commit, proof, quantity)
    }
    fn perform(&mut self) -> Result<Option<norn_kernel::Performed<Self::Input>>, Self::Error> {
        Ok(None)
    }
    fn observe(&mut self) -> Result<Option<norn_kernel::Observed<Self::Input>>, Self::Error> {
        Ok(None)
    }
    fn clock(&mut self) -> Result<Option<norn_kernel::DueTimer<Self::Input>>, Self::Error> {
        Ok(None)
    }
    fn publish(&mut self, receipt: &norn_kernel::CommitReceipt) -> Result<(), Self::Error> {
        self.inner.publish(receipt)
    }
}

/// ACTOR-STAMP-1 (Astra round 3): the stamp proof seals the prepared links
/// as the owned-row proof does. A perimeter host that drops one gets the
/// plain admission, which refuses the stamped creator; the untouched
/// transaction commits with its link.
#[test]
fn a_stamped_create_whose_prepared_links_changed_refuses() {
    let directory = tempfile::tempdir().unwrap();
    let definitions = types();
    let rules = [admit_rule(&rule_document(
        "ledger.payable.match_writer.rule",
    ))];
    let guards = guards();
    let catalog = TypeCatalog::new(&definitions)
        .with_action_rules(&rules)
        .with_transition_guards(&guards);
    let mut cell = open(&directory.path().join("tamper.sqlite"), &definitions, &[]);
    seed_receipt(&mut cell);
    let members = Members::of(&[SUBMITTER]);
    let raise = action("ledger.payable.raise");
    let request = call(
        &raise,
        SUBMITTER,
        json!({"payable_id": "PAY-1", "receipt_id": "GR-1", "amount": 50}),
        "p-1",
    );
    let dump = cell.store().read().dump().unwrap();
    let mut tampered = Tamper {
        inner: &mut cell,
        drop_links: true,
    };
    assert!(commit_action(&mut tampered, &catalog, &request, &members).is_err());
    assert_eq!(cell.store().read().dump().unwrap(), dump);
    let mut forwarded = Tamper {
        inner: &mut cell,
        drop_links: false,
    };
    assert!(matches!(
        commit_action(&mut forwarded, &catalog, &request, &members).unwrap(),
        ActionOutcome::Committed { .. }
    ));
    assert!(
        cell.load_link(&norn_kernel::LinkKey::new(
            FactKey::new(PAYABLE, "PAY-1"),
            "receipt",
            FactKey::new(RECEIPT, "GR-1"),
        ))
        .unwrap()
        .exists()
    );
}

/// ACTOR-INDEPENDENT-1 (Astra round 3): a guard reaches across ancestry. A
/// base-type transition on an object whose subtype introduces a protected
/// actor refuses with guards bound (the subtype action's guard applies) and
/// with guards unbound (fail closed), never slipping through either path.
#[test]
fn a_base_transition_on_a_subtype_with_a_protected_actor_refuses_bound_or_unbound() {
    let directory = tempfile::tempdir().unwrap();
    let mut definitions = types();
    for definition in [
        json!({"name": "ledger.Document", "properties": [
            {"id": "doc_id", "type": {"kind": "text"}},
            {"id": "status", "type": {"kind": "enum", "values": [{"value": "draft"}, {"value": "filed"}]},
                "default": "draft"}],
            "lifecycles": [{"property": "status", "initial": ["draft"], "terminal": ["filed"],
                "transitions": [{"id": "file", "from": ["draft"], "to": "filed"}]}]}),
        json!({"name": "ledger.Claim", "extends": "ledger.Document", "properties": [
            {"id": "filed_by", "type": {"kind": "text"}, "protected": true}]}),
    ] {
        definitions.push(serde_json::from_value(definition).unwrap());
    }
    let action_of = |payload: Value| -> ActionRoot { serde_json::from_value(payload).unwrap() };
    let fire = |kind: &str, guarded: bool| {
        let mut payload = json!({"actionId": format!("{kind}.file"),
            "scopeRequired": ["orgId", "environmentId"], "idempotent": true, "ledgered": true,
            "inputSchema": {"type": "object", "additionalProperties": false,
                "properties": {"doc_id": {"type": "string"}}, "required": ["doc_id"]},
            "outputSchema": {"type": "object", "additionalProperties": false, "properties": {}},
            "writes": [{"entityKind": kind, "change": "modified"}],
            "effectProgram": {"kind": "fire_transition", "objectIdField": "doc_id",
                "objectKind": kind, "entityArtifactId": format!("{kind}.entity"),
                "entityArtifactVersion": "1.0.0", "lifecycle": "status", "transition": "file"}});
        if guarded {
            payload["independentOf"] = json!(["filed_by"]);
        }
        action_of(payload)
    };
    let open_claim = action_of(json!({"actionId": "ledger.claim.open",
        "scopeRequired": ["orgId", "environmentId"], "idempotent": true, "ledgered": true,
        "inputSchema": {"type": "object", "additionalProperties": false,
            "properties": {"doc_id": {"type": "string"}, "filed_by": {"type": "string"}},
            "required": ["doc_id", "filed_by"]},
        "outputSchema": {"type": "object", "additionalProperties": false, "properties": {}},
        "derive": [{"field": "filed_by", "expression": "actor.subjectId", "inputRequired": false}],
        "writes": [{"entityKind": "ledger.Claim", "change": "created"}],
        "effectProgram": {"kind": "upsert_input_object", "objectIdField": "doc_id",
            "objectKind": "ledger.Claim", "entityArtifactId": "ledger.Claim.entity",
            "entityArtifactVersion": "1.0.0", "writeMode": {"kind": "create_only"}}}));
    let guards = transition_guards([&fire("ledger.Claim", true)]);
    let base = fire("ledger.Document", false);
    let unbound = TypeCatalog::new(&definitions);
    let bound = TypeCatalog::new(&definitions).with_transition_guards(&guards);
    let mut cell = open(&directory.path().join("claim.sqlite"), &definitions, &[]);
    let members = Members::of(&[SUBMITTER]);
    let opened = call(&open_claim, SUBMITTER, json!({"doc_id": "CL-1"}), "o-1");
    commit_action(&mut cell, &bound, &opened, &members).unwrap();
    let dump = cell.store().read().dump().unwrap();
    for (catalog, key) in [(&unbound, "f-unbound"), (&bound, "f-bound")] {
        let filed = call(&base, SUBMITTER, json!({"doc_id": "CL-1"}), key);
        let refusal = commit_action(&mut cell, catalog, &filed, &members).unwrap_err();
        assert_eq!(
            refusal.code(),
            "action.permission_denied",
            "{key}: {refusal}"
        );
        assert_eq!(cell.store().read().dump().unwrap(), dump);
    }
}

/// ACTOR-STAMP-1 (Astra round 4): the stamped envelope counts the commit's
/// events, its created links and every event the host attaches to the same
/// transaction. A linked stamped create at the exact event ceiling commits;
/// the same create with one attached audit event refuses atomically.
#[test]
fn a_stamped_create_at_its_event_ceiling_refuses_one_attached_event() {
    let directory = tempfile::tempdir().unwrap();
    let definitions = types();
    let rules = [admit_rule(&rule_document(
        "ledger.payable.match_writer.rule",
    ))];
    let guards = guards();
    let catalog = TypeCatalog::new(&definitions)
        .with_action_rules(&rules)
        .with_transition_guards(&guards);
    let members = Members::of(&[SUBMITTER]);
    let raise = action("ledger.payable.raise");
    let request = call(
        &raise,
        SUBMITTER,
        json!({"payable_id": "PAY-1", "receipt_id": "GR-1", "amount": 50}),
        "p-1",
    );
    let limits = |max_events: u32| norn_ir::EnvelopeLimits {
        max_events,
        ..norn_ir::EnvelopeLimits::V1
    };
    // The exact ceiling: the smallest event limit the linked create commits under.
    let ceiling = (1..=norn_ir::EnvelopeLimits::V1.max_events)
        .find(|max_events| {
            let path = directory.path().join(format!("probe-{max_events}.sqlite"));
            let mut cell = open(&path, &definitions, &[]);
            seed_receipt(&mut cell);
            commit_action_with_limits(
                &mut cell,
                &catalog,
                &request,
                &members,
                &limits(*max_events),
            )
            .is_ok()
        })
        .expect("the linked create commits within the V1 envelope");
    assert!(ceiling > 1);

    let mut cell = open(&directory.path().join("attached.sqlite"), &definitions, &[]);
    seed_receipt(&mut cell);
    let dump = cell.store().read().dump().unwrap();
    let audit = || {
        norn_cell::decision_ledger::ApprovalConsumed {
            decision_id: "audit".into(),
        }
        .event()
        .unwrap()
    };
    // At the ceiling, the one attached event is refused by the stamp's event
    // check itself, before ledger intake ever reads it.
    let refused = cell
        .attributed(vec![audit()], |cell| {
            commit_action_with_limits(cell, &catalog, &request, &members, &limits(ceiling))
                .map_err(Box::new)
        })
        .unwrap_err();
    assert_eq!(refused.code(), "decision.invalid_output", "{refused}");
    assert_eq!(
        refused.to_string(),
        format!(
            "invalid decision output: commit emitted {} events, limit is {ceiling}",
            ceiling.saturating_add(1)
        )
    );
    assert_eq!(cell.store().read().dump().unwrap(), dump);
    // Control: one more admitted event lets the same attachment past the
    // ceiling check, so the refusal above was the ceiling's, not the ledger's
    // (which rejects this unseeded approval on its own).
    let past = cell
        .attributed(vec![audit()], |cell| {
            commit_action_with_limits(
                cell,
                &catalog,
                &request,
                &members,
                &limits(ceiling.saturating_add(1)),
            )
            .map_err(Box::new)
        })
        .unwrap_err();
    assert!(!past.to_string().contains("commit emitted"), "{past}");
    assert_eq!(cell.store().read().dump().unwrap(), dump);
    assert!(values(&cell, PAYABLE, "PAY-1").is_none());
    assert!(
        !cell
            .load_link(&norn_kernel::LinkKey::new(
                FactKey::new(PAYABLE, "PAY-1"),
                "receipt",
                FactKey::new(RECEIPT, "GR-1"),
            ))
            .unwrap()
            .exists()
    );
    assert!(matches!(
        commit_action_with_limits(&mut cell, &catalog, &request, &members, &limits(ceiling))
            .unwrap(),
        ActionOutcome::Committed { .. }
    ));
}

/// CREATE-WRITER-1 (Astra rounds 7 and 8): a writer value is checked against
/// its property before it folds into the create. An invalid `set` refuses
/// with the property's own error in either order around a valid `set`, before
/// preparation or any host commit; the valid value alone commits.
#[test]
fn a_create_writer_cannot_mask_an_invalid_value_with_a_later_set() {
    let directory = tempfile::tempdir().unwrap();
    let mut definitions = types();
    let invoice = definitions
        .iter_mut()
        .find(|definition| definition.name.as_str() == INVOICE)
        .unwrap();
    let mut constrained = serde_json::to_value(&*invoice).unwrap();
    for property in constrained["properties"].as_array_mut().unwrap() {
        if property["id"] == "total" {
            property["constraints"] = json!({"min": "0"});
        }
    }
    *invoice = serde_json::from_value(constrained).unwrap();
    let members = Members::of(&[SUBMITTER]);
    let submit_action = action("ledger.invoice.submit");
    let patch = |total: &str| {
        format!(
            "{{op: 'patch', objectType: 'ledger.Invoice', key: event.input.invoice_id, patch: {{total: {total}}}}}"
        )
    };
    for (name, facts, commits) in [
        (
            "invalid-first",
            format!("[{}, {}]", patch("-1"), patch("105")),
            false,
        ),
        (
            "invalid-last",
            format!("[{}, {}]", patch("105"), patch("-1")),
            false,
        ),
        ("valid", format!("[{}]", patch("105")), true),
    ] {
        let rules = [writer_emitting(&facts)];
        let catalog = TypeCatalog::new(&definitions).with_action_rules(&rules);
        let mut cell = open(
            &directory.path().join(format!("{name}.sqlite")),
            &definitions,
            &[],
        );
        seed_tax_codes(&mut cell);
        let dump = cell.store().read().dump().unwrap();
        let created = call(
            &submit_action,
            SUBMITTER,
            submit("INV-1", "STD", 100),
            "s-1",
        );
        let outcome = commit_action(&mut cell, &catalog, &created, &members);
        if commits {
            assert!(matches!(outcome.unwrap(), ActionOutcome::Committed { .. }));
            assert_eq!(
                integer(&values(&cell, INVOICE, "INV-1").unwrap(), "total"),
                105
            );
        } else {
            let refusal = outcome.unwrap_err();
            assert_eq!(
                refusal.code(),
                "decision.invalid_output",
                "{name}: {refusal}"
            );
            assert!(
                refusal.to_string().starts_with(
                    "invalid decision output: the effective values for ledger.Invoice/INV-1: property `total` "
                ),
                "{name}: {refusal}"
            );
            assert_eq!(cell.store().read().dump().unwrap(), dump, "{name}");
            assert!(values(&cell, INVOICE, "INV-1").is_none(), "{name}");
        }
    }
}
