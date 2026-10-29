#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::too_many_lines,
    missing_docs,
    reason = "integration-test crate"
)]

use std::collections::BTreeMap;

use norn_cell::engine::{NativeEngine, SqlEngine};
use norn_cell::export::{ExportError, Exporter};
use norn_cell::{
    CellStore, CommitBatch, DefinitionReadmission, FactWriteRow, InstalledDefinitionInput, Model,
    RestoreError, StoreOptions,
};
use norn_ir::{ObjectTypeDef, RuntimeValue};
use serde_json::{Value, json};

const SCOPE: &str = "org_demo/dev";

#[test]
fn pre_open_legacy_definition_history_without_registry_table_reads_empty_registry() {
    let engine = NativeEngine::open_in_memory().expect("engine");
    engine
        .execute_batch(
            r#"
            CREATE TABLE "_norn_definition_version" (
              "version" INTEGER NOT NULL,
              "receipt_sha256" TEXT NOT NULL,
              "canonical_set" TEXT NOT NULL,
              "model_hash" TEXT NOT NULL,
              "ddl_hash" TEXT NOT NULL,
              "installed_at" INTEGER NOT NULL,
              "commit_seq" INTEGER NOT NULL,
              PRIMARY KEY ("version")
            );
            CREATE TABLE "_norn_definition_active" (
              "id" INTEGER NOT NULL,
              "version" INTEGER NOT NULL,
              PRIMARY KEY ("id")
            );
            INSERT INTO "_norn_definition_version" VALUES (1, 'digest', '[]', 'model', 'ddl', 1, 1);
            INSERT INTO "_norn_definition_active" VALUES (1, 1);
            "#,
        )
        .expect("legacy definition tables");

    let installed = CellStore::read_active_definitions(&engine)
        .expect("pre-open read")
        .expect("active definitions");
    assert_eq!(installed.version, 1);
    assert!(installed.registry.is_empty());
}

fn definitions(note: bool) -> (Model, String) {
    definitions_with_fields(if note { &[("note", None)] } else { &[] })
}

fn definitions_with_fields(fields: &[(&str, Option<&str>)]) -> (Model, String) {
    let mut properties = vec![json!({ "id": "code", "type": { "kind": "text" } })];
    for (name, default) in fields {
        let mut property =
            json!({ "id": name, "type": { "kind": "text" }, "constraints": { "nullable": true } });
        if let Some(default) = default {
            property["default"] = json!(default);
        }
        properties.push(property);
    }
    let definition = json!({ "name": "install.Item", "properties": properties });
    let typed: ObjectTypeDef =
        serde_json::from_value(definition.clone()).expect("typed definition");
    let model = Model::from_definitions(&[typed], &[]).expect("model");
    let artifacts = json!([{
        "irVersion": "0.1", "artifactId": "install.Item.entity", "kind": "entity", "namespace": "install", "name": "Item", "version": "0.1.0", "lifecycle": "draft",
        "scope": { "orgId": "org_demo", "environmentId": "dev" }, "dependencies": [],
        "provenance": { "source": "authoring_sdk", "sourcePath": "test" },
        "payload": { "entityVersion": "2.0", "objectType": definition }
    }]);
    let canonical = String::from_utf8(
        RuntimeValue::from_json(artifacts)
            .expect("exact")
            .canonical_json()
            .expect("canonical"),
    )
    .expect("utf8");
    (model, canonical)
}

fn input(canonical_set: String, at: u64) -> InstalledDefinitionInput {
    InstalledDefinitionInput {
        canonical_set,
        registry: Vec::new(),
        receipt_sha256: "a".repeat(64),
        admission_receipt: None,
        readmission: None,
        installed_at: at,
        diff: json!({ "added": [], "removed": [], "changed": [], "narrowing": [] }),
        layout: None,
    }
}

