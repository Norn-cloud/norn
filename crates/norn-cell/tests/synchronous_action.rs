#![cfg(feature = "native")]
#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::too_many_lines,
    missing_docs,
    reason = "atomic action correspondence fixtures"
)]
//! TXN-ATOMIC-1 / TXN-IDEMPOTENT-1: declared facts, links and outbox share SQLite durability.

mod quantity_support;

use norn_cell::{Model, SqliteCell, StoreOptions};
use norn_ir::{
    ActionQuantityEffect, ActionQuantityOperation, ActionRoot, ArtifactEnvelope, ArtifactPayload,
    DiagnosticSeverity, ObjectTypeDef, RuleRoot,
};
use norn_kernel::cell_host::JsonObject;
use norn_kernel::{
    ActionAuthorizer, ActionAuthzRequest, ActionCall, ActionDenial, ActionOutcome, CellCommit,
    CellHost, Command, DecisionTurn, FactEvent, FactKey, FactMutation, FactVersion, FactWrite,
    LinkKey, LinkMutation, LinkWrite, LogicalTime, SynchronousRule, TypeCatalog, World,
    commit_action,
};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

type Cell = SqliteCell<FactEvent, JsonObject, JsonObject>;
const ORDER: &str = "commerce.SalesOrder";
const QUOTE: &str = "commerce.Quote";
struct Allow;
impl ActionAuthorizer for Allow {
    fn authorize(&self, _: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }
    fn authorize_read(
        &self,
        _: &norn_kernel::ActionReadAuthzRequest<'_>,
    ) -> Result<(), ActionDenial> {
        Ok(())
    }
    fn authorize_read_type(
        &self,
        _: &norn_kernel::ActionReadTypeAuthzRequest<'_>,
    ) -> Result<(), ActionDenial> {
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

// The scheduler interposes only at the commit boundary; both proposals execute
// their declared rules, CAS checks and writes through the production SQLite host.
struct DeliveryRace<'a, F> {
    loser: &'a mut Cell,
    winner: Cell,
    run_winner: Option<F>,
    winner_only: Option<BTreeMap<String, Vec<String>>>,
}
impl<F: FnOnce(&mut Cell)> DeliveryRace<'_, F> {
    fn compete(&mut self) {
        let run = self.run_winner.take().unwrap();
        run(&mut self.winner);
        self.winner_only = Some(self.winner.store().read().dump().unwrap());
    }
}
impl<F: FnOnce(&mut Cell)> CellHost for DeliveryRace<'_, F> {
    type Event = FactEvent;
    type Command = JsonObject;
    type Input = JsonObject;
    type Error = norn_cell::CellError;
    fn load(&self, key: &FactKey) -> Result<norn_kernel::FactState, Self::Error> {
        self.loser.load(key)
    }
    fn supports_reference_reads(&self) -> bool {
        self.loser.supports_reference_reads()
    }
    fn load_reference(
        &self,
        key: &FactKey,
        max: u64,
    ) -> Result<Option<norn_kernel::BoundedFact>, Self::Error> {
        self.loser.load_reference(key, max)
    }
    fn load_bounded(
        &self,
        key: &FactKey,
        max: u64,
    ) -> Result<Option<norn_kernel::BoundedFact>, Self::Error> {
        self.loser.load_bounded(key, max)
    }
    fn load_link(&self, key: &LinkKey) -> Result<norn_kernel::LinkState, Self::Error> {
        self.loser.load_link(key)
    }
    fn commit(
        &mut self,
        commit: CellCommit<FactEvent, JsonObject, JsonObject>,
    ) -> Result<norn_kernel::CommitReceipt, norn_kernel::CommitError<Self::Error>> {
        self.compete();
        self.loser.commit(commit)
    }
    fn commit_action_references(
        &mut self,
        commit: CellCommit<FactEvent, JsonObject, JsonObject>,
        proof: norn_kernel::action_commit::ActionReferenceReads,
        quantity: Option<(&norn_ir::quantity_ledger::QuantityRequest, &str)>,
    ) -> Result<norn_kernel::CommitReceipt, norn_kernel::CommitError<Self::Error>> {
        self.compete();
        self.loser.commit_action_references(commit, proof, quantity)
    }
    fn perform(&mut self) -> Result<Option<norn_kernel::Performed<Self::Input>>, Self::Error> {
        panic!("enqueue only")
    }
    fn observe(&mut self) -> Result<Option<norn_kernel::Observed<Self::Input>>, Self::Error> {
        panic!("no observation dispatch")
    }
    fn clock(&mut self) -> Result<Option<norn_kernel::DueTimer<Self::Input>>, Self::Error> {
        panic!("no timer dispatch")
    }
    fn publish(&mut self, receipt: &norn_kernel::CommitReceipt) -> Result<(), Self::Error> {
        self.loser.publish(receipt)
    }
}

