#![allow(
    clippy::expect_used,
    clippy::panic,
    missing_docs,
    reason = "integration-test crate"
)]

use norn_ir::{ArtifactKind, artifact_payload_schema};
use serde_json::{Value, json};

fn widget_schema() -> Value {
    serde_json::to_value(artifact_payload_schema(&ArtifactKind::Widget))
        .expect("widget schema should serialize")
}

fn valid_widget_payload() -> Value {
    json!({
        "widgetVersion": "0.1",
        "widgetId": "commerce.widget.orders",
        "dataDependencies": [],
        "viewStates": [{ "stateId": "ready", "kind": "ready" }],
        "actionBindings": [],
        "permissions": [],
        "ux": {
            "uxVersion": "0.1",
            "pages": [{
                "pageId": "orders",
                "name": "Orders",
                "contextParams": [],
                "root": {
                    "nodeId": "orders-list",
                    "component": {
                        "kind": "list",
                        "dataBindingId": "orders",
                        "fields": [],
                        "selection": "single"
                    },
                    "children": [{
                        "nodeId": "order-number-field",
                        "component": { "kind": "field", "fieldId": "order-number" },
                        "children": []
                    }]
                }
            }],
            "dataBindings": [{
                "bindingId": "orders",
                "object": { "id": "commerce.Order", "version": "2.0" },
                "query": {
                    "kind": "object_list",
                    "filters": [],
                    "sort": [],
                    "page": { "size": 20, "maxSize": 100 }
                }
            }, {
                "bindingId": "order-customers",
                "object": { "id": "crm.Customer", "version": "2.0" },
                "query": {
                    "kind": "link_traversal",
                    "sourceBindingId": "orders",
                    "linkId": "customer",
                    "maxDepth": 1,
                    "maxRows": 50,
                    "page": { "size": 10, "maxSize": 50 }
                }
            }],
            "actionBindings": [{
                "bindingId": "approve",
                "action": { "id": "commerce.order.approve", "version": "0.1.0" },
                "inputMapping": {},
                "inputs": {},
                "requiredCapability": "commerce.order.approve"
            }],
            "fields": [{
                "fieldId": "order-number",
                "target": {
                    "kind": "property",
                    "property": { "bindingId": "orders", "propertyId": "order_number" }
                },
                "label": { "en": "Order number" },
                "control": "text"
            }, {
                "fieldId": "approve-reason",
                "target": {
                    "kind": "form_input",
                    "formId": "approve-form",
                    "inputName": "reason"
                },
                "label": { "en": "Reason" },
                "control": "text"
            }],
            "forms": [{
                "formId": "approve-form",
                "mode": "action_input",
                "inputNames": ["reason"],
                "submitBindingId": "approve",
                "submit": {
                    "label": { "en": "Approve" },
                    "confirmation": { "severity": "confirm", "strength": "click" }
                }
            }],
            "actionPlacements": [{
                "placementId": "approve-toolbar",
                "bindingId": "approve",
                "label": { "en": "Approve" },
                "formId": "approve-form",
                "location": "toolbar",
                "order": 1,
                "selectionInputMapping": {},
                "confirmation": { "severity": "safe", "strength": "none" }
            }],
            "navigationItems": [],
            "overrides": []
        }
    })
}

fn validation_errors(schema: &Value, instance: &Value) -> Vec<String> {
    let validator = jsonschema::options()
        .build(schema)
        .expect("generated widget schema should compile");
    validator
        .iter_errors(instance)
        .map(|error| error.to_string())
        .collect()
}

fn assert_rejected(schema: &Value, instance: &Value, case: &str) {
    let errors = validation_errors(schema, instance);
    assert!(!errors.is_empty(), "{case} should fail schema validation");
}

#[test]
fn widget_ux_payload_schema_accepts_a_closed_bounded_contract() {
    let schema = widget_schema();
    let payload = valid_widget_payload();
    let errors = validation_errors(&schema, &payload);
    assert!(errors.is_empty(), "valid Widget UX payload: {errors:?}");
}

