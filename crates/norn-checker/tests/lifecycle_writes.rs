#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    missing_docs,
    reason = "integration-test crate"
)]

//! Lifecycle writes (T3-LIFE-2, #2673): the canonical fact change of a
//! decide graph (T2), and decision and action writes of lifecycle properties
//! against the effective lifecycles of an artifact set (T3). The negative
//! artifact-set cases are `fixtures/ir/artifact-set/lifecycle-*.case.json`
//! (run by `tests/artifact_set.rs`); these tests cover every fact-change
//! shape rule, the effective (inherited, narrowed) machines, and where
//! string enums are admitted.

use std::collections::BTreeSet;

use norn_checker::lifecycle_writes::codes;
use norn_checker::lifecycles::codes::UNDECLARED_WRITE;
use norn_checker::{
    Diagnostic, EffectiveCatalog, EntityArtifactSetContext, check_action_firing,
    check_artifact_json, check_artifact_set,
};
use norn_ir::{ActionEffectProgram, ActionRoot, ArtifactEnvelope, JsonObject, PropertyTypeName};
use serde_json::{Value, json};

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures");
const ITEMS: &str = "/payload/outputSchema/properties/facts/items";

fn read_json(path: &str) -> Value {
    serde_json::from_str(
        &std::fs::read_to_string(format!("{FIXTURES}/{path}"))
            .unwrap_or_else(|error| panic!("{path} should be readable: {error}")),
    )
    .unwrap_or_else(|error| panic!("{path} should parse: {error}"))
}

fn approval_rule() -> Value {
    read_json("rule-ir-v2/valid/purchase-order-approval.rule.json")
}

fn purchase_order_entity() -> Value {
    read_json("ir/entity-v2/artifact-set/commerce-purchase-order.entity.json")
}

/// The narrowing subtype of the purchase order as an entity artifact.
fn rush_purchase_order_entity() -> Value {
    let mut entity = purchase_order_entity();
    entity["artifactId"] = json!("commerce.RushPurchaseOrder.entity");
    entity["name"] = json!("Rush purchase order");
    entity["payload"]["objectType"] =
        read_json("ir/entity-v2/object-types/rush-purchase-order.object-type.json");
    entity
}

fn envelope(value: Value) -> ArtifactEnvelope {
    serde_json::from_value(value).expect("envelope decodes")
}

fn pairs(findings: Vec<Diagnostic>) -> Vec<(String, String)> {
    findings
        .into_iter()
        .map(|Diagnostic { code, span, .. }| {
            (
                code.as_str().to_owned(),
                span.map(|span| span.pointer).unwrap_or_default(),
            )
        })
        .collect()
}

fn set_findings(artifacts: Vec<Value>) -> Vec<(String, String)> {
    let artifacts: Vec<ArtifactEnvelope> = artifacts.into_iter().map(envelope).collect();
    let registry = BTreeSet::<PropertyTypeName>::new();
    pairs(check_artifact_set(
        &artifacts,
        &EntityArtifactSetContext {
            registry: &registry,
            base: &[],
            tenant: None,
        },
    ))
}

/// The T2 findings of the approval rule after `mutate` edits its fact-change
/// item schema.
fn fact_change_findings(mutate: impl FnOnce(&mut Value)) -> Vec<(String, String)> {
    let mut rule = approval_rule();
    mutate(&mut rule["payload"]["outputSchema"]["properties"]["facts"]["items"]);
    pairs(check_artifact_json(&rule))
}

fn entity_with_mode() -> Value {
    let mut entity = purchase_order_entity();
    entity["payload"]["objectType"]["properties"]
        .as_array_mut()
        .expect("properties")
        .push(json!({
            "id": "mode",
            "type": { "kind": "enum", "values": [{ "value": "open" }, { "value": "closed" }] }
        }));
    entity
}

