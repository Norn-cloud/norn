#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs,
    reason = "integration-test crate"
)]

//! T1-IR-1d: typed Access (`policy`), Widget, Connector, Customization, and
//! View (`derived`) roots.
//!
//! Every fixture of these kinds decodes through the lenient envelope root *and*
//! through the strict typed root, and both re-encode to the same canonical
//! JSON. The three new envelope fixtures pin their Norn1 canonical hashes.

use norn_canonical::HashProfile;
use norn_ir::{
    ArtifactEnvelope, ArtifactKind, ConnectorAuth, ConnectorResource, ConnectorRoot,
    CustomizationOverlayOp, CustomizationRoot, PolicyRoot, ViewInputKind, ViewIrPayload,
    WholeNumber, WidgetQuery, WidgetRoot, artifact_payload_schema,
};
use serde_json::{Value, json};

const CONNECTOR_FIXTURE: &str =
    include_str!("../../../fixtures/ir/retail-inventory.connector.json");
const WIDGET_FIXTURE: &str = include_str!("../../../fixtures/ir/retail-replenishment.widget.json");
const WIDGET_UX_FIXTURE: &str = include_str!("../../../fixtures/ir/ux-orders-customer.widget.json");
const POLICY_FIXTURE: &str = include_str!("../../../fixtures/ir/retail-procurement.policy.json");
const CUSTOMIZATION_FIXTURE: &str =
    include_str!("../../../fixtures/ir/retail-sku-reorder-point.customization.json");
const DERIVED_FIXTURE: &str = include_str!("../../../fixtures/ir/retail-low-stock.derived.json");

fn canonical_hash(bytes: &[u8]) -> String {
    HashProfile::Norn1
        .hash_json(bytes)
        .expect("JSON should canonicalize")
}

fn payload_json(source: &str) -> Value {
    let envelope: Value = serde_json::from_str(source).expect("fixture should parse");
    envelope["payload"].clone()
}

/// Decode a payload into its typed root and assert the canonical JSON is
/// unchanged by the typed round-trip.
fn assert_typed_roundtrip<T>(name: &str, payload: &Value) -> T
where
    T: serde::de::DeserializeOwned + serde::Serialize,
{
    let typed: T = serde_json::from_value(payload.clone())
        .unwrap_or_else(|error| panic!("{name} should decode into its typed root: {error}"));
    let reencoded = serde_json::to_vec(&typed).expect("typed root should encode");
    let source = serde_json::to_vec(payload).expect("payload should encode");
    assert_eq!(
        canonical_hash(&reencoded),
        canonical_hash(&source),
        "{name} canonical JSON changed after typed round-trip"
    );
    typed
}

type Decode = fn(Value) -> bool;

fn decodes<T: serde::de::DeserializeOwned>(payload: Value) -> bool {
    serde_json::from_value::<T>(payload).is_ok()
}

#[test]
fn new_envelope_fixtures_roundtrip_with_pinned_hashes() {
    for (name, source, kind, pinned) in [
        (
            "fixtures/ir/retail-procurement.policy.json",
            POLICY_FIXTURE,
            ArtifactKind::Policy,
            "3244045430767ac35de1576708f83a626e94a966b4500507979ab94be9dca850",
        ),
        (
            "fixtures/ir/retail-sku-reorder-point.customization.json",
            CUSTOMIZATION_FIXTURE,
            ArtifactKind::Customization,
            "a585f9abf4a6208929706fbde129a4d55a3a0e8127dfda2ac520692809eecec5",
        ),
        (
            "fixtures/ir/retail-low-stock.derived.json",
            DERIVED_FIXTURE,
            ArtifactKind::Derived,
            "1c84ed289a35838983f538bd72e3b2622f1d99fb4e1dd1644d035206f4fda309",
        ),
    ] {
        let envelope: ArtifactEnvelope = serde_json::from_str(source)
            .unwrap_or_else(|error| panic!("{name} should deserialize: {error}"));
        assert_eq!(envelope.kind(), kind, "{name} kind");
        assert!(
            envelope.payload.unknown_fields().is_empty(),
            "{name} has undeclared members: {:?}",
            envelope.payload.unknown_fields()
        );
        let reserialized = serde_json::to_vec(&envelope).expect("envelope should serialize");
        let source_hash = canonical_hash(source.as_bytes());
        assert_eq!(
            canonical_hash(&reserialized),
            source_hash,
            "{name} canonical JSON changed after envelope round-trip"
        );
        assert_eq!(source_hash, pinned, "{name} canonical hash drifted");
    }
}

