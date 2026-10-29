#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::pedantic,
    reason = "integration-test crate: fixture helpers unwrap and index"
)]
#![deny(
    clippy::match_wildcard_for_single_variants,
    clippy::format_collect,
    clippy::format_push_string,
    clippy::inefficient_to_string,
    clippy::large_types_passed_by_value,
    clippy::needless_pass_by_value,
    clippy::large_futures
)]

//! The action commit path (T2-KER-2c, #2895): an authorized `fire_transition`
//! call commits once through the decision-commit path, replays on an exact
//! retry, and otherwise refuses with a stable code and nothing committed.
//! Fixtures: the demo's `commerce.SalesOrder` and `submit_invoice`.

use std::collections::{BTreeMap, BTreeSet};
use std::convert::Infallible;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};

use norn_ir::{ActionRoot, NamingCounterSeed, ObjectTypeDef, ObjectTypeName, PropertyId};
use norn_kernel::action::{ACTION_IDEMPOTENCY_KEY_MAX_BYTES, ActionCommandRejection};
use norn_kernel::cell_host::{JsonObject, JsonValue};
use norn_kernel::{
    ActionAuthorizer, ActionAuthzRequest, ActionCall, ActionDenial, ActionOutcome, ActionRefusal,
    CellCommit, CellHost, Command, CommitError, CommitModel, CommitReceipt, CommitVersions,
    CommitView, DecisionTurn, DueTimer, FactEvent, FactKey, FactMutation, FactSlice, FactState,
    FactVersion, FactWrite, LinkKey, LinkState, LogicalTime, Observed, OwnedRowsCommitProof,
    Performed, TurnEnvelopeLimits, TurnEnvelopeMeter, TypeCatalog, World, check_commit,
    commit_action, commit_action_with_limits, commit_action_with_meter, commit_decision_with_meter,
};
use norn_testkit::cell_host::MemoryCell;
use serde_json::{Value, json};

type Cell = MemoryCell<FactEvent, JsonObject, JsonObject>;

const ORDER: &str = "commerce.SalesOrder";

struct NoReadOrCommit;

impl CellHost for NoReadOrCommit {
    type Event = FactEvent;
    type Command = JsonObject;
    type Input = JsonObject;
    type Error = Infallible;

    fn load(&self, _: &FactKey) -> Result<FactState, Self::Error> {
        panic!("an over-limit action must be refused before target or record loads")
    }

    fn load_link(&self, _: &LinkKey) -> Result<LinkState, Self::Error> {
        panic!("an over-limit action must be refused before link loads")
    }

    fn commit(
        &mut self,
        _: CellCommit<Self::Event, Self::Command, Self::Input>,
    ) -> Result<CommitReceipt, CommitError<Self::Error>> {
        panic!("an over-limit action must be refused before commit")
    }

    fn perform(&mut self) -> Result<Option<Performed<Self::Input>>, Self::Error> {
        panic!("an action commit must not perform")
    }

    fn observe(&mut self) -> Result<Option<Observed<Self::Input>>, Self::Error> {
        panic!("an action commit must not observe")
    }

    fn clock(&mut self) -> Result<Option<DueTimer<Self::Input>>, Self::Error> {
        panic!("an action commit must not clock")
    }

    fn publish(&mut self, _: &CommitReceipt) -> Result<(), Self::Error> {
        panic!("an over-limit action must be refused before publish")
    }
}

fn fixture(path: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(path);
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn order_types() -> Vec<ObjectTypeDef> {
    let entity = fixture("ir/entity-v2/artifact-set/commerce-sales-order.entity.json");
    let other = json!({
        "name": "commerce.Quote",
        "properties": [
            { "id": "so_number", "type": { "kind": "text" } },
            { "id": "status", "type": { "kind": "enum", "values": [{ "value": "draft" }, { "value": "approved" }] }, "default": "draft" }
        ],
        "lifecycles": [{ "property": "status", "initial": ["draft"], "terminal": ["approved"],
                         "transitions": [{ "id": "approve", "from": ["draft"], "to": "approved" }] }]
    });
    vec![
        serde_json::from_value(entity["payload"]["objectType"].clone()).unwrap(),
        serde_json::from_value(other).unwrap(),
    ]
}

fn immutable_order_types() -> Vec<ObjectTypeDef> {
    let mut entity = fixture("ir/entity-v2/artifact-set/commerce-sales-order.entity.json");
    let object_type = entity["payload"]["objectType"].as_object_mut().unwrap();
    let status = object_type["properties"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|property| property["id"] == "status")
        .unwrap();
    status["type"]["values"]
        .as_array_mut()
        .unwrap()
        .push(json!({"value": "submitted"}));
    let lifecycle = object_type["lifecycles"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|lifecycle| lifecycle["property"] == "status")
        .unwrap();
    lifecycle["immutable"] = json!(["submitted", "cancelled"]);
    lifecycle["transitions"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|transition| transition["id"] == "cancel")
        .unwrap()["from"] = json!(["draft", "approved", "submitted"]);
    vec![serde_json::from_value(entity["payload"]["objectType"].clone()).unwrap()]
}

/// `submit_invoice` from the fixture, or a variant firing `transition`.
fn action(action_id: &str, transition: &str) -> ActionRoot {
    let mut payload =
        fixture("ir/commerce-sales-order-submit-invoice.action.json")["payload"].clone();
    payload["actionId"] = json!(action_id);
    payload["effectProgram"]["transition"] = json!(transition);
    serde_json::from_value(payload).unwrap()
}

fn submit_invoice() -> ActionRoot {
    action("commerce.sales_order.submit_invoice", "invoice")
}

fn approve() -> ActionRoot {
    action("commerce.sales_order.approve", "approve")
}

fn approve_with_submit_command(name_expression: &str) -> ActionRoot {
    let mut payload =
        fixture("ir/commerce-sales-order-submit-invoice.action.json")["payload"].clone();
    payload["actionId"] = json!("commerce.sales_order.approve");
    payload["effectProgram"]["transition"] = json!("approve");
    payload["commands"] = json!([{
        "connector": "erpnext",
        "operation": "submit",
        "doctype": "Sales Order",
        "name": name_expression,
        "fields": {}
    }]);
    serde_json::from_value(payload).unwrap()
}

fn approve_with_guarded_update_command(
    expected_modified: &str,
    ownership_value: &str,
    rows: &str,
) -> ActionRoot {
    let mut payload =
        fixture("ir/commerce-sales-order-submit-invoice.action.json")["payload"].clone();
    payload["actionId"] = json!("commerce.sales_order.approve");
    payload["effectProgram"]["transition"] = json!("approve");
    payload["inputSchema"]["properties"]["modified"] = json!({ "type": "string" });
    payload["inputSchema"]["properties"]["owner"] = json!({ "type": "string" });
    payload["inputSchema"]["properties"]["rows"] = json!({
        "type": "array",
        "items": {
            "type": "object",
            "properties": {
                "item_code": { "type": "string" },
                "qty": { "type": "integer" }
            },
            "required": ["item_code", "qty"],
            "additionalProperties": false
        }
    });
    payload["inputSchema"]["required"] = json!(["so_number", "modified", "owner", "rows"]);
    payload["commands"] = json!([{
        "connector": "erpnext",
        "operation": "update",
        "doctype": "Sales Order",
        "name": "input.so_number",
        "expectedModified": expected_modified,
        "fields": { "description": "input.owner" },
        "childMerges": [{
            "field": "items",
            "ownershipField": "custom_norn_owner",
            "ownershipValue": ownership_value,
            "rowKeyField": "item_code",
            "rows": rows
        }]
    }]);
    serde_json::from_value(payload).unwrap()
}

fn approve_with_repeated_command_action(command_count: usize) -> ActionRoot {
    let mut payload =
        fixture("ir/commerce-sales-order-submit-invoice.action.json")["payload"].clone();
    payload["actionId"] = json!("commerce.sales_order.approve");
    payload["effectProgram"]["transition"] = json!("approve");
    payload["inputSchema"]["properties"]["large"] = json!({ "type": "string" });
    payload["inputSchema"]["required"] = json!(["so_number", "large"]);
    let commands: Vec<_> = (0..command_count)
        .map(|_| {
            json!({
                "connector": "erpnext",
                "operation": "create",
                "doctype": "Sales Order",
                "fields": {
                    "quantity": "len(lower(upper(input.large)))",
                    "quantity_2": "len(lower(upper(input.large)))",
                    "quantity_3": "len(lower(upper(input.large)))",
                    "quantity_4": "len(lower(upper(input.large)))",
                    "quantity_5": "len(lower(upper(input.large)))",
                    "quantity_6": "len(lower(upper(input.large)))"
                }
            })
        })
        .collect();
    payload["commands"] = json!(commands);
    serde_json::from_value(payload).unwrap()
}

fn guarded_update_call<'a>(action: &'a ActionRoot, key: &str) -> ActionCall<'a> {
    let mut call = call(action, "SO-1", key);
    call.input.extend([
        (
            "modified".to_owned(),
            JsonValue::String("2026-10-09 13:00:00".to_owned()),
        ),
        (
            "owner".to_owned(),
            JsonValue::String("norn:rule/hold".to_owned()),
        ),
        (
            "rows".to_owned(),
            serde_json::from_value(json!([
                { "item_code": "ITEM-1", "qty": 3 },
                { "item_code": "ITEM-2", "qty": 5 }
            ]))
            .unwrap(),
        ),
    ]);
    call
}

fn repeated_command_call<'a>(
    action: &'a ActionRoot,
    key: &str,
    text_bytes: usize,
) -> ActionCall<'a> {
    let mut call = call(action, "SO-1", key);
    call.input.insert(
        "large".to_owned(),
        JsonValue::String("x".repeat(text_bytes)),
    );
    call
}

fn upsert_action(action_id: &str, write_mode: &Value) -> ActionRoot {
    upsert_action_for(action_id, ORDER, write_mode)
}

fn upsert_action_for(action_id: &str, object_kind: &str, write_mode: &Value) -> ActionRoot {
    let mut payload =
        fixture("ir/commerce-sales-order-submit-invoice.action.json")["payload"].clone();
    payload["actionId"] = json!(action_id);
    payload["effectProgram"] = json!({
        "kind": "upsert_input_object",
        "objectIdField": "so_number",
        "objectKind": object_kind,
        "entityArtifactId": "commerce.SalesOrder.entity",
        "entityArtifactVersion": "0.1.0",
        "writeMode": write_mode
    });
    serde_json::from_value(payload).unwrap()
}

fn business_unit_types() -> Vec<ObjectTypeDef> {
    vec![
        serde_json::from_value(json!({
            "name": "commerce.AuthzOrder",
            "properties": [
                { "id": "so_number", "type": { "kind": "text" } },
                { "id": "business_unit", "type": { "kind": "text" } },
                { "id": "amount", "type": { "kind": "integer" }, "default": "1" }
            ]
        }))
        .unwrap(),
        serde_json::from_value(json!({
            "name": "commerce.OwnAuthzOrder",
            "extends": "commerce.AuthzOrder",
            "properties": [
                { "id": "business_unit", "type": { "kind": "text" }, "default": "BU-own" }
            ]
        }))
        .unwrap(),
        serde_json::from_value(json!({
            "name": "commerce.OtherAuthzOrder",
            "extends": "commerce.AuthzOrder",
            "properties": [
                { "id": "business_unit", "type": { "kind": "text" }, "default": "BU-other" }
            ]
        }))
        .unwrap(),
    ]
}

fn business_unit_upsert_action(action_id: &str) -> ActionRoot {
    let mut action =
        upsert_action_for(action_id, "commerce.AuthzOrder", &json!({"kind": "upsert"}));
    action.writes = vec![
        norn_ir::ActionWrite {
            entity_kind: "commerce.AuthzOrder".to_owned(),
            change: norn_ir::WriteChange::Created,
        },
        norn_ir::ActionWrite {
            entity_kind: "commerce.AuthzOrder".to_owned(),
            change: norn_ir::WriteChange::Modified,
        },
    ];
    if let Some(norn_ir::JsonValue::Object(properties)) = action.input_schema.get_mut("properties")
    {
        properties.insert(
            "business_unit".to_owned(),
            norn_ir::JsonValue::Object(norn_ir::JsonObject::from([(
                "type".to_owned(),
                norn_ir::JsonValue::String("string".to_owned()),
            )])),
        );
    }
    action
}

fn cancel_submitted() -> ActionRoot {
    let mut payload =
        fixture("ir/commerce-sales-order-submit-invoice.action.json")["payload"].clone();
    payload["actionId"] = json!("commerce.sales_order.cancel");
    payload["effectProgram"]["transition"] = json!("cancel");
    serde_json::from_value(payload).unwrap()
}

fn scope() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("orgId".to_owned(), "org_demo".to_owned()),
        ("environmentId".to_owned(), "dev".to_owned()),
    ])
}

fn call<'a>(action: &'a ActionRoot, so_number: &str, key: &str) -> ActionCall<'a> {
    ActionCall {
        action,
        action_artifact_id: "fixture.action",
        action_version: "0.1.0",
        scope: scope(),
        actor: "agent:ops".to_owned(),
        input: JsonObject::from([(
            "so_number".to_owned(),
            JsonValue::String(so_number.to_owned()),
        )]),
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

#[test]
fn overlong_idempotency_key_is_refused_before_any_host_read_or_commit() {
    let types = order_types();
    let catalog = TypeCatalog::new(&types);
    let action = approve();
    let key = "k".repeat(ACTION_IDEMPOTENCY_KEY_MAX_BYTES + 1);
    let mut host = NoReadOrCommit;

    let refusal = commit_action(&mut host, &catalog, &call(&action, "SO-1", &key), &Allow)
        .expect_err("the key is over the byte limit");
    assert!(matches!(
        refusal,
        ActionRefusal::Command(ActionCommandRejection::IdempotencyKeyTooLong { .. })
    ));
    assert_eq!(refusal.code(), "action.idempotency_key_too_long");
    assert_eq!(refusal.category(), norn_kernel::RefusalCategory::Limit);
}

#[test]
fn overlong_target_key_is_refused_before_any_host_read_or_commit() {
    let types = order_types();
    let catalog = TypeCatalog::new(&types);
    let action = approve();
    let object_id = "k".repeat(norn_ir::identifier_limits::MAX_OBJECT_KEY_BYTES + 1);
    let mut host = NoReadOrCommit;

    let refusal = commit_action(
        &mut host,
        &catalog,
        &call(&action, &object_id, "idem-1"),
        &Allow,
    )
    .expect_err("the target key is over the byte limit");
    assert!(matches!(
        refusal,
        ActionRefusal::Decision(norn_kernel::DecisionRefusal::IdentifierTooLong {
            field: "object key",
            ..
        })
    ));
    assert_eq!(refusal.code(), "commit.identifier_too_long");
    assert_eq!(refusal.category(), norn_kernel::RefusalCategory::Limit);
}

struct Allow;

impl ActionAuthorizer for Allow {
    fn can_inspect_naming_series(
        &self,
        _: &norn_kernel::NamingSeriesInspectionRequest<'_>,
    ) -> bool {
        true
    }

    fn authorize(&self, _: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }
}

struct InputMatchesTargetKey;

impl ActionAuthorizer for InputMatchesTargetKey {
    fn authorize(&self, request: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        let generated = request.input.get("name").and_then(JsonValue::as_str);
        if generated == Some(request.target.key.as_str()) {
            Ok(())
        } else {
            Err(ActionDenial::Forbidden {
                reason: "authz.generated_key_mismatch".to_owned(),
            })
        }
    }
}

struct Deny(ActionDenial);

impl ActionAuthorizer for Deny {
    fn authorize(&self, _: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Err(self.0.clone())
    }
}

/// Authorizes one declared property value and retains the exact values the
/// kernel presented at each current/proposed-state check.
struct PropertyAuthorizer {
    property: &'static str,
    allowed: std::collections::BTreeSet<String>,
    observed: Mutex<Vec<Option<JsonObject>>>,
}

impl PropertyAuthorizer {
    fn allowing(property: &'static str, values: &[&str]) -> Self {
        Self {
            property,
            allowed: values.iter().map(|value| (*value).to_owned()).collect(),
            observed: Mutex::new(Vec::new()),
        }
    }

    fn observed(&self) -> Vec<Option<JsonObject>> {
        self.observed.lock().unwrap().clone()
    }
}

impl ActionAuthorizer for PropertyAuthorizer {
    #[expect(
        clippy::unwrap_in_result,
        reason = "This test fixture must panic on failed prerequisites rather than simulate a recoverable domain refusal"
    )]
    fn authorize(&self, request: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        let values = request.target_values.cloned();
        let allowed = values
            .as_ref()
            .and_then(|values| values.get(self.property))
            .and_then(JsonValue::as_str)
            .is_some_and(|value| self.allowed.contains(value));
        self.observed.lock().unwrap().push(values);
        if allowed {
            Ok(())
        } else {
            Err(ActionDenial::Forbidden {
                reason: format!("authz.{}_not_allowed", self.property),
            })
        }
    }
}

struct Revocable(Arc<AtomicBool>);

impl ActionAuthorizer for Revocable {
    fn authorize(&self, _: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        if self.0.load(Ordering::SeqCst) {
            Ok(())
        } else {
            Err(ActionDenial::Forbidden {
                reason: "authz.no_longer_authorized".to_owned(),
            })
        }
    }
}

fn seed(cell: &mut Cell, object_type: &str, so_number: &str, status: &str) -> FactKey {
    let key = FactKey::new(object_type, so_number);
    cell.commit(CellCommit {
        writes: vec![FactWrite {
            key: key.clone(),
            expected: FactVersion::ABSENT,
            mutation: FactMutation::Put {
                values: serde_json::from_value(json!({ "so_number": so_number, "status": status }))
                    .unwrap(),
            },
        }],
        ..CellCommit::empty()
    })
    .unwrap();
    key
}