fn stored_write_rule(op: &str, property: &str, value_schema: Value, expression: &str) -> Value {
    let mut rule = approval_rule();
    let items = &mut rule["payload"]["outputSchema"]["properties"]["facts"]["items"];
    let write_member = if op == "patch" { "patch" } else { "value" };
    let write_schema = if op == "patch" {
        json!({
            "type": "object",
            "properties": { (property): value_schema },
            "required": [],
            "additionalProperties": false
        })
    } else {
        value_schema
    };
    let mut properties = json!({
        "op": { "type": "string", "enum": [op] },
        "objectType": { "type": "string", "enum": ["commerce.PurchaseOrder"] },
        "key": { "type": "string" },
        (write_member): write_schema
    });
    if op == "set" {
        properties["property"] = json!({ "type": "string", "enum": [property] });
    }
    items["properties"] = properties;
    items["required"] = if op == "patch" {
        json!(["op", "objectType", "key"])
    } else {
        json!(["op", "objectType", "key", "property", "value"])
    };
    items["additionalProperties"] = json!(false);
    let expressions = rule["payload"]["graph"]["nodes"][1]["content"]["expressions"]
        .as_array_mut()
        .expect("expressions");
    expressions[0]["value"] = json!(expression);
    rule
}

fn invalid(pointer: &str) -> Vec<(String, String)> {
    vec![(
        codes::INVALID_FACT_CHANGE.to_owned(),
        format!("{ITEMS}{pointer}"),
    )]
}

#[test]
fn lifecycle_fact_change_fixture_is_valid() {
    assert_eq!(pairs(check_artifact_json(&approval_rule())), Vec::new());
    assert_eq!(
        set_findings(vec![purchase_order_entity(), approval_rule()]),
        Vec::new()
    );
}

#[test]
fn stored_property_writes_check_patch_and_set_contracts() {
    let entity = entity_with_mode();
    let checked = |rule| set_findings(vec![entity.clone(), rule]);
    let write =
        |value_schema, expression| stored_write_rule("patch", "mode", value_schema, expression);
    let patch = "[{op: 'patch', objectType: 'commerce.PurchaseOrder', key: event.poNumber, patch: {mode: 'open'}}]";
    assert_eq!(
        checked(write(json!({ "type": "string" }), patch)),
        Vec::new()
    );

    let invalid_literal = "[{op: 'patch', objectType: 'commerce.PurchaseOrder', key: event.poNumber, patch: {mode: 'bogus'}}]";
    assert!(
        checked(write(json!({ "type": "string" }), invalid_literal))
            .iter()
            .any(|(code, _)| code == codes::INVALID_STORED_PROPERTY_WRITE)
    );

    // A broad schema can carry a computed string. Admission checks its
    // effective property contract at runtime, while exact schema enums are
    // rejected here when they admit an invalid enum value.
    let computed = "[{op: 'patch', objectType: 'commerce.PurchaseOrder', key: event.poNumber, patch: {mode: event.kind}}]";
    assert_eq!(
        checked(write(json!({ "type": "string" }), computed)),
        Vec::new()
    );
    assert!(
        checked(write(
            json!({ "type": "string", "enum": ["bogus"] }),
            computed
        ))
        .iter()
        .any(|(code, _)| code == codes::INVALID_STORED_PROPERTY_WRITE)
    );

    // An optional schema member is still writable and must be a property of
    // the specific type the literal patch targets.
    let mut unknown_field = write(json!({ "type": "string" }), patch);
    unknown_field["payload"]["outputSchema"]["properties"]["facts"]["items"]["properties"]["patch"]
        ["properties"]["missing"] = json!({ "type": "string" });
    assert!(
        checked(unknown_field)
            .iter()
            .any(|(code, _)| code == codes::INVALID_STORED_PROPERTY_WRITE)
    );

    let set = stored_write_rule(
        "set",
        "mode",
        json!({ "type": "string" }),
        "[{op: 'set', objectType: 'commerce.PurchaseOrder', key: event.poNumber, property: 'mode', value: 'bogus'}]",
    );
    assert!(
        checked(set)
            .iter()
            .any(|(code, _)| code == codes::INVALID_STORED_PROPERTY_WRITE)
    );
}

#[test]
fn a_literal_create_does_not_disable_computed_patch_contract_checks() {
    let mut rule = stored_write_rule(
        "patch",
        "missing",
        json!({ "type": "string" }),
        "[{op: 'create', objectType: 'commerce.PurchaseOrder', key: 'new', values: {}}, event]",
    );
    let properties =
        &mut rule["payload"]["outputSchema"]["properties"]["facts"]["items"]["properties"];
    properties["op"]["enum"] = json!(["create", "patch"]);
    properties["values"] = json!({
        "type": "object", "properties": {}, "required": [], "additionalProperties": false
    });
    let findings = set_findings(vec![entity_with_mode(), rule]);
    assert!(
        findings
            .iter()
            .any(|(code, _)| code == codes::INVALID_STORED_PROPERTY_WRITE),
        "literal create hid an unknown computed patch member: {findings:?}"
    );
}

