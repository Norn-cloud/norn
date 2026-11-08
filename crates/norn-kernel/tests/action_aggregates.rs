#![allow(
    clippy::expect_used,
    clippy::panic,
    missing_docs,
    reason = "integration-test fixture and assertion helpers"
)]

//! #1105 U3: aggregate inputs fold over a predicate read during action
//! preparation, and the commit's read-set recheck keeps the fold valid.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::convert::Infallible;

use norn_ir::{ActionRoot, ObjectTypeDef};
use norn_kernel::cell_host::JsonObject;
use norn_kernel::{
    ActionAuthorizer, ActionAuthzRequest, ActionCall, ActionDenial, ActionOutcome,
    ActionReadAuthzRequest, ActionReadTypeAuthzRequest, CellCommit, CellHost, Command, CommitError,
    CommitReceipt, DecisionTurn, DueTimer, FactEvent, FactKey, FactMutation, FactState, FactWrite,
    LinkKey, LinkState, LogicalTime, Observed, Performed, PredicateObject, PredicateQuery,
    TypeCatalog, World, commit_action,
};
use norn_testkit::cell_host::MemoryCell;
use serde_json::{Value, json};

type Cell = MemoryCell<FactEvent, JsonObject, JsonObject>;

fn object(value: Value) -> JsonObject {
    serde_json::from_value(value).expect("exact fixture object")
}

fn types() -> Vec<ObjectTypeDef> {
    serde_json::from_value(json!([
        {"name": "books.Entry", "properties": [
            {"id": "account", "type": {"kind": "text"}},
            {"id": "booked_on", "type": {"kind": "date"}},
            {"id": "amount", "type": {"kind": "decimal", "scale": 2,
                "additivity": {"kind": "additive"}}}
        ]},
        {"name": "books.Spend", "properties": [
            {"id": "code", "type": {"kind": "text"}},
            {"id": "account", "type": {"kind": "text"}}
        ]}
    ]))
    .expect("typed entities")
}

fn action(end_inclusive: bool, max_items: u32, preconditions: &[&str]) -> ActionRoot {
    serde_json::from_value(json!({
        "actionId": "books.spend.create",
        "scopeRequired": ["orgId", "environmentId"],
        "idempotent": true, "ledgered": true,
        "inputSchema": {
            "type": "object", "additionalProperties": false,
            "properties": {
                "code": {"type": "string"}, "account": {"type": "string"},
                "from": {"type": "string"}, "to": {"type": "string"}
            },
            "required": ["code", "account", "from", "to"]
        },
        "outputSchema": {"type": "object", "properties": {}, "additionalProperties": false},
        "aggregates": [
            {"name": "total", "objectType": "books.Entry",
                "filter": {"account": "account"},
                "window": {"property": "booked_on", "start": "from", "end": "to",
                    "startInclusive": true, "endInclusive": end_inclusive},
                "aggregate": {"kind": "sum", "property": "amount"}, "maxItems": max_items},
            {"name": "entries", "objectType": "books.Entry",
                "filter": {"account": "account"},
                "window": {"property": "booked_on", "start": "from", "end": "to",
                    "startInclusive": true, "endInclusive": end_inclusive},
                "aggregate": {"kind": "count"}, "maxItems": max_items}
        ],
        "preconditions": preconditions,
        "writes": [{"entityKind": "books.Spend", "change": "created"}],
        "effectProgram": {
            "kind": "upsert_input_object", "objectKind": "books.Spend", "objectIdField": "code",
            "entityArtifactId": "books.Spend.entity", "entityArtifactVersion": "0.1.0",
            "writeMode": {"kind": "create_only"}
        }
    }))
    .expect("typed action")
}

fn call<'a>(action: &'a ActionRoot, code: &str) -> ActionCall<'a> {
    ActionCall {
        action,
        action_artifact_id: "books.spend.create.action",
        action_version: "0.1.0",
        scope: BTreeMap::from([
            ("orgId".to_owned(), "org_demo".to_owned()),
            ("environmentId".to_owned(), "dev".to_owned()),
        ]),
        actor: "books:clerk".to_owned(),
        input: object(json!({
            "code": code, "account": "A", "from": "2026-11-01", "to": "2026-11-30"
        })),
        idempotency_key: format!("spend-{code}"),
        object_type: None,
        turn: DecisionTurn {
            id: format!("spend-{code}-turn"),
            now: LogicalTime::from_millis(1000),
            world: World::Main,
            consumes: None,
        },
    }
}

struct Authorizer {
    hidden: Option<&'static str>,
}