fn seed_business_unit_order(cell: &mut Cell, key: &str, business_unit: &str) -> FactKey {
    let fact_key = FactKey::new("commerce.AuthzOrder", key);
    cell.commit(CellCommit {
        writes: vec![FactWrite {
            key: fact_key.clone(),
            expected: FactVersion::ABSENT,
            mutation: FactMutation::Put {
                values: JsonObject::from([
                    ("so_number".to_owned(), JsonValue::String(key.to_owned())),
                    (
                        "business_unit".to_owned(),
                        JsonValue::String(business_unit.to_owned()),
                    ),
                    (
                        "amount".to_owned(),
                        JsonValue::Number(norn_ir::Decimal::parse("1").unwrap()),
                    ),
                ]),
            },
        }],
        ..CellCommit::empty()
    })
    .unwrap();
    fact_key
}

fn status(cell: &Cell, key: &FactKey) -> Option<JsonValue> {
    cell.load(key)
        .unwrap()
        .values
        .and_then(|values| values.get("status").cloned())
}

/// A refused call: the expected code, and nothing committed.
#[track_caller]
fn assert_refused(
    cell: &mut Cell,
    catalog: &TypeCatalog<'_>,
    call: &ActionCall<'_>,
    authorizer: &dyn ActionAuthorizer,
    code: &str,
) -> ActionRefusal<Infallible> {
    let ledger = cell.ledger().len();
    let refusal = commit_action(cell, catalog, call, authorizer).unwrap_err();
    assert_eq!(refusal.code(), code, "{refusal}");
    assert!(!refusal.to_string().is_empty());
    assert_eq!(cell.ledger().len(), ledger, "a refused call committed");
    refusal
}

#[test]
fn action_commits_once_and_replays_retries() {
    let types = order_types();
    let catalog = TypeCatalog::new(&types);
    let mut cell = Cell::new(|command: &Command<JsonObject>| command.payload.clone());
    let order = seed(&mut cell, ORDER, "SO-1", "draft");
    let (submit, approve) = (submit_invoice(), approve());

    // The demo's refusal: invoicing a draft order.
    assert_refused(
        &mut cell,
        &catalog,
        &call(&submit, "SO-1", "k1"),
        &Allow,
        "lifecycle.validation.undeclared_write",
    );

    let approved =
        commit_action(&mut cell, &catalog, &call(&approve, "SO-1", "k2"), &Allow).unwrap();
    let ActionOutcome::Committed { receipt, record } = approved else {
        panic!("expected a commit: {approved:?}");
    };
    assert_eq!(
        (record.from.as_str(), record.to.as_str()),
        ("draft", "approved")
    );
    assert_eq!(
        status(&cell, &order),
        Some(JsonValue::String("approved".into()))
    );
    // One commit: the transition event, the object write, the call record.
    let entry = cell.ledger().last().unwrap();
    assert_eq!(entry.seq, receipt.seq);
    assert!(matches!(
        entry.events.as_slice(),
        [FactEvent::Transitioned { .. }, FactEvent::Modified { .. }]
    ));
    assert_eq!(entry.written.len(), 2);

    let invoiced =
        commit_action(&mut cell, &catalog, &call(&submit, "SO-1", "k3"), &Allow).unwrap();
    assert!(matches!(invoiced, ActionOutcome::Committed { .. }));

    // An exact retry replays: no second transition, no new ledger entry.
    let ledger = cell.ledger().len();
    let mut upgraded_call = call(&submit, "SO-1", "k3");
    upgraded_call.action_artifact_id = "fixture.action.v2";
    upgraded_call.action_version = "0.2.0";
    let retried = commit_action(&mut cell, &catalog, &upgraded_call, &Allow).unwrap();
    let ActionOutcome::Replayed { record } = retried else {
        panic!("expected a replay: {retried:?}");
    };
    assert_eq!(
        (record.from.as_str(), record.to.as_str()),
        ("approved", "invoiced")
    );
    assert_eq!(record.action_artifact_id.as_deref(), Some("fixture.action"));
    assert_eq!(record.action_version.as_deref(), Some("0.1.0"));
    let record_key = FactKey::new(
        norn_kernel::ACTION_CALL_TYPE,
        "commerce.sales_order.submit_invoice/k3",
    );
    let values = cell.load(&record_key).unwrap().values.unwrap();
    assert_eq!(
        values.get("actionArtifactId"),
        Some(&JsonValue::String("fixture.action".to_owned()))
    );
    assert_eq!(
        values.get("actionVersion"),
        Some(&JsonValue::String("0.1.0".to_owned()))
    );
    assert_eq!(cell.ledger().len(), ledger);

    // A fresh call (new key) is a second firing: refused from the target state.
    assert_refused(
        &mut cell,
        &catalog,
        &call(&submit, "SO-1", "k4"),
        &Allow,
        "lifecycle.validation.undeclared_write",
    );
    // The same key for a different call is refused.
    let mut other_actor = call(&submit, "SO-1", "k3");
    other_actor.actor = "agent:intruder".to_owned();
    assert_refused(
        &mut cell,
        &catalog,
        &other_actor,
        &Allow,
        "action.idempotency_conflict",
    );
}

#[test]
fn upsert_creates_and_replays_in_one_action_commit() {
    let types = order_types();
    let catalog = TypeCatalog::new(&types);
    let mut cell = Cell::new(|command: &Command<JsonObject>| command.payload.clone());
    let action = upsert_action("commerce.sales_order.create", &json!({"kind": "upsert"}));
    let action_call = call(&action, "SO-CREATE", "create-upsert");

    let committed = commit_action(&mut cell, &catalog, &action_call, &Allow).unwrap();
    let ActionOutcome::Committed { receipt, record } = committed else {
        panic!("expected a commit: {committed:?}");
    };
    assert_eq!(record.effect_kind.as_deref(), Some("upsert_input_object"));
    assert_eq!(
        cell.load(&FactKey::new(ORDER, "SO-CREATE"))
            .unwrap()
            .values
            .as_ref()
            .and_then(|values| values.get("status")),
        Some(&JsonValue::String("draft".to_owned()))
    );
    let entry = cell.ledger().last().unwrap();
    assert_eq!(entry.seq, receipt.seq);
    assert!(matches!(
        entry.events.as_slice(),
        [FactEvent::Created { .. }]
    ));
    assert_eq!(
        entry.written.len(),
        2,
        "object and idempotency record commit together"
    );

    let ledger_len = cell.ledger().len();
    assert!(matches!(
        commit_action(&mut cell, &catalog, &action_call, &Allow).unwrap(),
        ActionOutcome::Replayed { .. }
    ));
    assert_eq!(cell.ledger().len(), ledger_len);
}

#[test]
fn upsert_refuses_immutable_state_and_cancel_transition_still_works() {
    let types = immutable_order_types();
    let catalog = TypeCatalog::new(&types);
    let mut cell = Cell::new(|command: &Command<JsonObject>| command.payload.clone());
    let order = seed(&mut cell, ORDER, "SO-SUBMITTED", "submitted");
    let upsert = upsert_action("commerce.sales_order.edit", &json!({"kind": "upsert"}));

    assert_refused(
        &mut cell,
        &catalog,
        &call(&upsert, "SO-SUBMITTED", "edit-submitted"),
        &Allow,
        "lifecycle.validation.immutable_state_write",
    );
    assert_eq!(
        status(&cell, &order),
        Some(JsonValue::String("submitted".to_owned()))
    );

    let cancel = cancel_submitted();
    let outcome = commit_action(
        &mut cell,
        &catalog,
        &call(&cancel, "SO-SUBMITTED", "cancel-submitted"),
        &Allow,
    )
    .unwrap();
    assert!(matches!(outcome, ActionOutcome::Committed { .. }));
    assert_eq!(
        status(&cell, &order),
        Some(JsonValue::String("cancelled".to_owned()))
    );
}

#[test]
fn create_only_and_draft_only_action_modes_are_enforced() {
    let types = immutable_order_types();
    let catalog = TypeCatalog::new(&types);
    let mut cell = Cell::new(|command: &Command<JsonObject>| command.payload.clone());
    let create_only = upsert_action(
        "commerce.sales_order.create_only",
        &json!({"kind": "create_only"}),
    );
    assert!(matches!(
        commit_action(
            &mut cell,
            &catalog,
            &call(&create_only, "SO-NEW", "create-only"),
            &Allow
        )
        .unwrap(),
        ActionOutcome::Committed { .. }
    ));
    let existing_refusal = commit_action(
        &mut cell,
        &catalog,
        &call(&create_only, "SO-NEW", "create-only-again"),
        &Allow,
    )
    .unwrap_err();
    assert_eq!(existing_refusal.code(), "action.write_mode_failed");
    assert_eq!(
        cell.ledger().len(),
        1,
        "a refused create-only call is effect-free"
    );

    let submitted = seed(&mut cell, ORDER, "SO-SUBMITTED", "submitted");
    let draft_only = upsert_action(
        "commerce.sales_order.draft_edit",
        &json!({"kind": "draft_only", "lifecycle": "status", "state": "draft"}),
    );
    let ledger_len = cell.ledger().len();
    let refusal = commit_action(
        &mut cell,
        &catalog,
        &call(&draft_only, "SO-SUBMITTED", "draft-edit-submitted"),
        &Allow,
    )
    .unwrap_err();
    assert_eq!(refusal.code(), "action.write_mode_failed");
    assert_eq!(cell.ledger().len(), ledger_len);
    assert_eq!(
        status(&cell, &submitted),
        Some(JsonValue::String("submitted".to_owned()))
    );
}

#[test]
fn upsert_authorizes_canonical_proposed_values_for_creates() {
    let types = business_unit_types();
    let catalog = TypeCatalog::new(&types);
    let mut cell = Cell::new(|command: &Command<JsonObject>| command.payload.clone());
    let action = business_unit_upsert_action("commerce.authz_order.create");

    let mut own = call(&action, "OWN-1", "create-own");
    own.object_type = Some(ObjectTypeName("commerce.OwnAuthzOrder".to_owned()));
    let own_authorizer = PropertyAuthorizer::allowing("business_unit", &["BU-own"]);
    assert!(matches!(
        commit_action(&mut cell, &catalog, &own, &own_authorizer).unwrap(),
        ActionOutcome::Committed { .. }
    ));
    let expected = JsonObject::from([
        (
            "so_number".to_owned(),
            JsonValue::String("OWN-1".to_owned()),
        ),
        (
            "business_unit".to_owned(),
            JsonValue::String("BU-own".to_owned()),
        ),
        (
            "amount".to_owned(),
            JsonValue::Number(norn_ir::Decimal::parse("1").unwrap()),
        ),
    ]);
    assert_eq!(own_authorizer.observed(), vec![Some(expected.clone())]);
    assert_eq!(
        cell.load(&FactKey::new("commerce.OwnAuthzOrder", "OWN-1"))
            .unwrap()
            .values,
        Some(expected)
    );

    let own_key = FactKey::new("commerce.OwnAuthzOrder", "OWN-1");
    let own_state = cell.load(&own_key).unwrap();
    cell.commit(CellCommit {
        writes: vec![FactWrite {
            key: own_key,
            expected: own_state.version,
            mutation: FactMutation::Delete,
        }],
        ..CellCommit::empty()
    })
    .unwrap();
    assert_refused(
        &mut cell,
        &catalog,
        &own,
        &own_authorizer,
        "action.target_not_found",
    );
    let replay_checks = own_authorizer.observed();
    assert_eq!(replay_checks.len(), 2);
    assert_eq!(replay_checks[1], None);

    let mut other = call(&action, "OTHER-1", "create-other");
    other.object_type = Some(ObjectTypeName("commerce.OtherAuthzOrder".to_owned()));
    let other_authorizer = PropertyAuthorizer::allowing("business_unit", &["BU-own"]);
    let ledger_before_denied_create = cell.ledger().len();
    let refusal = assert_refused(
        &mut cell,
        &catalog,
        &other,
        &other_authorizer,
        "action.permission_denied",
    );
    assert!(
        refusal
            .to_string()
            .contains("authz.business_unit_not_allowed")
    );
    let observed_other = other_authorizer.observed();
    let proposed_other = observed_other
        .first()
        .and_then(|values| values.as_ref())
        .expect("the create is authorized against its proposed values");
    assert_eq!(observed_other.len(), 1);
    assert_eq!(
        proposed_other.get("amount"),
        Some(&JsonValue::Number(norn_ir::Decimal::parse("1").unwrap()))
    );
    assert!(
        cell.load(&FactKey::new("commerce.OtherAuthzOrder", "OTHER-1"))
            .unwrap()
            .values
            .is_none()
    );
    assert_eq!(cell.ledger().len(), ledger_before_denied_create);

    let invisible = Deny(ActionDenial::Invisible);
    assert_refused(
        &mut cell,
        &catalog,
        &other,
        &invisible,
        "action.target_not_found",
    );
}

#[test]
fn action_authorizes_old_and_proposed_values_for_updates_and_transitions() {
    let types = business_unit_types();
    let catalog = TypeCatalog::new(&types);
    let mut cell = Cell::new(|command: &Command<JsonObject>| command.payload.clone());
    let action = business_unit_upsert_action("commerce.authz_order.move");
    let first = seed_business_unit_order(&mut cell, "MOVE-1", "BU-A");
    let second = seed_business_unit_order(&mut cell, "MOVE-2", "BU-B");
    let authorizer = PropertyAuthorizer::allowing("business_unit", &["BU-A", "BU-B"]);

    let mut to_b = call(&action, "MOVE-1", "move-to-b");
    to_b.input.insert(
        "business_unit".to_owned(),
        JsonValue::String("BU-B".to_owned()),
    );
    assert!(matches!(
        commit_action(&mut cell, &catalog, &to_b, &authorizer).unwrap(),
        ActionOutcome::Committed { .. }
    ));

    let mut to_a = call(&action, "MOVE-2", "move-to-a");
    to_a.input.insert(
        "business_unit".to_owned(),
        JsonValue::String("BU-A".to_owned()),
    );
    assert!(matches!(
        commit_action(&mut cell, &catalog, &to_a, &authorizer).unwrap(),
        ActionOutcome::Committed { .. }
    ));
    let observed = authorizer.observed();
    let business_units = observed
        .iter()
        .map(|values| {
            values
                .as_ref()
                .and_then(|values| values.get("business_unit"))
                .and_then(JsonValue::as_str)
        })
        .collect::<Vec<_>>();
    assert_eq!(
        business_units,
        [Some("BU-A"), Some("BU-B"), Some("BU-B"), Some("BU-A")]
    );
    assert_eq!(
        cell.load(&first)
            .unwrap()
            .values
            .unwrap()
            .get("business_unit"),
        Some(&JsonValue::String("BU-B".to_owned()))
    );
    assert_eq!(
        cell.load(&second)
            .unwrap()
            .values
            .unwrap()
            .get("business_unit"),
        Some(&JsonValue::String("BU-A".to_owned()))
    );

    let blocked = seed_business_unit_order(&mut cell, "MOVE-DENIED", "BU-A");
    let old_only_authorizer = PropertyAuthorizer::allowing("business_unit", &["BU-A"]);
    let mut out_of_scope = call(&action, "MOVE-DENIED", "move-out-of-scope");
    out_of_scope.input.insert(
        "business_unit".to_owned(),
        JsonValue::String("BU-B".to_owned()),
    );
    assert_refused(
        &mut cell,
        &catalog,
        &out_of_scope,
        &old_only_authorizer,
        "action.permission_denied",
    );
    let refused_values = old_only_authorizer.observed();
    let refused_business_units = refused_values
        .iter()
        .map(|values| {
            values
                .as_ref()
                .and_then(|values| values.get("business_unit"))
                .and_then(JsonValue::as_str)
        })
        .collect::<Vec<_>>();
    assert_eq!(refused_business_units, [Some("BU-A"), Some("BU-B")]);
    assert_eq!(
        cell.load(&blocked)
            .unwrap()
            .values
            .unwrap()
            .get("business_unit"),
        Some(&JsonValue::String("BU-A".to_owned()))
    );

    let order_types = order_types();
    let order_catalog = TypeCatalog::new(&order_types);
    let mut transition_cell = Cell::new(|command: &Command<JsonObject>| command.payload.clone());
    seed(&mut transition_cell, ORDER, "SO-AUTHZ", "draft");
    let transition_authorizer = PropertyAuthorizer::allowing("status", &["draft", "approved"]);
    assert!(matches!(
        commit_action(
            &mut transition_cell,
            &order_catalog,
            &call(&approve(), "SO-AUTHZ", "approve-authz"),
            &transition_authorizer
        )
        .unwrap(),
        ActionOutcome::Committed { .. }
    ));
    let observed = transition_authorizer.observed();
    let observed_statuses = observed
        .iter()
        .map(|values| {
            values
                .as_ref()
                .and_then(|values| values.get("status"))
                .and_then(JsonValue::as_str)
        })
        .collect::<Vec<_>>();
    assert_eq!(observed_statuses, [Some("draft"), Some("approved")]);
}

#[test]
fn upsert_rejects_undeclared_or_malformed_input_before_any_effect() {
    let types = order_types();
    let catalog = TypeCatalog::new(&types);
    let mut cell = Cell::new(|command: &Command<JsonObject>| command.payload.clone());
    let upsert = upsert_action("commerce.sales_order.edit", &json!({"kind": "upsert"}));
    let mut action_call = call(&upsert, "SO-UNKNOWN-FIELD", "unknown-upsert-field");
    action_call.input.insert(
        "status".to_owned(),
        JsonValue::String("submitted".to_owned()),
    );

    let ledger_len = cell.ledger().len();
    let refusal = commit_action(&mut cell, &catalog, &action_call, &Allow).unwrap_err();
    assert_eq!(refusal.code(), "action.invalid_input");
    assert_eq!(cell.ledger().len(), ledger_len);
    assert!(
        !cell
            .load(&FactKey::new(ORDER, "SO-UNKNOWN-FIELD"))
            .unwrap()
            .exists()
    );

    let mut wrong_type = call(&upsert, "SO-WRONG-TYPE", "wrong-upsert-type");
    wrong_type
        .input
        .insert("so_number".to_owned(), JsonValue::Bool(true));
    let refusal = commit_action(&mut cell, &catalog, &wrong_type, &Allow).unwrap_err();
    assert_eq!(refusal.code(), "action.invalid_input");
    assert_eq!(cell.ledger().len(), ledger_len);
}

