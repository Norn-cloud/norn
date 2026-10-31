#![allow(
    clippy::expect_used,
    clippy::panic,
    missing_docs,
    reason = "integration-test assertions"
)]

//! Interface definitions are semantic model data for commit admission, not
//! projected SQLite schema.

use std::collections::BTreeMap;

use norn_cell::{BindingIndexSpec, Model, ModelDefinitionSnapshot};
use norn_ir::{InterfaceDef, ObjectTypeDef, PropertyTypeDef};
use serde::Serialize;
use serde_json::json;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct LegacyReplayDefinitions<'a> {
    object_types: BTreeMap<&'a str, &'a ObjectTypeDef>,
    property_types: BTreeMap<&'a str, &'a PropertyTypeDef>,
}

fn object_type() -> ObjectTypeDef {
    serde_json::from_value(json!({
        "name": "model.Item",
        "implements": ["model.Named"],
        "properties": [{"id": "name", "type": {"kind": "text"}}]
    }))
    .expect("object type")
}

fn interface(min_length: u64) -> InterfaceDef {
    serde_json::from_value(json!({
        "name": "model.Named",
        "properties": [{
            "id": "name",
            "type": {"kind": "text"},
            "constraints": {"minLength": min_length}
        }]
    }))
    .expect("interface")
}

fn invariant_interface(min_length: u64, expression: &str) -> InterfaceDef {
    serde_json::from_value(json!({
        "name": "model.Named",
        "properties": [{
            "id": "name",
            "type": {"kind": "text"},
            "constraints": {"minLength": min_length}
        }],
        "invariants": [{
            "id": "non_empty",
            "expression": expression,
            "message": "name must be non-empty"
        }]
    }))
    .expect("invariant interface")
}

fn legacy_model_hash(object: &ObjectTypeDef) -> String {
    let definitions = LegacyReplayDefinitions {
        object_types: BTreeMap::from([(object.name.as_str(), object)]),
        property_types: BTreeMap::new(),
    };
    let bytes = norn_ir::canonical::canonical_json_of(&definitions).expect("canonical definitions");
    norn_ir::canonical::HashProfile::Norn1.hash_canonical_bytes(&bytes)
}

#[test]
fn empty_interfaces_preserve_legacy_snapshot_and_model_hash() {
    let object = object_type();
    let old_snapshot = json!({
        "objectTypes": [object],
        "propertyTypes": []
    });
    let snapshot: ModelDefinitionSnapshot =
        serde_json::from_value(old_snapshot.clone()).expect("legacy snapshot decodes");
    assert!(snapshot.interfaces.is_empty());
    assert_eq!(
        serde_json::to_value(&snapshot).expect("snapshot encodes"),
        old_snapshot
    );

    let model = Model::from_snapshot(&snapshot).expect("legacy model rebuilds");
    assert_eq!(model.interfaces().count(), 0);
    assert_eq!(model.model_hash(), legacy_model_hash(&object_type()));
}

#[test]
fn interfaces_without_invariants_preserve_legacy_hash_but_remain_in_snapshots() {
    let object = object_type();
    let first_interface = interface(2);
    let changed_interface = interface(3);
    let first = Model::from_definitions_with_interfaces(
        std::slice::from_ref(&object),
        &[],
        std::slice::from_ref(&first_interface),
    )
    .expect("first model");
    let changed = Model::from_definitions_with_interfaces(
        std::slice::from_ref(&object),
        &[],
        std::slice::from_ref(&changed_interface),
    )
    .expect("changed model");

    assert_eq!(first.ddl_hash(), changed.ddl_hash());
    assert_eq!(first.model_hash(), legacy_model_hash(&object));
    assert_eq!(changed.model_hash(), legacy_model_hash(&object));
    assert_eq!(
        first.interfaces().cloned().collect::<Vec<_>>(),
        vec![first_interface]
    );
    assert_eq!(first.definitions_snapshot().interfaces, vec![interface(2)]);
}