impl ActionAuthorizer for Authorizer {
    fn authorize(&self, _: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }
    fn authorize_read(&self, request: &ActionReadAuthzRequest<'_>) -> Result<(), ActionDenial> {
        if self.hidden == Some(request.target.key.as_str()) {
            return Err(ActionDenial::Invisible);
        }
        Ok(())
    }
    fn authorize_read_type(&self, _: &ActionReadTypeAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }
}

const ALLOW: Authorizer = Authorizer { hidden: None };

fn put(cell: &mut Cell, id: &str, values: Value) {
    let key = FactKey::new("books.Entry", id);
    let expected = cell.load(&key).expect("current version").version;
    let mut commit = CellCommit::empty();
    commit.writes.push(FactWrite {
        key,
        expected,
        mutation: FactMutation::Put {
            values: object(values),
        },
    });
    cell.commit(commit).expect("seed commit");
}

fn entries() -> Cell {
    let mut cell =
        Cell::new(|command: &Command<JsonObject>| command.payload.clone()).with_types(types());
    for (id, account, on, amount) in [
        ("E1", "A", "2026-11-01", json!(10.00)),
        ("E2", "A", "2026-11-15", json!(25.25)),
        ("E3", "A", "2026-11-30", json!(99.99)),
        ("E4", "B", "2026-11-10", json!(5.00)),
        ("E5", "A", "2026-10-31", json!(1.00)),
    ] {
        put(
            &mut cell,
            id,
            json!({"account": account, "booked_on": on, "amount": amount}),
        );
    }
    cell
}

fn spend_exists(cell: &Cell, code: &str) -> bool {
    cell.load(&FactKey::new("books.Spend", code))
        .expect("spend")
        .exists()
}

/// Acceptance (a), (b), (e): exact sums and counts over the declared edges.
#[test]
fn sums_and_counts_match_the_reference_over_declared_edges() {
    let types = types();
    let catalog = TypeCatalog::new(&types);
    let mut cell = entries();

    let exclusive = action(
        false,
        10,
        &["aggregates.total == 35.25", "aggregates.entries == 2"],
    );
    let outcome = commit_action(&mut cell, &catalog, &call(&exclusive, "S1"), &ALLOW)
        .expect("E1 and E2 fall inside [from, to)");
    assert!(matches!(outcome, ActionOutcome::Committed { .. }));

    let inclusive = action(
        true,
        10,
        &["aggregates.total == 135.24", "aggregates.entries == 3"],
    );
    commit_action(&mut cell, &catalog, &call(&inclusive, "S2"), &ALLOW)
        .expect("the inclusive end admits E3");

    let wrong = action(false, 10, &["aggregates.total == 135.24"]);
    let refusal = commit_action(&mut cell, &catalog, &call(&wrong, "S3"), &ALLOW)
        .expect_err("the exclusive end excludes E3");
    assert_eq!(refusal.code(), "action.precondition_failed");
    assert!(spend_exists(&cell, "S1") && spend_exists(&cell, "S2"));
    assert!(!spend_exists(&cell, "S3"));
}

/// Acceptance (d): more matches than `maxItems` refuses before any write.
#[test]
fn more_matches_than_the_bound_refuse() {
    let types = types();
    let catalog = TypeCatalog::new(&types);
    let mut cell = entries();
    let bounded = action(false, 1, &["aggregates.entries >= 0"]);
    let refusal = commit_action(&mut cell, &catalog, &call(&bounded, "S1"), &ALLOW)
        .expect_err("two matches exceed maxItems 1");
    assert_eq!(refusal.code(), "action.aggregate_bound_exceeded");
    assert!(!spend_exists(&cell, "S1"));
}

/// Members count against the slice's canonical byte bound, so a few large
/// records refuse even when they are under `maxItems`.
#[test]
fn members_larger_than_the_slice_byte_bound_refuse() {
    let types = types();
    let catalog = TypeCatalog::new(&types);
    let mut cell = entries();
    let padding = "x".repeat(100_000);
    for id in ["E1", "E2", "E3"] {
        put(
            &mut cell,
            id,
            json!({"account": "A", "booked_on": "2026-11-05", "amount": 1.00, "note": padding}),
        );
    }
    let action = action(false, 10, &["aggregates.entries >= 0"]);
    let refusal = commit_action(&mut cell, &catalog, &call(&action, "S1"), &ALLOW)
        .expect_err("three 100 KB members exceed the 256 KiB slice bound");
    assert_eq!(refusal.code(), "action.aggregate_bytes_exceeded");
    assert!(!spend_exists(&cell, "S1"));
}