#[test]
fn connector_fixture_decodes_into_the_typed_root() {
    let connector: ConnectorRoot = assert_typed_roundtrip(
        "retail-inventory.connector.json",
        &payload_json(CONNECTOR_FIXTURE),
    );
    assert!(matches!(connector.auth, ConnectorAuth::ApiKey { .. }));
    assert!(matches!(
        connector.resources[1],
        ConnectorResource::CsvImport { .. }
    ));
    let policy = connector
        .dead_letter_policy
        .expect("fixture has a dead-letter policy");
    assert_eq!(policy.max_failures, WholeNumber(3));
    // Members today's validator does not check stay verbatim.
    let cursor = connector.cursor.expect("fixture has a cursor");
    assert_eq!(
        cursor.additional.keys().collect::<Vec<_>>(),
        ["initialValue"]
    );
}

#[test]
fn widget_fixture_decodes_into_the_typed_root() {
    let widget: WidgetRoot = assert_typed_roundtrip(
        "retail-replenishment.widget.json",
        &payload_json(WIDGET_FIXTURE),
    );
    assert!(matches!(
        widget.data_dependencies[0].query,
        WidgetQuery::ObjectDetail { .. }
    ));
    assert!(
        widget.action_bindings[0]
            .additional
            .contains_key("inputMapping")
    );
}

#[test]
fn child_grid_and_nested_row_targets_roundtrip_as_typed_widget_ir() {
    let mut payload = widget_ux_payload();
    payload["ux"]["pages"][0]["root"]["children"]
        .as_array_mut()
        .expect("children")
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

    let widget: WidgetRoot = assert_typed_roundtrip("Widget UX with child grid", &payload);
    let ux = widget.ux.expect("UX contract is present");
    assert!(matches!(
        ux.pages[0].root.children[1].component,
        norn_ir::widget_ux::WidgetComponent::ChildGrid { .. }
    ));
    assert!(matches!(
        ux.fields[2].target,
        norn_ir::widget_ux::WidgetFieldTarget::ChildRowProperty { .. }
    ));
}

fn widget_ux_payload() -> Value {
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

fn schema_definition<'a>(schema: &'a Value, name: &str) -> &'a Value {
    schema["$defs"]
        .get(name)
        .unwrap_or_else(|| panic!("schema definition {name} should exist"))
}

fn schema_requires(schema: &Value, member: &str) -> bool {
    schema["required"]
        .as_array()
        .is_some_and(|required| required.contains(&json!(member)))
}

fn data_query_variant<'a>(schema: &'a Value, kind: &str) -> &'a Value {
    schema_definition(schema, "WidgetDataQuery")["oneOf"]
        .as_array()
        .and_then(|variants| {
            variants
                .iter()
                .find(|variant| variant["properties"]["kind"]["const"] == kind)
        })
        .unwrap_or_else(|| panic!("WidgetDataQuery should have {kind} variant"))
}

