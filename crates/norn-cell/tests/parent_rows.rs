#![cfg(feature = "native")]
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs,
    reason = "integration-test crate"
)]

//! Native SQLite integration coverage for atomic parent and ordered child-row
//! action commits (#3254).

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use norn_cell::SqliteCell;
use norn_ir::ActionRoot;
use norn_kernel::cell_host::{FactKey, FactState, JsonObject, JsonValue, LinkKey, LinkState};
use norn_kernel::{
    ACTION_CALL_TYPE, ActionAuthorizer, ActionAuthzRequest, ActionCall, ActionDenial,
    ActionOutcome, ActionRefusal, CellCommit, CellHost, CommitError, CommitReceipt, DecisionTurn,
    DueTimer, EnvelopeLimits, FactEvent, FactVersion, LogicalTime, OWNED_ROWS_TYPE, Observed,
    OwnedRowsCommitProof, Performed, TypeCatalog, World, commit_action, commit_action_with_limits,
    owned_rows_manifest_key,
};
use serde_json::{Value, json};

const PARENT: &str = "test.Order";
const CHILD: &str = "test.OrderLine";
const COLLECTION: &str = "lines";

type NativeCell = SqliteCell<FactEvent, JsonObject, JsonObject>;

struct Allow;

impl ActionAuthorizer for Allow {
    fn authorize(&self, _: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        Ok(())
    }
}

fn types() -> Vec<norn_ir::ObjectTypeDef> {
    vec![
        serde_json::from_value(json!({
            "name": PARENT,
            "properties": [{ "id": "title", "type": { "kind": "text" } }],
            "links": [{
                "id": COLLECTION,
                "target": CHILD,
                "cardinality": { "to": "many", "from": "one" },
                "inverse": "parent"
            }],
            "ownedCollections": [{
                "id": COLLECTION,
                "childType": CHILD,
                "rowIdProperty": "row_id",
                "orderingKey": "idx",
                "cascadeOnParentDelete": true,
                "maxRows": 2
            }]
        }))
        .expect("parent definition decodes"),
        serde_json::from_value(json!({
            "name": CHILD,
            "properties": [
                { "id": "row_id", "type": { "kind": "text" } },
                { "id": "idx", "type": { "kind": "integer" } },
                { "id": "label", "type": { "kind": "text" } },
                { "id": "quantity", "type": { "kind": "integer" } },
                { "id": "double_quantity", "type": { "kind": "integer" } }
            ]
        }))
        .expect("child definition decodes"),
        serde_json::from_value(json!({
            "name": ACTION_CALL_TYPE,
            "properties": [
                { "id": "actionId", "type": { "kind": "text" } },
                { "id": "actionArtifactId", "type": { "kind": "text" }, "constraints": { "nullable": true } },
                { "id": "actionVersion", "type": { "kind": "text" }, "constraints": { "nullable": true } },
                { "id": "effectKind", "type": { "kind": "text" }, "constraints": { "nullable": true } },
                { "id": "object", "type": { "kind": "json-ext" } },
                { "id": "input", "type": { "kind": "json-ext" } },
                { "id": "effectiveInput", "type": { "kind": "json-ext" }, "constraints": { "nullable": true } },
                { "id": "derivedValues", "type": { "kind": "json-ext" }, "default": [] },
                { "id": "preconditionOutcomes", "type": { "kind": "json-ext" }, "default": [] },
                { "id": "actor", "type": { "kind": "text" } },
                { "id": "turn", "type": { "kind": "text" } },
                { "id": "transition", "type": { "kind": "text" } },
                { "id": "from", "type": { "kind": "text" } },
                { "id": "to", "type": { "kind": "text" } },
                { "id": "rowOperations", "type": { "kind": "json-ext" }, "constraints": { "nullable": true } }
            ]
        }))
        .expect("action-call definition decodes"),
        serde_json::from_value(json!({
            "name": OWNED_ROWS_TYPE,
            "properties": [
                { "id": "parentType", "type": { "kind": "text" } },
                { "id": "parentKey", "type": { "kind": "text" } },
                { "id": "collectionId", "type": { "kind": "text" } },
                { "id": "rowIds", "type": { "kind": "json-ext" } }
            ]
        }))
        .expect("owned-row manifest definition decodes"),
    ]
}

// Only the dense-order derivation scenarios declare this additional required
// stored value. It is produced by the action, not an EntityIR computed field.
fn types_with_derived_idx() -> Vec<norn_ir::ObjectTypeDef> {
    let mut definitions = types();
    definitions
        .iter_mut()
        .find(|definition| definition.name.as_str() == CHILD)
        .expect("child definition exists")
        .properties
        .push(
            serde_json::from_value(json!({
                "id": "derived_idx", "type": { "kind": "integer" }
            }))
            .expect("required stored derivation target decodes"),
        );
    definitions
}

fn types_with_max_rows(max_rows: u64) -> Vec<norn_ir::ObjectTypeDef> {
    let mut definitions = types();
    let mut parent = serde_json::to_value(definitions.remove(0)).expect("parent serializes");
    parent["ownedCollections"][0]["maxRows"] = json!(max_rows);
    definitions.insert(
        0,
        serde_json::from_value(parent).expect("bounded parent definition decodes"),
    );
    definitions
}

fn types_with_link_payload(payload: &str) -> Vec<norn_ir::ObjectTypeDef> {
    let mut definitions = types();
    let mut parent = serde_json::to_value(definitions.remove(0)).expect("parent serializes");
    parent["links"][0]["properties"] = json!([{
        "id": "tracking",
        "type": { "kind": "text" },
        "default": payload
    }]);
    definitions.insert(
        0,
        serde_json::from_value(parent).expect("parent with link payload decodes"),
    );
    definitions
}

fn types_with_row_defaults(include_lifecycle: bool) -> Vec<norn_ir::ObjectTypeDef> {
    let mut definitions = types_with_derived_idx();
    let mut child = serde_json::to_value(definitions.remove(1)).expect("child serializes");
    let properties = child["properties"]
        .as_array_mut()
        .expect("child properties are an array");
    let quantity = properties
        .iter_mut()
        .find(|property| property["id"] == "quantity")
        .expect("quantity property exists");
    quantity["default"] = json!("5");
    if include_lifecycle {
        properties.push(json!({
            "id": "status",
            "type": { "kind": "enum", "values": [{ "value": "draft" }, { "value": "done" }] },
            "default": "draft"
        }));
        child["lifecycles"] = json!([{
            "property": "status",
            "initial": ["draft"],
            "terminal": ["done"],
            "transitions": [{ "id": "finish", "from": ["draft"], "to": "done" }]
        }]);
    }
    definitions.insert(
        1,
        serde_json::from_value(child).expect("child with defaults decodes"),
    );
    definitions
}

fn action() -> ActionRoot {
    action_with_row_guard("test.order.edit", "self.quantity > 0")
}

fn action_with_row_guard(action_id: &str, row_guard: &str) -> ActionRoot {
    action_with_derivations(
        action_id,
        row_guard,
        &json!({ "double_quantity": "self.quantity * 2" }),
    )
}

fn action_with_derived_idx() -> ActionRoot {
    action_with_derivations(
        "test.order.edit",
        "self.quantity > 0",
        &json!({
            "double_quantity": "self.quantity * 2",
            "derived_idx": "self.idx"
        }),
    )
}

fn action_with_lifecycle_default_guard() -> ActionRoot {
    action_with_derivations(
        "test.order.edit.lifecycle-default",
        "self.status == \"draft\"",
        &json!({
            "double_quantity": "self.quantity * 2",
            "derived_idx": "self.idx"
        }),
    )
}

fn action_with_derivations(action_id: &str, row_guard: &str, derive: &Value) -> ActionRoot {
    let mut payload = common::fixtures();
    payload.push("ir/commerce-sales-order-submit-invoice.action.json");
    let document: Value =
        serde_json::from_str(&std::fs::read_to_string(payload).expect("reads action fixture"))
            .expect("action fixture parses");
    let mut payload = document["payload"].clone();
    payload["actionId"] = json!(action_id);
    payload["inputSchema"] = input_schema();
    payload["preconditions"] = json!([]);
    payload["writes"] = json!([
        { "entityKind": PARENT, "change": "created" },
        { "entityKind": PARENT, "change": "modified" },
        { "entityKind": PARENT, "change": "deleted" },
        { "entityKind": CHILD, "change": "created" },
        { "entityKind": CHILD, "change": "modified" },
        { "entityKind": CHILD, "change": "deleted" }
    ]);
    payload["effectProgram"] = json!({
        "kind": "mutate_parent_with_rows",
        "objectIdField": "parentId",
        "objectKind": PARENT,
        "entityArtifactId": "test.Order.entity",
        "entityArtifactVersion": "1.0.0",
        "parentValuesField": "parentValues",
        "parentVersionField": "parentVersion",
        "rowOpsField": "rowOps",
        "parentDeleteField": "deleteParent",
        "collections": [{
            "id": COLLECTION,
            "preconditions": [row_guard],
            "derive": derive
        }]
    });
    serde_json::from_value(payload).expect("typed action decodes")
}

fn input_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "parentId": { "type": "string" },
            "parentValues": { "type": ["null", "boolean", "number", "string", "array", "object"] },
            "parentVersion": { "type": "string" },
            "rowOps": { "type": ["null", "boolean", "number", "string", "array", "object"] },
            "deleteParent": { "type": "boolean" }
        },
        "required": ["parentId", "parentValues", "parentVersion", "rowOps"],
        "additionalProperties": false
    })
}

fn call<'a>(action: &'a ActionRoot, key: &str, input: Value) -> ActionCall<'a> {
    ActionCall {
        action,
        action_artifact_id: "test.Order.edit.action",
        action_version: "1.0.0",
        scope: BTreeMap::from([
            ("orgId".to_owned(), "org_demo".to_owned()),
            ("environmentId".to_owned(), "dev".to_owned()),
        ]),
        actor: "test:operator".to_owned(),
        input: serde_json::from_value(input).expect("action input decodes"),
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

fn action_input(parent_version: &str, parent_values: &Value, row_ops: &Value) -> Value {
    json!({
        "parentId": "order-1",
        "parentValues": parent_values,
        "parentVersion": parent_version,
        "rowOps": row_ops
    })
}

fn add(row_id: &str, label: &str, quantity: i64) -> Value {
    json!({
        "collection": COLLECTION,
        "op": "add",
        "rowId": row_id,
        "expectedVersion": "0",
        "values": { "label": label, "quantity": quantity }
    })
}

fn native_cell() -> NativeCell {
    let definitions = types();
    native_cell_for(&definitions)
}

fn native_cell_for(definitions: &[norn_ir::ObjectTypeDef]) -> NativeCell {
    let model = common::model_of(definitions);
    NativeCell::open_in_memory(model, "org_demo/dev", |command| command.payload.clone())
        .expect("native SQLite cell opens")
}

fn entity_catalog(definitions: &[norn_ir::ObjectTypeDef]) -> TypeCatalog<'_> {
    TypeCatalog::new(definitions.iter().take(2))
}

