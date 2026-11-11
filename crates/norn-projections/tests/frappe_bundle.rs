#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs,
    reason = "integration-test crate"
)]

//! Norn object types → Frappe projection bundle (#1085 B3): goldens,
//! idempotent re-generation, roles from access rules, and how a definition
//! change shows up in Frappe.

#[path = "support/frappe_defs.rs"]
mod frappe_defs;

use std::path::Path;

use frappe_defs::Definitions;
use norn_ir::ObjectTypeDef;
use norn_projections::{
    FRAPPE_BUNDLE_DOMAIN, FrappeProjectionError, FrappeProjectionInput, doctype_name, frappe_role,
    project_frappe_bundle,
};
use serde_json::{Value, json};

const UPDATE_ENV: &str = "NORN_PROJECTIONS_UPDATE_GOLDENS";
const TYPES: [&str; 3] = ["canon.Item", "canon.Party", "core.analytics.Restatement"];
const READERS: [&str; 4] = ["owner", "admin", "member", "viewer"];

fn three_systems() -> Definitions {
    frappe_defs::decode(include_str!(
        "../../../fixtures/demo/explorer/three-systems.definitions.json"
    ))
    .expect("the demo set decodes")
}

fn project(defs: &Definitions) -> Result<Value, FrappeProjectionError> {
    let selected: Vec<&ObjectTypeDef> = TYPES
        .iter()
        .map(|name| {
            defs.object_types
                .iter()
                .find(|def| def.name.as_str() == *name)
                .expect("the type is in the set")
        })
        .collect();
    let actions: Vec<_> = defs.actions.iter().collect();
    let access: Vec<_> = defs.access.iter().collect();
    let bundle = project_frappe_bundle(&FrappeProjectionInput {
        object_types: &selected,
        actions: &actions,
        access: &access,
        reader_roles: &READERS,
    })?;
    let canonical = norn_ir::canonical::canonical_json_of(&bundle)
        .map_err(|error| FrappeProjectionError::Canonical(error.to_string()))?;
    serde_json::from_slice(&canonical)
        .map_err(|error| FrappeProjectionError::Canonical(error.to_string()))
}

fn doctype<'a>(bundle: &'a Value, name: &str) -> &'a Value {
    bundle["doctypes"]
        .as_array()
        .expect("doctypes")
        .iter()
        .find(|doctype| doctype["name"] == name)
        .expect("the DocType is projected")
}

fn fieldnames(doctype: &Value) -> Vec<&str> {
    doctype["fields"]
        .as_array()
        .expect("fields")
        .iter()
        .map(|field| field["fieldname"].as_str().expect("fieldname"))
        .collect()
}

/// The demo set's bundle is the checked-in golden. Regenerate with
/// `NORN_PROJECTIONS_UPDATE_GOLDENS=1`.
#[test]
fn three_systems_bundle_matches_its_golden() {
    let text = serde_json::to_string_pretty(&project(&three_systems()).expect("projects"))
        .expect("prints")
        + "\n";
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/projections/frappe/three-systems.bundle.json");
    if std::env::var_os(UPDATE_ENV).is_some() {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, &text).expect("write golden");
    }
    assert_eq!(text, std::fs::read_to_string(&path).expect("golden exists"));
}

/// Re-generating unchanged definitions gives the same bytes, and the hash
/// commits to the rest of the bundle.
#[test]
fn regeneration_is_idempotent_and_the_hash_commits_to_the_content() {
    let defs = three_systems();
    let first = project(&defs).expect("projects");
    assert_eq!(first, project(&defs).expect("projects again"));
    let mut body = first;
    let hash = body
        .as_object_mut()
        .expect("object")
        .remove("bundleHash")
        .expect("hash");
    let recomputed =
        norn_ir::canonical::domain_hash_hex(FRAPPE_BUNDLE_DOMAIN, &body).expect("hash");
    assert_eq!(hash, json!(recomputed));
}

