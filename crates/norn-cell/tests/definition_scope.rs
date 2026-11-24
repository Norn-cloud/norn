//! Security #1376 regression suite: shared cell-scope and model binding
//! enforcement at install, check, reopen and the cell's scope binding.

use norn_cell::engine::{NativeEngine, SqlEngine};
use norn_cell::{CellError, CellStore, InstalledDefinitionInput, Model, StoreOptions};
use norn_ir::{ArtifactEnvelope, ObjectTypeDef};
use serde_json::{Value, json};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn entity_envelope(
    id: &str,
    name: &str,
    scope: &Value,
    extends: Option<&str>,
) -> Result<ArtifactEnvelope, Box<dyn std::error::Error>> {
    let mut object_type = json!({
        "name": name,
        "properties": [{ "id": "code", "type": { "kind": "text" } }],
        "identity": [{ "name": "primary", "properties": ["code"] }]
    });
    if let (Some(parent), Some(map)) = (extends, object_type.as_object_mut()) {
        map.insert("extends".to_owned(), json!(parent));
        map.insert("properties".to_owned(), json!([]));
        map.remove("identity");
    }
    Ok(serde_json::from_value(json!({
        "irVersion": "0.1",
        "artifactId": id,
        "kind": "entity",
        "namespace": "defs",
        "name": id,
        "version": "1.0.0",
        "lifecycle": "draft",
        "scope": scope,
        "dependencies": [],
        "provenance": { "source": "authoring_sdk" },
        "payload": {
            "entityVersion": "2.0",
            "objectType": object_type
        }
    }))?)
}

fn input_for(
    artifacts: &[ArtifactEnvelope],
) -> Result<InstalledDefinitionInput, Box<dyn std::error::Error>> {
    let canonical_set = String::from_utf8(norn_ir::canonical::canonical_json_of(artifacts)?)?;
    Ok(InstalledDefinitionInput {
        canonical_set,
        registry: vec![],
        receipt_sha256: "a".repeat(64),
        admission_receipt: None,
        readmission: None,
        diff: json!({}),
        installed_at: 1,
        layout: None,
    })
}

fn item_model() -> Result<Model, Box<dyn std::error::Error>> {
    let def: ObjectTypeDef = serde_json::from_value(json!({
        "name": "defs.Item",
        "properties": [{ "id": "code", "type": { "kind": "text" } }],
        "identity": [{ "name": "primary", "properties": ["code"] }]
    }))?;
    Ok(Model::from_definitions(&[def], &[])?)
}

#[test]
fn install_definitions_refuses_artifact_outside_cell_scope() -> TestResult {
    let model = item_model()?;
    let mut store = CellStore::open_in_memory(model.clone())?;

    // Different orgId must fail closed.
    let wrong_org = entity_envelope(
        "defs.Item.entity",
        "defs.Item",
        &json!({ "orgId": "org_other", "environmentId": "dev" }),
        None,
    )?;
    let err = store.install_definitions(
        model.clone(),
        input_for(std::slice::from_ref(&wrong_org))?,
        Some(0),
        "org_demo/dev",
    );
    assert!(
        matches!(&err, Err(CellError::DefinitionMigrationRefused(_))),
        "install_definitions must refuse wrong-org artifact: {err:?}"
    );
    let checked = store.check_definitions_in_scope(
        &model,
        &input_for(&[wrong_org])?,
        Some(0),
        "org_demo/dev",
    );
    assert!(
        matches!(&checked, Err(CellError::DefinitionMigrationRefused(_))),
        "check_definitions_in_scope must refuse wrong-org artifact: {checked:?}"
    );

    // Narrower tenant-scoped extension must fail closed against org_demo/dev cell scope.
    let valid_item = entity_envelope(
        "defs.Item.entity",
        "defs.Item",
        &json!({ "orgId": "org_demo", "environmentId": "dev" }),
        None,
    )?;
    let narrow_ext: ArtifactEnvelope = serde_json::from_value(json!({
        "irVersion": "0.1",
        "artifactId": "acme.ItemExt.entity",
        "kind": "entity",
        "namespace": "acme",
        "name": "ItemExt",
        "version": "1.0.0",
        "lifecycle": "draft",
        "scope": { "orgId": "org_demo", "environmentId": "dev", "tenantId": "tenant_alpha" },
        "dependencies": [],
        "provenance": { "source": "authoring_sdk" },
        "payload": {
            "entityVersion": "2.0",
            "objectTypeExtension": {
                "namespace": "acme",
                "target": "defs.Item",
                "properties": [{
                    "id": "acme__secret",
                    "type": { "kind": "text" },
                    "constraints": { "nullable": true }
                }]
            }
        }
    }))?;
    let ext_err = store.install_definitions(
        model.clone(),
        input_for(&[valid_item.clone(), narrow_ext])?,
        Some(0),
        "org_demo/dev",
    );
    assert!(
        matches!(&ext_err, Err(CellError::DefinitionMigrationRefused(_))),
        "install_definitions must refuse narrower tenant extension outside cell scope: {ext_err:?}"
    );

    // Same cell scope succeeds.
    store.install_definitions(model, input_for(&[valid_item])?, Some(0), "org_demo/dev")?;
    Ok(())
}

