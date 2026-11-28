//! Artifact-set checks for the versioned Widget UX authoring contract (#3173).

use crate::{JsonAt, TestMust};
use std::collections::BTreeSet;

use norn_checker::{Diagnostic, EntityArtifactSetContext};
use norn_ir::ArtifactEnvelope;
use norn_ui_checker::check_artifact_set;
use serde_json::{Value, json};

const WIDGET_ID: &str = "commerce.widget.orders";
const WIDGET_FIXTURE: &str = include_str!("../../../fixtures/ir/ux-orders-customer.widget.json");
const RECORD_PAGE: &str =
    include_str!("../../../registry/studio/artifacts/norn.studio.record.widget.json");
const DECISION_SHEET: &str =
    include_str!("../../../apps/studio/src/decisions/decision-sheet.widget.json");

fn widget() -> Value {
    serde_json::from_str(WIDGET_FIXTURE).req("UX fixture should be JSON")
}

fn envelope(value: Value) -> ArtifactEnvelope {
    serde_json::from_value(value).req("artifact envelope should decode")
}

fn j(raw: &str) -> Value {
    serde_json::from_str(raw).req(raw)
}

fn ux_mut(value: &mut Value) -> &mut Value {
    value.at_mut("payload").at_mut("ux")
}

fn ptr_mut<'a>(value: &'a mut Value, pointer: &str) -> &'a mut Value {
    value.pointer_mut(pointer).req(pointer)
}

fn arr_mut<'a>(value: &'a mut Value, pointer: &str) -> &'a mut Vec<Value> {
    ptr_mut(value, pointer).as_array_mut().req(pointer)
}

fn set_key(value: &mut Value, pointer: &str, key: &str, new: Value) {
    ptr_mut(value, pointer).set(key, new);
}

fn del_key(value: &mut Value, pointer: &str, key: &str) {
    let _ = ptr_mut(value, pointer)
        .as_object_mut()
        .req(pointer)
        .remove(key);
}

fn push_ptr(value: &mut Value, pointer: &str, item: Value) {
    arr_mut(value, pointer).push(item);
}

fn find_by_mut<'a>(value: &'a mut Value, pointer: &str, key: &str, id: &str) -> &'a mut Value {
    arr_mut(value, pointer)
        .iter_mut()
        .find(|item| item.at(key) == id)
        .req(id)
}

fn field_index(value: &Value, field_id: &str) -> usize {
    value
        .pointer("/payload/ux/fields")
        .req("fields")
        .as_array()
        .req("fields array")
        .iter()
        .position(|field| field.at("fieldId") == field_id)
        .req(field_id)
}

fn dep_entry(artifact_id: &str, kind: &str) -> Value {
    json!({ "artifactId": artifact_id, "kind": kind, "version": "0.1.0", "required": true })
}

fn push_dep(artifact: &mut Value, artifact_id: &str, kind: &str) {
    push_ptr(artifact, "/dependencies", dep_entry(artifact_id, kind));
}

fn field_node(node_id: &str, field_id: &str) -> Value {
    json!({ "nodeId": node_id, "component": { "kind": "field", "fieldId": field_id }, "children": [] })
}

fn widget_with(change: impl FnOnce(&mut Value)) -> Value {
    let mut value = widget();
    change(ux_mut(&mut value));
    value
}

fn apply_edits(root: &mut Value, edits: &[&str]) {
    for &spec in edits {
        if let Some(id) = spec.strip_prefix("drop ") {
            drop_transition(root, id);
        } else if let Some(rest) = spec.strip_prefix("del ") {
            let (ptr, key) = rest.split_once(' ').req(spec);
            del_key(root, if ptr == "." { "" } else { ptr }, key);
        } else if let Some(rest) = spec.strip_prefix("push ") {
            let (ptr, raw) = rest.split_once(" = ").req(spec);
            push_ptr(root, if ptr == "." { "" } else { ptr }, j(raw));
        } else if let Some(rest) = spec.strip_prefix("set ") {
            let (lhs, raw) = rest.split_once(" = ").req(spec);
            let (ptr, key) = lhs.split_once(' ').req(spec);
            set_key(root, if ptr == "." { "" } else { ptr }, key, j(raw));
        } else {
            unreachable!("unknown edit spec: {spec}");
        }
    }
}

fn entity(
    artifact_id: &str,
    object_name: &str,
    properties: &Value,
    identity: &str,
    links: &Value,
) -> Value {
    json!({
        "irVersion": "0.1", "artifactId": artifact_id, "kind": "entity",
        "namespace": artifact_id.split('.').next().req("namespace"),
        "name": object_name, "version": "0.1.0", "lifecycle": "draft",
        "scope": { "orgId": "org_demo", "environmentId": "dev" },
        "dependencies": [], "provenance": { "source": "authoring_sdk" },
        "payload": {
            "entityVersion": "2.0",
            "objectType": {
                "name": object_name, "properties": properties, "links": links,
                "identity": [{ "name": "primary", "properties": [identity] }]
            }
        }
    })
}

fn order_entity() -> Value {
    entity(
        "commerce.Order.entity",
        "commerce.Order",
        &j(r#"[
            {"id":"order_id","type":{"kind":"text"}},
            {"id":"order_number","type":{"kind":"text"}},
            {"id":"customer_number","type":{"kind":"text"}},
            {"id":"status","type":{"kind":"text"}},
            {"id":"revision","type":{"kind":"integer"}},
            {"id":"is_priority","type":{"kind":"boolean"}},
            {"id":"optional_note","type":{"kind":"text"},"constraints":{"nullable":true}}
        ]"#),
        "order_id",
        &j(
            r#"[{"id":"customer","target":"crm.Customer","cardinality":{"to":"one","from":"many"}}]"#,
        ),
    )
}

fn customer_entity() -> Value {
    entity(
        "crm.Customer.entity",
        "crm.Customer",
        &j(r#"[
            {"id":"customer_number","type":{"kind":"text"}},
            {"id":"display_name","type":{"kind":"text"}},
            {"id":"revision","type":{"kind":"text"}}
        ]"#),
        "customer_number",
        &json!([]),
    )
}

fn action_artifact() -> Value {
    j(r#"{
        "irVersion":"0.1","artifactId":"commerce.order.approve.action","kind":"action",
        "namespace":"commerce","name":"Approve order","version":"0.1.0","lifecycle":"draft",
        "scope":{"orgId":"org_demo","environmentId":"dev"},"dependencies":[],
        "provenance":{"source":"authoring_sdk"},
        "payload":{
            "actionId":"commerce.order.approve","scopeRequired":["orgId","environmentId"],
            "idempotent":true,"ledgered":true,"ledgerCategory":"commerce.order",
            "inputSchema":{
                "type":"object",
                "properties":{
                    "orderId":{"type":"string"},"reason":{"type":"string"},"note":{"type":"string"},
                    "comment":{"type":["string","null"]},
                    "metadata":{"type":"object","properties":{"source":{"type":"string"},"ticket":{"type":"string"}},"required":["source"],"additionalProperties":false},
                    "expectedVersion":{"type":"integer"}
                },
                "required":["orderId","reason"],"additionalProperties":false
            },
            "outputSchema":{"type":"object","properties":{},"additionalProperties":false}
        }
    }"#)
}

fn transition_order_entity() -> Value {
    let mut entity = order_entity();
    *ptr_mut(&mut entity, "/payload/objectType/properties/3") = j(
        r#"{"id":"status","type":{"kind":"enum","values":[{"value":"draft"},{"value":"approved"}]},"default":"draft"}"#,
    );
    set_key(
        &mut entity,
        "/payload/objectType",
        "lifecycles",
        j(
            r#"[{"property":"status","initial":["draft"],"terminal":["approved"],"transitions":[{"id":"approve","from":["draft"],"to":"approved"}]}]"#,
        ),
    );
    entity
}

fn fixture_approve_transition_action() -> Value {
    let mut action = action_artifact();
    apply_edits(
        &mut action,
        &[
            r#"set /payload writes = [{"entityKind":"commerce.Order","change":"modified"}]"#,
            r#"set /payload effectProgram = {"kind":"fire_transition","objectIdField":"orderId","objectKind":"commerce.Order","entityArtifactId":"commerce.Order.entity","entityArtifactVersion":"0.1.0","lifecycle":"status","transition":"approve"}"#,
        ],
    );
    action
}

fn zero_input_action_artifact() -> Value {
    let mut action = action_artifact();
    apply_edits(
        &mut action,
        &[
            r#"set . artifactId = "commerce.order.noop.action""#,
            r#"set . name = "No-op order action""#,
            r#"set /payload actionId = "commerce.order.noop""#,
            r#"set /payload inputSchema = {"type":"object","properties":{},"additionalProperties":false}"#,
        ],
    );
    action
}

fn required_version_action_artifact() -> Value {
    let mut action = action_artifact();
    apply_edits(
        &mut action,
        &[
            r#"set . artifactId = "commerce.order.edit.action""#,
            r#"set . name = "Edit order""#,
            r#"set /payload actionId = "commerce.order.edit""#,
            r#"set /payload/inputSchema required = ["orderId","reason","expectedVersion"]"#,
        ],
    );
    action
}

fn support_artifacts() -> Vec<Value> {
    vec![order_entity(), customer_entity(), action_artifact()]
}

fn ordered_rows_support() -> Vec<Value> {
    let mut parent = order_entity();
    apply_edits(
        &mut parent,
        &[
            r#"push /payload/objectType/links = {"id":"items","target":"commerce.OrderItem","cardinality":{"to":"many","from":"one"}}"#,
            r#"set /payload/objectType ownedCollections = [{"id":"items","childType":"commerce.OrderItem","rowIdProperty":"row_id","orderingKey":"idx","cascadeOnParentDelete":true,"maxRows":3}]"#,
        ],
    );
    let child = entity(
        "commerce.OrderItem.entity",
        "commerce.OrderItem",
        &j(r#"[
            {"id":"row_id","type":{"kind":"text"}},{"id":"idx","type":{"kind":"integer"}},
            {"id":"item_code","type":{"kind":"text"}},{"id":"quantity","type":{"kind":"integer"}},
            {"id":"optional_note","type":{"kind":"text"},"constraints":{"nullable":true}}
        ]"#),
        "row_id",
        &json!([]),
    );
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
        let mut act = j(r#"{
            "irVersion":"0.1","kind":"action","namespace":"commerce","version":"0.1.0","lifecycle":"draft",
            "scope":{"orgId":"org_demo","environmentId":"dev"},"provenance":{"source":"authoring_sdk"},
            "dependencies":[
                {"artifactId":"commerce.Order.entity","kind":"entity","version":"0.1.0","required":true},
                {"artifactId":"commerce.OrderItem.entity","kind":"entity","version":"0.1.0","required":true}
            ],
            "payload":{
                "scopeRequired":["orgId","environmentId"],"idempotent":true,"ledgered":true,"ledgerCategory":"commerce.order",
                "inputSchema":{"type":"object","properties":{
                    "orderId":{"type":"string"},"parent":{"type":["null","boolean","number","string","array","object"]},
                    "rows":{"type":["null","boolean","number","string","array","object"]},"expectedParentVersion":{"type":"string"}
                },"required":["orderId","parent","rows","expectedParentVersion"],"additionalProperties":false},
                "outputSchema":{"type":"object","properties":{},"additionalProperties":false},
                "writes":[
                    {"entityKind":"commerce.Order","change":"created"},{"entityKind":"commerce.Order","change":"modified"},
                    {"entityKind":"commerce.OrderItem","change":"created"},{"entityKind":"commerce.OrderItem","change":"modified"},
                    {"entityKind":"commerce.OrderItem","change":"deleted"}
                ],
                "effectProgram":{
                    "kind":"mutate_parent_with_rows","objectIdField":"orderId","objectKind":"commerce.Order",
                    "entityArtifactId":"commerce.Order.entity","entityArtifactVersion":"0.1.0",
                    "parentValuesField":"parent","rowOpsField":"rows","parentVersionField":"expectedParentVersion",
                    "collections":[{"id":"items"}]
                }
            }
        }"#);
        act.set("artifactId", json!(artifact_id));
        act.set("name", json!(action_id));
        set_key(&mut act, "/payload", "actionId", json!(action_id));
        artifacts.push(act);
    }
    artifacts
}

fn ordered_rows_with_display_only_properties() -> Vec<Value> {
    let mut artifacts = ordered_rows_support();
    let parent = artifacts.first_mut().req("parent");
    *find_by_mut(parent, "/payload/objectType/properties", "id", "status") = j(
        r#"{"id":"status","type":{"kind":"enum","values":[{"value":"draft"},{"value":"approved"}]},"default":"draft"}"#,
    );
    apply_edits(
        parent,
        &[
            r#"push /payload/objectType/properties = {"id":"display_label","type":{"kind":"text"},"computed":"self.order_number + \" view\""}"#,
            r#"set /payload/objectType lifecycles = [{"property":"status","initial":["draft"],"terminal":["approved"],"transitions":[{"id":"approve","from":["draft"],"to":"approved"}]}]"#,
        ],
    );
    let child = artifacts.get_mut(1).req("child");
    apply_edits(
        child,
        &[
            r#"push /payload/objectType/properties = {"id":"display_label","type":{"kind":"text"},"computed":"self.item_code + \" view\""}"#,
            r#"push /payload/objectType/properties = {"id":"row_state","type":{"kind":"enum","values":[{"value":"draft"},{"value":"archived"}]},"default":"draft"}"#,
            r#"set /payload/objectType lifecycles = [{"property":"row_state","initial":["draft"],"terminal":["archived"],"transitions":[{"id":"archive","from":["draft"],"to":"archived"}]}]"#,
        ],
    );
    artifacts
}

fn ordered_rows_widget() -> Value {
    let mut value = widget();
    for (id, kind) in [
        ("commerce.OrderItem.entity", "entity"),
        ("commerce.order.create-with-rows.action", "action"),
        ("commerce.order.edit-with-rows.action", "action"),
    ] {
        push_dep(&mut value, id, kind);
    }
    apply_edits(
        ux_mut(&mut value),
        &[
            r#"push /dataBindings = {"bindingId":"edit-order-detail","object":{"id":"commerce.Order.entity","version":"0.1.0"},"query":{"kind":"object_detail","key":[{"propertyId":"order_id","value":{"kind":"literal","value":"ORD-1"}}]}}"#,
            r#"push /dataBindings = {"bindingId":"edit-order-items","object":{"id":"commerce.OrderItem.entity","version":"0.1.0"},"query":{"kind":"link_traversal","sourceBindingId":"edit-order-detail","linkId":"items","maxDepth":1,"maxRows":3,"page":{"size":3,"maxSize":3}}}"#,
            r#"push /actionBindings = {"bindingId":"create-order-rows","action":{"id":"commerce.order.create-with-rows.action","version":"0.1.0"},"inputMapping":{"orderId":"input.draft.orderId","parent":"input.draft.parent","rows":"input.draft.rows","expectedParentVersion":"\"0\""},"inputs":{"draftFormId":"create-order-form"},"requiredCapability":"commerce.order.write"}"#,
            r#"push /actionBindings = {"bindingId":"edit-order-rows","action":{"id":"commerce.order.edit-with-rows.action","version":"0.1.0"},"inputMapping":{"orderId":"input.recordKey","parent":"input.draft.parent","rows":"input.draft.rows","expectedParentVersion":"input.recordVersion"},"inputs":{"draftFormId":"edit-order-form","recordBindingId":"edit-order-detail"},"requiredCapability":"commerce.order.write"}"#,
            r#"push /forms = {"formId":"create-order-form","mode":"create","inputNames":["orderId","parent","rows"],"submitBindingId":"create-order-rows","submit":{"label":{"en":"Create order"},"confirmation":{"severity":"destructive","strength":"click"}}}"#,
            r#"push /forms = {"formId":"edit-order-form","mode":"edit","dataBindingId":"edit-order-detail","inputNames":["parent","rows"],"submitBindingId":"edit-order-rows","submit":{"label":{"en":"Save order"},"confirmation":{"severity":"destructive","strength":"click"}}}"#,
            r#"push /fields = {"fieldId":"create-order-id","target":{"kind":"form_input","formId":"create-order-form","inputName":"orderId"},"label":{"en":"Order id"},"control":"text"}"#,
            r#"push /fields = {"fieldId":"create-order-number","target":{"kind":"parent_property","formId":"create-order-form","inputName":"parent","propertyId":"order_number"},"label":{"en":"Order number"},"control":"text"}"#,
            r#"push /fields = {"fieldId":"create-parent-order-id","target":{"kind":"parent_property","formId":"create-order-form","inputName":"parent","propertyId":"order_id"},"label":{"en":"Stored order id"},"control":"text"}"#,
            r#"push /fields = {"fieldId":"create-parent-customer","target":{"kind":"parent_property","formId":"create-order-form","inputName":"parent","propertyId":"customer_number"},"label":{"en":"Customer number"},"control":"text"}"#,
            r#"push /fields = {"fieldId":"create-parent-status","target":{"kind":"parent_property","formId":"create-order-form","inputName":"parent","propertyId":"status"},"label":{"en":"Status"},"control":"text"}"#,
            r#"push /fields = {"fieldId":"create-parent-revision","target":{"kind":"parent_property","formId":"create-order-form","inputName":"parent","propertyId":"revision"},"label":{"en":"Revision"},"control":"number"}"#,
            r#"push /fields = {"fieldId":"create-parent-priority","target":{"kind":"parent_property","formId":"create-order-form","inputName":"parent","propertyId":"is_priority"},"label":{"en":"Priority"},"control":"checkbox"}"#,
            r#"push /fields = {"fieldId":"create-item-code","target":{"kind":"child_row_property","formId":"create-order-form","collectionId":"items","propertyId":"item_code"},"label":{"en":"Item code"},"control":"text"}"#,
            r#"push /fields = {"fieldId":"create-item-quantity","target":{"kind":"child_row_property","formId":"create-order-form","collectionId":"items","propertyId":"quantity"},"label":{"en":"Quantity"},"control":"number"}"#,
            r#"push /fields = {"fieldId":"edit-order-number","target":{"kind":"parent_property","formId":"edit-order-form","inputName":"parent","propertyId":"order_number"},"label":{"en":"Order number"},"control":"text"}"#,
            r#"push /fields = {"fieldId":"edit-item-code","target":{"kind":"child_row_property","formId":"edit-order-form","collectionId":"items","propertyId":"item_code"},"label":{"en":"Item code"},"control":"text"}"#,
            r#"push /fields = {"fieldId":"edit-item-quantity","target":{"kind":"child_row_property","formId":"edit-order-form","collectionId":"items","propertyId":"quantity"},"label":{"en":"Quantity"},"control":"number"}"#,
            r#"push /fields = {"fieldId":"edit-item-note","target":{"kind":"child_row_property","formId":"edit-order-form","collectionId":"items","propertyId":"optional_note"},"label":{"en":"Note"},"control":"text","readOnlyWhen":{"expression":"input.row.item_code == \"LOCKED\"","inputs":{"rowGridId":"edit-order-grid"}}}"#,
            r#"push /pages/0/root/children = {"nodeId":"create-order-form-node","component":{"kind":"form","formId":"create-order-form"},"children":[{"nodeId":"create-order-id-node","component":{"kind":"field","fieldId":"create-order-id"},"children":[]},{"nodeId":"create-order-number-node","component":{"kind":"field","fieldId":"create-order-number"},"children":[]},{"nodeId":"create-parent-order-id-node","component":{"kind":"field","fieldId":"create-parent-order-id"},"children":[]},{"nodeId":"create-parent-customer-node","component":{"kind":"field","fieldId":"create-parent-customer"},"children":[]},{"nodeId":"create-parent-status-node","component":{"kind":"field","fieldId":"create-parent-status"},"children":[]},{"nodeId":"create-parent-revision-node","component":{"kind":"field","fieldId":"create-parent-revision"},"children":[]},{"nodeId":"create-parent-priority-node","component":{"kind":"field","fieldId":"create-parent-priority"},"children":[]},{"nodeId":"create-order-grid","component":{"kind":"child_grid","formId":"create-order-form","label":{"en":"Items"},"collectionId":"items","orderingKey":"idx","rowOpsInputName":"rows","maxRows":3,"fieldIds":["create-item-code","create-item-quantity"]},"children":[]}]}"#,
            r#"push /pages/0/root/children = {"nodeId":"edit-order-detail-node","component":{"kind":"detail","dataBindingId":"edit-order-detail"},"children":[]}"#,
            r#"push /pages/0/root/children = {"nodeId":"edit-order-form-node","component":{"kind":"form","formId":"edit-order-form"},"children":[{"nodeId":"edit-order-number-node","component":{"kind":"field","fieldId":"edit-order-number"},"children":[]},{"nodeId":"edit-order-grid","component":{"kind":"child_grid","formId":"edit-order-form","label":{"en":"Items"},"collectionId":"items","orderingKey":"idx","dataBindingId":"edit-order-items","rowOpsInputName":"rows","maxRows":3,"fieldIds":["edit-item-code","edit-item-quantity","edit-item-note"]},"children":[]}]}"#,
        ],
    );
    value
}