#[test]
fn action_enqueues_evaluated_command_in_the_transition_commit_and_replay_adds_nothing() {
    let types = order_types();
    let catalog = TypeCatalog::new(&types);
    let mut cell = Cell::new(|command: &Command<JsonObject>| command.payload.clone());
    seed(&mut cell, ORDER, "SO-1", "draft");
    let approve = approve_with_submit_command("input.so_number");
    let action_call = call(&approve, "SO-1", "submit-so-1");

    let committed = commit_action(&mut cell, &catalog, &action_call, &Allow).unwrap();
    let ActionOutcome::Committed { receipt, .. } = committed else {
        panic!("expected a commit: {committed:?}");
    };
    let entry = cell.ledger().last().unwrap();
    assert_eq!(entry.seq, receipt.seq);
    assert!(matches!(
        entry.events.as_slice(),
        [
            FactEvent::Transitioned { .. },
            FactEvent::Modified { .. },
            FactEvent::CommandEnqueued {
                generation: 0,
                action_call,
                ..
            }
        ] if action_call == "commerce.sales_order.approve/submit-so-1"
    ));
    assert_eq!(entry.written.len(), 2);
    assert_eq!(cell.outbox().len(), 1);
    let command = cell.outbox().first().unwrap();
    assert_eq!(command.id.0, "turn-submit-so-1/command/0");
    assert_eq!(command.idempotency_key.0, command.id.0);
    assert_eq!(
        command.payload.get("type"),
        Some(&JsonValue::String("norn.connector.command/v1".to_owned()))
    );
    assert_eq!(
        command.payload.get("name"),
        Some(&JsonValue::String("SO-1".to_owned()))
    );
    assert_eq!(
        command.payload.get("actionCall"),
        Some(&JsonValue::String(
            "commerce.sales_order.approve/submit-so-1".to_owned()
        ))
    );
    assert!(!command.payload.contains_key("expectedModified"));
    assert!(!command.payload.contains_key("childMerges"));

    let ledger_len = cell.ledger().len();
    assert!(matches!(
        commit_action(&mut cell, &catalog, &action_call, &Allow).unwrap(),
        ActionOutcome::Replayed { .. }
    ));
    assert_eq!(cell.ledger().len(), ledger_len);
    assert_eq!(cell.outbox().len(), 1);
}

#[test]
fn action_emits_evaluated_source_version_and_child_merge_wire() {
    let types = order_types();
    let catalog = TypeCatalog::new(&types);
    let mut cell = Cell::new(|command: &Command<JsonObject>| command.payload.clone());
    seed(&mut cell, ORDER, "SO-1", "draft");
    let action = approve_with_guarded_update_command("input.modified", "input.owner", "input.rows");
    let call = guarded_update_call(&action, "guarded-source-update");

    commit_action(&mut cell, &catalog, &call, &Allow).unwrap();

    let command = &cell.outbox()[0];
    assert_eq!(
        command.payload.get("expectedModified"),
        Some(&JsonValue::String("2026-10-09 13:00:00".to_owned()))
    );
    let child_merges = command
        .payload
        .get("childMerges")
        .and_then(JsonValue::as_array)
        .unwrap();
    assert_eq!(child_merges.len(), 1);
    assert_eq!(
        child_merges[0].get("field").and_then(JsonValue::as_str),
        Some("items")
    );
    assert_eq!(
        child_merges[0]
            .get("ownershipField")
            .and_then(JsonValue::as_str),
        Some("custom_norn_owner")
    );
    assert_eq!(
        child_merges[0]
            .get("ownershipValue")
            .and_then(JsonValue::as_str),
        Some("norn:rule/hold")
    );
    assert_eq!(
        child_merges[0]
            .get("rowKeyField")
            .and_then(JsonValue::as_str),
        Some("item_code")
    );
    assert_eq!(
        child_merges[0].get("rows").and_then(JsonValue::as_array),
        Some(&vec![
            serde_json::from_value(json!({ "item_code": "ITEM-1", "qty": 3 })).unwrap(),
            serde_json::from_value(json!({ "item_code": "ITEM-2", "qty": 5 })).unwrap(),
        ])
    );
    assert_eq!(
        command
            .payload
            .get("fields")
            .and_then(|fields| fields.get("description")),
        Some(&JsonValue::String("norn:rule/hold".to_owned()))
    );
}

#[test]
fn action_refuses_invalid_source_version_and_child_merge_expression_shapes_without_effects() {
    let cases = [
        ("123", "input.owner", "input.rows"),
        ("\"  \"", "input.owner", "input.rows"),
        ("input.modified", "123", "input.rows"),
        ("input.modified", "\"  \"", "input.rows"),
        ("input.modified", "input.owner", "1"),
        ("input.modified", "input.owner", "[1]"),
    ];
    for (index, (expected_modified, owner, rows)) in cases.into_iter().enumerate() {
        let types = order_types();
        let catalog = TypeCatalog::new(&types);
        let mut cell = Cell::new(|command: &Command<JsonObject>| command.payload.clone());
        seed(&mut cell, ORDER, "SO-1", "draft");
        let action = approve_with_guarded_update_command(expected_modified, owner, rows);
        let key = format!("invalid-guarded-command-{index}");
        let call = guarded_update_call(&action, &key);
        let ledger_len = cell.ledger().len();

        let refusal = commit_action(&mut cell, &catalog, &call, &Allow).unwrap_err();
        assert_eq!(refusal.code(), "decision.invalid_output");
        assert_eq!(cell.ledger().len(), ledger_len);
        assert!(cell.outbox().is_empty());
        assert_eq!(
            status(&cell, &FactKey::new(ORDER, "SO-1")),
            Some(JsonValue::String("draft".to_owned()))
        );
    }
}

#[test]
fn action_command_expressions_share_one_turn_fuel_budget() {
    let text_bytes = 30_000;

    let types = order_types();
    let catalog = TypeCatalog::new(&types);
    let single_action = approve_with_repeated_command_action(1);
    let mut single_cell = Cell::new(|command: &Command<JsonObject>| command.payload.clone());
    seed(&mut single_cell, ORDER, "SO-1", "draft");
    let single_call = repeated_command_call(&single_action, "single-command-fuel", text_bytes);
    commit_action(&mut single_cell, &catalog, &single_call, &Allow)
        .expect("one data-proportional expression fits the action turn budget");

    // Keep 31 commands so the action's transition plus outbound effects fit
    // the 32-event envelope while repeating the data-proportional expression.
    let action = approve_with_repeated_command_action(31);
    let mut cell = Cell::new(|command: &Command<JsonObject>| command.payload.clone());
    seed(&mut cell, ORDER, "SO-1", "draft");
    let call = repeated_command_call(&action, "shared-command-fuel", text_bytes);
    let ledger_len = cell.ledger().len();

    let refusal = commit_action(&mut cell, &catalog, &call, &Allow).unwrap_err();
    assert_eq!(refusal.code(), "decision.invalid_output");
    assert!(
        refusal
            .to_string()
            .contains("expression evaluation budget exhausted"),
        "expected a shared-fuel refusal, got {refusal}"
    );
    assert_eq!(cell.ledger().len(), ledger_len);
    assert!(cell.outbox().is_empty());
    assert_eq!(
        status(&cell, &FactKey::new(ORDER, "SO-1")),
        Some(JsonValue::String("draft".to_owned()))
    );
}

#[test]
fn action_command_expression_failure_commits_nothing() {
    let types = order_types();
    let catalog = TypeCatalog::new(&types);
    let mut cell = Cell::new(|command: &Command<JsonObject>| command.payload.clone());
    seed(&mut cell, ORDER, "SO-1", "draft");
    let approve = approve_with_submit_command("input.missing");
    let action_call = call(&approve, "SO-1", "invalid-command-expression");
    let ledger_len = cell.ledger().len();

    let refusal = commit_action(&mut cell, &catalog, &action_call, &Allow).unwrap_err();
    assert_eq!(refusal.code(), "decision.invalid_output");
    assert_eq!(cell.ledger().len(), ledger_len);
    assert!(cell.outbox().is_empty());
    assert_eq!(
        status(&cell, &FactKey::new(ORDER, "SO-1")),
        Some(JsonValue::String("draft".into()))
    );
}

#[test]
fn action_command_envelope_counts_the_ledger_enqueue_event() {
    let types = order_types();
    let catalog = TypeCatalog::new(&types);
    let mut cell = Cell::new(|command: &Command<JsonObject>| command.payload.clone());
    seed(&mut cell, ORDER, "SO-1", "draft");
    let approve = approve_with_submit_command("input.so_number");
    let action_call = call(&approve, "SO-1", "command-event-limit");
    let ledger_len = cell.ledger().len();
    let limits = norn_kernel::EnvelopeLimits {
        max_events: 1,
        max_commands: 1,
        max_timers: 0,
        ..norn_kernel::EnvelopeLimits::V1
    };

    let refusal =
        commit_action_with_limits(&mut cell, &catalog, &action_call, &Allow, &limits).unwrap_err();
    assert_eq!(refusal.code(), "decision.envelope_exceeded");
    assert_eq!(cell.ledger().len(), ledger_len);
    assert!(cell.outbox().is_empty());
}

#[test]
fn action_commit_applies_explicit_per_decision_limits() {
    let types = order_types();
    let catalog = TypeCatalog::new(&types);
    let mut cell = Cell::new(|command: &Command<JsonObject>| command.payload.clone());
    seed(&mut cell, ORDER, "SO-1", "draft");
    let approve = approve();
    let call = call(&approve, "SO-1", "limits");
    let ledger_len = cell.ledger().len();

    let no_events = norn_kernel::EnvelopeLimits {
        max_events: 0,
        ..norn_kernel::EnvelopeLimits::V1
    };
    let refusal =
        commit_action_with_limits(&mut cell, &catalog, &call, &Allow, &no_events).unwrap_err();
    assert_eq!(refusal.code(), "decision.envelope_exceeded");
    assert_eq!(refusal.category(), norn_kernel::RefusalCategory::Limit);
    assert_eq!(cell.ledger().len(), ledger_len);
    assert_eq!(
        status(&cell, &FactKey::new(ORDER, "SO-1")),
        Some(JsonValue::String("draft".into()))
    );

    let one_event = norn_kernel::EnvelopeLimits {
        max_events: 1,
        max_commands: 0,
        max_timers: 0,
        ..norn_kernel::EnvelopeLimits::V1
    };
    let refusal =
        commit_action_with_limits(&mut cell, &catalog, &call, &Allow, &one_event).unwrap_err();
    assert_eq!(refusal.code(), "decision.envelope_exceeded");
    assert_eq!(cell.ledger().len(), ledger_len);
    assert_eq!(
        status(&cell, &FactKey::new(ORDER, "SO-1")),
        Some(JsonValue::String("draft".into()))
    );

    let two_events = norn_kernel::EnvelopeLimits {
        max_events: 2,
        ..one_event
    };
    assert!(matches!(
        commit_action_with_limits(&mut cell, &catalog, &call, &Allow, &two_events).unwrap(),
        ActionOutcome::Committed { .. }
    ));
    assert_eq!(cell.ledger().len(), ledger_len + 1);
    assert_eq!(cell.ledger().last().unwrap().events.len(), 2);
}

#[test]
fn decision_and_action_commits_share_the_aggregate_turn_meter() {
    let types = order_types();
    let catalog = TypeCatalog::new(&types);
    let mut cell = Cell::new(|command: &Command<JsonObject>| command.payload.clone());
    seed(&mut cell, ORDER, "SO-1", "draft");
    let mut meter = TurnEnvelopeMeter::new(TurnEnvelopeLimits {
        max_events: 1,
        max_commands: 1,
        max_timers: 1,
    });
    let output: norn_ir::DecisionOutput = serde_json::from_value(json!({
        "facts": [{
            "op": "create",
            "objectType": ORDER,
            "key": "SO-meter-create",
            "values": { "so_number": "SO-meter-create", "status": "draft" }
        }],
        "commands": [],
        "timers": []
    }))
    .unwrap();
    let turn = DecisionTurn {
        id: "decision-before-action".to_owned(),
        now: LogicalTime::from_millis(1_000),
        world: World::Main,
        consumes: None,
    };
    commit_decision_with_meter(
        &mut cell,
        &catalog,
        &FactSlice::new(),
        &turn,
        &output,
        &mut meter,
    )
    .unwrap();
    assert_eq!(meter.used().events, 1);

    let approve = approve();
    let action_call = call(&approve, "SO-1", "after-limit");
    let ledger_len = cell.ledger().len();
    let refusal = commit_action_with_meter(&mut cell, &catalog, &action_call, &Allow, &mut meter)
        .unwrap_err();
    assert_eq!(refusal.code(), "decision.turn_envelope_exceeded");
    assert_eq!(refusal.category(), norn_kernel::RefusalCategory::Limit);
    assert_eq!(cell.ledger().len(), ledger_len);
    assert_eq!(meter.used().events, 1);
    assert_eq!(
        status(&cell, &FactKey::new(ORDER, "SO-1")),
        Some(JsonValue::String("draft".into()))
    );
}

#[test]
fn action_meter_does_not_charge_an_exact_retry() {
    let types = order_types();
    let catalog = TypeCatalog::new(&types);
    let mut cell = Cell::new(|command: &Command<JsonObject>| command.payload.clone());
    seed(&mut cell, ORDER, "SO-1", "draft");
    let approve = approve();
    let call = call(&approve, "SO-1", "meter-replay");
    let mut meter = TurnEnvelopeMeter::new(TurnEnvelopeLimits {
        max_events: 2,
        max_commands: 1,
        max_timers: 1,
    });

    assert!(matches!(
        commit_action_with_meter(&mut cell, &catalog, &call, &Allow, &mut meter).unwrap(),
        ActionOutcome::Committed { .. }
    ));
    assert_eq!(meter.used().events, 2);
    assert!(matches!(
        commit_action_with_meter(&mut cell, &catalog, &call, &Allow, &mut meter).unwrap(),
        ActionOutcome::Replayed { .. }
    ));
    assert_eq!(meter.used().events, 2);
}

/// A host on which another commit of the same call lands first.
struct Racing {
    cell: Cell,
    raced: bool,
    revoke_authorization_after_race: Option<Arc<AtomicBool>>,
}

impl CellHost for Racing {
    type Event = FactEvent;
    type Command = JsonObject;
    type Input = JsonObject;
    type Error = Infallible;

    fn load(&self, key: &FactKey) -> Result<FactState, Infallible> {
        self.cell.load(key)
    }

    fn load_link(&self, key: &norn_kernel::LinkKey) -> Result<norn_kernel::LinkState, Infallible> {
        self.cell.load_link(key)
    }

