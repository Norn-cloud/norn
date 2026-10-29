// Fixture decoding and exact outcome matching use expect/panic only in this test.
#![allow(
    clippy::expect_used,
    clippy::panic,
    missing_docs,
    reason = "focused generated-key action integration regression"
)]

//! Generated keys use prepared input and declared reads for both commit and replay.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::convert::Infallible;

use norn_ir::{ActionRoot, ObjectTypeDef, RuntimeValue};
use norn_kernel::cell_host::{BoundedFact, JsonObject};
use norn_kernel::{
    ActionAuthorizer, ActionAuthzRequest, ActionCall, ActionDenial, ActionOutcome,
    ActionReadAuthzRequest, ActionReadTypeAuthzRequest, CellCommit, CellHost, Command, CommitError,
    CommitReceipt, DecisionTurn, DueTimer, FactEvent, FactKey, FactMutation, FactState, FactWrite,
    LinkKey, LinkReadTarget, LinkState, LogicalTime, NamingSeriesInspectionRequest, Observed,
    Performed, TypeCatalog, World, commit_action,
};
use norn_testkit::cell_host::MemoryCell;
use serde_json::json;

type Cell = MemoryCell<FactEvent, JsonObject, JsonObject>;

fn values(value: serde_json::Value) -> JsonObject {
    serde_json::from_value(value).expect("test values")
}

fn types() -> Vec<ObjectTypeDef> {
    serde_json::from_value(json!([
        {"name": "test.Company", "properties": [
            {"id": "code", "type": {"kind": "text"}},
            {"id": "active", "type": {"kind": "boolean"}}
        ]},
        {"name": "test.NamedOrder", "properties": [
            {"id": "name", "type": {"kind": "text"}},
            {"id": "company", "type": {"kind": "text"}}
        ], "identity": [{
            "name": "primary", "properties": ["name"],
            "namingSeries": {
                "pattern": "SO-{company}-{YYYY}-{#####}",
                "timezone": "America/New_York",
                "scopeFields": ["company"],
                "resetPolicy": "year",
                "gapPolicy": "gapless_within_commit"
            }
        }]}
    ]))
    .expect("test types")
}

fn action() -> ActionRoot {
    serde_json::from_value(json!({
        "actionId": "test.named_order.create",
        "scopeRequired": ["orgId", "environmentId"],
        "idempotent": true,
        "ledgered": true,
        "inputSchema": {
            "type": "object", "additionalProperties": false,
            "properties": {
                "name": {"type": "string"},
                "company": {"type": "string"}
            },
            "required": ["company"]
        },
        "outputSchema": {"type": "object", "properties": {}, "additionalProperties": false},
        "derive": [{"field": "company", "expression": "upper(trim(input.company))"}],
        "slice": [{"name": "company", "objectType": "test.Company",
                   "key": {"property": {"from": "input", "property": "company"}}}],
        "preconditions": ["len(name) > 0", "slice.company.active"],
        "writes": [{"entityKind": "test.NamedOrder", "change": "created"}],
        "effectProgram": {
            "kind": "upsert_input_object",
            "objectKind": "test.NamedOrder",
            "objectIdField": "name",
            "entityArtifactId": "test.NamedOrder.entity",
            "entityArtifactVersion": "0.1.0",
            "writeMode": {"kind": "create_only"}
        }
    }))
    .expect("test action")
}

fn call(action: &ActionRoot) -> ActionCall<'_> {
    ActionCall {
        action,
        action_artifact_id: "test.named_order.create.action",
        action_version: "0.1.0",
        scope: BTreeMap::from([
            ("orgId".to_owned(), "org_demo".to_owned()),
            ("environmentId".to_owned(), "dev".to_owned()),
        ]),
        actor: "test:manager".to_owned(),
        input: values(json!({"company": " acme "})),
        idempotency_key: "create-1".to_owned(),
        object_type: None,
        turn: DecisionTurn {
            id: "create-1-turn".to_owned(),
            now: LogicalTime::from_millis(1_767_225_600_000),
            world: World::Main,
            consumes: None,
        },
    }
}

struct AllowGenerated;

impl ActionAuthorizer for AllowGenerated {
    fn authorize(&self, request: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        if request.input.get("name") == Some(&RuntimeValue::from(request.target.key.as_str()))
            && request.input.get("company") == Some(&RuntimeValue::from("ACME"))
        {
            Ok(())
        } else {
            Err(ActionDenial::Invisible)
        }
    }

    fn can_inspect_naming_series(&self, _: &NamingSeriesInspectionRequest<'_>) -> bool {
        true
    }

    fn authorize_read(&self, request: &ActionReadAuthzRequest<'_>) -> Result<(), ActionDenial> {
        if request.input.get("company") == Some(&RuntimeValue::from("ACME"))
            && request.input.get("name") == Some(&RuntimeValue::from("SO-ACME-2025-00001"))
        {
            Ok(())
        } else {
            Err(ActionDenial::Invisible)
        }
    }