#[test]
fn scalar_table_patch_and_set_values_obey_the_effective_enum_contract() {
    for op in ["patch", "set"] {
        for (literal, valid) in [("'open'", true), ("'bogus'", false)] {
            let mut rule = stored_write_rule(op, "mode", json!({ "type": "string" }), "[]");
            let value_field = if op == "patch" {
                "facts[0].patch.mode"
            } else {
                "facts[0].value"
            };
            let mut outputs = vec![
                json!({"id": "op", "field": "facts[0].op"}),
                json!({"id": "kind", "field": "facts[0].objectType"}),
                json!({"id": "key", "field": "facts[0].key"}),
                json!({"id": "value", "field": value_field}),
                json!({"id": "commands", "field": "commands"}),
                json!({"id": "timers", "field": "timers"}),
            ];
            let mut row = json!({
                "_id": "row", "condition": "true", "op": format!("'{op}'"),
                "kind": "'commerce.PurchaseOrder'", "key": "event.poNumber",
                "value": literal, "commands": "[]", "timers": "[]"
            });
            if op == "set" {
                outputs.push(json!({"id": "property", "field": "facts[0].property"}));
                row["property"] = json!("'mode'");
            }
            rule["payload"]["graph"]["nodes"][1] = json!({
                "id": "emit", "name": "emit", "type": "decisionTableNode",
                "content": {
                    "hitPolicy": "first", "inputs": [{"id": "condition"}],
                    "outputs": outputs, "rules": [row]
                }
            });
            let findings = set_findings(vec![entity_with_mode(), rule]);
            if valid {
                assert!(findings.is_empty(), "{op} {literal}: {findings:?}");
            } else {
                assert!(
                    findings
                        .iter()
                        .any(|(code, _)| code == codes::INVALID_STORED_PROPERTY_WRITE),
                    "{op} {literal}: {findings:?}"
                );
            }
        }
    }
}

#[test]
fn lifecycle_fact_change_schema_is_the_canonical_record() {
    // A member outside the fact change.
    assert_eq!(
        fact_change_findings(|items| items["properties"]["patches"] = json!({ "type": "string" })),
        invalid("/properties/patches")
    );
    // A required member missing, and one declared but optional.
    assert_eq!(
        fact_change_findings(|items| {
            items["properties"]
                .as_object_mut()
                .expect("object")
                .remove("key");
            items["required"] = json!(["op", "objectType"]);
        }),
        invalid("/properties")
    );
    assert_eq!(
        fact_change_findings(|items| items["required"] = json!(["op", "key"])),
        invalid("/required")
    );
    // `key` is a string; the enumerated members are string enums.
    assert_eq!(
        fact_change_findings(|items| items["properties"]["key"] = json!({ "type": "integer" })),
        invalid("/properties/key")
    );
    for member in ["objectType", "lifecycle", "transition"] {
        assert_eq!(
            fact_change_findings(|items| items["properties"][member] = json!({ "type": "string" })),
            invalid(&format!("/properties/{member}")),
            "{member}"
        );
    }
    // `values` is a record.
    assert_eq!(
        fact_change_findings(|items| items["properties"]["values"] = json!({ "type": "string" })),
        invalid("/properties/values")
    );
    // An unknown op.
    assert_eq!(
        fact_change_findings(|items| {
            items["properties"]["op"] =
                json!({ "type": "string", "enum": ["create", "upsert", "transition"] });
        }),
        invalid("/properties/op/enum/1")
    );
    // An emitted op without its members, and a member no emitted op carries.
    assert_eq!(
        fact_change_findings(|items| {
            items["properties"]["op"] =
                json!({ "type": "string", "enum": ["create", "transition", "set"] });
        }),
        [invalid("/properties"), invalid("/properties")].concat()
    );
    assert_eq!(
        fact_change_findings(|items| {
            items["properties"]["op"] = json!({ "type": "string", "enum": ["transition"] });
        }),
        invalid("/properties/values")
    );
}