/// A transition becomes a Workflow row only for the roles an access rule
/// grants the action that fires it; a transition nobody may send has none.
#[test]
fn workflow_roles_come_from_the_access_rules() {
    let bundle = project(&three_systems()).expect("projects");
    let workflow = &bundle["workflows"][0];
    assert_eq!(workflow["document_type"], "Core Analytics Restatement");
    assert_eq!(workflow["workflow_state_field"], "status");
    let rows: Vec<(&str, &str, &str, &str)> = workflow["transitions"]
        .as_array()
        .expect("transitions")
        .iter()
        .map(|row| {
            (
                row["state"].as_str().expect("state"),
                row["action"].as_str().expect("action"),
                row["next_state"].as_str().expect("next"),
                row["allowed"].as_str().expect("role"),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            ("ready", "Approve", "approved", "Norn Analytics Reviewer"),
            ("approved", "Activate", "active", "Norn Analytics Reviewer"),
        ]
    );
    let entry = &bundle["objectTypes"]["Core Analytics Restatement"];
    assert_eq!(
        entry["transitions"]["ready->approved"],
        "core.analytics.restatement.approve"
    );
    assert!(entry["transitions"].get("building->ready").is_none());
    let perms = &doctype(&bundle, "Core Analytics Restatement")["permissions"];
    assert!(
        perms
            .as_array()
            .expect("perms")
            .iter()
            .any(|perm| perm["role"] == "Norn Analytics Reviewer" && perm["write"] == 1)
    );
    assert_eq!(
        bundle["principal"]["roleMap"],
        json!([
            ["Norn Owner", "owner"],
            ["Norn Admin", "admin"],
            ["Norn Member", "member"],
            ["Norn Viewer", "viewer"]
        ])
    );
}

/// Adding a property and a state in Norn changes the generated `DocType` and
/// Workflow, and the bundle hash; nothing is edited on the Frappe side.
#[test]
fn a_definition_change_regenerates_the_frappe_records() {
    let before = project(&three_systems()).expect("projects");
    let mut defs = three_systems();
    let item = defs
        .object_types
        .iter_mut()
        .find(|def| def.name.as_str() == "canon.Item")
        .expect("item");
    item.properties.push(
        serde_json::from_value(json!({"id": "weight_g", "type": {"kind": "decimal", "additivity": {"kind": "additive"}, "scale": 3}, "constraints": {"nullable": true}}))
            .expect("property"),
    );
    let restatement = defs
        .object_types
        .iter_mut()
        .find(|def| def.name.as_str() == "core.analytics.Restatement")
        .expect("restatement");
    let status = restatement
        .properties
        .iter_mut()
        .find(|property| property.id.as_str() == "status")
        .expect("status");
    if let norn_ir::PropertyType::Enum { values } = &mut status.property_type {
        values.push(serde_json::from_value(json!({"value": "archived"})).expect("value"));
    }
    restatement.lifecycles[0].transitions.push(
        serde_json::from_value(json!({"id": "archive", "from": ["active"], "to": "archived"}))
            .expect("transition"),
    );
    let after = project(&defs).expect("projects");

    assert_ne!(before["bundleHash"], after["bundleHash"]);
    assert!(!fieldnames(doctype(&before, "Canon Item")).contains(&"weight_g"));
    assert!(fieldnames(doctype(&after, "Canon Item")).contains(&"weight_g"));
    let options = |bundle: &Value| {
        doctype(bundle, "Core Analytics Restatement")["fields"]
            .as_array()
            .expect("fields")
            .iter()
            .find(|field| field["fieldname"] == "status")
            .expect("status")["options"]
            .clone()
    };
    assert_eq!(options(&before), "building\nready\napproved\nactive");
    assert_eq!(
        options(&after),
        "building\nready\napproved\nactive\narchived"
    );
    let state_count = |bundle: &Value| {
        bundle["workflows"][0]["states"]
            .as_array()
            .expect("states")
            .len()
    };
    assert_eq!((state_count(&before), state_count(&after)), (4, 5));
}

#[test]
fn two_transitions_between_the_same_states_are_refused() {
    let mut defs = three_systems();
    let restatement = defs
        .object_types
        .iter_mut()
        .find(|def| def.name.as_str() == "core.analytics.Restatement")
        .expect("restatement");
    restatement.lifecycles[0].transitions.push(
        serde_json::from_value(json!({"id": "fast_track", "from": ["ready"], "to": "approved"}))
            .expect("transition"),
    );
    assert_eq!(
        project(&defs).expect_err("refused"),
        FrappeProjectionError::AmbiguousTransition {
            object_type: "core.analytics.Restatement".into(),
            from: "ready".into(),
            to: "approved".into(),
        }
    );
}

#[test]
fn names_avoid_native_doctypes_and_reserved_fields() {
    assert_eq!(doctype_name("canon.Item"), "Canon Item");
    assert_eq!(doctype_name("canon.OrderLine"), "Canon Order Line");
    assert_eq!(
        doctype_name("core.analytics.Restatement"),
        "Core Analytics Restatement"
    );
    assert_eq!(frappe_role("AnalyticsReviewer"), "Norn Analytics Reviewer");
    assert_eq!(frappe_role("viewer"), "Norn Viewer");
    let def: ObjectTypeDef = serde_json::from_value(json!({
        "name": "demo.Thing",
        "properties": [
            {"id": "id", "type": {"kind": "text"}},
            {"id": "name", "type": {"kind": "text"}},
            {"id": "owner", "type": {"kind": "text"}}
        ],
        "identity": [{"name": "primary", "properties": ["id"]}]
    }))
    .expect("type");
    let bundle = project_frappe_bundle(&FrappeProjectionInput {
        object_types: &[&def],
        actions: &[],
        access: &[],
        reader_roles: &["viewer"],
    })
    .expect("projects");
    let canonical = norn_ir::canonical::canonical_json_of(&bundle).expect("canonical");
    let bundle: Value = serde_json::from_slice(&canonical).expect("parses");
    assert_eq!(
        fieldnames(doctype(&bundle, "Demo Thing")),
        ["norn_name", "norn_owner"]
    );
    let entry = &bundle["objectTypes"]["Demo Thing"];
    assert_eq!(entry["idProperty"], "id");
    assert_eq!(
        entry["fields"],
        json!({"norn_name": "name", "norn_owner": "owner"})
    );
    assert_eq!(doctype(&bundle, "Demo Thing")["title_field"], "norn_name");
}