    #[expect(
        clippy::unwrap_in_result,
        reason = "This test fixture must panic on failed prerequisites rather than simulate a recoverable domain refusal"
    )]
    fn commit(
        &mut self,
        commit: CellCommit<FactEvent, JsonObject, JsonObject>,
    ) -> Result<CommitReceipt, CommitError<Infallible>> {
        if !self.raced {
            self.raced = true;
            self.cell.commit(commit.clone()).unwrap();
            if let Some(authorized) = &self.revoke_authorization_after_race {
                authorized.store(false, Ordering::SeqCst);
            }
        }
        self.cell.commit(commit)
    }

    fn commit_owned_rows(
        &mut self,
        commit: CellCommit<FactEvent, JsonObject, JsonObject>,
        proof: OwnedRowsCommitProof,
    ) -> Result<CommitReceipt, CommitError<Infallible>> {
        let Some(parent_write) = commit.writes.first() else {
            panic!("owned-row action commits have a parent write");
        };
        let key = parent_write.key.clone();
        let expected = parent_write.expected;
        let simulate_race = !self.raced;
        if simulate_race {
            self.raced = true;
        }
        let receipt = self.cell.commit_owned_rows(commit, proof)?;
        if simulate_race {
            if let Some(authorized) = &self.revoke_authorization_after_race {
                authorized.store(false, Ordering::SeqCst);
            }
            return Err(CommitError::Conflict {
                key,
                expected,
                current: FactVersion(expected.0.saturating_add(1)),
            });
        }
        Ok(receipt)
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

/// A cell wrapper that counts attempted commits, including refusals.
struct CommitCounting {
    cell: Cell,
    commit_calls: usize,
}

impl CellHost for CommitCounting {
    type Event = FactEvent;
    type Command = JsonObject;
    type Input = JsonObject;
    type Error = Infallible;

    fn load(&self, key: &FactKey) -> Result<FactState, Infallible> {
        self.cell.load(key)
    }

    fn load_link(&self, key: &LinkKey) -> Result<LinkState, Infallible> {
        self.cell.load_link(key)
    }

    fn commit(
        &mut self,
        commit: CellCommit<FactEvent, JsonObject, JsonObject>,
    ) -> Result<CommitReceipt, CommitError<Infallible>> {
        self.commit_calls = self.commit_calls.saturating_add(1);
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

#[test]
fn action_race_on_the_same_key_replays() {
    let types = order_types();
    let catalog = TypeCatalog::new(&types);
    let mut host = Racing {
        cell: Cell::new(|command: &Command<JsonObject>| command.payload.clone()),
        raced: true,
        revoke_authorization_after_race: None,
    };
    let order = seed(&mut host.cell, ORDER, "SO-1", "draft");
    host.raced = false;
    let approve = approve();
    let outcome =
        commit_action(&mut host, &catalog, &call(&approve, "SO-1", "k1"), &Allow).unwrap();
    assert!(
        matches!(outcome, ActionOutcome::Replayed { .. }),
        "{outcome:?}"
    );
    assert_eq!(
        status(&host.cell, &order),
        Some(JsonValue::String("approved".into()))
    );
    // The racing commit landed once; this call added nothing.
    assert_eq!(host.cell.ledger().len(), 2);
}

#[test]
fn action_race_on_a_consumed_delivery_replays() {
    let types = order_types();
    let catalog = TypeCatalog::new(&types);
    let mut host = Racing {
        cell: Cell::new(|command: &Command<JsonObject>| command.payload.clone()),
        raced: true,
        revoke_authorization_after_race: None,
    };
    seed(&mut host.cell, ORDER, "SO-1", "draft");
    // A due timer whose delivery the call consumes.
    host.cell
        .commit(CellCommit {
            effects: vec![norn_kernel::Effect::Schedule(norn_kernel::Timer {
                id: norn_kernel::TimerId("approve-later".to_owned()),
                due: LogicalTime::from_millis(0),
                payload: JsonObject::new(),
            })],
            ..CellCommit::empty()
        })
        .unwrap();
    let due = host.cell.clock().unwrap().unwrap();
    host.raced = false;
    let approve = approve();
    let mut fired = call(&approve, "SO-1", "k1");
    fired.turn.consumes = Some(due.consumed());
    // The racing commit consumes the delivery first; this call then sees
    // the delivery as no longer pending and replays the committed call.
    let outcome = commit_action(&mut host, &catalog, &fired, &Allow).unwrap();
    assert!(
        matches!(outcome, ActionOutcome::Replayed { .. }),
        "{outcome:?}"
    );
    assert_eq!(host.cell.ledger().len(), 3);
    assert_eq!(host.cell.clock().unwrap(), None);
}

#[test]
fn naming_series_does_not_retry_a_delivery_consumed_by_another_turn() {
    let types = naming_series_types("gapless_within_commit", "year");
    let catalog = TypeCatalog::new(&types);
    let mut cell = Cell::new(|command: &Command<JsonObject>| command.payload.clone());
    cell.commit(CellCommit {
        effects: vec![norn_kernel::Effect::Schedule(norn_kernel::Timer {
            id: norn_kernel::TimerId("consumed-elsewhere".to_owned()),
            due: LogicalTime::from_millis(0),
            payload: JsonObject::new(),
        })],
        ..CellCommit::empty()
    })
    .unwrap();
    let due = cell.clock().unwrap().unwrap();
    let consumed = due.consumed();
    cell.commit(CellCommit {
        consumes: Some(consumed.clone()),
        ..CellCommit::empty()
    })
    .unwrap();

    let ledger_before = cell.ledger().len();
    let mut host = CommitCounting {
        cell,
        commit_calls: 0,
    };
    let action = naming_series_action("commerce.named_order.create");
    let mut call = naming_call(&action, "ACME", "consumed-elsewhere", 1_767_225_600_000);
    call.turn.consumes = Some(consumed);

    let refusal = commit_action(&mut host, &catalog, &call, &Allow).unwrap_err();
    assert_eq!(refusal.code(), "decision.not_pending");
    assert_eq!(
        host.commit_calls, 1,
        "the consumed delivery is checked once"
    );
    assert_eq!(host.cell.ledger().len(), ledger_before);
    assert!(
        host.cell
            .load(&FactKey::new("commerce.NamedOrder", "SO-ACME-2025-00001"))
            .unwrap()
            .values
            .is_none()
    );
}

#[test]
fn action_existence_never_leaks() {
    let types = order_types();
    let catalog = TypeCatalog::new(&types);
    let mut cell = Cell::new(|command: &Command<JsonObject>| command.payload.clone());
    seed(&mut cell, ORDER, "SO-1", "draft");
    let approve = approve();
    let forbidden = Deny(ActionDenial::Forbidden {
        reason: "cedar_deny".to_owned(),
    });
    let invisible = Deny(ActionDenial::Invisible);
    // Visible but forbidden: permission denied.
    let refusal = assert_refused(
        &mut cell,
        &catalog,
        &call(&approve, "SO-1", "k1"),
        &forbidden,
        "action.permission_denied",
    );
    assert!(refusal.to_string().contains("cedar_deny"));
    // Invisible, absent-and-allowed, and absent-and-forbidden all read the
    // same: not found.
    let cases: [(&str, &dyn ActionAuthorizer); 4] = [
        ("SO-1", &invisible),
        ("SO-404", &Allow),
        ("SO-404", &forbidden),
        ("SO-404", &invisible),
    ];
    for (so_number, authorizer) in cases {
        assert_refused(
            &mut cell,
            &catalog,
            &call(&approve, so_number, "k1"),
            authorizer,
            "action.target_not_found",
        );
    }
}

#[test]
fn action_shape_scope_input_preconditions_and_kind() {
    let types = order_types();
    let catalog = TypeCatalog::new(&types);
    let mut cell = Cell::new(|command: &Command<JsonObject>| command.payload.clone());
    seed(&mut cell, ORDER, "SO-1", "draft");
    seed(&mut cell, "commerce.Quote", "Q-1", "draft");
    let approve = approve();

    let mut no_org = call(&approve, "SO-1", "k1");
    no_org.scope.remove("orgId");
    assert_refused(&mut cell, &catalog, &no_org, &Allow, "action.missing_scope");
    let mut no_env = call(&approve, "SO-1", "k1");
    no_env
        .scope
        .insert("environmentId".to_owned(), " ".to_owned());
    assert_refused(&mut cell, &catalog, &no_env, &Allow, "action.missing_scope");
    assert_refused(
        &mut cell,
        &catalog,
        &call(&approve, "SO-1", " "),
        &Allow,
        "action.blank_field",
    );
    let mut blank_artifact = call(&approve, "SO-1", "k-artifact");
    blank_artifact.action_artifact_id = " ";
    assert_refused(
        &mut cell,
        &catalog,
        &blank_artifact,
        &Allow,
        "action.blank_field",
    );
    let mut blank_version = call(&approve, "SO-1", "k-version");
    blank_version.action_version = "\t";
    assert_refused(
        &mut cell,
        &catalog,
        &blank_version,
        &Allow,
        "action.blank_field",
    );

    // `len(so_number) > 0` is the fixture's precondition, but a blank id is
    // refused first as invalid input.
    assert_refused(
        &mut cell,
        &catalog,
        &call(&approve, "", "k1"),
        &Allow,
        "action.invalid_input",
    );
    let mut guarded = approve.clone();
    guarded.preconditions = vec![norn_ir::ZenExpression("so_number != \"SO-1\"".to_owned())];
    assert_refused(
        &mut cell,
        &catalog,
        &call(&guarded, "SO-1", "k1"),
        &Allow,
        "action.precondition_failed",
    );

    let mut no_effect = approve.clone();
    no_effect.effect_program = None;
    assert_refused(
        &mut cell,
        &catalog,
        &call(&no_effect, "SO-1", "k1"),
        &Allow,
        "action.unsupported_effect",
    );

    // A target of an unrelated type is not the effect's kind.
    let mut quote = call(&approve, "Q-1", "k1");
    quote.object_type = Some(ObjectTypeName("commerce.Quote".to_owned()));
    assert_refused(
        &mut cell,
        &catalog,
        &quote,
        &Allow,
        "artifact_set.action.effect_kind_mismatch",
    );
}

#[test]
fn action_retry_binding_and_current_authorization_matrix() {
    let types = order_types();
    let catalog = TypeCatalog::new(&types);
    let mut cell = Cell::new(|command: &Command<JsonObject>| command.payload.clone());
    seed(&mut cell, ORDER, "SO-1", "draft");
    seed(&mut cell, ORDER, "SO-2", "draft");
    let mut approve = approve();
    // Admit an optional input so a changed input can retain the same target.
    if let Some(norn_ir::JsonValue::Object(properties)) = approve.input_schema.get_mut("properties")
    {
        properties.insert(
            "note".to_owned(),
            norn_ir::JsonValue::Object(norn_ir::JsonObject::from([(
                "type".to_owned(),
                norn_ir::JsonValue::String("string".to_owned()),
            )])),
        );
    }
    let original = call(&approve, "SO-1", "same-key");
    commit_action(&mut cell, &catalog, &original, &Allow).unwrap();

    let mut changed_input = original.clone();
    changed_input
        .input
        .insert("note".to_owned(), JsonValue::String("changed".to_owned()));
    let changed_target = call(&approve, "SO-2", "same-key");
    let mut changed_actor = original.clone();
    changed_actor.actor = "agent:other".to_owned();
    for changed in [changed_input, changed_target, changed_actor] {
        assert_refused(
            &mut cell,
            &catalog,
            &changed,
            &Allow,
            "action.idempotency_conflict",
        );
    }

    let forbidden = Deny(ActionDenial::Forbidden {
        reason: "authz.revoked".to_owned(),
    });
    let invisible = Deny(ActionDenial::Invisible);
    let cases: [(&dyn ActionAuthorizer, &str); 2] = [
        (&forbidden, "action.permission_denied"),
        (&invisible, "action.target_not_found"),
    ];
    for (authorizer, code) in cases {
        assert_refused(&mut cell, &catalog, &original, authorizer, code);
    }
    // Turn identity and time are delivery details, not call binding.
    let mut retry = original.clone();
    retry.turn.id = "later-turn".to_owned();
    retry.turn.now = LogicalTime::from_millis(2_000);
    let ledger = cell.ledger().len();
    assert!(matches!(
        commit_action(&mut cell, &catalog, &retry, &Allow).unwrap(),
        ActionOutcome::Replayed { .. }
    ));
    assert_eq!(cell.ledger().len(), ledger);
}

#[test]
fn action_preconditions_fail_closed_matrix() {
    let types = order_types();
    let catalog = TypeCatalog::new(&types);
    let mut cell = Cell::new(|command: &Command<JsonObject>| command.payload.clone());
    let target = seed(&mut cell, ORDER, "SO-1", "draft");
    for expression in ["false", "42", "missing_field > 0"] {
        let mut approve = approve();
        approve.preconditions = vec![norn_ir::ZenExpression(expression.to_owned())];
        assert_refused(
            &mut cell,
            &catalog,
            &call(&approve, "SO-1", "k1"),
            &Allow,
            "action.precondition_failed",
        );
        assert_eq!(
            status(&cell, &target),
            Some(JsonValue::String("draft".into()))
        );
    }
}

#[test]
fn action_subtype_uses_effective_lifecycle_guards() {
    let mut types = order_types();
    let subtype = "commerce.ReviewedOrder";
    types.push(
        serde_json::from_value(json!({
            "name": subtype,
            "extends": ORDER,
            "lifecycles": [{
                "property": "status",
                "initial": ["draft"],
                "terminal": ["invoiced", "cancelled"],
                "transitions": [
                    { "id": "approve", "from": ["draft"], "to": "approved",
                      "guards": ["self.so_number == \"SO-reviewed\""] },
                    { "id": "invoice", "from": ["approved"], "to": "invoiced" },
                    { "id": "cancel", "from": ["draft", "approved"], "to": "cancelled" }
                ]
            }]
        }))
        .unwrap(),
    );
    let catalog = TypeCatalog::new(&types);
    let mut cell = Cell::new(|command: &Command<JsonObject>| command.payload.clone());
    let blocked = seed(&mut cell, subtype, "SO-unreviewed", "draft");
    let accepted = seed(&mut cell, subtype, "SO-reviewed", "draft");
    let approve = approve();
    let mut blocked_call = call(&approve, "SO-unreviewed", "k1");
    blocked_call.object_type = Some(ObjectTypeName(subtype.to_owned()));
    assert_refused(
        &mut cell,
        &catalog,
        &blocked_call,
        &Allow,
        "decision.guard_false",
    );
    assert_eq!(
        status(&cell, &blocked),
        Some(JsonValue::String("draft".into()))
    );
    let mut accepted_call = call(&approve, "SO-reviewed", "k2");
    accepted_call.object_type = Some(ObjectTypeName(subtype.to_owned()));
    assert!(matches!(
        commit_action(&mut cell, &catalog, &accepted_call, &Allow).unwrap(),
        ActionOutcome::Committed { .. }
    ));
    assert_eq!(
        status(&cell, &accepted),
        Some(JsonValue::String("approved".into()))
    );
}

#[test]
fn action_race_rechecks_authorization_against_the_winning_state() {
    for consumes_delivery in [false, true] {
        let types = order_types();
        let catalog = TypeCatalog::new(&types);
        let authorized = Arc::new(AtomicBool::new(true));
        let mut host = Racing {
            cell: Cell::new(|command: &Command<JsonObject>| command.payload.clone()),
            raced: true,
            revoke_authorization_after_race: Some(Arc::clone(&authorized)),
        };
        let order = seed(&mut host.cell, ORDER, "SO-1", "draft");
        let approve = approve();
        let mut fired = call(&approve, "SO-1", "k1");
        if consumes_delivery {
            host.cell
                .commit(CellCommit {
                    effects: vec![norn_kernel::Effect::Schedule(norn_kernel::Timer {
                        id: norn_kernel::TimerId("approve-later".to_owned()),
                        due: LogicalTime::from_millis(0),
                        payload: JsonObject::new(),
                    })],
                    ..CellCommit::empty()
                })
                .unwrap();
            fired.turn.consumes = Some(host.cell.clock().unwrap().unwrap().consumed());
        }
        let ledger = host.cell.ledger().len();
        host.raced = false;
        let refusal =
            commit_action(&mut host, &catalog, &fired, &Revocable(authorized)).unwrap_err();
        assert_eq!(refusal.code(), "action.permission_denied");
        assert!(refusal.to_string().contains("authz.no_longer_authorized"));
        // Only the winner committed; the losing caller rechecked current
        // authorization before disclosing the replay.
        assert_eq!(host.cell.ledger().len(), ledger + 1);
        assert_eq!(
            status(&host.cell, &order),
            Some(JsonValue::String("approved".into()))
        );
    }
}

#[test]
fn legacy_action_record_replays_without_action_attribution() {
    let types = order_types();
    let catalog = TypeCatalog::new(&types);
    let mut cell = Cell::new(|command: &Command<JsonObject>| command.payload.clone());
    seed(&mut cell, ORDER, "SO-LEGACY", "draft");
    let approve = approve();
    let committed = commit_action(
        &mut cell,
        &catalog,
        &call(&approve, "SO-LEGACY", "k1"),
        &Allow,
    )
    .unwrap();
    assert!(matches!(committed, ActionOutcome::Committed { .. }));

    let record_key = FactKey::new(
        norn_kernel::ACTION_CALL_TYPE,
        "commerce.sales_order.approve/k1",
    );
    let state = cell.load(&record_key).unwrap();
    let mut values = state.values.unwrap();
    values.insert("actionArtifactId".to_owned(), JsonValue::Null);
    values.insert("actionVersion".to_owned(), JsonValue::Null);
    cell.commit(CellCommit {
        writes: vec![FactWrite {
            key: record_key,
            expected: state.version,
            mutation: FactMutation::Put { values },
        }],
        ..CellCommit::empty()
    })
    .unwrap();

    let mut upgraded_call = call(&approve, "SO-LEGACY", "k1");
    upgraded_call.action_artifact_id = "fixture.action.v2";
    upgraded_call.action_version = "0.2.0";
    let ledger = cell.ledger().len();
    let replayed = commit_action(&mut cell, &catalog, &upgraded_call, &Allow).unwrap();
    let ActionOutcome::Replayed { record } = replayed else {
        panic!("expected a replay: {replayed:?}");
    };
    assert_eq!(record.action_artifact_id, None);
    assert_eq!(record.action_version, None);
    assert_eq!(cell.ledger().len(), ledger);
}

fn naming_series_types(gap_policy: &str, reset_policy: &str) -> Vec<ObjectTypeDef> {
    vec![
        serde_json::from_value(json!({
            "name": "commerce.NamedOrder",
            "properties": [
                { "id": "name", "type": { "kind": "text" } },
                { "id": "company", "type": { "kind": "text" } }
            ],
            "identity": [{
                "name": "primary",
                "properties": ["name"],
                "namingSeries": {
                    "pattern": "SO-{company}-{YYYY}-{#####}",
                    "timezone": "America/New_York",
                    "scopeFields": ["company"],
                    "resetPolicy": reset_policy,
                    "gapPolicy": gap_policy
                }
            }]
        }))
        .unwrap(),
    ]
}

fn naming_series_action(action_id: &str) -> ActionRoot {
    let mut action = upsert_action_for(
        action_id,
        "commerce.NamedOrder",
        &json!({ "kind": "create_only" }),
    );
    action.effect_program = Some(norn_ir::ActionEffectProgram::UpsertInputObject {
        object_id_field: "name".to_owned(),
        object_kind: "commerce.NamedOrder".to_owned(),
        entity_artifact_id: "commerce.NamedOrder.entity".to_owned(),
        entity_artifact_version: "0.1.0".to_owned(),
        write_mode: norn_ir::UpsertWriteMode::CreateOnly,
    });
    action.input_schema = serde_json::from_value(json!({
        "type": "object",
        "properties": {
            "name": { "type": "string" },
            "company": { "type": "string" }
        },
        "required": ["company"],
        "additionalProperties": false
    }))
    .unwrap();
    action.preconditions = vec![norn_ir::ZenExpression("len(name) > 0".to_owned())];
    action.commands.clear();
    action
}

fn naming_call<'a>(
    action: &'a ActionRoot,
    company: &str,
    idempotency_key: &str,
    epoch_millis: u64,
) -> ActionCall<'a> {
    let mut call = call(action, "", idempotency_key);
    call.input = JsonObject::from([("company".to_owned(), JsonValue::String(company.to_owned()))]);
    call.turn.now = LogicalTime::from_millis(epoch_millis);
    call
}

const NUMBERED_BASE: &str = "commerce.NumberedBase";
const NUMBERED_CHILD: &str = "commerce.NumberedChild";
const NUMBERED_SIBLING: &str = "commerce.NumberedSibling";
const NUMBERED_ROOT: &str = "commerce.NumberedRoot";
const NUMBERED_OWNER: &str = "commerce.NumberedOwner";
const NUMBERED_OWNER_CHILD: &str = "commerce.NumberedOwnerChild";
const NUMBERED_OUTSIDE_SIBLING: &str = "commerce.NumberedOutsideSibling";

fn inherited_naming_series_types(gap_policy: &str) -> Vec<ObjectTypeDef> {
    vec![
        serde_json::from_value(json!({
            "name": NUMBERED_BASE,
            "properties": [
                { "id": "name", "type": { "kind": "text" } },
                { "id": "company", "type": { "kind": "text" } }
            ],
            "identity": [{
                "name": "primary",
                "properties": ["name"],
                "namingSeries": {
                    "pattern": "SO-{company}-{YYYY}-{#####}",
                    "timezone": "America/New_York",
                    "scopeFields": ["company"],
                    "resetPolicy": "year",
                    "gapPolicy": gap_policy
                }
            }]
        }))
        .unwrap(),
        serde_json::from_value(json!({
            "name": NUMBERED_CHILD,
            "extends": NUMBERED_BASE,
            "properties": []
        }))
        .unwrap(),
        serde_json::from_value(json!({
            "name": NUMBERED_SIBLING,
            "extends": NUMBERED_BASE,
            "properties": []
        }))
        .unwrap(),
    ]
}

fn intermediate_owner_naming_series_types(gap_policy: &str) -> Vec<ObjectTypeDef> {
    vec![
        serde_json::from_value(json!({
            "name": NUMBERED_ROOT,
            "properties": [
                { "id": "name", "type": { "kind": "text" } },
                { "id": "company", "type": { "kind": "text" } }
            ],
            "identity": []
        }))
        .unwrap(),
        serde_json::from_value(json!({
            "name": NUMBERED_OWNER,
            "extends": NUMBERED_ROOT,
            "properties": [],
            "identity": [{
                "name": "numbered",
                "properties": ["name"],
                "namingSeries": {
                    "pattern": "SO-{company}-{YYYY}-{#####}",
                    "timezone": "America/New_York",
                    "scopeFields": ["company"],
                    "resetPolicy": "year",
                    "gapPolicy": gap_policy
                }
            }]
        }))
        .unwrap(),
        serde_json::from_value(json!({
            "name": NUMBERED_OWNER_CHILD,
            "extends": NUMBERED_OWNER,
            "properties": []
        }))
        .unwrap(),
        serde_json::from_value(json!({
            "name": NUMBERED_OUTSIDE_SIBLING,
            "extends": NUMBERED_ROOT,
            "properties": []
        }))
        .unwrap(),
    ]
}

fn inherited_naming_series_action(action_id: &str, object_type: &str) -> ActionRoot {
    let mut action = upsert_action_for(action_id, object_type, &json!({ "kind": "create_only" }));
    action.effect_program = Some(norn_ir::ActionEffectProgram::UpsertInputObject {
        object_id_field: "name".to_owned(),
        object_kind: object_type.to_owned(),
        entity_artifact_id: format!("{object_type}.entity"),
        entity_artifact_version: "0.1.0".to_owned(),
        write_mode: norn_ir::UpsertWriteMode::CreateOnly,
    });
    action.input_schema = serde_json::from_value(json!({
        "type": "object",
        "properties": {
            "name": { "type": "string" },
            "company": { "type": "string" }
        },
        "required": ["company"],
        "additionalProperties": false
    }))
    .unwrap();
    action.preconditions = vec![norn_ir::ZenExpression("len(name) > 0".to_owned())];
    action.commands.clear();
    action
}

fn last_naming_counter_key(cell: &Cell) -> norn_kernel::FactKey {
    cell.ledger()
        .last()
        .unwrap()
        .events
        .iter()
        .find_map(|event| match event {
            FactEvent::NamingSeriesAllocated { counter, .. } => Some(counter.clone()),
            FactEvent::CommandEnqueued { .. }
            | FactEvent::NamingSeriesCounterSeeded { .. }
            | FactEvent::Created { .. }
            | FactEvent::Set { .. }
            | FactEvent::Modified { .. }
            | FactEvent::Deleted { .. }
            | FactEvent::Transitioned { .. }
            | FactEvent::InvariantRefused { .. } => None,
        })
        .unwrap()
}

fn outcome_key(outcome: ActionOutcome) -> String {
    match outcome {
        ActionOutcome::Committed { record, .. } | ActionOutcome::Replayed { record } => {
            record.object.key
        }
    }
}

#[test]
fn inherited_naming_series_shares_its_declaring_type_counter_for_both_gap_policies() {
    for gap_policy in ["gapless_within_commit", "allow_gaps"] {
        let types = inherited_naming_series_types(gap_policy);
        let catalog = TypeCatalog::new(&types);
        let mut cell = Cell::new(|command: &Command<JsonObject>| command.payload.clone())
            .with_types(types.clone());
        let mut counter_keys = BTreeSet::new();

        for (object_type, idempotency_key, expected_key) in [
            (NUMBERED_BASE, "owner-base-1", "SO-ACME-2025-00001"),
            (NUMBERED_CHILD, "owner-child-2", "SO-ACME-2025-00002"),
            (NUMBERED_BASE, "owner-base-3", "SO-ACME-2025-00003"),
        ] {
            let action_id = format!("commerce.numbered.create.{idempotency_key}");
            let action = inherited_naming_series_action(&action_id, object_type);
            let call = naming_call(&action, "ACME", idempotency_key, 1_767_225_600_000);
            let outcome = commit_action(&mut cell, &catalog, &call, &Allow).unwrap();
            let ActionOutcome::Committed { record, .. } = outcome else {
                panic!("the unique inherited-series create should commit");
            };
            assert_eq!(record.object.object_type.as_str(), object_type);
            assert_eq!(record.object.key, expected_key);
            counter_keys.insert(last_naming_counter_key(&cell));
        }

        assert_eq!(counter_keys.len(), 1, "one declaring type owns the bucket");
        let counter_key = counter_keys.into_iter().next().unwrap();
        let counter = cell.load(&counter_key).unwrap();
        let values = counter.values.unwrap();
        assert_eq!(
            values.get("objectType"),
            Some(&JsonValue::String(NUMBERED_BASE.to_owned()))
        );
        assert_eq!(
            values.get("current"),
            Some(&JsonValue::Number(norn_ir::Decimal::parse("3").unwrap()))
        );

        let child_seed = NamingCounterSeed {
            object_type: ObjectTypeName(NUMBERED_CHILD.to_owned()),
            key_name: norn_ir::KeyName("primary".to_owned()),
            scope: BTreeMap::from([(PropertyId("company".to_owned()), "ACME".to_owned())]),
            period: "2025".to_owned(),
            current: 4,
        };
        let ledger_before_seed = cell.ledger().len();
        let refusal = norn_kernel::naming_series::install_naming_counter_seeds(
            &mut cell,
            &catalog,
            &[child_seed],
        )
        .unwrap_err();
        assert!(refusal.to_string().contains("does not own"));
        assert_eq!(cell.ledger().len(), ledger_before_seed);
    }
}

#[test]
fn inherited_naming_series_collision_probe_covers_base_and_sibling_keys() {
    for gap_policy in ["gapless_within_commit", "allow_gaps"] {
        for occupied_type in [NUMBERED_BASE, NUMBERED_SIBLING] {
            let types = inherited_naming_series_types(gap_policy);
            let catalog = TypeCatalog::new(&types);
            let mut cell = Cell::new(|command: &Command<JsonObject>| command.payload.clone())
                .with_types(types.clone());
            let occupied_key = "SO-ACME-2025-00001";
            cell.commit(CellCommit {
                writes: vec![FactWrite {
                    key: FactKey::new(occupied_type, occupied_key),
                    expected: FactVersion::ABSENT,
                    mutation: FactMutation::Put {
                        values: JsonObject::from([
                            (
                                "name".to_owned(),
                                JsonValue::String(occupied_key.to_owned()),
                            ),
                            ("company".to_owned(), JsonValue::String("ACME".to_owned())),
                        ]),
                    },
                }],
                ..CellCommit::empty()
            })
            .unwrap();
            let ledger_before = cell.ledger().len();
            let action = inherited_naming_series_action(
                "commerce.numbered.create.cross_hierarchy_collision",
                NUMBERED_CHILD,
            );
            let call = naming_call(
                &action,
                "ACME",
                &format!("{gap_policy}-{occupied_type}"),
                1_767_225_600_000,
            );

            if gap_policy == "gapless_within_commit" {
                let refusal = commit_action(&mut cell, &catalog, &call, &Allow).unwrap_err();
                assert_eq!(refusal.code(), "decision.invalid_output");
                assert_eq!(cell.ledger().len(), ledger_before);
            } else {
                let outcome = commit_action(&mut cell, &catalog, &call, &Allow).unwrap();
                let ActionOutcome::Committed { record, .. } = outcome else {
                    panic!("a collision-skip create must commit once");
                };
                assert_eq!(record.object.object_type.as_str(), NUMBERED_CHILD);
                assert_eq!(record.object.key, "SO-ACME-2025-00002");
                assert_eq!(cell.ledger().len(), ledger_before + 1);
                let entry = cell.ledger().last().unwrap();
                let allocation = entry
                    .events
                    .iter()
                    .find(|event| matches!(event, FactEvent::NamingSeriesAllocated { .. }));
                assert!(matches!(
                    allocation,
                    Some(FactEvent::NamingSeriesAllocated { value: 2, .. })
                ));
                let counter = cell.load(&last_naming_counter_key(&cell)).unwrap();
                assert_eq!(
                    counter.values.unwrap().get("current"),
                    Some(&JsonValue::Number(norn_ir::Decimal::parse("2").unwrap()))
                );
                assert!(
                    cell.load(&FactKey::new(NUMBERED_CHILD, "SO-ACME-2025-00002"))
                        .unwrap()
                        .exists()
                );
            }
        }
    }
}

#[test]
fn intermediate_naming_series_owner_probes_the_hierarchy_root_for_sibling_keys() {
    for gap_policy in ["gapless_within_commit", "allow_gaps"] {
        for object_type in [NUMBERED_OWNER, NUMBERED_OWNER_CHILD] {
            let types = intermediate_owner_naming_series_types(gap_policy);
            let catalog = TypeCatalog::new(&types);
            let mut cell = Cell::new(|command: &Command<JsonObject>| command.payload.clone())
                .with_types(types.clone());
            let occupied_key = "SO-ACME-2025-00001";
            cell.commit(CellCommit {
                writes: vec![FactWrite {
                    key: FactKey::new(NUMBERED_OUTSIDE_SIBLING, occupied_key),
                    expected: FactVersion::ABSENT,
                    mutation: FactMutation::Put {
                        values: JsonObject::from([
                            (
                                "name".to_owned(),
                                JsonValue::String(occupied_key.to_owned()),
                            ),
                            ("company".to_owned(), JsonValue::String("ACME".to_owned())),
                        ]),
                    },
                }],
                ..CellCommit::empty()
            })
            .unwrap();
            let ledger_before = cell.ledger().len();
            let action = inherited_naming_series_action(
                &format!("commerce.numbered.create.{gap_policy}.{object_type}"),
                object_type,
            );
            let call = naming_call(
                &action,
                "ACME",
                &format!("nested-owner-{gap_policy}-{object_type}"),
                1_767_225_600_000,
            );

            if gap_policy == "gapless_within_commit" {
                let refusal = commit_action(&mut cell, &catalog, &call, &Allow).unwrap_err();
                assert_eq!(refusal.code(), "decision.invalid_output");
                assert_eq!(cell.ledger().len(), ledger_before);
            } else {
                let outcome = commit_action(&mut cell, &catalog, &call, &Allow).unwrap();
                let ActionOutcome::Committed { record, .. } = outcome else {
                    panic!("a collision-skip create must commit once");
                };
                assert_eq!(record.object.object_type.as_str(), object_type);
                assert_eq!(record.object.key, "SO-ACME-2025-00002");
                assert_eq!(cell.ledger().len(), ledger_before + 1);
                let entry = cell.ledger().last().unwrap();
                let allocation = entry
                    .events
                    .iter()
                    .find(|event| matches!(event, FactEvent::NamingSeriesAllocated { .. }));
                assert!(matches!(
                    allocation,
                    Some(FactEvent::NamingSeriesAllocated { value: 2, .. })
                ));
                let counter = cell.load(&last_naming_counter_key(&cell)).unwrap();
                assert_eq!(
                    counter.values.unwrap().get("current"),
                    Some(&JsonValue::Number(norn_ir::Decimal::parse("2").unwrap()))
                );
            }
            assert!(
                cell.load(&FactKey::new(NUMBERED_ROOT, occupied_key))
                    .unwrap()
                    .exists()
            );
        }
    }
}

struct BaseCollisionBeforeFirstCandidate {
    cell: Cell,
    injected: bool,
}

impl CellHost for BaseCollisionBeforeFirstCandidate {
    type Event = FactEvent;
    type Command = JsonObject;
    type Input = JsonObject;
    type Error = Infallible;

    fn load(&self, key: &FactKey) -> Result<FactState, Self::Error> {
        self.cell.load(key)
    }

    fn load_link(&self, key: &LinkKey) -> Result<LinkState, Self::Error> {
        self.cell.load_link(key)
    }

    fn commit(
        &mut self,
        commit: CellCommit<Self::Event, Self::Command, Self::Input>,
    ) -> Result<CommitReceipt, CommitError<Self::Error>> {
        let first_child_candidate = commit.writes.iter().any(|write| {
            write.key.object_type.as_str() == NUMBERED_CHILD
                && write.key.key == "SO-ACME-2025-00001"
        });
        if first_child_candidate && !self.injected {
            self.injected = true;
            let key = "SO-ACME-2025-00001";
            CellHost::commit(
                &mut self.cell,
                CellCommit {
                    writes: vec![FactWrite {
                        key: FactKey::new(NUMBERED_BASE, key),
                        expected: FactVersion::ABSENT,
                        mutation: FactMutation::Put {
                            values: JsonObject::from([
                                ("name".to_owned(), JsonValue::String(key.to_owned())),
                                ("company".to_owned(), JsonValue::String("ACME".to_owned())),
                            ]),
                        },
                    }],
                    ..CellCommit::empty()
                },
            )?;
        }
        CellHost::commit(&mut self.cell, commit)
    }

    fn perform(&mut self) -> Result<Option<Performed<JsonObject>>, Self::Error> {
        self.cell.perform()
    }

    fn observe(&mut self) -> Result<Option<Observed<JsonObject>>, Self::Error> {
        self.cell.observe()
    }

    fn clock(&mut self) -> Result<Option<DueTimer<JsonObject>>, Self::Error> {
        self.cell.clock()
    }

    fn publish(&mut self, receipt: &CommitReceipt) -> Result<(), Self::Error> {
        self.cell.publish(receipt)
    }
}

#[test]
fn inherited_naming_series_candidate_cas_catches_concurrent_base_create() {
    for gap_policy in ["gapless_within_commit", "allow_gaps"] {
        let types = inherited_naming_series_types(gap_policy);
        let catalog = TypeCatalog::new(&types);
        let mut host = BaseCollisionBeforeFirstCandidate {
            cell: Cell::new(|command: &Command<JsonObject>| command.payload.clone())
                .with_types(types.clone()),
            injected: false,
        };
        let action = inherited_naming_series_action(
            "commerce.numbered.create.collision_race",
            NUMBERED_CHILD,
        );
        let call = naming_call(
            &action,
            "ACME",
            &format!("collision-race-{gap_policy}"),
            1_767_225_600_000,
        );
        let result = commit_action(&mut host, &catalog, &call, &Allow);

        if gap_policy == "gapless_within_commit" {
            let refusal = result.unwrap_err();
            assert_eq!(refusal.code(), "decision.invalid_output");
            assert_eq!(host.cell.ledger().len(), 1);
        } else {
            let outcome = result.unwrap();
            assert_eq!(outcome_key(outcome), "SO-ACME-2025-00002");
            assert_eq!(host.cell.ledger().len(), 2);
            let counter = host
                .cell
                .load(&last_naming_counter_key(&host.cell))
                .unwrap();
            assert_eq!(
                counter.values.unwrap().get("current"),
                Some(&JsonValue::Number(norn_ir::Decimal::parse("2").unwrap()))
            );
        }
        let occupied = host
            .cell
            .load(&FactKey::new(NUMBERED_BASE, "SO-ACME-2025-00001"))
            .unwrap();
        assert!(occupied.exists());
        assert!(
            host.injected,
            "the test host inserted the competing base create"
        );
    }
}

#[test]
fn naming_series_allocates_in_the_create_commit_replays_and_resets_in_local_timezone() {
    let types = naming_series_types("gapless_within_commit", "year");
    let catalog = TypeCatalog::new(&types);
    let mut cell = Cell::new(|command: &Command<JsonObject>| command.payload.clone());
    let action = naming_series_action("commerce.named_order.create");

    // 2026-01-01 00:00 UTC is still 2025 in New York.
    let first_call = naming_call(&action, "ACME", "series-1", 1_767_225_600_000);
    let authorizer = InputMatchesTargetKey;
    let first = commit_action(&mut cell, &catalog, &first_call, &authorizer).unwrap();
    assert_eq!(outcome_key(first), "SO-ACME-2025-00001");
    let created = cell
        .load(&FactKey::new("commerce.NamedOrder", "SO-ACME-2025-00001"))
        .unwrap();
    assert_eq!(
        created
            .values
            .as_ref()
            .and_then(|values| values.get("name")),
        Some(&JsonValue::String("SO-ACME-2025-00001".to_owned()))
    );
    assert!(matches!(
        cell.ledger().last().unwrap().events.as_slice(),
        [FactEvent::Created { .. }, FactEvent::NamingSeriesAllocated {
            value: 1,
            period,
            epoch_millis: 1_767_225_600_000,
            tzdb_version,
            ..
        }] if period == "2025" && tzdb_version == norn_expr::temporal::NAMING_SERIES_TZDB_VERSION
    ));

    let ledger_after_first = cell.ledger().len();
    let replay_at_later_time = naming_call(&action, "ACME", "series-1", 1_767_243_600_000);
    let replayed = commit_action(&mut cell, &catalog, &replay_at_later_time, &authorizer).unwrap();
    assert_eq!(outcome_key(replayed), "SO-ACME-2025-00001");
    assert_eq!(cell.ledger().len(), ledger_after_first);

    let second_call = naming_call(&action, "ACME", "series-2", 1_767_225_600_000);
    assert_eq!(
        outcome_key(commit_action(&mut cell, &catalog, &second_call, &authorizer).unwrap()),
        "SO-ACME-2025-00002"
    );

    // 05:00 UTC is midnight in New York: only the period bucket resets.
    let next_period = naming_call(&action, "ACME", "series-3", 1_767_243_600_000);
    assert_eq!(
        outcome_key(commit_action(&mut cell, &catalog, &next_period, &authorizer).unwrap()),
        "SO-ACME-2026-00001"
    );
    assert!(matches!(
        cell.ledger().last().unwrap().events.last(),
        Some(FactEvent::NamingSeriesAllocated { period, value: 1, .. }) if period == "2026"
    ));

    let other_company = naming_call(&action, "OTHER", "series-4", 1_767_243_600_000);
    assert_eq!(
        outcome_key(commit_action(&mut cell, &catalog, &other_company, &authorizer).unwrap()),
        "SO-OTHER-2026-00001"
    );
}

#[test]
fn naming_series_generated_key_is_available_to_connector_command_expressions() {
    let types = naming_series_types("gapless_within_commit", "year");
    let catalog = TypeCatalog::new(&types);
    let mut cell = Cell::new(|command: &Command<JsonObject>| command.payload.clone());
    let mut action = naming_series_action("commerce.named_order.create_with_source");
    action.commands = serde_json::from_value(json!([{
        "connector": "erpnext",
        "operation": "update",
        "doctype": "Named Order",
        "name": "input.name",
        "fields": { "source_reference": "input.name" }
    }]))
    .unwrap();
    let call = naming_call(&action, "ACME", "series-command", 1_767_225_600_000);

    let generated_key = outcome_key(commit_action(&mut cell, &catalog, &call, &Allow).unwrap());

    assert_eq!(generated_key, "SO-ACME-2025-00001");
    let command = &cell.outbox()[0];
    assert_eq!(
        command.payload.get("name"),
        Some(&JsonValue::String(generated_key.clone()))
    );
    assert_eq!(
        command
            .payload
            .get("fields")
            .and_then(|fields| fields.get("source_reference")),
        Some(&JsonValue::String(generated_key))
    );
}

#[test]
fn naming_series_preconditions_and_connector_commands_share_one_fuel_budget() {
    // Each expression charges two string traversals, about 12.4K units: either
    // stage fits under 5M alone, but the combined turn exceeds that budget.
    const LARGE_INPUT_BYTES: usize = 6_200;
    const PRECONDITION_COUNT: usize = 240;
    const COMMAND_COUNT: usize = 30;

    let mut types = naming_series_types("gapless_within_commit", "year");
    types[0].properties.push(
        serde_json::from_value(json!({
            "id": "large",
            "type": { "kind": "text" }
        }))
        .unwrap(),
    );
    let catalog = TypeCatalog::new(&types);
    let mut action = naming_series_action("commerce.named_order.create_with_source");

    let mut schema = serde_json::to_value(&action.input_schema).unwrap();
    schema["properties"]["large"] = json!({ "type": "string" });
    schema["required"] = json!(["company", "large"]);
    action.input_schema = serde_json::from_value(schema).unwrap();

    let expensive = "len(lower(upper(input.large)))";
    action.preconditions =
        vec![norn_ir::ZenExpression(format!("{expensive} > 0")); PRECONDITION_COUNT];
    let commands = (0..COMMAND_COUNT)
        .map(|_| {
            json!({
                "connector": "erpnext",
                "operation": "update",
                "doctype": "Named Order",
                "name": "input.name",
                "fields": {
                    "probe_one": expensive,
                    "probe_two": expensive,
                    "probe_three": expensive,
                    "probe_four": expensive,
                    "probe_five": expensive,
                    "probe_six": expensive
                }
            })
        })
        .collect::<Vec<_>>();
    action.commands = serde_json::from_value(Value::Array(commands)).unwrap();

    let mut precondition_only_action = action.clone();
    precondition_only_action.commands.clear();
    let mut precondition_only_cell =
        Cell::new(|command: &Command<JsonObject>| command.payload.clone());
    let mut precondition_only_call = naming_call(
        &precondition_only_action,
        "ACME",
        "series-precondition-fuel",
        1_767_225_600_000,
    );
    precondition_only_call.input.insert(
        "large".to_owned(),
        JsonValue::String("x".repeat(LARGE_INPUT_BYTES)),
    );
    assert_eq!(
        outcome_key(
            commit_action(
                &mut precondition_only_cell,
                &catalog,
                &precondition_only_call,
                &Allow,
            )
            .expect("the precondition stage fits the turn fuel budget")
        ),
        "SO-ACME-2025-00001"
    );

    let mut command_only_action = action.clone();
    command_only_action.preconditions.clear();
    let mut command_only_cell = Cell::new(|command: &Command<JsonObject>| command.payload.clone());
    let mut command_only_call = naming_call(
        &command_only_action,
        "ACME",
        "series-command-fuel",
        1_767_225_600_000,
    );
    command_only_call.input.insert(
        "large".to_owned(),
        JsonValue::String("x".repeat(LARGE_INPUT_BYTES)),
    );
    assert_eq!(
        outcome_key(
            commit_action(&mut command_only_cell, &catalog, &command_only_call, &Allow,)
                .expect("the command stage fits the turn fuel budget")
        ),
        "SO-ACME-2025-00001"
    );
    assert_eq!(command_only_cell.outbox().len(), COMMAND_COUNT);
    assert_eq!(
        command_only_cell.outbox()[0].payload.get("name"),
        Some(&JsonValue::String("SO-ACME-2025-00001".to_owned()))
    );

    let mut cell = Cell::new(|command: &Command<JsonObject>| command.payload.clone());
    let mut call = naming_call(&action, "ACME", "series-shared-fuel", 1_767_225_600_000);
    call.input.insert(
        "large".to_owned(),
        JsonValue::String("x".repeat(LARGE_INPUT_BYTES)),
    );
    let ledger_len = cell.ledger().len();

    let refusal = commit_action(&mut cell, &catalog, &call, &Allow).unwrap_err();

    assert_eq!(refusal.code(), "decision.invalid_output");
    assert!(
        refusal.to_string().contains("budget exhausted"),
        "{refusal}"
    );
    assert_eq!(cell.ledger().len(), ledger_len);
    assert!(cell.outbox().is_empty());
    assert!(
        !cell
            .load(&FactKey::new("commerce.NamedOrder", "SO-ACME-2025-00001"))
            .unwrap()
            .exists()
    );
}

#[test]
fn naming_series_replay_rechecks_current_schema_and_preconditions() {
    let types = naming_series_types("gapless_within_commit", "year");
    let catalog = TypeCatalog::new(&types);
    let mut cell = Cell::new(|command: &Command<JsonObject>| command.payload.clone());
    let action = naming_series_action("commerce.named_order.create");
    let original = naming_call(
        &action,
        "ACME",
        "series-replay-validation",
        1_767_225_600_000,
    );

    let first = commit_action(&mut cell, &catalog, &original, &Allow).unwrap();
    assert_eq!(outcome_key(first), "SO-ACME-2025-00001");
    let ledger_after_create = cell.ledger().len();

    let unchanged_retry = naming_call(
        &action,
        "ACME",
        "series-replay-validation",
        1_767_243_600_000,
    );
    let replay = commit_action(&mut cell, &catalog, &unchanged_retry, &Allow).unwrap();
    assert_eq!(outcome_key(replay), "SO-ACME-2025-00001");
    assert_eq!(cell.ledger().len(), ledger_after_create);

    let mut tightened = naming_series_action("commerce.named_order.create");
    tightened.preconditions = vec![norn_ir::ZenExpression("name == \"BLOCKED\"".to_owned())];
    let tightened_retry = naming_call(
        &tightened,
        "ACME",
        "series-replay-validation",
        1_767_243_600_000,
    );
    let refusal = commit_action(&mut cell, &catalog, &tightened_retry, &Allow).unwrap_err();
    assert_eq!(refusal.code(), "action.precondition_failed");
    assert_eq!(cell.ledger().len(), ledger_after_create);

    let mut required_generated_key = naming_series_action("commerce.named_order.create");
    required_generated_key.input_schema = serde_json::from_value(json!({
        "type": "object",
        "properties": {
            "name": { "type": "string" },
            "company": { "type": "string" }
        },
        "required": ["company", "name"],
        "additionalProperties": false
    }))
    .unwrap();
    let schema_retry = naming_call(
        &required_generated_key,
        "ACME",
        "series-replay-validation",
        1_767_243_600_000,
    );
    let refusal = commit_action(&mut cell, &catalog, &schema_retry, &Allow).unwrap_err();
    assert_eq!(refusal.code(), "action.invalid_input");
    assert_eq!(cell.ledger().len(), ledger_after_create);

    let next = naming_call(
        &action,
        "ACME",
        "series-replay-validation-next",
        1_767_225_600_000,
    );
    assert_eq!(
        outcome_key(commit_action(&mut cell, &catalog, &next, &Allow).unwrap()),
        "SO-ACME-2025-00002"
    );
}

#[test]
fn naming_series_refusal_does_not_consume_a_counter_and_imported_seed_is_monotone() {
    let types = naming_series_types("gapless_within_commit", "year");
    let catalog = TypeCatalog::new(&types);
    let mut cell = Cell::new(|command: &Command<JsonObject>| command.payload.clone());
    let mut rejected = naming_series_action("commerce.named_order.create");
    rejected.preconditions = vec![norn_ir::ZenExpression("false".to_owned())];
    let rejected_call = naming_call(&rejected, "ACME", "series-rejected", 1_767_225_600_000);
    let ledger_before = cell.ledger().len();
    let refusal = commit_action(&mut cell, &catalog, &rejected_call, &Allow).unwrap_err();
    assert_eq!(refusal.code(), "action.precondition_failed");
    assert_eq!(cell.ledger().len(), ledger_before);

    let seed = NamingCounterSeed {
        object_type: ObjectTypeName("commerce.NamedOrder".to_owned()),
        key_name: norn_ir::KeyName("primary".to_owned()),
        scope: BTreeMap::from([(PropertyId("company".to_owned()), "ACME".to_owned())]),
        period: "2025".to_owned(),
        current: 37,
    };
    norn_kernel::naming_series::install_naming_counter_seeds(
        &mut cell,
        &catalog,
        std::slice::from_ref(&seed),
    )
    .unwrap();
    let seeded_ledger = cell.ledger().len();
    assert!(
        norn_kernel::naming_series::install_naming_counter_seeds(
            &mut cell,
            &catalog,
            std::slice::from_ref(&seed),
        )
        .unwrap()
        .is_none()
    );
    assert_eq!(
        cell.ledger().len(),
        seeded_ledger,
        "equal counter seeds are idempotent"
    );
    let action = naming_series_action("commerce.named_order.create");
    let next = naming_call(&action, "ACME", "series-38", 1_767_225_600_000);
    assert_eq!(
        outcome_key(commit_action(&mut cell, &catalog, &next, &Allow).unwrap()),
        "SO-ACME-2025-00038"
    );
    let mut changed_types = naming_series_types("gapless_within_commit", "year");
    changed_types[0].identity[0]
        .naming_series
        .as_mut()
        .unwrap()
        .pattern = "ORDER-{company}-{YYYY}-{#####}".to_owned();
    let changed_catalog = TypeCatalog::new(&changed_types);
    let changed_action = naming_series_action("commerce.named_order.create");
    let changed_call = naming_call(
        &changed_action,
        "ACME",
        "series-changed-pattern",
        1_767_225_600_000,
    );
    let ledger_before_rebinding = cell.ledger().len();
    let refusal = commit_action(&mut cell, &changed_catalog, &changed_call, &Allow).unwrap_err();
    assert_eq!(refusal.code(), "decision.invalid_output");
    assert!(
        refusal
            .to_string()
            .contains("different naming-series declaration")
    );
    assert_eq!(cell.ledger().len(), ledger_before_rebinding);
    let ledger_before_lowering = cell.ledger().len();
    let lower = NamingCounterSeed {
        current: 12,
        ..seed
    };
    let refusal =
        norn_kernel::naming_series::install_naming_counter_seeds(&mut cell, &catalog, &[lower])
            .unwrap_err();
    assert!(refusal.to_string().contains("cannot lower"));
    assert_eq!(cell.ledger().len(), ledger_before_lowering);
}

#[test]
fn naming_series_rejects_caller_supplied_keys_for_series_creates() {
    let types = naming_series_types("gapless_within_commit", "year");
    let catalog = TypeCatalog::new(&types);
    let mut cell = Cell::new(|command: &Command<JsonObject>| command.payload.clone());
    let create_only = naming_series_action("commerce.named_order.create");
    let mut supplied = naming_call(
        &create_only,
        "ACME",
        "series-manual-create",
        1_767_225_600_000,
    );
    supplied.input.insert(
        "name".to_owned(),
        JsonValue::String("SO-ACME-2025-00001".to_owned()),
    );
    let refusal = commit_action(&mut cell, &catalog, &supplied, &Allow).unwrap_err();
    assert_eq!(refusal.code(), "action.invalid_input");
    assert!(cell.ledger().is_empty());

    let mut upsert = naming_series_action("commerce.named_order.upsert");
    upsert.effect_program = Some(norn_ir::ActionEffectProgram::UpsertInputObject {
        object_id_field: "name".to_owned(),
        object_kind: "commerce.NamedOrder".to_owned(),
        entity_artifact_id: "commerce.NamedOrder.entity".to_owned(),
        entity_artifact_version: "0.1.0".to_owned(),
        write_mode: norn_ir::UpsertWriteMode::Upsert,
    });
    let mut supplied_upsert =
        naming_call(&upsert, "ACME", "series-manual-upsert", 1_767_225_600_000);
    supplied_upsert.input.insert(
        "name".to_owned(),
        JsonValue::String("CALLER-CHOSEN".to_owned()),
    );
    let refusal = commit_action(&mut cell, &catalog, &supplied_upsert, &Allow).unwrap_err();
    assert_eq!(refusal.code(), "action.write_mode_failed");
    assert!(cell.ledger().is_empty());
}

#[test]
fn naming_series_gap_policy_skips_only_occupied_candidates_when_allowed() {
    for (gap_policy, expected_key, expected_skip) in [
        ("gapless_within_commit", "", false),
        ("allow_gaps", "SO-ACME-2025-00002", true),
    ] {
        let types = naming_series_types(gap_policy, "year");
        let catalog = TypeCatalog::new(&types);
        let mut cell = Cell::new(|command: &Command<JsonObject>| command.payload.clone());
        cell.commit(CellCommit {
            writes: vec![FactWrite {
                key: FactKey::new("commerce.NamedOrder", "SO-ACME-2025-00001"),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put {
                    values: JsonObject::from([
                        (
                            "name".to_owned(),
                            JsonValue::String("SO-ACME-2025-00001".to_owned()),
                        ),
                        ("company".to_owned(), JsonValue::String("ACME".to_owned())),
                    ]),
                },
            }],
            ..CellCommit::empty()
        })
        .unwrap();
        let action = naming_series_action("commerce.named_order.create");
        let call = naming_call(&action, "ACME", "series-gap", 1_767_225_600_000);
        if expected_skip {
            assert_eq!(
                outcome_key(commit_action(&mut cell, &catalog, &call, &Allow).unwrap()),
                expected_key
            );
            assert!(matches!(
                cell.ledger().last().unwrap().events.last(),
                Some(FactEvent::NamingSeriesAllocated { value: 2, .. })
            ));
        } else {
            let ledger_before = cell.ledger().len();
            let refusal = commit_action(&mut cell, &catalog, &call, &Allow).unwrap_err();
            assert_eq!(refusal.code(), "decision.invalid_output");
            assert_eq!(cell.ledger().len(), ledger_before);
        }
    }
}

#[test]
fn naming_series_denied_calls_cannot_distinguish_private_allocator_failures() {
    let types = naming_series_types("gapless_within_commit", "year");
    let catalog = TypeCatalog::new(&types);
    for counter_state in [
        "fresh",
        "occupied",
        "exhausted",
        "malformed",
        "precondition",
    ] {
        let mut cell = Cell::new(|command: &Command<JsonObject>| command.payload.clone());
        if matches!(counter_state, "exhausted" | "malformed") {
            let seed = NamingCounterSeed {
                object_type: ObjectTypeName("commerce.NamedOrder".to_owned()),
                key_name: norn_ir::KeyName("primary".to_owned()),
                scope: BTreeMap::from([(PropertyId("company".to_owned()), "ACME".to_owned())]),
                period: "2025".to_owned(),
                current: if counter_state == "exhausted" {
                    99_999
                } else {
                    0
                },
            };
            let receipt = norn_kernel::naming_series::install_naming_counter_seeds(
                &mut cell,
                &catalog,
                &[seed],
            )
            .unwrap()
            .unwrap();
            if counter_state == "malformed" {
                let key = receipt.written[0].key.clone();
                let state = cell.load(&key).unwrap();
                let mut values = state.values.unwrap();
                values.insert("current".to_owned(), JsonValue::String("bad".to_owned()));
                cell.commit(CellCommit {
                    writes: vec![FactWrite {
                        key,
                        expected: state.version,
                        mutation: FactMutation::Put { values },
                    }],
                    ..CellCommit::empty()
                })
                .unwrap();
            }
        }
        if counter_state == "occupied" {
            cell.commit(CellCommit {
                writes: vec![FactWrite {
                    key: FactKey::new("commerce.NamedOrder", "SO-ACME-2025-00001"),
                    expected: FactVersion::ABSENT,
                    mutation: FactMutation::Put {
                        values: JsonObject::from([
                            (
                                "name".to_owned(),
                                JsonValue::String("SO-ACME-2025-00001".to_owned()),
                            ),
                            ("company".to_owned(), JsonValue::String("ACME".to_owned())),
                        ]),
                    },
                }],
                ..CellCommit::empty()
            })
            .unwrap();
        }
        let mut action = naming_series_action("commerce.named_order.create");
        if counter_state == "precondition" {
            action.preconditions = vec![norn_ir::ZenExpression("false".to_owned())];
        }
        let call = naming_call(&action, "ACME", "private-series", 1_767_225_600_000);
        let ledger_before = cell.ledger().len();
        for denial in [
            ActionDenial::Invisible,
            ActionDenial::Forbidden {
                reason: "denied".to_owned(),
            },
        ] {
            let refusal = commit_action(&mut cell, &catalog, &call, &Deny(denial)).unwrap_err();
            assert_eq!(refusal.code(), "action.target_not_found", "{counter_state}");
            assert_eq!(cell.ledger().len(), ledger_before);
        }
    }

    // Key-dependent authorization still sees the actual allocated key, even
    // when that authorizer has not opted into private counter diagnostics.
    let mut cell = Cell::new(|command: &Command<JsonObject>| command.payload.clone());
    let action = naming_series_action("commerce.named_order.create");
    let call = naming_call(&action, "ACME", "private-series-allowed", 1_767_225_600_000);
    assert_eq!(
        outcome_key(commit_action(&mut cell, &catalog, &call, &InputMatchesTargetKey).unwrap()),
        "SO-ACME-2025-00001"
    );
}

#[derive(Default)]
struct ConcurrentState {
    seq: norn_kernel::LedgerSeq,
    facts: BTreeMap<FactKey, FactState>,
    concrete: BTreeMap<FactKey, ObjectTypeName>,
    ledger: Vec<(norn_kernel::LedgerSeq, Vec<FactEvent>)>,
}

impl CommitView for ConcurrentState {
    type Error = Infallible;

    fn pending(&self, _: &norn_kernel::Consumed) -> Result<bool, Infallible> {
        Ok(false)
    }

    fn version(&self, key: &FactKey) -> Result<FactVersion, Infallible> {
        Ok(self
            .facts
            .get(key)
            .map_or(FactVersion::ABSENT, |state| state.version))
    }

    fn concrete(&self, key: &FactKey) -> Result<Option<ObjectTypeName>, Infallible> {
        Ok(self.concrete.get(key).cloned())
    }

    fn link_version(&self, _: &LinkKey) -> Result<FactVersion, Infallible> {
        Ok(FactVersion::ABSENT)
    }

    fn links_of(&self, _: &FactKey) -> Result<Vec<LinkKey>, Infallible> {
        Ok(Vec::new())
    }
}

struct ConcurrentShared {
    state: Mutex<ConcurrentState>,
    types: Vec<ObjectTypeDef>,
    first_attempts: Barrier,
    commit_calls: AtomicUsize,
}

#[derive(Clone)]
struct ConcurrentHost(Arc<ConcurrentShared>);

impl ConcurrentHost {
    fn new(types: Vec<ObjectTypeDef>) -> Self {
        Self(Arc::new(ConcurrentShared {
            state: Mutex::new(ConcurrentState::default()),
            types,
            first_attempts: Barrier::new(2),
            commit_calls: AtomicUsize::new(0),
        }))
    }
}

impl CellHost for ConcurrentHost {
    type Event = FactEvent;
    type Command = JsonObject;
    type Input = JsonObject;
    type Error = Infallible;

    #[expect(
        clippy::unwrap_in_result,
        reason = "This test fixture must panic on failed prerequisites rather than simulate a recoverable domain refusal"
    )]
    fn load(&self, key: &FactKey) -> Result<FactState, Infallible> {
        Ok(self
            .0
            .state
            .lock()
            .unwrap()
            .facts
            .get(key)
            .cloned()
            .unwrap_or_default())
    }

    fn load_link(&self, _: &LinkKey) -> Result<LinkState, Infallible> {
        Ok(LinkState::default())
    }

    #[expect(
        clippy::unwrap_in_result,
        reason = "This test fixture must panic on failed prerequisites rather than simulate a recoverable domain refusal"
    )]
    fn commit(
        &mut self,
        commit: CellCommit<FactEvent, JsonObject, JsonObject>,
    ) -> Result<CommitReceipt, CommitError<Infallible>> {
        if self.0.commit_calls.fetch_add(1, Ordering::SeqCst) < 2 {
            self.0.first_attempts.wait();
        }
        let mut state = self.0.state.lock().unwrap();
        let catalog = TypeCatalog::new(&self.0.types);
        let model = CommitModel::new(&catalog);
        let CommitVersions { written, linked } = check_commit(&commit, &model, &*state)?;
        let seq = state.seq.checked_next().ok_or(CommitError::Overflow)?;
        for (write, version) in commit.writes.iter().zip(&written) {
            let values = match &write.mutation {
                FactMutation::Put { values } => {
                    state
                        .concrete
                        .insert(write.key.clone(), write.key.object_type.clone());
                    Some(values.clone())
                }
                FactMutation::Delete => {
                    state.concrete.remove(&write.key);
                    None
                }
            };
            state.facts.insert(
                write.key.clone(),
                FactState {
                    version: version.version,
                    values,
                },
            );
        }
        state.seq = seq;
        state.ledger.push((seq, commit.events));
        Ok(CommitReceipt {
            seq,
            written,
            linked,
        })
    }

    fn perform(&mut self) -> Result<Option<Performed<JsonObject>>, Infallible> {
        Ok(None)
    }

    fn observe(&mut self) -> Result<Option<Observed<JsonObject>>, Infallible> {
        Ok(None)
    }

    fn clock(&mut self) -> Result<Option<DueTimer<JsonObject>>, Infallible> {
        Ok(None)
    }

    fn publish(&mut self, _: &CommitReceipt) -> Result<(), Infallible> {
        Ok(())
    }
}