#[test]
fn modified_before_after_migration_uses_pinned_model_without_changing_commitments() {
    let (base, canonical) = definitions(false);
    let mut store = CellStore::open_in_memory(base.clone()).expect("store");
    store
        .install_definitions(base, input(canonical, 1), Some(0), SCOPE)
        .expect("base definitions");
    let write = |values| FactWriteRow::Put {
        object_type: "install.Item".to_owned(),
        key: "item".to_owned(),
        values,
    };
    let batch = |logical_time, values, events| CommitBatch {
        world: "main".to_owned(),
        scope: SCOPE.to_owned(),
        logical_time,
        writes: vec![write(values)],
        events,
        ..CommitBatch::default()
    };
    store
        .commit(&batch(
            1,
            BTreeMap::from([("code".to_owned(), RuntimeValue::from("a"))]),
            vec![],
        ))
        .expect("original fact");
    let fields = [("note", None), ("defaulted", Some("declared"))];
    let (extended, canonical) = definitions_with_fields(&fields);
    store
        .install_definitions(extended, input(canonical, 2), Some(1), SCOPE)
        .expect("add nullable and defaulted fields");
    let (changed_default, canonical) =
        definitions_with_fields(&[("note", None), ("defaulted", Some("new default"))]);
    store
        .install_definitions(changed_default, input(canonical, 3), Some(2), SCOPE)
        .expect("changing a default preserves its materialized old values");
    let before = json!({"code":"a","note":null,"defaulted":"declared"});
    let after = json!({"code":"b","note":"edited","defaulted":"declared"});
    let event = |before: serde_json::Value, after: serde_json::Value| {
        let changed = before
            .as_object()
            .expect("before")
            .keys()
            .chain(after.as_object().expect("after").keys())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .filter(|property| {
                before.as_object().expect("before").get(*property)
                    != after.as_object().expect("after").get(*property)
            })
            .collect::<Vec<_>>();
        RuntimeValue::from_json(json!({
            "op":"modified","key":{"objectType":"install.Item","key":"item"},
            "beforeVersion":1,"afterVersion":2,"before":before,"after":after,
            "changedProperties":changed,
        }))
        .expect("modified event")
    };
    let values = RuntimeValue::from_json(after.clone())
        .expect("after")
        .as_object()
        .expect("values")
        .clone();
    for (property, forged) in [
        ("code", json!("uncommitted")),
        ("note", json!("uncommitted")),
        ("defaulted", json!(null)),
        ("defaulted", json!("new default")),
        ("invented", json!(null)),
    ] {
        let mut forged_before = before.clone();
        forged_before[property] = forged;
        assert!(
            store
                .commit(&batch(
                    3,
                    values.clone(),
                    vec![event(forged_before, after.clone())],
                ))
                .is_err(),
            "forged before property {property}"
        );
        assert_eq!(store.export_commit_count().expect("head"), 4);
    }
    let authentic = event(before.clone(), after);
    store
        .commit(&batch(3, values, vec![authentic.clone()]))
        .expect("schema-materialized before snapshot");
    let original = store
        .read()
        .object_at_version("main", "install.Item", "item", 1)
        .expect("immutable original")
        .expect("fact");
    assert_eq!(original.values.len(), 1);
    let current_before = RuntimeValue::from_json(before).expect("before");
    assert!(
        store
            .read()
            .verify_fact_snapshot(
                "main",
                "install.Item",
                "item",
                1,
                current_before.as_object().expect("before values"),
            )
            .is_err(),
        "the exact snapshot contract must remain unchanged"
    );
    let (later, canonical) = definitions_with_fields(&[
        ("note", None),
        ("defaulted", Some("new default")),
        ("later", None),
    ]);
    store
        .install_definitions(later, input(canonical, 4), Some(3), SCOPE)
        .expect("later model");
    store
        .read()
        .verify_event_snapshots("main", &authentic)
        .expect("historical event uses its emitting model");
    let records = (1..=store.export_commit_count().expect("head"))
        .map(|seq| store.export_record(seq).expect("export").expect("record"))
        .collect::<Vec<_>>();
    let hash = records.last().expect("last record").hash().expect("hash");
    CellStore::rebuild_export_history(
        NativeEngine::open_in_memory().expect("replay engine"),
        &records,
        &hash,
    )
    .expect("rebuild authenticated migration history");
}