#[test]
fn lifecycle_string_enums_are_admitted_in_rule_ir_schemas_only() {
    // A malformed enum in a RuleIR v2 schema is an invalid schema.
    for (bad, at) in [
        (json!([]), "/enum"),
        (json!(["create", "transition", "create"]), "/enum/2"),
        (json!(["create", 1, "transition"]), "/enum/1"),
    ] {
        assert_eq!(
            fact_change_findings(|items| {
                items["properties"]["op"] = json!({ "type": "string", "enum": bad });
            }),
            vec![(
                "rule.validation.invalid_schema".to_owned(),
                format!("{ITEMS}/properties/op{at}")
            )]
        );
    }
    // ActionIR keeps the enum-free subset its runtime input validation
    // implements.
    let mut action = read_json("ir/retail-reorder.action.json");
    action["payload"]["inputSchema"]["properties"]["skuId"] =
        json!({ "type": "string", "enum": ["a"] });
    assert_eq!(
        pairs(check_artifact_json(&action)),
        vec![(
            "action.validation.invalid_schema".to_owned(),
            "/payload/inputSchema/properties/skuId/enum".to_owned()
        )]
    );
}

#[test]
fn effective_lifecycles_are_the_declared_list_narrowed_along_inheritance() {
    let artifacts = vec![
        envelope(purchase_order_entity()),
        envelope(rush_purchase_order_entity()),
    ];
    let catalog = EffectiveCatalog::from_artifacts(&artifacts);
    let order = catalog
        .object_type("commerce.PurchaseOrder.entity")
        .expect("defined")
        .expect("available");
    let declared: Vec<norn_ir::Lifecycle> = serde_json::from_value(
        purchase_order_entity()["payload"]["objectType"]["lifecycles"].clone(),
    )
    .expect("lifecycles decode");
    // The checker's list is the declared list (descriptions aside), so the
    // diagram and the write checks render and enforce one machine.
    let effective: Vec<norn_ir::Lifecycle> = order
        .lifecycles
        .iter()
        .map(|lifecycle| lifecycle.lifecycle.clone())
        .collect();
    let without_descriptions = |lifecycles: Vec<norn_ir::Lifecycle>| {
        let mut lifecycles = lifecycles;
        for lifecycle in &mut lifecycles {
            lifecycle.description = None;
            for transition in &mut lifecycle.transitions {
                transition.description = None;
            }
        }
        lifecycles.sort_by(|left, right| left.property.cmp(&right.property));
        lifecycles
    };
    assert_eq!(effective, without_descriptions(declared));

    let rush = catalog
        .object_type("commerce.RushPurchaseOrder.entity")
        .expect("defined")
        .expect("available");
    let status = rush.lifecycle("status").expect("redeclared");
    assert_eq!(
        status.states,
        ["draft", "submitted", "approved", "received", "cancelled"]
    );
    let ids: Vec<&str> = status
        .lifecycle
        .transitions
        .iter()
        .map(|transition| transition.id.as_str())
        .collect();
    assert_eq!(ids, ["submit", "approve", "receive", "cancel"]);
    // Inherited unchanged.
    assert_eq!(
        rush.lifecycle("payment_status"),
        order.lifecycle("payment_status")
    );
}

#[test]
fn lifecycle_transitions_are_checked_against_the_effective_machine() {
    // `reject` is declared by the purchase order but dropped by the rush
    // subtype, so a decision that may fire it on either type is rejected for
    // the subtype only.
    let mut rule = approval_rule();
    let items = &mut rule["payload"]["outputSchema"]["properties"]["facts"]["items"]["properties"];
    items["objectType"]["enum"] = json!(["commerce.PurchaseOrder", "commerce.RushPurchaseOrder"]);
    items["transition"]["enum"] = json!(["submit", "approve", "reject"]);
    let mut rush = rush_purchase_order_entity();
    rush["dependencies"] = json!([{
        "artifactId": "commerce.PurchaseOrder.entity",
        "kind": "entity",
        "version": "0.1.0",
        "required": true
    }]);
    assert_eq!(
        set_findings(vec![purchase_order_entity(), rush, rule]),
        vec![(
            codes::UNKNOWN_TRANSITION.to_owned(),
            format!("{ITEMS}/properties/transition/enum/2")
        )]
    );
}

