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