#[test]
fn parallel_naming_series_creates_resolve_the_counter_cas_to_contiguous_unique_keys() {
    let types = naming_series_types("gapless_within_commit", "year");
    let catalog = TypeCatalog::new(&types);
    let host = ConcurrentHost::new(types.clone());
    let action = naming_series_action("commerce.named_order.concurrent_create");
    let result_keys = std::thread::scope(|scope| {
        let first_host = host.clone();
        let first_action = &action;
        let first_catalog = &catalog;
        let first = scope.spawn(move || {
            let mut host = first_host;
            outcome_key(
                commit_action(
                    &mut host,
                    first_catalog,
                    &naming_call(first_action, "ACME", "parallel-1", 1_767_225_600_000),
                    &InputMatchesTargetKey,
                )
                .unwrap(),
            )
        });
        let second_host = host.clone();
        let second_action = &action;
        let second_catalog = &catalog;
        let second = scope.spawn(move || {
            let mut host = second_host;
            outcome_key(
                commit_action(
                    &mut host,
                    second_catalog,
                    &naming_call(second_action, "ACME", "parallel-2", 1_767_225_600_000),
                    &InputMatchesTargetKey,
                )
                .unwrap(),
            )
        });
        vec![first.join().unwrap(), second.join().unwrap()]
    });
    let keys: BTreeSet<_> = result_keys.iter().cloned().collect();
    assert_eq!(keys.len(), 2);
    assert_eq!(
        keys,
        BTreeSet::from([
            "SO-ACME-2025-00001".to_owned(),
            "SO-ACME-2025-00002".to_owned(),
        ])
    );
    let state = host.0.state.lock().unwrap();
    assert_eq!(state.ledger.len(), 2, "losing CAS retries without a gap");
}