#[test]
fn install_definitions_refuses_cross_scope_supertype_inside_cell_set() -> TestResult {
    let super_def: ObjectTypeDef = serde_json::from_value(json!({
        "name": "defs.Base",
        "properties": [{ "id": "code", "type": { "kind": "text" } }],
        "identity": [{ "name": "primary", "properties": ["code"] }]
    }))?;
    let sub_def: ObjectTypeDef = serde_json::from_value(json!({
        "name": "defs.Child",
        "extends": "defs.Base",
        "properties": []
    }))?;
    let model = Model::from_definitions(&[super_def, sub_def], &[])?;
    let mut store = CellStore::open_in_memory(model.clone())?;

    // Both artifacts individually pass org_demo/dev cell visibility, but global defs.Child
    // extending org_demo/dev defs.Base is a cross-scope entity reference and must fail closed.
    let base_scoped = entity_envelope(
        "defs.Base.entity",
        "defs.Base",
        &json!({ "orgId": "org_demo", "environmentId": "dev" }),
        None,
    )?;
    let child_global = entity_envelope(
        "defs.Child.entity",
        "defs.Child",
        &json!({}),
        Some("defs.Base"),
    )?;
    let err = store.install_definitions(
        model,
        input_for(&[base_scoped, child_global])?,
        Some(0),
        "org_demo/dev",
    );
    assert!(
        matches!(&err, Err(CellError::DefinitionMigrationRefused(_))),
        "install_definitions must refuse cross-scope supertype edge inside canonical_set: {err:?}"
    );
    Ok(())
}

#[test]
fn definitions_are_checked_for_a_bound_scope_against_the_model_their_set_projects() -> TestResult {
    let model = item_model()?;
    let mut store = CellStore::open_in_memory(model.clone())?;
    let item = entity_envelope(
        "defs.Item.entity",
        "defs.Item",
        &json!({ "orgId": "org_demo", "environmentId": "dev" }),
        None,
    )?;
    let unbound =
        store.check_definitions(&model, &input_for(std::slice::from_ref(&item))?, Some(0));
    assert!(
        matches!(&unbound, Err(CellError::DefinitionMigrationRefused(_))),
        "a cell bound to no scope has no consumer to check for: {unbound:?}"
    );
    let widened: ObjectTypeDef = serde_json::from_value(json!({
        "name": "defs.Item",
        "properties": [
            { "id": "code", "type": { "kind": "text" } },
            { "id": "secret", "type": { "kind": "text" }, "constraints": { "nullable": true } }
        ],
        "identity": [{ "name": "primary", "properties": ["code"] }]
    }))?;
    let stray: ObjectTypeDef = serde_json::from_value(json!({
        "name": "defs.Stray",
        "properties": [{ "id": "code", "type": { "kind": "text" } }],
        "identity": [{ "name": "primary", "properties": ["code"] }]
    }))?;
    let items: ObjectTypeDef = serde_json::from_value(json!({
        "name": "defs.Item",
        "properties": [{ "id": "code", "type": { "kind": "text" } }],
        "identity": [{ "name": "primary", "properties": ["code"] }]
    }))?;
    for forged in [
        Model::from_definitions(&[widened], &[])?,
        Model::from_definitions(&[items, stray], &[])?,
    ] {
        let refused = store.install_definitions(
            forged,
            input_for(std::slice::from_ref(&item))?,
            Some(0),
            "org_demo/dev",
        );
        assert!(
            matches!(&refused, Err(CellError::DefinitionReceiptInvalid(_))),
            "a model other than its set's projection must not activate: {refused:?}"
        );
    }
    store.install_definitions(model, input_for(&[item])?, Some(0), "org_demo/dev")?;
    Ok(())
}

fn org_item() -> Result<ArtifactEnvelope, Box<dyn std::error::Error>> {
    entity_envelope(
        "defs.Item.entity",
        "defs.Item",
        &json!({ "orgId": "org_demo", "environmentId": "dev" }),
        None,
    )
}

#[test]
fn an_activated_cell_is_bound_to_its_install_scope() -> TestResult {
    let model = item_model()?;
    let mut store = CellStore::open_in_memory(model.clone())?;
    store.install_definitions(model, input_for(&[org_item()?])?, Some(0), "org_demo/dev")?;
    let rebound = store.bind("scope", "org_other/dev");
    assert!(
        matches!(&rebound, Err(CellError::BindingMismatch { .. })),
        "{rebound:?}"
    );
    Ok(())
}