/// The rush subtype as an entity artifact that depends on the purchase order.
fn rush_with_dependency() -> Value {
    let mut rush = rush_purchase_order_entity();
    rush["dependencies"] = json!([{
        "artifactId": "commerce.PurchaseOrder.entity",
        "kind": "entity",
        "version": "0.1.0",
        "required": true
    }]);
    rush
}

/// A `fire_transition` action (T3-LIFE-3, #2867) on `object_kind`, defined
/// by `entity_artifact_id`: the submit-invoice fixture retargeted.
fn transition_action(
    object_kind: &str,
    entity_artifact_id: &str,
    lifecycle: &str,
    transition: &str,
) -> Value {
    let mut action = read_json("ir/commerce-sales-order-submit-invoice.action.json");
    action["artifactId"] = json!("commerce.purchase_order.transition.action");
    let payload = &mut action["payload"];
    payload["actionId"] = json!("commerce.purchase_order.transition");
    let schema = json!({
        "type": "object",
        "properties": { "po_number": { "type": "string" } },
        "required": ["po_number"],
        "additionalProperties": false
    });
    payload["inputSchema"] = schema.clone();
    payload["outputSchema"] = schema;
    payload["preconditions"] = json!(["len(po_number) > 0"]);
    payload["writes"] = json!([{ "entityKind": object_kind, "change": "modified" }]);
    payload["effectProgram"] = json!({
        "kind": "fire_transition",
        "objectIdField": "po_number",
        "objectKind": object_kind,
        "entityArtifactId": entity_artifact_id,
        "entityArtifactVersion": "0.1.0",
        "lifecycle": lifecycle,
        "transition": transition
    });
    action
}

fn series_purchase_order_entity() -> Value {
    let mut entity = purchase_order_entity();
    entity["payload"]["objectType"]["properties"]
        .as_array_mut()
        .expect("properties")
        .push(json!({ "id": "company", "type": { "kind": "text" } }));
    entity["payload"]["objectType"]["identity"][0]["namingSeries"] = json!({
        "pattern": "PO-{company}-{YYYY}{MM}{DD}-{#####}",
        "timezone": "America/New_York",
        "scopeFields": ["company"],
        "resetPolicy": "day",
        "gapPolicy": "gapless_within_commit"
    });
    entity
}

fn series_create_action(
    object_id_required: bool,
    scope_required: bool,
    write_mode: &Value,
) -> Value {
    let mut action = read_json("ir/commerce-sales-order-submit-invoice.action.json");
    action["artifactId"] = json!("commerce.purchase_order.create.action");
    action["name"] = json!("Create purchase order");
    let payload = &mut action["payload"];
    payload["actionId"] = json!("commerce.purchase_order.create");
    let required = match (object_id_required, scope_required) {
        (true, true) => json!(["po_number", "company"]),
        (true, false) => json!(["po_number"]),
        (false, true) => json!(["company"]),
        (false, false) => json!([]),
    };
    payload["inputSchema"] = json!({
        "type": "object",
        "properties": {
            "po_number": { "type": "string" },
            "company": { "type": "string" }
        },
        "required": required,
        "additionalProperties": false
    });
    payload["preconditions"] = json!([]);
    payload["writes"] = json!([
        { "entityKind": "commerce.PurchaseOrder", "change": "created" }
    ]);
    payload["effectProgram"] = json!({
        "kind": "upsert_input_object",
        "objectIdField": "po_number",
        "objectKind": "commerce.PurchaseOrder",
        "entityArtifactId": "commerce.PurchaseOrder.entity",
        "entityArtifactVersion": "0.1.0",
        "writeMode": write_mode
    });
    action
}

#[test]
fn action_transitions_are_checked_against_the_effective_machine() {
    let set = |action: Value| {
        set_findings(vec![
            purchase_order_entity(),
            rush_with_dependency(),
            action,
        ])
    };
    // `reject` is declared by the purchase order and dropped by the rush
    // subtype, which narrows its status lifecycle.
    assert_eq!(
        set(transition_action(
            "commerce.PurchaseOrder",
            "commerce.PurchaseOrder.entity",
            "status",
            "reject"
        )),
        Vec::new()
    );
    assert_eq!(
        set(transition_action(
            "commerce.RushPurchaseOrder",
            "commerce.RushPurchaseOrder.entity",
            "status",
            "reject"
        )),
        vec![(
            codes::UNKNOWN_TRANSITION.to_owned(),
            "/payload/effectProgram/transition".to_owned()
        )]
    );
    // `pay` is inherited unchanged: the subtype does not redeclare the
    // payment lifecycle.
    assert_eq!(
        set(transition_action(
            "commerce.RushPurchaseOrder",
            "commerce.RushPurchaseOrder.entity",
            "payment_status",
            "pay"
        )),
        Vec::new()
    );
}