fn parent_key() -> FactKey {
    FactKey::new(PARENT, "order-1")
}

fn row_key(row_id: &str) -> FactKey {
    FactKey::new(CHILD, row_id)
}

fn manifest_key() -> FactKey {
    owned_rows_manifest_key(&parent_key(), COLLECTION).expect("manifest key derives")
}

fn assert_parent(cell: &NativeCell, expected_version: u64, expected_title: &str) {
    let parent = cell.load(&parent_key()).expect("loads parent");
    assert_eq!(parent.version.0, expected_version);
    assert_eq!(
        parent
            .values
            .as_ref()
            .and_then(|values| values.get("title")),
        Some(&JsonValue::String(expected_title.to_owned()))
    );
}

fn integer(value: i64) -> JsonValue {
    serde_json::from_value(json!(value)).expect("integer")
}

fn assert_row(cell: &NativeCell, row_id: &str, version: u64, idx: i64, label: &str, quantity: i64) {
    let row = cell.load(&row_key(row_id)).expect("loads row");
    assert_eq!(row.version.0, version);
    let values = row.values.expect("row exists");
    assert_eq!(
        values.get("row_id"),
        Some(&JsonValue::String(row_id.to_owned()))
    );
    assert_eq!(values.get("idx"), Some(&integer(idx)));
    assert_eq!(
        values.get("label"),
        Some(&JsonValue::String(label.to_owned()))
    );
    assert_eq!(values.get("quantity"), Some(&integer(quantity)));
    assert_eq!(
        values.get("double_quantity"),
        Some(&integer(
            quantity
                .checked_mul(2)
                .expect("fixture quantity doubled fits in i64"),
        )),
        "the manifest-trusted derivation is materialized"
    );
}

fn assert_derived_idx(cell: &NativeCell, row_id: &str, expected: i64) {
    let row = cell.load(&row_key(row_id)).expect("loads derived row");
    assert_eq!(
        row.values
            .as_ref()
            .and_then(|values| values.get("derived_idx")),
        Some(&integer(expected)),
        "the derived value observes final dense order"
    );
}

fn assert_manifest(cell: &NativeCell, row_ids: &[&str]) {
    let manifest = cell
        .load(&manifest_key())
        .expect("loads owned rows manifest")
        .values
        .expect("manifest exists");
    assert_eq!(
        manifest.get("rowIds"),
        Some(&serde_json::from_value(json!(row_ids)).expect("row ID array"))
    );
}

fn seed_rows(cell: &mut NativeCell, catalog: &TypeCatalog<'_>, action: &ActionRoot) {
    commit_action(
        cell,
        catalog,
        &call(
            action,
            "seed",
            action_input(
                "0",
                &json!({ "title": "initial" }),
                &json!([add("row-a", "A", 1), add("row-b", "B", 2)]),
            ),
        ),
        &Allow,
    )
    .expect("creates parent and two rows");
}

#[test]
fn parent_and_rows_commit_once_then_reorder_keeps_stable_ids() {
    let definitions = types();
    let catalog = entity_catalog(&definitions);
    let mut cell = native_cell();
    let action = action();

    let committed = commit_action(
        &mut cell,
        &catalog,
        &call(
            &action,
            "create-and-add",
            action_input(
                "0",
                &json!({ "title": "initial" }),
                &json!([add("row-a", "A", 1), add("row-b", "B", 2)]),
            ),
        ),
        &Allow,
    )
    .expect("commits parent and rows");
    let ActionOutcome::Committed { receipt, record } = committed else {
        panic!("expected a new action commit, got {committed:?}");
    };

    assert_eq!(cell.commits().expect("reads ledger").len(), 1);
    assert!(
        receipt
            .written
            .iter()
            .any(|write| write.key == parent_key())
    );
    assert!(
        receipt
            .written
            .iter()
            .any(|write| write.key == row_key("row-a"))
    );
    assert!(
        receipt
            .written
            .iter()
            .any(|write| write.key == row_key("row-b"))
    );
    assert!(
        receipt
            .written
            .iter()
            .any(|write| write.key == manifest_key())
    );
    assert_eq!(
        receipt.linked.len(),
        2,
        "each child is linked to the parent in the same commit"
    );
    assert_eq!(
        serde_json::to_value(&record).expect("serializes action ledger receipt")["rowOperations"]
            .as_array()
            .map(Vec::len),
        Some(2),
        "the durable action receipt lists both row operations"
    );
    assert_parent(&cell, 1, "initial");
    assert_row(&cell, "row-a", 1, 1, "A", 1);
    assert_row(&cell, "row-b", 1, 2, "B", 2);
    assert_manifest(&cell, &["row-a", "row-b"]);
    for row_id in ["row-a", "row-b"] {
        let link = LinkKey::new(parent_key(), COLLECTION, row_key(row_id));
        assert!(cell.load_link(&link).expect("loads row link").exists());
    }

    let reorder = json!([{
        "collection": COLLECTION,
        "op": "reorder",
        "rowIds": [
            { "rowId": "row-b", "expectedVersion": "1" },
            { "rowId": "row-a", "expectedVersion": "1" }
        ]
    }]);
    commit_action(
        &mut cell,
        &catalog,
        &call(
            &action,
            "reorder",
            action_input("1", &json!({ "title": "reordered" }), &reorder),
        ),
        &Allow,
    )
    .expect("reorders all current rows");

    assert_eq!(cell.commits().expect("reads ledger").len(), 2);
    assert_parent(&cell, 2, "reordered");
    assert_row(&cell, "row-b", 2, 1, "B", 2);
    assert_row(&cell, "row-a", 2, 2, "A", 1);
    assert_manifest(&cell, &["row-b", "row-a"]);
}

struct RacingNative {
    cell: NativeCell,
    raced: bool,
    revoke_child_authorization: Option<Arc<AtomicBool>>,
}

impl CellHost for RacingNative {
    type Event = FactEvent;
    type Command = JsonObject;
    type Input = JsonObject;
    type Error = norn_cell::CellError;

    fn load(&self, key: &FactKey) -> Result<FactState, Self::Error> {
        self.cell.load(key)
    }

    fn load_link(&self, key: &LinkKey) -> Result<LinkState, Self::Error> {
        self.cell.load_link(key)
    }