#[test]
fn modified_before_replays_schema_cycles_with_equal_endpoint_hashes() {
    let (model_a, canonical_a) = definitions_with_fields(&[("note", Some("declared"))]);
    let (model_b, canonical_b) = definitions(false);
    let original_hash = model_a.model_hash().to_owned();
    let mut store = CellStore::open_in_memory(model_a.clone()).expect("store");
    store
        .install_definitions(
            model_a.clone(),
            input(canonical_a.clone(), 1),
            Some(0),
            SCOPE,
        )
        .expect("install A");
    store
        .commit(&batch(1, json!({"code":"a","note":null})))
        .expect("explicit null overrides the declared default");
    store
        .install_definitions(model_b, input(canonical_b, 2), Some(1), SCOPE)
        .expect("B may remove an all-null property");
    store
        .install_definitions(model_a, input(canonical_a, 3), Some(2), SCOPE)
        .expect("reinstall A and materialize its default");
    assert_eq!(store.read().model().model_hash(), original_hash);
    let current = store
        .read()
        .object("main", "install.Item", "one")
        .expect("current projection")
        .expect("fact");
    assert_eq!(
        current.version, 1,
        "schema changes do not advance fact versions"
    );
    assert_eq!(current.values["note"], RuntimeValue::from("declared"));
    let original = store
        .read()
        .object_at_version("main", "install.Item", "one", 1)
        .expect("immutable original")
        .expect("fact");
    assert_eq!(original.values["note"], RuntimeValue::Null);

    let after = json!({"code":"b","note":"declared"});
    let event = json!({
        "op":"modified","key":{"objectType":"install.Item","key":"one"},
        "beforeVersion":1,"afterVersion":2,
        "before":{"code":"a","note":"declared"},"after":after,
        "changedProperties":["code"],
    });
    let mut forged = event.clone();
    forged["before"]["note"] = json!("uncommitted");
    forged["changedProperties"] = json!(["code", "note"]);
    let mut forged_batch = batch(3, after.clone());
    forged_batch.events = vec![RuntimeValue::from_json(forged).expect("forged event")];
    assert!(matches!(
        store.commit(&forged_batch),
        Err(norn_cell::CellError::HistoricalValueUnproven(_))
    ));
    assert_eq!(store.export_commit_count().expect("head"), 4);

    let authentic = RuntimeValue::from_json(event).expect("authentic event");
    let mut accepted = batch(3, after);
    accepted.events = vec![authentic.clone()];
    store.commit(&accepted).expect("A to B to A modification");
    store
        .read()
        .verify_event_snapshots("main", &authentic)
        .expect("equal endpoint model hashes still replay intermediate schemas");
    let records = (1..=store.export_commit_count().expect("head"))
        .map(|seq| store.export_record(seq).expect("export").expect("record"))
        .collect::<Vec<_>>();
    let hash = records.last().expect("last record").hash().expect("hash");
    let rebuilt = CellStore::rebuild_export_history(
        NativeEngine::open_in_memory().expect("replay engine"),
        &records,
        &hash,
    )
    .expect("rebuild authenticated schema-cycle history");
    rebuilt
        .read()
        .verify_event_snapshots("main", &authentic)
        .expect("replayed before proof");
    assert_eq!(
        rebuilt
            .read()
            .object("main", "install.Item", "one")
            .expect("rebuilt current")
            .expect("fact")
            .values["code"],
        RuntimeValue::from("b")
    );
}

fn layout_sidecar(x: i64) -> String {
    let value = RuntimeValue::from_json(json!({
        "layoutVersion": "0.1",
        "artifacts": {
            "install.Item.entity": {
                "nodes": {
                    "node-1": { "position": { "x": x, "y": 2 } }
                }
            }
        }
    }))
    .expect("layout sidecar is exact JSON");
    String::from_utf8(value.canonical_json().expect("layout canonicalizes"))
        .expect("layout is UTF-8")
}