/// INV-OVERDRAW, TXN-TWO, TXN-READSET-1, TXN-ATOMIC-1, TXN-IDEMPOTENT-1.
#[test]
fn two_deliveries_of_four_against_five_commit_exactly_one_stock_and_books() {
    let documents: Vec<Value> =
        serde_json::from_str(include_str!("fixtures/delivery-race.json")).unwrap();
    let sources: Vec<_> = documents
        .iter()
        .cloned()
        .map(norn_ir::RuntimeValue::from_json)
        .collect::<Result<_, _>>()
        .unwrap();
    let sources = norn_checker::receipt::ReceiptSources::new(&sources, &[]).unwrap();
    let checked = norn_checker::receipt::check_artifact_set_with_receipt(
        &sources,
        &norn_checker::EntityArtifactSetContext {
            registry: &std::collections::BTreeSet::default(),
            base: &[],
            tenant: None,
        },
        &norn_ir::ResourceLimitProfile::norn_limits_v1(),
    )
    .unwrap();
    assert!(checked.receipt.admitted, "{:?}", checked.diagnostics);
    let mut definitions: Vec<ObjectTypeDef> = documents
        .iter()
        .filter_map(|d| d["payload"].get("objectType"))
        .map(|d| serde_json::from_value(d.clone()).unwrap())
        .collect();
    definitions.push(types().pop().unwrap());
    let action: ActionRoot = serde_json::from_value(
        documents.iter().find(|d| d["kind"] == "action").unwrap()["payload"].clone(),
    )
    .unwrap();
    let rule = SynchronousRule::admit(
        action.synchronous_rule.clone().unwrap(),
        &serde_json::from_value(
            documents.iter().find(|d| d["kind"] == "rule").unwrap()["payload"].clone(),
        )
        .unwrap(),
        &BTreeMap::new(),
        norn_kernel::DecisionBinding {
            profile_id: checked.receipt.expr_profile,
            environment_hash: checked.receipt.type_env.unwrap().sha256,
        },
    )
    .unwrap();
    let rules = [rule];
    let catalog = TypeCatalog::new(&definitions).with_action_rules(&rules);
    let directory = tempfile::tempdir().unwrap();
    // Exercise both operation orderings; no thread timing or sleeps decide a winner.
    for (winning, losing) in [("ONE", "TWO"), ("TWO", "ONE")] {
        let path = directory.path().join(format!("{winning}.sqlite"));
        let mut cell = open(&path, &definitions);
        let holding = FactKey::new("fixture.RaceHolding", "LOT");
        let mut seed = CellCommit::empty();
        for (key, values) in [
            (holding.clone(), json!({"id":"LOT","quantity":5,"value":50})),
            (
                FactKey::new("fixture.RaceDelivery", "ONE"),
                json!({"id":"ONE","status":"pending"}),
            ),
            (
                FactKey::new("fixture.RaceDelivery", "TWO"),
                json!({"id":"TWO","status":"pending"}),
            ),
        ] {
            seed.writes.push(FactWrite {
                key,
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put {
                    values: serde_json::from_value(values).unwrap(),
                },
            });
        }
        cell.commit(seed).unwrap();
        let make_call = |id: &str| {
            let mut request = call(&action);
            request.action_artifact_id = "example.delivery.post.action";
            request.actor = "warehouse-operator".into();
            request.input =
                serde_json::from_value(json!({"id":id,"holding":"LOT","quantity":4})).unwrap();
            request.idempotency_key = id.into();
            request.turn.id = id.into();
            request
        };
        let winner_call = make_call(winning);
        let loser_call = make_call(losing);
        let mut race = DeliveryRace {
            loser: &mut cell,
            winner: open(&path, &definitions),
            run_winner: Some(|winner: &mut Cell| {
                commit_action(winner, &catalog, &winner_call, &Allow).unwrap();
            }),
            winner_only: None,
        };
        let refusal = commit_action(&mut race, &catalog, &loser_call, &Allow).unwrap_err();
        assert!(
            matches!(
                refusal.code(),
                "decision.stale_state" | "action.precondition_stale_read"
            ),
            "{refusal}"
        );
        let winner_only = race.winner_only.take().unwrap();
        assert!(race.run_winner.is_none());
        drop(race);
        // Every authoritative table, including receipts, links, ledger and pending
        // effects, must equal the real winner-only state without any exclusions.
        assert_eq!(cell.store().read().dump().unwrap(), winner_only);
        assert_eq!(
            serde_json::to_value(cell.load(&holding).unwrap().values.unwrap()).unwrap(),
            json!({"id":"LOT","quantity":1,"value":10})
        );
        for (id, status) in [(winning, "posted"), (losing, "pending")] {
            let delivery = cell
                .load(&FactKey::new("fixture.RaceDelivery", id))
                .unwrap()
                .values
                .unwrap();
            assert_eq!(delivery.get("status"), Some(&status.into()));
        }
        for (id, expected) in [(winning, true), (losing, false)] {
            let link = LinkKey::new(
                FactKey::new("fixture.RaceDelivery", id),
                "holding",
                holding.clone(),
            );
            assert_eq!(cell.load_link(&link).unwrap().exists(), expected);
        }
        let (legs, complete) = cell
            .store()
            .read()
            .root_rows("main", "fixture.RaceLeg", 4)
            .unwrap();
        assert!(complete);
        assert_eq!(legs.len(), 2);
        for (suffix, account, amount) in [("debit", "COGS", 40), ("credit", "Inventory", -40)] {
            let id = format!("{winning}-{suffix}");
            assert_eq!(
                serde_json::to_value(
                    cell.load(&FactKey::new("fixture.RaceLeg", &id))
                        .unwrap()
                        .values
                        .unwrap()
                )
                .unwrap(),
                json!({"id":id,"delivery":winning,"account":account,"amount":amount,"currency":"USD"})
            );
        }
        assert!(cell.store().read().pending_commands().unwrap().is_empty());
        drop(cell);
        let mut cell = open(&path, &definitions);
        assert_eq!(cell.store().read().dump().unwrap(), winner_only);
        assert!(matches!(
            commit_action(&mut cell, &catalog, &winner_call, &Allow).unwrap(),
            ActionOutcome::Replayed { .. }
        ));
        assert_eq!(
            commit_action(&mut cell, &catalog, &loser_call, &Allow)
                .unwrap_err()
                .code(),
            "action.precondition_failed"
        );
        assert_eq!(cell.store().read().dump().unwrap(), winner_only);
    }
}

