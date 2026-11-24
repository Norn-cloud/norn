//! Security #1376 regression suite for Ring-1 Widget UX admission: widgets must
//! not bind to extension properties or links outside their own or broader scope.

use std::collections::BTreeSet;

use norn_checker::EntityArtifactSetContext;
use norn_ir::ArtifactEnvelope;
use norn_ui_checker::check_artifact_set;
use serde_json::{Value, json};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const WIDGET_FIXTURE: &str = include_str!("../../../fixtures/ir/ux-orders-customer.widget.json");

fn decode(value: Value) -> Result<ArtifactEnvelope, Box<dyn std::error::Error>> {
    Ok(serde_json::from_value(value)?)
}

fn entity(
    artifact_id: &str,
    object_name: &str,
    properties: &Value,
    identity: &str,
    links: &Value,
) -> Result<ArtifactEnvelope, Box<dyn std::error::Error>> {
    let namespace = artifact_id.split('.').next().unwrap_or("commerce");
    decode(json!({
        "irVersion": "0.1",
        "artifactId": artifact_id,
        "kind": "entity",
        "namespace": namespace,
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
    }))
}

fn base_artifacts() -> Result<Vec<ArtifactEnvelope>, Box<dyn std::error::Error>> {
    let order = entity(
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
    )?;
    let customer = entity(
        "crm.Customer.entity",
        "crm.Customer",
        &json!([
            { "id": "customer_number", "type": { "kind": "text" } },
            { "id": "display_name", "type": { "kind": "text" } },
            { "id": "revision", "type": { "kind": "text" } }
        ]),
        "customer_number",
        &json!([]),
    )?;
    let action = decode(json!({
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
                    "reason": { "type": "string" }
                },
                "required": ["orderId"],
                "additionalProperties": false
            },
            "outputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
            "writes": []
        }
    }))?;
    Ok(vec![order, customer, action])
}

fn order_extension(scope: &Value) -> Result<ArtifactEnvelope, Box<dyn std::error::Error>> {
    decode(json!({
        "irVersion": "0.1",
        "artifactId": "acme.Order.extension",
        "kind": "entity",
        "namespace": "acme",
        "name": "Order extension",
        "version": "0.1.0",
        "lifecycle": "draft",
        "scope": scope,
        "dependencies": [],
        "provenance": { "source": "authoring_sdk" },
        "payload": {
            "entityVersion": "2.0",
            "objectTypeExtension": {
                "namespace": "acme",
                "target": "commerce.Order",
                "properties": [{
                    "id": "acme__internal_note",
                    "type": { "kind": "text" },
                    "constraints": { "nullable": true }
                }]
            }
        }
    }))
}

fn widget_displaying_extension(
    scope: Value,
) -> Result<ArtifactEnvelope, Box<dyn std::error::Error>> {
    let mut doc: Value = serde_json::from_str(WIDGET_FIXTURE)?;
    if let Some(root) = doc.as_object_mut() {
        root.insert("scope".to_owned(), scope);
    }
    for pointer in [
        "/payload/ux/fields/1/target/property/propertyId",
        "/payload/ux/pages/0/root/children/0/component/fields/1/propertyId",
    ] {
        let field = doc
            .pointer_mut(pointer)
            .ok_or_else(|| format!("fixture missing {pointer}"))?;
        *field = json!("acme__internal_note");
    }
    decode(doc)
}

#[test]
fn widget_cannot_bind_out_of_scope_extension_property() -> TestResult {
    let ext_scope = json!({
        "orgId": "org_demo",
        "environmentId": "dev",
        "tenantId": "tenant_alpha"
    });
    let broader_scope = json!({
        "orgId": "org_demo",
        "environmentId": "dev"
    });
    let sibling_scope = json!({
        "orgId": "org_demo",
        "environmentId": "dev",
        "tenantId": "tenant_beta"
    });
    let narrower_scope = json!({
        "orgId": "org_demo",
        "environmentId": "dev",
        "tenantId": "tenant_alpha",
        "workspaceId": "ws_1"
    });

    let ext = order_extension(&ext_scope)?;
    let shared = base_artifacts()?;

    for in_base in [false, true] {
        let mut base = shared.clone();
        if in_base {
            base.push(ext.clone());
        }
        let check = |scope: Value| -> Result<Vec<_>, Box<dyn std::error::Error>> {
            let widget = widget_displaying_extension(scope)?;
            let candidates = if in_base {
                vec![widget]
            } else {
                vec![ext.clone(), widget]
            };
            let findings = check_artifact_set(
                &candidates,
                &EntityArtifactSetContext {
                    registry: &BTreeSet::new(),
                    base: &base,
                    tenant: None,
                },
            );
            Ok(findings
                .into_iter()
                .filter(|d| {
                    d.is_error() && d.artifact_id.as_deref() == Some("commerce.widget.orders")
                })
                .collect())
        };

        let f_broader = check(broader_scope.clone())?;
        assert!(
            !f_broader.is_empty(),
            "in_base={in_base}: broader widget must not bind narrower extension property: {f_broader:#?}"
        );

        let f_sibling = check(sibling_scope.clone())?;
        assert!(
            !f_sibling.is_empty(),
            "in_base={in_base}: sibling widget must not bind sibling extension property: {f_sibling:#?}"
        );

        let f_same = check(ext_scope.clone())?;
        assert!(
            f_same.is_empty(),
            "in_base={in_base}: same-scope widget must bind visible extension property cleanly: {f_same:#?}"
        );

        let f_narrower = check(narrower_scope.clone())?;
        assert!(
            f_narrower.is_empty(),
            "in_base={in_base}: narrower widget must bind broader extension property cleanly: {f_narrower:#?}"
        );
    }
    Ok(())
}