const OWNED_SERIES_CHILD: &str = "commerce.OwnedSeriesLine";

fn owned_series_types(parent_series: bool, child_series: bool) -> Vec<ObjectTypeDef> {
    let mut parent = naming_series_types("gapless_within_commit", "year").remove(0);
    if !parent_series {
        parent.identity.clear();
    }
    let mut parent = serde_json::to_value(parent).unwrap();
    parent["links"] = json!([{
        "id": "lines", "target": OWNED_SERIES_CHILD,
        "cardinality": {"to": "many", "from": "one"}, "inverse": "parent"
    }]);
    parent["ownedCollections"] = json!([{
        "id": "lines", "childType": OWNED_SERIES_CHILD, "rowIdProperty": "row_id",
        "orderingKey": "idx", "cascadeOnParentDelete": true, "maxRows": 3
    }]);
    let mut base = json!({
        "name": "commerce.OwnedSeriesBase",
        "properties": [
            {"id": "row_id", "type": {"kind": "text"}},
            {"id": "idx", "type": {"kind": "integer"}},
            {"id": "name", "type": {"kind": "text"}},
            {"id": "label", "type": {"kind": "text"}}
        ]
    });
    if child_series {
        base["identity"] = json!([{
            "name": "primary", "properties": ["name"], "namingSeries": {
                "pattern": "LINE-{#####}", "timezone": "UTC", "scopeFields": [],
                "resetPolicy": "never", "gapPolicy": "gapless_within_commit"
            }
        }]);
    }
    vec![
        serde_json::from_value(parent).unwrap(),
        serde_json::from_value(base).unwrap(),
        serde_json::from_value(json!({
            "name": OWNED_SERIES_CHILD, "extends": "commerce.OwnedSeriesBase"
        }))
        .unwrap(),
    ]
}

