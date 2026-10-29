#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs,
    reason = "integration-test crate"
)]

//! Artifact-set checks for the versioned Widget UX authoring contract (#3173).

use std::collections::BTreeSet;

use norn_checker::{Diagnostic, EntityArtifactSetContext, check_artifact_set};
use norn_ir::ArtifactEnvelope;
use serde_json::{Value, json};

const WIDGET_ID: &str = "commerce.widget.orders";
const WIDGET_FIXTURE: &str = include_str!("../../../fixtures/ir/ux-orders-customer.widget.json");

fn widget() -> Value {
    serde_json::from_str(WIDGET_FIXTURE).expect("UX fixture should be JSON")
}

fn envelope(value: Value) -> ArtifactEnvelope {
    serde_json::from_value(value).expect("artifact envelope should decode")
}

fn entity(
    artifact_id: &str,
    object_name: &str,
    properties: &Value,
    identity: &str,
    links: &Value,
) -> Value {
    json!({
        "irVersion": "0.1",
        "artifactId": artifact_id,
        "kind": "entity",
        "namespace": artifact_id.split('.').next().expect("namespace"),
        "name": object_name,
        "version": "0.1.0",
        "lifecycle": "draft",
        "scope": { "orgId": "org_demo", "environmentId": "dev" },
        "dependencies": [],
        "provenance": { "source": "authoring_sdk" },
        "payload": {
            "entityVersion": "2.0",
            "objectType": {
                "name": object_name,
                "properties": properties,
                "links": links,
                "identity": [{ "name": "primary", "properties": [identity] }]
            }
        }
    })
}

fn order_entity() -> Value {
    entity(
        "commerce.Order.entity",
        "commerce.Order",
        &json!([
            { "id": "order_id", "type": { "kind": "text" } },
            { "id": "order_number", "type": { "kind": "text" } },
            { "id": "customer_number", "type": { "kind": "text" } },
            { "id": "status", "type": { "kind": "text" } },
            { "id": "revision", "type": { "kind": "integer" } },
            { "id": "is_priority", "type": { "kind": "boolean" } },
            { "id": "optional_note", "type": { "kind": "text" }, "constraints": { "nullable": true } }
        ]),
        "order_id",
        &json!([{
            "id": "customer",
            "target": "crm.Customer",
            "cardinality": { "to": "one", "from": "many" }
        }]),
    )
}

fn customer_entity() -> Value {
    entity(
        "crm.Customer.entity",
        "crm.Customer",
        &json!([
            { "id": "customer_number", "type": { "kind": "text" } },
            { "id": "display_name", "type": { "kind": "text" } },
            { "id": "revision", "type": { "kind": "text" } }
        ]),
        "customer_number",
        &json!([]),
    )
}

fn action_artifact() -> Value {
    json!({
        "irVersion": "0.1",
        "artifactId": "commerce.order.approve.action",
        "kind": "action",
        "namespace": "commerce",
        "name": "Approve order",
        "version": "0.1.0",
        "lifecycle": "draft",
        "scope": { "orgId": "org_demo", "environmentId": "dev" },
        "dependencies": [],
        "provenance": { "source": "authoring_sdk" },
        "payload": {
            "actionId": "commerce.order.approve",
            "scopeRequired": ["orgId", "environmentId"],
            "idempotent": true,
            "ledgered": true,
            "ledgerCategory": "commerce.order",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "orderId": { "type": "string" },
                    "reason": { "type": "string" },
                    "note": { "type": "string" },
                    "comment": { "type": ["string", "null"] },
                    "metadata": {
                        "type": "object",
                        "properties": {
                            "source": { "type": "string" },
                            "ticket": { "type": "string" }
                        },
                        "required": ["source"],
                        "additionalProperties": false
                    },
                    "expectedVersion": { "type": "integer" }
                },
                "required": ["orderId", "reason"],
                "additionalProperties": false
            },
            "outputSchema": {
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }
        }
    })
}

fn transition_order_entity() -> Value {
    let mut entity = order_entity();
    entity["payload"]["objectType"]["properties"][3] = json!({
        "id": "status",
        "type": { "kind": "enum", "values": [{ "value": "draft" }, { "value": "approved" }] },
        "default": "draft"
    });
    entity["payload"]["objectType"]["lifecycles"] = json!([{
        "property": "status",
        "initial": ["draft"],
        "terminal": ["approved"],
        "transitions": [{ "id": "approve", "from": ["draft"], "to": "approved" }]
    }]);
    entity
}

fn fixture_approve_transition_action() -> Value {
    let mut action = action_artifact();
    action["payload"]["writes"] = json!([{
        "entityKind": "commerce.Order",
        "change": "modified"
    }]);
    action["payload"]["effectProgram"] = json!({
        "kind": "fire_transition",
        "objectIdField": "orderId",
        "objectKind": "commerce.Order",
        "entityArtifactId": "commerce.Order.entity",
        "entityArtifactVersion": "0.1.0",
        "lifecycle": "status",
        "transition": "approve"
    });
    action
}

fn zero_input_action_artifact() -> Value {
    let mut action = action_artifact();
    action["artifactId"] = json!("commerce.order.noop.action");
    action["name"] = json!("No-op order action");
    action["payload"]["actionId"] = json!("commerce.order.noop");
    action["payload"]["inputSchema"] = json!({
        "type": "object",
        "properties": {},
        "additionalProperties": false
    });
    action
}

fn required_version_action_artifact() -> Value {
    let mut action = action_artifact();
    action["artifactId"] = json!("commerce.order.edit.action");
    action["name"] = json!("Edit order");
    action["payload"]["actionId"] = json!("commerce.order.edit");
    action["payload"]["inputSchema"]["required"] = json!(["orderId", "reason", "expectedVersion"]);
    action
}

fn support_artifacts() -> Vec<Value> {
    vec![order_entity(), customer_entity(), action_artifact()]
}

fn ordered_rows_support() -> Vec<Value> {
    let child = entity(
        "commerce.OrderItem.entity",
        "commerce.OrderItem",
        &json!([
            { "id": "row_id", "type": { "kind": "text" } },
            { "id": "idx", "type": { "kind": "integer" } },
            { "id": "item_code", "type": { "kind": "text" } },
            { "id": "quantity", "type": { "kind": "integer" } },
            { "id": "optional_note", "type": { "kind": "text" }, "constraints": { "nullable": true } }
        ]),
        "row_id",
        &json!([]),
    );
    let mut parent = order_entity();
    parent["payload"]["objectType"]["links"]
        .as_array_mut()
        .expect("order links")
        .push(json!({
            "id": "items",
            "target": "commerce.OrderItem",
            "cardinality": { "to": "many", "from": "one" }
        }));
    parent["payload"]["objectType"]["ownedCollections"] = json!([{
        "id": "items",
        "childType": "commerce.OrderItem",
        "rowIdProperty": "row_id",
        "orderingKey": "idx",
        "cascadeOnParentDelete": true,
        "maxRows": 3
    }]);

    let mut artifacts = vec![parent, child, customer_entity(), action_artifact()];
    for (artifact_id, action_id) in [
        (
            "commerce.order.create-with-rows.action",
            "commerce.order.create-with-rows",
        ),
        (
            "commerce.order.edit-with-rows.action",
            "commerce.order.edit-with-rows",
        ),
    ] {
        artifacts.push(json!({
            "irVersion": "0.1",
            "artifactId": artifact_id,
            "kind": "action",
            "namespace": "commerce",
            "name": action_id,
            "version": "0.1.0",
            "lifecycle": "draft",
            "scope": { "orgId": "org_demo", "environmentId": "dev" },
            "dependencies": [
                { "artifactId": "commerce.Order.entity", "kind": "entity", "version": "0.1.0", "required": true },
                { "artifactId": "commerce.OrderItem.entity", "kind": "entity", "version": "0.1.0", "required": true }
            ],
            "provenance": { "source": "authoring_sdk" },
            "payload": {
                "actionId": action_id,
                "scopeRequired": ["orgId", "environmentId"],
                "idempotent": true,
                "ledgered": true,
                "ledgerCategory": "commerce.order",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "orderId": { "type": "string" },
                        "parent": { "type": ["null", "boolean", "number", "string", "array", "object"] },
                        "rows": { "type": ["null", "boolean", "number", "string", "array", "object"] },
                        "expectedParentVersion": { "type": "string" }
                    },
                    "required": ["orderId", "parent", "rows", "expectedParentVersion"],
                    "additionalProperties": false
                },
                "outputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
                "writes": [
                    { "entityKind": "commerce.Order", "change": "created" },
                    { "entityKind": "commerce.Order", "change": "modified" },
                    { "entityKind": "commerce.OrderItem", "change": "created" },
                    { "entityKind": "commerce.OrderItem", "change": "modified" },
                    { "entityKind": "commerce.OrderItem", "change": "deleted" }
                ],
                "effectProgram": {
                    "kind": "mutate_parent_with_rows",
                    "objectIdField": "orderId",
                    "objectKind": "commerce.Order",
                    "entityArtifactId": "commerce.Order.entity",
                    "entityArtifactVersion": "0.1.0",
                    "parentValuesField": "parent",
                    "rowOpsField": "rows",
                    "parentVersionField": "expectedParentVersion",
                    "collections": [{ "id": "items" }]
                }
            }
        }));
    }
    artifacts
}

fn ordered_rows_with_display_only_properties() -> Vec<Value> {
    let mut artifacts = ordered_rows_support();
    let parent = &mut artifacts[0]["payload"]["objectType"];
    let parent_properties = parent["properties"]
        .as_array_mut()
        .expect("parent properties");
    let status = parent_properties
        .iter_mut()
        .find(|property| property["id"] == "status")
        .expect("parent status property");
    *status = json!({
        "id": "status",
        "type": { "kind": "enum", "values": [{ "value": "draft" }, { "value": "approved" }] },
        "default": "draft"
    });
    parent_properties.push(json!({
        "id": "display_label",
        "type": { "kind": "text" },
        "computed": "self.order_number + \" view\""
    }));
    parent["lifecycles"] = json!([{
        "property": "status",
        "initial": ["draft"],
        "terminal": ["approved"],
        "transitions": [{ "id": "approve", "from": ["draft"], "to": "approved" }]
    }]);

    let child = &mut artifacts[1]["payload"]["objectType"];
    child["properties"].as_array_mut().expect("child properties").extend([
        json!({
            "id": "display_label",
            "type": { "kind": "text" },
            "computed": "self.item_code + \" view\""
        }),
        json!({
            "id": "row_state",
            "type": { "kind": "enum", "values": [{ "value": "draft" }, { "value": "archived" }] },
            "default": "draft"
        }),
    ]);
    child["lifecycles"] = json!([{
        "property": "row_state",
        "initial": ["draft"],
        "terminal": ["archived"],
        "transitions": [{ "id": "archive", "from": ["draft"], "to": "archived" }]
    }]);
    artifacts
}

fn ordered_rows_widget() -> Value {
    let mut value = widget();
    for artifact_id in [
        "commerce.OrderItem.entity",
        "commerce.order.create-with-rows.action",
        "commerce.order.edit-with-rows.action",
    ] {
        value["dependencies"]
            .as_array_mut()
            .expect("widget dependencies")
            .push(json!({
                "artifactId": artifact_id,
                "kind": if artifact_id.ends_with(".entity") { "entity" } else { "action" },
                "version": "0.1.0",
                "required": true
            }));
    }
    let ux = &mut value["payload"]["ux"];
    ux["dataBindings"].as_array_mut().expect("data bindings").extend([
        json!({
            "bindingId": "edit-order-detail",
            "object": { "id": "commerce.Order.entity", "version": "0.1.0" },
            "query": { "kind": "object_detail", "key": [{ "propertyId": "order_id", "value": { "kind": "literal", "value": "ORD-1" } }] }
        }),
        json!({
            "bindingId": "edit-order-items",
            "object": { "id": "commerce.OrderItem.entity", "version": "0.1.0" },
            "query": { "kind": "link_traversal", "sourceBindingId": "edit-order-detail", "linkId": "items", "maxDepth": 1, "maxRows": 3, "page": { "size": 3, "maxSize": 3 } }
        }),
    ]);
    ux["actionBindings"].as_array_mut().expect("action bindings").extend([
        json!({
            "bindingId": "create-order-rows",
            "action": { "id": "commerce.order.create-with-rows.action", "version": "0.1.0" },
            "inputMapping": { "orderId": "input.draft.orderId", "parent": "input.draft.parent", "rows": "input.draft.rows", "expectedParentVersion": "\"0\"" },
            "inputs": { "draftFormId": "create-order-form" },
            "requiredCapability": "commerce.order.write"
        }),
        json!({
            "bindingId": "edit-order-rows",
            "action": { "id": "commerce.order.edit-with-rows.action", "version": "0.1.0" },
            "inputMapping": { "orderId": "input.recordKey", "parent": "input.draft.parent", "rows": "input.draft.rows", "expectedParentVersion": "input.recordVersion" },
            "inputs": { "draftFormId": "edit-order-form", "recordBindingId": "edit-order-detail" },
            "requiredCapability": "commerce.order.write"
        })
    ]);
    ux["forms"].as_array_mut().expect("forms").extend([
        json!({
            "formId": "create-order-form", "mode": "create", "inputNames": ["orderId", "parent", "rows"],
            "submitBindingId": "create-order-rows", "submit": { "label": { "en": "Create order" }, "confirmation": { "severity": "destructive", "strength": "click" } }
        }),
        json!({
            "formId": "edit-order-form", "mode": "edit", "dataBindingId": "edit-order-detail", "inputNames": ["parent", "rows"],
            "submitBindingId": "edit-order-rows", "submit": { "label": { "en": "Save order" }, "confirmation": { "severity": "destructive", "strength": "click" } }
        })
    ]);
    ux["fields"].as_array_mut().expect("fields").extend([
        json!({ "fieldId": "create-order-id", "target": { "kind": "form_input", "formId": "create-order-form", "inputName": "orderId" }, "label": { "en": "Order id" }, "control": "text" }),
        json!({ "fieldId": "create-order-number", "target": { "kind": "parent_property", "formId": "create-order-form", "inputName": "parent", "propertyId": "order_number" }, "label": { "en": "Order number" }, "control": "text" }),
        json!({ "fieldId": "create-parent-order-id", "target": { "kind": "parent_property", "formId": "create-order-form", "inputName": "parent", "propertyId": "order_id" }, "label": { "en": "Stored order id" }, "control": "text" }),
        json!({ "fieldId": "create-parent-customer", "target": { "kind": "parent_property", "formId": "create-order-form", "inputName": "parent", "propertyId": "customer_number" }, "label": { "en": "Customer number" }, "control": "text" }),
        json!({ "fieldId": "create-parent-status", "target": { "kind": "parent_property", "formId": "create-order-form", "inputName": "parent", "propertyId": "status" }, "label": { "en": "Status" }, "control": "text" }),
        json!({ "fieldId": "create-parent-revision", "target": { "kind": "parent_property", "formId": "create-order-form", "inputName": "parent", "propertyId": "revision" }, "label": { "en": "Revision" }, "control": "number" }),
        json!({ "fieldId": "create-parent-priority", "target": { "kind": "parent_property", "formId": "create-order-form", "inputName": "parent", "propertyId": "is_priority" }, "label": { "en": "Priority" }, "control": "checkbox" }),
        json!({ "fieldId": "create-item-code", "target": { "kind": "child_row_property", "formId": "create-order-form", "collectionId": "items", "propertyId": "item_code" }, "label": { "en": "Item code" }, "control": "text" }),
        json!({ "fieldId": "create-item-quantity", "target": { "kind": "child_row_property", "formId": "create-order-form", "collectionId": "items", "propertyId": "quantity" }, "label": { "en": "Quantity" }, "control": "number" }),
        json!({ "fieldId": "edit-order-number", "target": { "kind": "parent_property", "formId": "edit-order-form", "inputName": "parent", "propertyId": "order_number" }, "label": { "en": "Order number" }, "control": "text" }),
        json!({ "fieldId": "edit-item-code", "target": { "kind": "child_row_property", "formId": "edit-order-form", "collectionId": "items", "propertyId": "item_code" }, "label": { "en": "Item code" }, "control": "text" }),
        json!({ "fieldId": "edit-item-quantity", "target": { "kind": "child_row_property", "formId": "edit-order-form", "collectionId": "items", "propertyId": "quantity" }, "label": { "en": "Quantity" }, "control": "number" }),
        json!({ "fieldId": "edit-item-note", "target": { "kind": "child_row_property", "formId": "edit-order-form", "collectionId": "items", "propertyId": "optional_note" }, "label": { "en": "Note" }, "control": "text", "readOnlyWhen": { "expression": "input.row.item_code == \"LOCKED\"", "inputs": { "rowGridId": "edit-order-grid" } } })
    ]);
    let page_children = ux["pages"][0]["root"]["children"]
        .as_array_mut()
        .expect("page root children");
    page_children.extend([
        json!({
            "nodeId": "create-order-form-node", "component": { "kind": "form", "formId": "create-order-form" },
            "children": [
                { "nodeId": "create-order-id-node", "component": { "kind": "field", "fieldId": "create-order-id" }, "children": [] },
                { "nodeId": "create-order-number-node", "component": { "kind": "field", "fieldId": "create-order-number" }, "children": [] },
                { "nodeId": "create-parent-order-id-node", "component": { "kind": "field", "fieldId": "create-parent-order-id" }, "children": [] },
                { "nodeId": "create-parent-customer-node", "component": { "kind": "field", "fieldId": "create-parent-customer" }, "children": [] },
                { "nodeId": "create-parent-status-node", "component": { "kind": "field", "fieldId": "create-parent-status" }, "children": [] },
                { "nodeId": "create-parent-revision-node", "component": { "kind": "field", "fieldId": "create-parent-revision" }, "children": [] },
                { "nodeId": "create-parent-priority-node", "component": { "kind": "field", "fieldId": "create-parent-priority" }, "children": [] },
                { "nodeId": "create-order-grid", "component": { "kind": "child_grid", "formId": "create-order-form", "label": { "en": "Items" }, "collectionId": "items", "orderingKey": "idx", "rowOpsInputName": "rows", "maxRows": 3, "fieldIds": ["create-item-code", "create-item-quantity"] }, "children": [] }
            ]
        }),
        json!({
            "nodeId": "edit-order-detail-node", "component": { "kind": "detail", "dataBindingId": "edit-order-detail" }, "children": []
        }),
        json!({
            "nodeId": "edit-order-form-node", "component": { "kind": "form", "formId": "edit-order-form" },
            "children": [
                { "nodeId": "edit-order-number-node", "component": { "kind": "field", "fieldId": "edit-order-number" }, "children": [] },
                { "nodeId": "edit-order-grid", "component": { "kind": "child_grid", "formId": "edit-order-form", "label": { "en": "Items" }, "collectionId": "items", "orderingKey": "idx", "dataBindingId": "edit-order-items", "rowOpsInputName": "rows", "maxRows": 3, "fieldIds": ["edit-item-code", "edit-item-quantity", "edit-item-note"] }, "children": [] }
            ]
        })
    ]);
    value
}