#[test]
fn widget_ux_decodes_and_roundtrips_as_a_closed_versioned_contract() {
    let payload = widget_ux_payload();
    let widget: WidgetRoot = assert_typed_roundtrip("minimal Widget UX", &payload);
    assert_eq!(
        widget.ux.expect("UX contract is present").pages[0].page_id,
        "orders"
    );

    let mut malformed = payload.clone();
    malformed["ux"]["dataBindings"][0]["query"]["unchecked"] = json!(true);
    assert!(
        !decodes::<WidgetRoot>(malformed),
        "UX query members are closed"
    );

    let mut malformed = payload.clone();
    malformed["ux"]["unchecked"] = json!(true);
    assert!(
        !decodes::<WidgetRoot>(malformed),
        "UX root members are closed"
    );

    let mut malformed = payload.clone();
    malformed["ux"]["uxVersion"] = json!("0.2");
    assert!(
        !decodes::<WidgetRoot>(malformed),
        "unknown UX versions are rejected"
    );

    let mut malformed = payload.clone();
    malformed["ux"] = Value::Null;
    assert!(!decodes::<WidgetRoot>(malformed), "null UX is rejected");

    let mut malformed = payload.clone();
    malformed["ux"]["dataBindings"][0]["query"]
        .as_object_mut()
        .expect("list query is an object")
        .remove("page");
    assert!(
        !decodes::<WidgetRoot>(malformed),
        "list page bounds are required"
    );

    let mut malformed = payload;
    malformed["ux"]["dataBindings"][1]["query"]
        .as_object_mut()
        .expect("traversal query is an object")
        .remove("maxRows");
    assert!(
        !decodes::<WidgetRoot>(malformed),
        "traversal row bounds are required"
    );
}

fn widget_ux_with_raw_filter_number(number: &str) -> String {
    let mut payload = widget_ux_payload();
    payload["ux"]["dataBindings"][0]["query"]["filters"] = json!([{
        "property": { "bindingId": "orders", "propertyId": "id" },
        "operator": "eq",
        "value": { "kind": "literal", "value": "__RAW_NUMBER__" }
    }]);
    serde_json::to_string(&payload)
        .expect("UX payload should encode")
        .replace("\"__RAW_NUMBER__\"", number)
}

fn widget_envelope_with_raw_filter_number(number: &str) -> String {
    let marker = "\"value\": \"draft\"";
    assert_eq!(WIDGET_UX_FIXTURE.matches(marker).count(), 1);
    WIDGET_UX_FIXTURE.replacen(marker, &format!("\"value\": {number}"), 1)
}

#[test]
fn widget_ux_raw_json_rejects_only_numbers_that_change_through_jsonvalue() {
    for literal in ["9007199254740993", "0.3000000000000000000000000001"] {
        let raw = widget_ux_with_raw_filter_number(literal);
        assert!(
            serde_json::from_str::<WidgetRoot>(&raw).is_err(),
            "direct WidgetRoot ingress must reject lossy UX literal {literal}"
        );

        let raw_filter = format!("{{\"kind\":\"literal\",\"value\":{literal}}}");
        assert!(
            serde_json::from_str::<norn_ir::widget_ux::WidgetFilterValue>(&raw_filter).is_err(),
            "direct WidgetFilterValue ingress must reject lossy literal {literal}"
        );

        let envelope = widget_envelope_with_raw_filter_number(literal);
        assert!(
            serde_json::from_str::<ArtifactEnvelope>(&envelope).is_err(),
            "full flattened envelope ingress must reject lossy UX literal {literal}"
        );
    }

    for literal in ["9007199254740992", "0.1"] {
        let raw = widget_ux_with_raw_filter_number(literal);
        assert!(serde_json::from_str::<WidgetRoot>(&raw).is_ok());
        let raw_filter = format!("{{\"kind\":\"literal\",\"value\":{literal}}}");
        assert!(serde_json::from_str::<norn_ir::widget_ux::WidgetFilterValue>(&raw_filter).is_ok());
        let envelope = widget_envelope_with_raw_filter_number(literal);
        assert!(serde_json::from_str::<ArtifactEnvelope>(&envelope).is_ok());
    }
}