    #[expect(
        clippy::unwrap_in_result,
        reason = "This test fixture must panic on failed prerequisites rather than simulate a recoverable domain refusal"
    )]
    fn commit(
        &mut self,
        commit: CellCommit<Self::Event, Self::Command, Self::Input>,
    ) -> Result<CommitReceipt, CommitError<Self::Error>> {
        if !self.raced {
            self.raced = true;
            self.cell
                .commit(commit.clone())
                .expect("race winner commits");
            if let Some(authorized) = &self.revoke_child_authorization {
                authorized.store(false, Ordering::SeqCst);
            }
        }
        self.cell.commit(commit)
    }

    fn commit_owned_rows(
        &mut self,
        commit: CellCommit<Self::Event, Self::Command, Self::Input>,
        proof: OwnedRowsCommitProof,
    ) -> Result<CommitReceipt, CommitError<Self::Error>> {
        let parent_write = commit
            .writes
            .iter()
            .find(|write| write.key.object_type.as_str() == PARENT)
            .expect("parent write is included in the atomic row commit");
        let key = parent_write.key.clone();
        let expected = parent_write.expected;
        let simulate_race = !self.raced;
        if simulate_race {
            self.raced = true;
        }
        let receipt = self.cell.commit_owned_rows(commit, proof)?;
        if simulate_race {
            if let Some(authorized) = &self.revoke_child_authorization {
                authorized.store(false, Ordering::SeqCst);
            }
            // The inner commit is the competing writer's winner. Surface the
            // CAS loss to the caller so it exercises replay-after-race.
            return Err(CommitError::Conflict {
                key,
                expected,
                current: FactVersion(expected.0.saturating_add(1)),
            });
        }
        Ok(receipt)
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
fn concurrent_identical_parent_row_call_replays_winner_after_cas_loss() {
    let definitions = types();
    let catalog = entity_catalog(&definitions);
    let mut host = RacingNative {
        cell: native_cell(),
        raced: false,
        revoke_child_authorization: None,
    };
    let action = action();
    let outcome = commit_action(
        &mut host,
        &catalog,
        &call(
            &action,
            "same-race-key",
            action_input(
                "0",
                &json!({ "title": "created once" }),
                &json!([add("race-row", "R", 3)]),
            ),
        ),
        &Allow,
    )
    .expect("the CAS loser replays the committed winner");

    assert!(
        matches!(outcome, ActionOutcome::Replayed { .. }),
        "{outcome:?}"
    );
    assert_eq!(host.cell.commits().expect("reads ledger").len(), 1);
    assert_parent(&host.cell, 1, "created once");
    assert_row(&host.cell, "race-row", 1, 1, "R", 3);
    assert_manifest(&host.cell, &["race-row"]);
}

struct RevocableChild(Arc<AtomicBool>);

impl ActionAuthorizer for RevocableChild {
    fn authorize(&self, request: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        if request.target.object_type.as_str() == CHILD && !self.0.load(Ordering::SeqCst) {
            Err(ActionDenial::Forbidden {
                reason: "child authorization was revoked after the winning commit".to_owned(),
            })
        } else {
            Ok(())
        }
    }
}

#[test]
fn concurrent_parent_row_replay_rechecks_current_child_authorization() {
    let definitions = types();
    let catalog = entity_catalog(&definitions);
    let authorized = Arc::new(AtomicBool::new(true));
    let mut host = RacingNative {
        cell: native_cell(),
        raced: false,
        revoke_child_authorization: Some(Arc::clone(&authorized)),
    };
    let action = action();
    let refusal = commit_action(
        &mut host,
        &catalog,
        &call(
            &action,
            "revoked-race-key",
            action_input(
                "0",
                &json!({ "title": "created once" }),
                &json!([add("race-row", "R", 3)]),
            ),
        ),
        &RevocableChild(authorized),
    )
    .expect_err("replay cannot disclose a row after its authorization is revoked");

    assert!(
        matches!(refusal, ActionRefusal::PermissionDenied { .. }),
        "{refusal:?}"
    );
    assert_eq!(host.cell.commits().expect("reads ledger").len(), 1);
}

#[test]
fn add_derivation_observes_the_assigned_dense_order() {
    let definitions = types_with_derived_idx();
    let catalog = entity_catalog(&definitions);
    let mut cell = native_cell_for(&definitions);
    let action = action_with_derived_idx();

    commit_action(
        &mut cell,
        &catalog,
        &call(
            &action,
            "derive-added-index",
            action_input(
                "0",
                &json!({ "title": "initial" }),
                &json!([add("row-a", "A", 1)]),
            ),
        ),
        &Allow,
    )
    .expect("creates a row whose derive reads its assigned index");

    assert_row(&cell, "row-a", 1, 1, "A", 1);
    assert_derived_idx(&cell, "row-a", 1);
}

#[test]
fn add_derivation_observes_exact_integer_property_default_before_final_validation() {
    let definitions = types_with_row_defaults(false);
    let catalog = entity_catalog(&definitions);
    let mut cell = native_cell_for(&definitions);
    let action = action_with_derived_idx();

    commit_action(
        &mut cell,
        &catalog,
        &call(
            &action,
            "derive-from-defaulted-integer",
            action_input(
                "0",
                &json!({ "title": "defaulted" }),
                &json!([{
                    "collection": COLLECTION,
                    "op": "add",
                    "rowId": "row-default",
                    "expectedVersion": "0",
                    "values": { "label": "default quantity" }
                }]),
            ),
        ),
        &Allow,
    )
    .expect("materializes defaults before expressions, then validates final row");

    assert_row(&cell, "row-default", 1, 1, "default quantity", 5);
    assert_derived_idx(&cell, "row-default", 1);
}

#[test]
fn row_guard_observes_defaulted_initial_lifecycle_with_derived_fields_omitted() {
    let definitions = types_with_row_defaults(true);
    let catalog = entity_catalog(&definitions);
    let mut cell = native_cell_for(&definitions);
    let action = action_with_lifecycle_default_guard();

    commit_action(
        &mut cell,
        &catalog,
        &call(
            &action,
            "guard-defaulted-initial-lifecycle",
            action_input(
                "0",
                &json!({ "title": "initial lifecycle" }),
                &json!([{
                    "collection": COLLECTION,
                    "op": "add",
                    "rowId": "row-lifecycle-default",
                    "expectedVersion": "0",
                    "values": { "label": "draft row", "quantity": 3 }
                }]),
            ),
        ),
        &Allow,
    )
    .expect("guard sees lifecycle default and final validation sees derived values");

    let row = cell
        .load(&row_key("row-lifecycle-default"))
        .expect("loads lifecycle row");
    let values = row.values.expect("row exists");
    assert_eq!(
        values.get("status"),
        Some(&JsonValue::String("draft".to_owned()))
    );
    assert_eq!(values.get("double_quantity"), Some(&integer(6)));
    assert_eq!(values.get("derived_idx"), Some(&integer(1)));
}

#[test]
fn update_and_reorder_derivation_observe_the_final_index_once() {
    let definitions = types_with_derived_idx();
    let catalog = entity_catalog(&definitions);
    let mut cell = native_cell_for(&definitions);
    let action = action_with_derived_idx();
    seed_rows(&mut cell, &catalog, &action);

    let operations = json!([
        {
            "collection": COLLECTION,
            "op": "update",
            "rowId": "row-a",
            "expectedVersion": "1",
            "values": { "quantity": 5 }
        },
        {
            "collection": COLLECTION,
            "op": "reorder",
            "rowIds": [
                { "rowId": "row-b", "expectedVersion": "1" },
                { "rowId": "row-a", "expectedVersion": "1" }
            ]
        }
    ]);
    commit_action(
        &mut cell,
        &catalog,
        &call(
            &action,
            "derive-updated-reordered-index",
            action_input("1", &json!({ "title": "reordered" }), &operations),
        ),
        &Allow,
    )
    .expect("updates and reorders with final-index derivations");

    assert_row(&cell, "row-a", 2, 2, "A", 5);
    assert_derived_idx(&cell, "row-a", 2);
    assert_row(&cell, "row-b", 2, 1, "B", 2);
    assert_derived_idx(&cell, "row-b", 1);
}

#[test]
fn untouched_row_derives_again_when_another_operation_changes_its_index() {
    let definitions = types_with_derived_idx();
    let catalog = entity_catalog(&definitions);
    let mut cell = native_cell_for(&definitions);
    let action = action_with_derived_idx();
    seed_rows(&mut cell, &catalog, &action);

    let remove_first = json!([{
        "collection": COLLECTION,
        "op": "remove",
        "rowId": "row-a",
        "expectedVersion": "1"
    }]);
    commit_action(
        &mut cell,
        &catalog,
        &call(
            &action,
            "derive-shifted-untouched-index",
            action_input("1", &json!({ "title": "removed first" }), &remove_first),
        ),
        &Allow,
    )
    .expect("removes the first row and re-derives the shifted untouched row");

    assert_eq!(
        cell.load(&row_key("row-a"))
            .expect("removed row tombstone")
            .values,
        None
    );
    assert_row(&cell, "row-b", 2, 1, "B", 2);
    assert_derived_idx(&cell, "row-b", 1);
    assert_manifest(&cell, &["row-b"]);
}

#[test]
fn maximum_valid_parent_key_uses_a_bounded_manifest_key() {
    let definitions = types();
    let catalog = entity_catalog(&definitions);
    let mut cell = native_cell();
    let action = action();
    let parent_id = "p".repeat(norn_ir::identifier_limits::MAX_OBJECT_KEY_BYTES);
    let parent = FactKey::new(PARENT, parent_id.clone());
    let manifest = owned_rows_manifest_key(&parent, COLLECTION).expect("manifest key derives");
    assert!(manifest.key.len() <= norn_ir::identifier_limits::MAX_OBJECT_KEY_BYTES);

    let mut input = action_input(
        "0",
        &json!({ "title": "large identity" }),
        &json!([add("row-large-parent", "line", 1)]),
    );
    input["parentId"] = json!(parent_id);
    commit_action(
        &mut cell,
        &catalog,
        &call(&action, "large-parent-key", input),
        &Allow,
    )
    .expect("maximum valid parent identity commits with its manifest");

    let stored_manifest = cell
        .load(&manifest)
        .expect("loads bounded manifest key")
        .values
        .expect("manifest is present");
    assert_eq!(
        stored_manifest.get("parentKey"),
        Some(&JsonValue::String(parent_id))
    );
}

#[test]
fn byte_budget_includes_loaded_parent_and_child_value_snapshots() {
    let definitions = types();
    let catalog = entity_catalog(&definitions);
    let mut cell = native_cell();
    let action = action();
    let output_limits = EnvelopeLimits {
        max_output_bytes: 2 * 1024 * 1024,
        ..EnvelopeLimits::V1
    };
    let initial = "p".repeat(110 * 1024);
    commit_action_with_limits(
        &mut cell,
        &catalog,
        &call(
            &action,
            "large-state-seed",
            action_input(
                "0",
                &json!({ "title": initial }),
                &json!([add("large-row", &initial, 1)]),
            ),
        ),
        &Allow,
        &output_limits,
    )
    .expect("explicit output limit admits the large seed fixtures");

    let replacement = "q".repeat(110 * 1024);
    let update = json!([{
        "collection": COLLECTION,
        "op": "update",
        "rowId": "large-row",
        "expectedVersion": "1",
        "values": { "label": replacement }
    }]);
    let refusal = commit_action_with_limits(
        &mut cell,
        &catalog,
        &call(
            &action,
            "large-state-update",
            action_input("1", &json!({}), &update),
        ),
        &Allow,
        &output_limits,
    )
    .expect_err("loaded parent and child plus input exceed the bounded slice bytes");
    assert!(!refusal.code().is_empty());
    assert_eq!(cell.commits().expect("reads ledger").len(), 1);
    assert_parent(&cell, 1, &initial);
    assert_row(&cell, "large-row", 1, 1, &initial, 1);
    assert_manifest(&cell, &["large-row"]);
    assert_eq!(
        cell.load(&FactKey::new(
            ACTION_CALL_TYPE,
            "test.order.edit/large-state-update"
        ))
        .expect("failed action record remains absent"),
        FactState::default()
    );
}

#[test]
fn byte_budget_includes_loaded_owned_row_link_values() {
    let action = action();
    let output_limits = EnvelopeLimits {
        max_output_bytes: 2 * 1024 * 1024,
        ..EnvelopeLimits::V1
    };

    for (payload_size, should_commit) in [(240 * 1024, true), (260 * 1024, false)] {
        let payload = "x".repeat(payload_size);
        let definitions = types_with_link_payload(&payload);
        let catalog = entity_catalog(&definitions);
        let mut cell = native_cell_for(&definitions);

        commit_action_with_limits(
            &mut cell,
            &catalog,
            &call(
                &action,
                &format!("link-budget-seed-{payload_size}"),
                action_input(
                    "0",
                    &json!({ "title": "initial" }),
                    &json!([add("row-a", "initial", 1)]),
                ),
            ),
            &Allow,
            &output_limits,
        )
        .expect("the seed writes the link value under the output envelope");

        let link_key = LinkKey::new(parent_key(), COLLECTION, row_key("row-a"));
        let link_before = cell.load_link(&link_key).expect("loads owned row link");
        assert_eq!(
            link_before
                .values
                .as_ref()
                .and_then(|values| values.get("tracking")),
            Some(&JsonValue::String(payload))
        );

        let update = json!([{
            "collection": COLLECTION,
            "op": "update",
            "rowId": "row-a",
            "expectedVersion": "1",
            "values": { "label": "updated" }
        }]);
        let update_result = commit_action_with_limits(
            &mut cell,
            &catalog,
            &call(
                &action,
                &format!("link-budget-update-{payload_size}"),
                action_input("1", &json!({}), &update),
            ),
            &Allow,
            &output_limits,
        );

        if should_commit {
            update_result.expect("a link value within the bounded slice bytes is admitted");
            assert_eq!(cell.commits().expect("reads ledger").len(), 2);
            assert_parent(&cell, 2, "initial");
            assert_row(&cell, "row-a", 2, 1, "updated", 1);
            assert_manifest(&cell, &["row-a"]);
            assert_eq!(
                cell.load_link(&link_key).expect("link remains readable"),
                link_before
            );
        } else {
            let refusal = update_result
                .expect_err("an oversized loaded link value exceeds the bounded slice bytes");
            assert!(matches!(
                refusal,
                ActionRefusal::Decision(norn_kernel::DecisionRefusal::InvalidOutput { reason })
                    if reason == "owned-row state, links, manifests, and action input snapshot exceed the bounded slice bytes"
            ));
            assert_eq!(cell.commits().expect("reads ledger").len(), 1);
            assert_parent(&cell, 1, "initial");
            assert_row(&cell, "row-a", 1, 1, "initial", 1);
            assert_manifest(&cell, &["row-a"]);
            assert_eq!(
                cell.load_link(&link_key).expect("link remains unchanged"),
                link_before
            );
            assert_eq!(
                cell.load(&FactKey::new(
                    ACTION_CALL_TYPE,
                    format!("test.order.edit/link-budget-update-{payload_size}")
                ))
                .expect("failed action record remains absent"),
                FactState::default()
            );
        }
    }
}

#[test]
fn failed_row_guard_aborts_parent_rows_links_and_ledger_together() {
    let definitions = types();
    let catalog = entity_catalog(&definitions);
    let mut cell = native_cell();
    let action = action();
    let blocked = json!({
        "collection": COLLECTION,
        "op": "add",
        "rowId": "blocked-row",
        "expectedVersion": "0",
        "values": { "label": "blocked", "quantity": 0 }
    });

    let refusal = commit_action(
        &mut cell,
        &catalog,
        &call(
            &action,
            "guard-failure",
            action_input(
                "0",
                &json!({ "title": "must roll back" }),
                &json!([add("good-row", "valid", 1), blocked]),
            ),
        ),
        &Allow,
    )
    .expect_err("the second row fails its trusted guard");
    assert!(!refusal.code().is_empty());
    assert!(cell.commits().expect("reads ledger").is_empty());
    assert_eq!(
        cell.load(&parent_key()).expect("parent remains absent"),
        FactState::default()
    );
    assert_eq!(
        cell.load(&row_key("good-row"))
            .expect("first row remains absent"),
        FactState::default()
    );
    assert_eq!(
        cell.load(&row_key("blocked-row"))
            .expect("blocked row remains absent"),
        FactState::default()
    );
    assert_eq!(
        cell.load(&manifest_key()).expect("manifest remains absent"),
        FactState::default()
    );
    let action_record_key = FactKey::new(ACTION_CALL_TYPE, "test.order.edit/guard-failure");
    assert_eq!(
        cell.load(&action_record_key)
            .expect("ledger receipt remains absent"),
        FactState::default()
    );
    assert_eq!(
        cell.load_link(&LinkKey::new(parent_key(), COLLECTION, row_key("good-row")))
            .expect("link remains absent"),
        LinkState::default()
    );
}

struct DenyChildLabel(&'static str);

impl ActionAuthorizer for DenyChildLabel {
    fn authorize(&self, request: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        let is_denied_child = request.target.object_type.as_str() == CHILD
            && request.target_values.and_then(|values| values.get("label"))
                == Some(&JsonValue::String(self.0.to_owned()));
        if is_denied_child {
            Err(ActionDenial::Forbidden {
                reason: "child label is denied by policy".to_owned(),
            })
        } else {
            Ok(())
        }
    }
}

#[test]
fn authorization_denial_on_one_proposed_child_aborts_the_whole_commit() {
    // Guard-failing and guard-passing denied proposals have the same refusal.
    for quantity in [0, 2] {
        let definitions = types();
        let catalog = entity_catalog(&definitions);
        let mut cell = native_cell();
        let action = action();
        let denied = json!({
            "collection": COLLECTION,
            "op": "add",
            "rowId": "denied-row",
            "expectedVersion": "0",
            "values": { "label": "denied", "quantity": quantity }
        });

        let refusal = commit_action(
            &mut cell,
            &catalog,
            &call(
                &action,
                "child-policy-denial",
                action_input(
                    "0",
                    &json!({ "title": "must roll back" }),
                    &json!([add("allowed-row", "allowed", 1), denied]),
                ),
            ),
            &DenyChildLabel("denied"),
        )
        .expect_err("the authorizer denies one child's proposed state");
        assert!(matches!(refusal, ActionRefusal::PermissionDenied { .. }));
        assert!(cell.commits().expect("reads ledger").is_empty());
        assert_eq!(
            cell.load(&parent_key()).expect("parent remains absent"),
            FactState::default()
        );
        for row_id in ["allowed-row", "denied-row"] {
            assert_eq!(
                cell.load(&row_key(row_id)).expect("child remains absent"),
                FactState::default()
            );
            assert_eq!(
                cell.load_link(&LinkKey::new(parent_key(), COLLECTION, row_key(row_id)))
                    .expect("child link remains absent"),
                LinkState::default()
            );
        }
        assert_eq!(
            cell.load(&manifest_key()).expect("manifest remains absent"),
            FactState::default()
        );
        assert_eq!(
            cell.load(&FactKey::new(
                ACTION_CALL_TYPE,
                "test.order.edit/child-policy-denial"
            ))
            .expect("action ledger receipt remains absent"),
            FactState::default()
        );
    }
}

#[test]
fn stale_parent_and_row_versions_refuse_without_partial_writes() {
    let definitions = types();
    let catalog = entity_catalog(&definitions);
    let mut cell = native_cell();
    let action = action();
    seed_rows(&mut cell, &catalog, &action);

    let update = json!([{
        "collection": COLLECTION,
        "op": "update",
        "rowId": "row-a",
        "expectedVersion": "1",
        "values": { "label": "A updated", "quantity": 3 }
    }]);
    commit_action(
        &mut cell,
        &catalog,
        &call(
            &action,
            "update-row",
            action_input("1", &json!({ "title": "updated" }), &update),
        ),
        &Allow,
    )
    .expect("updates parent and row");
    assert_parent(&cell, 2, "updated");
    assert_row(&cell, "row-a", 2, 1, "A updated", 3);

    let before = cell.commits().expect("reads ledger").len();
    let stale_parent = commit_action(
        &mut cell,
        &catalog,
        &call(
            &action,
            "stale-parent",
            action_input("1", &json!({ "title": "stale parent" }), &json!([])),
        ),
        &Allow,
    )
    .expect_err("the parent CAS is stale");
    assert!(!stale_parent.code().is_empty());
    assert_eq!(cell.commits().expect("reads ledger").len(), before);
    assert_parent(&cell, 2, "updated");

    let stale_row = json!([{
        "collection": COLLECTION,
        "op": "update",
        "rowId": "row-a",
        "expectedVersion": "1",
        "values": { "label": "must not land", "quantity": 4 }
    }]);
    let refusal = commit_action(
        &mut cell,
        &catalog,
        &call(
            &action,
            "stale-row",
            action_input("2", &json!({ "title": "row CAS refused" }), &stale_row),
        ),
        &Allow,
    )
    .expect_err("the row CAS is stale");
    assert!(!refusal.code().is_empty());
    assert_eq!(cell.commits().expect("reads ledger").len(), before);
    assert_parent(&cell, 2, "updated");
    assert_row(&cell, "row-a", 2, 1, "A updated", 3);
}

#[test]
fn row_only_edit_advances_parent_cas_and_exact_retry_replays_its_receipt() {
    let definitions = types();
    let catalog = entity_catalog(&definitions);
    let mut cell = native_cell();
    let action = action();
    seed_rows(&mut cell, &catalog, &action);

    let first_input = action_input(
        "1",
        &json!({}),
        &json!([{
            "collection": COLLECTION,
            "op": "update",
            "rowId": "row-a",
            "expectedVersion": "1",
            "values": { "label": "A row-only", "quantity": 1 }
        }]),
    );
    let first = commit_action(
        &mut cell,
        &catalog,
        &call(&action, "row-only-edit", first_input.clone()),
        &Allow,
    )
    .expect("row-only edit commits with its parent CAS");
    let ActionOutcome::Committed { record, .. } = first else {
        panic!("expected initial row-only commit, got {first:?}");
    };
    let recorded_operations = serde_json::to_value(&record)
        .expect("serializes committed action receipt")["rowOperations"]
        .clone();

    assert_eq!(cell.commits().expect("reads ledger").len(), 2);
    assert_parent(&cell, 2, "initial");
    assert_row(&cell, "row-a", 2, 1, "A row-only", 1);

    let stale_other_row = json!([{
        "collection": COLLECTION,
        "op": "update",
        "rowId": "row-b",
        "expectedVersion": "1",
        "values": { "label": "B from stale snapshot", "quantity": 2 }
    }]);
    let refusal = commit_action(
        &mut cell,
        &catalog,
        &call(
            &action,
            "other-editor-old-parent",
            action_input("1", &json!({}), &stale_other_row),
        ),
        &Allow,
    )
    .expect_err("the row-only commit advanced the parent version");
    assert!(!refusal.code().is_empty());
    assert_eq!(cell.commits().expect("reads ledger").len(), 2);
    assert_parent(&cell, 2, "initial");
    assert_row(&cell, "row-b", 1, 2, "B", 2);

    let replayed = commit_action(
        &mut cell,
        &catalog,
        &call(&action, "row-only-edit", first_input),
        &Allow,
    )
    .expect("the exact row-only call replays");
    let ActionOutcome::Replayed { record } = replayed else {
        panic!("expected exact retry to replay, got {replayed:?}");
    };
    assert_eq!(
        serde_json::to_value(&record).expect("serializes replayed receipt")["rowOperations"],
        recorded_operations,
        "the replay returns the same durable row-operation receipt"
    );
    assert_eq!(cell.commits().expect("reads ledger").len(), 2);
    assert_parent(&cell, 2, "initial");
    assert_row(&cell, "row-a", 2, 1, "A row-only", 1);
    assert_row(&cell, "row-b", 1, 2, "B", 2);
}

#[test]
fn update_guard_checks_derived_proposed_row_and_refuses_zero_quantity() {
    let definitions = types();
    let catalog = entity_catalog(&definitions);
    let mut cell = native_cell();
    let action = action();
    seed_rows(&mut cell, &catalog, &action);

    let invalid_update = json!([{
        "collection": COLLECTION,
        "op": "update",
        "rowId": "row-a",
        "expectedVersion": "1",
        "values": { "label": "must roll back", "quantity": 0 }
    }]);
    let refusal = commit_action(
        &mut cell,
        &catalog,
        &call(
            &action,
            "zero-quantity-update",
            action_input("1", &json!({ "title": "must roll back" }), &invalid_update),
        ),
        &Allow,
    )
    .expect_err("the trusted row guard sees the proposed zero quantity");
    assert!(!refusal.code().is_empty());

    assert_eq!(cell.commits().expect("reads ledger").len(), 1);
    assert_parent(&cell, 1, "initial");
    assert_row(&cell, "row-a", 1, 1, "A", 1);
    assert_manifest(&cell, &["row-a", "row-b"]);
    assert!(
        cell.load_link(&LinkKey::new(parent_key(), COLLECTION, row_key("row-a")))
            .expect("existing child link survives refusal")
            .exists()
    );
    assert_eq!(
        cell.load(&FactKey::new(
            ACTION_CALL_TYPE,
            "test.order.edit/zero-quantity-update"
        ))
        .expect("refused action has no receipt"),
        FactState::default()
    );
}

#[test]
fn failed_row_guard_during_parent_cascade_delete_rolls_back_everything() {
    let definitions = types();
    let catalog = entity_catalog(&definitions);
    let mut cell = native_cell();
    let create_action = action();
    seed_rows(&mut cell, &catalog, &create_action);

    let parent_before = cell.load(&parent_key()).expect("loads seeded parent");
    let first_row_before = cell.load(&row_key("row-a")).expect("loads first row");
    let second_row_before = cell.load(&row_key("row-b")).expect("loads second row");
    let manifest_before = cell.load(&manifest_key()).expect("loads manifest");
    let link_a = LinkKey::new(parent_key(), COLLECTION, row_key("row-a"));
    let link_b = LinkKey::new(parent_key(), COLLECTION, row_key("row-b"));
    let first_link_before = cell.load_link(&link_a).expect("loads first link");
    let second_link_before = cell.load_link(&link_b).expect("loads second link");
    let commits_before = cell.commits().expect("snapshots ledger");

    let restrictive_delete =
        action_with_row_guard("test.order.restrictive_delete", "self.quantity < 0");
    let refusal = commit_action(
        &mut cell,
        &catalog,
        &call(
            &restrictive_delete,
            "cascade-row-guard-failure",
            json!({
                "parentId": "order-1",
                "parentValues": {},
                "parentVersion": "1",
                "rowOps": [],
                "deleteParent": true
            }),
        ),
        &Allow,
    )
    .expect_err("the trusted row condition rejects the rows being cascaded");
    assert!(
        refusal.to_string().contains("self.quantity < 0"),
        "the refusal should identify the failing child-row guard: {refusal}"
    );

    assert_eq!(
        cell.load(&parent_key()).expect("parent unchanged"),
        parent_before
    );
    assert_eq!(
        cell.load(&row_key("row-a")).expect("first row unchanged"),
        first_row_before
    );
    assert_eq!(
        cell.load(&row_key("row-b")).expect("second row unchanged"),
        second_row_before
    );
    assert_eq!(
        cell.load(&manifest_key()).expect("manifest unchanged"),
        manifest_before
    );
    assert_eq!(
        cell.load_link(&link_a).expect("first link unchanged"),
        first_link_before
    );
    assert_eq!(
        cell.load_link(&link_b).expect("second link unchanged"),
        second_link_before
    );
    assert_eq!(cell.commits().expect("ledger unchanged"), commits_before);
    assert_eq!(
        cell.load(&FactKey::new(
            ACTION_CALL_TYPE,
            "test.order.restrictive_delete/cascade-row-guard-failure"
        ))
        .expect("failed action has no receipt"),
        FactState::default()
    );
}

#[test]
fn collection_bound_and_cascade_delete_apply_to_all_rows() {
    let definitions = types();
    let catalog = entity_catalog(&definitions);
    let mut cell = native_cell();
    let action = action();
    let too_many = commit_action(
        &mut cell,
        &catalog,
        &call(
            &action,
            "over-bound",
            action_input(
                "0",
                &json!({ "title": "too many" }),
                &json!([
                    add("row-a", "A", 1),
                    add("row-b", "B", 2),
                    add("row-c", "C", 3)
                ]),
            ),
        ),
        &Allow,
    )
    .expect_err("maxRows rejects a third child");
    assert!(!too_many.code().is_empty());
    assert!(cell.commits().expect("reads ledger").is_empty());
    assert_eq!(
        cell.load(&parent_key()).expect("parent absent"),
        FactState::default()
    );
    assert_eq!(
        cell.load(&row_key("row-a")).expect("rows absent"),
        FactState::default()
    );

    seed_rows(&mut cell, &catalog, &action);
    let deleted = commit_action(
        &mut cell,
        &catalog,
        &call(
            &action,
            "cascade-delete",
            json!({
                "parentId": "order-1",
                "parentValues": {},
                "parentVersion": "1",
                "rowOps": [],
                "deleteParent": true
            }),
        ),
        &Allow,
    )
    .expect("deletes parent and all declared owned rows");
    assert!(matches!(deleted, ActionOutcome::Committed { .. }));
    assert_eq!(cell.commits().expect("reads ledger").len(), 2);
    assert_eq!(
        cell.load(&parent_key()).expect("parent tombstone").values,
        None
    );
    for row_id in ["row-a", "row-b"] {
        assert_eq!(
            cell.load(&row_key(row_id)).expect("row tombstone").values,
            None
        );
        assert!(
            !cell
                .load_link(&LinkKey::new(parent_key(), COLLECTION, row_key(row_id)))
                .expect("link removed")
                .exists()
        );
    }
}

#[test]
fn cascade_event_envelope_counts_ledger_events_not_physical_writes() {
    let definitions = types_with_max_rows(100);
    let catalog = entity_catalog(&definitions);
    let mut cell = native_cell_for(&definitions);
    let action = action();
    let additions = (0..15)
        .map(|index| add(&format!("row-{index}"), "child", 1))
        .collect::<Vec<_>>();
    commit_action(
        &mut cell,
        &catalog,
        &call(
            &action,
            "seed-fifteen-rows",
            action_input("0", &json!({ "title": "fifteen" }), &json!(additions)),
        ),
        &Allow,
    )
    .expect("parent plus fifteen rows fits the default event envelope");

    let outcome = commit_action(
        &mut cell,
        &catalog,
        &call(
            &action,
            "delete-fifteen-rows",
            json!({
                "parentId": "order-1",
                "parentValues": {},
                "parentVersion": "1",
                "rowOps": [],
                "deleteParent": true
            }),
        ),
        &Allow,
    )
    .expect("the 16 fact events fit the default max_events of 32");
    assert!(matches!(outcome, ActionOutcome::Committed { .. }));
    assert_eq!(cell.commits().expect("reads ledger").len(), 2);
    for index in 0..15 {
        let row = cell
            .load(&row_key(&format!("row-{index}")))
            .expect("row tombstone is readable");
        assert_eq!(row.values, None);
    }
}

#[test]
fn large_owned_snapshot_does_not_consume_the_event_envelope() {
    let definitions = types_with_max_rows(100);
    let catalog = entity_catalog(&definitions);
    let mut cell = native_cell_for(&definitions);
    let action = action();
    let additions = (0..31)
        .map(|index| add(&format!("row-{index}"), "child", 1))
        .collect::<Vec<_>>();

    commit_action(
        &mut cell,
        &catalog,
        &call(
            &action,
            "seed-thirty-one-rows",
            action_input("0", &json!({ "title": "large" }), &json!(additions)),
        ),
        &Allow,
    )
    .expect("parent and 31 rows fit 32 output events");

    commit_action(
        &mut cell,
        &catalog,
        &call(
            &action,
            "update-one-of-thirty-one-rows",
            action_input(
                "1",
                &json!({}),
                &json!([{
                    "collection": COLLECTION,
                    "op": "update",
                    "rowId": "row-0",
                    "expectedVersion": "1",
                    "values": { "label": "updated" }
                }]),
            ),
        ),
        &Allow,
    )
    .expect("one-row edit fits 32 output events");
    assert_eq!(cell.commits().expect("reads ledger").len(), 2);
    assert_parent(&cell, 2, "large");
    assert_row(&cell, "row-0", 2, 1, "updated", 1);
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one native-store scenario verifies sparse reorder and event rollback"
)]
fn full_reorder_snapshot_can_exceed_event_limit_when_only_two_rows_move() {
    let definitions = types_with_max_rows(100);
    let catalog = entity_catalog(&definitions);
    let mut cell = native_cell_for(&definitions);
    let action = action();
    let additions = (0..41)
        .map(|index| add(&format!("row-{index}"), "child", 1))
        .collect::<Vec<_>>();
    let seed_limits = EnvelopeLimits {
        max_events: 64,
        ..EnvelopeLimits::V1
    };
    commit_action_with_limits(
        &mut cell,
        &catalog,
        &call(
            &action,
            "seed-forty-one-rows",
            action_input("0", &json!({ "title": "large" }), &json!(additions)),
        ),
        &Allow,
        &seed_limits,
    )
    .expect("the wider seed envelope admits 41 rows");

    commit_action(
        &mut cell,
        &catalog,
        &call(
            &action,
            "update-one-of-forty-one-rows",
            action_input(
                "1",
                &json!({}),
                &json!([{
                    "collection": COLLECTION,
                    "op": "update",
                    "rowId": "row-0",
                    "expectedVersion": "1",
                    "values": { "label": "updated" }
                }]),
            ),
        ),
        &Allow,
    )
    .expect("one-row update fits default event limits over a 41-row snapshot");

    let mut current_order = (0..41)
        .map(|index| format!("row-{index}"))
        .collect::<Vec<_>>();
    current_order.swap(0, 40);
    let version_for = |row_id: &str| match row_id {
        "row-0" => "2",
        _ => "1",
    };
    let two_position_reorder = current_order
        .iter()
        .map(|row_id| json!({ "rowId": row_id, "expectedVersion": version_for(row_id) }))
        .collect::<Vec<_>>();
    commit_action(
        &mut cell,
        &catalog,
        &call(
            &action,
            "swap-two-of-forty-one-rows",
            action_input(
                "2",
                &json!({}),
                &json!([{
                    "collection": COLLECTION,
                    "op": "reorder",
                    "rowIds": two_position_reorder
                }]),
            ),
        ),
        &Allow,
    )
    .expect("the full 41-row order is valid when only two rows change position");

    let swapped_versions = |row_id: &str| match row_id {
        "row-0" => "3",
        "row-40" => "2",
        _ => "1",
    };
    let mut full_shift_order = current_order.clone();
    full_shift_order.rotate_left(1);
    let full_shift = full_shift_order
        .iter()
        .map(|row_id| json!({ "rowId": row_id, "expectedVersion": swapped_versions(row_id) }))
        .collect::<Vec<_>>();
    let before_failed_reorder = cell.commits().expect("reads ledger");
    let refusal = commit_action(
        &mut cell,
        &catalog,
        &call(
            &action,
            "shift-all-forty-one-rows",
            action_input(
                "3",
                &json!({}),
                &json!([{
                    "collection": COLLECTION,
                    "op": "reorder",
                    "rowIds": full_shift
                }]),
            ),
        ),
        &Allow,
    )
    .expect_err("a reorder that emits over 32 fact events is refused");
    assert_eq!(
        refusal.code(),
        "decision.envelope_exceeded",
        "the rejection is the emitted-event cap, not an invalid row snapshot"
    );
    assert_eq!(
        cell.commits().expect("reads ledger after refusal"),
        before_failed_reorder,
        "the over-envelope reorder is atomic"
    );
    assert_parent(&cell, 3, "large");
    assert_manifest(
        &cell,
        &current_order.iter().map(String::as_str).collect::<Vec<_>>(),
    );
    for (offset, row_id) in current_order.iter().enumerate() {
        let expected_idx = offset + 1;
        let expected_version = match row_id.as_str() {
            "row-0" => 3,
            "row-40" => 2,
            _ => 1,
        };
        assert_row(
            &cell,
            row_id,
            expected_version,
            i64::try_from(expected_idx).expect("index fits i64"),
            if row_id == "row-0" {
                "updated"
            } else {
                "child"
            },
            1,
        );
    }
}

#[test]
fn mixed_row_operations_apply_original_versions_once_and_commit_final_order() {
    let definitions = types();
    let catalog = entity_catalog(&definitions);
    let mut cell = native_cell();
    let action = action();
    seed_rows(&mut cell, &catalog, &action);

    let operations = json!([
        {
            "collection": COLLECTION,
            "op": "update",
            "rowId": "row-a",
            "expectedVersion": "1",
            "values": { "label": "A updated", "quantity": 3 }
        },
        {
            "collection": COLLECTION,
            "op": "remove",
            "rowId": "row-b",
            "expectedVersion": "1"
        },
        add("row-c", "C", 4),
        {
            "collection": COLLECTION,
            "op": "reorder",
            "rowIds": [
                { "rowId": "row-c", "expectedVersion": "0" },
                { "rowId": "row-a", "expectedVersion": "1" }
            ]
        }
    ]);
    let committed = commit_action(
        &mut cell,
        &catalog,
        &call(
            &action,
            "mixed-row-ops",
            action_input("1", &json!({ "title": "mixed update" }), &operations),
        ),
        &Allow,
    )
    .expect("updates, removes, adds, and reorders in one parent commit");
    let ActionOutcome::Committed { receipt, record } = committed else {
        panic!("expected a new mixed-row commit, got {committed:?}");
    };

    assert_eq!(cell.commits().expect("reads ledger").len(), 2);
    assert_parent(&cell, 2, "mixed update");
    assert_eq!(
        cell.load(&row_key("row-b"))
            .expect("removed row tombstone")
            .values,
        None
    );
    assert_row(&cell, "row-c", 1, 1, "C", 4);
    assert_row(&cell, "row-a", 2, 2, "A updated", 3);
    assert_manifest(&cell, &["row-c", "row-a"]);
    assert!(
        receipt
            .written
            .iter()
            .any(|write| write.key == row_key("row-a"))
    );
    assert!(
        receipt
            .written
            .iter()
            .any(|write| write.key == row_key("row-b"))
    );
    assert!(
        receipt
            .written
            .iter()
            .any(|write| write.key == row_key("row-c"))
    );
    assert_eq!(
        receipt.linked.len(),
        2,
        "the added and removed child links share the parent commit"
    );
    // The updated and reordered row-a retains its membership link; only
    // the added row-c and removed row-b write links in this commit.
    let added_link = LinkKey::new(parent_key(), COLLECTION, row_key("row-c"));
    let removed_link = LinkKey::new(parent_key(), COLLECTION, row_key("row-b"));
    let linked: BTreeSet<_> = receipt.linked.iter().map(|write| &write.key).collect();
    assert_eq!(linked, BTreeSet::from([&added_link, &removed_link]));
    assert_eq!(
        serde_json::to_value(&record).expect("serializes action receipt")["rowOperations"]
            .as_array()
            .map(Vec::len),
        Some(4)
    );
    for (row_id, expected) in [("row-a", true), ("row-b", false), ("row-c", true)] {
        let link = LinkKey::new(parent_key(), COLLECTION, row_key(row_id));
        assert_eq!(
            cell.load_link(&link)
                .expect("loads final child link")
                .exists(),
            expected
        );
    }
}

const GUARD: &str = "test.RowGuard";
const FLAG: &str = "test.RowFlag";

fn preparation_types() -> Vec<norn_ir::ObjectTypeDef> {
    let mut definitions = types();
    definitions.extend(
        serde_json::from_value::<Vec<norn_ir::ObjectTypeDef>>(json!([
            {
                "name": GUARD,
                "properties": [{"id": "active", "type": {"kind": "boolean"}}],
                "links": [{"id": "flags", "target": FLAG,
                    "cardinality": {"to": "many", "from": "one"}}]
            },
            {"name": FLAG, "properties": [{"id": "label", "type": {"kind": "text"}}]}
        ]))
        .expect("precondition dependencies decode"),
    );
    definitions
}

fn prepared_rows_action() -> ActionRoot {
    let mut payload = serde_json::to_value(action()).expect("action serializes");
    payload["inputSchema"]["properties"]["guardId"] = json!({"type": "string"});
    payload["derive"] = json!([
        {"field": "parentId", "expression": "trim(input.parentId)"},
        {"field": "guardId", "expression": "\"guard-1\"", "inputRequired": false}
    ]);
    payload["slice"] = json!([
        {"name": "guard", "objectType": GUARD,
            "key": {"property": {"from": "input", "property": "guardId"}}},
        {"name": "flags", "objectType": FLAG,
            "key": {"link": {"from": "guard", "link": "flags"}}}
    ]);
    payload["preconditions"] = json!(["slice.guard.active", "len(slice.flags) == 0"]);
    payload["effectProgram"]["collections"][0]["derive"]["double_quantity"] =
        json!("input.parentId == \"order-1\" ? self.quantity * 2 : 0");
    serde_json::from_value(payload).expect("prepared owned-row action decodes")
}

fn prepared_rows_input() -> Value {
    let mut input = action_input(
        "0",
        &json!({"title": "prepared"}),
        &json!([add("row-a", "A", 2)]),
    );
    input["parentId"] = json!(" order-1 ");
    input
}

fn guard_key() -> FactKey {
    FactKey::new(GUARD, "guard-1")
}

fn seed_guard(cell: &mut NativeCell, active: bool) {
    cell.commit(CellCommit {
        writes: vec![norn_kernel::FactWrite {
            key: guard_key(),
            expected: FactVersion::ABSENT,
            mutation: norn_kernel::FactMutation::Put {
                values: serde_json::from_value(json!({"active": active})).expect("guard values"),
            },
        }],
        ..CellCommit::default()
    })
    .expect("seed guard");
}

struct PreparedAllow;

impl ActionAuthorizer for PreparedAllow {
    fn authorize(&self, request: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        assert_eq!(
            request.input.get("parentId"),
            Some(&JsonValue::String("order-1".to_owned()))
        );
        assert_eq!(
            request.input.get("guardId"),
            Some(&JsonValue::String("guard-1".to_owned()))
        );
        Ok(())
    }

    fn authorize_read(
        &self,
        request: &norn_kernel::ActionReadAuthzRequest<'_>,
    ) -> Result<(), ActionDenial> {
        assert_eq!(
            request.input.get("guardId"),
            Some(&JsonValue::String("guard-1".to_owned()))
        );
        Ok(())
    }

    fn authorize_read_type(
        &self,
        _: &norn_kernel::ActionReadTypeAuthzRequest<'_>,
    ) -> Result<(), ActionDenial> {
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum PreparationInterference {
    ExtraRead,
    ExtraEvent,
    DropFactRead,
    DropLinkRead,
    FactRace,
    LinkRace,
}

struct PreparedRowsHost {
    cell: NativeCell,
    interference: PreparationInterference,
}

impl CellHost for PreparedRowsHost {
    type Event = FactEvent;
    type Command = JsonObject;
    type Input = JsonObject;
    type Error = norn_cell::CellError;

    fn load(&self, key: &FactKey) -> Result<FactState, Self::Error> {
        self.cell.load(key)
    }

    fn load_bounded(
        &self,
        key: &FactKey,
        max_bytes: u64,
    ) -> Result<Option<norn_kernel::BoundedFact>, Self::Error> {
        self.cell.load_bounded(key, max_bytes)
    }

    fn load_link(&self, key: &LinkKey) -> Result<LinkState, Self::Error> {
        self.cell.load_link(key)
    }

    fn read_link_targets(
        &self,
        source: &FactKey,
        link: &str,
        max_targets: usize,
        max_key_bytes: u64,
    ) -> Result<Option<Vec<norn_kernel::LinkReadTarget>>, Self::Error> {
        self.cell
            .read_link_targets(source, link, max_targets, max_key_bytes)
    }

    fn commit(
        &mut self,
        commit: CellCommit<FactEvent, JsonObject, JsonObject>,
    ) -> Result<CommitReceipt, CommitError<Self::Error>> {
        self.cell.commit(commit)
    }

    #[expect(
        clippy::unwrap_in_result,
        reason = "This test fixture must panic on failed prerequisites rather than simulate a recoverable domain refusal"
    )]
    fn commit_owned_rows(
        &mut self,
        mut commit: CellCommit<FactEvent, JsonObject, JsonObject>,
        proof: OwnedRowsCommitProof,
    ) -> Result<CommitReceipt, CommitError<Self::Error>> {
        assert!(commit.reads.iter().any(|read| read.key == guard_key()));
        assert_eq!(commit.link_reads.len(), 1);
        match self.interference {
            // Model a perimeter adapter appending a ledger audit event.
            PreparationInterference::ExtraEvent => {
                commit.events.push(FactEvent::CommandEnqueued {
                    command_id: "perimeter-audit".to_owned(),
                    generation: 0,
                    idempotency_key: "perimeter-audit".to_owned(),
                    action_call: "audited-rows".to_owned(),
                    payload: JsonObject::new(),
                });
            }
            PreparationInterference::ExtraRead => commit.reads.push(norn_kernel::FactExpectation {
                key: FactKey::new(FLAG, "absent-visibility-token"),
                expected: FactVersion::ABSENT,
            }),
            PreparationInterference::DropFactRead => {
                commit.reads.retain(|read| read.key != guard_key());
            }
            PreparationInterference::DropLinkRead => commit.link_reads.clear(),
            PreparationInterference::FactRace => {
                self.cell
                    .commit(CellCommit {
                        writes: vec![norn_kernel::FactWrite {
                            key: guard_key(),
                            expected: FactVersion(1),
                            mutation: norn_kernel::FactMutation::Put {
                                values: serde_json::from_value(json!({"active": false}))
                                    .expect("guard values"),
                            },
                        }],
                        ..CellCommit::default()
                    })
                    .expect("concurrent fact update");
            }
            PreparationInterference::LinkRace => {
                let flag = FactKey::new(FLAG, "flag-1");
                self.cell
                    .commit(CellCommit {
                        writes: vec![norn_kernel::FactWrite {
                            key: flag.clone(),
                            expected: FactVersion::ABSENT,
                            mutation: norn_kernel::FactMutation::Put {
                                values: serde_json::from_value(json!({"label": "blocks rows"}))
                                    .expect("flag values"),
                            },
                        }],
                        links: vec![norn_kernel::LinkWrite {
                            key: LinkKey::new(guard_key(), "flags", flag),
                            expected: FactVersion::ABSENT,
                            mutation: norn_kernel::LinkMutation::Add {
                                values: JsonObject::new(),
                            },
                        }],
                        ..CellCommit::default()
                    })
                    .expect("concurrent link insertion");
            }
        }
        self.cell.commit_owned_rows(commit, proof)
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
fn owned_rows_use_prepared_input_and_persist_precondition_evidence() {
    let definitions = preparation_types();
    let catalog = TypeCatalog::new(&definitions);
    let action = prepared_rows_action();
    let mut cell = native_cell_for(&definitions);
    seed_guard(&mut cell, true);
    let mut host = PreparedRowsHost {
        cell,
        interference: PreparationInterference::ExtraRead,
    };
    let request = call(&action, "prepared-rows", prepared_rows_input());
    let preparation = norn_kernel::prepare_action_input_for_call::<norn_cell::CellError>(
        &action,
        &request.input,
        request.action_artifact_id,
        request.action_version,
    )
    .expect("prepare action input once");
    let outcome = norn_kernel::commit_action_with_preparation(
        &mut host,
        &catalog,
        &request,
        &PreparedAllow,
        preparation,
    )
    .expect("prepared owned-row action commits with additional perimeter read");
    let ActionOutcome::Committed { record, .. } = outcome else {
        panic!("expected commit");
    };
    assert_eq!(record.input, request.input);
    assert_eq!(
        record
            .effective_input
            .as_ref()
            .and_then(|input| input.get("parentId")),
        Some(&JsonValue::String("order-1".to_owned()))
    );
    assert_eq!(record.derived_values.len(), 2);
    assert_eq!(record.precondition_outcomes.len(), 2);
    assert!(
        record
            .precondition_outcomes
            .iter()
            .all(|outcome| outcome.satisfied)
    );
    let stored = host
        .cell
        .load(&FactKey::new(
            ACTION_CALL_TYPE,
            "test.order.edit/prepared-rows",
        ))
        .expect("load record");
    assert_eq!(
        norn_kernel::ActionCallRecord::from_values(&stored.values.expect("record exists")),
        Some(record)
    );
    assert_parent(&host.cell, 1, "prepared");
    assert_row(&host.cell, "row-a", 1, 1, "A", 2);
    assert!(matches!(
        commit_action(&mut host, &catalog, &request, &PreparedAllow).expect("raw retry replays"),
        ActionOutcome::Replayed { .. }
    ));
    let mut altered = request.clone();
    altered.input.insert(
        "parentId".to_owned(),
        JsonValue::String("order-1".to_owned()),
    );
    assert!(
        matches!(
            commit_action(&mut host, &catalog, &altered, &PreparedAllow),
            Err(ActionRefusal::IdempotencyConflict { .. })
        ),
        "equal effective input cannot replace the raw idempotency binding"
    );
}

#[test]
fn owned_rows_refuse_false_or_stale_preconditions_without_partial_writes() {
    let definitions = preparation_types();
    let catalog = TypeCatalog::new(&definitions);
    let action = prepared_rows_action();
    let mut cell = native_cell_for(&definitions);
    seed_guard(&mut cell, false);
    let request = call(&action, "guarded-rows", prepared_rows_input());
    assert!(matches!(
        commit_action(&mut cell, &catalog, &request, &PreparedAllow),
        Err(ActionRefusal::PreconditionFailed { .. })
    ));
    assert!(!cell.load(&parent_key()).expect("load parent").exists());
    assert_eq!(cell.commits().expect("ledger").len(), 1);

    for interference in [
        PreparationInterference::FactRace,
        PreparationInterference::LinkRace,
    ] {
        let mut cell = native_cell_for(&definitions);
        seed_guard(&mut cell, true);
        let mut host = PreparedRowsHost { cell, interference };
        let refusal = commit_action(&mut host, &catalog, &request, &PreparedAllow)
            .expect_err("raced read refuses");
        assert!(
            matches!(refusal, ActionRefusal::StalePreconditionRead { ref key, ref link }
            if *key == guard_key() && link.is_some() == matches!(interference, PreparationInterference::LinkRace))
        );
        assert!(!host.cell.load(&parent_key()).expect("load parent").exists());
        assert!(
            !host
                .cell
                .load(&row_key("row-a"))
                .expect("load child")
                .exists()
        );
        assert!(
            !host
                .cell
                .load(&manifest_key())
                .expect("load manifest")
                .exists()
        );
        assert!(
            !host
                .cell
                .load(&FactKey::new(
                    ACTION_CALL_TYPE,
                    "test.order.edit/guarded-rows"
                ))
                .expect("load record")
                .exists()
        );
        assert_eq!(
            host.cell.commits().expect("ledger").len(),
            2,
            "only the seed and competing mutation commit"
        );
    }
}

#[test]
fn owned_row_proof_rejects_removed_precondition_fact_or_link_reads() {
    let definitions = preparation_types();
    let catalog = TypeCatalog::new(&definitions);
    let action = prepared_rows_action();
    for interference in [
        PreparationInterference::DropFactRead,
        PreparationInterference::DropLinkRead,
    ] {
        let mut cell = native_cell_for(&definitions);
        seed_guard(&mut cell, true);
        let mut host = PreparedRowsHost { cell, interference };
        let refusal = commit_action(
            &mut host,
            &catalog,
            &call(&action, "tampered-rows", prepared_rows_input()),
            &PreparedAllow,
        )
        .expect_err("proof rejects dropped read");
        assert!(matches!(refusal,
            ActionRefusal::Decision(norn_kernel::DecisionRefusal::InvalidOutput { ref reason })
                if reason.contains("protected owned-row transaction")));

        assert!(!host.cell.load(&parent_key()).expect("load parent").exists());
        assert!(
            !host
                .cell
                .load(&row_key("row-a"))
                .expect("load child")
                .exists()
        );
        assert_eq!(host.cell.commits().expect("ledger").len(), 1);
    }
}

#[test]
fn optional_parent_delete_marker_accepts_omission_or_false_and_rejects_malformed_values() {
    let definitions = types();
    let catalog = entity_catalog(&definitions);
    let action = action();
    for marker in [None, Some(json!(false))] {
        let mut cell = native_cell_for(&definitions);
        let mut input = action_input(
            "0",
            &json!({"title": "retained"}),
            &json!([add("row-a", "A", 2)]),
        );
        if let Some(marker) = marker {
            input["deleteParent"] = marker;
        }
        commit_action(
            &mut cell,
            &catalog,
            &call(&action, "optional-delete", input),
            &Allow,
        )
        .expect("omission and false both retain the parent");
        assert_parent(&cell, 1, "retained");
        assert_row(&cell, "row-a", 1, 1, "A", 2);
        assert_eq!(cell.commits().expect("ledger").len(), 1);
    }
    for marker in [Value::Null, json!("false"), json!(0), json!([]), json!({})] {
        let mut cell = native_cell_for(&definitions);
        let mut input = action_input(
            "0",
            &json!({"title": "refused"}),
            &json!([add("row-a", "A", 2)]),
        );
        input["deleteParent"] = marker;
        let refusal = commit_action(
            &mut cell,
            &catalog,
            &call(&action, "malformed-delete", input),
            &Allow,
        )
        .expect_err("present non-Boolean delete markers are refused");
        assert!(matches!(refusal, ActionRefusal::InvalidInput { .. }));
        assert!(!cell.load(&parent_key()).expect("load parent").exists());
        assert!(!cell.load(&row_key("row-a")).expect("load row").exists());
        assert!(!cell.load(&manifest_key()).expect("load manifest").exists());
        assert!(cell.commits().expect("ledger").is_empty());
    }
}

#[derive(Default)]
struct BudgetReadAuthorizer {
    guard_reads: std::cell::Cell<usize>,
    flag_reads: std::cell::Cell<usize>,
}

impl ActionAuthorizer for BudgetReadAuthorizer {
    fn authorize(&self, request: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        PreparedAllow.authorize(request)
    }

    fn authorize_read(
        &self,
        request: &norn_kernel::ActionReadAuthzRequest<'_>,
    ) -> Result<(), ActionDenial> {
        match request.target.object_type.as_str() {
            GUARD => self
                .guard_reads
                .set(self.guard_reads.get().saturating_add(1)),
            FLAG => self.flag_reads.set(self.flag_reads.get().saturating_add(1)),
            _ => {}
        }
        PreparedAllow.authorize_read(request)
    }

    fn authorize_read_type(
        &self,
        request: &norn_kernel::ActionReadTypeAuthzRequest<'_>,
    ) -> Result<(), ActionDenial> {
        PreparedAllow.authorize_read_type(request)
    }
}

fn seed_budget_parent(cell: &mut NativeCell, catalog: &TypeCatalog<'_>, title: &str) {
    commit_action_with_limits(
        cell,
        catalog,
        &call(
            &action(),
            "budget-parent-seed",
            action_input(
                "0",
                &json!({"title": title}),
                &json!([add("row-a", "A", 2)]),
            ),
        ),
        &Allow,
        &EnvelopeLimits {
            max_output_bytes: 2 * 1024 * 1024,
            ..EnvelopeLimits::V1
        },
    )
    .expect("seed owned snapshot within its existing budget");
}

fn assert_budget_refusal_is_atomic(cell: &NativeCell, title: &str, ledger_len: usize) {
    assert_parent(cell, 1, title);
    assert_row(cell, "row-a", 1, 1, "A", 2);
    assert_manifest(cell, &["row-a"]);
    assert_eq!(cell.commits().expect("ledger").len(), ledger_len);
    assert!(
        !cell
            .load(&FactKey::new(
                ACTION_CALL_TYPE,
                "test.order.edit/shared-budget"
            ))
            .expect("read refused call record")
            .exists()
    );
}

#[test]
fn owned_rows_and_declared_dependencies_share_the_canonical_byte_budget() {
    let mut definitions = preparation_types();
    definitions
        .iter_mut()
        .find(|definition| definition.name.as_str() == GUARD)
        .expect("guard type")
        .properties
        .push(
            serde_json::from_value(json!({"id": "notes", "type": {"kind": "text"}}))
                .expect("notes property"),
        );
    let catalog = TypeCatalog::new(&definitions);
    let action = prepared_rows_action();
    let title = "p".repeat(144 * 1024);
    for (dependency_bytes, accepted) in [(72 * 1024, true), (144 * 1024, false)] {
        let mut cell = native_cell_for(&definitions);
        seed_budget_parent(&mut cell, &catalog, &title);
        cell.commit(CellCommit {
            writes: vec![norn_kernel::FactWrite {
                key: guard_key(),
                expected: FactVersion::ABSENT,
                mutation: norn_kernel::FactMutation::Put {
                    values: serde_json::from_value(
                        json!({"active": true, "notes": "g".repeat(dependency_bytes)}),
                    )
                    .expect("guard values"),
                },
            }],
            ..CellCommit::default()
        })
        .expect("seed dependency independently below 256 KiB");
        let authorizer = BudgetReadAuthorizer::default();
        let outcome = commit_action(
            &mut cell,
            &catalog,
            &call(
                &action,
                "shared-budget",
                action_input("1", &json!({"title": "updated"}), &json!([])),
            ),
            &authorizer,
        );
        if accepted {
            outcome.expect("combined snapshot below 256 KiB commits");
            assert_parent(&cell, 2, "updated");
            assert_eq!(authorizer.guard_reads.get(), 1);
        } else {
            assert!(
                matches!(outcome, Err(ActionRefusal::TargetNotFound)),
                "bounded host refuses the dependency before decoding beyond the remaining bytes"
            );
            assert_eq!(authorizer.guard_reads.get(), 0);
            assert_eq!(authorizer.flag_reads.get(), 0);
            assert_budget_refusal_is_atomic(&cell, &title, 2);
        }
    }
}

fn seed_budget_flags(cell: &mut NativeCell, count: u32) {
    let mut commit = CellCommit::default();
    for index in 0..count {
        let key = FactKey::new(FLAG, format!("flag-{index:04}"));
        commit.writes.push(norn_kernel::FactWrite {
            key: key.clone(),
            expected: FactVersion::ABSENT,
            mutation: norn_kernel::FactMutation::Put {
                values: serde_json::from_value(json!({"label": "flag"})).expect("flag values"),
            },
        });
        commit.links.push(norn_kernel::LinkWrite {
            key: LinkKey::new(guard_key(), "flags", key),
            expected: FactVersion::ABSENT,
            mutation: norn_kernel::LinkMutation::Add {
                values: JsonObject::new(),
            },
        });
    }
    cell.commit(commit)
        .expect("seed independently bounded declared link slice");
}

#[test]
fn owned_rows_and_declared_dependencies_share_the_record_budget() {
    let definitions = preparation_types();
    let catalog = TypeCatalog::new(&definitions);
    let mut action = prepared_rows_action();
    action.preconditions =
        serde_json::from_value(json!(["slice.guard.active"])).expect("guard precondition");
    let maximum = norn_ir::ResourceLimitProfile::norn_limits_v1()
        .slice
        .max_records;
    // The existing owned-row budget charges the manifest and current child;
    // the declared single guard consumes one more record before its flag list.
    let remaining = maximum
        .checked_sub(3)
        .expect("profile admits owned snapshot and guard");
    for (flags, accepted) in [(remaining, true), (remaining.saturating_add(1), false)] {
        let mut cell = native_cell_for(&definitions);
        seed_budget_parent(&mut cell, &catalog, "original");
        seed_guard(&mut cell, true);
        seed_budget_flags(&mut cell, flags);
        assert!(
            flags.saturating_add(1) <= maximum,
            "the declared slice fits independently"
        );
        let authorizer = BudgetReadAuthorizer::default();
        let outcome = commit_action(
            &mut cell,
            &catalog,
            &call(
                &action,
                "shared-budget",
                action_input("1", &json!({"title": "updated"}), &json!([])),
            ),
            &authorizer,
        );
        assert_eq!(authorizer.guard_reads.get(), 1);
        if accepted {
            outcome.expect("combined record count exactly at the cap commits");
            assert_parent(&cell, 2, "updated");
            assert_eq!(
                authorizer.flag_reads.get(),
                usize::try_from(flags).expect("small flag count")
            );
        } else {
            assert!(matches!(
                outcome,
                Err(ActionRefusal::SliceUnavailable { .. })
            ));
            assert_eq!(
                authorizer.flag_reads.get(),
                0,
                "adjacency count refuses before loading any flag values"
            );
            assert_budget_refusal_is_atomic(&cell, "original", 3);
        }
    }
}

#[test]
fn required_stored_row_field_must_be_supplied_or_derived_by_the_action() {
    let definitions = types_with_derived_idx();
    let catalog = entity_catalog(&definitions);
    let mut cell = native_cell_for(&definitions);
    let ordinary = action();
    let input = action_input(
        "0",
        &json!({"title": "required derivation"}),
        &json!([add("row-a", "A", 2)]),
    );
    let refusal = commit_action(
        &mut cell,
        &catalog,
        &call(&ordinary, "missing-required-derivation", input.clone()),
        &Allow,
    )
    .expect_err("a required stored property cannot be omitted without a derivation");
    assert!(
        matches!(
            refusal,
            ActionRefusal::Decision(norn_kernel::DecisionRefusal::Host(
                norn_cell::CellError::MissingValue { ref owner, ref property }
            )) if owner == CHILD && property == "derived_idx"
        ),
        "{refusal:?}"
    );
    assert!(!cell.load(&parent_key()).expect("load parent").exists());
    assert!(!cell.load(&row_key("row-a")).expect("load row").exists());
    assert!(!cell.load(&manifest_key()).expect("load manifest").exists());
    assert!(
        !cell
            .load(&FactKey::new(
                ACTION_CALL_TYPE,
                "test.order.edit/missing-required-derivation"
            ))
            .expect("load refused action record")
            .exists()
    );
    assert!(cell.commits().expect("ledger").is_empty());

    let derived = action_with_derived_idx();
    commit_action(
        &mut cell,
        &catalog,
        &call(&derived, "supplied-required-derivation", input),
        &Allow,
    )
    .expect("the action supplies the required stored property before storage validation");
    assert_parent(&cell, 1, "required derivation");
    assert_row(&cell, "row-a", 1, 1, "A", 2);
    assert_derived_idx(&cell, "row-a", 1);
    assert_eq!(cell.commits().expect("ledger").len(), 1);
}

#[test]
fn unprepared_parent_row_receipts_obey_default_and_custom_output_limits() {
    for (max_bytes, label) in [
        (
            EnvelopeLimits::default().max_output_bytes,
            "x".repeat(40_000),
        ),
        (1, "small".to_owned()),
    ] {
        let definitions = types();
        let catalog = entity_catalog(&definitions);
        let mut cell = native_cell();
        let action = action();
        assert!(
            action.derive.is_empty() && action.slice.is_empty() && action.preconditions.is_empty()
        );
        let refusal = commit_action_with_limits(
            &mut cell,
            &catalog,
            &call(
                &action,
                "oversized-receipt",
                action_input(
                    "0",
                    &json!({ "title": "new" }),
                    &json!([add("row-a", &label, 1)]),
                ),
            ),
            &Allow,
            &EnvelopeLimits {
                max_output_bytes: max_bytes,
                ..EnvelopeLimits::default()
            },
        )
        .expect_err("receipt exceeds the output envelope");
        assert!(
            matches!(refusal, ActionRefusal::InputBoundExceeded { subject, .. } if subject == "action call receipt")
        );
        assert!(cell.commits().expect("ledger").is_empty());
        assert_eq!(
            cell.load(&parent_key()).expect("parent absent"),
            FactState::default()
        );
        assert_eq!(
            cell.load(&row_key("row-a")).expect("child absent"),
            FactState::default()
        );
        assert_eq!(
            cell.load(&manifest_key()).expect("manifest absent"),
            FactState::default()
        );
    }
}

#[test]
fn perimeter_events_obey_the_sealed_owned_row_envelope() {
    let mut definitions = preparation_types();
    let mut parent = serde_json::to_value(definitions.remove(0)).expect("parent serializes");
    parent["ownedCollections"][0]["maxRows"] = json!(100);
    definitions.insert(0, serde_json::from_value(parent).expect("bounded parent"));
    let catalog = TypeCatalog::new(&definitions);
    let action = prepared_rows_action();
    for (fact_events, max_events, accepted) in
        [(2, 2, false), (2, 3, true), (32, 32, false), (32, 33, true)]
    {
        let mut cell = native_cell_for(&definitions);
        seed_guard(&mut cell, true);
        let mut host = PreparedRowsHost {
            cell,
            interference: PreparationInterference::ExtraEvent,
        };
        let additions = (0..fact_events - 1)
            .map(|index| add(&format!("row-{index}"), "child", 1))
            .collect::<Vec<_>>();
        let request = call(
            &action,
            "audited-rows",
            action_input("0", &json!({"title": "audited"}), &json!(additions)),
        );
        let outcome = commit_action_with_limits(
            &mut host,
            &catalog,
            &request,
            &PreparedAllow,
            &EnvelopeLimits {
                max_events,
                ..EnvelopeLimits::V1
            },
        );
        if accepted {
            outcome.expect("the augmented commit fits its configured envelope");
            assert_eq!(host.cell.commits().expect("ledger").len(), 2);
            assert_parent(&host.cell, 1, "audited");
        } else {
            assert!(matches!(outcome, Err(ActionRefusal::Decision(
                norn_kernel::DecisionRefusal::InvalidOutput { reason }
            )) if reason == format!("commit emitted {} events, limit is {max_events}", fact_events + 1)));
            assert_eq!(host.cell.commits().expect("ledger").len(), 1);
            assert!(!host.cell.load(&parent_key()).expect("parent").exists());
            assert!(!host.cell.load(&manifest_key()).expect("manifest").exists());
            for index in 0..fact_events - 1 {
                let key = row_key(&format!("row-{index}"));
                assert!(!host.cell.load(&key).expect("child").exists());
                assert!(
                    !host
                        .cell
                        .load_link(&LinkKey::new(parent_key(), COLLECTION, key))
                        .expect("membership")
                        .exists()
                );
            }
            assert!(
                !host
                    .cell
                    .load(&FactKey::new(
                        ACTION_CALL_TYPE,
                        "test.order.edit/audited-rows"
                    ))
                    .expect("receipt")
                    .exists()
            );
        }
    }
}

#[test]
fn caller_cannot_choose_an_owned_child_lifecycle_initial_state() {
    let mut definitions = types_with_row_defaults(true);
    let mut child = serde_json::to_value(definitions.remove(1)).expect("child serializes");
    child["properties"]
        .as_array_mut()
        .expect("properties")
        .iter_mut()
        .find(|property| property["id"] == "status")
        .expect("status")["type"]["values"] =
        json!([{ "value": "draft" }, { "value": "alternate" }, { "value": "done" }]);
    child["lifecycles"][0]["initial"] = json!(["draft", "alternate"]);
    definitions.insert(
        1,
        serde_json::from_value(child).expect("multi-initial child"),
    );
    let catalog = entity_catalog(&definitions);
    let action = action_with_derived_idx();
    for state in ["draft", "alternate", "done"] {
        let mut cell = native_cell_for(&definitions);
        let mut operation = add("row-a", "child", 1);
        operation["values"]["status"] = json!(state);
        let outcome = commit_action(
            &mut cell,
            &catalog,
            &call(
                &action,
                "raw-lifecycle",
                action_input("0", &json!({"title": "refused"}), &json!([operation])),
            ),
            &Allow,
        );
        assert!(matches!(outcome, Err(ActionRefusal::InvalidInput { field }) if field == "rowOps"));
        assert!(cell.commits().expect("ledger").is_empty());
        assert!(!cell.load(&parent_key()).expect("parent").exists());
        assert!(!cell.load(&row_key("row-a")).expect("row").exists());
        assert!(!cell.load(&manifest_key()).expect("manifest").exists());
    }
    let mut cell = native_cell_for(&definitions);
    commit_action(
        &mut cell,
        &catalog,
        &call(
            &action,
            "default-lifecycle",
            action_input(
                "0",
                &json!({"title": "defaulted"}),
                &json!([add("row-a", "child", 1)]),
            ),
        ),
        &Allow,
    )
    .expect("omitting lifecycle state uses its default");
    assert_eq!(
        cell.load(&row_key("row-a"))
            .expect("row")
            .values
            .expect("values")
            .get("status"),
        Some(&JsonValue::String("draft".to_owned()))
    );
}