// INV-TRANSFER / TXN-ATOMIC-1 / TXN-IDEMPOTENT-1, independent of T1's pack.
#[test]
fn same_company_custody_transfer_preserves_quantity_value_and_books() {
    let documents: Vec<Value> =
        serde_json::from_str(include_str!("fixtures/custody-transfer.json")).unwrap();
    let sources: Vec<_> = documents
        .iter()
        .cloned()
        .map(norn_ir::RuntimeValue::from_json)
        .collect::<Result<_, _>>()
        .unwrap();
    let sources = norn_checker::receipt::ReceiptSources::new(&sources, &[]).unwrap();
    let checked = norn_checker::receipt::check_artifact_set_with_receipt(
        &sources,
        &norn_checker::EntityArtifactSetContext {
            registry: &std::collections::BTreeSet::default(),
            base: &[],
            tenant: None,
        },
        &norn_ir::ResourceLimitProfile::norn_limits_v1(),
    )
    .unwrap();
    assert!(checked.receipt.admitted, "{:?}", checked.diagnostics);
    let mut definitions: Vec<ObjectTypeDef> = documents
        .iter()
        .filter_map(|d| d["payload"].get("objectType"))
        .map(|d| serde_json::from_value(d.clone()).unwrap())
        .collect();
    definitions.push(types().pop().unwrap()); // The ordinary action receipt schema.
    let action: ActionRoot = serde_json::from_value(
        documents.iter().find(|d| d["kind"] == "action").unwrap()["payload"].clone(),
    )
    .unwrap();
    let rule = SynchronousRule::admit(
        action.synchronous_rule.clone().unwrap(),
        &serde_json::from_value(
            documents.iter().find(|d| d["kind"] == "rule").unwrap()["payload"].clone(),
        )
        .unwrap(),
        &BTreeMap::new(),
        norn_kernel::DecisionBinding {
            profile_id: checked.receipt.expr_profile,
            environment_hash: checked.receipt.type_env.unwrap().sha256,
        },
    )
    .unwrap();
    let rules = [rule];
    let catalog = TypeCatalog::new(&definitions).with_action_rules(&rules);
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("custody.sqlite");
    let mut cell = open(&path, &definitions);
    let holding = FactKey::new("fixture.CustodyHolding", "LOT");
    let movement = FactKey::new("fixture.CustodyMovement", "MOVE");
    let mut seed = CellCommit::empty();
    let seed_rows = [
        (
            holding.clone(),
            json!({"id":"LOT","company":"COMPANY","location":"A","quantity":2,"value":20,"status":"ready"}),
        ),
        (
            FactKey::new("fixture.CustodyHolding", "UNMOVED"),
            json!({"id":"UNMOVED","company":"COMPANY","location":"A","quantity":3,"value":60,"status":"ready"}),
        ),
        (
            FactKey::new("fixture.CustodyLocation", "A"),
            json!({"id":"A","company":"COMPANY"}),
        ),
        (
            FactKey::new("fixture.CustodyLocation", "B"),
            json!({"id":"B","company":"COMPANY"}),
        ),
        (
            FactKey::new("fixture.CustodyLocation", "OTHER"),
            json!({"id":"OTHER","company":"ANOTHER"}),
        ),
        (
            FactKey::new("fixture.CustodyAccount", "Inventory"),
            json!({"id":"Inventory","company":"COMPANY","currency":"USD","amount":80}),
        ),
        (
            FactKey::new("fixture.CustodyAccount", "Opening"),
            json!({"id":"Opening","company":"COMPANY","currency":"USD","amount":-80}),
        ),
        (
            FactKey::new("fixture.CustodyAccount", "Sales"),
            json!({"id":"Sales","company":"COMPANY","currency":"USD","amount":0}),
        ),
        (
            FactKey::new("fixture.CustodyAccount", "Receivable"),
            json!({"id":"Receivable","company":"COMPANY","currency":"USD","amount":0}),
        ),
        (
            FactKey::new("fixture.CustodyAccount", "COGS"),
            json!({"id":"COGS","company":"COMPANY","currency":"USD","amount":0}),
        ),
    ];
    for (key, values) in seed_rows {
        seed.writes.push(FactWrite {
            key,
            expected: FactVersion::ABSENT,
            mutation: FactMutation::Put {
                values: serde_json::from_value(values).unwrap(),
            },
        });
    }
    cell.commit(seed).unwrap();
    let unmoved = cell
        .load(&FactKey::new("fixture.CustodyHolding", "UNMOVED"))
        .unwrap();
    let books = cell
        .store()
        .read()
        .root_rows("main", "fixture.CustodyAccount", 16)
        .unwrap();
    let book_states: Vec<_> = books
        .0
        .iter()
        .map(|(id, _)| {
            cell.load(&FactKey::new("fixture.CustodyAccount", id))
                .unwrap()
        })
        .collect();
    assert!(books.1);
    let mut request = call(&action);
    request.action_artifact_id = "example.custody.transfer.action";
    request.actor = "warehouse-operator".into();
    request.input =
        serde_json::from_value(json!({"holding":"LOT","destination":"B","movement":"MOVE"}))
            .unwrap();
    // Wrong company, no-op and absent destination refuse before any consequences.
    let before = cell.store().read().dump().unwrap();
    for destination in ["OTHER", "A", "MISSING"] {
        request
            .input
            .insert("destination".into(), destination.into());
        let refusal = commit_action(&mut cell, &catalog, &request, &Allow).unwrap_err();
        assert_eq!(
            refusal.code(),
            if destination == "MISSING" {
                "action.slice_object_missing"
            } else {
                "action.precondition_failed"
            }
        );
        assert_eq!(cell.store().read().dump().unwrap(), before);
    }
    request.input.insert("destination".into(), "B".into());
    commit_action(&mut cell, &catalog, &request, &Allow).unwrap();
    let state = cell.load(&holding).unwrap().values.unwrap();
    assert_eq!(
        serde_json::to_value(state).unwrap(),
        json!({"id":"LOT","company":"COMPANY","location":"B","quantity":2,"value":20,"status":"moved"})
    );
    assert_eq!(
        serde_json::to_value(cell.load(&movement).unwrap().values.unwrap()).unwrap(),
        json!({"id":"MOVE","company":"COMPANY","from":"A","to":"B","quantity":2,"value":20})
    );
    for (link, target) in [
        ("holding", holding),
        ("destination", FactKey::new("fixture.CustodyLocation", "B")),
    ] {
        assert!(
            cell.load_link(&LinkKey::new(movement.clone(), link, target))
                .unwrap()
                .exists()
        );
    }
    assert_eq!(
        cell.load(&FactKey::new("fixture.CustodyHolding", "UNMOVED"))
            .unwrap(),
        unmoved
    );
    let (holdings, complete) = cell
        .store()
        .read()
        .root_rows("main", "fixture.CustodyHolding", 4)
        .unwrap();
    assert!(complete);
    assert_eq!(holdings.len(), 2);
    // Complete account population, including versions, must be byte-identical.
    assert_eq!(
        cell.store()
            .read()
            .root_rows("main", "fixture.CustodyAccount", 16)
            .unwrap(),
        books
    );
    assert_eq!(
        books
            .0
            .iter()
            .map(|(id, _)| cell
                .load(&FactKey::new("fixture.CustodyAccount", id))
                .unwrap())
            .collect::<Vec<_>>(),
        book_states
    );
    assert!(cell.store().read().pending_commands().unwrap().is_empty());
    let committed = cell.store().read().dump().unwrap();
    drop(cell);
    let mut cell = open(&path, &definitions);
    assert_eq!(cell.store().read().dump().unwrap(), committed);
    assert!(matches!(
        commit_action(&mut cell, &catalog, &request, &Allow).unwrap(),
        ActionOutcome::Replayed { .. }
    ));
    assert_eq!(cell.store().read().dump().unwrap(), committed);
    request.input.insert("destination".into(), "A".into());
    assert_eq!(
        commit_action(&mut cell, &catalog, &request, &Allow)
            .unwrap_err()
            .code(),
        "action.idempotency_conflict"
    );
    assert_eq!(cell.store().read().dump().unwrap(), committed);
}