#[test]
fn widget_record_runtime_key_value_roundtrips_with_the_reserved_key_property() {
    let value = json!({
        "kind": "record",
        "property": { "bindingId": "orders", "propertyId": "$key" }
    });
    let typed: norn_ir::widget_ux::WidgetFilterValue =
        serde_json::from_value(value.clone()).expect("runtime key filter value should decode");
    assert_eq!(
        serde_json::to_value(typed).expect("runtime key filter value should encode"),
        value
    );
    assert!(
        serde_json::from_value::<norn_ir::widget_ux::WidgetFilterValue>(json!({
            "kind": "record",
            "property": { "bindingId": "orders", "propertyId": "$key" },
            "bindingId": "orders"
        }))
        .is_err()
    );

    let context_parameter = json!({
        "paramId": "orderKey",
        "type": "text",
        "required": true,
        "runtimeKeyOf": { "id": "commerce.Order.entity", "version": "0.1.0" }
    });
    let typed: norn_ir::widget_ux::WidgetContextParam =
        serde_json::from_value(context_parameter.clone())
            .expect("runtime key context marker should decode");
    assert_eq!(
        serde_json::to_value(typed).expect("runtime key context marker should encode"),
        context_parameter
    );
    assert!(
        serde_json::from_value::<norn_ir::widget_ux::WidgetContextParam>(json!({
            "paramId": "orderKey",
            "type": "text",
            "required": true,
            "runtimeKeyOf": null
        }))
        .is_err()
    );
}

#[test]
fn widget_ux_value_ingress_guards_precision_without_changing_legacy_paths() {
    let ux_envelope: ArtifactEnvelope = serde_json::from_value(
        serde_json::from_str(WIDGET_UX_FIXTURE).expect("UX fixture should parse as JSON"),
    )
    .expect("in-memory envelope decoding remains compatible");
    assert_eq!(ux_envelope.kind(), ArtifactKind::Widget);

    let value_filter = json!({ "kind": "literal", "value": 9_007_199_254_740_993_u64 });
    assert!(serde_json::from_value::<norn_ir::widget_ux::WidgetFilterValue>(value_filter).is_err());
    assert!(
        serde_json::from_value::<norn_ir::widget_ux::WidgetFilterValue>(json!({
            "kind": "literal",
            "value": 0.1
        }))
        .is_ok()
    );

    let mut widget_value = widget_ux_payload();
    widget_value["ux"]["dataBindings"][0]["query"]["filters"] = json!([{
        "property": { "bindingId": "orders", "propertyId": "id" },
        "operator": "eq",
        "value": { "kind": "literal", "value": 9_007_199_254_740_993_u64 }
    }]);
    assert!(serde_json::from_value::<WidgetRoot>(widget_value).is_err());

    let mut envelope_value: Value =
        serde_json::from_str(WIDGET_UX_FIXTURE).expect("UX fixture should parse as JSON");
    envelope_value["payload"]["ux"]["dataBindings"][0]["query"]["filters"][0]["value"]["value"] =
        json!(9_007_199_254_740_993_u64);
    assert!(serde_json::from_value::<ArtifactEnvelope>(envelope_value).is_err());

    let legacy_with_large_number = WIDGET_FIXTURE.replace(
        "\"kind\": \"ready\"",
        "\"kind\": \"ready\", \"legacyNumber\": 9007199254740993",
    );
    assert!(serde_json::from_str::<ArtifactEnvelope>(&legacy_with_large_number).is_ok());

    let constructed_float = 9.1417e60_f64;
    let mut legacy_value: Value =
        serde_json::from_str(WIDGET_FIXTURE).expect("legacy widget should parse as JSON");
    legacy_value["payload"]["extensions"] = json!({
        "numeric-test/0.1.0": constructed_float
    });
    let legacy_envelope: ArtifactEnvelope = serde_json::from_value(legacy_value)
        .expect("legacy constructed float should remain accepted");
    let roundtrip = serde_json::to_value(legacy_envelope).expect("legacy envelope should encode");
    assert_eq!(
        roundtrip["payload"]["extensions"]["numeric-test/0.1.0"]
            .as_f64()
            .expect("extension value should remain numeric")
            .to_bits(),
        constructed_float.to_bits(),
        "non-UX from_value numeric bits must not pass through a text reparse"
    );
}