#[test]
fn optional_create_id_requires_a_matching_series_and_required_scope_inputs() {
    let valid = series_create_action(false, true, &json!({ "kind": "create_only" }));
    assert_eq!(
        set_findings(vec![series_purchase_order_entity(), valid]),
        Vec::new()
    );

    let no_series = series_create_action(false, true, &json!({ "kind": "create_only" }));
    assert_eq!(
        set_findings(vec![purchase_order_entity(), no_series]),
        vec![(
            norn_checker::action_set::codes::NAMING_SERIES_TARGET_MISMATCH.to_owned(),
            "/payload/effectProgram/objectIdField".to_owned()
        )]
    );

    let missing_scope = series_create_action(false, false, &json!({ "kind": "create_only" }));
    assert_eq!(
        set_findings(vec![series_purchase_order_entity(), missing_scope]),
        vec![(
            norn_checker::action_set::codes::NAMING_SERIES_TARGET_MISMATCH.to_owned(),
            "/payload/inputSchema/properties/company".to_owned()
        )]
    );

    let manual_id_create = series_create_action(true, true, &json!({ "kind": "create_only" }));
    assert_eq!(
        set_findings(vec![series_purchase_order_entity(), manual_id_create]),
        vec![(
            norn_checker::action_set::codes::NAMING_SERIES_TARGET_MISMATCH.to_owned(),
            "/payload/effectProgram/objectIdField".to_owned()
        )]
    );

    let mut wrong_required_id = series_create_action(true, true, &json!({ "kind": "create_only" }));
    wrong_required_id["payload"]["effectProgram"]["objectIdField"] = json!("company");
    assert_eq!(
        set_findings(vec![series_purchase_order_entity(), wrong_required_id]),
        vec![(
            norn_checker::action_set::codes::NAMING_SERIES_TARGET_MISMATCH.to_owned(),
            "/payload/effectProgram/objectIdField".to_owned()
        )]
    );

    let manual_upsert = series_create_action(true, true, &json!({ "kind": "upsert" }));
    assert_eq!(
        set_findings(vec![series_purchase_order_entity(), manual_upsert]),
        vec![
            (
                norn_checker::action_set::codes::NAMING_SERIES_TARGET_MISMATCH.to_owned(),
                "/payload/effectProgram/writeMode".to_owned()
            ),
            (
                norn_checker::action_set::codes::UNDECLARED_EFFECT_WRITE.to_owned(),
                "/payload/effectProgram".to_owned()
            )
        ]
    );

    let mut wrong_upsert_id = series_create_action(true, true, &json!({ "kind": "upsert" }));
    wrong_upsert_id["payload"]["effectProgram"]["objectIdField"] = json!("company");
    assert!(
        set_findings(vec![series_purchase_order_entity(), wrong_upsert_id]).contains(&(
            norn_checker::action_set::codes::NAMING_SERIES_TARGET_MISMATCH.to_owned(),
            "/payload/effectProgram/objectIdField".to_owned()
        ))
    );

    let mut wrong_draft_id = series_create_action(
        true,
        true,
        &json!({ "kind": "draft_only", "lifecycle": "status", "state": "draft" }),
    );
    wrong_draft_id["payload"]["effectProgram"]["objectIdField"] = json!("company");
    assert!(
        set_findings(vec![series_purchase_order_entity(), wrong_draft_id]).contains(&(
            norn_checker::action_set::codes::NAMING_SERIES_TARGET_MISMATCH.to_owned(),
            "/payload/effectProgram/objectIdField".to_owned()
        ))
    );
}

#[test]
fn a_transition_of_a_domain_engine_type_is_not_writable_by_an_action() {
    let mut order = read_json("ir/entity-v2/artifact-set/commerce-sales-order.entity.json");
    order["payload"]["mutationAuthority"] =
        json!({ "kind": "domain_engine", "domain": "inventory" });
    let action = read_json("ir/commerce-sales-order-submit-invoice.action.json");
    assert_eq!(
        set_findings(vec![order, action]),
        [
            "/payload/effectProgram/objectKind",
            "/payload/writes/0/entityKind"
        ]
        .into_iter()
        .map(|pointer| (
            norn_checker::action_set::codes::TARGET_NOT_WRITABLE.to_owned(),
            pointer.to_owned()
        ))
        .collect::<Vec<_>>()
    );
}

