#![cfg(feature = "native")]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs,
    reason = "atomic action correspondence fixtures"
)]
//! TXN-ATOMIC-1 / TXN-IDEMPOTENT-1: declared facts, links and outbox share SQLite durability.

use norn_cell::{Model, SqliteCell, StoreOptions};
use norn_ir::{ActionRoot, ObjectTypeDef};
use norn_kernel::cell_host::JsonObject;
use norn_kernel::{
    ActionAuthorizer, ActionAuthzRequest, ActionCall, ActionDenial, ActionOutcome, CellCommit,
    CellHost, Command, DecisionTurn, FactEvent, FactKey, FactMutation, FactVersion, FactWrite,
    LinkKey, LinkMutation, LinkWrite, LogicalTime, SynchronousRule, TypeCatalog, World,
    commit_action,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;

type Cell = SqliteCell<FactEvent, JsonObject, JsonObject>;
const ORDER: &str = "commerce.SalesOrder";
const QUOTE: &str = "commerce.Quote";
struct Allow;
impl ActionAuthorizer for Allow {
    fn authorize(&self, _: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }
}
struct DenyQuote;
impl ActionAuthorizer for DenyQuote {
    fn authorize(&self, request: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        if request.target.object_type.as_str() == QUOTE {
            Err(ActionDenial::Forbidden {
                reason: "secondary write denied".into(),
            })
        } else {
            Ok(())
        }
    }
}
fn types() -> Vec<ObjectTypeDef> {
    let mut definitions = Vec::new();
    let documents: Vec<Value> = serde_json::from_str(include_str!(
        "../../norn-kernel/tests/fixtures/action-writer.json"
    ))
    .unwrap();
    for document in documents {
        if let Some(definition) = document["payload"].get("objectType") {
            definitions.push(serde_json::from_value(definition.clone()).unwrap());
        }
    }
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
fn rule() -> SynchronousRule {
    let documents: Vec<Value> = serde_json::from_str(include_str!(
        "../../norn-kernel/tests/fixtures/action-writer.json"
    ))
    .unwrap();
    SynchronousRule::admit(
        norn_ir::ActionRuleRef {
            artifact_id: "example.atomic.writer.rule".into(),
            version: "1.0.0".into(),
        },
        &serde_json::from_value(
            documents
                .iter()
                .find(|document| document["artifactId"] == "example.atomic.writer.rule")
                .unwrap()["payload"]
                .clone(),
        )
        .unwrap(),
        &BTreeMap::new(),
        norn_kernel::DecisionBinding {
            profile_id: "norn-zen/6".into(),
            environment_hash: "sqlite-action".into(),
        },
    )
    .unwrap()
}
fn action() -> ActionRoot {
    let documents: Vec<Value> = serde_json::from_str(include_str!(
        "../../norn-kernel/tests/fixtures/action-writer.json"
    ))
    .unwrap();
    let document = documents
        .into_iter()
        .find(|document| document["kind"] == "action")
        .unwrap();
    serde_json::from_value(document["payload"].clone()).unwrap()
}

fn call(action: &ActionRoot) -> ActionCall<'_> {
    ActionCall {
        action,
        action_artifact_id: "example.atomic.approve.action",
        action_version: "1.0.0",
        scope: BTreeMap::from([
            ("orgId".into(), "org_demo".into()),
            ("environmentId".into(), "dev".into()),
        ]),
        actor: "SO-1".into(),
        input: serde_json::from_value(json!({"so_number":"SO-1"})).unwrap(),
        idempotency_key: "atomic-request".into(),
        object_type: None,
        turn: DecisionTurn {
            id: "atomic-request".into(),
            now: LogicalTime::from_millis(0),
            world: World::Main,
            consumes: None,
        },
    }
}
fn open(path: &Path, definitions: &[ObjectTypeDef]) -> Cell {
    Cell::open(
        path,
        Model::from_definitions(definitions, &[]).unwrap(),
        StoreOptions::default(),
        "org_demo/dev",
        |_: &Command<JsonObject>| panic!("enqueue must not execute a payment"),
    )
    .unwrap()
}
fn seed(cell: &mut Cell, occupied: bool) {
    let mut proposal = CellCommit::empty();
    for (kind, id) in [(ORDER, "SO-1")]
        .into_iter()
        .chain(occupied.then_some((QUOTE, "OLD")))
    {
        proposal.writes.push(FactWrite {
            key: FactKey::new(kind, id),
            expected: FactVersion::ABSENT,
            mutation: FactMutation::Put {
                values: serde_json::from_value(json!({"so_number":id,"status":"draft"})).unwrap(),
            },
        });
    }
    if occupied {
        proposal.links.push(LinkWrite {
            key: LinkKey::new(
                FactKey::new(ORDER, "SO-1"),
                "quote",
                FactKey::new(QUOTE, "OLD"),
            ),
            expected: FactVersion::ABSENT,
            mutation: LinkMutation::Add {
                values: JsonObject::new(),
            },
        });
    }
    cell.commit(proposal).unwrap();
}
#[test]
fn atomic_action_links_outbox_and_receipt_survive_reopen_and_exact_retry() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("atomic.sqlite");
    let definitions = types();
    let rules = [rule()];
    let catalog = TypeCatalog::new(&definitions).with_action_rules(&rules);
    let action = action();
    let request = call(&action);
    let mut cell = open(&path, &definitions);
    seed(&mut cell, false);
    commit_action(&mut cell, &catalog, &request, &Allow).unwrap();
    assert!(
        cell.load_link(&LinkKey::new(
            FactKey::new(ORDER, "SO-1"),
            "quote",
            FactKey::new(QUOTE, "SO-1")
        ))
        .unwrap()
        .exists()
    );
    assert!(
        cell.load_link(&LinkKey::new(
            FactKey::new(QUOTE, "SO-1"),
            "order",
            FactKey::new(ORDER, "SO-1")
        ))
        .unwrap()
        .exists()
    );
    let commands = cell.store().read().pending_commands().unwrap();
    assert_eq!(commands.len(), 1);
    assert!(commands[0].result.is_none());
    assert!(
        serde_json::to_string(&commands[0].command)
            .unwrap()
            .contains("PaymentIntent")
    );
    let dump = cell.store().read().dump().unwrap();
    drop(cell);
    let mut cell = open(&path, &definitions);
    assert_eq!(cell.store().read().dump().unwrap(), dump);
    assert!(matches!(
        commit_action(&mut cell, &catalog, &request, &Allow).unwrap(),
        ActionOutcome::Replayed { .. }
    ));
    assert_eq!(cell.store().read().dump().unwrap(), dump);
    let mut changed = call(&action);
    changed.actor = "different-principal".into();
    let refusal = commit_action(&mut cell, &catalog, &changed, &Allow).unwrap_err();
    assert_eq!(refusal.code(), "action.idempotency_conflict");
    assert_eq!(cell.store().read().dump().unwrap(), dump);
}
#[test]
fn late_link_failure_and_secondary_denial_persist_no_partial_proposal() {
    let directory = tempfile::tempdir().unwrap();
    let definitions = types();
    let rules = [rule()];
    let catalog = TypeCatalog::new(&definitions).with_action_rules(&rules);
    let action = action();
    let request = call(&action);
    let scenarios: [(bool, &dyn ActionAuthorizer); 2] = [(true, &Allow), (false, &DenyQuote)];
    for (occupied, authorizer) in scenarios {
        let path = directory.path().join(if occupied {
            "cardinality.sqlite"
        } else {
            "authorization.sqlite"
        });
        let mut cell = open(&path, &definitions);
        seed(&mut cell, occupied);
        let dump = cell.store().read().dump().unwrap();
        let error = commit_action(&mut cell, &catalog, &request, authorizer).unwrap_err();
        if occupied {
            assert!(error.to_string().contains("cardinality"), "{error}");
        } else {
            assert_eq!(error.code(), "action.permission_denied");
        }
        assert_eq!(cell.store().read().dump().unwrap(), dump);
        assert!(cell.store().read().pending_commands().unwrap().is_empty());
        drop(cell);
        let cell = open(&path, &definitions);
        assert_eq!(cell.store().read().dump().unwrap(), dump);
        assert!(!cell.load(&FactKey::new(QUOTE, "SO-1")).unwrap().exists());
    }
}