fn ordered_rows_display_widget() -> Value {
    let mut value = ordered_rows_widget();
    let fields = value["payload"]["ux"]["fields"]
        .as_array_mut()
        .expect("fields");
    for field_id in ["create-parent-status", "edit-order-number"] {
        let field = fields
            .iter_mut()
            .find(|field| field["fieldId"] == field_id)
            .expect("existing parent field");
        field["control"] = json!("display");
    }
    fields.extend([
        json!({ "fieldId": "edit-parent-display-label", "target": { "kind": "parent_property", "formId": "edit-order-form", "inputName": "parent", "propertyId": "display_label" }, "label": { "en": "Parent display label" }, "control": "display" }),
        json!({ "fieldId": "edit-parent-lifecycle-state", "target": { "kind": "parent_property", "formId": "edit-order-form", "inputName": "parent", "propertyId": "status" }, "label": { "en": "Parent lifecycle state" }, "control": "display" }),
        json!({ "fieldId": "edit-item-display-label", "target": { "kind": "child_row_property", "formId": "edit-order-form", "collectionId": "items", "propertyId": "display_label" }, "label": { "en": "Item display label" }, "control": "display" }),
        json!({ "fieldId": "edit-item-lifecycle-state", "target": { "kind": "child_row_property", "formId": "edit-order-form", "collectionId": "items", "propertyId": "row_state" }, "label": { "en": "Item lifecycle state" }, "control": "display" }),
    ]);
    let edit_form = value["payload"]["ux"]["pages"][0]["root"]["children"]
        .as_array_mut()
        .expect("page children")
        .iter_mut()
        .find(|node| node["nodeId"] == "edit-order-form-node")
        .expect("edit form node");
    let children = edit_form["children"]
        .as_array_mut()
        .expect("edit form children");
    children.extend([
        json!({ "nodeId": "edit-parent-display-label-node", "component": { "kind": "field", "fieldId": "edit-parent-display-label" }, "children": [] }),
        json!({ "nodeId": "edit-parent-lifecycle-state-node", "component": { "kind": "field", "fieldId": "edit-parent-lifecycle-state" }, "children": [] }),
    ]);
    let grid = children
        .iter_mut()
        .find(|node| node["nodeId"] == "edit-order-grid")
        .expect("edit grid node");
    grid["component"]["fieldIds"] = json!([
        "edit-item-code",
        "edit-item-quantity",
        "edit-item-note",
        "edit-item-display-label",
        "edit-item-lifecycle-state"
    ]);
    value
}

fn findings(widget: Value) -> Vec<Diagnostic> {
    findings_with_base(widget, &[])
}

fn findings_with_base(widget: Value, base: &[Value]) -> Vec<Diagnostic> {
    findings_with_support(widget, support_artifacts(), base)
}

fn findings_with_support(
    widget: Value,
    support_artifacts: Vec<Value>,
    base: &[Value],
) -> Vec<Diagnostic> {
    let mut values = support_artifacts;
    values.push(widget);
    let artifacts: Vec<_> = values.into_iter().map(envelope).collect();
    let base: Vec<_> = base.iter().cloned().map(envelope).collect();
    check_artifact_set(
        &artifacts,
        &EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &base,
            tenant: None,
        },
    )
}

fn widget_findings(widget: Value) -> Vec<Diagnostic> {
    findings(widget)
        .into_iter()
        .filter(|finding| finding.artifact_id.as_deref() == Some(WIDGET_ID))
        .collect()
}

fn assert_issue(found: &[Diagnostic], pointer_part: &str, message_part: &str) {
    assert!(
        found.iter().any(|finding| {
            finding
                .span
                .as_ref()
                .is_some_and(|span| span.pointer.contains(pointer_part))
                && finding.message.contains(message_part)
        }),
        "expected finding at *{pointer_part}* containing {message_part:?}; got {found:#?}"
    );
}

fn assert_clean(found: &[Diagnostic]) {
    assert!(
        found.is_empty(),
        "expected a clean artifact set; got {found:#?}"
    );
}

#[test]
fn create_and_edit_forms_admit_bounded_owned_child_grids_with_row_conditions() {
    let widget = ordered_rows_widget();
    assert_clean(&findings_with_support(widget, ordered_rows_support(), &[]));
}

#[test]
fn keyless_parent_row_edit_form_accepts_runtime_detail_and_requires_exact_parent() {
    let mut value = ordered_rows_widget();
    let ux = &mut value["payload"]["ux"];
    let page_id = ux["pages"][0]["pageId"]
        .as_str()
        .expect("page id")
        .to_owned();
    ux["pages"][0]["contextParams"] = json!([{ "paramId": "key", "type": "text", "required": true, "runtimeKeyOf": { "id": "commerce.Order.entity", "version": "0.1.0" } }]);
    // This fixture is a hosted widget, whose host supplies the required key.
    ux.as_object_mut().expect("ux object").remove("app");
    let binding = ux["dataBindings"]
        .as_array_mut()
        .expect("bindings")
        .iter_mut()
        .find(|b| b["bindingId"] == "edit-order-detail")
        .expect("parent detail");
    binding["query"] =
        json!({ "kind": "object_runtime_detail", "pageId": page_id, "parameterId": "key" });
    let mut support = ordered_rows_support();
    support[0]["payload"]["objectType"]
        .as_object_mut()
        .expect("parent entity")
        .remove("identity");
    assert_clean(&findings_with_support(value.clone(), support.clone(), &[]));
    let binding = value["payload"]["ux"]["dataBindings"]
        .as_array_mut()
        .expect("bindings")
        .iter_mut()
        .find(|b| b["bindingId"] == "edit-order-detail")
        .expect("parent detail");
    binding["object"]["id"] = json!("commerce.OrderItem.entity");
    assert_issue(
        &findings_with_support(value, support, &[]),
        "/dataBindingId",
        "edit form must bind the exact parent entity detail",
    );
}

#[test]
fn child_action_derivations_require_display_controls_in_create_and_edit_grids() {
    let mut support = ordered_rows_support();
    support[1]["payload"]["objectType"]["properties"]
        .as_array_mut()
        .expect("child properties")
        .extend([
            json!({ "id": "double_quantity", "type": { "kind": "integer" } }),
            json!({ "id": "has_quantity", "type": { "kind": "boolean" } }),
        ]);
    for action in support.iter_mut().filter(|artifact| {
        artifact["payload"]["effectProgram"]["kind"] == "mutate_parent_with_rows"
    }) {
        action["payload"]["effectProgram"]["collections"][0]["derive"] = json!({
            "double_quantity": "self.quantity * 2",
            "has_quantity": "self.quantity > 0"
        });
    }
    let mut widget = ordered_rows_widget();
    for mode in ["create", "edit"] {
        for property in ["double_quantity", "has_quantity"] {
            let field_id = format!("{mode}-{property}");
            widget["payload"]["ux"]["fields"]
                .as_array_mut()
                .expect("fields")
                .push(json!({
                    "fieldId": field_id,
                    "target": { "kind": "child_row_property", "formId": format!("{mode}-order-form"), "collectionId": "items", "propertyId": property },
                    "label": { "en": property }, "control": "display"
                }));
            let form = widget["payload"]["ux"]["pages"][0]["root"]["children"]
                .as_array_mut()
                .expect("page children")
                .iter_mut()
                .find(|node| node["nodeId"] == format!("{mode}-order-form-node"))
                .expect("form");
            let grid = form["children"]
                .as_array_mut()
                .expect("form children")
                .iter_mut()
                .find(|node| node["nodeId"] == format!("{mode}-order-grid"))
                .expect("grid");
            grid["component"]["fieldIds"]
                .as_array_mut()
                .expect("grid fields")
                .push(json!(field_id));
        }
    }
    assert_clean(&findings_with_support(widget.clone(), support.clone(), &[]));
    for mode in ["create", "edit"] {
        for (property, control) in [("double_quantity", "number"), ("has_quantity", "checkbox")] {
            let mut editable = widget.clone();
            let fields = editable["payload"]["ux"]["fields"]
                .as_array_mut()
                .expect("fields");
            let index = fields
                .iter()
                .position(|field| field["fieldId"] == format!("{mode}-{property}"))
                .expect("derived field");
            fields[index]["control"] = json!(control);
            // Conditional read-only is insufficient: the presentation must be display-only.
            fields[index]["readOnlyWhen"] = json!({ "expression": "true", "inputs": {} });
            let found = findings_with_support(editable, support.clone(), &[]);
            assert_issue(
                &found,
                &format!("/fields/{index}/control"),
                "action-derived properties",
            );
            assert_issue(&found, "/component/fieldIds/", "action-derived properties");
        }
    }
}

#[test]
fn display_only_parent_and_child_lifecycle_fields_admit_on_row_only_edit_forms() {
    let widget = ordered_rows_display_widget();
    assert_clean(&findings_with_support(
        widget,
        ordered_rows_with_display_only_properties(),
        &[],
    ));
}

#[test]
fn parent_and_child_lifecycle_fields_reject_editable_controls() {
    let support = ordered_rows_with_display_only_properties();
    let mut parent_widget = ordered_rows_display_widget();
    let parent_fields = parent_widget["payload"]["ux"]["fields"]
        .as_array_mut()
        .expect("fields");
    let parent_index = parent_fields
        .iter()
        .position(|field| field["fieldId"] == "edit-parent-lifecycle-state")
        .expect("parent lifecycle display field");
    parent_fields[parent_index]["control"] = json!("text");
    assert_issue(
        &findings_with_support(parent_widget, support.clone(), &[]),
        &format!("/fields/{parent_index}/control"),
        "lifecycle-managed parent properties are read-only",
    );

    let mut child_widget = ordered_rows_display_widget();
    let child_fields = child_widget["payload"]["ux"]["fields"]
        .as_array_mut()
        .expect("fields");
    let child_index = child_fields
        .iter()
        .position(|field| field["fieldId"] == "edit-item-lifecycle-state")
        .expect("child lifecycle display field");
    child_fields[child_index]["control"] = json!("text");
    assert_issue(
        &findings_with_support(child_widget, support, &[]),
        &format!("/fields/{child_index}/control"),
        "lifecycle-managed properties are read-only",
    );
}

#[test]
fn child_grid_requires_lifecycle_defaults_for_add() {
    let widget = ordered_rows_display_widget();
    let mut support = ordered_rows_with_display_only_properties();
    let child_properties = support[1]["payload"]["objectType"]["properties"]
        .as_array_mut()
        .expect("child properties");
    let lifecycle_property = child_properties
        .iter_mut()
        .find(|property| property["id"] == "row_state")
        .expect("child lifecycle property");
    lifecycle_property
        .as_object_mut()
        .expect("lifecycle property object")
        .remove("default");

    assert_issue(
        &findings_with_support(widget, support, &[]),
        "/component/fieldIds",
        "child grid Add requires lifecycle property \"row_state\" to declare a default in one of its initial states",
    );
}

#[test]
fn parent_create_requires_default_for_read_only_lifecycle_property() {
    let widget = ordered_rows_display_widget();
    let mut support = ordered_rows_with_display_only_properties();
    let parent_properties = support[0]["payload"]["objectType"]["properties"]
        .as_array_mut()
        .expect("parent properties");
    let lifecycle_property = parent_properties
        .iter_mut()
        .find(|property| property["id"] == "status")
        .expect("parent lifecycle property");
    lifecycle_property
        .as_object_mut()
        .expect("lifecycle property object")
        .remove("default");

    assert_issue(
        &findings_with_support(widget, support, &[]),
        "/submitBindingId",
        "create form requires lifecycle property \"status\" to declare a default in one of its initial states",
    );
}

#[test]
fn create_parent_form_requires_coverage_for_required_parent_properties() {
    let mut widget = ordered_rows_widget();
    widget["payload"]["ux"]["fields"]
        .as_array_mut()
        .expect("fields")
        .retain(|field| field["fieldId"] != "create-parent-customer");
    assert_issue(
        &findings_with_support(widget, ordered_rows_support(), &[]),
        "/forms/",
        "required parent property \"customer_number\"",
    );
}

#[test]
fn child_grid_requires_add_complete_coverage_for_required_child_properties() {
    let mut widget = ordered_rows_widget();
    let page_children = widget["payload"]["ux"]["pages"][0]["root"]["children"]
        .as_array_mut()
        .expect("page children");
    let create_form = page_children
        .iter_mut()
        .find(|node| node["nodeId"] == "create-order-form-node")
        .expect("create form node");
    let create_grid = create_form["children"]
        .as_array_mut()
        .expect("form children")
        .iter_mut()
        .find(|node| node["nodeId"] == "create-order-grid")
        .expect("create child grid");
    create_grid["component"]["fieldIds"] = json!(["create-item-code"]);
    widget["payload"]["ux"]["fields"]
        .as_array_mut()
        .expect("fields")
        .retain(|field| field["fieldId"] != "create-item-quantity");
    assert_issue(
        &findings_with_support(widget, ordered_rows_support(), &[]),
        "/component/fieldIds",
        "required child property \"quantity\"",
    );
}

#[test]
fn parent_rows_forms_do_not_allow_member_access_on_opaque_json_payloads() {
    let mut widget = ordered_rows_widget();
    widget["payload"]["ux"]["actionBindings"][1]["inputMapping"]["parent"] =
        json!("input.draft.parent.order_number");
    assert_issue(
        &findings_with_support(widget, ordered_rows_support(), &[]),
        "/actionBindings/1/inputMapping/parent",
        "ZEN mapping",
    );
}

#[test]
fn child_row_condition_must_name_the_field_enclosing_grid() {
    let mut widget = ordered_rows_widget();
    let fields = widget["payload"]["ux"]["fields"]
        .as_array_mut()
        .expect("fields array");
    let field_index = fields
        .iter()
        .position(|field| field["fieldId"] == "edit-item-note")
        .expect("edit note field");
    fields[field_index]["readOnlyWhen"]["inputs"]["rowGridId"] = json!("create-order-grid");
    assert_issue(
        &findings_with_support(widget, ordered_rows_support(), &[]),
        &format!("/fields/{field_index}/readOnlyWhen/inputs/rowGridId"),
        "exact enclosing ChildGrid",
    );
}

#[test]
fn child_grid_rejects_bounds_and_order_keys_that_differ_from_entity_contract() {
    for (member, value, message) in [
        (
            "orderingKey",
            json!("rank"),
            "must match the owned collection",
        ),
        ("maxRows", json!(4), "must be positive and no greater"),
    ] {
        let mut widget = ordered_rows_widget();
        let grid = widget["payload"]["ux"]["pages"][0]["root"]["children"]
            .as_array_mut()
            .expect("root children")
            .iter_mut()
            .find(|node| node["nodeId"] == "edit-order-form-node")
            .expect("edit form node")["children"]
            .as_array_mut()
            .expect("form children")
            .iter_mut()
            .find(|node| node["nodeId"] == "edit-order-grid")
            .expect("edit grid node");
        grid["component"][member] = value;
        assert_issue(
            &findings_with_support(widget, ordered_rows_support(), &[]),
            &format!("component/{member}"),
            message,
        );
    }
}

#[test]
fn orders_page_composes_bounded_reads_customer_navigation_and_action_form() {
    assert_clean(&findings(widget()));
}

#[test]
fn fixture_row_transition_uses_the_runtime_id_instead_of_the_business_order_id() {
    let runtime_record = json!({
        "id": "runtime-order-A",
        "values": { "order_id": "business-order-B", "status": "draft" }
    });
    assert_ne!(runtime_record["id"], runtime_record["values"]["order_id"]);

    let mut value = widget();
    let ux = &mut value["payload"]["ux"];
    ux["actionBindings"][0]["inputMapping"]["orderId"] = json!("input.recordKey");
    ux["actionBindings"][0]["inputMapping"]["reason"] = json!("\"Approved\"");
    let _ = ux["actionBindings"][0]["inputs"]
        .as_object_mut()
        .expect("action inputs")
        .remove("draftFormId");
    let _ = ux["actionPlacements"][0]
        .as_object_mut()
        .expect("action placement")
        .remove("formId");
    ux["forms"] = json!([]);
    ux["fields"]
        .as_array_mut()
        .expect("fields array")
        .retain(|field| field["target"]["kind"] != "form_input");
    ux["pages"][0]["root"]["children"]
        .as_array_mut()
        .expect("page children")
        .remove(2);

    let support = vec![
        transition_order_entity(),
        customer_entity(),
        fixture_approve_transition_action(),
    ];
    assert_clean(&findings_with_support(value.clone(), support.clone(), &[]));

    value["payload"]["ux"]["actionBindings"][0]["inputMapping"]["orderId"] =
        json!("input.record.order_id");
    assert_issue(
        &findings_with_support(value, support, &[]),
        "/pages/0/root/children/0/children/1",
        "objectIdField must map directly from input.recordKey",
    );
}

#[test]
fn a_renamed_entity_property_is_rejected() {
    let mut value = widget();
    value["payload"]["ux"]["fields"][0]["target"]["property"]["propertyId"] =
        json!("order_numeral");
    assert_issue(
        &widget_findings(value),
        "/fields/0/target/property",
        "property",
    );
}