fn ordered_rows_display_widget() -> Value {
    let mut value = ordered_rows_widget();
    let ux = ux_mut(&mut value);
    for field_id in ["create-parent-status", "edit-order-number"] {
        find_by_mut(ux, "/fields", "fieldId", field_id).set("control", json!("display"));
    }
    apply_edits(
        ux,
        &[
            r#"push /fields = {"fieldId":"edit-parent-display-label","target":{"kind":"parent_property","formId":"edit-order-form","inputName":"parent","propertyId":"display_label"},"label":{"en":"Parent display label"},"control":"display"}"#,
            r#"push /fields = {"fieldId":"edit-parent-lifecycle-state","target":{"kind":"parent_property","formId":"edit-order-form","inputName":"parent","propertyId":"status"},"label":{"en":"Parent lifecycle state"},"control":"display"}"#,
            r#"push /fields = {"fieldId":"edit-item-display-label","target":{"kind":"child_row_property","formId":"edit-order-form","collectionId":"items","propertyId":"display_label"},"label":{"en":"Item display label"},"control":"display"}"#,
            r#"push /fields = {"fieldId":"edit-item-lifecycle-state","target":{"kind":"child_row_property","formId":"edit-order-form","collectionId":"items","propertyId":"row_state"},"label":{"en":"Item lifecycle state"},"control":"display"}"#,
        ],
    );
    let edit_form = find_by_mut(
        ux,
        "/pages/0/root/children",
        "nodeId",
        "edit-order-form-node",
    );
    arr_mut(edit_form, "/children").extend([
        field_node(
            "edit-parent-display-label-node",
            "edit-parent-display-label",
        ),
        field_node(
            "edit-parent-lifecycle-state-node",
            "edit-parent-lifecycle-state",
        ),
    ]);
    set_key(
        find_by_mut(edit_form, "/children", "nodeId", "edit-order-grid"),
        "/component",
        "fieldIds",
        j(
            r#"["edit-item-code","edit-item-quantity","edit-item-note","edit-item-display-label","edit-item-lifecycle-state"]"#,
        ),
    );
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

fn widget_findings_with_base(widget: Value, base: &[Value]) -> Vec<Diagnostic> {
    findings_with_base(widget, base)
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
    assert_eq!(found, [], "expected a clean artifact set; got {found:#?}");
}

/// For fixtures that deliberately strip a widget down to one reference-semantics question (an
/// isolated page or binding): the contract must be clean except for the named interaction findings
/// that stripping causes. Interaction rules judge whole documents and are covered by
/// `tests/ui_interaction.rs`.
fn assert_contract_clean(found: &[Diagnostic], expected_interaction: &[&str]) {
    let unexpected: Vec<&Diagnostic> = found
        .iter()
        .filter(|finding| !expected_interaction.contains(&finding.code.as_str()))
        .collect();
    assert_eq!(
        unexpected.len(),
        0,
        "expected a clean widget contract; got {unexpected:#?}"
    );
}

fn assert_edit_matrix(cases: &[(&[&str], &str, &str)]) {
    for &(edits, pointer, message) in cases {
        assert_issue(
            &widget_findings(widget_with(|ux| apply_edits(ux, edits))),
            pointer,
            message,
        );
    }
}

fn with_base_order(base_properties: &Value) -> Vec<Value> {
    let mut support = support_artifacts();
    apply_edits(
        support.first_mut().req("order"),
        &[
            r#"set . dependencies = [{"artifactId":"commerce.BaseOrder.entity","kind":"entity","version":"0.1.0","required":true}]"#,
            r#"set /payload/objectType extends = "commerce.BaseOrder""#,
            r#"set /payload/objectType/identity/0 name = "order_local""#,
        ],
    );
    support.push(entity(
        "commerce.BaseOrder.entity",
        "commerce.BaseOrder",
        base_properties,
        "base_id",
        &json!([]),
    ));
    support
}

fn overflow_order_support(extra_prop: &str) -> Vec<Value> {
    let mut support = support_artifacts();
    let properties = arr_mut(
        support.first_mut().req("order"),
        "/payload/objectType/properties",
    );
    for index in 0..100 {
        properties
            .push(json!({ "id": format!("extra_field_{index:03}"), "type": { "kind": "text" } }));
    }
    properties.push(json!({ "id": extra_prop, "type": { "kind": "text" } }));
    support
}

fn add_approve_form_field(
    ux: &mut Value,
    input_name: &str,
    field_id: &str,
    control: &str,
    map_expr: Option<&str>,
    required_when: Option<Value>,
) {
    push_ptr(ux, "/forms/0/inputNames", json!(input_name));
    if let Some(expr) = map_expr {
        set_key(
            ux,
            "/actionBindings/0/inputMapping",
            input_name,
            json!(expr),
        );
    }
    let mut field = json!({
        "fieldId": field_id,
        "target": { "kind": "form_input", "formId": "approve-form", "inputName": input_name },
        "label": { "en": input_name }, "control": control
    });
    if let Some(condition) = required_when {
        field.set("requiredWhen", condition);
    }
    push_ptr(ux, "/fields", field);
    push_ptr(
        ux,
        "/pages/0/root/children/2/children",
        json!({
            "nodeId": format!("{field_id}-node"), "semanticId": format!("{field_id}-field"),
            "component": { "kind": "field", "fieldId": field_id }, "children": []
        }),
    );
}

// ============================================================================
// Ordered rows and owned child grids
// ============================================================================

#[test]
fn create_and_edit_forms_admit_bounded_owned_child_grids_with_row_conditions() {
    assert_clean(&findings_with_support(
        ordered_rows_widget(),
        ordered_rows_support(),
        &[],
    ));
}

#[test]
fn keyless_parent_row_edit_form_accepts_runtime_detail_and_requires_exact_parent() {
    let mut value = ordered_rows_widget();
    let ux = ux_mut(&mut value);
    let page_id = ptr_mut(ux, "/pages/0/pageId")
        .as_str()
        .req("page id")
        .to_owned();
    apply_edits(
        ux,
        &[
            r#"set /pages/0 contextParams = [{"paramId":"key","type":"text","required":true,"runtimeKeyOf":{"id":"commerce.Order.entity","version":"0.1.0"}}]"#,
            "del . app",
        ],
    );
    set_key(
        find_by_mut(ux, "/dataBindings", "bindingId", "edit-order-detail"),
        "",
        "query",
        json!({ "kind": "object_runtime_detail", "pageId": page_id, "parameterId": "key" }),
    );
    let mut support = ordered_rows_support();
    del_key(
        support.first_mut().req("parent"),
        "/payload/objectType",
        "identity",
    );
    assert_clean(&findings_with_support(value.clone(), support.clone(), &[]));
    set_key(
        find_by_mut(
            ux_mut(&mut value),
            "/dataBindings",
            "bindingId",
            "edit-order-detail",
        ),
        "/object",
        "id",
        json!("commerce.OrderItem.entity"),
    );
    assert_issue(
        &findings_with_support(value, support, &[]),
        "/dataBindingId",
        "edit form must bind the exact parent entity detail",
    );
}

#[test]
fn child_action_derivations_require_display_controls_in_create_and_edit_grids() {
    let mut support = ordered_rows_support();
    arr_mut(
        support.get_mut(1).req("child"),
        "/payload/objectType/properties",
    )
    .extend([
        j(r#"{"id":"double_quantity","type":{"kind":"integer"}}"#),
        j(r#"{"id":"has_quantity","type":{"kind":"boolean"}}"#),
    ]);
    for action in support.iter_mut().filter(|a| {
        a.pointer("/payload/effectProgram/kind") == Some(&json!("mutate_parent_with_rows"))
    }) {
        set_key(
            action,
            "/payload/effectProgram/collections/0",
            "derive",
            j(r#"{"double_quantity":"self.quantity * 2","has_quantity":"self.quantity > 0"}"#),
        );
    }
    let mut widget = ordered_rows_widget();
    for mode in ["create", "edit"] {
        for property in ["double_quantity", "has_quantity"] {
            let field_id = format!("{mode}-{property}");
            push_ptr(
                ux_mut(&mut widget),
                "/fields",
                json!({
                    "fieldId": field_id,
                    "target": { "kind": "child_row_property", "formId": format!("{mode}-order-form"), "collectionId": "items", "propertyId": property },
                    "label": { "en": property }, "control": "display"
                }),
            );
            let form = find_by_mut(
                ux_mut(&mut widget),
                "/pages/0/root/children",
                "nodeId",
                &format!("{mode}-order-form-node"),
            );
            let grid = find_by_mut(form, "/children", "nodeId", &format!("{mode}-order-grid"));
            push_ptr(grid, "/component/fieldIds", json!(field_id));
        }
    }
    assert_clean(&findings_with_support(widget.clone(), support.clone(), &[]));
    for mode in ["create", "edit"] {
        for (property, control) in [("double_quantity", "number"), ("has_quantity", "checkbox")] {
            let mut editable = widget.clone();
            let index = field_index(&editable, &format!("{mode}-{property}"));
            let fptr = format!("/fields/{index}");
            set_key(ux_mut(&mut editable), &fptr, "control", json!(control));
            set_key(
                ux_mut(&mut editable),
                &fptr,
                "readOnlyWhen",
                j(r#"{"expression":"true","inputs":{}}"#),
            );
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
fn ordered_rows_lifecycle_controls_and_defaults() {
    let support = ordered_rows_with_display_only_properties();
    assert_clean(&findings_with_support(
        ordered_rows_display_widget(),
        support.clone(),
        &[],
    ));
    for (field_id, expected_msg) in [
        (
            "edit-parent-lifecycle-state",
            "lifecycle-managed parent properties are read-only",
        ),
        (
            "edit-item-lifecycle-state",
            "lifecycle-managed properties are read-only",
        ),
    ] {
        let mut editable = ordered_rows_display_widget();
        let index = field_index(&editable, field_id);
        set_key(
            ux_mut(&mut editable),
            &format!("/fields/{index}"),
            "control",
            json!("text"),
        );
        assert_issue(
            &findings_with_support(editable, support.clone(), &[]),
            &format!("/fields/{index}/control"),
            expected_msg,
        );
    }
    for (entity_idx, prop_id, pointer, expected_msg) in [
        (
            1,
            "row_state",
            "/component/fieldIds",
            "child grid Add requires lifecycle property \"row_state\" to declare a default in one of its initial states",
        ),
        (
            0,
            "status",
            "/submitBindingId",
            "create form requires lifecycle property \"status\" to declare a default in one of its initial states",
        ),
    ] {
        let mut no_default = ordered_rows_with_display_only_properties();
        del_key(
            find_by_mut(
                no_default.get_mut(entity_idx).req("entity"),
                "/payload/objectType/properties",
                "id",
                prop_id,
            ),
            "",
            "default",
        );
        assert_issue(
            &findings_with_support(ordered_rows_display_widget(), no_default, &[]),
            pointer,
            expected_msg,
        );
    }
}

#[test]
fn ordered_rows_coverage_and_condition_refusals() {
    let cases: &[(fn(&mut Value, &mut Vec<Value>), &str, &str)] = &[
        (
            |w, _| {
                arr_mut(ux_mut(w), "/fields")
                    .retain(|f| f.at("fieldId") != "create-parent-customer")
            },
            "/forms/",
            "required parent property \"customer_number\"",
        ),
        (
            |w, _| {
                let form = find_by_mut(
                    ux_mut(w),
                    "/pages/0/root/children",
                    "nodeId",
                    "create-order-form-node",
                );
                set_key(
                    find_by_mut(form, "/children", "nodeId", "create-order-grid"),
                    "/component",
                    "fieldIds",
                    j(r#"["create-item-code"]"#),
                );
                arr_mut(ux_mut(w), "/fields").retain(|f| f.at("fieldId") != "create-item-quantity");
            },
            "/component/fieldIds",
            "required child property \"quantity\"",
        ),
        (
            |w, _| {
                set_key(
                    ux_mut(w),
                    "/actionBindings/1/inputMapping",
                    "parent",
                    json!("input.draft.parent.order_number"),
                )
            },
            "/actionBindings/1/inputMapping/parent",
            "ZEN mapping",
        ),
        (
            |w, _| {
                set_key(
                    find_by_mut(ux_mut(w), "/fields", "fieldId", "edit-item-note"),
                    "/readOnlyWhen/inputs",
                    "rowGridId",
                    json!("create-order-grid"),
                )
            },
            "/readOnlyWhen/inputs/rowGridId",
            "exact enclosing ChildGrid",
        ),
    ];
    for &(mutate, pointer, message) in cases {
        let mut w = ordered_rows_widget();
        let mut s = ordered_rows_support();
        mutate(&mut w, &mut s);
        assert_issue(&findings_with_support(w, s, &[]), pointer, message);
    }
}

#[test]
fn ordered_rows_bounds_and_computed_child_refusals() {
    let cases: &[(fn(&mut Value, &mut Vec<Value>), &str, &str)] = &[
        (
            |w, _| {
                let form = find_by_mut(
                    ux_mut(w),
                    "/pages/0/root/children",
                    "nodeId",
                    "edit-order-form-node",
                );
                set_key(
                    find_by_mut(form, "/children", "nodeId", "edit-order-grid"),
                    "/component",
                    "orderingKey",
                    json!("rank"),
                );
            },
            "component/orderingKey",
            "must match the owned collection",
        ),
        (
            |w, _| {
                let form = find_by_mut(
                    ux_mut(w),
                    "/pages/0/root/children",
                    "nodeId",
                    "edit-order-form-node",
                );
                set_key(
                    find_by_mut(form, "/children", "nodeId", "edit-order-grid"),
                    "/component",
                    "maxRows",
                    json!(4),
                );
            },
            "component/maxRows",
            "must be positive and no greater",
        ),
        (
            |w, s| {
                push_ptr(
                    s.get_mut(1).req("child"),
                    "/payload/objectType/properties",
                    j(
                        r#"{"id":"computed_quantity","type":{"kind":"integer"},"computed":"self.quantity * 2"}"#,
                    ),
                );
                set_key(
                    find_by_mut(ux_mut(w), "/fields", "fieldId", "edit-item-quantity"),
                    "",
                    "readOnlyWhen",
                    j(
                        r#"{"expression":"input.row.computed_quantity > 0","inputs":{"rowGridId":"edit-order-grid"}}"#,
                    ),
                );
            },
            "/readOnlyWhen",
            "computed_quantity",
        ),
    ];
    for &(mutate, pointer, message) in cases {
        let mut w = ordered_rows_widget();
        let mut s = ordered_rows_support();
        mutate(&mut w, &mut s);
        assert_issue(&findings_with_support(w, s, &[]), pointer, message);
    }
}

#[test]
fn child_grid_row_ids_must_admit_generated_uuids() {
    for (type_raw, constraints_raw, refused) in [
        (r#"{"kind":"text","format":"email"}"#, "{}", true),
        (r#"{"kind":"text"}"#, r#"{"minLength":100}"#, true),
        (r#"{"kind":"text"}"#, r#"{"maxLength":20}"#, true),
        (r#"{"kind":"text"}"#, r#"{"pattern":"^ROW-[0-9]+$"}"#, true),
        (
            r#"{"kind":"text","format":"uuid"}"#,
            r#"{"minLength":36,"maxLength":36}"#,
            false,
        ),
    ] {
        let mut support = ordered_rows_support();
        let prop = find_by_mut(
            support.get_mut(1).req("child"),
            "/payload/objectType/properties",
            "id",
            "row_id",
        );
        prop.set("type", j(type_raw));
        prop.set("constraints", j(constraints_raw));
        let found = findings_with_support(ordered_rows_widget(), support, &[]);
        if refused {
            assert_issue(&found, "/component/collectionId", "generated UUIDs");
        } else {
            assert_clean(&found);
        }
    }
}

#[test]
fn generated_parent_key_controls_require_display_including_inherited_keys() {
    for inherited in [false, true] {
        let mut support = ordered_rows_support();
        set_key(
            support.first_mut().req("parent"),
            "/payload/objectType/identity/0",
            "namingSeries",
            j(
                r#"{"pattern":"ORD-{#####}","timezone":"UTC","resetPolicy":"never","gapPolicy":"gapless_within_commit"}"#,
            ),
        );
        if inherited {
            let mut base = support.first().req("parent").clone();
            apply_edits(
                &mut base,
                &[
                    r#"set . artifactId = "commerce.OrderBase.entity""#,
                    r#"set . name = "commerce.OrderBase""#,
                    r#"set /payload/objectType name = "commerce.OrderBase""#,
                    r#"set /payload/objectType links = []"#,
                    r#"set /payload/objectType ownedCollections = []"#,
                ],
            );
            apply_edits(
                support.first_mut().req("parent"),
                &[
                    r#"set /payload/objectType identity = []"#,
                    r#"set /payload/objectType extends = "commerce.OrderBase""#,
                    r#"set /payload/objectType properties = []"#,
                ],
            );
            support.push(base);
        }
        let mut widget = ordered_rows_widget();
        let ux = ux_mut(&mut widget);
        arr_mut(ux, "/forms").retain(|f| f.at("formId") != "create-order-form");
        arr_mut(ux, "/fields")
            .retain(|f| f.pointer("/target/formId") != Some(&json!("create-order-form")));
        arr_mut(ux, "/actionBindings").retain(|b| b.at("bindingId") != "create-order-rows");
        arr_mut(ux, "/pages/0/root/children")
            .retain(|n| n.at("nodeId") != "create-order-form-node");
        push_ptr(
            ux,
            "/fields",
            j(
                r#"{"fieldId":"edit-generated-key","target":{"kind":"parent_property","formId":"edit-order-form","inputName":"parent","propertyId":"order_id"},"label":{"en":"Generated key"},"control":"display"}"#,
            ),
        );
        push_ptr(
            find_by_mut(
                ux,
                "/pages/0/root/children",
                "nodeId",
                "edit-order-form-node",
            ),
            "/children",
            field_node("edit-generated-key-node", "edit-generated-key"),
        );
        assert_clean(&findings_with_support(widget.clone(), support.clone(), &[]));
        let index = field_index(&widget, "edit-generated-key");
        let fptr = format!("/fields/{index}");
        for conditional in [false, true] {
            set_key(ux_mut(&mut widget), &fptr, "control", json!("text"));
            if conditional {
                set_key(
                    ux_mut(&mut widget),
                    &fptr,
                    "readOnlyWhen",
                    j(r#"{"expression":"true","inputs":{}}"#),
                );
            }
            assert_issue(
                &findings_with_support(widget.clone(), support.clone(), &[]),
                &format!("/fields/{index}/control"),
                "generated parent properties are also read-only",
            );
        }
    }
}

// ============================================================================
// Core orders fixture, transitions, and base UX refusal matrices
// ============================================================================

#[test]
fn orders_page_composes_bounded_reads_customer_navigation_and_action_form() {
    assert_clean(&findings(widget()));
}

#[test]
fn fixture_row_transition_uses_the_runtime_id_instead_of_the_business_order_id() {
    let mut value = widget_with(|ux| {
        apply_edits(
            ux,
            &[
                r#"set /actionBindings/0/inputMapping orderId = "input.recordKey""#,
                r#"set /actionBindings/0/inputMapping reason = "\"Approved\"""#,
                "del /actionBindings/0/inputs draftFormId",
                "del /actionPlacements/0 formId",
                "set . forms = []",
            ],
        );
        arr_mut(ux, "/fields").retain(|f| f.pointer("/target/kind") != Some(&json!("form_input")));
        arr_mut(ux, "/pages/0/root/children").remove(2);
    });
    let support = vec![
        transition_order_entity(),
        customer_entity(),
        fixture_approve_transition_action(),
    ];
    assert_clean(&findings_with_support(value.clone(), support.clone(), &[]));
    set_key(
        ux_mut(&mut value),
        "/actionBindings/0/inputMapping",
        "orderId",
        json!("input.record.order_id"),
    );
    assert_issue(
        &findings_with_support(value, support, &[]),
        "/pages/0/root/children/0/children/1",
        "objectIdField must map directly from input.recordKey",
    );
}

const BASE_MAPPING_AND_CONDITION_REFUSALS: &[(&[&str], &str, &str)] = &[
    (
        &[r#"set /fields/0/target/property propertyId = "order_numeral""#],
        "/fields/0/target/property",
        "property",
    ),
    (
        &[
            r#"set /actionBindings/0/inputs recordBindingId = "orders""#,
            r#"set /actionBindings/0/inputMapping reason = "input.record.revision""#,
        ],
        "/actionBindings/0/inputMapping/reason",
        "incompatible",
    ),
    (
        &["del /actionBindings/0/inputMapping orderId"],
        "/forms/0/submitBindingId",
        "required submit input",
    ),
    (
        &[r#"set /actionBindings/0/inputMapping undeclared = "input.draft.reason""#],
        "/actionBindings/0/inputMapping/undeclared",
        "not declared",
    ),
    (
        &[r#"set /actionBindings/0/inputMapping reason = "null""#],
        "/actionBindings/0/inputMapping/reason",
        "incompatible",
    ),
    (
        &[r#"set /actionBindings/0/inputMapping note = "input.record.optional_note""#],
        "/actionBindings/0/inputMapping/note",
        "may be null",
    ),
    (
        &[r#"set /actionBindings/0/inputMapping metadata = "input.draft.reason""#],
        "/actionBindings/0/inputMapping/metadata",
        "incompatible",
    ),
    (
        &[r#"set /forms/0 submitBindingId = "missing-action""#],
        "/forms/0/submitBindingId",
        "does not exist",
    ),
    (
        &[r#"set /fields/0/visibleWhen expression = "input.record.status""#],
        "/fields/0/visibleWhen",
        "Boolean",
    ),
    (
        &[r#"set /fields/0/visibleWhen expression = "input.actor.role == \"admin\"""#],
        "/fields/0/visibleWhen",
        "type-check",
    ),
    (
        &[
            r#"set /fields/0 visibleWhen = {"expression":"input.context.customerNumber != \"\"","inputs":{"contextPageId":"customer"}}"#,
        ],
        "/fields/0/visibleWhen/inputs/contextPageId",
        "page",
    ),
    (
        &[
            r#"set /actionBindings/0/inputs contextPageId = "customer""#,
            r#"set /actionBindings/0/inputMapping reason = "input.context.customerNumber""#,
        ],
        "/actionBindings/0",
        "page",
    ),
];

const BASE_BOUNDS_NAV_AND_TREE_REFUSALS: &[(&[&str], &str, &str)] = &[
    (
        &[r#"set /pages/0/root/children/0/component selection = "none""#],
        "/dataBindings/1/query/sourceBindingId",
        "source",
    ),
    (
        &[
            r#"push /overrides = {"targetSemanticId":"missing-semantic-id","value":{"kind":"label","value":{"en":"Missing"}}}"#,
        ],
        "/overrides/0/targetSemanticId",
        "composed baseline page",
    ),
    (
        &[r#"set /pages/0/root/children/0 nodeId = "orders-page""#],
        "/pages/0/root/children/0/nodeId",
        "unique",
    ),
    (
        &["set /dataBindings/0/query/page maxSize = 0"],
        "/dataBindings/0/query/page",
        "positive",
    ),
    (
        &["set /dataBindings/1/query maxRows = 0"],
        "/dataBindings/1/query",
        "positive",
    ),
    (
        &["del /dataBindings/1/query maxRows"],
        "/payload/ux",
        "missing field",
    ),
    (
        &["del /dataBindings/0/query/page maxSize"],
        "/payload/ux",
        "missing field",
    ),
    (
        &[r#"set /dataBindings/1/query linkId = "unknown_link""#],
        "/dataBindings/1/query/linkId",
        "not declared",
    ),
    (
        &[r#"set /dataBindings/1 object = {"id":"commerce.Order.entity","version":"0.1.0"}"#],
        "/dataBindings/1/object",
        "target",
    ),
    (
        &[r#"set /fields/0 control = "number""#],
        "/fields/0/control",
        "incompatible",
    ),
    (
        &[r#"set /navigationItems/0/destination pageId = "unknown-page""#],
        "/navigationItems/0/destination/pageId",
        "does not exist",
    ),
    (
        &[r#"set /navigationItems/0 requiredCapability = " ""#],
        "/navigationItems/0/requiredCapability",
        "must not be blank",
    ),
    (
        &[r#"set /app navigationItemIds = ["customer-nav"]"#],
        "/navigationItems/0/destination/parameters/customerNumber",
        "not available in this list row or detail context",
    ),
    (
        &[
            r#"set /forms/0/submit/confirmation severity = "irreversible""#,
            r#"set /forms/0/submit/confirmation strength = "none""#,
        ],
        "/forms/0/submit/confirmation/strength",
        "irreversible actions need typed confirmation",
    ),
    (
        &[
            r#"set /forms/0/submit/confirmation severity = "irreversible""#,
            r#"set /forms/0/submit/confirmation strength = "typed""#,
            "del /forms/0/submit/confirmation phrase",
        ],
        "/forms/0/submit/confirmation/phrase",
        "typed confirmation requires nonblank localized confirmation text",
    ),
    (
        &[
            r#"set /pages/0/root/children/0/component selection = "multiple""#,
            "del /actionPlacements/0 formId",
        ],
        "/forms/0/dataBindingId",
        "prefill requires a declared record or list-row source",
    ),
    (
        &[
            r#"set /dataBindings/0/query filters = [{"property":{"bindingId":"orders","propertyId":"status"},"operator":"eq","value":{"kind":"record","property":{"bindingId":"orders","propertyId":"$key"}}}]"#,
        ],
        "/dataBindings/0/query/filters/0/value",
        "runtime keys cannot supply query or filter values",
    ),
    (
        &[
            r#"set /dataBindings/2/query/key/0 value = {"kind":"record","property":{"bindingId":"orders","propertyId":"$key"}}"#,
        ],
        "/dataBindings/2/query/key/0/value",
        "runtime keys cannot supply query or filter values",
    ),
];

#[test]
fn base_widget_ux_refusal_matrices() {
    assert_clean(&widget_findings(widget_with(|ux| {
        set_key(
            ux,
            "/actionBindings/0/inputMapping",
            "comment",
            json!("null"),
        );
    })));
    assert_edit_matrix(BASE_MAPPING_AND_CONDITION_REFUSALS);
    assert_edit_matrix(BASE_BOUNDS_NAV_AND_TREE_REFUSALS);
}

#[test]
fn footer_and_toolbar_placement_invariants() {
    let mut toolbar_sel = widget_with_toolbar_action();
    set_key(
        ux_mut(&mut toolbar_sel),
        "/actionBindings/1/inputs",
        "selectionBindingId",
        json!("orders"),
    );
    assert_issue(
        &widget_findings(toolbar_sel),
        "/actionBindings/1",
        "selection",
    );

    let mut repeat_submit = widget_with_secondary_footer_action();
    set_key(
        ux_mut(&mut repeat_submit),
        "/actionPlacements/1",
        "bindingId",
        json!("approve"),
    );
    assert_issue(
        &widget_findings(repeat_submit),
        "/pages/0/root/children/2/children/2/component/placementId",
        "own submit action as a secondary footer action",
    );

    let mut footer_opens = widget_with_secondary_footer_action();
    set_key(
        ux_mut(&mut footer_opens),
        "/actionPlacements/1",
        "formId",
        json!("approve-form"),
    );
    assert_issue(
        &widget_findings(footer_opens),
        "/actionPlacements/1/formId",
        "must use the form's submit action binding",
    );
}

// ============================================================================
// Record query cycles, nullable filters, sort/filter indexes, integer proofs
// ============================================================================

fn add_customer_detail_from_record(value: &mut Value, binding_id: &str, source_id: &str) {
    let ux = ux_mut(value);
    let mut binding = ptr_mut(ux, "/dataBindings/2").clone();
    binding.set("bindingId", json!(binding_id));
    set_key(
        &mut binding,
        "/query/key/0",
        "value",
        json!({ "kind": "record", "property": { "bindingId": source_id, "propertyId": "customer_number" } }),
    );
    push_ptr(ux, "/dataBindings", binding);
    push_ptr(
        ux,
        "/pages/1/root/children",
        json!({ "nodeId": format!("{binding_id}-node"), "component": { "kind": "detail", "dataBindingId": binding_id }, "children": [] }),
    );
}

#[test]
fn record_query_dependency_cycles_and_acyclicity() {
    let mut acyclic = widget();
    add_customer_detail_from_record(&mut acyclic, "related-customer", "customer-detail");
    add_customer_detail_from_record(&mut acyclic, "related-customer-copy", "related-customer");
    assert_clean(&widget_findings(acyclic));

    let cases: &[(fn(&mut Value), &str, &[&str], &[&str])] = &[
        (
            |v| {
                apply_edits(
                    ux_mut(v),
                    &[
                        r#"set /dataBindings/2/query/key/0 value = {"kind":"record","property":{"bindingId":"customer-detail","propertyId":"customer_number"}}"#,
                    ],
                );
                add_customer_detail_from_record(v, "downstream-customer", "customer-detail");
            },
            "/dataBindings/2/query/key/0/value/property/bindingId",
            &["customer-detail"],
            &["downstream-customer"],
        ),
        (
            |v| {
                add_customer_detail_from_record(v, "related-customer", "customer-detail");
                apply_edits(
                    ux_mut(v),
                    &[
                        r#"set /dataBindings/2/query/key/0 value = {"kind":"record","property":{"bindingId":"related-customer","propertyId":"customer_number"}}"#,
                    ],
                );
            },
            "/dataBindings/2/query/key/0/value/property/bindingId",
            &["customer-detail", "related-customer"],
            &[],
        ),
        (
            |v| {
                apply_edits(
                    ux_mut(v),
                    &[
                        r#"set /dataBindings/0/query/filters/0 value = {"kind":"record","property":{"bindingId":"orders","propertyId":"status"}}"#,
                    ],
                )
            },
            "/dataBindings/0/query/filters/0/value/property/bindingId",
            &["orders"],
            &[],
        ),
        (
            |v| {
                apply_edits(
                    ux_mut(v),
                    &[
                        r#"set /pages/0/root/children/1/component selection = "single""#,
                        r#"set /dataBindings/0/query/filters/0 value = {"kind":"record","property":{"bindingId":"order-customers","propertyId":"display_name"}}"#,
                    ],
                )
            },
            "/dataBindings/0/query/filters/0/value/property/bindingId",
            &["orders", "order-customers"],
            &[],
        ),
    ];
    for &(mutate, pointer, must_mention, must_not_mention) in cases {
        let mut value = widget();
        mutate(&mut value);
        let found = widget_findings(value);
        assert_issue(&found, pointer, "query record dependency cycle");
        assert_eq!(
            found.len(),
            1,
            "the cycle is the only invalid contract: {found:#?}"
        );
        let msg = &found.first().req("cycle finding").message;
        for m in must_mention {
            assert!(msg.contains(m), "expected {m} in {msg:?}");
        }
        for ex in must_not_mention {
            assert!(!msg.contains(ex), "unexpected {ex} in {msg:?}");
        }
    }
}

#[test]
fn nullable_filter_literals_require_an_operand_for_every_ordered_operator() {
    for (type_raw, nonnull) in [
        (r#"{"kind":"text"}"#, json!("note")),
        (r#"{"kind":"integer"}"#, json!(42)),
        (r#"{"kind":"date"}"#, json!("2026-10-07")),
        (
            r#"{"kind":"timestamp","timezone":"utc"}"#,
            json!("2026-10-07T00:00:00Z"),
        ),
    ] {
        let mut support = support_artifacts();
        set_key(
            support.first_mut().req("order"),
            "/payload/objectType/properties/6",
            "type",
            j(type_raw),
        );
        let mut value = widget_with(|ux| {
            set_key(
                ux,
                "/dataBindings/0/query/filters/0/property",
                "propertyId",
                json!("optional_note"),
            )
        });
        let fptr = "/dataBindings/0/query/filters/0";
        for op in ["lt", "lte", "gt", "gte"] {
            set_key(ux_mut(&mut value), fptr, "operator", json!(op));
            set_key(
                ux_mut(&mut value),
                fptr,
                "value",
                j(r#"{"kind":"literal","value":null}"#),
            );
            let found = findings_with_support(value.clone(), support.clone(), &[]);
            assert_issue(
                &found,
                "/dataBindings/0/query/filters/0/value",
                "cannot be null",
            );
            assert_eq!(found.len(), 1);
            set_key(
                ux_mut(&mut value),
                fptr,
                "value",
                json!({ "kind": "literal", "value": nonnull }),
            );
            assert_clean(&findings_with_support(value.clone(), support.clone(), &[]));
        }
        for op in ["eq", "neq", "in"] {
            let operand = if op == "in" {
                json!([null, nonnull])
            } else {
                Value::Null
            };
            set_key(ux_mut(&mut value), fptr, "operator", json!(op));
            set_key(
                ux_mut(&mut value),
                fptr,
                "value",
                json!({ "kind": "literal", "value": operand }),
            );
            assert_clean(&findings_with_support(value.clone(), support.clone(), &[]));
        }
        if nonnull == json!(42) {
            set_key(ux_mut(&mut value), fptr, "operator", json!("gt"));
            set_key(
                ux_mut(&mut value),
                fptr,
                "value",
                j(r#"{"kind":"literal","value":1.5}"#),
            );
            assert_issue(
                &findings_with_support(value, support, &[]),
                "/dataBindings/0/query/filters/0/value",
                "integer",
            );
        }
    }
}

#[test]
fn list_sort_binding_index_and_hierarchy_invariants() {
    let multiple = widget_with(|ux| {
        apply_edits(
            ux,
            &[
                r#"push /dataBindings/0/query/sort = {"property":{"bindingId":"orders","propertyId":"revision"},"direction":"asc"}"#,
            ],
        )
    });
    assert_issue(
        &widget_findings(multiple),
        "/dataBindings/0/query/sort",
        "at most one sort key",
    );

    let mut computed = support_artifacts();
    set_key(
        computed.first_mut().req("order"),
        "/payload/objectType/properties/1",
        "computed",
        json!("self.order_id"),
    );
    assert_issue(
        &findings_with_support(widget(), computed, &[]),
        "/dataBindings/0/query/sort/0/property",
        "computed sort properties have no stored column",
    );

    let mut ts = support_artifacts();
    set_key(
        ts.first_mut().req("order"),
        "/payload/objectType/properties/1",
        "type",
        j(r#"{"kind":"timestamp","timezone":"utc"}"#),
    );
    assert_issue(
        &findings_with_support(widget(), ts, &[]),
        "/dataBindings/0/query/sort/0/property",
        "no exact binding-index order",
    );

    let overflow = widget_with(|ux| {
        set_key(
            ux,
            "/dataBindings/0/query/sort/0/property",
            "propertyId",
            json!("overflow_sort"),
        )
    });
    assert_issue(
        &findings_with_support(overflow, overflow_order_support("overflow_sort"), &[]),
        "/dataBindings/0/query/sort/0/property",
        "overflow payload",
    );

    let root_support = with_base_order(&j(
        r#"[{"id":"base_id","type":{"kind":"text"}},{"id":"base_number","type":{"kind":"text"}}]"#,
    ));
    let root_sort = widget_with(|ux| {
        set_key(
            ux,
            "/dataBindings/0/query/sort/0/property",
            "propertyId",
            json!("base_number"),
        )
    });
    assert_clean(&findings_with_support(root_sort, root_support, &[]));
    assert_issue(
        &findings_with_support(
            widget(),
            with_base_order(&j(r#"[{"id":"base_id","type":{"kind":"text"}}]"#)),
            &[],
        ),
        "/dataBindings/0/query/sort/0/property",
        "declared below the hierarchy root",
    );
}

#[test]
fn filters_require_direct_stored_columns_for_binding_indexes() {
    let filter_on = |prop: &str, val: Value| {
        widget_with(|ux| {
            set_key(ux, "/fields/0", "control", json!("display"));
            set_key(ux, "/dataBindings/0/query", "sort", json!([]));
            set_key(
                ux,
                "/dataBindings/0/query/filters/0",
                "property",
                json!({ "bindingId": "orders", "propertyId": prop }),
            );
            set_key(
                ux,
                "/dataBindings/0/query/filters/0",
                "value",
                json!({ "kind": "literal", "value": val }),
            );
        })
    };
    let mut computed = support_artifacts();
    set_key(
        computed.first_mut().req("order"),
        "/payload/objectType/properties/1",
        "computed",
        json!("self.order_id"),
    );
    assert_issue(
        &findings_with_support(filter_on("order_number", json!("ORD-100")), computed, &[]),
        "/dataBindings/0/query/filters/0/property",
        "computed filter properties have no stored column",
    );
    for (type_raw, val) in [
        (
            r#"{"kind":"timestamp","timezone":"utc"}"#,
            json!("2026-10-07T00:00:00Z"),
        ),
        (r#"{"kind":"decimal","scale":2}"#, Value::Null),
    ] {
        let mut s = support_artifacts();
        set_key(
            s.first_mut().req("order"),
            "/payload/objectType/properties/1",
            "type",
            j(type_raw),
        );
        set_key(
            s.first_mut().req("order"),
            "/payload/objectType/properties/1",
            "constraints",
            j(r#"{"nullable":true}"#),
        );
        assert_clean(&findings_with_support(
            filter_on("order_number", val),
            s,
            &[],
        ));
    }
    assert_issue(
        &findings_with_support(
            filter_on("overflow_filter", json!("x")),
            overflow_order_support("overflow_filter"),
            &[],
        ),
        "/dataBindings/0/query/filters/0/property",
        "overflow payload",
    );
}

const INTEGER_PROOF_CASES: &[(&str, Option<(&str, &str)>, Option<&str>)] = &[
    ("42", None, None),
    ("input.record.revision", None, None),
    ("1.5", None, Some("integer")),
    ("input.record.revision + 1", None, Some("integer")),
    ("3 / 2", None, Some("integer")),
    (
        "{source: 'ui', count: 42}",
        Some(("count", r#"{"type":"integer"}"#)),
        None,
    ),
    (
        "({source: 'ui', count: (input.record.revision)})",
        Some(("count", r#"{"type":"integer"}"#)),
        None,
    ),
    (
        "{source: 'ui', count: 1.5}",
        Some(("count", r#"{"type":"integer"}"#)),
        Some("integer"),
    ),
    (
        r#"{source: "ui", count: 1.5}"#,
        Some(("count", r#"{"type":"integer"}"#)),
        Some("integer"),
    ),
    (
        "{source: 'ui', count: 1 + 1}",
        Some(("count", r#"{"type":"integer"}"#)),
        Some("integer"),
    ),
    (
        "{source: 'ui', count: input.record.revision + 1}",
        Some(("count", r#"{"type":"integer"}"#)),
        Some("integer"),
    ),
    (
        "{source: 'ui', samples: [(1), input.record.revision, 2]}",
        Some(("samples", r#"{"type":"array","items":{"type":"integer"}}"#)),
        None,
    ),
    (
        "{source: 'ui', samples: [42.0]}",
        Some(("samples", r#"{"type":"array","items":{"type":"integer"}}"#)),
        None,
    ),
    (
        "{source: 'ui', samples: [1, 1.5]}",
        Some(("samples", r#"{"type":"array","items":{"type":"integer"}}"#)),
        Some("integer"),
    ),
    (
        "{source: 'ui', samples: [1, input.record.revision + 1]}",
        Some(("samples", r#"{"type":"array","items":{"type":"integer"}}"#)),
        Some("integer"),
    ),
    (
        "{source: 'ui', samples: [1, null, input.record.revision]}",
        Some((
            "samples",
            r#"{"type":"array","items":{"type":["integer","null"]}}"#,
        )),
        Some("may be null"),
    ),
    (
        "{source: 'ui', rows: [{count: 1}, {count: null}]}",
        Some((
            "rows",
            r#"{"type":"array","items":{"type":"object","properties":{"count":{"type":["integer","null"]}},"required":["count"],"additionalProperties":false}}"#,
        )),
        None,
    ),
    (
        "{source: 'ui', rows: [{count: 1}, {count: null}, {count: 1.5}]}",
        Some((
            "rows",
            r#"{"type":"array","items":{"type":"object","properties":{"count":{"type":["integer","null"]}},"required":["count"],"additionalProperties":false}}"#,
        )),
        Some("integer"),
    ),
    (
        "{source: 'ui', rows: [{counts: [1, input.record.revision]}, {counts: [(2), 3]}]}",
        Some((
            "rows",
            r#"{"type":"array","items":{"type":"object","properties":{"counts":{"type":"array","items":{"type":"integer"}}},"required":["counts"],"additionalProperties":false}}"#,
        )),
        None,
    ),
    (
        "{source: 'ui', rows: [{counts: [1]}, {counts: [2, 1.5]}]}",
        Some((
            "rows",
            r#"{"type":"array","items":{"type":"object","properties":{"counts":{"type":"array","items":{"type":"integer"}}},"required":["counts"],"additionalProperties":false}}"#,
        )),
        Some("integer"),
    ),
    (
        "{source: 'ui', rows: [{counts: [1]}, {counts: [input.record.revision + 1]}]}",
        Some((
            "rows",
            r#"{"type":"array","items":{"type":"object","properties":{"counts":{"type":"array","items":{"type":"integer"}}},"required":["counts"],"additionalProperties":false}}"#,
        )),
        Some("integer"),
    ),
    (
        "{source: 'ui', matrix: [[1, 2], [input.record.revision, 3]]}",
        Some((
            "matrix",
            r#"{"type":"array","items":{"type":"array","items":{"type":"integer"}}}"#,
        )),
        None,
    ),
    (
        "{source: 'ui', matrix: [[1], [2, 1.5]]}",
        Some((
            "matrix",
            r#"{"type":"array","items":{"type":"array","items":{"type":"integer"}}}"#,
        )),
        Some("integer"),
    ),
];

/// Integer action inputs admit only a declared integer source or an integral literal, and every
/// constructed object, array, row, and matrix member keeps that proof after type joining.
#[test]
fn integer_action_inputs_require_an_integral_proof() {
    for &(expression, metadata, expected) in INTEGER_PROOF_CASES {
        let (found, pointer) = match metadata {
            None => (
                widget_findings(widget_with(|ux| {
                    set_key(
                        ux,
                        "/actionBindings/0/inputMapping",
                        "expectedVersion",
                        json!(expression),
                    )
                })),
                "/actionBindings/0/inputMapping/expectedVersion",
            ),
            Some((member, schema_raw)) => {
                let value = widget_with(|ux| {
                    set_key(
                        ux,
                        "/actionBindings/0/inputMapping",
                        "metadata",
                        json!(expression),
                    )
                });
                let mut support = support_artifacts();
                set_key(
                    support.get_mut(2).req("action"),
                    "/payload/inputSchema/properties/metadata/properties",
                    member,
                    j(schema_raw),
                );
                (
                    findings_with_support(value, support, &[]),
                    "/actionBindings/0/inputMapping/metadata",
                )
            }
        };
        match expected {
            None => assert_clean(&found),
            Some(message) => assert_issue(&found, pointer, message),
        }
    }
}

// ============================================================================
// Form inputs, edit version conflicts, array inputs, dependencies & scope
// ============================================================================

#[test]
fn optional_and_display_form_input_contracts() {
    assert_clean(&widget_findings(widget_with(|ux| {
        add_approve_form_field(
            ux,
            "note",
            "approve-note",
            "text",
            Some("input.draft.note"),
            None,
        )
    })));
    assert_issue(
        &widget_findings(widget_with(|ux| {
            add_approve_form_field(ux, "comment", "approve-comment", "text_area", None, None)
        })),
        "/forms/0/inputNames/2",
        "no action input mapping",
    );
    assert_clean(&widget_findings(widget_with(|ux| {
        add_approve_form_field(
            ux,
            "comment",
            "approve-comment-display",
            "display",
            Some("input.draft.comment"),
            None,
        )
    })));
    assert_edit_matrix(&[
        (
            &[r#"set /fields/5 control = "display""#],
            "/fields/5/target/initial",
            "required display form input",
        ),
        (
            &["del /fields/4/target initial"],
            "/fields/4/target/initial",
            "required display form input",
        ),
        (
            &[r#"set /fields/4/target/initial propertyId = "revision""#],
            "/fields/4/target/initial",
            "type does not fit",
        ),
        (
            &[
                r#"set /forms/0 dataBindingId = "order-customers""#,
                r#"set /fields/4/target initial = {"bindingId":"order-customers","propertyId":"display_name"}"#,
            ],
            "/fields/4/target/initial",
            "unavailable from the form's record source",
        ),
    ]);
}

#[test]
fn a_nullable_form_input_must_stay_clearable() {
    let clear_pointers = |required_when: Option<Value>| -> Vec<String> {
        findings(widget_with(|ux| {
            add_approve_form_field(
                ux,
                "comment",
                "approve-comment",
                "text_area",
                Some("input.draft.comment"),
                required_when,
            )
        }))
        .into_iter()
        .filter(|f| f.code.as_str() == "widget.interaction.nullable_without_clear")
        .map(|f| f.span.map_or_default(|span| span.pointer))
        .collect()
    };
    assert_eq!(clear_pointers(None).len(), 0);
    let found = clear_pointers(Some(j(r#"{"expression":"true","inputs":{}}"#)));
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        found[0].starts_with("/payload/ux/pages/0/root/children/2/children/"),
        "{found:?}"
    );
}

#[test]
fn optional_array_action_inputs_accept_an_empty_array_mapping() {
    let mut array_action = action_artifact();
    apply_edits(
        &mut array_action,
        &[
            r#"set . artifactId = "commerce.order.tag.action""#,
            r#"set . name = "Tag orders""#,
            r#"set /payload actionId = "commerce.order.tag""#,
            r#"set /payload inputSchema = {"type":"object","properties":{"tags":{"type":"array","items":{"type":"string"}}},"required":[],"additionalProperties":false}"#,
        ],
    );
    let mut value = widget();
    push_dep(&mut value, "commerce.order.tag.action", "action");
    apply_edits(
        ux_mut(&mut value),
        &[
            r#"push /actionBindings = {"bindingId":"tag-orders","action":{"id":"commerce.order.tag.action","version":"0.1.0"},"inputMapping":{"tags":"[]"},"inputs":{},"requiredCapability":"commerce.order.tag"}"#,
        ],
    );
    let mut support = support_artifacts();
    support.push(array_action);
    assert_contract_clean(
        &findings_with_support(value, support, &[]),
        &["widget.interaction.control_missing_for_transition"],
    );
}

#[test]
fn edit_forms_require_a_matching_version_input_mapping() {
    let mut valid = widget();
    push_dep(&mut valid, "commerce.order.edit.action", "action");
    let ux = ux_mut(&mut valid);
    apply_edits(
        ux,
        &[
            r#"push /dataBindings = {"bindingId":"order-detail","object":{"id":"commerce.Order.entity","version":"0.1.0"},"query":{"kind":"object_detail","key":[{"propertyId":"order_id","value":{"kind":"literal","value":"ORD-100"}}]}}"#,
            r#"set /actionBindings/0 action = {"id":"commerce.order.edit.action","version":"0.1.0"}"#,
            r#"set /actionBindings/0/inputs recordBindingId = "order-detail""#,
            r#"set /actionBindings/0/inputMapping expectedVersion = "input.record.revision""#,
            r#"set /forms/0 mode = "edit""#,
            r#"set /forms/0 dataBindingId = "order-detail""#,
            r#"set /forms/0 versionConflictBinding = {"property":{"bindingId":"order-detail","propertyId":"revision"},"inputName":"expectedVersion","onConflict":"reject_and_retain_draft"}"#,
            r#"set /actionPlacements/0/enabledWhen/inputs recordBindingId = "order-detail""#,
            r#"set /actionPlacements/0 location = "detail""#,
            "del /actionPlacements/0 selectionBindingId",
            r#"set /fields/4/target initial = {"bindingId":"order-detail","propertyId":"order_id"}"#,
        ],
    );
    let action_node = arr_mut(ux, "/pages/0/root/children/0/children").remove(1);
    push_ptr(
        ux,
        "/pages/0/root/children",
        json!({
            "nodeId": "order-detail-edit-surface-node", "semanticId": "order-detail-edit-surface",
            "component": { "kind": "detail", "dataBindingId": "order-detail" }, "children": [action_node]
        }),
    );
    let mut support = support_artifacts();
    support.push(required_version_action_artifact());
    assert_clean(&findings_with_support(valid.clone(), support.clone(), &[]));
    apply_edits(
        ux_mut(&mut valid),
        &[
            r#"set /actionBindings/0/inputs recordBindingId = "orders""#,
            r#"set /actionBindings/0/inputMapping expectedVersion = "input.record.revision""#,
        ],
    );
    assert_issue(
        &findings_with_support(valid, support, &[]),
        "/forms/0/versionConflictBinding/inputName",
        "matching edited record",
    );
}

#[test]
fn references_require_exact_envelope_dependencies_and_compatible_scope() {
    let mut missing_dep = widget();
    missing_dep.set(
        "dependencies",
        json!([
            dep_entry("commerce.Order.entity", "entity"),
            dep_entry("commerce.order.approve.action", "action")
        ]),
    );
    assert_issue(
        &widget_findings(missing_dep),
        "/dataBindings/1/object",
        "dependency",
    );

    let mut customer = customer_entity();
    set_key(&mut customer, "/scope", "orgId", json!("org_elsewhere"));
    let found = findings_with_support(
        widget(),
        vec![order_entity(), customer, action_artifact()],
        &[],
    );
    assert!(
        found
            .iter()
            .any(|f| f.artifact_id.as_deref() == Some(WIDGET_ID)
                && f.code.as_str() == "artifact_set.scope.incompatible_reference"),
        "expected scope incompatibility on the widget reference; got {found:#?}"
    );
}

// ============================================================================
// Composition, baseline overrides, and enum selects
// ============================================================================

fn baseline_widget(semantic_id: &str) -> Value {
    let mut base = j(r#"{
        "irVersion":"0.1","artifactId":"commerce.widget.baseline","kind":"widget",
        "namespace":"commerce","name":"Baseline widget","version":"0.1.0","lifecycle":"draft",
        "scope":{"orgId":"org_demo","environmentId":"dev"},
        "dependencies":[{"artifactId":"crm.Customer.entity","kind":"entity","version":"0.1.0","required":true}],
        "provenance":{"source":"authoring_sdk"},
        "payload":{
            "widgetVersion":"0.1","widgetId":"commerce.widget.baseline",
            "dataDependencies":[],"viewStates":[{"stateId":"ready","kind":"ready"}],
            "actionBindings":[],"permissions":[],
            "ux":{
                "uxVersion":"0.1",
                "pages":[{"pageId":"baseline-page","name":"Baseline","contextParams":[],"root":{"nodeId":"baseline-root","component":{"kind":"field","fieldId":"baseline-revision-field"},"children":[]}}],
                "dataBindings":[{"bindingId":"orders","object":{"id":"crm.Customer.entity","version":"0.1.0"},"query":{"kind":"object_detail","key":[{"propertyId":"customer_number","value":{"kind":"literal","value":"C-001"}}]}}],
                "actionBindings":[],
                "fields":[{"fieldId":"baseline-revision-field","target":{"kind":"property","property":{"bindingId":"orders","propertyId":"revision"}},"label":{"en":"Revision"},"control":"text"}],
                "forms":[],"actionPlacements":[],"navigationItems":[],"overrides":[]
            }
        }
    }"#);
    set_key(
        ux_mut(&mut base),
        "/pages/0/root",
        "semanticId",
        json!(semantic_id),
    );
    base
}

fn required_form_input_baseline() -> Value {
    let mut baseline = baseline_widget("baseline-form-surface");
    baseline.set(
        "dependencies",
        json!([dep_entry("commerce.order.approve.action", "action")]),
    );
    apply_edits(
        ux_mut(&mut baseline),
        &[
            "set . dataBindings = []",
            r#"set . actionBindings = [{"bindingId":"baseline-submit","action":{"id":"commerce.order.approve.action","version":"0.1.0"},"inputMapping":{"orderId":"\"ORD-100\"","reason":"input.draft.reason"},"inputs":{"draftFormId":"baseline-form"},"requiredCapability":"commerce.order.approve"}]"#,
            r#"set . forms = [{"formId":"baseline-form","mode":"action_input","inputNames":["reason"],"submitBindingId":"baseline-submit","submit":{"label":{"en":"Approve"},"confirmation":{"severity":"safe","strength":"none"}}}]"#,
            r#"set . fields = [{"fieldId":"baseline-required-reason","target":{"kind":"form_input","formId":"baseline-form","inputName":"reason"},"label":{"en":"Reason"},"control":"text_area"}]"#,
            r#"set /pages/0/root component = {"kind":"form","formId":"baseline-form"}"#,
            r#"set /pages/0/root children = [{"nodeId":"baseline-required-reason-node","semanticId":"baseline-required-alias","component":{"kind":"field","fieldId":"baseline-required-reason"},"children":[]}]"#,
        ],
    );
    baseline
}

fn compose_baseline(mut value: Value) -> Value {
    set_key(ux_mut(&mut value), "", "overrides", json!([]));
    push_dep(&mut value, "commerce.widget.baseline", "widget");
    apply_edits(
        ux_mut(&mut value),
        &[
            r#"push /pages/0/root/children = {"nodeId":"baseline-composition-node","semanticId":"baseline-composition","component":{"kind":"composition","widget":{"id":"commerce.widget.baseline","version":"0.1.0"},"pageId":"baseline-page"},"children":[]}"#,
        ],
    );
    value
}

fn enum_select_baseline(control: &str, option_key_type: Option<&str>) -> (Value, Vec<Value>) {
    let mut baseline = baseline_widget("baseline-revision");
    let mut support = support_artifacts();
    set_key(
        support.get_mut(1).req("customer"),
        "/payload/objectType/properties/2",
        "type",
        j(r#"{"kind":"enum","values":[{"value":"draft"},{"value":"approved"}]}"#),
    );
    set_key(
        ux_mut(&mut baseline),
        "/fields/0",
        "control",
        json!(control),
    );
    if let Some(key_raw) = option_key_type {
        support.push(entity(
            "commerce.StatusOption.entity",
            "commerce.StatusOption",
            &json!([{ "id": "status_id", "type": j(key_raw) }]),
            "status_id",
            &json!([]),
        ));
        push_dep(&mut baseline, "commerce.StatusOption.entity", "entity");
        apply_edits(
            ux_mut(&mut baseline),
            &[
                r#"push /dataBindings = {"bindingId":"status-options","object":{"id":"commerce.StatusOption.entity","version":"0.1.0"},"query":{"kind":"object_list","filters":[],"sort":[],"page":{"size":10,"maxSize":10}}}"#,
                r#"set /fields/0 optionQueryBindingId = "status-options""#,
            ],
        );
    }
    (baseline, support)
}

fn baseline_list_host(selection: &str, override_cond: &Value) -> (Value, Value) {
    let mut baseline = baseline_widget("baseline-list-semantic");
    baseline.set(
        "dependencies",
        json!([dep_entry("commerce.Order.entity", "entity")]),
    );
    *ptr_mut(ux_mut(&mut baseline), "/dataBindings/0") = widget()
        .pointer("/payload/ux/dataBindings/0")
        .req("orders binding")
        .clone();
    set_key(
        ux_mut(&mut baseline),
        "/fields/0/target/property",
        "propertyId",
        json!("order_number"),
    );
    set_key(
        ux_mut(&mut baseline),
        "/pages/0/root",
        "component",
        json!({ "kind": "list", "dataBindingId": "orders", "selection": selection, "fields": [{ "bindingId": "orders", "propertyId": "order_number" }] }),
    );
    let mut host = baseline.clone();
    host.set("artifactId", json!(WIDGET_ID));
    set_key(&mut host, "/payload", "widgetId", json!(WIDGET_ID));
    push_dep(&mut host, "commerce.widget.baseline", "widget");
    apply_edits(
        ux_mut(&mut host),
        &[
            "set . fields = []",
            r#"set /pages/0 pageId = "host-page""#,
            r#"set /pages/0 root = {"nodeId":"host-composition","semanticId":"host-composition","component":{"kind":"composition","widget":{"id":"commerce.widget.baseline","version":"0.1.0"},"pageId":"baseline-page"},"children":[]}"#,
        ],
    );
    set_key(
        ux_mut(&mut host),
        "",
        "overrides",
        json!([{ "targetSemanticId": "baseline-revision-field", "value": { "kind": "visibility", "condition": override_cond } }]),
    );
    (baseline, host)
}

/// #1376: a composed page is held to the declarations of the catalog entries it places, though
/// they live outside the page.
#[test]
fn a_composed_page_is_held_to_its_placed_catalog_declarations() {
    for (catalog_raw, refused) in [
        (
            r#"[{"entryId":"tile","label":{"en":"Tile"},"trust":"native"}]"#,
            false,
        ),
        (
            r#"[{"entryId":"tile","label":{"en":"Tile"},"trust":"native"},{"entryId":"tile","label":{"en":"Tile"},"trust":"native"}]"#,
            true,
        ),
        (
            r#"[{"entryId":"tile","label":{"en":"Tile"},"trust":"native","renders":"number"},{"entryId":"money","label":{"en":"Money"},"trust":"native","renders":"number"}]"#,
            true,
        ),
        (
            r#"[{"entryId":"tile","label":{"en":"Tile"},"trust":"native","machine":{"machineVersion":"0.1","initialStateId":"missing","fields":[],"events":[],"states":[{"stateId":"idle","role":"idle"}],"transitions":[]}}]"#,
            true,
        ),
    ] {
        let mut baseline = baseline_widget("baseline-revision");
        let ux = ux_mut(&mut baseline);
        ux.set("catalog", j(catalog_raw));
        let field = ptr_mut(ux, "/pages/0/root").clone();
        set_key(
            ux,
            "/pages/0",
            "root",
            json!({
                "nodeId": "baseline-section", "semanticId": "baseline-section",
                "component": { "kind": "section", "section": { "sectionId": "baseline", "kind": "section", "label": { "en": "Baseline" }, "fields": [], "order": 1 } },
                "children": [field, { "nodeId": "baseline-tile", "semanticId": "baseline-tile", "component": { "kind": "catalog", "entry": { "entryId": "tile" } }, "children": [] }]
            }),
        );
        let found = widget_findings_with_base(compose_baseline(widget()), &[baseline]);
        assert_eq!(
            found
                .iter()
                .any(|f| f.message.contains("fails typed UX validation")),
            refused,
            "{found:#?}"
        );
    }
}

#[test]
fn enum_selects_and_imported_override_contexts() {
    let mut bad_override = compose_baseline(widget());
    apply_edits(
        ux_mut(&mut bad_override),
        &[
            r#"push /overrides = {"targetSemanticId":"baseline-revision","value":{"kind":"control","control":"number"}}"#,
        ],
    );
    assert_issue(
        &findings_with_base(bad_override, &[baseline_widget("baseline-revision")]),
        "/overrides/0/control",
        "incompatible",
    );

    let mut select_base = baseline_widget("baseline-revision");
    apply_edits(
        ux_mut(&mut select_base),
        &[
            r#"push /dataBindings = {"bindingId":"customer-options","object":{"id":"crm.Customer.entity","version":"0.1.0"},"query":{"kind":"object_list","filters":[],"sort":[],"page":{"size":10,"maxSize":10}}}"#,
            r#"set /fields/0 control = "select""#,
            r#"set /fields/0 optionQueryBindingId = "customer-options""#,
        ],
    );
    let mut select_host = compose_baseline(widget());
    apply_edits(
        ux_mut(&mut select_host),
        &[
            r#"set . overrides = [{"targetSemanticId":"baseline-revision","value":{"kind":"control","control":"select"}}]"#,
        ],
    );
    assert_clean(&findings_with_base(select_host, &[select_base]));

    let matching = r#"{"kind":"enum","values":[{"value":"draft"},{"value":"approved"}]}"#;
    for (option_key, direct_ok, override_check) in [
        (None, true, Some(true)),
        (Some(matching), true, Some(true)),
        (Some(r#"{"kind":"integer"}"#), false, Some(false)),
        (
            Some(r#"{"kind":"enum","values":[{"value":"draft"},{"value":"cancelled"}]}"#),
            false,
            None,
        ),
    ] {
        let (direct, support) = enum_select_baseline("select", option_key);
        let found = findings_with_support(direct, support, &[]);
        if direct_ok {
            assert_clean(&found);
        } else {
            assert_issue(&found, "/fields/0/control", "incompatible");
        }
        if let Some(compatible) = override_check {
            let (baseline, support) = enum_select_baseline("display", option_key);
            assert_clean(&findings_with_support(
                baseline.clone(),
                support.clone(),
                &[],
            ));
            let mut host = compose_baseline(widget());
            apply_edits(
                ux_mut(&mut host),
                &[
                    r#"set . overrides = [{"targetSemanticId":"baseline-revision-field","value":{"kind":"control","control":"select"}}]"#,
                ],
            );
            let found = findings_with_support(host, support, &[baseline]);
            if compatible {
                assert_clean(&found);
            } else {
                assert_issue(&found, "/overrides/0/control", "incompatible");
            }
        }
    }
}

#[test]
fn composed_control_overrides_keep_required_form_initialization_metadata() {
    let baseline = required_form_input_baseline();
    assert_clean(&findings(baseline.clone()));
    for target in ["baseline-required-alias", "baseline-required-reason"] {
        let mut host = compose_baseline(widget());
        set_key(
            ux_mut(&mut host),
            "",
            "overrides",
            json!([{ "targetSemanticId": target, "value": { "kind": "control", "control": "display" } }]),
        );
        assert_issue(
            &findings_with_base(host, std::slice::from_ref(&baseline)),
            "/overrides/0/value/control",
            "required display form input",
        );
    }
    let mut initialized = required_form_input_baseline();
    push_dep(&mut initialized, "crm.Customer.entity", "entity");
    let detail_binding = baseline_widget("baseline-detail")
        .pointer("/payload/ux/dataBindings/0")
        .req("detail")
        .clone();
    set_key(
        ux_mut(&mut initialized),
        "",
        "dataBindings",
        json!([detail_binding]),
    );
    apply_edits(
        ux_mut(&mut initialized),
        &[
            r#"set /forms/0 dataBindingId = "orders""#,
            r#"set /fields/0/target initial = {"bindingId":"orders","propertyId":"revision"}"#,
        ],
    );
    assert_clean(&findings(initialized.clone()));
    let mut host = compose_baseline(widget());
    apply_edits(
        ux_mut(&mut host),
        &[
            r#"set . overrides = [{"targetSemanticId":"baseline-required-alias","value":{"kind":"control","control":"display"}}]"#,
        ],
    );
    assert_clean(&findings_with_base(host, &[initialized]));
}

#[test]
fn composing_one_page_preserves_unrelated_forms_and_override_only_placements() {
    let mut multi = baseline_widget("baseline-revision");
    push_dep(&mut multi, "commerce.order.approve.action", "action");
    apply_edits(
        ux_mut(&mut multi),
        &[
            r#"set . actionBindings = [{"bindingId":"unrelated-submit","action":{"id":"commerce.order.approve.action","version":"0.1.0"},"inputMapping":{"orderId":"input.draft.orderId","reason":"input.draft.reason"},"inputs":{"draftFormId":"unrelated-form"},"requiredCapability":"commerce.order.approve"}]"#,
            r#"set . forms = [{"formId":"unrelated-form","mode":"action_input","inputNames":["orderId","reason"],"submitBindingId":"unrelated-submit","submit":{"label":{"en":"Approve"},"confirmation":{"severity":"confirm","strength":"click"}}}]"#,
            r#"push /fields = {"fieldId":"unrelated-order-id","target":{"kind":"form_input","formId":"unrelated-form","inputName":"orderId"},"label":{"en":"Order ID"},"control":"text"}"#,
            r#"push /fields = {"fieldId":"unrelated-reason","target":{"kind":"form_input","formId":"unrelated-form","inputName":"reason"},"label":{"en":"Reason"},"control":"text_area"}"#,
            r#"push /pages = {"pageId":"unrelated-page","name":"Unrelated action form","contextParams":[],"root":{"nodeId":"unrelated-root","semanticId":"unrelated-root-semantic","component":{"kind":"form","formId":"unrelated-form"},"children":[{"nodeId":"unrelated-order-id-node","component":{"kind":"field","fieldId":"unrelated-order-id"},"children":[]},{"nodeId":"unrelated-reason-node","component":{"kind":"field","fieldId":"unrelated-reason"},"children":[]}]}}"#,
        ],
    );
    assert_clean(&findings(multi.clone()));
    let host = widget_with(|ux| {
        set_key(
            ux,
            "/pages/1/root",
            "semanticId",
            json!("unrelated-root-semantic"),
        )
    });
    assert_clean(&findings_with_base(compose_baseline(host), &[multi]));

    let mut act_base = baseline_widget("baseline-action-semantic");
    push_dep(&mut act_base, "commerce.order.approve.action", "action");
    apply_edits(
        ux_mut(&mut act_base),
        &[
            "set . fields = []",
            r#"set . actionBindings = [{"bindingId":"baseline-approve","action":{"id":"commerce.order.approve.action","version":"0.1.0"},"inputMapping":{"orderId":"\"ORD-100\"","reason":"\"Approved\""},"inputs":{},"requiredCapability":"commerce.order.approve"}]"#,
            r#"set . actionPlacements = [{"placementId":"baseline-action","bindingId":"baseline-approve","label":{"en":"Approve"},"location":"toolbar","order":1,"selectionInputMapping":{},"confirmation":{"severity":"safe","strength":"none"}}]"#,
            r#"set /pages/0/root component = {"kind":"action","placementId":"baseline-action"}"#,
        ],
    );
    assert_clean(&findings(act_base.clone()));
    let mut host = widget_with_toolbar_action();
    let _ = arr_mut(ux_mut(&mut host), "/pages/0/root/children").pop();
    let mut host = compose_baseline(host);
    apply_edits(
        ux_mut(&mut host),
        &[
            r#"set . overrides = [{"targetSemanticId":"baseline-action-semantic","value":{"kind":"action_placement","placementId":"approve-secondary-footer"}}]"#,
        ],
    );
    assert_clean(&findings_with_base(host, &[act_base]));
}

#[test]
fn composed_binding_surface_isolation() {
    let (baseline, mut host) = baseline_list_host(
        "none",
        &j(
            r#"{"expression":"input.record.status == \"draft\"","inputs":{"recordBindingId":"orders"}}"#,
        ),
    );
    assert_clean(&findings(baseline.clone()));
    assert_clean(&findings_with_base(
        host.clone(),
        std::slice::from_ref(&baseline),
    ));
    let composition = ptr_mut(ux_mut(&mut host), "/pages/0/root").clone();
    set_key(
        ux_mut(&mut host),
        "/dataBindings/0/query/page",
        "maxSize",
        json!(20),
    );
    set_key(
        ux_mut(&mut host),
        "/pages/0",
        "root",
        json!({
            "nodeId": "host-root",
            "component": { "kind": "section", "section": { "sectionId": "host-section", "kind": "section", "label": { "en": "Host" }, "fields": [], "order": 1 } },
            "children": [composition, { "nodeId": "host-alias-list", "component": { "kind": "list", "dataBindingId": "orders", "fields": [], "selection": "none" }, "children": [] }]
        }),
    );
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

    let (baseline, mut host) = baseline_list_host(
        "single",
        &j(r#"{"expression":"len(input.selection) > 0","inputs":{"selectionBindingId":"orders"}}"#),
    );
    assert_clean(&findings(baseline.clone()));
    assert_clean(&findings_with_base(
        host.clone(),
        std::slice::from_ref(&baseline),
    ));
    set_key(
        ux_mut(&mut host),
        "/dataBindings/0/query/page",
        "maxSize",
        json!(20),
    );
    assert_issue(
        &findings_with_base(host, &[baseline]),
        "/overrides/0/value/condition/inputs/selectionBindingId",
        "selection",
    );
}

#[test]
fn composed_ownership_parameter_and_collision_refusals() {
    let mut form_base = baseline_widget("baseline-form-owned");
    push_dep(&mut form_base, "commerce.order.approve.action", "action");
    apply_edits(
        ux_mut(&mut form_base),
        &[
            "set . fields = []",
            "set . dataBindings = []",
            r#"set . actionBindings = [{"bindingId":"baseline-submit","action":{"id":"commerce.order.approve.action","version":"0.1.0"},"inputMapping":{"orderId":"\"ORD-100\"","reason":"\"Approved\""},"inputs":{},"requiredCapability":"commerce.order.approve"}]"#,
            r#"set . forms = [{"formId":"baseline-form","mode":"action_input","inputNames":[],"submitBindingId":"baseline-submit","submit":{"label":{"en":"Approve"},"confirmation":{"severity":"safe","strength":"none"}}}]"#,
            r#"set /pages/0/root component = {"kind":"form","formId":"baseline-form"}"#,
        ],
    );
    assert_clean(&findings(form_base.clone()));
    let mut host = compose_baseline(widget());
    apply_edits(
        ux_mut(&mut host),
        &[
            r#"set . overrides = [{"targetSemanticId":"baseline-form-owned","value":{"kind":"visibility","condition":{"expression":"len(input.selection) > 0","inputs":{"selectionBindingId":"orders"}}}}]"#,
        ],
    );
    assert_issue(
        &findings_with_base(host, &[form_base]),
        "/overrides/0/value/condition/inputs/selectionBindingId",
        "selection",
    );

    let mut sec_base = baseline_widget("baseline-order-section");
    sec_base.set(
        "dependencies",
        json!([dep_entry("commerce.Order.entity", "entity")]),
    );
    *ptr_mut(ux_mut(&mut sec_base), "/dataBindings/0") = widget()
        .pointer("/payload/ux/dataBindings/0")
        .req("orders")
        .clone();
    apply_edits(
        ux_mut(&mut sec_base),
        &[
            r#"set /fields/0/target property = {"bindingId":"orders","propertyId":"order_number"}"#,
            r#"set /pages/0/root component = {"kind":"section","section":{"sectionId":"baseline-order-section","kind":"section","label":{"en":"Orders"},"fields":[{"bindingId":"orders","propertyId":"order_number"}],"order":1}}"#,
            "set /pages/0/root children = []",
        ],
    );
    assert_issue(
        &findings(sec_base.clone()),
        "/fields/0",
        "field value requires an available",
    );
    assert_issue(
        &findings_with_base(compose_baseline(widget()), &[sec_base]),
        "/component",
        "fails typed UX validation",
    );

    let mut param_base = baseline_widget("baseline-parameter-field");
    apply_edits(
        ux_mut(&mut param_base),
        &[
            r#"set /pages/0 contextParams = [{"paramId":"customerNumber","type":"text","required":true}]"#,
        ],
    );
    assert_clean(&findings(param_base.clone()));
    assert_issue(
        &findings_with_base(compose_baseline(widget()), &[param_base]),
        "/component/pageId",
        "required context",
    );

    let found = widget_findings_with_base(
        compose_baseline(widget()),
        &[baseline_widget("orders-page")],
    );
    assert!(
        found.iter().any(|f| f.message.contains("semantic")),
        "expected imported semantic id collision; got {found:#?}"
    );
}

// ============================================================================
// Drafts, toolbar/footer placements, and multi-page / row context isolation
// ============================================================================

fn widget_with_secondary_footer_action() -> Value {
    widget_with(|ux| {
        apply_edits(
            ux,
            &[
                r#"push /actionBindings = {"bindingId":"approve-secondary","action":{"id":"commerce.order.approve.action","version":"0.1.0"},"inputMapping":{"orderId":"input.draft.orderId","reason":"input.draft.reason"},"inputs":{"draftFormId":"approve-form"},"requiredCapability":"commerce.order.approve"}"#,
                r#"push /actionPlacements = {"placementId":"approve-secondary-footer","bindingId":"approve-secondary","label":{"en":"Save draft"},"location":"form_footer","order":2,"selectionInputMapping":{},"confirmation":{"severity":"confirm","strength":"click"}}"#,
                r#"push /pages/0/root/children/2/children = {"nodeId":"approve-secondary-footer-node","semanticId":"approve-secondary-footer-action","component":{"kind":"action","placementId":"approve-secondary-footer"},"children":[]}"#,
            ],
        );
    })
}

fn widget_with_toolbar_action() -> Value {
    let mut value = widget_with_secondary_footer_action();
    let ux = ux_mut(&mut value);
    apply_edits(
        ux,
        &[
            r#"set /actionBindings/1 inputMapping = {"orderId":"\"ORD-100\"","reason":"\"Approved from toolbar\""}"#,
            "set /actionBindings/1 inputs = {}",
            r#"set /actionPlacements/1 location = "toolbar""#,
            r#"set /actionPlacements/1 confirmation = {"severity":"safe","strength":"none"}"#,
        ],
    );
    let action_node = arr_mut(ux, "/pages/0/root/children/2/children")
        .pop()
        .req("secondary action");
    push_ptr(ux, "/pages/0/root/children", action_node);
    value
}

fn widget_with_zero_input_toolbar_action() -> Value {
    let mut value = widget_with_toolbar_action();
    push_dep(&mut value, "commerce.order.noop.action", "action");
    apply_edits(
        ux_mut(&mut value),
        &[
            r#"set /actionBindings/1 action = {"id":"commerce.order.noop.action","version":"0.1.0"}"#,
            "set /actionBindings/1 inputMapping = {}",
            "set /actionBindings/1 inputs = {}",
        ],
    );
    value
}

fn widget_with_no_input_form() -> Value {
    widget_with(|ux| {
        apply_edits(
            ux,
            &[
                "set /forms/0 inputNames = []",
                r#"set /actionBindings/0 inputs = {"recordBindingId":"orders"}"#,
                r#"set /actionBindings/0 inputMapping = {"orderId":"\"ORD-100\"","reason":"\"Approved\""}"#,
                "set /pages/0/root/children/2 children = []",
            ],
        );
        arr_mut(ux, "/fields").retain(|f| f.pointer("/target/kind") != Some(&json!("form_input")));
    })
}

fn zero_input_widget_findings(value: Value) -> Vec<Diagnostic> {
    let mut support = support_artifacts();
    support.push(zero_input_action_artifact());
    findings_with_support(value, support, &[])
}

#[test]
fn zero_input_toolbar_binding_contracts() {
    assert_clean(&widget_findings(widget_with_toolbar_action()));
    assert_clean(&zero_input_widget_findings(
        widget_with_zero_input_toolbar_action(),
    ));
    for (inputs_raw, pointer, message) in [
        (
            r#"{"recordBindingId":"missing-record"}"#,
            "/actionBindings/1/inputs/recordBindingId",
            "record input",
        ),
        (
            r#"{"recordBindingId":"orders"}"#,
            "/actionBindings/1/inputs/recordBindingId",
            "record input",
        ),
        (
            r#"{"selectionBindingId":"orders"}"#,
            "/actionBindings/1/inputs/selectionBindingId",
            "selection",
        ),
        (
            r#"{"draftFormId":"approve-form"}"#,
            "/actionBindings/1/inputs/draftFormId",
            "every invocation",
        ),
        (
            r#"{"contextPageId":"customer"}"#,
            "/actionBindings/1/inputs/contextPageId",
            "context is unavailable",
        ),
    ] {
        let mut value = widget_with_zero_input_toolbar_action();
        set_key(
            ux_mut(&mut value),
            "/actionBindings/1",
            "inputs",
            j(inputs_raw),
        );
        assert_issue(&zero_input_widget_findings(value), pointer, message);
    }
}

#[test]
fn empty_form_draft_and_render_node_contracts() {
    let mut empty = widget_with_no_input_form();
    assert_clean(&widget_findings(empty.clone()));
    set_key(
        ux_mut(&mut empty),
        "/actionBindings/0/inputs",
        "draftFormId",
        json!("approve-form"),
    );
    assert_clean(&widget_findings(empty.clone()));
    let mut bad_read = empty.clone();
    set_key(
        ux_mut(&mut bad_read),
        "/actionBindings/0/inputMapping",
        "orderId",
        json!("input.draft.orderId"),
    );
    assert_issue(
        &widget_findings(bad_read),
        "/actionBindings/0/inputMapping/orderId",
        "incompatible",
    );
    set_key(
        ux_mut(&mut empty),
        "/actionBindings/0/inputMapping",
        "orderId",
        json!("input.record.order_id"),
    );
    assert_clean(&widget_findings(empty.clone()));
    set_key(
        ux_mut(&mut empty),
        "/actionBindings/0/inputs",
        "draftFormId",
        json!("other-form"),
    );
    assert_issue(
        &widget_findings(empty),
        "/forms/0/submitBindingId",
        "declare this form",
    );
    assert_issue(
        &widget_findings(widget_with(|ux| {
            del_key(ux, "/actionBindings/0/inputs", "draftFormId")
        })),
        "/forms/0/submitBindingId",
        "declare this form",
    );

    let mut unrendered = widget_with_no_input_form();
    let form_node = arr_mut(ux_mut(&mut unrendered), "/pages/0/root/children").remove(2);
    assert_issue(
        &widget_findings(unrendered.clone()),
        "/actionPlacements/0/formId",
        "render",
    );
    push_ptr(ux_mut(&mut unrendered), "/pages/1/root/children", form_node);
    assert_issue(
        &widget_findings(unrendered),
        "/actionPlacements/0/formId",
        "render",
    );
}

#[test]
fn shared_binding_and_selection_opener_isolation() {
    let shared_row = widget_with(|ux| {
        apply_edits(
            ux,
            &[
                r#"set /pages/0/root/children/0/component selection = "none""#,
                r#"set /actionBindings/0/inputMapping orderId = "input.record.order_id""#,
            ],
        );
        let mut toolbar = ptr_mut(ux, "/actionPlacements/0").clone();
        apply_edits(
            &mut toolbar,
            &[
                r#"set . placementId = "approve-toolbar""#,
                r#"set . location = "toolbar""#,
                "del . selectionBindingId",
                "del . enabledWhen",
            ],
        );
        push_ptr(ux, "/actionPlacements", toolbar);
        apply_edits(
            ux,
            &[
                r#"push /pages/0/root/children = {"nodeId":"approve-toolbar-node","semanticId":"approve-toolbar-action","component":{"kind":"action","placementId":"approve-toolbar"},"children":[]}"#,
            ],
        );
    });
    assert_issue(&widget_findings(shared_row), "/actionBindings/0", "record");

    let cond =
        j(r#"{"expression":"len(input.selection) > 0","inputs":{"selectionBindingId":"orders"}}"#);
    let mut opener = widget_with(|ux| {
        set_key(ux, "/actionPlacements/0", "location", json!("selection"));
        set_key(ux, "/actionPlacements/0", "enabledWhen", cond.clone());
    });
    assert_clean(&widget_findings(opener.clone()));
    let mut submit_sel = opener.clone();
    set_key(
        ux_mut(&mut submit_sel),
        "/actionBindings/0/inputs",
        "selectionBindingId",
        json!("orders"),
    );
    assert_issue(
        &widget_findings(submit_sel),
        "/actionBindings/0",
        "selection",
    );
    set_key(ux_mut(&mut opener), "/fields/4", "visibleWhen", cond);
    assert_issue(
        &widget_findings(opener),
        "/fields/4/visibleWhen/inputs/selectionBindingId",
        "selection",
    );
}

#[test]
fn draft_backed_bindings_cannot_escape_their_owning_form() {
    assert_clean(&widget_findings(widget_with_secondary_footer_action()));
    for (location, target_ptr) in [
        ("toolbar", "/pages/0/root/children"),
        ("row", "/pages/0/root/children/0/children"),
        ("selection", "/pages/0/root/children/0/children"),
        ("detail", "/pages/1/root/children"),
    ] {
        let mut value = widget_with_secondary_footer_action();
        let ux = ux_mut(&mut value);
        set_key(ux, "/actionPlacements/1", "location", json!(location));
        if matches!(location, "row" | "selection") {
            set_key(
                ux,
                "/actionPlacements/1",
                "selectionBindingId",
                json!("orders"),
            );
        }
        let node = arr_mut(ux, "/pages/0/root/children/2/children")
            .pop()
            .req("secondary action");
        push_ptr(ux, target_ptr, node);
        assert_issue(
            &widget_findings(value),
            "/actionBindings/1/inputs/draftFormId",
            "every invocation",
        );
    }

    let mut shared_toolbar = widget_with_secondary_footer_action();
    let mut toolbar = ptr_mut(ux_mut(&mut shared_toolbar), "/actionPlacements/1").clone();
    apply_edits(
        &mut toolbar,
        &[
            r#"set . placementId = "secondary-toolbar""#,
            r#"set . location = "toolbar""#,
        ],
    );
    push_ptr(ux_mut(&mut shared_toolbar), "/actionPlacements", toolbar);
    apply_edits(
        ux_mut(&mut shared_toolbar),
        &[
            r#"push /pages/0/root/children = {"nodeId":"secondary-toolbar-node","component":{"kind":"action","placementId":"secondary-toolbar"},"children":[]}"#,
        ],
    );
    assert_issue(
        &widget_findings(shared_toolbar),
        "/actionBindings/1/inputs/draftFormId",
        "every invocation",
    );

    let mut other_form = widget_with_secondary_footer_action();
    let ux = ux_mut(&mut other_form);
    let mut form = ptr_mut(ux, "/forms/0").clone();
    apply_edits(
        &mut form,
        &[
            r#"set . formId = "other-form""#,
            "set . inputNames = []",
            r#"set . submitBindingId = "other-submit""#,
        ],
    );
    let mut submit = ptr_mut(ux, "/actionBindings/0").clone();
    apply_edits(
        &mut submit,
        &[
            r#"set . bindingId = "other-submit""#,
            "set . inputs = {}",
            r#"set . inputMapping = {"orderId":"\"ORD-100\"","reason":"\"Approved\""}"#,
        ],
    );
    push_ptr(ux, "/forms", form);
    push_ptr(ux, "/actionBindings", submit);
    apply_edits(
        ux,
        &[
            r#"push /pages/0/root/children = {"nodeId":"other-form-node","component":{"kind":"form","formId":"other-form"},"children":[{"nodeId":"other-footer-node","component":{"kind":"action","placementId":"approve-secondary-footer"},"children":[]}]}"#,
        ],
    );
    assert_issue(
        &widget_findings(other_form),
        "/actionBindings/1/inputs/draftFormId",
        "every invocation",
    );
}

#[test]
fn draft_conditions_require_the_matching_form_at_each_render_site() {
    let cond =
        j(r#"{"expression":"input.draft.reason != \"\"","inputs":{"draftFormId":"approve-form"}}"#);
    assert_clean(&widget_findings(widget_with(|ux| {
        set_key(ux, "/fields/4", "visibleWhen", cond.clone())
    })));
    let mut footer = widget_with_secondary_footer_action();
    set_key(
        ux_mut(&mut footer),
        "/actionPlacements/1",
        "enabledWhen",
        cond.clone(),
    );
    assert_clean(&widget_findings(footer));

    for (collection, member) in [
        ("fields", "visibleWhen"),
        ("navigationItems", "visibleWhen"),
        ("actionPlacements", "enabledWhen"),
    ] {
        assert_issue(
            &widget_findings(widget_with(|ux| {
                set_key(ux, &format!("/{collection}/0"), member, cond.clone())
            })),
            &format!("/{collection}/0/{member}/inputs/draftFormId"),
            "matching form",
        );
    }
    let mut composed = compose_baseline(widget());
    set_key(
        ux_mut(&mut composed),
        "",
        "overrides",
        json!([{ "targetSemanticId": "baseline-revision-field", "value": { "kind": "visibility", "condition": cond } }]),
    );
    assert_issue(
        &findings_with_base(composed, &[baseline_widget("baseline-root")]),
        "/overrides/0/value/condition/inputs/draftFormId",
        "matching form",
    );
}

fn render_orders_list_on_customer_page(value: &mut Value) {
    apply_edits(
        ux_mut(value),
        &[
            r#"push /pages/1/root/children = {"nodeId":"customer-orders-list-node","component":{"kind":"list","dataBindingId":"orders","fields":[],"selection":"single"},"children":[]}"#,
        ],
    );
}

fn move_customer_navigation_to_page(value: &mut Value) {
    let ux = ux_mut(value);
    let navigation = arr_mut(ux, "/pages/0/root/children/0/children").remove(0);
    apply_edits(
        ux,
        &[
            r#"set /navigationItems/0 visibleWhen = {"expression":"input.record.status == \"draft\"","inputs":{"recordBindingId":"orders"}}"#,
        ],
    );
    push_ptr(ux, "/pages/0/root/children", navigation);
}

type PageConsumerCase = (
    fn() -> Value,
    fn(&mut Value),
    &'static str,
    &'static str,
    bool,
);

fn assert_page_consumer_cases(cases: &[PageConsumerCase]) {
    for &(setup, copy_to_customer, pointer, message, recovers) in cases {
        let mut value = setup();
        assert_clean(&widget_findings(value.clone()));
        copy_to_customer(&mut value);
        assert_issue(&widget_findings(value.clone()), pointer, message);
        if recovers {
            render_orders_list_on_customer_page(&mut value);
            assert_clean(&widget_findings(value));
        }
    }
}

#[test]
fn selected_record_field_and_form_consumers_require_rendered_list() {
    assert_page_consumer_cases(&[
        (
            || {
                widget_with(|ux| {
                    apply_edits(
                        ux,
                        &[
                            r#"set /fields/0 visibleWhen = {"expression":"len(input.selection) > 0","inputs":{"selectionBindingId":"orders"}}"#,
                        ],
                    )
                })
            },
            |v| {
                apply_edits(
                    ux_mut(v),
                    &[
                        r#"push /pages/1/root/children = {"nodeId":"borrowed-order-number-node","semanticId":"borrowed-order-number","component":{"kind":"field","fieldId":"order-number"},"children":[]}"#,
                    ],
                )
            },
            "/fields/0/visibleWhen/inputs/selectionBindingId",
            "selection",
            false,
        ),
        (
            widget,
            |v| {
                push_ptr(
                    ux_mut(v),
                    "/pages/1/root/children",
                    field_node("customer-order-number-node", "order-number"),
                )
            },
            "/fields/0/visibleWhen/inputs/recordBindingId",
            "record input",
            true,
        ),
        (
            widget_with_no_input_form,
            |v| {
                apply_edits(
                    ux_mut(v),
                    &[
                        r#"push /pages/1/root/children = {"nodeId":"customer-approve-form-node","component":{"kind":"form","formId":"approve-form"},"children":[]}"#,
                    ],
                )
            },
            "/forms/0/dataBindingId",
            "prefill",
            true,
        ),
    ]);
}

#[test]
fn selected_record_query_and_navigation_consumers_require_rendered_list() {
    assert_page_consumer_cases(&[
        (
            || {
                let mut v = widget();
                let mut b = ptr_mut(ux_mut(&mut v), "/dataBindings/0").clone();
                apply_edits(
                    &mut b,
                    &[
                        r#"set . bindingId = "matching-orders""#,
                        r#"set /query/filters/0/property bindingId = "matching-orders""#,
                        r#"set /query/filters/0 value = {"kind":"record","property":{"bindingId":"orders","propertyId":"status"}}"#,
                        "set /query sort = []",
                    ],
                );
                push_ptr(ux_mut(&mut v), "/dataBindings", b);
                apply_edits(
                    ux_mut(&mut v),
                    &[
                        r#"push /pages/0/root/children = {"nodeId":"matching-orders-node","component":{"kind":"list","dataBindingId":"matching-orders","fields":[],"selection":"none"},"children":[]}"#,
                    ],
                );
                v
            },
            |v| {
                apply_edits(
                    ux_mut(v),
                    &[
                        r#"push /pages/1/root/children = {"nodeId":"customer-matching-orders-node","component":{"kind":"list","dataBindingId":"matching-orders","fields":[],"selection":"none"},"children":[]}"#,
                    ],
                )
            },
            "/dataBindings/3/query/filters/0/value",
            "record value requires",
            true,
        ),
        (
            widget,
            |v| {
                let mut list = ptr_mut(ux_mut(v), "/pages/0/root/children/1").clone();
                apply_edits(
                    &mut list,
                    &[
                        r#"set . nodeId = "customer-linked-customers-node""#,
                        r#"set . semanticId = "customer-linked-customers""#,
                    ],
                );
                push_ptr(ux_mut(v), "/pages/1/root/children", list);
            },
            "/dataBindings/1/query/sourceBindingId",
            "single-selection list",
            true,
        ),
    ]);
}

#[test]
fn selected_record_navigation_and_detail_consumers_require_rendered_list() {
    assert_page_consumer_cases(&[
        (
            || {
                widget_with(|ux| {
                    apply_edits(
                        ux,
                        &[
                            r#"set /navigationItems/0 visibleWhen = {"expression":"input.record.status == \"draft\"","inputs":{"recordBindingId":"orders"}}"#,
                        ],
                    )
                })
            },
            |v| {
                apply_edits(
                    ux_mut(v),
                    &[
                        r#"push /pages/1/root/children = {"nodeId":"customer-navigation-borrowed-node","component":{"kind":"navigation","itemId":"customer-nav"},"children":[]}"#,
                    ],
                )
            },
            "/navigationItems/0/visibleWhen/inputs/recordBindingId",
            "record input",
            false,
        ),
        (
            || {
                let mut v = widget();
                move_customer_navigation_to_page(&mut v);
                set_key(
                    ux_mut(&mut v),
                    "/pages/0/root/children/1/component",
                    "selection",
                    json!("single"),
                );
                v
            },
            |v| {
                let mut nav = ptr_mut(ux_mut(v), "/pages/0/root/children/3").clone();
                apply_edits(
                    &mut nav,
                    &[
                        r#"set . nodeId = "customer-selected-navigation-node""#,
                        r#"set . semanticId = "customer-selected-navigation""#,
                    ],
                );
                push_ptr(ux_mut(v), "/pages/1/root/children", nav);
            },
            "/navigationItems/0/visibleWhen/inputs/recordBindingId",
            "record input",
            true,
        ),
    ]);
    let mut detail = widget_with(|ux| {
        apply_edits(
            ux,
            &[
                r#"set /dataBindings/2/query/key/0 value = {"kind":"record","property":{"bindingId":"orders","propertyId":"customer_number"}}"#,
            ],
        )
    });
    assert_issue(
        &widget_findings(detail.clone()),
        "/dataBindings/2/query/key/0/value",
        "record value requires",
    );
    render_orders_list_on_customer_page(&mut detail);
    assert_clean(&widget_findings(detail));
}

fn widget_with_unselected_orders() -> Value {
    widget_with(|ux| {
        apply_edits(
            ux,
            &[
                r#"set /pages/0/root/children/0/component selection = "none""#,
                r#"set /dataBindings/1 query = {"kind":"object_list","filters":[],"sort":[],"page":{"size":10,"maxSize":10}}"#,
            ],
        )
    })
}

fn widget_with_unavailable_page_order_value(selection: &str, section: bool) -> Value {
    widget_with(|ux| {
        let _ = arr_mut(ux, "/pages/0/root/children/0/component/fields").remove(0);
        set_key(
            ux,
            "/pages/0/root/children/0/component",
            "selection",
            json!(selection),
        );
        del_key(ux, "/fields/0", "visibleWhen");
        if section {
            apply_edits(
                ux,
                &[
                    r#"set /pages/0/root/component/section fields = [{"bindingId":"orders","propertyId":"order_number"}]"#,
                ],
            );
        } else {
            push_ptr(
                ux,
                "/pages/0/root/children",
                field_node("page-order-number-node", "order-number"),
            );
        }
    })
}

#[test]
fn row_surface_and_page_navigation_record_isolation() {
    for section in [false, true] {
        for selection in ["none", "multiple"] {
            assert_issue(
                &widget_findings(widget_with_unavailable_page_order_value(selection, section)),
                "/fields/0",
                "field value requires an available",
            );
        }
        assert_clean(&widget_findings(widget_with_unavailable_page_order_value(
            "single", section,
        )));
    }
    assert_clean(&widget_findings(widget_with_unselected_orders()));

    for keep_row_field in [false, true] {
        let mut value = widget_with_unselected_orders();
        if !keep_row_field {
            let _ = arr_mut(
                ux_mut(&mut value),
                "/pages/0/root/children/0/component/fields",
            )
            .remove(0);
        }
        push_ptr(
            ux_mut(&mut value),
            "/pages/0/root/children",
            field_node("page-order-number-node", "order-number"),
        );
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

    let mut form_row = widget_with_unselected_orders();
    apply_edits(
        ux_mut(&mut form_row),
        &[
            r#"set /fields/4 visibleWhen = {"expression":"input.record.status == \"draft\"","inputs":{"recordBindingId":"orders"}}"#,
        ],
    );
    assert_clean(&widget_findings(form_row));

    let mut row_only = widget_with_unselected_orders();
    move_customer_navigation_to_page(&mut row_only);
    assert_issue(
        &widget_findings(row_only),
        "/navigationItems/0/visibleWhen/inputs/recordBindingId",
        "record input",
    );
    let app_nav = widget_with(|ux| {
        apply_edits(
            ux,
            &[
                r#"set /navigationItems/0 visibleWhen = {"expression":"input.record.status == \"draft\"","inputs":{"recordBindingId":"orders"}}"#,
                r#"set /app navigationItemIds = ["customer-nav"]"#,
            ],
        )
    });
    assert_issue(
        &widget_findings(app_nav),
        "/navigationItems/0/visibleWhen/inputs/recordBindingId",
        "record input",
    );
}

// ============================================================================
// Runtime keys and metric bucket display format
// ============================================================================

fn runtime_key_navigation_widget(source: Value) -> Value {
    let mut parameters = serde_json::Map::new();
    parameters.insert("orderKey".to_owned(), source);
    widget_with(|ux| {
        apply_edits(
            ux,
            &[
                r#"push /pages = {"pageId":"order-runtime-key","name":"Order runtime key","contextParams":[{"paramId":"orderKey","type":"text","required":true,"runtimeKeyOf":{"id":"commerce.Order.entity","version":"0.1.0"}}],"root":{"nodeId":"order-runtime-key-root","component":{"kind":"section","section":{"sectionId":"order-runtime-key-section","kind":"section","label":{"en":"Order runtime key"},"fields":[],"order":3}},"children":[]}}"#,
                r#"push /app/pageRefs = {"pageId":"order-runtime-key"}"#,
                r#"push /pages/0/root/children/0/children = {"nodeId":"order-runtime-key-navigation-node","component":{"kind":"navigation","itemId":"order-runtime-key-navigation"},"children":[]}"#,
            ],
        );
        push_ptr(
            ux,
            "/navigationItems",
            json!({
                "itemId": "order-runtime-key-navigation", "label": { "en": "Open order runtime key" },
                "requiredCapability": "commerce.order.read",
                "destination": { "kind": "page", "pageId": "order-runtime-key", "parameters": parameters }
            }),
        );
    })
}

#[test]
fn navigation_runtime_keys_require_a_matching_marked_entity_and_record_context() {
    let valid = runtime_key_navigation_widget(j(
        r#"{"kind":"record","property":{"bindingId":"orders","propertyId":"$key"}}"#,
    ));
    assert_clean(&widget_findings(valid.clone()));
    let mut routed = valid;
    apply_edits(
        ux_mut(&mut routed),
        &[
            r#"push /app/routes = {"routeId":"order-runtime-key-route","path":"/orders/by-runtime-key/{orderKey}","pageId":"order-runtime-key","parameters":[{"routeParameter":"orderKey","contextParameterId":"orderKey"}]}"#,
        ],
    );
    assert_clean(&widget_findings(routed));

    for (source_raw, extra_binding, message) in [
        (
            r#"{"kind":"record","property":{"bindingId":"customer-detail","propertyId":"$key"}}"#,
            false,
            "exact marked entity binding",
        ),
        (
            r#"{"kind":"record","property":{"bindingId":"unknown-orders","propertyId":"$key"}}"#,
            false,
            "exact marked entity binding",
        ),
        (
            r#"{"kind":"record","property":{"bindingId":"unrendered-order","propertyId":"$key"}}"#,
            true,
            "not available in this list row",
        ),
        (
            r#"{"kind":"record","property":{"bindingId":"orders","propertyId":"order_id"}}"#,
            false,
            "matching runtime-key binding or marked context parameter",
        ),
        (
            r#"{"kind":"literal","value":"business-1"}"#,
            false,
            "matching runtime-key binding or marked context parameter",
        ),
        (
            r#"{"kind":"context_parameter","pageId":"customer","parameterId":"customerNumber"}"#,
            false,
            "same required text runtimeKeyOf entity",
        ),
    ] {
        let mut value = runtime_key_navigation_widget(j(source_raw));
        if extra_binding {
            apply_edits(
                ux_mut(&mut value),
                &[
                    r#"push /dataBindings = {"bindingId":"unrendered-order","object":{"id":"commerce.Order.entity","version":"0.1.0"},"query":{"kind":"object_list","filters":[],"sort":[],"page":{"size":10,"maxSize":10}}}"#,
                ],
            );
        }
        assert_issue(
            &widget_findings(value),
            "/navigationItems/1/destination/parameters/orderKey",
            message,
        );
    }
}

#[test]
fn navigation_forwards_runtime_key_context_only_with_the_same_entity_marker() {
    let mut fwd = runtime_key_navigation_widget(j(
        r#"{"kind":"record","property":{"bindingId":"orders","propertyId":"$key"}}"#,
    ));
    apply_edits(
        ux_mut(&mut fwd),
        &[
            r#"push /pages = {"pageId":"order-runtime-key-source","name":"Order runtime key source","contextParams":[{"paramId":"sourceOrderKey","type":"text","required":true,"runtimeKeyOf":{"id":"commerce.Order.entity","version":"0.1.0"}}],"root":{"nodeId":"order-runtime-key-source-root","component":{"kind":"section","section":{"sectionId":"order-runtime-key-source-section","kind":"section","label":{"en":"Order runtime key source"},"fields":[],"order":4}},"children":[{"nodeId":"order-runtime-key-forward-node","component":{"kind":"navigation","itemId":"order-runtime-key-forward"},"children":[]}]}}"#,
            r#"push /app/pageRefs = {"pageId":"order-runtime-key-source"}"#,
            r#"push /navigationItems = {"itemId":"order-runtime-key-forward","label":{"en":"Forward order runtime key"},"requiredCapability":"commerce.order.read","destination":{"kind":"page","pageId":"order-runtime-key","parameters":{"orderKey":{"kind":"context_parameter","pageId":"order-runtime-key-source","parameterId":"sourceOrderKey"}}}}"#,
        ],
    );
    assert_contract_clean(
        &widget_findings(fwd.clone()),
        &[
            "widget.interaction.unreachable_page",
            "widget.interaction.keyboard_unreachable_control",
        ],
    );
    set_key(
        ux_mut(&mut fwd),
        "/pages/2/contextParams/0/runtimeKeyOf",
        "id",
        json!("crm.Customer.entity"),
    );
    assert_issue(
        &widget_findings(fwd.clone()),
        "/navigationItems/2/destination/parameters/orderKey",
        "same required text runtimeKeyOf entity",
    );
    del_key(ux_mut(&mut fwd), "/pages/2/contextParams/0", "runtimeKeyOf");
    assert_issue(
        &widget_findings(fwd),
        "/navigationItems/2/destination/parameters/orderKey",
        "runtime-key context parameters cannot be copied to unmarked destination parameters",
    );
}

#[test]
fn runtime_detail_requires_the_exact_entity_marker_and_owning_page() {
    let mut value = widget_with(|ux| {
        apply_edits(
            ux,
            &[
                r#"set /pages/1/contextParams/0 runtimeKeyOf = {"id":"crm.Customer.entity","version":"0.1.0"}"#,
                r#"set /dataBindings/2 query = {"kind":"object_runtime_detail","pageId":"customer","parameterId":"customerNumber"}"#,
                "set . navigationItems = []",
            ],
        );
        arr_mut(ux, "/pages/0/root/children/0/children")
            .retain(|n| n.pointer("/component/kind") != Some(&json!("navigation")));
    });
    let mut base = support_artifacts();
    del_key(
        base.get_mut(1).req("customer"),
        "/payload/objectType",
        "identity",
    );
    assert_contract_clean(
        &findings_with_support(value.clone(), base.clone(), &[]),
        &[
            "widget.interaction.unreachable_page",
            "widget.interaction.keyboard_unreachable_control",
        ],
    );
    set_key(
        ux_mut(&mut value),
        "/pages/1/contextParams/0/runtimeKeyOf",
        "id",
        json!("commerce.Order.entity"),
    );
    assert_issue(
        &findings_with_support(value, base, &[]),
        "/query/parameterId",
        "exact bound entity",
    );
}

fn metric_buckets_widget(nullable: bool) -> (Value, Vec<Value>) {
    let mut support = support_artifacts();
    push_ptr(
        support.first_mut().req("order"),
        "/payload/objectType/properties",
        json!({ "id": "buckets", "type": { "kind": "json-ext" }, "constraints": { "nullable": nullable } }),
    );
    let value = widget_with(|ux| {
        apply_edits(
            ux,
            &[
                r#"set /fields/1/target/property propertyId = "buckets""#,
                r#"set /fields/1 format = "metric_buckets""#,
                r#"set /pages/0/root/children/0/component/fields/1 propertyId = "buckets""#,
            ],
        )
    });
    (value, support)
}

#[test]
fn metric_bucket_display_format_contracts() {
    for nullable in [false, true] {
        let (value, support) = metric_buckets_widget(nullable);
        assert_clean(&findings_with_support(value, support, &[]));
    }
    assert_issue(
        &widget_findings(widget_with(|ux| {
            set_key(ux, "/fields/1", "format", json!("metric_buckets"))
        })),
        "/fields/1/format",
        "json-ext property",
    );
    let (mut editable, support) = metric_buckets_widget(false);
    set_key(
        ux_mut(&mut editable),
        "/fields/1",
        "control",
        json!("text_area"),
    );
    let found = findings_with_support(editable, support, &[]);
    assert_issue(&found, "/fields/1/control", "incompatible");
    assert_issue(&found, "/fields/1/format", "incompatible");
    assert_issue(
        &widget_findings(widget_with(|ux| {
            set_key(ux, "/fields/4", "format", json!("metric_buckets"))
        })),
        "/fields/4/format",
        "not an input",
    );
}

// ============================================================================
// UI-MACHINE-1 / UI-VIEW-1 / Studio record page / UI-CATALOG-1
// ============================================================================

fn machine_widget(machine: Value) -> Value {
    widget_with(|ux| {
        set_key(ux, "/pages/0", "machine", machine);
        apply_edits(
            ux,
            &[
                r#"set /pages/0 view = {"kind":"collection","subject":{"kind":"object_type","object":{"id":"commerce.Order.entity","version":"0.1.0"}}}"#,
                r#"set /pages/0/root/children/0 behaviors = ["selection/selection-model","focus/roving-focus"]"#,
            ],
        );
    })
}

/// The approve flow on the orders page: every outcome of the call is handled, errors recover, a
/// cancel is a request whose outcome follows. The same document drives `@norn/demo-ui`'s machine tests.
fn approve_machine() -> Value {
    serde_json::from_str(include_str!(
        "../../../fixtures/ui/orders-approve.machine.json"
    ))
    .req("machine fixture should be JSON")
}

fn ui_codes(widget: Value) -> Vec<(String, String)> {
    widget_findings(widget)
        .into_iter()
        .filter(|f| {
            f.code.as_str().starts_with("widget.machine.")
                || f.code.as_str().starts_with("widget.view.")
        })
        .map(|f| (f.code.as_str().to_owned(), f.message))
        .collect()
}

fn assert_ui_code(widget: Value, code: &str, message_part: &str) {
    let found = ui_codes(widget);
    assert!(
        found
            .iter()
            .any(|(c, m)| c == code && m.contains(message_part)),
        "expected {code} containing {message_part:?}; got {found:#?}"
    );
}

fn edit(mut machine: Value, change: impl FnOnce(&mut Value)) -> Value {
    change(&mut machine);
    machine_widget(machine)
}

fn drop_transition(machine: &mut Value, id: &str) {
    arr_mut(machine, "/transitions").retain(|t| t.at("transitionId") != id);
}

const MACHINE_REFUSALS: &[(&[&str], &str, &str)] = &[
    (
        &["drop on-timeout"],
        "widget.machine.pending_outcome_uncovered",
        "timeout",
    ),
    (
        &["drop on-approval"],
        "widget.machine.pending_outcome_uncovered",
        "pending_approval",
    ),
    (
        &["drop on-cancelled"],
        "widget.machine.pending_outcome_uncovered",
        "cancelled",
    ),
    (
        &[r#"set /transitions/3 guard = "input.event.retryable""#],
        "widget.machine.pending_outcome_uncovered",
        "refused:permission",
    ),
    (
        &[r#"set /events/5/source outcome = "answer_moved""#],
        "widget.machine.outcome_impossible",
        "answer_moved",
    ),
    (
        &["drop approval-seen"],
        "widget.machine.absorbing_state",
        "awaiting-approval",
    ),
    (
        &[r#"set /states/0 role = "pending""#],
        "widget.machine.initial_not_idle",
        "browsing",
    ),
    (
        &["drop on-approval"],
        "widget.machine.unreachable_state",
        "awaiting-approval",
    ),
    (
        &[
            r#"push /events = {"eventId":"orders-changed","source":{"kind":"subscription","bindingId":"orders"}}"#,
            "drop retry-failed",
            r#"set /transitions/11 on = "orders-changed""#,
        ],
        "widget.machine.error_without_recovery",
        "failed",
    ),
    (
        &[
            r#"push /transitions = {"transitionId":"walk-away","from":"approving","on":"dismiss","target":"browsing"}"#,
        ],
        "widget.machine.pending_abandoned",
        "approving",
    ),
    (
        &["set /transitions/0 effects = []"],
        "widget.machine.pending_entered_without_start",
        "approving",
    ),
    (
        &[
            r#"set /transitions/2 effects = [{"kind":"start","call":{"kind":"action","bindingId":"approve"}}]"#,
        ],
        "widget.machine.start_without_pending",
        "browsing",
    ),
    (
        &["del /states/1 deadlineMs"],
        "widget.machine.pending_without_deadline",
        "approving",
    ),
    (
        &[
            r#"push /events = {"eventId":"tick","source":{"kind":"timer","afterMs":1000}}"#,
            r#"push /transitions = {"transitionId":"poll","from":"browsing","on":"tick","target":"browsing"}"#,
        ],
        "widget.machine.input_free_cycle",
        "browsing",
    ),
    (
        &[
            r#"push /transitions = {"transitionId":"again","from":"refused","on":"dismiss","target":"browsing"}"#,
        ],
        "widget.machine.nondeterministic",
        "dismiss",
    ),
    (
        &[r#"set /transitions/3 assign = {"lastCode":"input.event.retryable"}"#],
        "widget.machine.invalid_expression",
        "type-check",
    ),
    (
        &[r#"set /transitions/3 assign = {"missing":"input.event.code"}"#],
        "widget.machine.unknown_reference",
        "missing",
    ),
];

const VIEW_REFUSALS: &[(&[&str], &str, &str)] = &[
    (
        &[r#"set /pages/0/view kind = "form""#],
        "widget.view.subject_mismatch",
        "Form",
    ),
    (
        &[r#"set /pages/0/view/subject/object id = "crm.Invoice.entity""#],
        "widget.view.subject_unbound",
        "neither read",
    ),
    (
        &[r#"set /pages/0/root behaviors = ["selection/selection-model"]"#],
        "widget.view.behavior_unfit",
        "orders-page",
    ),
];

#[test]
fn page_machine_admission_renderer_catalog_and_refusal_matrix() {
    assert_eq!(ui_codes(machine_widget(approve_machine())), []);
    let widget = envelope(machine_widget(approve_machine()));
    let norn_ir::ArtifactPayload::Widget(payload) = &widget.payload else {
        panic!("widget payload");
    };
    let ux: norn_ir::WidgetUx =
        serde_json::from_value(serde_json::to_value(payload.ux.as_value().req("ux")).req("ux"))
            .req("typed ux");
    let catalog = norn_ui_checker::ui_sdk::renderer_capabilities(&ux);
    for cap in [
        "agent:confirm",
        "agent:redaction",
        "view:collection",
        "component:list",
        "behavior:selection/selection-model",
        "behavior:focus/roving-focus",
        "effect:start:action",
        "effect:cancel",
        "event:user",
        "outcome:pending_approval",
    ] {
        assert!(catalog.contains(cap), "{cap} missing from {catalog:?}");
    }
    for &(edits, code, part) in MACHINE_REFUSALS {
        assert_ui_code(
            edit(approve_machine(), |m| apply_edits(m, edits)),
            code,
            part,
        );
    }
    for &(edits, code, part) in VIEW_REFUSALS {
        let mut w = machine_widget(approve_machine());
        apply_edits(ux_mut(&mut w), edits);
        assert_ui_code(w, code, part);
    }
}

fn record_page_errors(page: Value) -> Vec<(String, String)> {
    findings_with_support(page, Vec::new(), &[])
        .into_iter()
        .filter(Diagnostic::is_error)
        .map(|f| (f.span.map_or_default(|s| s.pointer), f.message))
        .collect()
}

fn assert_refused_at(found: &[(String, String)], pointer: &str, part: &str) {
    assert!(
        found
            .iter()
            .any(|(at, m)| at == pointer && m.contains(part)),
        "expected a finding at {pointer} saying {part:?}; got {found:#?}"
    );
}

fn record_page_with(change: impl FnOnce(&mut Value)) -> Value {
    let mut page: Value = serde_json::from_str(RECORD_PAGE).req("record page should be JSON");
    change(ux_mut(&mut page));
    page
}

fn record_transition(id: &str) -> usize {
    let page: Value = serde_json::from_str(RECORD_PAGE).req("record page should be JSON");
    page.pointer("/payload/ux/pages/0/machine/transitions")
        .req("transitions")
        .as_array()
        .req("transitions array")
        .iter()
        .position(|t| t.at("transitionId") == id)
        .req(id)
}

#[test]
fn studio_pages_are_admitted_clean_and_broken_record_pages_refuse() {
    for (source, artifact_id) in [
        (DECISION_SHEET, "norn.studio.decision-sheet.widget"),
        (RECORD_PAGE, "norn.studio.record.widget"),
    ] {
        let doc: Value = serde_json::from_str(source).req("studio fixture");
        let found: Vec<_> = findings_with_support(doc, Vec::new(), &[])
            .into_iter()
            .filter(|f| f.artifact_id.as_deref() == Some(artifact_id))
            .collect();
        assert_clean(&found);
    }
    for &(edit_spec, pointer, part) in &[
        (
            r#"set /catalog/0/label/msg source = "Reading a record""#,
            "/payload/ux/catalog/0/label/msg/id",
            "hash of its source text",
        ),
        (
            r#"set /contextReads/0 keyParameterId = "key""#,
            "/payload/ux/contextReads/0/keyParameterId",
            "required text context parameter",
        ),
        (
            r#"set /pages/0/root/component/section fields = [{"bindingId":"record","propertyId":"display_name"}]"#,
            "/payload/ux/pages/0/root/component/section/fields/0",
            "unknown data binding",
        ),
        (
            r#"set /pages/0/root/children/0/component heldIn = ["waiting"]"#,
            "/payload/ux/pages/0/root/children/0/component/heldIn/0",
            "does not declare",
        ),
    ] {
        assert_issue(
            &findings_with_support(
                record_page_with(|ux| apply_edits(ux, &[edit_spec])),
                Vec::new(),
                &[],
            ),
            pointer,
            part,
        );
    }
    let sheet: Value = serde_json::from_str(DECISION_SHEET).req("decision sheet");
    let codes = |doc: Value| -> Vec<(String, String)> {
        findings_with_support(doc, Vec::new(), &[])
            .into_iter()
            .map(|f| (f.code.as_str().to_owned(), f.message))
            .collect()
    };
    let mut dead = sheet.clone();
    arr_mut(ux_mut(&mut dead), "/pages/0/machine/events").retain(|e| e.at("eventId") != "stay");
    arr_mut(ux_mut(&mut dead), "/pages/0/machine/transitions").retain(|t| t.at("on") != "stay");
    let found = codes(dead);
    assert!(
        found
            .iter()
            .any(|(c, m)| c == "widget.machine.control_without_event" && m.contains("stay-button")),
        "{found:#?}"
    );

    let mut untyped = sheet;
    apply_edits(
        ux_mut(&mut untyped),
        &[
            r#"set /pages/0/machine/events/0 payload = [{"fieldId":"confirm","type":"boolean","required":true}]"#,
        ],
    );
    let found = codes(untyped);
    assert!(
        found
            .iter()
            .any(|(c, m)| c == "widget.machine.invalid_event" && m.contains("option-choice")),
        "{found:#?}"
    );
}

#[test]
fn record_page_layer_visibility_and_primitive_laws() {
    let retry = record_transition("retry");
    let retry_ptr = format!("/payload/ux/pages/0/machine/transitions/{retry}/from");
    for trapped in [
        record_page_with(|ux| {
            apply_edits(
                ux,
                &[r#"set /pages/0/root/children/1/component heldIn = ["showing"]"#],
            )
        }),
        record_page_with(|ux| {
            let children = arr_mut(ux, "/pages/0/root/children");
            let mut failed = children.get_mut(1).req("failed").take();
            del_key(&mut failed, "/component", "heldIn");
            *children.get_mut(1).req("failed slot") = json!({
                "nodeId": "failed-layer", "component": { "kind": "catalog", "entry": { "entryId": "ui-sdk:dialog" } }, "children": [failed]
            });
        }),
    ] {
        let found = record_page_errors(trapped);
        assert_refused_at(&found, &retry_ptr, "not drawn in `failed`");
        assert!(
            found
                .iter()
                .any(|(at, _)| at.starts_with("/payload/ux/pages/0/machine/states/5")),
            "{found:#?}"
        );
    }

    let mut layered = machine_widget(approve_machine());
    apply_edits(
        ux_mut(&mut layered),
        &[
            r#"push /pages/0/root/children = {"nodeId":"held-layer","component":{"kind":"catalog","entry":{"entryId":"ui-sdk:dialog"},"heldIn":["browsing"]},"children":[{"nodeId":"held-control","component":{"kind":"control","label":{"en":"Again","ar":"مجددًا"},"input":"trigger"},"children":[]}]}"#,
            r#"push /pages/0/machine/events = {"eventId":"held-again","source":{"kind":"user","nodeId":"held-control"}}"#,
            r#"push /pages/0/machine/transitions = {"transitionId":"held-again","from":"awaiting-approval","on":"held-again","target":"browsing"}"#,
        ],
    );
    assert_ui_code(
        layered,
        "widget.machine.invalid_event",
        "`held-again` comes from node `held-control`, which is not drawn in `awaiting-approval`",
    );

    let mut non_layer = machine_widget(approve_machine());
    apply_edits(
        ux_mut(&mut non_layer),
        &[
            r#"push /pages/0/root/children = {"nodeId":"scrolled","component":{"kind":"catalog","entry":{"entryId":"ui-sdk:scroll-area"}},"children":[{"nodeId":"scrolled-control","component":{"kind":"control","label":{"en":"Again","ar":"مجددًا"},"input":"trigger"},"children":[]}]}"#,
            r#"push /pages/0/root/children = {"nodeId":"checked","component":{"kind":"catalog","entry":{"entryId":"ui-sdk:checkbox"}},"children":[]}"#,
            r#"push /pages/0/machine/events = {"eventId":"scrolled-again","source":{"kind":"user","nodeId":"scrolled-control"}}"#,
            r#"push /pages/0/machine/events = {"eventId":"checked-change","source":{"kind":"user","nodeId":"checked","emits":"onCheckedChange"}}"#,
            r#"push /pages/0/machine/transitions = {"transitionId":"scrolled-again","from":"awaiting-approval","on":"scrolled-again","target":"browsing"}"#,
            r#"push /pages/0/machine/transitions = {"transitionId":"checked-change","from":"awaiting-approval","on":"checked-change","target":"browsing"}"#,
        ],
    );
    assert!(
        !ui_codes(non_layer)
            .iter()
            .any(|(_, m)| m.contains("not drawn in"))
    );

    let undeclared = record_page_with(|ux| {
        apply_edits(
            ux,
            &[
                r#"push /pages/0/root/children = {"nodeId":"held-button","component":{"kind":"catalog","entry":{"entryId":"ui-sdk:button"},"heldIn":["nowhere"]},"children":[]}"#,
            ],
        )
    });
    assert_refused_at(
        &record_page_errors(undeclared),
        "/payload/ux/pages/0/root/children/5/component/heldIn/0",
        "does not declare",
    );
}

#[test]
fn record_page_context_read_and_message_text_laws() {
    let cat_read = record_page_with(|ux| {
        apply_edits(
            ux,
            &[
                r#"set /catalog/0 machine = {"machineVersion":"0.1","initialStateId":"idle","fields":[],"events":[{"eventId":"go","source":{"kind":"user","nodeId":"self"}}],"states":[{"stateId":"idle","role":"idle"},{"stateId":"reading","role":"pending","awaits":{"kind":"read","bindingId":"record"},"deadlineMs":1000}],"transitions":[{"transitionId":"go","from":"idle","on":"go","target":"reading","effects":[{"kind":"start","call":{"kind":"read","bindingId":"record"}}]}]}"#,
            ],
        )
    });
    let found = record_page_errors(cat_read);
    assert_refused_at(
        &found,
        "/payload/ux/catalog/0/machine/states/1/awaits",
        "read only by the machine of the page",
    );
    assert_refused_at(
        &found,
        "/payload/ux/catalog/0/machine/transitions/0/effects/0/call",
        "read only by the machine of the page",
    );

    let data_msg = record_page_with(|ux| {
        apply_edits(
            ux,
            &[
                r#"set /pages/0/machine fields = [{"fieldId":"note","type":"json","required":false,"initial":{"msg":{"id":"not-a-hash","source":"data"}}}]"#,
            ],
        )
    });
    assert!(
        !record_page_errors(data_msg)
            .iter()
            .any(|(at, _)| at.contains("/fields/0/initial"))
    );

    let hash = b"a\0b\0c".iter().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| {
        (h ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3)
    });
    let shared = format!("m{hash:016x}");
    let nul_collision = record_page_with(|ux| {
        set_key(
            ux,
            "/catalog/0",
            "label",
            json!({ "msg": { "id": shared, "source": "b\u{0}c", "context": "a" } }),
        );
        set_key(
            ux,
            "/catalog/1",
            "label",
            json!({ "msg": { "id": shared, "source": "c", "context": "a\u{0}b" } }),
        );
    });
    let found = record_page_errors(nul_collision);
    assert_refused_at(&found, "/payload/ux/catalog/0/label/msg/source", "U+0000");
    assert_refused_at(&found, "/payload/ux/catalog/1/label/msg/context", "U+0000");
    assert_refused_at(
        &found,
        "/payload/ux/catalog/1/label/msg/id",
        "names two texts",
    );

    let copied_id = record_page_with(|ux| {
        let first_id = ptr_mut(ux, "/catalog/0/label/msg/id").clone();
        set_key(ux, "/catalog/1/label/msg", "id", first_id);
    });
    assert_refused_at(
        &record_page_errors(copied_id),
        "/payload/ux/catalog/1/label/msg/id",
        "names two texts",
    );
}

fn catalog_widget() -> Value {
    let mut widget = machine_widget(approve_machine());
    apply_edits(
        ux_mut(&mut widget),
        &[
            r#"set . catalog = [{"entryId":"kpi-tile","label":{"en":"KPI tile","ar":"بطاقة مؤشر"},"trust":"contract_checked","framework":"foldkit","density":"scan","props":[{"fieldId":"title","type":"text","required":true},{"fieldId":"note","type":"text","required":false}],"events":[{"eventId":"drill","payload":[{"fieldId":"period","type":"text","required":true}]}],"behaviors":["platform/announce"]},{"entryId":"yield-map","label":{"en":"Yield map","ar":"خريطة المردود"},"trust":"opaque_island","framework":"three","density":"focus","props":[{"fieldId":"points","type":"json","required":true}],"events":[{"eventId":"point-selected","payload":[{"fieldId":"siteId","type":"text","required":true}]}]},{"entryId":"money","label":{"en":"Money","ar":"مبلغ"},"trust":"native","renders":"number"}]"#,
            r#"push /pages/0/root/children = {"nodeId":"kpi-node","component":{"kind":"catalog","entry":{"entryId":"kpi-tile"},"props":{"title":"\"Approvals\"","note":"input.fields.lastCode"}},"children":[]}"#,
            r#"push /pages/0/root/children = {"nodeId":"map-node","component":{"kind":"catalog","entry":{"entryId":"yield-map"},"props":{"points":"input.fields.lastCode"}},"children":[]}"#,
            r#"push /pages/0/machine/events = {"eventId":"drill","source":{"kind":"user","nodeId":"kpi-node","emits":"drill"},"payload":[{"fieldId":"period","type":"text","required":true}]}"#,
            r#"push /pages/0/machine/events = {"eventId":"site","source":{"kind":"user","nodeId":"map-node","emits":"point-selected"},"payload":[{"fieldId":"siteId","type":"text","required":true}]}"#,
            r#"push /pages/0/machine/transitions = {"transitionId":"drill-browsing","from":"browsing","on":"drill","target":"browsing"}"#,
            r#"push /pages/0/machine/transitions = {"transitionId":"site-browsing","from":"browsing","on":"site","target":"browsing"}"#,
        ],
    );
    widget
}

const CATALOG_JOIN_REFUSALS: &[(&str, &str, &str)] = &[
    (
        r#"set /pages/0/root/children/3/component/entry entryId = "gauge""#,
        "widget.catalog.unknown_entry",
        "gauge",
    ),
    (
        r#"set /pages/0/root/children/3/component/props colour = "\"red\"""#,
        "widget.catalog.unknown_prop",
        "colour",
    ),
    (
        "del /pages/0/root/children/3/component/props title",
        "widget.catalog.unbound_prop",
        "title",
    ),
    (
        r#"set /pages/0/root/children/3/component/props title = "1 + 1""#,
        "widget.catalog.invalid_prop",
        "title",
    ),
    (
        r#"set /pages/0/machine/events/10/source emits = "explode""#,
        "widget.catalog.unknown_event",
        "explode",
    ),
    (
        r#"set /pages/0/machine/events/10/payload/0 type = "integer""#,
        "widget.catalog.event_payload_mismatch",
        "drill",
    ),
    (
        r#"set /catalog/0 renders = "number""#,
        "widget.catalog.duplicate_renderer",
        "number",
    ),
    (
        r#"set /pages/0/machine/transitions/0 componentGuard = "When#0""#,
        "widget.machine.component_guard_outside_catalog",
        "ZEN",
    ),
];

fn assert_catalog_code(widget: Value, code: &str, part: &str) {
    let found: Vec<(String, String)> = widget_findings(widget)
        .into_iter()
        .map(|f| (f.code.as_str().to_owned(), f.message))
        .collect();
    assert!(
        found.iter().any(|(c, m)| c == code && m.contains(part)),
        "expected {code} containing {part:?}; got {found:#?}"
    );
}

#[test]
fn catalog_join_scope_and_guard_contracts() {
    let found = ui_codes(catalog_widget());
    let catalog: Vec<_> = widget_findings(catalog_widget())
        .into_iter()
        .filter(|f| f.code.as_str().starts_with("widget.catalog."))
        .collect();
    assert!(
        found.is_empty() && catalog.is_empty(),
        "{found:#?} {catalog:#?}"
    );

    let provider_id = "commerce.catalog_provider.widget";
    let narrow = j(r#"{"orgId":"org_demo","environmentId":"dev","tenantId":"tenant_alpha"}"#);
    let mut provider = catalog_widget();
    provider.set("artifactId", json!(provider_id));
    set_key(&mut provider, "/payload", "widgetId", json!(provider_id));
    provider.set("scope", narrow.clone());
    let version = provider.at("version").clone();
    for (scope, refused) in [(widget().at("scope").clone(), true), (narrow, false)] {
        let mut consumer = catalog_widget();
        consumer.set("scope", scope);
        push_ptr(
            &mut consumer,
            "/dependencies",
            json!({ "artifactId": provider_id, "kind": "widget", "version": version, "required": true }),
        );
        for node in arr_mut(ux_mut(&mut consumer), "/pages/0/root/children")
            .iter_mut()
            .filter(|n| n.at("nodeId") == "kpi-node")
        {
            set_key(
                node,
                "/component/entry",
                "widget",
                json!({ "id": provider_id, "version": version }),
            );
        }
        let found = widget_findings_with_base(consumer, std::slice::from_ref(&provider));
        assert_eq!(
            found
                .iter()
                .any(|f| f.code.as_str() == "widget.catalog.unknown_entry"),
            refused,
            "{found:#?}"
        );
    }

    for &(edit_spec, code, part) in CATALOG_JOIN_REFUSALS {
        let mut w = catalog_widget();
        apply_edits(ux_mut(&mut w), &[edit_spec]);
        assert_catalog_code(w, code, part);
    }
    let mut island_machine = catalog_widget();
    set_key(
        ux_mut(&mut island_machine),
        "/catalog/1",
        "machine",
        approve_machine(),
    );
    assert_catalog_code(island_machine, "widget.catalog.island_machine", "yield-map");

    let mut with_guard = catalog_widget();
    apply_edits(
        ux_mut(&mut with_guard),
        &[
            r#"set /catalog/0 machine = {"machineVersion":"0.1","initialStateId":"Expanded","fields":[],"events":[{"eventId":"ClickedPin","source":{"kind":"user","nodeId":"self"}}],"states":[{"stateId":"Expanded","role":"idle"},{"stateId":"Pinned","role":"idle"}],"transitions":[{"transitionId":"pin","from":"Expanded","on":"ClickedPin","target":"Pinned","componentGuard":"When#0"},{"transitionId":"stay","from":"Expanded","on":"ClickedPin","target":"Expanded","componentGuard":"Otherwise#1"},{"transitionId":"unpin","from":"Pinned","on":"ClickedPin","target":"Expanded"}]}"#,
        ],
    );
    assert_eq!(ui_codes(with_guard), []);
}

// ============================================================================
// Property-based structural invariants (proptest)
// ============================================================================

fn sorted_diagnostic_keys(
    values: &[Value],
    order: &[usize],
) -> Vec<(String, Option<String>, String)> {
    let artifacts: Vec<_> = order
        .iter()
        .filter_map(|&idx| values.get(idx).cloned())
        .map(envelope)
        .collect();
    let mut keys: Vec<_> = check_artifact_set(
        &artifacts,
        &EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &[],
            tenant: None,
        },
    )
    .into_iter()
    .map(|finding| {
        (
            finding.code.as_str().to_owned(),
            finding.artifact_id,
            finding.span.map_or_default(|span| span.pointer),
        )
    })
    .collect();
    keys.sort();
    keys
}

proptest::proptest! {
    #![proptest_config(proptest::test_runner::Config::with_cases(32))]

    /// Structural determinism: artifact-set admission and refusal diagnostics are invariant
    /// under every permutation of the artifact slice.
    #[test]
    fn prop_artifact_set_checking_is_permutation_invariant(
        a in 0_usize..4,
        b in 0_usize..3,
        c in 0_usize..2,
    ) {
        let mut remaining = vec![0_usize, 1, 2, 3];
        let order = [
            remaining.remove(a),
            remaining.remove(b),
            remaining.remove(c),
            remaining.remove(0),
        ];
        let clean_set = [order_entity(), customer_entity(), action_artifact(), widget()];
        proptest::prop_assert_eq!(sorted_diagnostic_keys(&clean_set, &order), Vec::new());

        let broken_widget = widget_with(|ux| del_key(ux, "/actionBindings/0/inputMapping", "orderId"));
        let broken_set = [order_entity(), customer_entity(), action_artifact(), broken_widget];
        let baseline = sorted_diagnostic_keys(&broken_set, &[0, 1, 2, 3]);
        proptest::prop_assert!(!baseline.is_empty());
        proptest::prop_assert_eq!(sorted_diagnostic_keys(&broken_set, &order), baseline);
    }

    /// Monotonicity / refusal boundary: list page bounds admit iff `0 < size <= maxSize`,
    /// and traversal bounds admit iff `0 < size <= maxSize <= maxRows`.
    #[test]
    fn prop_page_and_traversal_bounds_refusal_boundary(
        size in 0_u32..=20,
        max_size in 0_u32..=20,
        max_rows in 0_u32..=20,
    ) {
        let list_widget = widget_with(|ux| {
            set_key(ux, "/dataBindings/0/query", "page", json!({ "size": size, "maxSize": max_size }));
        });
        let list_valid = size > 0 && max_size > 0 && size <= max_size;
        let list_found = widget_findings(list_widget);
        proptest::prop_assert_eq!(list_found.is_empty(), list_valid, "list bounds ({size}, {max_size}): {list_found:#?}");

        let traversal_widget = widget_with(|ux| {
            set_key(ux, "/dataBindings/1/query", "maxRows", json!(max_rows));
            set_key(ux, "/dataBindings/1/query", "page", json!({ "size": size, "maxSize": max_size }));
        });
        let traversal_valid = list_valid && max_rows > 0 && max_size <= max_rows;
        let traversal_found = widget_findings(traversal_widget);
        proptest::prop_assert_eq!(
            traversal_found.is_empty(),
            traversal_valid,
            "traversal bounds ({size}, {max_size}, {max_rows}): {traversal_found:#?}"
        );
    }

    /// Integral proof boundary: exact integer literals admit for integer action inputs,
    /// while any non-integral decimal literal refuses with an integer-proof error.
    #[test]
    fn prop_integer_action_input_literal_proof_boundary(
        whole in 0_u32..=100_000,
        frac_prefix in 0_u32..=99,
        frac_last in 1_u32..=9,
    ) {
        let int_expr = format!("{whole}");
        let int_found = widget_findings(widget_with(|ux| set_key(ux, "/actionBindings/0/inputMapping", "expectedVersion", json!(int_expr))));
        proptest::prop_assert_eq!(int_found, Vec::new());

        let frac_expr = format!("{whole}.{frac_prefix:02}{frac_last}");
        let frac_found = widget_findings(widget_with(|ux| set_key(ux, "/actionBindings/0/inputMapping", "expectedVersion", json!(frac_expr))));
        proptest::prop_assert!(
            frac_found.iter().any(|f| {
                f.span.as_ref().is_some_and(|s| s.pointer.contains("/actionBindings/0/inputMapping/expectedVersion"))
                    && f.message.contains("integer")
            }),
            "expected integer refusal for {frac_expr}; got {frac_found:#?}"
        );
    }

    /// Blank capabilities and undeclared entity properties always refuse at their exact pointers.
    #[test]
    fn prop_blank_capabilities_and_unknown_properties_always_refuse(
        blank in "[ \t\r\n]{0,8}",
        suffix in "[a-z]{1,10}",
    ) {
        let cap_found = widget_findings(widget_with(|ux| set_key(ux, "/navigationItems/0", "requiredCapability", json!(blank))));
        proptest::prop_assert!(
            cap_found.iter().any(|f| {
                f.span.as_ref().is_some_and(|s| s.pointer.contains("/navigationItems/0/requiredCapability"))
                    && f.message.contains("must not be blank")
            }),
            "expected blank capability refusal; got {cap_found:#?}"
        );

        let prop_name = format!("unknown_{suffix}");
        let prop_found = widget_findings(widget_with(|ux| set_key(ux, "/fields/0/target/property", "propertyId", json!(prop_name))));
        proptest::prop_assert!(
            prop_found.iter().any(|f| {
                f.span.as_ref().is_some_and(|s| s.pointer.contains("/fields/0/target/property"))
                    && f.message.contains("property")
            }),
            "expected unknown property refusal for {prop_name}; got {prop_found:#?}"
        );
    }
}

#[path = "widget_ux/ui_sdk_laws.rs"]
mod ui_sdk_laws;

#[path = "widget_ux/verified_receipt.rs"]
mod verified_receipt;