    fn authorize_read_type(&self, _: &ActionReadTypeAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }
}

fn set_company(cell: &mut Cell, active: bool) {
    let key = FactKey::new("test.Company", "ACME");
    let expected = cell.load(&key).expect("company state").version;
    let mut commit = CellCommit::empty();
    commit.writes.push(FactWrite {
        key,
        expected,
        mutation: FactMutation::Put {
            values: values(json!({"code": "ACME", "active": active})),
        },
    });
    cell.commit(commit).expect("company update");
}

/// Changes the authorized slice fact after evaluation and before the cell CAS.
struct RacingCell {
    cell: RefCell<Cell>,
    race: bool,
}

impl CellHost for RacingCell {
    type Event = FactEvent;
    type Command = JsonObject;
    type Input = JsonObject;
    type Error = Infallible;

    fn load(&self, key: &FactKey) -> Result<FactState, Self::Error> {
        self.cell.borrow().load(key)
    }

    fn load_bounded(
        &self,
        key: &FactKey,
        max_bytes: u64,
    ) -> Result<Option<BoundedFact>, Self::Error> {
        self.cell.borrow().load_bounded(key, max_bytes)
    }

    fn load_link(&self, key: &LinkKey) -> Result<LinkState, Self::Error> {
        self.cell.borrow().load_link(key)
    }

    fn read_link_targets(
        &self,
        source: &FactKey,
        link: &str,
        max_targets: usize,
        max_key_bytes: u64,
    ) -> Result<Option<Vec<LinkReadTarget>>, Self::Error> {
        self.cell
            .borrow()
            .read_link_targets(source, link, max_targets, max_key_bytes)
    }

    fn commit(
        &mut self,
        commit: CellCommit<FactEvent, JsonObject, JsonObject>,
    ) -> Result<CommitReceipt, CommitError<Self::Error>> {
        if self.race {
            self.race = false;
            set_company(self.cell.get_mut(), false);
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

#[test]
fn generated_create_uses_derived_scope_and_slice_and_rechecks_replay() {
    let definitions = types();
    let catalog = TypeCatalog::new(&definitions);
    let action = action();
    let invocation = call(&action);
    let mut cell = Cell::new(|command: &Command<JsonObject>| command.payload.clone());
    set_company(&mut cell, true);

    let ActionOutcome::Committed { record, .. } =
        commit_action(&mut cell, &catalog, &invocation, &AllowGenerated).expect("generated create")
    else {
        panic!("expected a new commit")
    };
    assert_eq!(record.object.key, "SO-ACME-2025-00001");
    assert_eq!(record.input, invocation.input);
    assert_eq!(
        record.effective_input,
        Some(values(json!({
            "name": "SO-ACME-2025-00001",
            "company": "ACME"
        })))
    );
    assert_eq!(record.derived_values.len(), 1);
    assert_eq!(record.precondition_outcomes.len(), 2);

    set_company(&mut cell, false);
    let before_replay = cell.ledger().len();
    assert_eq!(
        commit_action(&mut cell, &catalog, &invocation, &AllowGenerated)
            .expect_err("current slice rejects replay")
            .code(),
        "action.precondition_failed"
    );
    assert_eq!(cell.ledger().len(), before_replay);

    set_company(&mut cell, true);
    let before_replay = cell.ledger().len();
    let ActionOutcome::Replayed { record: replayed } =
        commit_action(&mut cell, &catalog, &invocation, &AllowGenerated).expect("exact replay")
    else {
        panic!("expected replay")
    };
    assert_eq!(replayed, record);
    assert_eq!(cell.ledger().len(), before_replay);
}

#[test]
fn generated_create_refuses_stale_slice_without_allocating_a_number() {
    let definitions = types();
    let catalog = TypeCatalog::new(&definitions);
    let action = action();
    let invocation = call(&action);
    let mut cell = Cell::new(|command: &Command<JsonObject>| command.payload.clone());
    set_company(&mut cell, true);
    let mut host = RacingCell {
        cell: RefCell::new(cell),
        race: true,
    };
    let before = host.cell.borrow().ledger().len();

    let refusal = commit_action(&mut host, &catalog, &invocation, &AllowGenerated)
        .expect_err("slice fact changed before commit");
    assert_eq!(refusal.code(), "action.precondition_stale_read");
    assert_eq!(host.cell.borrow().ledger().len(), before + 1);
    assert!(
        !host
            .cell
            .borrow()
            .load(&FactKey::new("test.NamedOrder", "SO-ACME-2025-00001"))
            .expect("numbered target")
            .exists()
    );
    set_company(host.cell.get_mut(), true);
    let outcome = commit_action(&mut host, &catalog, &invocation, &AllowGenerated)
        .expect("next successful call takes the first number");
    assert!(matches!(outcome, ActionOutcome::Committed { record, .. }
        if record.object.key == "SO-ACME-2025-00001"));
}