#[test]
fn widget_ux_schema_is_versioned_closed_and_requires_query_bounds() {
    let schema = serde_json::to_value(artifact_payload_schema(&ArtifactKind::Widget))
        .expect("widget schema should serialize");
    assert_eq!(schema["properties"]["ux"]["$ref"], "#/$defs/WidgetUx");

    let ux = schema_definition(&schema, "WidgetUx");
    assert_eq!(ux["additionalProperties"], json!(false));
    for member in [
        "uxVersion",
        "pages",
        "dataBindings",
        "actionBindings",
        "fields",
        "forms",
        "actionPlacements",
        "navigationItems",
        "overrides",
    ] {
        assert!(schema_requires(ux, member), "WidgetUx requires {member}");
    }
    let version = schema_definition(&schema, "IrVersion01");
    assert_eq!(version["enum"], json!(["0.1"]));

    let list = data_query_variant(&schema, "object_list");
    assert_eq!(list["additionalProperties"], json!(false));
    assert!(
        schema_requires(list, "page"),
        "object_list requires page bounds"
    );

    let traversal = data_query_variant(&schema, "link_traversal");
    assert_eq!(traversal["additionalProperties"], json!(false));
    for member in ["sourceBindingId", "linkId", "maxDepth", "maxRows", "page"] {
        assert!(
            schema_requires(traversal, member),
            "link_traversal requires {member}"
        );
    }

    let action = schema_definition(&schema, "WidgetUxActionBinding");
    assert_eq!(action["additionalProperties"], json!(false));
    assert_eq!(
        action["properties"]["inputMapping"]["type"],
        json!("object")
    );
    assert_eq!(
        action["properties"]["inputMapping"]["additionalProperties"]["$ref"],
        "#/$defs/ZenExpression"
    );
}

#[test]
fn access_policy_fixture_and_approval_form_decode_into_the_typed_root() {
    let policy: PolicyRoot = assert_typed_roundtrip(
        "retail-procurement.policy.json",
        &payload_json(POLICY_FIXTURE),
    );
    let PolicyRoot::Obligations(policy) = policy else {
        panic!("versioned policy should select the obligations form");
    };
    assert_eq!(policy.obligations[0].review_roles, ["procurement_manager"]);

    let approval = json!({
        "policyId": "commerce.discount.policy",
        "appliesTo": "commerce.discount.apply",
        "reviewRequiredWhen": "discount > 0.2",
        "approvalRoles": ["sales_manager"]
    });
    let approval: PolicyRoot = assert_typed_roundtrip("approval policy", &approval);
    assert!(matches!(approval, PolicyRoot::Approval(_)));
}

#[test]
fn access_policy_generic_rules_roundtrip_through_the_closed_envelope_root() {
    let wire = json!({
        "irVersion": "0.1",
        "artifactId": "core.entitlements.policy",
        "kind": "policy",
        "namespace": "core",
        "name": "Entitlement Policy",
        "version": "0.1.0",
        "lifecycle": "active",
        "scope": {},
        "dependencies": [],
        "provenance": { "source": "system" },
        "payload": {
            "accessVersion": "0.1",
            "policyId": "core.entitlements",
            "rules": [],
            "genericRules": [{
                "ruleId": "entitlement.none",
                "target": { "kind": "allAdmittedObjectTypes" },
                "actions": { "kind": "allAdmittedActions" },
                "scope": { "dimensions": ["org"] },
                "allOf": [{ "entitlement": { "kind": "none" } }]
            }]
        }
    });
    let envelope: ArtifactEnvelope = serde_json::from_value(wire.clone())
        .expect("genericRules is a declared policy payload member");
    assert!(envelope.payload.unknown_fields().is_empty());
    assert_eq!(
        serde_json::to_value(envelope.payload.to_json_value()).expect("payload serializes"),
        wire["payload"]
    );
    assert_eq!(
        serde_json::to_value(envelope).expect("envelope serializes")["payload"],
        wire["payload"],
        "policy genericRules must survive the envelope wire round-trip"
    );

    let PolicyRoot::Access(access) = serde_json::from_value(wire["payload"].clone())
        .expect("strict AccessIR root validates genericRules")
    else {
        panic!("accessVersion selects AccessIR");
    };
    assert_eq!(access.generic_rules.len(), 1);
}