#[test]
fn widget_ux_payload_schema_rejects_invalid_versions_null_and_unknown_members() {
    let schema = widget_schema();
    let payload = valid_widget_payload();

    let mut invalid = payload.clone();
    invalid["ux"]["uxVersion"] = json!("0.2");
    assert_rejected(&schema, &invalid, "unknown UX version");

    let mut invalid = payload.clone();
    invalid["ux"] = Value::Null;
    assert_rejected(&schema, &invalid, "null UX");

    let mut invalid = payload.clone();
    invalid["ux"]["unexpected"] = json!(true);
    assert_rejected(&schema, &invalid, "unknown UX member");

    let mut invalid = payload;
    invalid["ux"]["dataBindings"][0]["query"]["unexpected"] = json!(true);
    assert_rejected(&schema, &invalid, "unknown query member");

    let mut invalid = valid_widget_payload();
    invalid["ux"]["pages"][0]["root"]["component"]["unexpected"] = json!(true);
    assert_rejected(&schema, &invalid, "unknown component member");
}

#[test]
fn widget_ux_payload_schema_requires_list_and_traversal_bounds() {
    let schema = widget_schema();
    let payload = valid_widget_payload();

    let mut invalid = payload.clone();
    invalid["ux"]["dataBindings"][0]["query"]
        .as_object_mut()
        .expect("list query should be an object")
        .remove("page");
    assert_rejected(&schema, &invalid, "list query without page bounds");

    for bound in ["maxDepth", "maxRows", "page"] {
        let mut invalid = payload.clone();
        invalid["ux"]["dataBindings"][1]["query"]
            .as_object_mut()
            .expect("traversal query should be an object")
            .remove(bound);
        assert_rejected(&schema, &invalid, &format!("traversal without {bound}"));
    }
}

#[test]
fn child_grid_schema_is_closed_and_requires_its_ordered_collection_contract() {
    let schema = widget_schema();
    let component = schema["$defs"]["WidgetComponent"]["oneOf"]
        .as_array()
        .expect("WidgetComponent variants")
        .iter()
        .find(|variant| variant["properties"]["kind"]["const"] == "child_grid")
        .expect("child_grid variant");
    assert_eq!(component["additionalProperties"], json!(false));
    for member in [
        "formId",
        "label",
        "collectionId",
        "orderingKey",
        "rowOpsInputName",
        "maxRows",
        "fieldIds",
    ] {
        assert!(
            component["required"].as_array().is_some_and(|members| {
                members.iter().any(|value| value.as_str() == Some(member))
            }),
            "child_grid requires {member}"
        );
    }

    let mut payload = valid_widget_payload();
    payload["ux"]["pages"][0]["root"]["children"]
        .as_array_mut()
        .expect("root children")
        .push(json!({
            "nodeId": "items-grid",
            "component": {
                "kind": "child_grid",
                "formId": "approve-form",
                "label": { "en": "Items" },
                "collectionId": "items",
                "orderingKey": "idx",
                "rowOpsInputName": "rows",
                "maxRows": 8,
                "fieldIds": ["item-name"]
            },
            "children": []
        }));
    payload["ux"]["fields"].as_array_mut().expect("fields").push(json!({
        "fieldId": "item-name",
        "target": { "kind": "child_row_property", "formId": "approve-form", "collectionId": "items", "propertyId": "name" },
        "label": { "en": "Item name" },
        "control": "text"
    }));
    assert!(validation_errors(&schema, &payload).is_empty());

    payload["ux"]["pages"][0]["root"]["children"][1]["component"]["unexpected"] = json!(true);
    assert_rejected(&schema, &payload, "unknown child grid member");
}

#[test]
fn orders_customer_widget_fixture_is_valid_against_the_generated_schema() {
    let source = include_str!("../../../fixtures/ir/ux-orders-customer.widget.json");
    let envelope: Value = serde_json::from_str(source).expect("demo widget fixture should parse");
    let schema = widget_schema();
    let errors = validation_errors(&schema, &envelope["payload"]);
    assert!(
        errors.is_empty(),
        "demo Widget UX fixture schema errors: {errors:?}"
    );
}