#[test]
fn a_reopened_cell_serves_its_definitions_only_under_its_bound_scope() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("cell.sqlite");
    let model = item_model()?;
    let mut store = CellStore::open(&path, model.clone(), StoreOptions::default())?;
    store.install_definitions(
        model.clone(),
        input_for(&[org_item()?])?,
        Some(0),
        "org_demo/dev",
    )?;
    drop(store);
    drop(CellStore::open(
        &path,
        model.clone(),
        StoreOptions::default(),
    )?);
    NativeEngine::open(&path, StoreOptions::default())?
        .execute_batch(r#"DELETE FROM "_norn_meta" WHERE "key" = 'binding:scope'"#)?;
    let reopened = CellStore::open(&path, model, StoreOptions::default());
    assert!(
        matches!(&reopened, Err(CellError::DefinitionMigrationRefused(_))),
        "{:?}",
        reopened.err()
    );
    Ok(())
}

#[test]
fn only_the_exact_kernel_record_types_ride_beside_a_definition_set() -> TestResult {
    let item: ObjectTypeDef = serde_json::from_value(json!({
        "name": "defs.Item",
        "properties": [{ "id": "code", "type": { "kind": "text" } }],
        "identity": [{ "name": "primary", "properties": ["code"] }]
    }))?;
    let exact = norn_cell::kernel_record_types();
    let mut tampered = exact.clone();
    let action_call = tampered.first_mut().ok_or("action call record")?;
    action_call.properties.push(serde_json::from_value(
        json!({ "id": "exfiltrated", "type": { "kind": "text" }, "constraints": { "nullable": true } }),
    )?);
    for (records, admitted) in [(tampered, false), (exact, true)] {
        let mut types = vec![item.clone()];
        types.extend(records);
        let model = Model::from_definitions(&types, &[])?;
        let mut store = CellStore::open_in_memory(model.clone())?;
        let installed =
            store.install_definitions(model, input_for(&[org_item()?])?, Some(0), "org_demo/dev");
        assert_eq!(installed.is_ok(), admitted, "{:?}", installed.err());
    }
    Ok(())
}

#[test]
fn a_definition_set_with_an_extension_is_refused_at_the_cell() -> TestResult {
    let extension: ArtifactEnvelope = serde_json::from_value(json!({
        "irVersion": "0.1",
        "artifactId": "acme.ItemExt.entity",
        "kind": "entity",
        "namespace": "acme",
        "name": "ItemExt",
        "version": "1.0.0",
        "lifecycle": "draft",
        "scope": { "orgId": "org_demo", "environmentId": "dev" },
        "dependencies": [],
        "provenance": { "source": "authoring_sdk" },
        "payload": { "entityVersion": "2.0", "objectTypeExtension": {
            "namespace": "acme",
            "target": "defs.Item",
            "properties": [{ "id": "acme__note", "type": { "kind": "text" }, "constraints": { "nullable": true } }]
        } }
    }))?;
    let model = item_model()?;
    let mut store = CellStore::open_in_memory(model.clone())?;
    let refused = store.install_definitions(
        model,
        input_for(&[org_item()?, extension])?,
        Some(0),
        "org_demo/dev",
    );
    assert!(
        matches!(&refused, Err(CellError::DefinitionMigrationRefused(_))),
        "{refused:?}"
    );
    Ok(())
}

#[test]
fn a_definition_set_may_not_declare_a_kernel_record_type() -> TestResult {
    for name in [norn_kernel::ACTION_CALL_TYPE, "norn.actioncall"] {
        let mut record = norn_cell::kernel_record_types()
            .into_iter()
            .next()
            .ok_or("action call record")?;
        record.name = norn_ir::ObjectTypeName(name.to_owned());
        record.properties.push(serde_json::from_value(
            json!({ "id": "exfiltrated", "type": { "kind": "text" }, "constraints": { "nullable": true } }),
        )?);
        let declared: ArtifactEnvelope = serde_json::from_value(json!({
            "irVersion": "0.1",
            "artifactId": "defs.Calls.entity",
            "kind": "entity",
            "namespace": "defs",
            "name": "Calls",
            "version": "1.0.0",
            "lifecycle": "draft",
            "scope": { "orgId": "org_demo", "environmentId": "dev" },
            "dependencies": [],
            "provenance": { "source": "authoring_sdk" },
            "payload": { "entityVersion": "2.0", "objectType": record }
        }))?;
        let model = Model::from_definitions(&[record], &[])?;
        let mut store = CellStore::open_in_memory(model.clone())?;
        let input = input_for(std::slice::from_ref(&declared))?;
        let checked = store.check_definitions_in_scope(&model, &input, Some(0), "org_demo/dev");
        assert!(
            matches!(&checked, Err(CellError::DefinitionMigrationRefused(_))),
            "{name}: {checked:?}"
        );
        let installed = store.install_definitions(model, input, Some(0), "org_demo/dev");
        assert!(
            matches!(&installed, Err(CellError::DefinitionMigrationRefused(_))),
            "{name}: {installed:?}"
        );
    }
    Ok(())
}