#[test]
fn action_input_mapping_must_match_the_declared_input_type() {
    let mut value = widget();
    value["payload"]["ux"]["actionBindings"][0]["inputs"]["recordBindingId"] = json!("orders");
    value["payload"]["ux"]["actionBindings"][0]["inputMapping"]["reason"] =
        json!("input.record.revision");
    assert_issue(
        &widget_findings(value),
        "/actionBindings/0/inputMapping/reason",
        "incompatible",
    );
}

#[test]
fn required_action_inputs_must_be_mapped() {
    let mut value = widget();
    let _ = value["payload"]["ux"]["actionBindings"][0]["inputMapping"]
        .as_object_mut()
        .expect("input mapping object")
        .remove("orderId");
    assert_issue(
        &widget_findings(value),
        "/forms/0/submitBindingId",
        "required submit input",
    );
}

#[test]
fn a_shared_action_binding_cannot_borrow_another_placements_row_record() {
    let mut value = widget();
    let ux = &mut value["payload"]["ux"];
    ux["pages"][0]["root"]["children"][0]["component"]["selection"] = json!("none");
    ux["actionBindings"][0]["inputMapping"]["orderId"] = json!("input.record.order_id");
    let mut toolbar = ux["actionPlacements"][0].clone();
    toolbar["placementId"] = json!("approve-toolbar");
    toolbar["location"] = json!("toolbar");
    let toolbar_object = toolbar.as_object_mut().expect("placement object");
    let _ = toolbar_object.remove("selectionBindingId");
    let _ = toolbar_object.remove("enabledWhen");
    ux["actionPlacements"]
        .as_array_mut()
        .expect("placements array")
        .push(toolbar);
    ux["pages"][0]["root"]["children"]
        .as_array_mut()
        .expect("page children")
        .push(json!({
            "nodeId": "approve-toolbar-node",
            "semanticId": "approve-toolbar-action",
            "component": { "kind": "action", "placementId": "approve-toolbar" },
            "children": []
        }));
    assert_issue(&widget_findings(value), "/actionBindings/0", "record");
}

#[test]
fn traversal_requires_a_detail_or_single_selected_source_record() {
    let mut value = widget();
    value["payload"]["ux"]["pages"][0]["root"]["children"][0]["component"]["selection"] =
        json!("none");
    assert_issue(
        &widget_findings(value),
        "/dataBindings/1/query/sourceBindingId",
        "source",
    );
}

fn add_customer_detail_from_record(value: &mut Value, binding_id: &str, source_id: &str) {
    let ux = &mut value["payload"]["ux"];
    let mut binding = ux["dataBindings"][2].clone();
    binding["bindingId"] = json!(binding_id);
    binding["query"]["key"][0]["value"] = json!({
        "kind": "record",
        "property": { "bindingId": source_id, "propertyId": "customer_number" }
    });
    ux["dataBindings"]
        .as_array_mut()
        .expect("data bindings array")
        .push(binding);
    ux["pages"][1]["root"]["children"]
        .as_array_mut()
        .expect("customer page children")
        .push(json!({
            "nodeId": format!("{binding_id}-node"),
            "component": { "kind": "detail", "dataBindingId": binding_id },
            "children": []
        }));
}

#[test]
fn acyclic_record_query_dependencies_remain_valid() {
    let mut value = widget();
    add_customer_detail_from_record(&mut value, "related-customer", "customer-detail");
    add_customer_detail_from_record(&mut value, "related-customer-copy", "related-customer");
    assert_clean(&widget_findings(value));
}

#[test]
fn a_detail_key_cannot_require_its_own_current_record() {
    let mut value = widget();
    value["payload"]["ux"]["dataBindings"][2]["query"]["key"][0]["value"] = json!({
        "kind": "record",
        "property": { "bindingId": "customer-detail", "propertyId": "customer_number" }
    });
    add_customer_detail_from_record(&mut value, "downstream-customer", "customer-detail");
    let found = widget_findings(value);
    assert_issue(
        &found,
        "/dataBindings/2/query/key/0/value/property/bindingId",
        "query record dependency cycle",
    );
    assert_eq!(found.len(), 1, "the cycle is the only invalid contract");
    let cycles: Vec<_> = found
        .iter()
        .filter(|finding| finding.message.contains("query record dependency cycle"))
        .collect();
    assert_eq!(cycles.len(), 1, "a downstream binding is not itself cyclic");
    assert!(cycles[0].message.contains("customer-detail"));
    assert!(!cycles[0].message.contains("downstream-customer"));
}

#[test]
fn detail_keys_cannot_mutually_require_each_others_current_records() {
    let mut value = widget();
    add_customer_detail_from_record(&mut value, "related-customer", "customer-detail");
    value["payload"]["ux"]["dataBindings"][2]["query"]["key"][0]["value"] = json!({
        "kind": "record",
        "property": { "bindingId": "related-customer", "propertyId": "customer_number" }
    });
    let found = widget_findings(value);
    assert_issue(
        &found,
        "/dataBindings/2/query/key/0/value/property/bindingId",
        "query record dependency cycle",
    );
    assert_eq!(found.len(), 1, "the cycle is the only invalid contract");
    assert!(found.iter().any(|finding| {
        finding.message.contains("query record dependency cycle")
            && finding.message.contains("customer-detail")
            && finding.message.contains("related-customer")
    }));
}

#[test]
fn a_single_selection_list_filter_cannot_require_its_own_record() {
    let mut value = widget();
    value["payload"]["ux"]["dataBindings"][0]["query"]["filters"][0]["value"] = json!({
        "kind": "record",
        "property": { "bindingId": "orders", "propertyId": "status" }
    });
    let found = widget_findings(value);
    assert_issue(
        &found,
        "/dataBindings/0/query/filters/0/value/property/bindingId",
        "query record dependency cycle",
    );
    assert_eq!(found.len(), 1, "the cycle is the only invalid contract");
}

#[test]
fn a_traversal_and_its_sources_filter_cannot_form_a_record_query_cycle() {
    let mut value = widget();
    let ux = &mut value["payload"]["ux"];
    ux["pages"][0]["root"]["children"][1]["component"]["selection"] = json!("single");
    ux["dataBindings"][0]["query"]["filters"][0]["value"] = json!({
        "kind": "record",
        "property": { "bindingId": "order-customers", "propertyId": "display_name" }
    });
    let found = widget_findings(value);
    assert_issue(
        &found,
        "/dataBindings/0/query/filters/0/value/property/bindingId",
        "query record dependency cycle",
    );
    assert_eq!(found.len(), 1, "the cycle is the only invalid contract");
    assert!(found.iter().any(|finding| {
        finding.message.contains("query record dependency cycle")
            && finding.message.contains("orders")
            && finding.message.contains("order-customers")
    }));
}

#[test]
fn nullable_filter_literals_require_an_operand_for_every_ordered_operator() {
    for (property_type, nonnull) in [
        (json!({ "kind": "text" }), json!("note")),
        (json!({ "kind": "integer" }), json!(42)),
        (json!({ "kind": "date" }), json!("2026-10-07")),
        (
            json!({ "kind": "timestamp", "timezone": "utc" }),
            json!("2026-10-07T00:00:00Z"),
        ),
    ] {
        let mut support = support_artifacts();
        support[0]["payload"]["objectType"]["properties"][6]["type"] = property_type;
        let mut value = widget();
        value["payload"]["ux"]["dataBindings"][0]["query"]["filters"][0]["property"]["propertyId"] =
            json!("optional_note");
        for operator in ["lt", "lte", "gt", "gte"] {
            value["payload"]["ux"]["dataBindings"][0]["query"]["filters"][0]["operator"] =
                json!(operator);
            value["payload"]["ux"]["dataBindings"][0]["query"]["filters"][0]["value"] =
                json!({ "kind": "literal", "value": null });
            let found = findings_with_support(value.clone(), support.clone(), &[]);
            assert_issue(
                &found,
                "/dataBindings/0/query/filters/0/value",
                "cannot be null",
            );
            assert_eq!(found.len(), 1);
            value["payload"]["ux"]["dataBindings"][0]["query"]["filters"][0]["value"] =
                json!({ "kind": "literal", "value": nonnull });
            assert_clean(&findings_with_support(value.clone(), support.clone(), &[]));
        }
        for operator in ["eq", "neq", "in"] {
            value["payload"]["ux"]["dataBindings"][0]["query"]["filters"][0]["operator"] =
                json!(operator);
            let operand = if operator == "in" {
                json!([null, nonnull])
            } else {
                Value::Null
            };
            value["payload"]["ux"]["dataBindings"][0]["query"]["filters"][0]["value"] =
                json!({ "kind": "literal", "value": operand });
            assert_clean(&findings_with_support(value.clone(), support.clone(), &[]));
        }
        if nonnull == json!(42) {
            value["payload"]["ux"]["dataBindings"][0]["query"]["filters"][0]["operator"] =
                json!("gt");
            value["payload"]["ux"]["dataBindings"][0]["query"]["filters"][0]["value"] =
                json!({ "kind": "literal", "value": 1.5 });
            assert_issue(
                &findings_with_support(value, support, &[]),
                "/dataBindings/0/query/filters/0/value",
                "integer",
            );
        }
    }
}

#[test]
fn list_sorts_must_fit_the_exact_binding_index_order() {
    let mut multiple_keys = widget();
    multiple_keys["payload"]["ux"]["dataBindings"][0]["query"]["sort"]
        .as_array_mut()
        .expect("sort array")
        .push(json!({
            "property": { "bindingId": "orders", "propertyId": "revision" },
            "direction": "asc"
        }));
    assert_issue(
        &widget_findings(multiple_keys),
        "/dataBindings/0/query/sort",
        "at most one sort key",
    );

    let mut computed_support = support_artifacts();
    computed_support[0]["payload"]["objectType"]["properties"][1]["computed"] =
        json!("self.order_id");
    assert_issue(
        &findings_with_support(widget(), computed_support, &[]),
        "/dataBindings/0/query/sort/0/property",
        "computed sort properties have no stored column",
    );

    let mut timestamp_support = support_artifacts();
    timestamp_support[0]["payload"]["objectType"]["properties"][1]["type"] = json!({
        "kind": "timestamp",
        "timezone": "utc"
    });
    assert_issue(
        &findings_with_support(widget(), timestamp_support, &[]),
        "/dataBindings/0/query/sort/0/property",
        "no exact binding-index order",
    );

    let mut overflow_support = support_artifacts();
    let properties = overflow_support[0]["payload"]["objectType"]["properties"]
        .as_array_mut()
        .expect("order properties");
    for index in 0..100 {
        properties.push(json!({
            "id": format!("extra_field_{index:03}"),
            "type": { "kind": "text" }
        }));
    }
    properties.push(json!({
        "id": "overflow_sort",
        "type": { "kind": "text" }
    }));
    let mut overflow_sort = widget();
    overflow_sort["payload"]["ux"]["dataBindings"][0]["query"]["sort"][0]["property"]["propertyId"] =
        json!("overflow_sort");
    assert_issue(
        &findings_with_support(overflow_sort, overflow_support, &[]),
        "/dataBindings/0/query/sort/0/property",
        "overflow payload",
    );
}

#[test]
fn list_sort_accepts_a_property_inherited_from_the_hierarchy_root() {
    let mut support = support_artifacts();
    support[0]["dependencies"] = json!([{
        "artifactId": "commerce.BaseOrder.entity",
        "kind": "entity",
        "version": "0.1.0",
        "required": true
    }]);
    support[0]["payload"]["objectType"]["extends"] = json!("commerce.BaseOrder");
    support[0]["payload"]["objectType"]["identity"][0]["name"] = json!("order_local");
    support.push(entity(
        "commerce.BaseOrder.entity",
        "commerce.BaseOrder",
        &json!([
            { "id": "base_id", "type": { "kind": "text" } },
            { "id": "base_number", "type": { "kind": "text" } }
        ]),
        "base_id",
        &json!([]),
    ));
    let mut value = widget();
    value["payload"]["ux"]["dataBindings"][0]["query"]["sort"][0]["property"]["propertyId"] =
        json!("base_number");
    assert_clean(&findings_with_support(value, support, &[]));
}

#[test]
fn filters_require_direct_stored_columns_for_binding_indexes() {
    let mut computed_support = support_artifacts();
    computed_support[0]["payload"]["objectType"]["properties"][1]["computed"] =
        json!("self.order_id");
    let mut computed_filter = widget();
    computed_filter["payload"]["ux"]["dataBindings"][0]["query"]["sort"] = json!([]);
    computed_filter["payload"]["ux"]["dataBindings"][0]["query"]["filters"][0]["property"] =
        json!({ "bindingId": "orders", "propertyId": "order_number" });
    computed_filter["payload"]["ux"]["dataBindings"][0]["query"]["filters"][0]["value"] =
        json!({ "kind": "literal", "value": "ORD-100" });
    assert_issue(
        &findings_with_support(computed_filter, computed_support, &[]),
        "/dataBindings/0/query/filters/0/property",
        "computed filter properties have no stored column",
    );

    for (property_type, filter_value) in [
        (
            json!({ "kind": "timestamp", "timezone": "utc" }),
            json!("2026-10-07T00:00:00Z"),
        ),
        (json!({ "kind": "decimal", "scale": 2 }), Value::Null),
    ] {
        let mut support = support_artifacts();
        support[0]["payload"]["objectType"]["properties"][1]["type"] = property_type;
        // Preserve existing decimal literal admission: nullable null is valid.
        support[0]["payload"]["objectType"]["properties"][1]["constraints"] =
            json!({ "nullable": true });
        let mut value = widget();
        value["payload"]["ux"]["fields"][0]["control"] = json!("display");
        value["payload"]["ux"]["dataBindings"][0]["query"]["sort"] = json!([]);
        value["payload"]["ux"]["dataBindings"][0]["query"]["filters"][0]["property"] =
            json!({ "bindingId": "orders", "propertyId": "order_number" });
        value["payload"]["ux"]["dataBindings"][0]["query"]["filters"][0]["value"] =
            json!({ "kind": "literal", "value": filter_value });
        assert_clean(&findings_with_support(value, support, &[]));
    }

    let mut overflow_support = support_artifacts();
    let properties = overflow_support[0]["payload"]["objectType"]["properties"]
        .as_array_mut()
        .expect("order properties");
    for index in 0..100 {
        properties.push(json!({
            "id": format!("extra_field_{index:03}"),
            "type": { "kind": "text" }
        }));
    }
    properties.push(json!({
        "id": "overflow_filter",
        "type": { "kind": "text" }
    }));
    let mut overflow_filter = widget();
    overflow_filter["payload"]["ux"]["dataBindings"][0]["query"]["sort"] = json!([]);
    overflow_filter["payload"]["ux"]["dataBindings"][0]["query"]["filters"][0]["property"] =
        json!({ "bindingId": "orders", "propertyId": "overflow_filter" });
    overflow_filter["payload"]["ux"]["dataBindings"][0]["query"]["filters"][0]["value"] =
        json!({ "kind": "literal", "value": "x" });
    assert_issue(
        &findings_with_support(overflow_filter, overflow_support, &[]),
        "/dataBindings/0/query/filters/0/property",
        "overflow payload",
    );
}

#[test]
fn subtype_owned_sort_properties_are_refused_by_root_order_indexes() {
    let mut support = support_artifacts();
    support[0]["dependencies"] = json!([{
        "artifactId": "commerce.BaseOrder.entity",
        "kind": "entity",
        "version": "0.1.0",
        "required": true
    }]);
    support[0]["payload"]["objectType"]["extends"] = json!("commerce.BaseOrder");
    support[0]["payload"]["objectType"]["identity"][0]["name"] = json!("order_local");
    support.push(entity(
        "commerce.BaseOrder.entity",
        "commerce.BaseOrder",
        &json!([{ "id": "base_id", "type": { "kind": "text" } }]),
        "base_id",
        &json!([]),
    ));
    assert_issue(
        &findings_with_support(widget(), support, &[]),
        "/dataBindings/0/query/sort/0/property",
        "declared below the hierarchy root",
    );
}

#[test]
fn integer_action_inputs_require_a_declared_integer_or_integral_literal() {
    for expression in ["42", "input.record.revision"] {
        let mut value = widget();
        value["payload"]["ux"]["actionBindings"][0]["inputMapping"]["expectedVersion"] =
            json!(expression);
        assert_clean(&widget_findings(value));
    }
    for expression in ["1.5", "input.record.revision + 1", "3 / 2"] {
        let mut value = widget();
        value["payload"]["ux"]["actionBindings"][0]["inputMapping"]["expectedVersion"] =
            json!(expression);
        assert_issue(
            &widget_findings(value),
            "/actionBindings/0/inputMapping/expectedVersion",
            "integer",
        );
    }
}

#[test]
fn nested_integer_action_inputs_cannot_admit_fractional_expressions() {
    let mut value = widget();
    value["payload"]["ux"]["actionBindings"][0]["inputMapping"]["metadata"] =
        json!("{source: \"ui\", count: 1.5}");
    let mut support = support_artifacts();
    support[2]["payload"]["inputSchema"]["properties"]["metadata"]["properties"]["count"] =
        json!({ "type": "integer" });
    assert_issue(
        &findings_with_support(value, support, &[]),
        "/actionBindings/0/inputMapping/metadata",
        "integer",
    );
}

#[test]
fn integer_array_action_inputs_cannot_admit_fractional_elements() {
    let mut value = widget();
    value["payload"]["ux"]["actionBindings"][0]["inputMapping"]["metadata"] =
        json!("{source: \"ui\", samples: [1.5]}");
    let mut support = support_artifacts();
    support[2]["payload"]["inputSchema"]["properties"]["metadata"]["properties"]["samples"] =
        json!({ "type": "array", "items": { "type": "integer" } });
    assert_issue(
        &findings_with_support(value, support, &[]),
        "/actionBindings/0/inputMapping/metadata",
        "integer",
    );
}

fn integer_metadata_findings(expression: &str, member: &str, schema: Value) -> Vec<Diagnostic> {
    let mut value = widget();
    value["payload"]["ux"]["actionBindings"][0]["inputMapping"]["metadata"] = json!(expression);
    let mut support = support_artifacts();
    support[2]["payload"]["inputSchema"]["properties"]["metadata"]["properties"][member] = schema;
    findings_with_support(value, support, &[])
}