// INV-TRANSFER / TXN-ATOMIC-1 / TXN-IDEMPOTENT-1, independent of T1's pack.
#[test]
fn same_company_custody_transfer_preserves_quantity_value_and_books() {
    let documents: Vec<Value> = serde_json::from_str(include_str!(
        "fixtures/custody-transfer.json"
    )).unwrap();
    let mut definitions: Vec<ObjectTypeDef> = documents.iter()
        .filter_map(|d| d["payload"].get("objectType"))
        .map(|d| serde_json::from_value(d.clone()).unwrap()).collect();
    definitions.push(types().pop().unwrap()); // The ordinary action receipt schema.
    let action: ActionRoot = serde_json::from_value(documents.iter()
        .find(|d| d["kind"] == "action").unwrap()["payload"].clone()).unwrap();
    let rule = SynchronousRule::admit(action.synchronous_rule.clone().unwrap(),
        &serde_json::from_value(documents.iter().find(|d| d["kind"] == "rule").unwrap()["payload"].clone()).unwrap(),
        &BTreeMap::new(), norn_kernel::DecisionBinding {
            profile_id: "norn-zen/6".into(), environment_hash: "custody-transfer".into(),
        }).unwrap();
    let rules = [rule];
    let catalog = TypeCatalog::new(&definitions).with_action_rules(&rules);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("custody.sqlite");
    let mut cell = open(&path, &definitions);
    let holding = FactKey::new("fixture.CustodyHolding", "LOT");
    let movement = FactKey::new("fixture.CustodyMovement", "MOVE");
    let mut seed = CellCommit::empty();
    let seed_rows = [
        (holding.clone(), json!({"id":"LOT","company":"COMPANY","location":"A","quantity":5,"value":80,"status":"ready"})),
        (FactKey::new("fixture.CustodyLocation", "A"), json!({"id":"A","company":"COMPANY"})),
        (FactKey::new("fixture.CustodyLocation", "B"), json!({"id":"B","company":"COMPANY"})),
        (FactKey::new("fixture.CustodyLocation", "OTHER"), json!({"id":"OTHER","company":"ANOTHER"})),
        (FactKey::new("fixture.CustodyAccount", "Inventory"), json!({"id":"Inventory","company":"COMPANY","currency":"USD","amount":80})),
        (FactKey::new("fixture.CustodyAccount", "Opening"), json!({"id":"Opening","company":"COMPANY","currency":"USD","amount":-80})),
        (FactKey::new("fixture.CustodyAccount", "Sales"), json!({"id":"Sales","company":"COMPANY","currency":"USD","amount":0})),
        (FactKey::new("fixture.CustodyAccount", "Receivable"), json!({"id":"Receivable","company":"COMPANY","currency":"USD","amount":0})),
        (FactKey::new("fixture.CustodyAccount", "COGS"), json!({"id":"COGS","company":"COMPANY","currency":"USD","amount":0})),
    ];
    for (key, values) in seed_rows {
        seed.writes.push(FactWrite {key, expected: FactVersion::ABSENT,
            mutation: FactMutation::Put {values: serde_json::from_value(values).unwrap()}});
    }
    cell.commit(seed).unwrap();
    let books = cell.store().read().root_rows("main", "fixture.CustodyAccount", 16).unwrap();
    let book_states: Vec<_> = books.0.iter().map(|(id, _)| cell.load(&FactKey::new("fixture.CustodyAccount", id)).unwrap()).collect();
    assert!(books.1);
    let mut request = call(&action);
    request.action_artifact_id = "example.custody.transfer.action";
    request.actor = "warehouse-operator".into();
    request.input = serde_json::from_value(json!({"holding":"LOT","destination":"B","movement":"MOVE"})).unwrap();
    // Wrong company, no-op and absent destination refuse before any consequences.
    let before = cell.store().read().dump().unwrap();
    for destination in ["OTHER", "A", "MISSING"] {
        request.input.insert("destination".into(), destination.into());
        let refusal = commit_action(&mut cell, &catalog, &request, &Allow).unwrap_err();
        assert_eq!(refusal.code(), if destination == "MISSING" { "action.slice_object_missing" } else { "action.precondition_failed" });
        assert_eq!(cell.store().read().dump().unwrap(), before);
    }
    request.input.insert("destination".into(), "B".into());
    commit_action(&mut cell, &catalog, &request, &Allow).unwrap();
    let state = cell.load(&holding).unwrap().values.unwrap();
    assert_eq!(serde_json::to_value(state).unwrap(), json!({"id":"LOT","company":"COMPANY","location":"B","quantity":5,"value":80,"status":"moved"}));
    assert_eq!(serde_json::to_value(cell.load(&movement).unwrap().values.unwrap()).unwrap(),
        json!({"id":"MOVE","company":"COMPANY","from":"A","to":"B","quantity":5,"value":80}));
    for (link, target) in [("holding", holding), ("destination", FactKey::new("fixture.CustodyLocation", "B"))] {
        assert!(cell.load_link(&LinkKey::new(movement.clone(), link, target)).unwrap().exists());
    }
    // Complete account population, including versions, must be byte-identical.
    assert_eq!(cell.store().read().root_rows("main", "fixture.CustodyAccount", 16).unwrap(), books);
    assert_eq!(books.0.iter().map(|(id, _)| cell.load(&FactKey::new("fixture.CustodyAccount", id)).unwrap()).collect::<Vec<_>>(), book_states);
    assert!(cell.store().read().pending_commands().unwrap().is_empty());
    let committed = cell.store().read().dump().unwrap();
    drop(cell);
    let mut cell = open(&path, &definitions);
    assert_eq!(cell.store().read().dump().unwrap(), committed);
    assert!(matches!(commit_action(&mut cell, &catalog, &request, &Allow).unwrap(), ActionOutcome::Replayed {..}));
    assert_eq!(cell.store().read().dump().unwrap(), committed);
    request.input.insert("destination".into(), "A".into());
    assert_eq!(commit_action(&mut cell, &catalog, &request, &Allow).unwrap_err().code(), "action.idempotency_conflict");
    assert_eq!(cell.store().read().dump().unwrap(), committed);
}