fn admission_receipt(checker_version: &str, expr_profile: &str) -> (String, String) {
    let value = RuntimeValue::from_json(json!({
        "format": "norn-checker/receipt@1",
        "checker": { "name": "norn-checker", "version": checker_version },
        "artifactSet": { "artifacts": [], "base": [], "sha256": "0".repeat(64) },
        "limitProfile": { "id": "norn.limits", "version": 1, "sha256": "0".repeat(64) },
        "exprProfile": expr_profile,
        "diagnostics": { "count": 0, "errors": 0, "sha256": "0".repeat(64) },
        "admitted": true
    }))
    .expect("receipt is exact JSON");
    let receipt = String::from_utf8(value.canonical_json().expect("receipt canonicalizes"))
        .expect("canonical receipt is UTF-8");
    let digest = norn_checker::admission_receipt_sha256(&receipt).expect("receipt hashes");
    (receipt, digest)
}

fn batch(time: u64, values: Value) -> CommitBatch {
    CommitBatch {
        world: "main".to_owned(),
        scope: SCOPE.to_owned(),
        logical_time: time,
        writes: vec![FactWriteRow::Put {
            object_type: "install.Item".to_owned(),
            key: "one".to_owned(),
            values: serde_json::from_value(values).expect("values"),
        }],
        ..CommitBatch::default()
    }
}

#[tokio::test]
async fn version_zero_then_two_installs_restore_through_typed_models_and_preserve_the_journal() {
    let directory = tempfile::tempdir().expect("directory");
    let (v1, json1) = definitions(false);
    let (v2, json2) = definitions(true);
    let mut store = CellStore::open_in_memory(v1.clone()).expect("store");
    store.bind("scope", SCOPE).expect("bind");
    let mut legacy = batch(10, json!({ "code": "one" }));
    // A business event cannot assert a definitions version.
    legacy.events.push(
        RuntimeValue::from_json(json!({ "type": "definitions.installed", "version": 99 }))
            .expect("event"),
    );
    store.commit(&legacy).expect("legacy commit");
    let mut input1 = input(json1.clone(), 20);
    input1.registry = vec!["@norn/core@1.2.3".to_owned()];
    let retained_layout = layout_sidecar(10);
    input1.layout = Some(retained_layout.clone());
    let installed1 = store
        .install_definitions(v1, input1, Some(0), SCOPE)
        .expect("version1");
    let mut input2 = input(json2.clone(), 30);
    input2.registry = vec!["@norn/core@1.2.4".to_owned(), "@norn/ui@0.9.0".to_owned()];
    let installed2 = store
        .install_definitions(v2.clone(), input2, Some(1), SCOPE)
        .expect("version2");
    assert_eq!(installed1.registry, vec!["@norn/core@1.2.3".to_owned()]);
    assert_eq!(installed1.layout.as_deref(), Some(retained_layout.as_str()));
    assert_eq!(installed1.layout_revision, 1);
    assert_eq!(
        installed2.registry,
        vec!["@norn/core@1.2.4".to_owned(), "@norn/ui@0.9.0".to_owned(),]
    );
    assert_eq!(installed2.layout.as_deref(), Some(retained_layout.as_str()));
    assert_eq!(installed2.layout_revision, 1);
    assert_eq!(
        store
            .read()
            .installed_definitions(Some(2))
            .expect("persisted version")
            .expect("version2")
            .registry,
        installed2.registry
    );
    store
        .commit(&batch(40, json!({ "code": "one", "note": "retained" })))
        .expect("new column data");
    let original: Vec<_> = (1..=4)
        .map(|seq| store.export_record(seq).expect("journal").expect("record"))
        .collect();
    assert!(original[0].initial_model.is_some());
    assert!(original[0].definition_install.is_none());
    assert_eq!(original[0].receipt.body.definitions_version, 0);
    assert!(original[1].initial_model.is_none());
    assert_eq!(
        original[1]
            .definition_install
            .as_ref()
            .expect("install1")
            .installed
            .version,
        1
    );
    assert_eq!(
        original[1]
            .definition_install
            .as_ref()
            .expect("install1")
            .installed
            .registry,
        vec!["@norn/core@1.2.3".to_owned()]
    );
    assert_eq!(
        original[1]
            .definition_install
            .as_ref()
            .expect("install1")
            .installed
            .layout
            .as_deref(),
        Some(retained_layout.as_str())
    );
    assert_eq!(
        original[2]
            .definition_install
            .as_ref()
            .expect("install2")
            .installed
            .version,
        2
    );
    assert_eq!(
        original[2]
            .definition_install
            .as_ref()
            .expect("install2")
            .installed
            .registry,
        vec!["@norn/core@1.2.4".to_owned(), "@norn/ui@0.9.0".to_owned(),]
    );
    let registry_event =
        RuntimeValue::from_json(json!(["@norn/core@1.2.3"])).expect("registry event");
    assert_eq!(
        original[1].batch.events[0].get("registry"),
        Some(&registry_event)
    );
    let mut exporter = Exporter::local(&directory.path().join("export")).expect("exporter");
    for record in &original {
        exporter
            .append(record)
            .await
            .expect("append verified record");
    }
    let manifest = exporter.verify().await.expect("independent verify");
    assert_eq!(manifest.definitions_version, 2);
    assert_eq!(
        exporter
            .active_definitions()
            .await
            .expect("active")
            .expect("v2")
            .canonical_set,
        json2
    );
    assert_eq!(
        exporter
            .active_definitions()
            .await
            .expect("active")
            .expect("v2")
            .registry,
        vec!["@norn/core@1.2.4".to_owned(), "@norn/ui@0.9.0".to_owned(),]
    );
    let restored_path = directory.path().join("restored.sqlite");
    let bindings = BTreeMap::from([("scope".to_owned(), SCOPE.to_owned())]);
    let report = CellStore::restore_from(
        &mut exporter,
        &restored_path,
        v2.clone(),
        SCOPE,
        &bindings,
        StoreOptions::default(),
    )
    .await
    .expect("typed restore");
    assert_eq!(report.manifest, manifest);
    let restored = CellStore::open(&restored_path, v2, StoreOptions::default()).expect("reopen");
    assert_eq!(
        restored
            .read()
            .installed_definitions(Some(1))
            .expect("history")
            .expect("v1")
            .canonical_set,
        json1
    );
    assert_eq!(
        restored
            .read()
            .installed_definitions(Some(1))
            .expect("history")
            .expect("v1")
            .registry,
        vec!["@norn/core@1.2.3".to_owned()]
    );
    let restored_v2 = restored
        .read()
        .installed_definitions(None)
        .expect("active")
        .expect("v2");
    assert_eq!(restored_v2.version, 2);
    assert_eq!(
        restored_v2.registry,
        vec!["@norn/core@1.2.4".to_owned(), "@norn/ui@0.9.0".to_owned(),]
    );
    assert_eq!(
        restored_v2.layout.as_deref(),
        Some(retained_layout.as_str())
    );
    assert_eq!(restored_v2.layout_revision, 1);
    assert_eq!(
        restored
            .read()
            .object("main", "install.Item", "one")
            .expect("object")
            .expect("one")
            .values["note"]
            .as_str(),
        Some("retained")
    );
    for record in original {
        assert_eq!(
            restored
                .export_record(record.receipt.body.commit_seq)
                .expect("replayed journal"),
            Some(record)
        );
    }
}