#[test]
fn constructed_integer_object_members_keep_their_literal_and_direct_source_proofs() {
    for expression in [
        "{source: 'ui', count: 42}",
        "({source: 'ui', count: (input.record.revision)})",
    ] {
        assert_clean(&integer_metadata_findings(
            expression,
            "count",
            json!({ "type": "integer" }),
        ));
    }
    for expression in [
        "{source: 'ui', count: 1.5}",
        "{source: 'ui', count: 1 + 1}",
        "{source: 'ui', count: input.record.revision + 1}",
    ] {
        assert_issue(
            &integer_metadata_findings(expression, "count", json!({ "type": "integer" })),
            "/actionBindings/0/inputMapping/metadata",
            "integer",
        );
    }
}

#[test]
fn constructed_integer_arrays_keep_every_members_proof_after_type_joining() {
    let schema = json!({ "type": "array", "items": { "type": "integer" } });
    for expression in [
        "{source: 'ui', samples: [(1), input.record.revision, 2]}",
        "{source: 'ui', samples: [42.0]}",
    ] {
        assert_clean(&integer_metadata_findings(
            expression,
            "samples",
            schema.clone(),
        ));
    }
    for expression in [
        "{source: 'ui', samples: [1, 1.5]}",
        "{source: 'ui', samples: [1, input.record.revision + 1]}",
    ] {
        assert_issue(
            &integer_metadata_findings(expression, "samples", schema.clone()),
            "/actionBindings/0/inputMapping/metadata",
            "integer",
        );
    }
    assert_issue(
        &integer_metadata_findings(
            "{source: 'ui', samples: [1, null, input.record.revision]}",
            "samples",
            json!({ "type": "array", "items": { "type": ["integer", "null"] } }),
        ),
        "/actionBindings/0/inputMapping/metadata",
        "may be null",
    );
}

#[test]
fn nullable_integer_record_members_keep_every_nonnull_source_proof() {
    let schema = json!({
        "type": "array", "items": {
            "type": "object", "properties": {
                "count": { "type": ["integer", "null"] }
            },
            "required": ["count"], "additionalProperties": false
        }
    });
    assert_clean(&integer_metadata_findings(
        "{source: 'ui', rows: [{count: 1}, {count: null}]}",
        "rows",
        schema.clone(),
    ));
    assert_issue(
        &integer_metadata_findings(
            "{source: 'ui', rows: [{count: 1}, {count: null}, {count: 1.5}]}",
            "rows",
            schema,
        ),
        "/actionBindings/0/inputMapping/metadata",
        "integer",
    );
}

#[test]
fn nested_integer_lists_and_records_validate_every_constructed_leaf() {
    let schema = json!({
        "type": "array", "items": {
            "type": "object", "properties": {
                "counts": { "type": "array", "items": { "type": "integer" } }
            },
            "required": ["counts"], "additionalProperties": false
        }
    });
    assert_clean(&integer_metadata_findings(
        "{source: 'ui', rows: [{counts: [1, input.record.revision]}, {counts: [(2), 3]}]}",
        "rows",
        schema.clone(),
    ));
    for expression in [
        "{source: 'ui', rows: [{counts: [1]}, {counts: [2, 1.5]}]}",
        "{source: 'ui', rows: [{counts: [1]}, {counts: [input.record.revision + 1]}]}",
    ] {
        assert_issue(
            &integer_metadata_findings(expression, "rows", schema.clone()),
            "/actionBindings/0/inputMapping/metadata",
            "integer",
        );
    }
    let matrix = json!({
        "type": "array", "items": { "type": "array", "items": { "type": "integer" } }
    });
    assert_clean(&integer_metadata_findings(
        "{source: 'ui', matrix: [[1, 2], [input.record.revision, 3]]}",
        "matrix",
        matrix.clone(),
    ));
    assert_issue(
        &integer_metadata_findings("{source: 'ui', matrix: [[1], [2, 1.5]]}", "matrix", matrix),
        "/actionBindings/0/inputMapping/metadata",
        "integer",
    );
}

#[test]
fn undeclared_action_input_mappings_are_rejected() {
    let mut value = widget();
    value["payload"]["ux"]["actionBindings"][0]["inputMapping"]["undeclared"] =
        json!("input.draft.reason");
    assert_issue(
        &widget_findings(value),
        "/actionBindings/0/inputMapping/undeclared",
        "not declared",
    );
}

#[test]
fn omitted_nullable_and_nested_optional_action_inputs_are_valid() {
    // The fixture intentionally maps only the required ActionIR members.
    // `note`, `comment`, and `metadata` are optional ActionIR members, so
    // none has to be fabricated in a mapping or form.
    assert_clean(&widget_findings(widget()));
}

#[test]
fn null_literal_matches_only_nullable_action_inputs() {
    let mut nullable = widget();
    nullable["payload"]["ux"]["actionBindings"][0]["inputMapping"]["comment"] = json!("null");
    assert_clean(&widget_findings(nullable));

    let mut nonnullable = widget();
    nonnullable["payload"]["ux"]["actionBindings"][0]["inputMapping"]["reason"] = json!("null");
    assert_issue(
        &widget_findings(nonnullable),
        "/actionBindings/0/inputMapping/reason",
        "incompatible",
    );
}

#[test]
fn an_optional_nonnullable_action_input_can_be_included_and_mapped_from_its_form() {
    let mut value = widget();
    let ux = &mut value["payload"]["ux"];
    ux["forms"][0]["inputNames"]
        .as_array_mut()
        .expect("input names")
        .push(json!("note"));
    ux["actionBindings"][0]["inputMapping"]["note"] = json!("input.draft.note");
    ux["fields"]
        .as_array_mut()
        .expect("fields array")
        .push(json!({
            "fieldId": "approve-note",
            "target": { "kind": "form_input", "formId": "approve-form", "inputName": "note" },
            "label": { "en": "Note" },
            "control": "text"
        }));
    ux["pages"][0]["root"]["children"][2]["children"]
        .as_array_mut()
        .expect("form children")
        .push(json!({
            "nodeId": "approve-note-node",
            "semanticId": "approve-note-field",
            "component": { "kind": "field", "fieldId": "approve-note" },
            "children": []
        }));
    assert_clean(&widget_findings(value));
}

#[test]
fn a_nullable_record_value_cannot_map_to_an_optional_nonnullable_action_input() {
    let mut value = widget();
    value["payload"]["ux"]["actionBindings"][0]["inputMapping"]["note"] =
        json!("input.record.optional_note");
    assert_issue(
        &widget_findings(value),
        "/actionBindings/0/inputMapping/note",
        "may be null",
    );
}

#[test]
fn a_nested_action_input_mapping_is_checked_recursively() {
    let mut value = widget();
    value["payload"]["ux"]["actionBindings"][0]["inputMapping"]["metadata"] =
        json!("input.draft.reason");
    assert_issue(
        &widget_findings(value),
        "/actionBindings/0/inputMapping/metadata",
        "incompatible",
    );
}

#[test]
fn explicitly_including_an_optional_form_input_requires_a_mapping_and_field() {
    let mut value = widget();
    let ux = &mut value["payload"]["ux"];
    ux["forms"][0]["inputNames"] = json!(["orderId", "reason", "comment"]);
    ux["fields"]
        .as_array_mut()
        .expect("fields array")
        .push(json!({
            "fieldId": "approve-comment",
            "target": { "kind": "form_input", "formId": "approve-form", "inputName": "comment" },
            "label": { "en": "Comment" },
            "control": "text_area"
        }));
    ux["pages"][0]["root"]["children"][2]["children"]
        .as_array_mut()
        .expect("form children")
        .push(json!({
            "nodeId": "approve-comment-node",
            "semanticId": "approve-comment-field",
            "component": { "kind": "field", "fieldId": "approve-comment" },
            "children": []
        }));
    assert_issue(
        &widget_findings(value),
        "/forms/0/inputNames/2",
        "no action input mapping",
    );
}

#[test]
fn required_display_form_inputs_need_a_valid_initial_value() {
    let mut value = widget();
    assert_clean(&widget_findings(value.clone()));
    let mut display_without_initial = widget();
    display_without_initial["payload"]["ux"]["fields"][5]["control"] = json!("display");
    assert_issue(
        &widget_findings(display_without_initial),
        "/fields/5/target/initial",
        "required display form input",
    );
    let _ = value["payload"]["ux"]["fields"][4]["target"]
        .as_object_mut()
        .expect("form input target")
        .remove("initial");
    assert_issue(
        &widget_findings(value),
        "/fields/4/target/initial",
        "required display form input",
    );

    let mut wrong_type = widget();
    wrong_type["payload"]["ux"]["fields"][4]["target"]["initial"]["propertyId"] = json!("revision");
    assert_issue(
        &widget_findings(wrong_type),
        "/fields/4/target/initial",
        "type does not fit",
    );

    let mut unavailable = widget();
    unavailable["payload"]["ux"]["forms"][0]["dataBindingId"] = json!("order-customers");
    unavailable["payload"]["ux"]["fields"][4]["target"]["initial"] = json!({
        "bindingId": "order-customers", "propertyId": "display_name"
    });
    assert_issue(
        &widget_findings(unavailable),
        "/fields/4/target/initial",
        "unavailable from the form's record source",
    );
}

#[test]
fn an_optional_display_form_input_needs_no_prefill() {
    let mut value = widget();
    let ux = &mut value["payload"]["ux"];
    ux["forms"][0]["inputNames"] = json!(["orderId", "reason", "comment"]);
    ux["actionBindings"][0]["inputMapping"]["comment"] = json!("input.draft.comment");
    ux["fields"]
        .as_array_mut()
        .expect("fields array")
        .push(json!({
            "fieldId": "approve-comment-display",
            "target": {
                "kind": "form_input",
                "formId": "approve-form",
                "inputName": "comment"
            },
            "label": { "en": "Comment" },
            "control": "display"
        }));
    ux["pages"][0]["root"]["children"][2]["children"]
        .as_array_mut()
        .expect("form children")
        .push(json!({
            "nodeId": "approve-comment-display-node",
            "component": { "kind": "field", "fieldId": "approve-comment-display" },
            "children": []
        }));
    assert_clean(&widget_findings(value));
}

#[test]
fn composed_control_overrides_keep_required_form_initialization_metadata() {
    let baseline = required_form_input_baseline();
    assert_clean(&findings(baseline.clone()));
    for target in ["baseline-required-alias", "baseline-required-reason"] {
        let mut host = compose_baseline(widget());
        host["payload"]["ux"]["overrides"] = json!([{
            "targetSemanticId": target,
            "value": { "kind": "control", "control": "display" }
        }]);
        assert_issue(
            &findings_with_base(host, std::slice::from_ref(&baseline)),
            "/overrides/0/value/control",
            "required display form input",
        );
    }

    let mut initialized = required_form_input_baseline();
    initialized["dependencies"]
        .as_array_mut()
        .expect("dependencies array")
        .push(json!({
            "artifactId": "crm.Customer.entity",
            "kind": "entity",
            "version": "0.1.0",
            "required": true
        }));
    let detail_binding = baseline_widget("baseline-detail");
    initialized["payload"]["ux"]["dataBindings"] =
        json!([detail_binding["payload"]["ux"]["dataBindings"][0].clone()]);
    initialized["payload"]["ux"]["forms"][0]["dataBindingId"] = json!("orders");
    initialized["payload"]["ux"]["fields"][0]["target"]["initial"] = json!({
        "bindingId": "orders", "propertyId": "revision"
    });
    assert_clean(&findings(initialized.clone()));
    let mut host = compose_baseline(widget());
    host["payload"]["ux"]["overrides"] = json!([{
        "targetSemanticId": "baseline-required-alias",
        "value": { "kind": "control", "control": "display" }
    }]);
    assert_clean(&findings_with_base(host, &[initialized]));
}

#[test]
fn conditions_must_be_boolean_and_use_only_declared_inputs() {
    let mut non_boolean = widget();
    non_boolean["payload"]["ux"]["fields"][0]["visibleWhen"]["expression"] =
        json!("input.record.status");
    assert_issue(
        &widget_findings(non_boolean),
        "/fields/0/visibleWhen",
        "Boolean",
    );

    let mut undeclared = widget();
    undeclared["payload"]["ux"]["fields"][0]["visibleWhen"]["expression"] =
        json!("input.actor.role == \"admin\"");
    assert_issue(
        &widget_findings(undeclared),
        "/fields/0/visibleWhen",
        "type-check",
    );
}

#[test]
fn orphaned_overrides_and_duplicate_node_ids_are_rejected() {
    let mut orphan = widget();
    orphan["payload"]["ux"]["overrides"]
        .as_array_mut()
        .expect("overrides array")
        .push(json!({
            "targetSemanticId": "missing-semantic-id",
            "value": { "kind": "label", "value": { "en": "Missing" } }
        }));
    assert_issue(
        &widget_findings(orphan),
        "/overrides/0/targetSemanticId",
        "composed baseline page",
    );

    let mut duplicate = widget();
    duplicate["payload"]["ux"]["pages"][0]["root"]["children"][0]["nodeId"] = json!("orders-page");
    assert_issue(
        &widget_findings(duplicate),
        "/pages/0/root/children/0/nodeId",
        "unique",
    );
}

#[test]
fn list_and_traversal_bounds_are_required_and_positive() {
    let mut zero_list_maximum = widget();
    zero_list_maximum["payload"]["ux"]["dataBindings"][0]["query"]["page"]["maxSize"] = json!(0);
    assert_issue(
        &widget_findings(zero_list_maximum),
        "/dataBindings/0/query/page",
        "positive",
    );

    let mut zero_traversal = widget();
    zero_traversal["payload"]["ux"]["dataBindings"][1]["query"]["maxRows"] = json!(0);
    assert_issue(
        &widget_findings(zero_traversal),
        "/dataBindings/1/query",
        "positive",
    );

    let mut missing_traversal_bound = widget();
    let _ = missing_traversal_bound["payload"]["ux"]["dataBindings"][1]["query"]
        .as_object_mut()
        .expect("traversal query object")
        .remove("maxRows");
    assert_issue(
        &widget_findings(missing_traversal_bound),
        "/payload/ux",
        "missing field",
    );

    let mut missing_list_bound = widget();
    let _ = missing_list_bound["payload"]["ux"]["dataBindings"][0]["query"]["page"]
        .as_object_mut()
        .expect("list page bounds")
        .remove("maxSize");
    assert_issue(
        &widget_findings(missing_list_bound),
        "/payload/ux",
        "missing field",
    );
}

#[test]
fn link_traversals_must_use_a_declared_link_and_its_exact_target() {
    let mut unknown_link = widget();
    unknown_link["payload"]["ux"]["dataBindings"][1]["query"]["linkId"] = json!("unknown_link");
    assert_issue(
        &widget_findings(unknown_link),
        "/dataBindings/1/query/linkId",
        "not declared",
    );

    let mut wrong_target = widget();
    wrong_target["payload"]["ux"]["dataBindings"][1]["object"] = json!({
        "id": "commerce.Order.entity",
        "version": "0.1.0"
    });
    assert_issue(
        &widget_findings(wrong_target),
        "/dataBindings/1/object",
        "target",
    );
}

#[test]
fn control_type_and_navigation_destination_are_checked() {
    let mut wrong_control = widget();
    wrong_control["payload"]["ux"]["fields"][0]["control"] = json!("number");
    assert_issue(
        &widget_findings(wrong_control),
        "/fields/0/control",
        "incompatible",
    );

    let mut missing_page = widget();
    missing_page["payload"]["ux"]["navigationItems"][0]["destination"]["pageId"] =
        json!("unknown-page");
    assert_issue(
        &widget_findings(missing_page),
        "/navigationItems/0/destination/pageId",
        "does not exist",
    );
}

#[test]
fn capability_requirements_must_be_named() {
    let mut value = widget();
    value["payload"]["ux"]["navigationItems"][0]["requiredCapability"] = json!(" ");
    assert_issue(
        &widget_findings(value),
        "/navigationItems/0/requiredCapability",
        "must not be blank",
    );
}

#[test]
fn app_navigation_cannot_use_a_row_only_record_value() {
    let mut value = widget();
    value["payload"]["ux"]["app"]["navigationItemIds"] = json!(["customer-nav"]);
    assert_issue(
        &widget_findings(value),
        "/navigationItems/0/destination/parameters/customerNumber",
        "not available in this list row or detail context",
    );
}

#[test]
fn form_submit_confirmation_requires_the_strength_and_phrase_for_its_severity() {
    let mut irreversible_without_typed = widget();
    irreversible_without_typed["payload"]["ux"]["forms"][0]["submit"]["confirmation"]["severity"] =
        json!("irreversible");
    irreversible_without_typed["payload"]["ux"]["forms"][0]["submit"]["confirmation"]["strength"] =
        json!("none");
    assert_issue(
        &widget_findings(irreversible_without_typed),
        "/forms/0/submit/confirmation/strength",
        "irreversible actions need typed confirmation",
    );

    let mut typed_without_phrase = widget();
    typed_without_phrase["payload"]["ux"]["forms"][0]["submit"]["confirmation"]["severity"] =
        json!("irreversible");
    typed_without_phrase["payload"]["ux"]["forms"][0]["submit"]["confirmation"]["strength"] =
        json!("typed");
    let _ = typed_without_phrase["payload"]["ux"]["forms"][0]["submit"]["confirmation"]
        .as_object_mut()
        .expect("confirmation object")
        .remove("phrase");
    assert_issue(
        &widget_findings(typed_without_phrase),
        "/forms/0/submit/confirmation/phrase",
        "typed confirmation requires nonblank localized confirmation text",
    );
}