#[test]
fn customization_fixture_decodes_into_the_typed_root() {
    let customization: CustomizationRoot = assert_typed_roundtrip(
        "retail-sku-reorder-point.customization.json",
        &payload_json(CUSTOMIZATION_FIXTURE),
    );
    assert!(matches!(
        customization.overlay.operations[0],
        CustomizationOverlayOp::AddField { .. }
    ));

    let mut executable = payload_json(CUSTOMIZATION_FIXTURE);
    executable["overlay"]["operations"] = json!([{ "op": "custom_hook", "hookId": "reprice" }]);
    executable["executable"] = json!({
        "boundary": "capability_rpc",
        "inputSchema": { "type": "object" },
        "outputSchema": { "type": "object" },
        "errorSchema": { "type": "object" },
        "scopeCapabilities": ["retail.sku.read"],
        "requiredServices": [],
        "resourceLimits": {
            "maxWallTimeMs": 1000, "maxMemoryMb": 64.0, "maxNetworkCalls": 0, "maxStorageBytes": 0
        },
        "ambientCredentials": false
    });
    let customization: CustomizationRoot =
        assert_typed_roundtrip("customization with executable", &executable);
    let limits = customization
        .executable
        .expect("executable decoded")
        .resource_limits;
    assert_eq!(limits.max_memory_mb, WholeNumber(64));

    // Retired boundaries do not decode (ruling 2026-09-25 §11; D33): custom
    // code is a pool Function behind `perform`, not an inline executable.
    for boundary in ["trusted_rust", "capability_rpc"] {
        let mut admitted = executable.clone();
        admitted["executable"]["boundary"] = json!(boundary);
        assert!(decodes::<CustomizationRoot>(admitted), "{boundary}");
    }
    for boundary in ["effectful_ts", "wasm"] {
        let mut retired = executable.clone();
        retired["executable"]["boundary"] = json!(boundary);
        assert!(
            !decodes::<CustomizationRoot>(retired),
            "{boundary} must not decode"
        );
    }
}

#[test]
fn view_fixture_decodes_into_the_typed_root() {
    let view: ViewIrPayload = assert_typed_roundtrip(
        "retail-low-stock.derived.json",
        &payload_json(DERIVED_FIXTURE),
    );
    assert_eq!(view.inputs[0].kind, ViewInputKind::Entity);
    assert_eq!(
        view.scope_required,
        Some(vec!["orgId".to_owned(), "environmentId".to_owned()])
    );
    // `scopeRequired` is typed as scope dimensions (T1-IR-2).
    let mut malformed = payload_json(DERIVED_FIXTURE);
    malformed["scopeRequired"] = json!("orgId");
    assert!(!decodes::<ViewIrPayload>(malformed));
}