#[test]
fn an_action_fires_from_the_source_states_of_the_objects_own_lifecycle() {
    let artifacts = vec![
        envelope(purchase_order_entity()),
        envelope(rush_purchase_order_entity()),
    ];
    let catalog = EffectiveCatalog::from_artifacts(&artifacts);
    let effective = |artifact_id: &str| {
        catalog
            .object_type(artifact_id)
            .expect("defined")
            .expect("available")
    };
    let order = effective("commerce.PurchaseOrder.entity");
    let rush = effective("commerce.RushPurchaseOrder.entity");
    let action = |object_kind: &str, transition: &str| -> ActionRoot {
        serde_json::from_value(
            transition_action(
                object_kind,
                "commerce.PurchaseOrder.entity",
                "status",
                transition,
            )["payload"]
                .clone(),
        )
        .expect("action payload decodes")
    };
    let values = |status: Option<&str>| -> JsonObject {
        let mut values = json!({ "po_number": "PO-0001" });
        if let Some(status) = status {
            values["status"] = json!(status);
        }
        serde_json::from_value(values).expect("values decode")
    };
    let refused = |member: &str, code: &str| {
        vec![(code.to_owned(), format!("/payload/effectProgram/{member}"))]
    };
    let cancel = action("commerce.PurchaseOrder", "cancel");

    // `cancel` leaves draft, submitted, and approved on a purchase order; the
    // rush subtype narrows it to draft, and its objects follow their own
    // (narrowed) lifecycle.
    assert_eq!(
        pairs(check_action_firing(
            &cancel,
            order,
            &values(Some("submitted"))
        )),
        Vec::new()
    );
    assert_eq!(
        pairs(check_action_firing(
            &cancel,
            rush,
            &values(Some("submitted"))
        )),
        refused("transition", UNDECLARED_WRITE)
    );
    assert_eq!(
        pairs(check_action_firing(&cancel, rush, &values(Some("draft")))),
        Vec::new()
    );
    // No state, an undeclared transition, and an object of another type all
    // fail closed.
    assert_eq!(
        pairs(check_action_firing(&cancel, order, &values(None))),
        refused("transition", UNDECLARED_WRITE)
    );
    assert_eq!(
        pairs(check_action_firing(
            &action("commerce.PurchaseOrder", "reject"),
            rush,
            &values(Some("submitted"))
        )),
        refused("transition", codes::UNKNOWN_TRANSITION)
    );
    assert_eq!(
        pairs(check_action_firing(
            &action("commerce.RushPurchaseOrder", "cancel"),
            order,
            &values(Some("draft"))
        )),
        refused(
            "objectKind",
            norn_checker::action_set::codes::EFFECT_KIND_MISMATCH
        )
    );
    // Only a transition effect is a firing.
    let mut upsert = cancel;
    upsert.effect_program = Some(ActionEffectProgram::UpsertInputObject {
        object_id_field: "po_number".to_owned(),
        object_kind: "commerce.PurchaseOrder".to_owned(),
        entity_artifact_id: "commerce.PurchaseOrder.entity".to_owned(),
        entity_artifact_version: "0.1.0".to_owned(),
        write_mode: norn_ir::UpsertWriteMode::default(),
    });
    assert_eq!(
        pairs(check_action_firing(
            &upsert,
            order,
            &values(Some("received"))
        )),
        Vec::new()
    );
}

#[test]
fn generated_series_key_cannot_be_written_by_input_derivation() {
    let mut action = series_create_action(false, true, &json!({ "kind": "create_only" }));
    action["payload"]["derive"] = json!([{
        "field": "po_number",
        "expression": "\"MANUAL\"",
        "inputRequired": false
    }]);
    assert!(
        set_findings(vec![series_purchase_order_entity(), action]).contains(&(
            norn_checker::action_set::codes::NAMING_SERIES_TARGET_MISMATCH.to_owned(),
            "/payload/derive/0/field".to_owned()
        ))
    );
}