#[test]
fn invariant_bearing_interface_changes_model_hash_without_changing_ddl_and_survives_restore() {
    let object = object_type();
    let first_interface = invariant_interface(2, "self.name != \"\"");
    let changed_invariant = invariant_interface(2, "self.name == \"valid\"");
    let changed_metadata = invariant_interface(3, "self.name != \"\"");
    let first = Model::from_definitions_with_interfaces(
        std::slice::from_ref(&object),
        &[],
        std::slice::from_ref(&first_interface),
    )
    .expect("first model");
    let changed = Model::from_definitions_with_interfaces(
        std::slice::from_ref(&object),
        &[],
        &[changed_invariant],
    )
    .expect("changed invariant model");
    let metadata_changed =
        Model::from_definitions_with_interfaces(&[object], &[], &[changed_metadata])
            .expect("changed model");

    assert_eq!(first.ddl_hash(), changed.ddl_hash());
    assert_eq!(first.ddl_hash(), metadata_changed.ddl_hash());
    assert_ne!(first.model_hash(), changed.model_hash());
    assert_ne!(first.model_hash(), metadata_changed.model_hash());

    let snapshot = first.definitions_snapshot();
    assert_eq!(snapshot.interfaces, vec![first_interface]);
    let encoded = serde_json::to_value(&snapshot).expect("snapshot encodes");
    assert!(encoded.get("interfaces").is_some());
    let decoded: ModelDefinitionSnapshot =
        serde_json::from_value(encoded).expect("snapshot decodes");
    let restored = Model::from_snapshot(&decoded).expect("model restores");
    assert_eq!(
        restored.interfaces().cloned().collect::<Vec<_>>(),
        vec![invariant_interface(2, "self.name != \"\"")]
    );
    assert_eq!(restored.model_hash(), first.model_hash());
}

#[test]
fn binding_indexes_survive_model_snapshot_and_legacy_snapshots_remain_index_free() {
    let object = object_type();
    let legacy =
        Model::from_definitions(std::slice::from_ref(&object), &[]).expect("legacy model projects");
    let indexed = Model::from_definitions_with_binding_indexes(
        std::slice::from_ref(&object),
        &[],
        &[],
        &[BindingIndexSpec {
            object_type: "model.Item".to_owned(),
            property_id: "name".to_owned(),
            sort: true,
            filter: true,
        }],
    )
    .expect("binding indexes project");
    assert_ne!(legacy.ddl_hash(), indexed.ddl_hash());
    assert_ne!(legacy.model_hash(), indexed.model_hash());

    let snapshot = indexed.definitions_snapshot();
    assert_eq!(snapshot.binding_indexes.len(), 1);
    let encoded = serde_json::to_value(&snapshot).expect("snapshot encodes");
    let decoded: ModelDefinitionSnapshot =
        serde_json::from_value(encoded).expect("snapshot decodes");
    let restored = Model::from_snapshot(&decoded).expect("indexed model restores");
    let statements = restored
        .schema()
        .object_tables
        .iter()
        .flat_map(|table| table.statements.iter())
        .collect::<Vec<_>>();
    assert!(
        statements
            .iter()
            .any(|statement| statement.contains("binding-property/name/asc"))
    );
    assert!(
        statements
            .iter()
            .any(|statement| statement.contains("binding-property/name/desc"))
    );
    assert_eq!(restored.ddl_hash(), indexed.ddl_hash());
    assert_eq!(restored.model_hash(), indexed.model_hash());

    let old_snapshot = json!({
        "objectTypes": [object_type()],
        "propertyTypes": []
    });
    let decoded_old: ModelDefinitionSnapshot =
        serde_json::from_value(old_snapshot.clone()).expect("legacy snapshot decodes");
    assert!(decoded_old.binding_indexes.is_empty());
    assert_eq!(
        serde_json::to_value(&decoded_old).expect("legacy snapshot encodes"),
        old_snapshot
    );
    let restored_old = Model::from_snapshot(&decoded_old).expect("legacy model restores");
    assert_eq!(restored_old.ddl_hash(), legacy.ddl_hash());
    assert_eq!(restored_old.model_hash(), legacy.model_hash());
}

#[test]
fn invariant_free_child_ancestry_changes_hash_and_survives_snapshot_reconstruction() {
    let object = object_type();
    let mut first = invariant_interface(1, "self.name != \"\"");
    first.name = norn_ir::InterfaceName("model.Parent".to_owned());
    let mut child = interface(1);
    let flat = Model::from_definitions_with_interfaces(
        std::slice::from_ref(&object),
        &[],
        &[child.clone(), first.clone()],
    )
    .expect("flat model");
    child.extends = vec![first.name.clone()];
    let inherited = Model::from_definitions_with_interfaces(&[object], &[], &[child, first])
        .expect("inherited model");
    assert_eq!(flat.ddl_hash(), inherited.ddl_hash());
    assert_ne!(flat.model_hash(), inherited.model_hash());
    assert!(inherited.is_a("model.Item", "model.Parent"));
    let restored =
        Model::from_snapshot(&inherited.definitions_snapshot()).expect("snapshot rebuilds");
    assert_eq!(restored.model_hash(), inherited.model_hash());
    assert!(restored.is_a("model.Item", "model.Parent"));
}