/// Members the validator lets you omit but rejects when present as `null`
/// decode the same way: omitted is valid, explicit `null` is an error.
#[test]
fn optional_members_may_be_omitted_but_not_null() {
    let cases: [(&str, &[&str], &str, Decode); 7] = [
        ("connector", &[], "cursor", decodes::<ConnectorRoot>),
        (
            "connector",
            &[],
            "deadLetterPolicy",
            decodes::<ConnectorRoot>,
        ),
        (
            "connector",
            &[],
            "customStepHookIds",
            decodes::<ConnectorRoot>,
        ),
        ("widget", &[], "projectionHints", decodes::<WidgetRoot>),
        ("widget", &[], "proposal", decodes::<WidgetRoot>),
        ("widget", &[], "ux", decodes::<WidgetRoot>),
        (
            "widget",
            &["projectionHints"],
            "renderer",
            decodes::<WidgetRoot>,
        ),
    ];
    for (kind, parents, member, decode) in cases {
        let source = if kind == "connector" {
            CONNECTOR_FIXTURE
        } else {
            WIDGET_FIXTURE
        };
        let mut omitted = payload_json(source);
        let mut target = &mut omitted;
        for parent in parents {
            target = &mut target[*parent];
        }
        target
            .as_object_mut()
            .expect("parent is an object")
            .remove(member);
        assert!(decode(omitted.clone()), "{kind} {member} omitted");

        let mut null = omitted;
        let mut target = &mut null;
        for parent in parents {
            target = &mut target[*parent];
        }
        target[member] = Value::Null;
        assert!(!decode(null), "{kind} {member} = null must be rejected");
    }

    let mut binding = payload_json(WIDGET_FIXTURE);
    binding["actionBindings"][0]["actionVersion"] = Value::Null;
    assert!(!decodes::<WidgetRoot>(binding));

    let mut view = payload_json(DERIVED_FIXTURE);
    view["temporalScope"] = Value::Null;
    assert!(!decodes::<ViewIrPayload>(view));

    // The formerly unchecked members are typed now (T1-IR-2): omitted is
    // valid, `null` is not.
    let mut view = payload_json(DERIVED_FIXTURE);
    view.as_object_mut()
        .expect("payload object")
        .remove("scopeRequired");
    assert!(decodes::<ViewIrPayload>(view.clone()));
    view["scopeRequired"] = Value::Null;
    assert!(!decodes::<ViewIrPayload>(view));

    // The obligations form of an Access policy keeps the approval-form
    // members optional, with their approval-form types.
    let policy = payload_json(POLICY_FIXTURE);
    for (member, valid, invalid) in [
        ("appliesTo", json!("purchase_order"), json!(7)),
        ("reviewRequiredWhen", json!("amount > 1000"), Value::Null),
        ("approvalRoles", json!(["procurement_manager"]), json!("x")),
    ] {
        let mut candidate = policy.clone();
        candidate[member] = valid;
        let decoded: PolicyRoot = assert_typed_roundtrip(member, &candidate);
        assert!(matches!(decoded, PolicyRoot::Obligations(_)), "{member}");
        candidate[member] = invalid;
        assert!(!decodes::<PolicyRoot>(candidate), "{member}");
    }
}

#[test]
fn whole_numbers_accept_integral_values_only() {
    for (value, ok) in [
        (json!(3), true),
        (json!(3.0), true),
        (json!(0), true),
        (json!(1.5), false),
        (json!(-1), false),
        (json!("3"), false),
    ] {
        assert_eq!(
            serde_json::from_value::<WholeNumber>(value.clone()).is_ok(),
            ok,
            "{value}"
        );
    }
}

#[test]
fn typed_roots_are_closed_at_the_root() {
    let roots: [(&str, Decode); 5] = [
        (CONNECTOR_FIXTURE, decodes::<ConnectorRoot>),
        (WIDGET_FIXTURE, decodes::<WidgetRoot>),
        (POLICY_FIXTURE, decodes::<PolicyRoot>),
        (CUSTOMIZATION_FIXTURE, decodes::<CustomizationRoot>),
        (DERIVED_FIXTURE, decodes::<ViewIrPayload>),
    ];
    for (source, decode) in roots {
        let mut payload = payload_json(source);
        assert!(decode(payload.clone()));
        payload["processId"] = json!("order_fulfillment");
        assert!(!decode(payload), "unknown root member must be rejected");
    }
}