fn owned_series_action() -> ActionRoot {
    let mut action = naming_series_action("commerce.named_order.rows");
    action.preconditions.clear();
    action.writes = serde_json::from_value(json!([
        {"entityKind": "commerce.NamedOrder", "change": "created"},
        {"entityKind": "commerce.NamedOrder", "change": "modified"},
        {"entityKind": "commerce.NamedOrder", "change": "deleted"},
        {"entityKind": OWNED_SERIES_CHILD, "change": "created"},
        {"entityKind": OWNED_SERIES_CHILD, "change": "modified"},
        {"entityKind": OWNED_SERIES_CHILD, "change": "deleted"}
    ]))
    .unwrap();
    action.input_schema = serde_json::from_value(json!({
        "type": "object", "properties": {
            "name": {"type": "string"}, "version": {"type": "string"},
            "values": {"type": ["null", "boolean", "number", "string", "array", "object"]},
            "rows": {"type": "array", "items": {"type": ["null", "boolean", "number", "string", "array", "object"]}}
        }, "required": ["name", "version", "values", "rows"], "additionalProperties": false
    })).unwrap();
    action.effect_program = Some(
        serde_json::from_value(json!({
            "kind": "mutate_parent_with_rows", "objectIdField": "name",
            "objectKind": "commerce.NamedOrder", "entityArtifactId": "commerce.NamedOrder.entity",
            "entityArtifactVersion": "0.1.0", "parentValuesField": "values",
            "parentVersionField": "version", "rowOpsField": "rows",
            "collections": [{"id": "lines", "preconditions": [], "derive": {}}]
        }))
        .unwrap(),
    );
    action
}

fn owned_series_call<'a>(
    action: &'a ActionRoot,
    key: &str,
    version: &str,
    values: &Value,
    rows: &Value,
) -> ActionCall<'a> {
    let mut request = call(action, "", key);
    request.input = serde_json::from_value(json!({
        "name": "ORDER-1", "version": version, "values": values, "rows": rows
    }))
    .unwrap();
    request
}

fn owned_series_add(row_id: &str) -> Value {
    json!({"collection": "lines", "op": "add", "rowId": row_id,
        "expectedVersion": "0", "values": {"name": format!("LINE-{row_id}"), "label": "line"}})
}

#[test]
fn owned_row_expressions_share_action_fuel_and_refuse_atomically() {
    use norn_expr::{EvaluationBudget, Expression, Value as ExprValue};
    use norn_kernel::{EnvelopeLimits, RefusalCategory, ResourceLimitProfile};

    // Each expression fits the old fresh per-expression budget. Repeating it
    // over bounded rows exceeds the shared action budget without large input.
    let items = serde_json::to_string(&(0..160).collect::<Vec<_>>()).unwrap();
    let guard = format!("sum(map({items}, sum(map({items}, # * 2)))) > 0");
    let derivation = format!("{guard} ? 'derived' : ''");
    let cost = |source: &str| {
        Expression::compile(source)
            .unwrap()
            .evaluate(&ExprValue::Null, EvaluationBudget::default())
            .unwrap()
            .units_used()
    };
    let guard_cost = cost(&guard);
    let derivation_cost = cost(&derivation);
    let fuel_limit = ResourceLimitProfile::norn_limits_v1().turn.fuel_units;
    let limits = EnvelopeLimits {
        max_events: 256,
        ..EnvelopeLimits::V1
    };
    let mut definitions = owned_series_types(false, false);
    definitions[0].owned_collections[0].max_rows.0 = 100;
    let catalog = TypeCatalog::new(&definitions);

    for (derive_rows, guard_rows) in [(true, false), (false, true), (true, true)] {
        let mut action = owned_series_action();
        let norn_ir::ActionEffectProgram::MutateParentWithRows { collections, .. } =
            action.effect_program.as_mut().unwrap()
        else {
            panic!("expected owned-row effect");
        };
        if derive_rows {
            collections[0].derive = serde_json::from_value(json!({ "label": derivation })).unwrap();
        }
        if guard_rows {
            collections[0].preconditions = serde_json::from_value(json!([guard])).unwrap();
        }
        // Also prove rows consume the remainder of ordinary action expressions.
        action.preconditions = serde_json::from_value(json!([guard])).unwrap();
        let row_cost =
            u64::from(derive_rows) * derivation_cost + u64::from(guard_rows) * guard_cost;
        let within = usize::try_from((fuel_limit - guard_cost) / row_cost).unwrap();
        assert!((2..100).contains(&within));

        for count in [within, within + 1] {
            let mut cell = Cell::new(|command: &Command<JsonObject>| command.payload.clone())
                .with_types(definitions.clone());
            let rows = (0..count)
                .map(|index| {
                    let mut row = owned_series_add(&format!("row-{index}"));
                    if derive_rows {
                        row["values"].as_object_mut().unwrap().remove("label");
                    }
                    row
                })
                .collect::<Vec<_>>();
            let request = owned_series_call(
                &action,
                "row-fuel",
                "0",
                &json!({ "name": "ORDER-1", "company": "ACME" }),
                &json!(rows),
            );
            let result = commit_action_with_limits(&mut cell, &catalog, &request, &Allow, &limits);
            let parent = FactKey::new("commerce.NamedOrder", "ORDER-1");
            let manifest = norn_kernel::owned_rows_manifest_key(&parent, "lines").unwrap();
            let record = FactKey::new(
                norn_kernel::action_commit::ACTION_CALL_TYPE,
                "commerce.named_order.rows/row-fuel",
            );
            if count == within {
                assert!(matches!(result.unwrap(), ActionOutcome::Committed { .. }));
                assert_eq!(cell.ledger().len(), 1);
                assert!(cell.load(&parent).unwrap().exists());
                assert!(cell.load(&manifest).unwrap().exists());
                assert!(cell.load(&record).unwrap().exists());
            } else {
                let refusal = result.unwrap_err();
                assert_eq!(refusal.category(), RefusalCategory::Limit);
                assert!(
                    matches!(refusal, ActionRefusal::ExpressionBudgetExceeded { limit }
                    if limit == fuel_limit)
                );
                assert!(cell.ledger().is_empty());
                for key in [&parent, &manifest, &record] {
                    assert_eq!(cell.load(key).unwrap(), FactState::default());
                }
            }
            for index in 0..count {
                let row = FactKey::new(OWNED_SERIES_CHILD, format!("row-{index}"));
                let link = LinkKey::new(parent.clone(), "lines", row.clone());
                assert_eq!(cell.load(&row).unwrap().exists(), count == within);
                assert_eq!(cell.load_link(&link).unwrap().exists(), count == within);
            }
        }
    }
}

fn existing_owned_series_cell(action: &ActionRoot) -> Cell {
    // Existing imported rows predate series installation; their stable row id
    // differs from the inherited generated business-name property.
    let previous = owned_series_types(false, false);
    let catalog = TypeCatalog::new(&previous);
    let mut cell = Cell::new(|command: &Command<JsonObject>| command.payload.clone())
        .with_types(previous.clone());
    commit_action(
        &mut cell,
        &catalog,
        &owned_series_call(
            action,
            "seed-rows",
            "0",
            &json!({"name": "ORDER-1", "company": "ACME"}),
            &json!([owned_series_add("row-a"), owned_series_add("row-b")]),
        ),
        &Allow,
    )
    .unwrap();
    let definitions = owned_series_types(true, true);
    cell = cell.with_types(definitions.clone());
    norn_kernel::naming_series::install_naming_counter_seeds(
        &mut cell,
        &TypeCatalog::new(&definitions),
        &[
            NamingCounterSeed {
                object_type: ObjectTypeName("commerce.NamedOrder".to_owned()),
                key_name: norn_ir::KeyName("primary".to_owned()),
                scope: BTreeMap::from([(PropertyId("company".to_owned()), "ACME".to_owned())]),
                period: "2025".to_owned(),
                current: 7,
            },
            NamingCounterSeed {
                object_type: ObjectTypeName("commerce.OwnedSeriesBase".to_owned()),
                key_name: norn_ir::KeyName("primary".to_owned()),
                scope: BTreeMap::new(),
                period: String::new(),
                current: 7,
            },
        ],
    )
    .unwrap();
    cell
}