#[test]
fn optional_array_action_inputs_accept_an_empty_array_mapping() {
    let mut array_action = action_artifact();
    array_action["artifactId"] = json!("commerce.order.tag.action");
    array_action["name"] = json!("Tag orders");
    array_action["payload"]["actionId"] = json!("commerce.order.tag");
    array_action["payload"]["inputSchema"] = json!({
        "type": "object",
        "properties": {
            "tags": { "type": "array", "items": { "type": "string" } }
        },
        "required": [],
        "additionalProperties": false
    });

    let mut value = widget();
    value["dependencies"]
        .as_array_mut()
        .expect("dependencies array")
        .push(json!({
            "artifactId": "commerce.order.tag.action",
            "kind": "action",
            "version": "0.1.0",
            "required": true
        }));
    value["payload"]["ux"]["actionBindings"]
        .as_array_mut()
        .expect("action bindings array")
        .push(json!({
            "bindingId": "tag-orders",
            "action": { "id": "commerce.order.tag.action", "version": "0.1.0" },
            "inputMapping": { "tags": "[]" },
            "inputs": {},
            "requiredCapability": "commerce.order.tag"
        }));
    let mut support = support_artifacts();
    support.push(array_action);
    assert_clean(&findings_with_support(value, support, &[]));
}

#[test]
fn multiple_selection_is_not_a_global_form_prefill_record() {
    let mut value = widget();
    value["payload"]["ux"]["pages"][0]["root"]["children"][0]["component"]["selection"] =
        json!("multiple");
    let _ = value["payload"]["ux"]["actionPlacements"][0]
        .as_object_mut()
        .expect("opener placement")
        .remove("formId");
    assert_issue(
        &widget_findings(value),
        "/forms/0/dataBindingId",
        "prefill requires a declared record or list-row source",
    );
}

fn edit_form_widget() -> Value {
    let mut value = widget();
    value["dependencies"]
        .as_array_mut()
        .expect("dependencies array")
        .push(json!({
            "artifactId": "commerce.order.edit.action",
            "kind": "action",
            "version": "0.1.0",
            "required": true
        }));
    let ux = &mut value["payload"]["ux"];
    ux["dataBindings"]
        .as_array_mut()
        .expect("bindings array")
        .push(json!({
            "bindingId": "order-detail",
            "object": { "id": "commerce.Order.entity", "version": "0.1.0" },
            "query": {
                "kind": "object_detail",
                "key": [{
                    "propertyId": "order_id",
                    "value": { "kind": "literal", "value": "ORD-100" }
                }]
            }
        }));
    ux["actionBindings"][0]["action"] = json!({
        "id": "commerce.order.edit.action",
        "version": "0.1.0"
    });
    ux["forms"][0]["mode"] = json!("edit");
    ux["forms"][0]["dataBindingId"] = json!("order-detail");
    ux["forms"][0]["versionConflictBinding"] = json!({
        "property": { "bindingId": "order-detail", "propertyId": "revision" },
        "inputName": "expectedVersion",
        "onConflict": "reject_and_retain_draft"
    });
    ux["actionBindings"][0]["inputs"]["recordBindingId"] = json!("order-detail");
    ux["actionBindings"][0]["inputMapping"]["expectedVersion"] = json!("input.record.revision");
    ux["actionPlacements"][0]["enabledWhen"]["inputs"]["recordBindingId"] = json!("order-detail");
    ux["fields"][4]["target"]["initial"] = json!({
        "bindingId": "order-detail",
        "propertyId": "order_id"
    });
    ux["actionPlacements"][0]["location"] = json!("detail");
    let _ = ux["actionPlacements"][0]
        .as_object_mut()
        .expect("placement object")
        .remove("selectionBindingId");
    let action_node = ux["pages"][0]["root"]["children"][0]["children"]
        .as_array_mut()
        .expect("orders list children")
        .remove(1);
    ux["pages"][0]["root"]["children"]
        .as_array_mut()
        .expect("page root children")
        .push(json!({
            "nodeId": "order-detail-edit-surface-node",
            "semanticId": "order-detail-edit-surface",
            "component": { "kind": "detail", "dataBindingId": "order-detail" },
            "children": [action_node]
        }));
    value
}

#[test]
fn edit_forms_require_a_matching_version_input_mapping() {
    let valid = edit_form_widget();
    let mut support = support_artifacts();
    support.push(required_version_action_artifact());
    assert_clean(&findings_with_support(valid.clone(), support.clone(), &[]));

    let mut wrong_source = valid;
    wrong_source["payload"]["ux"]["actionBindings"][0]["inputs"]["recordBindingId"] =
        json!("orders");
    wrong_source["payload"]["ux"]["actionBindings"][0]["inputMapping"]["expectedVersion"] =
        json!("input.record.revision");
    assert_issue(
        &findings_with_support(wrong_source, support, &[]),
        "/forms/0/versionConflictBinding/inputName",
        "matching edited record",
    );
}

#[test]
fn forms_must_submit_through_a_declared_action_binding() {
    let mut value = widget();
    value["payload"]["ux"]["forms"][0]["submitBindingId"] = json!("missing-action");
    assert_issue(
        &widget_findings(value),
        "/forms/0/submitBindingId",
        "does not exist",
    );
}

#[test]
fn references_require_exact_envelope_dependencies_and_compatible_scope() {
    let mut missing_dependency = widget();
    missing_dependency["dependencies"] = json!([
        { "artifactId": "commerce.Order.entity", "kind": "entity", "version": "0.1.0", "required": true },
        { "artifactId": "commerce.order.approve.action", "kind": "action", "version": "0.1.0", "required": true }
    ]);
    assert_issue(
        &widget_findings(missing_dependency),
        "/dataBindings/1/object",
        "dependency",
    );

    let mut customer = customer_entity();
    customer["scope"]["orgId"] = json!("org_elsewhere");
    let artifacts: Vec<_> = [order_entity(), customer, action_artifact(), widget()]
        .into_iter()
        .map(envelope)
        .collect();
    let found = check_artifact_set(
        &artifacts,
        &EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &[],
            tenant: None,
        },
    );
    assert!(
        found.iter().any(|finding| {
            finding.artifact_id.as_deref() == Some(WIDGET_ID)
                && finding.code.as_str() == "artifact_set.scope.incompatible_reference"
        }),
        "expected scope incompatibility on the widget reference; got {found:#?}"
    );
}

fn baseline_widget(semantic_id: &str) -> Value {
    json!({
        "irVersion": "0.1",
        "artifactId": "commerce.widget.baseline",
        "kind": "widget",
        "namespace": "commerce",
        "name": "Baseline widget",
        "version": "0.1.0",
        "lifecycle": "draft",
        "scope": { "orgId": "org_demo", "environmentId": "dev" },
        "dependencies": [{
            "artifactId": "crm.Customer.entity",
            "kind": "entity",
            "version": "0.1.0",
            "required": true
        }],
        "provenance": { "source": "authoring_sdk" },
        "payload": {
            "widgetVersion": "0.1",
            "widgetId": "commerce.widget.baseline",
            "dataDependencies": [],
            "viewStates": [{ "stateId": "ready", "kind": "ready" }],
            "actionBindings": [],
            "permissions": [],
            "ux": {
                "uxVersion": "0.1",
                "pages": [{
                    "pageId": "baseline-page",
                    "name": "Baseline",
                    "contextParams": [],
                    "root": {
                        "nodeId": "baseline-root",
                        "semanticId": semantic_id,
                        "component": { "kind": "field", "fieldId": "baseline-revision-field" },
                        "children": []
                    }
                }],
                "dataBindings": [{
                    "bindingId": "orders",
                    "object": { "id": "crm.Customer.entity", "version": "0.1.0" },
                    "query": {
                        "kind": "object_detail",
                        "key": [{ "propertyId": "customer_number", "value": { "kind": "literal", "value": "C-001" } }]
                    }
                }],
                "actionBindings": [],
                "fields": [{
                    "fieldId": "baseline-revision-field",
                    "target": { "kind": "property", "property": { "bindingId": "orders", "propertyId": "revision" } },
                    "label": { "en": "Revision" },
                    "control": "text"
                }],
                "forms": [],
                "actionPlacements": [],
                "navigationItems": [],
                "overrides": []
            }
        }
    })
}

fn required_form_input_baseline() -> Value {
    let mut baseline = baseline_widget("baseline-form-surface");
    baseline["dependencies"] = json!([{
        "artifactId": "commerce.order.approve.action",
        "kind": "action",
        "version": "0.1.0",
        "required": true
    }]);
    let ux = &mut baseline["payload"]["ux"];
    ux["dataBindings"] = json!([]);
    ux["actionBindings"] = json!([{
        "bindingId": "baseline-submit",
        "action": { "id": "commerce.order.approve.action", "version": "0.1.0" },
        "inputMapping": {
            "orderId": "\"ORD-100\"",
            "reason": "input.draft.reason"
        },
        "inputs": { "draftFormId": "baseline-form" },
        "requiredCapability": "commerce.order.approve"
    }]);
    ux["forms"] = json!([{
        "formId": "baseline-form",
        "mode": "action_input",
        "inputNames": ["reason"],
        "submitBindingId": "baseline-submit",
        "submit": {
            "label": { "en": "Approve" },
            "confirmation": { "severity": "safe", "strength": "none" }
        }
    }]);
    ux["fields"] = json!([{
        "fieldId": "baseline-required-reason",
        "target": {
            "kind": "form_input",
            "formId": "baseline-form",
            "inputName": "reason"
        },
        "label": { "en": "Reason" },
        "control": "text_area"
    }]);
    ux["pages"][0]["root"]["component"] = json!({ "kind": "form", "formId": "baseline-form" });
    ux["pages"][0]["root"]["children"] = json!([{
        "nodeId": "baseline-required-reason-node",
        "semanticId": "baseline-required-alias",
        "component": { "kind": "field", "fieldId": "baseline-required-reason" },
        "children": []
    }]);
    baseline
}

fn compose_baseline(mut value: Value) -> Value {
    // This helper tests overrides against imported baseline ids, not the
    // fixture's own customization target.
    value["payload"]["ux"]["overrides"] = json!([]);
    value["dependencies"]
        .as_array_mut()
        .expect("dependencies array")
        .push(json!({
            "artifactId": "commerce.widget.baseline",
            "kind": "widget",
            "version": "0.1.0",
            "required": true
        }));
    value["payload"]["ux"]["pages"][0]["root"]["children"]
        .as_array_mut()
        .expect("page children")
        .push(json!({
            "nodeId": "baseline-composition-node",
            "semanticId": "baseline-composition",
            "component": {
                "kind": "composition",
                "widget": { "id": "commerce.widget.baseline", "version": "0.1.0" },
                "pageId": "baseline-page"
            },
            "children": []
        }));
    value
}

#[test]
fn imported_override_types_use_the_baseline_binding_context() {
    let mut value = compose_baseline(widget());
    value["payload"]["ux"]["overrides"]
        .as_array_mut()
        .expect("overrides array")
        .push(json!({
            "targetSemanticId": "baseline-revision",
            "value": { "kind": "control", "control": "number" }
        }));
    let base = [baseline_widget("baseline-revision")];
    assert_issue(
        &findings_with_base(value, &base),
        "/overrides/0/control",
        "incompatible",
    );
}

#[test]
fn imported_select_override_uses_the_baseline_option_binding_context() {
    let mut baseline = baseline_widget("baseline-revision");
    baseline["payload"]["ux"]["dataBindings"]
        .as_array_mut()
        .expect("bindings array")
        .push(json!({
            "bindingId": "customer-options",
            "object": { "id": "crm.Customer.entity", "version": "0.1.0" },
            "query": {
                "kind": "object_list",
                "filters": [],
                "sort": [],
                "page": { "size": 10, "maxSize": 10 }
            }
        }));
    baseline["payload"]["ux"]["fields"][0]["control"] = json!("select");
    baseline["payload"]["ux"]["fields"][0]["optionQueryBindingId"] = json!("customer-options");

    let mut value = compose_baseline(widget());
    value["payload"]["ux"]["overrides"] = json!([{
        "targetSemanticId": "baseline-revision",
        "value": { "kind": "control", "control": "select" }
    }]);
    assert_clean(&findings_with_base(value, &[baseline]));
}

fn enum_select_baseline(control: &str, option_key_type: Option<Value>) -> (Value, Vec<Value>) {
    let mut baseline = baseline_widget("baseline-revision");
    let mut support = support_artifacts();
    support[1]["payload"]["objectType"]["properties"][2]["type"] = json!({
        "kind": "enum", "values": [{ "value": "draft" }, { "value": "approved" }]
    });
    baseline["payload"]["ux"]["fields"][0]["control"] = json!(control);
    if let Some(key_type) = option_key_type {
        support.push(entity(
            "commerce.StatusOption.entity",
            "commerce.StatusOption",
            &json!([{ "id": "status_id", "type": key_type }]),
            "status_id",
            &json!([]),
        ));
        baseline["dependencies"]
            .as_array_mut()
            .expect("dependencies array")
            .push(json!({
                "artifactId": "commerce.StatusOption.entity", "kind": "entity",
                "version": "0.1.0", "required": true
            }));
        baseline["payload"]["ux"]["dataBindings"]
            .as_array_mut()
            .expect("data bindings array")
            .push(json!({
                "bindingId": "status-options",
                "object": { "id": "commerce.StatusOption.entity", "version": "0.1.0" },
                "query": {
                    "kind": "object_list", "filters": [], "sort": [],
                    "page": { "size": 10, "maxSize": 10 }
                }
            }));
        baseline["payload"]["ux"]["fields"][0]["optionQueryBindingId"] = json!("status-options");
    }
    (baseline, support)
}

#[test]
fn enum_selects_with_option_queries_require_compatible_identity_types() {
    let matching_enum = json!({
        "kind": "enum", "values": [{ "value": "draft" }, { "value": "approved" }]
    });
    for option_key in [None, Some(matching_enum)] {
        let (value, support) = enum_select_baseline("select", option_key);
        assert_clean(&findings_with_support(value, support, &[]));
    }
    for option_key in [
        json!({ "kind": "integer" }),
        json!({ "kind": "enum", "values": [{ "value": "draft" }, { "value": "cancelled" }] }),
    ] {
        let (value, support) = enum_select_baseline("select", Some(option_key));
        assert_issue(
            &findings_with_support(value, support, &[]),
            "/fields/0/control",
            "incompatible",
        );
    }
}

#[test]
fn imported_enum_select_overrides_validate_the_option_query_identity_type() {
    for (option_key, compatible) in [
        (None, true),
        (
            Some(json!({
                "kind": "enum", "values": [{ "value": "draft" }, { "value": "approved" }]
            })),
            true,
        ),
        (Some(json!({ "kind": "integer" })), false),
    ] {
        let (baseline, support) = enum_select_baseline("display", option_key);
        assert_clean(&findings_with_support(
            baseline.clone(),
            support.clone(),
            &[],
        ));
        let mut value = compose_baseline(widget());
        value["payload"]["ux"]["overrides"] = json!([{
            "targetSemanticId": "baseline-revision-field",
            "value": { "kind": "control", "control": "select" }
        }]);
        let found = findings_with_support(value, support, &[baseline]);
        if compatible {
            assert_clean(&found);
        } else {
            assert_issue(&found, "/overrides/0/control", "incompatible");
        }
    }
}

#[test]
fn composing_one_page_keeps_unrelated_page_forms_and_action_mappings_valid() {
    let mut baseline = baseline_widget("baseline-revision");
    baseline["dependencies"]
        .as_array_mut()
        .expect("dependencies array")
        .push(json!({
            "artifactId": "commerce.order.approve.action",
            "kind": "action",
            "version": "0.1.0",
            "required": true
        }));
    let ux = &mut baseline["payload"]["ux"];
    ux["actionBindings"] = json!([{
        "bindingId": "unrelated-submit",
        "action": { "id": "commerce.order.approve.action", "version": "0.1.0" },
        "inputMapping": {
            "orderId": "input.draft.orderId",
            "reason": "input.draft.reason"
        },
        "inputs": { "draftFormId": "unrelated-form" },
        "requiredCapability": "commerce.order.approve"
    }]);
    ux["forms"] = json!([{
        "formId": "unrelated-form",
        "mode": "action_input",
        "inputNames": ["orderId", "reason"],
        "submitBindingId": "unrelated-submit",
        "submit": {
            "label": { "en": "Approve" },
            "confirmation": { "severity": "confirm", "strength": "click" }
        }
    }]);
    ux["fields"] = json!([{
        "fieldId": "baseline-revision-field",
        "target": {
            "kind": "property",
            "property": { "bindingId": "orders", "propertyId": "revision" }
        },
        "label": { "en": "Revision" },
        "control": "text"
    }, {
        "fieldId": "unrelated-order-id",
        "target": { "kind": "form_input", "formId": "unrelated-form", "inputName": "orderId" },
        "label": { "en": "Order ID" },
        "control": "text"
    }, {
        "fieldId": "unrelated-reason",
        "target": { "kind": "form_input", "formId": "unrelated-form", "inputName": "reason" },
        "label": { "en": "Reason" },
        "control": "text_area"
    }]);
    ux["pages"]
        .as_array_mut()
        .expect("pages array")
        .push(json!({
            "pageId": "unrelated-page",
            "name": "Unrelated action form",
            "contextParams": [],
            "root": {
                "nodeId": "unrelated-root",
                "semanticId": "unrelated-root-semantic",
                "component": { "kind": "form", "formId": "unrelated-form" },
                "children": [{
                    "nodeId": "unrelated-order-id-node",
                    "semanticId": "unrelated-order-id-semantic",
                    "component": { "kind": "field", "fieldId": "unrelated-order-id" },
                    "children": []
                }, {
                    "nodeId": "unrelated-reason-node",
                    "semanticId": "unrelated-reason-semantic",
                    "component": { "kind": "field", "fieldId": "unrelated-reason" },
                    "children": []
                }]
            }
        }));

    // The whole source Widget stays internally valid, even though its
    // unrelated action-input page is not the page selected for composition.
    assert_clean(&findings(baseline.clone()));
    let mut host = widget();
    host["payload"]["ux"]["pages"][1]["root"]["semanticId"] = json!("unrelated-root-semantic");
    assert_clean(&findings_with_base(compose_baseline(host), &[baseline]));
}

