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
    SynchronousRule, TypeCatalog, World, actor_derived_fields, commit_action,
    commit_action_with_preparation, prepare_action_input_for_call,
};
use serde_json::{Value, json};

type Cell = SqliteCell<FactEvent, JsonObject, JsonObject>;

const FIXTURE: &str = include_str!("fixtures/governed-create.json");
const INVOICE: &str = "ledger.Invoice";
const POSTING: &str = "ledger.Posting";
const TAX_CODE: &str = "ledger.TaxCode";
const TRANSFER: &str = "supply.Transfer";
const SUBMITTER: &str = "person:submitter";
const APPROVER: &str = "person:approver";

/// The scoped authority mapper's current grants. A principal acts only while
/// it holds membership, and every check reads the grants as they are now, so
/// a revocation takes effect at the next check, including the commit's.
struct Members(Mutex<BTreeSet<String>>);

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
        if self.0.lock().unwrap().contains(principal) {
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

fn documents() -> Vec<Value> {
    serde_json::from_str(FIXTURE).unwrap()
}

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
fn artifacts() -> Vec<ArtifactEnvelope> {
    let artifacts: Vec<ArtifactEnvelope> = serde_json::from_str(FIXTURE).unwrap();
    assert!(
        artifacts
            .iter()
            .any(|artifact| artifact.kind() == ArtifactKind::Law)
    );
    artifacts
}

fn writer() -> SynchronousRule {
    let document = documents()
        .into_iter()
        .find(|document| document["artifactId"] == "ledger.invoice.writer.rule")
        .unwrap();
    SynchronousRule::admit(
        norn_ir::ActionRuleRef {
            artifact_id: "ledger.invoice.writer.rule".into(),
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

fn action(id: &str) -> ActionRoot {
    let document = documents()
        .into_iter()
        .find(|document| document["kind"] == "action" && document["payload"]["actionId"] == id)
        .unwrap();
    serde_json::from_value(document["payload"].clone()).unwrap()
}

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

fn values(cell: &Cell, kind: &str, key: &str) -> Option<JsonObject> {
    cell.load(&FactKey::new(kind, key)).unwrap().values
}

fn text<'a>(values: &'a JsonObject, property: &str) -> Option<&'a str> {
    values.get(property).and_then(RuntimeValue::as_str)
}

fn integer(values: &JsonObject, property: &str) -> i64 {
    match serde_json::to_value(&values[property]).unwrap() {
        Value::Number(number) => number.as_i64().unwrap(),
        Value::String(text) => text.parse().unwrap(),
        other => panic!("{property} is not an integer: {other}"),
    }
}

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
    let catalog = TypeCatalog::new(&definitions)
        .with_action_rules(&rules)
        .with_governing_laws(&artifacts);
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
        assert!(commit_action(&mut cell, &catalog, &spoofed, &members).is_err());
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
    assert!(commit_action(&mut cell, &catalog, &laundered, &members).is_err());
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
    let catalog = TypeCatalog::new(&definitions);
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