#[tokio::test]
async fn readmission_export_restores_the_receipt_and_original_prefix_exactly() {
    let directory = tempfile::tempdir().expect("directory");
    let (model, canonical) = definitions(false);
    let mut store = CellStore::open_in_memory(model.clone()).expect("store");
    store.bind("scope", SCOPE).expect("bind");

    // Simulate an existing hash-only cell from before admission receipt bodies
    // were retained. Its committed definition record remains part of history.
    let old_hash = "a".repeat(64);
    let mut first_input = input(canonical.clone(), 10);
    first_input.receipt_sha256 = old_hash.clone();
    let first = store
        .install_definitions(model.clone(), first_input, Some(0), SCOPE)
        .expect("legacy version");
    let (receipt, receipt_sha256) = admission_receipt("0.1.0", "norn-zen/6");
    let mut readmit_input = input(canonical, 20);
    readmit_input.receipt_sha256 = receipt_sha256;
    readmit_input.admission_receipt = Some(receipt);
    readmit_input.readmission = Some(DefinitionReadmission {
        previous_receipt_sha256: old_hash,
        previous_checker_version: "0.1.0".to_owned(),
        previous_expr_profile: "norn-zen/4".to_owned(),
        checker_version: "0.1.0".to_owned(),
        expr_profile: "norn-zen/6".to_owned(),
        layout_migration: false,
    });
    let second = store
        .install_definitions(model.clone(), readmit_input, Some(first.version), SCOPE)
        .expect("readmission");
    let original: Vec<_> = (1..=2)
        .map(|seq| store.export_record(seq).expect("journal").expect("record"))
        .collect();
    let mut exporter = Exporter::local(&directory.path().join("export")).expect("exporter");
    for record in &original {
        exporter
            .append(record)
            .await
            .expect("publish verified record");
    }

    let target = directory.path().join("restored.sqlite");
    CellStore::restore_from(
        &mut exporter,
        &target,
        model.clone(),
        SCOPE,
        &BTreeMap::from([("scope".to_owned(), SCOPE.to_owned())]),
        StoreOptions::default(),
    )
    .await
    .expect("restore readmission");
    let restored = CellStore::open(&target, model, StoreOptions::default()).expect("reopen");
    assert_eq!(
        restored
            .read()
            .installed_definitions(None)
            .expect("active definitions"),
        Some(second)
    );
    for record in &original {
        assert_eq!(
            restored
                .export_record(record.receipt.body.commit_seq)
                .expect("restored journal"),
            Some(record.clone())
        );
    }
}