fn widget_with_secondary_footer_action() -> Value {
    let mut value = widget();
    let ux = &mut value["payload"]["ux"];
    ux["actionBindings"]
        .as_array_mut()
        .expect("action bindings array")
        .push(json!({
            "bindingId": "approve-secondary",
            "action": { "id": "commerce.order.approve.action", "version": "0.1.0" },
            "inputMapping": {
                "orderId": "input.draft.orderId",
                "reason": "input.draft.reason"
            },
            "inputs": { "draftFormId": "approve-form" },
            "requiredCapability": "commerce.order.approve"
        }));
    ux["actionPlacements"]
        .as_array_mut()
        .expect("placements array")
        .push(json!({
            "placementId": "approve-secondary-footer",
            "bindingId": "approve-secondary",
            "label": { "en": "Save draft" },
            "location": "form_footer",
            "order": 2,
            "selectionInputMapping": {},
            "confirmation": { "severity": "confirm", "strength": "click" }
        }));
    ux["pages"][0]["root"]["children"][2]["children"]
        .as_array_mut()
        .expect("form children")
        .push(json!({
            "nodeId": "approve-secondary-footer-node",
            "semanticId": "approve-secondary-footer-action",
            "component": { "kind": "action", "placementId": "approve-secondary-footer" },
            "children": []
        }));
    value
}

fn widget_with_toolbar_action() -> Value {
    let mut value = widget_with_secondary_footer_action();
    let ux = &mut value["payload"]["ux"];
    ux["actionBindings"][1]["inputMapping"] = json!({
        "orderId": "\"ORD-100\"",
        "reason": "\"Approved from toolbar\""
    });
    ux["actionBindings"][1]["inputs"] = json!({});
    ux["actionPlacements"][1]["location"] = json!("toolbar");
    ux["actionPlacements"][1]["confirmation"] = json!({
        "severity": "safe",
        "strength": "none"
    });
    let action_node = ux["pages"][0]["root"]["children"][2]["children"]
        .as_array_mut()
        .expect("form children")
        .pop()
        .expect("secondary action node");
    ux["pages"][0]["root"]["children"]
        .as_array_mut()
        .expect("section children")
        .push(action_node);
    value
}

fn widget_with_zero_input_toolbar_action() -> Value {
    let mut value = widget_with_toolbar_action();
    value["dependencies"]
        .as_array_mut()
        .expect("dependencies array")
        .push(json!({
            "artifactId": "commerce.order.noop.action",
            "kind": "action",
            "version": "0.1.0",
            "required": true
        }));
    value["payload"]["ux"]["actionBindings"][1]["action"] = json!({
        "id": "commerce.order.noop.action", "version": "0.1.0"
    });
    value["payload"]["ux"]["actionBindings"][1]["inputMapping"] = json!({});
    value["payload"]["ux"]["actionBindings"][1]["inputs"] = json!({});
    value
}

fn zero_input_widget_findings(value: Value) -> Vec<Diagnostic> {
    let mut support = support_artifacts();
    support.push(zero_input_action_artifact());
    findings_with_support(value, support, &[])
}

#[test]
fn a_toolbar_action_can_be_a_child_of_a_section_without_form_context() {
    assert_clean(&widget_findings(widget_with_toolbar_action()));
}

#[test]
fn zero_input_action_bindings_still_validate_each_declared_source() {
    assert_clean(&zero_input_widget_findings(
        widget_with_zero_input_toolbar_action(),
    ));
    for (inputs, pointer, message) in [
        (
            json!({ "recordBindingId": "missing-record" }),
            "/actionBindings/1/inputs/recordBindingId",
            "record input",
        ),
        (
            json!({ "recordBindingId": "orders" }),
            "/actionBindings/1/inputs/recordBindingId",
            "record input",
        ),
        (
            json!({ "selectionBindingId": "orders" }),
            "/actionBindings/1/inputs/selectionBindingId",
            "selection",
        ),
        (
            json!({ "draftFormId": "approve-form" }),
            "/actionBindings/1/inputs/draftFormId",
            "every invocation",
        ),
        (
            json!({ "contextPageId": "customer" }),
            "/actionBindings/1/inputs/contextPageId",
            "context is unavailable",
        ),
    ] {
        let mut value = widget_with_zero_input_toolbar_action();
        value["payload"]["ux"]["actionBindings"][1]["inputs"] = inputs;
        assert_issue(&zero_input_widget_findings(value), pointer, message);
    }
}

#[test]
fn a_toolbar_binding_cannot_declare_a_selection_input() {
    let mut value = widget_with_toolbar_action();
    value["payload"]["ux"]["actionBindings"][1]["inputs"]["selectionBindingId"] = json!("orders");
    assert_issue(&widget_findings(value), "/actionBindings/1", "selection");
}

#[test]
fn a_selection_form_opener_can_use_selection_only_before_opening_the_form() {
    let mut value = widget();
    value["payload"]["ux"]["actionPlacements"][0]["location"] = json!("selection");
    let condition = json!({
        "expression": "len(input.selection) > 0",
        "inputs": { "selectionBindingId": "orders" }
    });
    value["payload"]["ux"]["actionPlacements"][0]["enabledWhen"] = condition.clone();
    assert_clean(&widget_findings(value.clone()));
    let mut submit = value.clone();
    submit["payload"]["ux"]["actionBindings"][0]["inputs"]["selectionBindingId"] = json!("orders");
    assert_issue(&widget_findings(submit), "/actionBindings/0", "selection");
    value["payload"]["ux"]["fields"][4]["visibleWhen"] = condition;
    assert_issue(
        &widget_findings(value),
        "/fields/4/visibleWhen/inputs/selectionBindingId",
        "selection",
    );
}

#[test]
fn composed_form_selection_conditions_preserve_ownership_without_a_matching_host_form() {
    let mut baseline = baseline_widget("baseline-form-owned");
    baseline["dependencies"]
        .as_array_mut()
        .expect("dependencies array")
        .push(widget()["dependencies"][2].clone());
    let ux = &mut baseline["payload"]["ux"];
    ux["fields"] = json!([]);
    ux["dataBindings"] = json!([]);
    ux["actionBindings"] = json!([{
        "bindingId": "baseline-submit",
        "action": { "id": "commerce.order.approve.action", "version": "0.1.0" },
        "inputMapping": { "orderId": "\"ORD-100\"", "reason": "\"Approved\"" },
        "inputs": {}, "requiredCapability": "commerce.order.approve"
    }]);
    ux["forms"] = json!([{
        "formId": "baseline-form", "mode": "action_input", "inputNames": [],
        "submitBindingId": "baseline-submit",
        "submit": { "label": { "en": "Approve" }, "confirmation": { "severity": "safe", "strength": "none" } }
    }]);
    ux["pages"][0]["root"]["component"] = json!({ "kind": "form", "formId": "baseline-form" });
    assert_clean(&findings(baseline.clone()));
    let mut value = compose_baseline(widget());
    value["payload"]["ux"]["overrides"] = json!([{
        "targetSemanticId": "baseline-form-owned",
        "value": {
            "kind": "visibility",
            "condition": {
                "expression": "len(input.selection) > 0",
                "inputs": { "selectionBindingId": "orders" }
            }
        }
    }]);
    assert_issue(
        &findings_with_base(value, &[baseline]),
        "/overrides/0/value/condition/inputs/selectionBindingId",
        "selection",
    );
}

#[test]
fn draft_backed_bindings_cannot_be_invoked_directly_outside_their_form() {
    assert_clean(&widget_findings(widget()));
    assert_clean(&widget_findings(widget_with_secondary_footer_action()));
    for location in ["toolbar", "row", "selection", "detail"] {
        let mut value = widget_with_secondary_footer_action();
        let ux = &mut value["payload"]["ux"];
        ux["actionPlacements"][1]["location"] = json!(location);
        if matches!(location, "row" | "selection") {
            ux["actionPlacements"][1]["selectionBindingId"] = json!("orders");
        }
        let action_node = ux["pages"][0]["root"]["children"][2]["children"]
            .as_array_mut()
            .expect("form children")
            .pop()
            .expect("secondary action node");
        let children = match location {
            "toolbar" => &mut ux["pages"][0]["root"]["children"],
            "row" | "selection" => &mut ux["pages"][0]["root"]["children"][0]["children"],
            "detail" => &mut ux["pages"][1]["root"]["children"],
            _ => unreachable!("bounded locations"),
        };
        children
            .as_array_mut()
            .expect("surface children")
            .push(action_node);
        assert_issue(
            &widget_findings(value),
            "/actionBindings/1/inputs/draftFormId",
            "every invocation",
        );
    }
}

#[test]
fn a_shared_binding_cannot_borrow_a_matching_footers_draft_for_a_toolbar() {
    let mut value = widget_with_secondary_footer_action();
    let ux = &mut value["payload"]["ux"];
    let mut toolbar = ux["actionPlacements"][1].clone();
    toolbar["placementId"] = json!("secondary-toolbar");
    toolbar["location"] = json!("toolbar");
    ux["actionPlacements"]
        .as_array_mut()
        .expect("placements array")
        .push(toolbar);
    ux["pages"][0]["root"]["children"]
        .as_array_mut()
        .expect("page children")
        .push(json!({
            "nodeId": "secondary-toolbar-node",
            "component": { "kind": "action", "placementId": "secondary-toolbar" },
            "children": []
        }));
    assert_issue(
        &widget_findings(value),
        "/actionBindings/1/inputs/draftFormId",
        "every invocation",
    );
}

#[test]
fn draft_conditions_require_the_matching_form_at_each_render_site() {
    let condition = json!({
        "expression": "input.draft.reason != \"\"",
        "inputs": { "draftFormId": "approve-form" }
    });
    let mut field = widget();
    field["payload"]["ux"]["fields"][4]["visibleWhen"] = condition.clone();
    assert_clean(&widget_findings(field));

    let mut footer = widget_with_secondary_footer_action();
    footer["payload"]["ux"]["actionPlacements"][1]["enabledWhen"] = condition.clone();
    assert_clean(&widget_findings(footer));

    for (collection, index, member) in [
        ("fields", 0, "visibleWhen"),
        ("navigationItems", 0, "visibleWhen"),
        ("actionPlacements", 0, "enabledWhen"),
    ] {
        let mut value = widget();
        value["payload"]["ux"][collection][index][member] = condition.clone();
        assert_issue(
            &widget_findings(value),
            &format!("/{collection}/{index}/{member}/inputs/draftFormId"),
            "matching form",
        );
    }

    let mut composed = compose_baseline(widget());
    composed["payload"]["ux"]["overrides"] = json!([{
        "targetSemanticId": "baseline-revision-field",
        "value": { "kind": "visibility", "condition": condition }
    }]);
    assert_issue(
        &findings_with_base(composed, &[baseline_widget("baseline-root")]),
        "/overrides/0/value/condition/inputs/draftFormId",
        "matching form",
    );
}

#[test]
fn a_footer_placement_reused_in_another_form_cannot_borrow_the_first_forms_draft() {
    let mut value = widget_with_secondary_footer_action();
    let ux = &mut value["payload"]["ux"];
    let mut form = ux["forms"][0].clone();
    form["formId"] = json!("other-form");
    form["inputNames"] = json!([]);
    form["submitBindingId"] = json!("other-submit");
    let mut submit = ux["actionBindings"][0].clone();
    submit["bindingId"] = json!("other-submit");
    submit["inputs"] = json!({});
    submit["inputMapping"] = json!({
        "orderId": "\"ORD-100\"",
        "reason": "\"Approved\""
    });
    ux["forms"].as_array_mut().expect("forms array").push(form);
    ux["actionBindings"]
        .as_array_mut()
        .expect("action bindings array")
        .push(submit);
    ux["pages"][0]["root"]["children"]
        .as_array_mut()
        .expect("page children")
        .push(json!({
            "nodeId": "other-form-node",
            "component": { "kind": "form", "formId": "other-form" },
            "children": [{
                "nodeId": "other-footer-node",
                "component": { "kind": "action", "placementId": "approve-secondary-footer" },
                "children": []
            }]
        }));
    assert_issue(
        &widget_findings(value),
        "/actionBindings/1/inputs/draftFormId",
        "every invocation",
    );
}

#[test]
fn field_conditions_cannot_borrow_another_pages_context() {
    let mut value = widget();
    value["payload"]["ux"]["fields"][0]["visibleWhen"] = json!({
        "expression": "input.context.customerNumber != \"\"",
        "inputs": { "contextPageId": "customer" }
    });
    assert_issue(
        &widget_findings(value),
        "/fields/0/visibleWhen/inputs/contextPageId",
        "page",
    );
}

#[test]
fn field_selection_conditions_require_a_rendered_list_on_every_use_page() {
    let mut value = widget();
    value["payload"]["ux"]["fields"][0]["visibleWhen"] = json!({
        "expression": "len(input.selection) > 0",
        "inputs": { "selectionBindingId": "orders" }
    });
    assert_clean(&widget_findings(value.clone()));
    value["payload"]["ux"]["pages"][1]["root"]["children"]
        .as_array_mut()
        .expect("customer page children")
        .push(json!({
            "nodeId": "borrowed-order-number-node",
            "semanticId": "borrowed-order-number",
            "component": { "kind": "field", "fieldId": "order-number" },
            "children": []
        }));
    assert_issue(
        &widget_findings(value),
        "/fields/0/visibleWhen/inputs/selectionBindingId",
        "selection",
    );
}

fn render_orders_list_on_customer_page(value: &mut Value) {
    value["payload"]["ux"]["pages"][1]["root"]["children"]
        .as_array_mut()
        .expect("customer page children")
        .push(json!({
            "nodeId": "customer-orders-list-node",
            "component": {
                "kind": "list", "dataBindingId": "orders", "fields": [], "selection": "single"
            },
            "children": []
        }));
}

#[test]
fn selected_record_conditions_require_a_rendered_list_on_every_use_page() {
    let mut value = widget();
    assert_clean(&widget_findings(value.clone()));
    value["payload"]["ux"]["pages"][1]["root"]["children"]
        .as_array_mut()
        .expect("customer page children")
        .push(json!({
            "nodeId": "customer-order-number-node",
            "component": { "kind": "field", "fieldId": "order-number" },
            "children": []
        }));
    assert_issue(
        &widget_findings(value.clone()),
        "/fields/0/visibleWhen/inputs/recordBindingId",
        "record input",
    );
    render_orders_list_on_customer_page(&mut value);
    assert_clean(&widget_findings(value));
}

fn widget_with_unselected_orders() -> Value {
    let mut value = widget();
    let ux = &mut value["payload"]["ux"];
    ux["pages"][0]["root"]["children"][0]["component"]["selection"] = json!("none");
    ux["dataBindings"][1]["query"] = json!({
        "kind": "object_list", "filters": [], "sort": [],
        "page": { "size": 10, "maxSize": 10 }
    });
    value
}

fn widget_with_unavailable_page_order_value(selection: &str, section: bool) -> Value {
    let mut value = widget();
    let ux = &mut value["payload"]["ux"];
    let order_list = &mut ux["pages"][0]["root"]["children"][0]["component"];
    let _ = order_list["fields"]
        .as_array_mut()
        .expect("list fields")
        .remove(0);
    order_list["selection"] = json!(selection);
    let _ = ux["fields"][0]
        .as_object_mut()
        .expect("order number field")
        .remove("visibleWhen");
    let included_property = json!({ "bindingId": "orders", "propertyId": "order_number" });
    if section {
        ux["pages"][0]["root"]["component"]["section"]["fields"] = json!([included_property]);
    } else {
        ux["pages"][0]["root"]["children"]
            .as_array_mut()
            .expect("page children")
            .push(json!({
                "nodeId": "page-order-number-node",
                "component": { "kind": "field", "fieldId": "order-number" },
                "children": []
            }));
    }
    value
}

#[test]
fn property_values_require_a_current_record_at_every_rendered_use() {
    for section in [false, true] {
        for selection in ["none", "multiple"] {
            assert_issue(
                &widget_findings(widget_with_unavailable_page_order_value(selection, section)),
                "/fields/0",
                "field value requires an available",
            );
        }
    }

    assert_clean(&widget_findings(widget_with_unavailable_page_order_value(
        "single", true,
    )));
    assert_clean(&widget_findings(widget_with_unavailable_page_order_value(
        "single", false,
    )));
    assert_clean(&widget_findings(widget_with_unselected_orders()));
    assert_clean(&widget_findings(widget()));
}

#[test]
fn unselected_list_records_are_available_only_at_every_matching_row_surface() {
    assert_clean(&widget_findings(widget_with_unselected_orders()));
    for keep_row_field in [false, true] {
        let mut value = widget_with_unselected_orders();
        let ux = &mut value["payload"]["ux"];
        if !keep_row_field {
            let _ = ux["pages"][0]["root"]["children"][0]["component"]["fields"]
                .as_array_mut()
                .expect("list fields")
                .remove(0);
        }
        ux["pages"][0]["root"]["children"]
            .as_array_mut()
            .expect("page children")
            .push(json!({
                "nodeId": "page-order-number-node",
                "component": { "kind": "field", "fieldId": "order-number" },
                "children": []
            }));
        let found = widget_findings(value);
        assert_issue(
            &found,
            "/fields/0/visibleWhen/inputs/recordBindingId",
            "record input",
        );
        assert_issue(&found, "/fields/0", "field value requires an available");
        assert_eq!(
            found.len(),
            2,
            "the field and its condition both read the unavailable row"
        );
    }
}

#[test]
fn form_field_conditions_keep_the_row_record_captured_by_their_form() {
    let mut value = widget_with_unselected_orders();
    value["payload"]["ux"]["fields"][4]["visibleWhen"] = json!({
        "expression": "input.record.status == \"draft\"",
        "inputs": { "recordBindingId": "orders" }
    });
    assert_clean(&widget_findings(value));
}