#[test]
fn an_unreadable_member_refuses_instead_of_shrinking_the_fold() {
    let types = types();
    let catalog = TypeCatalog::new(&types);
    let mut cell = entries();
    let action = action(false, 10, &["aggregates.entries >= 0"]);
    let hidden = Authorizer { hidden: Some("E2") };
    let refusal = commit_action(&mut cell, &catalog, &call(&action, "S1"), &hidden)
        .expect_err("a hidden member cannot be folded");
    assert_eq!(refusal.code(), "action.target_not_found");
    assert!(!spend_exists(&cell, "S1"));
}

enum Race {
    /// Another writer adds a matching entry after the fold.
    InsertInside,
    /// Another writer adds an entry outside the window after the fold.
    InsertOutside,
    /// The host cannot enumerate the predicate.
    Unsupported,
}

/// Simulates another writer committing between preparation and commit.
struct RacingCell {
    cell: RefCell<Cell>,
    race: Option<Race>,
}

impl CellHost for RacingCell {
    type Event = FactEvent;
    type Command = JsonObject;
    type Input = JsonObject;
    type Error = Infallible;
    fn load(&self, key: &FactKey) -> Result<FactState, Self::Error> {
        self.cell.borrow().load(key)
    }
    fn load_link(&self, key: &LinkKey) -> Result<LinkState, Self::Error> {
        self.cell.borrow().load_link(key)
    }
    fn read_predicate(
        &self,
        predicate: &PredicateQuery,
        max_members: usize,
        max_bytes: u64,
    ) -> Result<Option<Vec<PredicateObject>>, Self::Error> {
        if matches!(self.race, Some(Race::Unsupported)) {
            return Ok(None);
        }
        self.cell
            .borrow()
            .read_predicate(predicate, max_members, max_bytes)
    }
    fn commit(
        &mut self,
        commit: CellCommit<FactEvent, JsonObject, JsonObject>,
    ) -> Result<CommitReceipt, CommitError<Self::Error>> {
        match self.race.take() {
            Some(Race::InsertInside) => put(
                self.cell.get_mut(),
                "E9",
                json!({"account": "A", "booked_on": "2026-11-20", "amount": 1.00}),
            ),
            Some(Race::InsertOutside) => put(
                self.cell.get_mut(),
                "E9",
                json!({"account": "A", "booked_on": "2026-12-20", "amount": 1.00}),
            ),
            Some(Race::Unsupported) | None => {}
        }
        self.cell.get_mut().commit(commit)
    }
    fn perform(&mut self) -> Result<Option<Performed<Self::Input>>, Self::Error> {
        self.cell.get_mut().perform()
    }
    fn observe(&mut self) -> Result<Option<Observed<Self::Input>>, Self::Error> {
        self.cell.get_mut().observe()
    }
    fn clock(&mut self) -> Result<Option<DueTimer<Self::Input>>, Self::Error> {
        self.cell.get_mut().clock()
    }
    fn publish(&mut self, receipt: &CommitReceipt) -> Result<(), Self::Error> {
        self.cell.get_mut().publish(receipt)
    }
}

/// Acceptance (c): a concurrent write inside the window conflicts; one
/// outside it does not.
#[test]
fn a_concurrent_match_conflicts_and_a_write_outside_the_window_does_not() {
    let types = types();
    let catalog = TypeCatalog::new(&types);
    let action = action(false, 10, &["aggregates.total == 35.25"]);

    let mut inside = RacingCell {
        cell: RefCell::new(entries()),
        race: Some(Race::InsertInside),
    };
    let refusal = commit_action(&mut inside, &catalog, &call(&action, "S1"), &ALLOW)
        .expect_err("the fold's members changed before commit");
    assert_eq!(refusal.code(), "action.aggregate_stale_read");
    assert!(!spend_exists(&inside.cell.borrow(), "S1"));

    let mut outside = RacingCell {
        cell: RefCell::new(entries()),
        race: Some(Race::InsertOutside),
    };
    commit_action(&mut outside, &catalog, &call(&action, "S1"), &ALLOW)
        .expect("a write outside the window leaves the fold valid");
    assert!(spend_exists(&outside.cell.borrow(), "S1"));
}

#[test]
fn a_host_that_cannot_enumerate_the_predicate_fails_closed() {
    let types = types();
    let catalog = TypeCatalog::new(&types);
    let action = action(false, 10, &["aggregates.entries >= 0"]);
    let mut host = RacingCell {
        cell: RefCell::new(entries()),
        race: Some(Race::Unsupported),
    };
    let refusal = commit_action(&mut host, &catalog, &call(&action, "S1"), &ALLOW)
        .expect_err("an incomplete enumeration cannot be folded");
    assert_eq!(refusal.code(), "action.aggregate_unavailable");
}