fn quantity_inputs(mut schema: Value) -> Value {
    let fields = schema["properties"].as_object_mut().unwrap();
    fields.insert("reservation_id".into(), json!({"type":"string"}));
    fields.insert("posting_id".into(), json!({"type":"string"}));
    fields.insert("balance_versions".into(), json!({
        "type":"array", "items":{
            "type":"object", "additionalProperties":false,
            "properties":{"account":{"type":"string"},"unit":{"type":"string"},"layer":{"type":"string"},"version":{"type":"integer"}},
            "required":["account","unit","layer","version"]
        }
    }));
    schema["required"].as_array_mut().unwrap().extend([
        json!("reservation_id"),
        json!("posting_id"),
        json!("balance_versions"),
    ]);
    schema
}

// Typed Rust composition reuses the generated writer and the existing quantity contract.
fn quantity_action() -> (ActionRoot, SynchronousRule) {
    let mut artifacts: Vec<ArtifactEnvelope> = serde_json::from_str(include_str!(
        "../../norn-kernel/tests/fixtures/action-writer.json"
    ))
    .unwrap();
    let mut action = action();
    action.quantity_effect = Some(ActionQuantityEffect {
        ledger_id: "stock".into(),
        balance_versions_field: "balance_versions".into(),
        operation: ActionQuantityOperation::PostReservation {
            reservation_id_field: "reservation_id".into(),
            posting_id_field: "posting_id".into(),
        },
    });
    let input = quantity_inputs(serde_json::to_value(&action.input_schema).unwrap());
    action.input_schema = serde_json::from_value(input.clone()).unwrap();
    let mut writer: RuleRoot = artifacts
        .iter()
        .find(|artifact| artifact.artifact_id == "example.atomic.writer.rule")
        .unwrap()
        .payload
        .to_json_value()
        .decode()
        .unwrap();
    let mut schema = serde_json::to_value(&writer.input_schema).unwrap();
    schema["properties"]["event"]["properties"]["input"] = input;
    writer.input_schema = serde_json::from_value(schema).unwrap();
    for artifact in &mut artifacts {
        let root = match &artifact.payload {
            ArtifactPayload::Action(_) => serde_json::to_value(&action).unwrap(),
            ArtifactPayload::Rule(_) => serde_json::to_value(&writer).unwrap(),
            ArtifactPayload::Entity(_)
            | ArtifactPayload::Relation(_)
            | ArtifactPayload::Connector(_)
            | ArtifactPayload::Widget(_)
            | ArtifactPayload::Customization(_)
            | ArtifactPayload::Setting(_)
            | ArtifactPayload::BusinessPack(_)
            | ArtifactPayload::Policy(_)
            | ArtifactPayload::Derived(_)
            | ArtifactPayload::SourceSchema(_)
            | ArtifactPayload::Mapping(_)
            | ArtifactPayload::Law(_)
            | ArtifactPayload::DefinedConcept(_) => continue,
        };
        artifact.payload = ArtifactPayload::from_kind_json(
            &artifact.kind(),
            serde_json::from_value(root).unwrap(),
        )
        .unwrap();
    }
    let findings = norn_checker::check_artifact_set(
        &artifacts,
        &norn_checker::EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &[],
            tenant: None,
        },
    );
    assert!(
        findings
            .iter()
            .all(|d| d.severity != DiagnosticSeverity::Error),
        "{findings:?}"
    );
    let env = norn_checker::derive_type_env(
        &artifacts,
        &[],
        &norn_ir::ResourceLimitProfile::norn_limits_v1(),
    )
    .unwrap();
    let rule = SynchronousRule::admit(
        action.synchronous_rule.clone().unwrap(),
        &writer,
        &BTreeMap::new(),
        norn_kernel::DecisionBinding {
            profile_id: "norn-zen/6".into(),
            environment_hash: env.reference().unwrap().sha256,
        },
    )
    .unwrap();
    (action, rule)
}