fn refuse_owned_series_atomically(
    cell: &mut Cell,
    catalog: &TypeCatalog<'_>,
    request: &ActionCall<'_>,
) -> ActionRefusal<Infallible> {
    let ledger = cell.ledger().to_vec();
    let mut keys = ledger
        .iter()
        .flat_map(|entry| entry.written.iter().map(|write| write.key.clone()))
        .collect::<std::collections::BTreeSet<_>>();
    let parent = FactKey::new("commerce.NamedOrder", "ORDER-1");
    keys.extend([
        parent.clone(),
        FactKey::new(OWNED_SERIES_CHILD, "row-c"),
        FactKey::new(
            norn_kernel::action_commit::ACTION_CALL_TYPE,
            format!("{}/{}", request.action.action_id, request.idempotency_key),
        ),
        norn_kernel::owned_rows_manifest_key(&parent, "lines").unwrap(),
    ]);
    let facts = keys
        .into_iter()
        .map(|key| {
            let state = cell.load(&key).unwrap();
            (key, state)
        })
        .collect::<BTreeMap<_, _>>();
    let mut links = ledger
        .iter()
        .flat_map(|entry| entry.linked.iter().map(|write| write.key.clone()))
        .collect::<std::collections::BTreeSet<_>>();
    links.insert(LinkKey::new(
        parent,
        "lines",
        FactKey::new(OWNED_SERIES_CHILD, "row-c"),
    ));
    let links = links
        .into_iter()
        .map(|key| {
            let state = cell.load_link(&key).unwrap();
            (key, state)
        })
        .collect::<BTreeMap<_, _>>();
    let refusal = commit_action(cell, catalog, request, &Allow).unwrap_err();
    assert_eq!(cell.ledger(), ledger);
    for (key, state) in facts {
        assert_eq!(cell.load(&key).unwrap(), state);
    }
    for (key, state) in links {
        assert_eq!(cell.load_link(&key).unwrap(), state);
    }
    assert!(cell.outbox().is_empty());
    refusal
}

#[test]
fn owned_rows_cannot_create_generated_parents_or_children_without_allocation() {
    let definitions = owned_series_types(true, true);
    let catalog = TypeCatalog::new(&definitions);
    let action = owned_series_action();
    let mut fresh = Cell::new(|command: &Command<JsonObject>| command.payload.clone())
        .with_types(definitions.clone());
    let create = owned_series_call(
        &action,
        "manual-parent",
        "0",
        &json!({"company": "ACME"}),
        &json!([]),
    );
    assert!(
        matches!(refuse_owned_series_atomically(&mut fresh, &catalog, &create),
        ActionRefusal::InvalidInput { field } if field == "name")
    );
    let mut existing = existing_owned_series_cell(&action);
    let add = owned_series_call(
        &action,
        "manual-child",
        "1",
        &json!({}),
        &json!([owned_series_add("row-c")]),
    );
    assert!(
        matches!(refuse_owned_series_atomically(&mut existing, &catalog, &add),
        ActionRefusal::InvalidInput { field } if field == "rows")
    );
}

#[test]
fn owned_rows_preserve_generated_names_but_allow_updates_and_reorders() {
    let definitions = owned_series_types(true, true);
    let catalog = TypeCatalog::new(&definitions);
    let action = owned_series_action();
    let mut cell = existing_owned_series_cell(&action);
    let parent_patch = owned_series_call(
        &action,
        "rename-parent",
        "1",
        &json!({"name": "OTHER"}),
        &json!([]),
    );
    assert!(
        matches!(refuse_owned_series_atomically(&mut cell, &catalog, &parent_patch),
        ActionRefusal::InvalidInput { field } if field == "values")
    );
    let child_patch = owned_series_call(
        &action,
        "rename-child",
        "1",
        &json!({}),
        &json!([{
            "collection": "lines", "op": "update", "rowId": "row-a", "expectedVersion": "1", "values": {"name": "OTHER"}
        }]),
    );
    assert!(
        matches!(refuse_owned_series_atomically(&mut cell, &catalog, &child_patch),
        ActionRefusal::InvalidInput { field } if field == "rows")
    );
    let mut derived = action.clone();
    if let Some(norn_ir::ActionEffectProgram::MutateParentWithRows { collections, .. }) =
        &mut derived.effect_program
    {
        collections[0].derive.insert(
            PropertyId("name".to_owned()),
            norn_ir::ZenExpression("\"OTHER\"".to_owned()),
        );
    }
    let derive = owned_series_call(&derived, "derive-name", "1", &json!({}), &json!([]));
    assert!(matches!(
        refuse_owned_series_atomically(&mut cell, &catalog, &derive),
        ActionRefusal::UnsupportedEffect { .. }
    ));
    let update = owned_series_call(
        &action,
        "edit-existing",
        "1",
        &json!({"company": "ACME"}),
        &json!([
            {"collection": "lines", "op": "update", "rowId": "row-a", "expectedVersion": "1", "values": {"label": "edited"}},
            {"collection": "lines", "op": "reorder", "rowIds": [
                {"rowId": "row-b", "expectedVersion": "1"}, {"rowId": "row-a", "expectedVersion": "1"}
            ]}
        ]),
    );
    let before = cell.ledger().len();
    let ActionOutcome::Committed { receipt, record } =
        commit_action(&mut cell, &catalog, &update, &Allow).unwrap()
    else {
        panic!("expected existing-row update");
    };
    assert_eq!(cell.ledger().len(), before + 1);
    assert!(receipt.linked.is_empty());
    assert_eq!(record.row_operations.len(), 2);
    assert!(
        receipt
            .written
            .iter()
            .all(|write| write.key.object_type.as_str()
                != norn_kernel::naming_series::NAMING_SERIES_COUNTER_TYPE)
    );
    for (row_id, idx) in [("row-b", 1_i64), ("row-a", 2_i64)] {
        let key = FactKey::new(OWNED_SERIES_CHILD, row_id);
        let values = cell.load(&key).unwrap().values.unwrap();
        assert_eq!(
            values.get("name"),
            Some(&JsonValue::String(format!("LINE-{row_id}")))
        );
        assert_eq!(values.get("idx"), Some(&JsonValue::from(idx)));
        assert!(
            cell.load_link(&LinkKey::new(
                FactKey::new("commerce.NamedOrder", "ORDER-1"),
                "lines",
                key
            ))
            .unwrap()
            .exists()
        );
    }
    assert_eq!(
        cell.load(&FactKey::new(OWNED_SERIES_CHILD, "row-a"))
            .unwrap()
            .values
            .unwrap()
            .get("label"),
        Some(&JsonValue::from("edited"))
    );
}

fn many_owned_collections(count: usize) -> (Vec<ObjectTypeDef>, ActionRoot) {
    let mut definitions = owned_series_types(false, false);
    let mut parent = serde_json::to_value(&definitions[0]).unwrap();
    parent["ownedCollections"] = json!(
        (0..count)
            .map(|index| json!({
                "id": format!("lines{index}"), "childType": OWNED_SERIES_CHILD,
                "rowIdProperty": "row_id", "orderingKey": "idx", "maxRows": 3,
                "cascadeOnParentDelete": true
            }))
            .collect::<Vec<_>>()
    );
    parent["links"] = json!(
        (0..count)
            .map(|index| json!({
                "id": format!("lines{index}"), "target": OWNED_SERIES_CHILD,
                "cardinality": {"to": "many", "from": "one"}
            }))
            .collect::<Vec<_>>()
    );
    definitions[0] = serde_json::from_value(parent).unwrap();
    let mut action = owned_series_action();
    if let Some(norn_ir::ActionEffectProgram::MutateParentWithRows { collections, .. }) =
        &mut action.effect_program
    {
        *collections = serde_json::from_value(json!(
            (0..count)
                .map(|index| json!({"id": format!("lines{index}")}))
                .collect::<Vec<_>>()
        ))
        .unwrap();
    }
    (definitions, action)
}

#[test]
fn unchanged_owned_manifests_use_snapshot_bounds_not_event_limits() {
    let (definitions, action) = many_owned_collections(33);
    let catalog = TypeCatalog::new(&definitions);
    let mut cell = Cell::new(|command: &Command<JsonObject>| command.payload.clone())
        .with_types(definitions.clone());
    commit_action(
        &mut cell,
        &catalog,
        &owned_series_call(
            &action,
            "create-many",
            "0",
            &json!({"name": "ORDER-1", "company": "ACME"}),
            &json!([]),
        ),
        &Allow,
    )
    .unwrap();
    let before = cell.ledger().to_vec();
    let ActionOutcome::Committed { receipt, .. } = commit_action(
        &mut cell,
        &catalog,
        &owned_series_call(&action, "read-many", "1", &json!({}), &json!([])),
        &Allow,
    )
    .unwrap() else {
        panic!("expected empty patch to commit");
    };
    assert_eq!(cell.ledger().len(), before.len() + 1);
    assert!(cell.ledger().last().unwrap().events.is_empty());
    assert_eq!(
        receipt.written.len(),
        1,
        "only the action receipt is written"
    );
    let rows = (0..33)
        .map(|index| {
            json!({
                "collection": format!("lines{index}"), "op": "add", "rowId": format!("row-{index}"),
                "expectedVersion": "0", "values": {"name": format!("LINE-{index}"), "label": "line"}
            })
        })
        .collect::<Vec<_>>();
    let request = owned_series_call(&action, "emit-too-many", "1", &json!({}), &json!(rows));
    assert!(matches!(
        refuse_owned_series_atomically(&mut cell, &catalog, &request),
        ActionRefusal::Decision(norn_kernel::DecisionRefusal::EnvelopeExceeded {
            kind: norn_kernel::EnvelopeKind::Events,
            limit: 32,
            ..
        })
    ));
}

#[test]
fn owned_collection_manifests_still_obey_the_snapshot_record_bound() {
    let max_records = norn_kernel::ResourceLimitProfile::norn_limits_v1()
        .slice
        .max_records;
    let (definitions, action) = many_owned_collections(usize::try_from(max_records).unwrap() + 1);
    let catalog = TypeCatalog::new(&definitions);
    let mut cell = Cell::new(|command: &Command<JsonObject>| command.payload.clone())
        .with_types(definitions.clone());
    let request = owned_series_call(
        &action,
        "too-many-snapshots",
        "0",
        &json!({"name": "ORDER-1", "company": "ACME"}),
        &json!([]),
    );
    assert!(
        matches!(refuse_owned_series_atomically(&mut cell, &catalog, &request),
        ActionRefusal::SliceUnavailable { entry, .. } if entry == "$owned_rows")
    );
}

#[test]
fn base_owned_row_actions_preserve_subtype_collections_and_refuse_uncovered_deletes() {
    let mut definitions = owned_series_types(false, false);
    let subtype = "commerce.SpecialOrder";
    definitions.push(serde_json::from_value(json!({
        "name": subtype, "extends": "commerce.NamedOrder",
        "links": [{"id": "extras", "target": OWNED_SERIES_CHILD, "cardinality": {"to": "many", "from": "one"}}],
        "ownedCollections": [{"id": "extras", "childType": OWNED_SERIES_CHILD, "rowIdProperty": "row_id",
            "orderingKey": "idx", "maxRows": 3, "cascadeOnParentDelete": true}]
    })).unwrap());
    let catalog = TypeCatalog::new(&definitions);
    let base_action = owned_series_action();
    let mut subtype_action = base_action.clone();
    if let Some(norn_ir::ActionEffectProgram::MutateParentWithRows {
        object_kind,
        collections,
        ..
    }) = &mut subtype_action.effect_program
    {
        *object_kind = subtype.to_owned();
        collections.push(serde_json::from_value(json!({"id": "extras"})).unwrap());
    }
    let mut cell = Cell::new(|command: &Command<JsonObject>| command.payload.clone())
        .with_types(definitions.clone());
    let mut seed = owned_series_call(
        &subtype_action,
        "seed-subtype",
        "0",
        &json!({"name": "ORDER-1", "company": "ACME"}),
        &json!([
            owned_series_add("row-a"),
            {"collection": "extras", "op": "add", "rowId": "extra-a", "expectedVersion": "0",
                "values": {"name": "EXTRA", "label": "untouched"}}
        ]),
    );
    seed.object_type = Some(ObjectTypeName(subtype.to_owned()));
    commit_action(&mut cell, &catalog, &seed, &Allow).unwrap();
    let parent = FactKey::new(subtype, "ORDER-1");
    let manifest = norn_kernel::owned_rows_manifest_key(&parent, "extras").unwrap();
    let child = FactKey::new(OWNED_SERIES_CHILD, "extra-a");
    let link = LinkKey::new(parent.clone(), "extras", child.clone());
    let preserved = (
        cell.load(&manifest).unwrap(),
        cell.load(&child).unwrap(),
        cell.load_link(&link).unwrap(),
    );
    for (key, version, values, rows) in [
        ("base-parent", "1", json!({"company": "OTHER"}), json!([])),
        (
            "base-row",
            "2",
            json!({}),
            json!([{"collection": "lines", "op": "update", "rowId": "row-a",
            "expectedVersion": "1", "values": {"label": "edited"}}]),
        ),
    ] {
        let mut request = owned_series_call(&base_action, key, version, &values, &rows);
        request.object_type = Some(ObjectTypeName(subtype.to_owned()));
        commit_action(&mut cell, &catalog, &request, &Allow).unwrap();
        assert_eq!(
            (
                cell.load(&manifest).unwrap(),
                cell.load(&child).unwrap(),
                cell.load_link(&link).unwrap()
            ),
            preserved
        );
    }
    let mut request = owned_series_call(
        &base_action,
        "unconfigured-row",
        "3",
        &json!({}),
        &json!([{"collection": "extras", "op": "remove", "rowId": "extra-a", "expectedVersion": "1"}]),
    );
    request.object_type = Some(ObjectTypeName(subtype.to_owned()));
    assert!(matches!(
        refuse_owned_series_atomically(&mut cell, &catalog, &request),
        ActionRefusal::InvalidInput { .. }
    ));
    let mut deleting = base_action.clone();
    let mut schema = serde_json::to_value(&deleting.input_schema).unwrap();
    schema["properties"]["delete"] = json!({"type": "boolean"});
    deleting.input_schema = serde_json::from_value(schema).unwrap();
    if let Some(norn_ir::ActionEffectProgram::MutateParentWithRows {
        parent_delete_field,
        ..
    }) = &mut deleting.effect_program
    {
        *parent_delete_field = Some("delete".to_owned());
    }
    let mut request = owned_series_call(&deleting, "uncovered-delete", "3", &json!({}), &json!([]));
    request.object_type = Some(ObjectTypeName(subtype.to_owned()));
    request
        .input
        .insert("delete".to_owned(), JsonValue::Bool(true));
    assert!(matches!(
        refuse_owned_series_atomically(&mut cell, &catalog, &request),
        ActionRefusal::UnsupportedEffect { .. }
    ));
    assert!(cell.load(&parent).unwrap().exists());
    assert_eq!(
        (
            cell.load(&manifest).unwrap(),
            cell.load(&child).unwrap(),
            cell.load_link(&link).unwrap()
        ),
        preserved
    );
    let mut incomplete = subtype_action.clone();
    if let Some(norn_ir::ActionEffectProgram::MutateParentWithRows { collections, .. }) =
        &mut incomplete.effect_program
    {
        collections.pop();
    }
    let mut request =
        owned_series_call(&incomplete, "missing-coverage", "3", &json!({}), &json!([]));
    request.object_type = Some(ObjectTypeName(subtype.to_owned()));
    assert!(matches!(
        refuse_owned_series_atomically(&mut cell, &catalog, &request),
        ActionRefusal::UnsupportedEffect { .. }
    ));
}

#[test]
fn authored_brand_create_refuses_missing_required_legal_owner_without_a_receipt() {
    let envelope: Value = serde_json::from_str(include_str!(
        "../../../registry/core/artifacts/party.Brand.entity.json"
    ))
    .unwrap();
    let brand: ObjectTypeDef =
        serde_json::from_value(envelope["payload"]["objectType"].clone()).unwrap();
    let owner: ObjectTypeDef =
        serde_json::from_value(json!({"name":"party.Organization"})).unwrap();
    let types = vec![brand, owner];
    let value_types: Value = serde_json::from_str(include_str!(
        "../../../registry/core/artifacts/core.value_types.entity.json"
    ))
    .unwrap();
    let property_types: Vec<norn_ir::PropertyTypeDef> =
        serde_json::from_value(value_types["payload"]["propertyTypes"].clone()).unwrap();
    let catalog = TypeCatalog::new(&types).with_property_types(&property_types);
    let mut cell = Cell::new(|command: &Command<JsonObject>| command.payload.clone())
        .with_types(types.clone())
        .with_property_types(property_types.clone());
    let mut payload =
        fixture("ir/commerce-sales-order-submit-invoice.action.json")["payload"].clone();
    payload["actionId"] = json!("party.brand.create");
    payload["inputSchema"] = json!({"type":"object","properties":{
        "brand_code":{"type":"string"},"display_name":{"type":"string"}},
        "required":["brand_code","display_name"],"additionalProperties":false});
    payload["outputSchema"] = payload["inputSchema"].clone();
    payload["preconditions"] = json!([]);
    payload["writes"] = json!([{"entityKind":"party.Brand","change":"created"}]);
    payload["effectProgram"] = json!({"kind":"upsert_input_object","objectIdField":"brand_code",
        "objectKind":"party.Brand","entityArtifactId":"party.Brand.entity",
        "entityArtifactVersion":"0.1.0","writeMode":{"kind":"create_only"}});
    let action: ActionRoot = serde_json::from_value(payload).unwrap();
    let mut request = call(&action, "BRAND-1", "brand-ownerless");
    request.input =
        serde_json::from_value(json!({"brand_code":"BRAND-1","display_name":"Brand"})).unwrap();
    let refusal = commit_action(&mut cell, &catalog, &request, &Allow).unwrap_err();
    assert!(
        refusal.to_string().contains("required link `legal_entity`"),
        "{refusal}"
    );
    assert!(cell.ledger().is_empty());
    assert!(
        !cell
            .load(&FactKey::new("party.Brand", "BRAND-1"))
            .unwrap()
            .exists()
    );
}