#[test]
fn composed_row_conditions_require_the_exact_imported_binding_surface() {
    let mut baseline = baseline_widget("baseline-list-semantic");
    baseline["dependencies"] = widget()["dependencies"].clone();
    let ux = &mut baseline["payload"]["ux"];
    ux["dataBindings"][0] = widget()["payload"]["ux"]["dataBindings"][0].clone();
    ux["fields"][0]["target"]["property"]["propertyId"] = json!("order_number");
    ux["pages"][0]["root"]["component"] = json!({
        "kind": "list", "dataBindingId": "orders", "selection": "none",
        "fields": [{ "bindingId": "orders", "propertyId": "order_number" }]
    });
    assert_clean(&findings(baseline.clone()));
    let mut host = baseline.clone();
    host["artifactId"] = json!(WIDGET_ID);
    host["payload"]["widgetId"] = json!(WIDGET_ID);
    host["dependencies"]
        .as_array_mut()
        .expect("dependencies array")
        .push(json!({
            "artifactId": "commerce.widget.baseline", "kind": "widget",
            "version": "0.1.0", "required": true
        }));
    let ux = &mut host["payload"]["ux"];
    ux["fields"] = json!([]);
    ux["pages"][0]["pageId"] = json!("host-page");
    let composition = json!({
        "nodeId": "host-composition-node",
        "component": {
            "kind": "composition",
            "widget": { "id": "commerce.widget.baseline", "version": "0.1.0" },
            "pageId": "baseline-page"
        },
        "children": []
    });
    ux["pages"][0]["root"] = composition.clone();
    ux["overrides"] = json!([{
        "targetSemanticId": "baseline-revision-field",
        "value": {
            "kind": "visibility",
            "condition": {
                "expression": "input.record.status == \"draft\"",
                "inputs": { "recordBindingId": "orders" }
            }
        }
    }]);
    assert_clean(&findings_with_base(
        host.clone(),
        std::slice::from_ref(&baseline),
    ));
    let ux = &mut host["payload"]["ux"];
    ux["dataBindings"][0]["query"]["page"]["maxSize"] = json!(20);
    ux["pages"][0]["root"] = json!({
        "nodeId": "host-root",
        "component": {
            "kind": "section",
            "section": {
                "sectionId": "host-section", "kind": "section", "label": { "en": "Host" },
                "fields": [], "order": 1
            }
        },
        "children": [composition, {
            "nodeId": "host-alias-list",
            "component": { "kind": "list", "dataBindingId": "orders", "fields": [], "selection": "none" },
            "children": []
        }]
    });
    let found = findings_with_base(host, &[baseline]);
    assert_issue(
        &found,
        "/overrides/0/value/condition/inputs/recordBindingId",
        "record input",
    );
    assert_eq!(
        found.len(),
        1,
        "the imported row cannot borrow an alias binding"
    );
}

#[test]
fn composed_section_property_values_require_a_current_record() {
    let mut baseline = baseline_widget("baseline-order-section");
    baseline["dependencies"] = json!([{
        "artifactId": "commerce.Order.entity",
        "kind": "entity",
        "version": "0.1.0",
        "required": true
    }]);
    let ux = &mut baseline["payload"]["ux"];
    ux["dataBindings"][0] = widget()["payload"]["ux"]["dataBindings"][0].clone();
    ux["fields"][0]["target"]["property"] = json!({
        "bindingId": "orders", "propertyId": "order_number"
    });
    ux["pages"][0]["root"]["component"] = json!({
        "kind": "section",
        "section": {
            "sectionId": "baseline-order-section",
            "kind": "section",
            "label": { "en": "Orders" },
            "fields": [{ "bindingId": "orders", "propertyId": "order_number" }],
            "order": 1
        }
    });
    ux["pages"][0]["root"]["children"] = json!([]);
    assert_issue(
        &findings(baseline.clone()),
        "/fields/0",
        "field value requires an available",
    );

    assert_issue(
        &findings_with_base(compose_baseline(widget()), &[baseline]),
        "/component",
        "fails typed UX validation",
    );
}

#[test]
fn form_prefill_requires_the_source_list_on_every_form_page() {
    let mut value = widget_with_no_input_form();
    assert_clean(&widget_findings(value.clone()));
    value["payload"]["ux"]["pages"][1]["root"]["children"]
        .as_array_mut()
        .expect("customer page children")
        .push(json!({
            "nodeId": "customer-approve-form-node",
            "component": { "kind": "form", "formId": "approve-form" },
            "children": []
        }));
    assert_issue(
        &widget_findings(value.clone()),
        "/forms/0/dataBindingId",
        "prefill",
    );
    render_orders_list_on_customer_page(&mut value);
    assert_clean(&widget_findings(value));
}

#[test]
fn record_filters_require_the_selected_source_on_every_consuming_page() {
    let mut value = widget();
    let ux = &mut value["payload"]["ux"];
    let mut binding = ux["dataBindings"][0].clone();
    binding["bindingId"] = json!("matching-orders");
    binding["query"]["filters"][0]["property"]["bindingId"] = json!("matching-orders");
    binding["query"]["filters"][0]["value"] = json!({
        "kind": "record", "property": { "bindingId": "orders", "propertyId": "status" }
    });
    binding["query"]["sort"] = json!([]);
    ux["dataBindings"]
        .as_array_mut()
        .expect("data bindings array")
        .push(binding);
    let list = json!({
        "nodeId": "matching-orders-node",
        "component": {
            "kind": "list", "dataBindingId": "matching-orders", "fields": [], "selection": "none"
        },
        "children": []
    });
    ux["pages"][0]["root"]["children"]
        .as_array_mut()
        .expect("orders page children")
        .push(list.clone());
    assert_clean(&widget_findings(value.clone()));
    let mut second_list = list;
    second_list["nodeId"] = json!("customer-matching-orders-node");
    value["payload"]["ux"]["pages"][1]["root"]["children"]
        .as_array_mut()
        .expect("customer page children")
        .push(second_list);
    assert_issue(
        &widget_findings(value.clone()),
        "/dataBindings/3/query/filters/0/value",
        "record value requires",
    );
    render_orders_list_on_customer_page(&mut value);
    assert_clean(&widget_findings(value));
}

#[test]
fn traversals_require_the_selected_source_on_every_consuming_page() {
    let mut value = widget();
    assert_clean(&widget_findings(value.clone()));
    let mut list = value["payload"]["ux"]["pages"][0]["root"]["children"][1].clone();
    list["nodeId"] = json!("customer-linked-customers-node");
    list["semanticId"] = json!("customer-linked-customers");
    value["payload"]["ux"]["pages"][1]["root"]["children"]
        .as_array_mut()
        .expect("customer page children")
        .push(list);
    assert_issue(
        &widget_findings(value.clone()),
        "/dataBindings/1/query/sourceBindingId",
        "single-selection list",
    );
    render_orders_list_on_customer_page(&mut value);
    assert_clean(&widget_findings(value));
}

#[test]
fn detail_identity_records_require_the_selected_source_on_the_detail_page() {
    let mut value = widget();
    value["payload"]["ux"]["dataBindings"][2]["query"]["key"][0]["value"] = json!({
        "kind": "record",
        "property": { "bindingId": "orders", "propertyId": "customer_number" }
    });
    assert_issue(
        &widget_findings(value.clone()),
        "/dataBindings/2/query/key/0/value",
        "record value requires",
    );
    render_orders_list_on_customer_page(&mut value);
    assert_clean(&widget_findings(value));
}

#[test]
fn navigation_conditions_cannot_borrow_another_pages_selected_record() {
    let mut value = widget();
    value["payload"]["ux"]["navigationItems"][0]["visibleWhen"] = json!({
        "expression": "input.record.status == \"draft\"",
        "inputs": { "recordBindingId": "orders" }
    });
    assert_clean(&widget_findings(value.clone()));
    value["payload"]["ux"]["pages"][1]["root"]["children"]
        .as_array_mut()
        .expect("customer page children")
        .push(json!({
            "nodeId": "customer-navigation-borrowed-node",
            "component": { "kind": "navigation", "itemId": "customer-nav" },
            "children": []
        }));
    assert_issue(
        &widget_findings(value),
        "/navigationItems/0/visibleWhen/inputs/recordBindingId",
        "record input",
    );
}

fn runtime_key_navigation_widget(source: Value) -> Value {
    let mut value = widget();
    let ux = &mut value["payload"]["ux"];
    let mut parameters = serde_json::Map::new();
    parameters.insert("orderKey".to_owned(), source);
    ux["pages"].as_array_mut().expect("pages").push(json!({
        "pageId": "order-runtime-key",
        "name": "Order runtime key",
        "contextParams": [{
            "paramId": "orderKey",
            "type": "text",
            "required": true,
            "runtimeKeyOf": { "id": "commerce.Order.entity", "version": "0.1.0" }
        }],
        "root": {
            "nodeId": "order-runtime-key-root",
            "component": { "kind": "section", "section": {
                "sectionId": "order-runtime-key-section",
                "kind": "section",
                "label": { "en": "Order runtime key" },
                "fields": [],
                "order": 3
            }},
            "children": []
        }
    }));
    ux["app"]["pageRefs"]
        .as_array_mut()
        .expect("page refs")
        .push(json!({ "pageId": "order-runtime-key" }));
    ux["navigationItems"]
        .as_array_mut()
        .expect("navigation items")
        .push(json!({
            "itemId": "order-runtime-key-navigation",
            "label": { "en": "Open order runtime key" },
            "requiredCapability": "commerce.order.read",
            "destination": {
                "kind": "page",
                "pageId": "order-runtime-key",
                "parameters": parameters
            }
        }));
    ux["pages"][0]["root"]["children"][0]["children"]
        .as_array_mut()
        .expect("order list children")
        .push(json!({
            "nodeId": "order-runtime-key-navigation-node",
            "component": { "kind": "navigation", "itemId": "order-runtime-key-navigation" },
            "children": []
        }));
    value
}

#[test]
fn navigation_runtime_keys_require_a_matching_marked_entity_and_record_context() {
    let valid = runtime_key_navigation_widget(json!({
        "kind": "record",
        "property": { "bindingId": "orders", "propertyId": "$key" }
    }));
    assert_clean(&widget_findings(valid.clone()));

    let mut routed = valid;
    routed["payload"]["ux"]["app"]["routes"]
        .as_array_mut()
        .expect("routes")
        .push(json!({
            "routeId": "order-runtime-key-route",
            "path": "/orders/by-runtime-key/{orderKey}",
            "pageId": "order-runtime-key",
            "parameters": [{
                "routeParameter": "orderKey",
                "contextParameterId": "orderKey"
            }]
        }));
    assert_clean(&widget_findings(routed));

    let wrong_binding = runtime_key_navigation_widget(json!({
        "kind": "record",
        "property": { "bindingId": "customer-detail", "propertyId": "$key" }
    }));
    assert_issue(
        &widget_findings(wrong_binding),
        "/navigationItems/1/destination/parameters/orderKey",
        "exact marked entity binding",
    );

    let unknown_binding = runtime_key_navigation_widget(json!({
        "kind": "record",
        "property": { "bindingId": "unknown-orders", "propertyId": "$key" }
    }));
    assert_issue(
        &widget_findings(unknown_binding),
        "/navigationItems/1/destination/parameters/orderKey",
        "exact marked entity binding",
    );

    let mut unavailable_record = runtime_key_navigation_widget(json!({
        "kind": "record",
        "property": { "bindingId": "unrendered-order", "propertyId": "$key" }
    }));
    unavailable_record["payload"]["ux"]["dataBindings"]
        .as_array_mut()
        .expect("data bindings")
        .push(json!({
            "bindingId": "unrendered-order",
            "object": { "id": "commerce.Order.entity", "version": "0.1.0" },
            "query": {
                "kind": "object_list",
                "filters": [],
                "sort": [],
                "page": { "size": 10, "maxSize": 10 }
            }
        }));
    assert_issue(
        &widget_findings(unavailable_record),
        "/navigationItems/1/destination/parameters/orderKey",
        "not available in this list row",
    );

    let business_property = runtime_key_navigation_widget(json!({
        "kind": "record",
        "property": { "bindingId": "orders", "propertyId": "order_id" }
    }));
    assert_issue(
        &widget_findings(business_property),
        "/navigationItems/1/destination/parameters/orderKey",
        "matching runtime-key binding or marked context parameter",
    );

    let literal =
        runtime_key_navigation_widget(json!({ "kind": "literal", "value": "business-1" }));
    assert_issue(
        &widget_findings(literal),
        "/navigationItems/1/destination/parameters/orderKey",
        "matching runtime-key binding or marked context parameter",
    );

    let ordinary_context = runtime_key_navigation_widget(json!({
        "kind": "context_parameter",
        "pageId": "customer",
        "parameterId": "customerNumber"
    }));
    assert_issue(
        &widget_findings(ordinary_context),
        "/navigationItems/1/destination/parameters/orderKey",
        "same required text runtimeKeyOf entity",
    );
}

#[test]
fn navigation_forwards_runtime_key_context_only_with_the_same_entity_marker() {
    let mut value = runtime_key_navigation_widget(json!({
        "kind": "record",
        "property": { "bindingId": "orders", "propertyId": "$key" }
    }));
    let ux = &mut value["payload"]["ux"];
    ux["pages"].as_array_mut().expect("pages").push(json!({
        "pageId": "order-runtime-key-source",
        "name": "Order runtime key source",
        "contextParams": [{
            "paramId": "sourceOrderKey",
            "type": "text",
            "required": true,
            "runtimeKeyOf": { "id": "commerce.Order.entity", "version": "0.1.0" }
        }],
        "root": {
            "nodeId": "order-runtime-key-source-root",
            "component": { "kind": "section", "section": {
                "sectionId": "order-runtime-key-source-section",
                "kind": "section",
                "label": { "en": "Order runtime key source" },
                "fields": [],
                "order": 4
            }},
            "children": [{
                "nodeId": "order-runtime-key-forward-node",
                "component": { "kind": "navigation", "itemId": "order-runtime-key-forward" },
                "children": []
            }]
        }
    }));
    ux["app"]["pageRefs"]
        .as_array_mut()
        .expect("page refs")
        .push(json!({ "pageId": "order-runtime-key-source" }));
    ux["navigationItems"]
        .as_array_mut()
        .expect("navigation items")
        .push(json!({
            "itemId": "order-runtime-key-forward",
            "label": { "en": "Forward order runtime key" },
            "requiredCapability": "commerce.order.read",
            "destination": {
                "kind": "page",
                "pageId": "order-runtime-key",
                "parameters": { "orderKey": {
                    "kind": "context_parameter",
                    "pageId": "order-runtime-key-source",
                    "parameterId": "sourceOrderKey"
                }}
            }
        }));
    assert_clean(&widget_findings(value.clone()));

    value["payload"]["ux"]["pages"][2]["contextParams"][0]["runtimeKeyOf"]["id"] =
        json!("crm.Customer.entity");
    assert_issue(
        &widget_findings(value.clone()),
        "/navigationItems/2/destination/parameters/orderKey",
        "same required text runtimeKeyOf entity",
    );

    let _ = value["payload"]["ux"]["pages"][2]["contextParams"][0]
        .as_object_mut()
        .expect("target runtime key parameter")
        .remove("runtimeKeyOf");
    assert_issue(
        &widget_findings(value),
        "/navigationItems/2/destination/parameters/orderKey",
        "runtime-key context parameters cannot be copied to unmarked destination parameters",
    );
}

#[test]
fn runtime_keys_cannot_be_used_as_query_or_detail_identity_values() {
    let mut value = widget();
    value["payload"]["ux"]["dataBindings"][0]["query"]["filters"] = json!([{
        "property": { "bindingId": "orders", "propertyId": "status" },
        "operator": "eq",
        "value": { "kind": "record", "property": { "bindingId": "orders", "propertyId": "$key" } }
    }]);
    assert_issue(
        &widget_findings(value.clone()),
        "/dataBindings/0/query/filters/0/value",
        "runtime keys cannot supply query or filter values",
    );

    value["payload"]["ux"]["dataBindings"][2]["query"]["key"][0]["value"] = json!({
        "kind": "record",
        "property": { "bindingId": "orders", "propertyId": "$key" }
    });
    assert_issue(
        &widget_findings(value),
        "/dataBindings/2/query/key/0/value",
        "runtime keys cannot supply query or filter values",
    );
}

fn move_customer_navigation_to_page(value: &mut Value) {
    let ux = &mut value["payload"]["ux"];
    let navigation = ux["pages"][0]["root"]["children"][0]["children"]
        .as_array_mut()
        .expect("list children")
        .remove(0);
    ux["pages"][0]["root"]["children"]
        .as_array_mut()
        .expect("page children")
        .push(navigation);
    ux["navigationItems"][0]["visibleWhen"] = json!({
        "expression": "input.record.status == \"draft\"",
        "inputs": { "recordBindingId": "orders" }
    });
}

#[test]
fn page_navigation_can_use_its_declared_selected_record_on_every_use_page() {
    let mut value = widget();
    move_customer_navigation_to_page(&mut value);
    assert_clean(&widget_findings(value.clone()));
    value["payload"]["ux"]["pages"][0]["root"]["children"][1]["component"]["selection"] =
        json!("single");
    assert_clean(&widget_findings(value.clone()));
    let mut navigation = value["payload"]["ux"]["pages"][0]["root"]["children"][3].clone();
    navigation["nodeId"] = json!("customer-selected-navigation-node");
    navigation["semanticId"] = json!("customer-selected-navigation");
    value["payload"]["ux"]["pages"][1]["root"]["children"]
        .as_array_mut()
        .expect("customer page children")
        .push(navigation);
    assert_issue(
        &widget_findings(value.clone()),
        "/navigationItems/0/visibleWhen/inputs/recordBindingId",
        "record input",
    );
    render_orders_list_on_customer_page(&mut value);
    assert_clean(&widget_findings(value));
}

#[test]
fn page_and_app_navigation_cannot_borrow_row_only_records() {
    let mut row_only = widget_with_unselected_orders();
    move_customer_navigation_to_page(&mut row_only);
    assert_issue(
        &widget_findings(row_only),
        "/navigationItems/0/visibleWhen/inputs/recordBindingId",
        "record input",
    );
    let mut app = widget();
    app["payload"]["ux"]["navigationItems"][0]["visibleWhen"] = json!({
        "expression": "input.record.status == \"draft\"",
        "inputs": { "recordBindingId": "orders" }
    });
    app["payload"]["ux"]["app"]["navigationItemIds"] = json!(["customer-nav"]);
    assert_issue(
        &widget_findings(app),
        "/navigationItems/0/visibleWhen/inputs/recordBindingId",
        "record input",
    );
}

