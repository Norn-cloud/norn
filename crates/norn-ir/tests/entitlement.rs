//! Canonical entitlement wire shape and fixed generic policy contracts.

use norn_ir::entitlement::{
    Entitlement, EntitlementCondition, EntitlementGrantee, EntitlementStatus, ObjectRef,
};
use norn_ir::{AccessIr, ObjectTypeName};
use serde_json::{Value, json};

#[test]
fn entitlement_v01_uses_the_closed_typed_wire_shape() {
    let wire = json!({
        "entitlement_version": "0.1",
        "id": "erpnext:DocPerm:42",
        "grantee": {
            "kind": "role",
            "objectType": "identity.Role",
            "key": "accounts_manager"
        },
        "target_type": "erpnext.SalesInvoice",
        "actions": ["erpnext.sales_invoice.read"],
        "scope_attributes": { "company": "Acme" },
        "condition": { "kind": "ownerOnly", "link": "owner" },
        "source": {
            "system": "erpnext",
            "recordId": "DocPerm/42",
            "version": "17.0"
        },
        "status": "shadow"
    });
    let row: Entitlement = serde_json::from_value(wire.clone()).expect("entitlement decodes");
    assert_eq!(
        row.grantee,
        EntitlementGrantee::Role {
            object_type: ObjectTypeName("identity.Role".to_owned()),
            key: "accounts_manager".to_owned(),
        }
    );
    assert_eq!(row.status, EntitlementStatus::Shadow);
    assert_eq!(
        row.condition,
        EntitlementCondition::OwnerOnly {
            link: norn_ir::LinkId("owner".to_owned()),
        }
    );
    assert_eq!(
        serde_json::to_value(row).expect("entitlement serializes"),
        wire
    );
}

#[test]
fn entitlement_linked_record_is_a_typed_object_reference() {
    let row: Entitlement = serde_json::from_value(json!({
        "entitlement_version": "0.1",
        "id": "erpnext:UserPermission:9",
        "grantee": {
            "kind": "principal",
            "objectType": "party.Person",
            "key": "person-1"
        },
        "target_type": "erpnext.SalesInvoice",
        "actions": ["erpnext.sales_invoice.read"],
        "scope_attributes": {},
        "condition": {
            "kind": "linkedRecord",
            "link": "customer",
            "target": { "objectType": "erpnext.Customer", "key": "customer-3" }
        },
        "source": {
            "system": "erpnext",
            "recordId": "UserPermission/9",
            "version": "17.0"
        },
        "status": "enforced"
    }))
    .expect("entitlement decodes");
    assert_eq!(
        row.condition,
        EntitlementCondition::LinkedRecord {
            link: norn_ir::LinkId("customer".to_owned()),
            target: ObjectRef {
                object_type: ObjectTypeName("erpnext.Customer".to_owned()),
                key: "customer-3".to_owned(),
            },
        }
    );
}

#[test]
fn entitlement_conditions_and_generic_access_selectors_are_closed() {
    let bad_condition: Result<Entitlement, _> = serde_json::from_value(json!({
        "entitlement_version": "0.1",
        "id": "x",
        "grantee": { "kind": "role", "objectType": "identity.Role", "key": "r" },
        "target_type": "erpnext.Invoice",
        "actions": ["erpnext.invoice.read"],
        "condition": { "kind": "arbitraryCode", "expression": "true" },
        "source": { "system": "erpnext", "recordId": "x", "version": "1" },
        "status": "enforced"
    }));
    assert!(bad_condition.is_err());

    let generic = json!({
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
    });
    let access: AccessIr = serde_json::from_value(generic).expect("generic rule decodes");
    assert_eq!(access.generic_rules.len(), 1);

    let bad_selector: Value = json!({
        "accessVersion": "0.1",
        "policyId": "core.entitlements",
        "rules": [],
        "genericRules": [{
            "ruleId": "bad",
            "target": { "kind": "arbitraryType", "type": "erpnext.Invoice" },
            "actions": { "kind": "allAdmittedActions" },
            "scope": { "dimensions": ["org"] },
            "allOf": [{ "entitlement": { "kind": "none" } }]
        }]
    });
    assert!(serde_json::from_value::<AccessIr>(bad_selector).is_err());
}