#[test]
fn null_extensions_are_rejected_by_the_typed_roots() {
    let roots: [(&str, Decode); 5] = [
        (CONNECTOR_FIXTURE, decodes::<ConnectorRoot>),
        (WIDGET_FIXTURE, decodes::<WidgetRoot>),
        (POLICY_FIXTURE, decodes::<PolicyRoot>),
        (CUSTOMIZATION_FIXTURE, decodes::<CustomizationRoot>),
        (DERIVED_FIXTURE, decodes::<ViewIrPayload>),
    ];
    for (source, decode) in roots {
        let mut payload = payload_json(source);
        payload["extensions"] = json!({ "example.com/audit/1.0.0": { "enabled": true } });
        assert!(decode(payload.clone()));
        payload["extensions"] = Value::Null;
        assert!(!decode(payload), "extensions: null must be rejected");
    }
}

#[test]
fn orders_customer_widget_ux_fixture_decodes_and_roundtrips() {
    let source = include_str!("../../../fixtures/ir/ux-orders-customer.widget.json");
    let payload = payload_json(source);
    let widget: WidgetRoot = assert_typed_roundtrip("ux-orders-customer.widget.json", &payload);
    let ux = widget
        .ux
        .expect("demo widget carries its typed UX contract");
    assert_eq!(ux.pages.len(), 2);
    assert_eq!(ux.data_bindings.len(), 3);
    assert_eq!(
        ux.forms[0].input_names,
        vec!["orderId".to_owned(), "reason".to_owned()]
    );
}

#[test]
fn widget_ui_law_members_are_closed_and_versioned() {
    let mut payload = widget_ux_payload();
    payload["ux"]["pages"][0]["root"]["handlers"] = json!([{
        "kind": "outcome_surface", "states": ["pending", "success", "failure", "unknown_outcome"],
        "currencyRefs": ["currency.USD"]
    }]);
    payload["ux"]["actionPlacements"][0]["retry"] = json!(true);
    payload["ux"]["actionPlacements"][0]["handlers"] = json!([{
        "kind": "error_banner", "codes": ["Denied"]
    }]);
    payload["ux"]["forms"][0]["submit"]["handlers"] = json!([{
        "kind": "result_surface", "codes": ["Saved"]
    }]);
    payload["ux"]["shellHandlers"] = json!({
        "handlersVersion": "0.1", "handlers": [{ "kind": "error_banner", "codes": ["Denied"] }]
    });
    assert_typed_roundtrip::<WidgetRoot>("Widget UI handlers", &payload);

    for invalid in [
        json!({ "kind": "unchecked_handler", "codes": ["Denied"] }),
        json!({ "kind": "error_banner", "codes": ["Denied"], "unchecked": true }),
        json!({ "kind": "outcome_surface", "states": ["cancelled"] }),
    ] {
        let mut malformed = payload.clone();
        malformed["ux"]["pages"][0]["root"]["handlers"] = json!([invalid]);
        assert!(!decodes::<WidgetRoot>(malformed));
    }
    let mut malformed = payload.clone();
    malformed["ux"]["shellHandlers"]["handlersVersion"] = json!("0.2");
    assert!(!decodes::<WidgetRoot>(malformed));
    let mut malformed = payload.clone();
    malformed["ux"]["shellHandlers"] = Value::Null;
    assert!(!decodes::<WidgetRoot>(malformed));
    let mut malformed = payload;
    malformed["ux"]["actionPlacements"][0]["retry"] = json!("yes");
    assert!(!decodes::<WidgetRoot>(malformed));
}

#[test]
fn dialog_ui_law_shape_is_closed() {
    let mut payload = widget_ux_payload();
    payload["ux"]["pages"][0]["root"]["component"] = json!({
        "kind": "dialog", "blocking": true, "bindingId": "invoke", "dismissalMethods": ["escape"]
    });
    assert_typed_roundtrip::<WidgetRoot>("Widget dialog", &payload);
    let mut malformed = payload.clone();
    malformed["ux"]["pages"][0]["root"]["component"]["dismissalMethods"] = json!(["cancel"]);
    assert!(!decodes::<WidgetRoot>(malformed));
    let mut malformed = payload;
    malformed["ux"]["pages"][0]["root"]["component"]["bindingId"] = Value::Null;
    assert!(!decodes::<WidgetRoot>(malformed));
}