#[tokio::test]
async fn typed_install_tampering_version_jumps_and_untyped_schema_changes_fail_independent_verification()
 {
    let directory = tempfile::tempdir().expect("directory");
    let (v1, json1) = definitions(false);
    let (v2, json2) = definitions(true);
    let mut store = CellStore::open_in_memory(v1.clone()).expect("store");
    store.bind("scope", SCOPE).expect("bind");
    store
        .install_definitions(v1, input(json1, 10), Some(0), SCOPE)
        .expect("v1");
    store
        .install_definitions(v2.clone(), input(json2, 20), Some(1), SCOPE)
        .expect("v2");
    let first = store.export_record(1).expect("record").expect("first");
    let second = store.export_record(2).expect("record").expect("second");
    let mut exporter = Exporter::local(&directory.path().join("export")).expect("exporter");
    exporter.append(&first).await.expect("first");
    let mut changed = second.clone();
    changed.definition_install = None;
    assert!(
        exporter.append(&changed).await.is_err(),
        "receipt cannot advance definitions without typed state"
    );
    let mut changed = second.clone();
    changed
        .definition_install
        .as_mut()
        .expect("typed")
        .installed
        .version = 3;
    assert!(
        matches!(
            exporter.append(&changed).await,
            Err(ExportError::Invalid(_))
        ),
        "version jump"
    );
    let mut changed = second.clone();
    changed
        .definition_install
        .as_mut()
        .expect("typed")
        .installed
        .canonical_set = "[{}] ".to_owned();
    assert!(
        exporter.append(&changed).await.is_err(),
        "canonical set text is independently validated"
    );
    let mut changed = second.clone();
    let mut snapshot =
        serde_json::to_value(&changed.definition_install.as_ref().expect("typed").model)
            .expect("snapshot");
    snapshot["objectTypes"][0]["properties"][0]["default"] = json!("different semantics");
    changed.definition_install.as_mut().expect("typed").model =
        serde_json::from_value(snapshot).expect("typed snapshot");
    assert!(
        exporter.append(&changed).await.is_err(),
        "same DDL cannot conceal changed default semantics"
    );
    let mut changed = second.clone();
    changed.initial_model = Some(v2.definitions_snapshot());
    assert!(
        exporter.append(&changed).await.is_err(),
        "initial model is genesis-only"
    );
    exporter
        .append(&second)
        .await
        .expect("unchanged typed install");
    let target = directory.path().join("wrong-model.sqlite");
    assert!(matches!(
        CellStore::restore_from(
            &mut exporter,
            &target,
            definitions(false).0,
            SCOPE,
            &BTreeMap::from([("scope".to_owned(), SCOPE.to_owned())]),
            StoreOptions::default()
        )
        .await,
        Err(RestoreError::ModelMismatch)
    ));
    assert!(!target.exists());
}