#[test]
fn action_bindings_cannot_borrow_another_pages_context() {
    let mut value = widget();
    value["payload"]["ux"]["actionBindings"][0]["inputs"]["contextPageId"] = json!("customer");
    value["payload"]["ux"]["actionBindings"][0]["inputMapping"]["reason"] =
        json!("input.context.customerNumber");
    assert_issue(&widget_findings(value), "/actionBindings/0", "page");
}

fn widget_with_no_input_form() -> Value {
    let mut value = widget();
    let ux = &mut value["payload"]["ux"];
    ux["forms"][0]["inputNames"] = json!([]);
    ux["actionBindings"][0]["inputs"] = json!({
        "recordBindingId": "orders"
    });
    ux["actionBindings"][0]["inputMapping"] = json!({
        "orderId": "\"ORD-100\"",
        "reason": "\"Approved\""
    });
    ux["fields"]
        .as_array_mut()
        .expect("fields array")
        .retain(|field| field["target"]["kind"] != "form_input");
    ux["pages"][0]["root"]["children"][2]["children"] = json!([]);
    value
}

#[test]
fn empty_forms_allow_an_optional_matching_draft_but_nonempty_forms_require_it() {
    let mut value = widget_with_no_input_form();
    assert_clean(&widget_findings(value.clone()));
    value["payload"]["ux"]["actionBindings"][0]["inputs"]["draftFormId"] = json!("approve-form");
    assert_clean(&widget_findings(value.clone()));
    let mut empty_draft_read = value.clone();
    empty_draft_read["payload"]["ux"]["actionBindings"][0]["inputMapping"]["orderId"] =
        json!("input.draft.orderId");
    assert_issue(
        &widget_findings(empty_draft_read),
        "/actionBindings/0/inputMapping/orderId",
        "incompatible",
    );
    value["payload"]["ux"]["actionBindings"][0]["inputMapping"]["orderId"] =
        json!("input.record.order_id");
    assert_clean(&widget_findings(value.clone()));
    value["payload"]["ux"]["actionBindings"][0]["inputs"]["draftFormId"] = json!("other-form");
    assert_issue(
        &widget_findings(value),
        "/forms/0/submitBindingId",
        "declare this form",
    );

    let mut nonempty = widget();
    let _ = nonempty["payload"]["ux"]["actionBindings"][0]["inputs"]
        .as_object_mut()
        .expect("expression inputs")
        .remove("draftFormId");
    assert_issue(
        &widget_findings(nonempty),
        "/forms/0/submitBindingId",
        "declare this form",
    );
}

#[test]
fn a_no_input_form_still_needs_a_render_node_for_its_submit() {
    assert_clean(&widget_findings(widget_with_no_input_form()));
    let mut value = widget_with_no_input_form();
    let _ = value["payload"]["ux"]["pages"][0]["root"]["children"]
        .as_array_mut()
        .expect("page children")
        .remove(2);
    assert_issue(
        &widget_findings(value),
        "/actionPlacements/0/formId",
        "render",
    );

    let mut wrong_page = widget_with_no_input_form();
    let form_node = wrong_page["payload"]["ux"]["pages"][0]["root"]["children"]
        .as_array_mut()
        .expect("page children")
        .remove(2);
    wrong_page["payload"]["ux"]["pages"][1]["root"]["children"]
        .as_array_mut()
        .expect("customer page children")
        .push(form_node);
    assert_issue(
        &widget_findings(wrong_page),
        "/actionPlacements/0/formId",
        "render",
    );
}

#[test]
fn an_action_placement_used_only_by_a_composed_override_is_valid() {
    let mut baseline = baseline_widget("baseline-action-semantic");
    baseline["dependencies"]
        .as_array_mut()
        .expect("dependencies array")
        .push(json!({
            "artifactId": "commerce.order.approve.action",
            "kind": "action",
            "version": "0.1.0",
            "required": true
        }));
    let ux = &mut baseline["payload"]["ux"];
    ux["fields"] = json!([]);
    ux["actionBindings"] = json!([{
        "bindingId": "baseline-approve",
        "action": { "id": "commerce.order.approve.action", "version": "0.1.0" },
        "inputMapping": { "orderId": "\"ORD-100\"", "reason": "\"Approved\"" },
        "inputs": {},
        "requiredCapability": "commerce.order.approve"
    }]);
    ux["actionPlacements"] = json!([{
        "placementId": "baseline-action",
        "bindingId": "baseline-approve",
        "label": { "en": "Approve" },
        "location": "toolbar",
        "order": 1,
        "selectionInputMapping": {},
        "confirmation": { "severity": "safe", "strength": "none" }
    }]);
    ux["pages"][0]["root"]["component"] =
        json!({ "kind": "action", "placementId": "baseline-action" });
    assert_clean(&findings(baseline.clone()));

    let mut value = widget_with_toolbar_action();
    let _ = value["payload"]["ux"]["pages"][0]["root"]["children"]
        .as_array_mut()
        .expect("page children")
        .pop();
    let mut value = compose_baseline(value);
    value["payload"]["ux"]["overrides"] = json!([{
        "targetSemanticId": "baseline-action-semantic",
        "value": { "kind": "action_placement", "placementId": "approve-secondary-footer" }
    }]);
    assert_clean(&findings_with_base(value, &[baseline]));
}

#[test]
fn composition_cannot_supply_an_unmapped_required_baseline_page_parameter() {
    let mut baseline = baseline_widget("baseline-parameter-field");
    baseline["payload"]["ux"]["pages"][0]["contextParams"] = json!([{
        "paramId": "customerNumber",
        "type": "text",
        "required": true
    }]);
    assert_clean(&findings(baseline.clone()));
    assert_issue(
        &findings_with_base(compose_baseline(widget()), &[baseline]),
        "/component/pageId",
        "required context",
    );
}

#[test]
fn composed_field_visibility_uses_only_an_exact_imported_list_binding() {
    let mut baseline = baseline_widget("baseline-list-semantic");
    baseline["dependencies"] = json!([{
        "artifactId": "commerce.Order.entity",
        "kind": "entity",
        "version": "0.1.0",
        "required": true
    }]);
    baseline["payload"]["ux"]["dataBindings"][0] =
        widget()["payload"]["ux"]["dataBindings"][0].clone();
    baseline["payload"]["ux"]["fields"][0]["target"]["property"]["propertyId"] =
        json!("order_number");
    baseline["payload"]["ux"]["pages"][0]["root"]["component"] = json!({
        "kind": "list",
        "dataBindingId": "orders",
        "fields": [{ "bindingId": "orders", "propertyId": "order_number" }],
        "selection": "single"
    });
    assert_clean(&findings(baseline.clone()));

    let mut value = baseline.clone();
    value["artifactId"] = json!(WIDGET_ID);
    value["payload"]["widgetId"] = json!(WIDGET_ID);
    value["dependencies"]
        .as_array_mut()
        .expect("dependencies array")
        .push(json!({
            "artifactId": "commerce.widget.baseline",
            "kind": "widget",
            "version": "0.1.0",
            "required": true
        }));
    value["payload"]["ux"]["fields"] = json!([]);
    value["payload"]["ux"]["pages"][0]["pageId"] = json!("host-page");
    value["payload"]["ux"]["pages"][0]["root"] = json!({
        "nodeId": "host-composition",
        "semanticId": "host-composition",
        "component": {
            "kind": "composition",
            "widget": { "id": "commerce.widget.baseline", "version": "0.1.0" },
            "pageId": "baseline-page"
        },
        "children": []
    });
    value["payload"]["ux"]["overrides"] = json!([{
        "targetSemanticId": "baseline-revision-field",
        "value": {
            "kind": "visibility",
            "condition": {
                "expression": "len(input.selection) > 0",
                "inputs": { "selectionBindingId": "orders" }
            }
        }
    }]);
    assert_clean(&findings_with_base(
        value.clone(),
        std::slice::from_ref(&baseline),
    ));
    value["payload"]["ux"]["dataBindings"][0]["query"]["page"]["maxSize"] = json!(20);
    assert_issue(
        &findings_with_base(value, &[baseline]),
        "/overrides/0/value/condition/inputs/selectionBindingId",
        "selection",
    );
}

#[test]
fn secondary_form_footer_action_is_hosted_by_its_form_without_opening_it() {
    assert_clean(&widget_findings(widget_with_secondary_footer_action()));
}

#[test]
fn a_form_footer_cannot_repeat_its_hosted_forms_submit_binding() {
    let mut value = widget_with_secondary_footer_action();
    value["payload"]["ux"]["actionPlacements"][1]["bindingId"] = json!("approve");
    assert_issue(
        &widget_findings(value),
        "/pages/0/root/children/2/children/2/component/placementId",
        "own submit action as a secondary footer action",
    );
}

#[test]
fn a_form_footer_placement_cannot_open_a_form() {
    let mut value = widget_with_secondary_footer_action();
    value["payload"]["ux"]["actionPlacements"][1]["formId"] = json!("approve-form");
    assert_issue(
        &widget_findings(value),
        "/actionPlacements/1/formId",
        "must use the form's submit action binding",
    );
}

#[test]
fn composed_semantic_id_collisions_are_rejected() {
    let value = compose_baseline(widget());
    let base = [baseline_widget("orders-page")];
    let found = widget_findings_with_base(value, &base);
    assert!(
        found
            .iter()
            .any(|finding| finding.message.contains("semantic")),
        "expected imported semantic id collision; got {found:#?}"
    );
}

fn widget_findings_with_base(widget: Value, base: &[Value]) -> Vec<Diagnostic> {
    findings_with_base(widget, base)
        .into_iter()
        .filter(|finding| finding.artifact_id.as_deref() == Some(WIDGET_ID))
        .collect()
}

#[test]
fn runtime_detail_requires_the_exact_entity_marker_and_owning_page() {
    let mut value = widget();
    value["payload"]["ux"]["pages"][1]["contextParams"][0]["runtimeKeyOf"] =
        json!({ "id": "crm.Customer.entity", "version": "0.1.0" });
    value["payload"]["ux"]["dataBindings"][2]["query"] = json!({
        "kind": "object_runtime_detail", "pageId": "customer", "parameterId": "customerNumber"
    });
    // The row navigation must now carry actual runtime metadata.
    value["payload"]["ux"]["navigationItems"][0]["destination"]["parameters"]["customerNumber"] = json!({ "kind": "context_parameter", "pageId": "customer", "parameterId": "customerNumber" });
    // Remove the cross-entity row navigation from this isolated detail fixture.
    value["payload"]["ux"]["navigationItems"] = json!([]);
    value["payload"]["ux"]["pages"][0]["root"]["children"][0]["children"]
        .as_array_mut()
        .expect("row children")
        .retain(|node| node["component"]["kind"] != "navigation");
    let mut base = support_artifacts();
    base[1]["payload"]["objectType"]
        .as_object_mut()
        .expect("customer")
        .remove("identity");
    assert_clean(&findings_with_support(value.clone(), base.clone(), &[]));
    value["payload"]["ux"]["pages"][1]["contextParams"][0]["runtimeKeyOf"]["id"] =
        json!("commerce.Order.entity");
    assert_issue(
        &findings_with_support(value, base, &[]),
        "/query/parameterId",
        "exact bound entity",
    );
}

#[test]
fn computed_child_properties_are_unavailable_in_row_conditions() {
    let mut support = ordered_rows_support();
    support[1]["payload"]["objectType"]["properties"].as_array_mut().expect("properties").push(json!({
        "id": "computed_quantity", "type": { "kind": "integer" }, "computed": "self.quantity * 2"
    }));
    let mut widget = ordered_rows_widget();
    let fields = widget["payload"]["ux"]["fields"]
        .as_array_mut()
        .expect("fields");
    let field = fields
        .iter_mut()
        .find(|field| field["fieldId"] == "edit-item-quantity")
        .expect("quantity field");
    field["readOnlyWhen"] = json!({ "expression": "input.row.computed_quantity > 0", "inputs": { "rowGridId": "edit-order-grid" } });
    assert_issue(
        &findings_with_support(widget, support, &[]),
        "/readOnlyWhen",
        "computed_quantity",
    );
}

#[test]
fn child_grid_row_ids_must_admit_generated_uuids() {
    for (type_, constraints, refused) in [
        (
            json!({ "kind": "text", "format": "email" }),
            json!({}),
            true,
        ),
        (json!({ "kind": "text" }), json!({ "minLength": 100 }), true),
        (json!({ "kind": "text" }), json!({ "maxLength": 20 }), true),
        (
            json!({ "kind": "text" }),
            json!({ "pattern": "^ROW-[0-9]+$" }),
            true,
        ),
        (
            json!({ "kind": "text", "format": "uuid" }),
            json!({ "minLength": 36, "maxLength": 36 }),
            false,
        ),
    ] {
        let mut support = ordered_rows_support();
        let property = support[1]["payload"]["objectType"]["properties"]
            .as_array_mut()
            .expect("properties")
            .iter_mut()
            .find(|property| property["id"] == "row_id")
            .expect("row ID");
        property["type"] = type_;
        property["constraints"] = constraints;
        let findings = findings_with_support(ordered_rows_widget(), support, &[]);
        if refused {
            assert_issue(&findings, "/component/collectionId", "generated UUIDs");
        } else {
            assert_clean(&findings);
        }
    }
}

#[test]
fn generated_parent_key_controls_require_display_including_inherited_keys() {
    for inherited in [false, true] {
        let mut support = ordered_rows_support();
        let series = json!({"pattern": "ORD-{#####}", "timezone": "UTC",
            "resetPolicy": "never", "gapPolicy": "gapless_within_commit"});
        support[0]["payload"]["objectType"]["identity"][0]["namingSeries"] = series;
        if inherited {
            let mut base = support[0].clone();
            base["artifactId"] = json!("commerce.OrderBase.entity");
            base["name"] = json!("commerce.OrderBase");
            base["payload"]["objectType"]["name"] = json!("commerce.OrderBase");
            base["payload"]["objectType"]["links"] = json!([]);
            base["payload"]["objectType"]["ownedCollections"] = json!([]);
            support[0]["payload"]["objectType"]["identity"] = json!([]);
            support[0]["payload"]["objectType"]["extends"] = json!("commerce.OrderBase");
            // The list's indexed sort properties must also live at the root.
            support[0]["payload"]["objectType"]["properties"] = json!([]);
            support.push(base);
        }
        let mut widget = ordered_rows_widget();
        let ux = &mut widget["payload"]["ux"];
        ux["forms"]
            .as_array_mut()
            .expect("forms")
            .retain(|form| form["formId"] != "create-order-form");
        ux["fields"]
            .as_array_mut()
            .expect("fields")
            .retain(|field| field["target"]["formId"] != "create-order-form");
        ux["actionBindings"]
            .as_array_mut()
            .expect("bindings")
            .retain(|binding| binding["bindingId"] != "create-order-rows");
        ux["pages"][0]["root"]["children"]
            .as_array_mut()
            .expect("nodes")
            .retain(|node| node["nodeId"] != "create-order-form-node");
        ux["fields"].as_array_mut().expect("fields").push(json!({
            "fieldId": "edit-generated-key", "target": {"kind": "parent_property", "formId": "edit-order-form",
                "inputName": "parent", "propertyId": "order_id"}, "label": {"en": "Generated key"}, "control": "display"
        }));
        ux["pages"][0]["root"]["children"]
            .as_array_mut()
            .expect("nodes")
            .iter_mut()
            .find(|node| node["nodeId"] == "edit-order-form-node")
            .expect("edit form")["children"]
            .as_array_mut()
            .expect("children")
            .push(json!({"nodeId": "edit-generated-key-node",
                "component": {"kind": "field", "fieldId": "edit-generated-key"}, "children": []}));
        assert_clean(&findings_with_support(widget.clone(), support.clone(), &[]));
        for conditional in [false, true] {
            let fields = widget["payload"]["ux"]["fields"]
                .as_array_mut()
                .expect("fields");
            let index = fields
                .iter()
                .position(|field| field["fieldId"] == "edit-generated-key")
                .expect("generated field");
            fields[index]["control"] = json!("text");
            if conditional {
                fields[index]["readOnlyWhen"] = json!({"expression": "true", "inputs": {}});
            }
            assert_issue(
                &findings_with_support(widget.clone(), support.clone(), &[]),
                &format!("/fields/{index}/control"),
                "generated parent properties are also read-only",
            );
        }
    }
}

fn metric_buckets_widget(nullable: bool) -> (Value, Vec<Value>) {
    let mut support = support_artifacts();
    support[0]["payload"]["objectType"]["properties"]
        .as_array_mut()
        .expect("order properties")
        .push(json!({
            "id": "buckets",
            "type": { "kind": "json-ext" },
            "constraints": { "nullable": nullable }
        }));
    let mut value = widget();
    value["payload"]["ux"]["fields"][1]["target"]["property"]["propertyId"] = json!("buckets");
    value["payload"]["ux"]["fields"][1]["format"] = json!("metric_buckets");
    value["payload"]["ux"]["pages"][0]["root"]["children"][0]["component"]["fields"][1]["propertyId"] =
        json!("buckets");
    (value, support)
}

#[test]
fn metric_bucket_display_admits_json_ext_including_nullable_results() {
    for nullable in [false, true] {
        let (value, support) = metric_buckets_widget(nullable);
        assert_clean(&findings_with_support(value, support, &[]));
    }
}

#[test]
fn metric_bucket_format_rejects_primitive_properties_and_edit_controls() {
    let mut primitive = widget();
    primitive["payload"]["ux"]["fields"][1]["format"] = json!("metric_buckets");
    assert_issue(
        &widget_findings(primitive),
        "/fields/1/format",
        "json-ext property",
    );

    let (mut editable, support) = metric_buckets_widget(false);
    editable["payload"]["ux"]["fields"][1]["control"] = json!("text_area");
    let found = findings_with_support(editable, support, &[]);
    assert_issue(&found, "/fields/1/control", "incompatible");
    assert_issue(&found, "/fields/1/format", "incompatible");
}

#[test]
fn metric_bucket_format_is_not_an_action_input_presentation() {
    let mut value = widget();
    value["payload"]["ux"]["fields"][4]["format"] = json!("metric_buckets");
    assert_issue(&widget_findings(value), "/fields/4/format", "not an input");
}