/// TXN-ALL: facts, links, postings, outbox and timer cancellation share one receipt.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one witness compares all effects before refusal and after successful reopen/retry"
)]
fn txn_all_refuses_atomically_and_preserves_the_retained_receipt_after_reopen() {
    use norn_ir::quantity_ledger::{EvaluatedLedgerLeg, QuantityOperation, QuantityResolution};
    let directory = tempfile::tempdir().unwrap();
    let definitions = types();
    let (action, rule) = quantity_action();
    let rules = [rule];
    let catalog = TypeCatalog::new(&definitions).with_action_rules(&rules);
    for occupied in [true, false] {
        let path = directory.path().join(if occupied {
            "refused.sqlite"
        } else {
            "accepted.sqlite"
        });
        let mut cell = open(&path, &definitions);
        seed(&mut cell, occupied);
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
        assert_eq!(cell.store().read().next_timer_due().unwrap(), Some(100));
        let quantity = quantity_support::request(
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
            .quantity_balance_versions("main", &quantity)
            .unwrap();
        let mut request = call(&action);
        request.input.extend(
            serde_json::from_value::<JsonObject>(json!({
                "reservation_id":"r", "posting_id":"shipment", "balance_versions":fences,
            }))
            .unwrap(),
        );
        let before = cell.store().read().dump().unwrap();
        let result = commit_action(&mut cell, &catalog, &request, &Allow);
        if occupied {
            let error = result.unwrap_err();
            assert!(error.to_string().contains("cardinality"), "{error}");
            assert_eq!(cell.store().read().dump().unwrap(), before);
            drop(cell);
            let cell = open(&path, &definitions);
            assert_eq!(cell.store().read().dump().unwrap(), before);
            assert_eq!(cell.store().read().next_timer_due().unwrap(), Some(100));
            continue;
        }
        let ActionOutcome::Committed { receipt, record } = result.unwrap() else {
            panic!("first delivery must commit");
        };
        assert_eq!(receipt.written.len(), 3);
        for key in [FactKey::new(ORDER, "SO-1"), FactKey::new(QUOTE, "SO-1")] {
            assert!(receipt.written.iter().any(|write| write.key == key));
            assert!(cell.load(&key).unwrap().exists());
        }
        assert_eq!(receipt.linked.len(), 2);
        assert_eq!(cell.store().read().pending_commands().unwrap().len(), 1);
        assert_eq!(cell.store().read().next_timer_due().unwrap(), None);
        let posting = cell
            .store()
            .read()
            .quantity_posting("main", "stock", "shipment")
            .unwrap()
            .expect("shipment posting");
        let expected_legs: Vec<_> = [("out", "lot", "-10"), ("in", "transit", "10")]
            .into_iter()
            .map(|(id, account, amount)| EvaluatedLedgerLeg {
                leg_id: id.into(),
                account_id: account.into(),
                unit_id: "item".into(),
                layer: "physical".into(),
                unit_precision: 0,
                amount_unscaled: amount.into(),
            })
            .collect();
        assert_eq!(posting.legs, expected_legs);
        for (account, posted) in [("lot", "10"), ("transit", "10")] {
            let balance = cell
                .store()
                .read()
                .quantity_balance(
                    "main",
                    "stock",
                    &norn_kernel::quantity::QuantityKey {
                        account: account.into(),
                        unit: "item".into(),
                        layer: "physical".into(),
                    },
                )
                .unwrap();
            assert_eq!(balance.posted, posted);
            assert_eq!(balance.held, "0");
        }
        let retained = cell.store().export_record(receipt.seq.0).unwrap().unwrap();
        let committed = cell.store().read().dump().unwrap();
        drop(cell);
        let mut cell = open(&path, &definitions);
        assert_eq!(cell.store().read().dump().unwrap(), committed);
        assert_eq!(
            commit_action(&mut cell, &catalog, &request, &Allow).unwrap(),
            ActionOutcome::Replayed { record }
        );
        assert_eq!(
            cell.store().export_record(receipt.seq.0).unwrap().unwrap(),
            retained
        );
        assert_eq!(cell.store().read().dump().unwrap(), committed);
        request
            .input
            .insert("posting_id".into(), "changed-posting".into());
        assert_eq!(
            commit_action(&mut cell, &catalog, &request, &Allow)
                .unwrap_err()
                .code(),
            "action.idempotency_conflict"
        );
        assert_eq!(cell.store().read().dump().unwrap(), committed);
    }
}
