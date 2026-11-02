#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs,
    reason = "scenario assertions"
)]

use norn_cell::cell_identity::CellId;
use norn_cell::engine::NativeEngine;
use norn_cell::export::Exporter;
use norn_cell::{CellError, CellStore, CommitBatch, Model, RestoreError, StoreOptions};
use std::collections::BTreeMap;

fn id(value: &str) -> CellId {
    CellId::try_from(value.to_owned()).expect("identity")
}
fn model() -> Model {
    Model::from_definitions(&[], &[]).expect("empty model")
}
fn a() -> CellId {
    id("00112233445566778899aabbccddeeff")
}
fn b() -> CellId {
    id("ffeeddccbbaa99887766554433221100")
}

#[test]
fn cell_id_generated_once_and_reopen_refuses_another_identity() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    let engine = NativeEngine::open(&path, StoreOptions::default()).expect("engine");
    let mut calls = 0;
    let mut store = CellStore::from_engine_with_identity_source(engine, model(), |_| {
        calls += 1;
        Ok(a())
    })
    .expect("new");
    assert_eq!(calls, 1);
    store.bind("scope", "org_demo/dev").expect("scope");
    store
        .commit(&CommitBatch {
            scope: "org_demo/dev".into(),
            ..CommitBatch::default()
        })
        .expect("commit");
    let receipt = store.read().receipt_at(1).expect("receipt");
    drop(store);
    let engine = NativeEngine::open(&path, StoreOptions::default()).expect("engine");
    let reopened = CellStore::from_engine_with_identity_source(engine, model(), |_| {
        calls += 1;
        Ok(b())
    })
    .expect("reopen");
    assert_eq!(calls, 1);
    assert_eq!(reopened.cell_id().expect("id"), Some(a()));
    assert_eq!(reopened.read().receipt_at(1).expect("receipt"), receipt);
    assert!(matches!(
        reopened.require_cell_id(&b()),
        Err(CellError::BindingMismatch { .. })
    ));
    drop(reopened);
    let engine = NativeEngine::open(&path, StoreOptions::default()).expect("engine");
    assert!(matches!(
        CellStore::from_engine_bound(engine, model(), &b()),
        Err(CellError::BindingMismatch { .. })
    ));
}

#[test]
fn same_scope_stores_get_independent_ids() {
    let mut first = CellStore::open_in_memory(model()).expect("first");
    let mut second = CellStore::open_in_memory(model()).expect("second");
    first.bind("scope", "org_demo/dev").expect("scope");
    second.bind("scope", "org_demo/dev").expect("scope");
    assert_ne!(first.cell_id().expect("id"), second.cell_id().expect("id"));
    assert!(matches!(
        first.bind("cell_id", b().as_str()),
        Err(CellError::BindingMismatch { .. })
    ));
}

#[tokio::test]
async fn restore_requires_same_explicit_identity_and_export_refuses_unqualified_history() {
    let dir = tempfile::tempdir().expect("directory");
    let engine = NativeEngine::open_in_memory().expect("engine");
    let mut store = CellStore::from_engine_bound(engine, model(), &a()).expect("source");
    store.bind("scope", "org_demo/dev").expect("scope");
    store
        .commit(&CommitBatch {
            scope: "org_demo/dev".into(),
            ..CommitBatch::default()
        })
        .expect("commit");
    let record = store.export_record(1).expect("read").expect("record");
    let mut exporter = Exporter::local(&dir.path().join("export")).expect("exporter");
    exporter.append(&record).await.expect("export");
    let target = dir.path().join("restored.sqlite");
    let mut bindings = BTreeMap::from([
        ("scope".into(), "org_demo/dev".into()),
        ("cell_id".into(), String::from(b())),
    ]);
    assert!(matches!(
        CellStore::restore_from(
            &mut exporter,
            &target,
            model(),
            "org_demo/dev",
            &bindings,
            StoreOptions::default()
        )
        .await,
        Err(RestoreError::IdentityMismatch)
    ));
    assert!(!target.exists());
    bindings.remove("cell_id");
    assert!(matches!(
        CellStore::restore_from(
            &mut exporter,
            &target,
            model(),
            "org_demo/dev",
            &bindings,
            StoreOptions::default()
        )
        .await,
        Err(RestoreError::IdentityMismatch)
    ));
    assert!(!target.exists());
    bindings.insert("cell_id".into(), String::from(a()));
    CellStore::restore_from(
        &mut exporter,
        &target,
        model(),
        "org_demo/dev",
        &bindings,
        StoreOptions::default(),
    )
    .await
    .expect("same identity restore");
    let restored = CellStore::open(&target, model(), StoreOptions::default()).expect("reopen");
    assert_eq!(restored.cell_id().expect("id"), Some(a()));
    let mut old = record;
    old.bindings.remove("cell_id");
    let mut old_exporter = Exporter::local(&dir.path().join("legacy")).expect("exporter");
    assert!(matches!(
        old_exporter.append(&old).await,
        Err(norn_cell::export::ExportError::IdentityMismatch)
    ));
}

#[test]
fn legacy_history_stays_local_and_cannot_adopt_an_identity() {
    use norn_cell::engine::SqlEngine as _;
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("legacy.sqlite");
    drop(CellStore::open(&path, model(), StoreOptions::default()).expect("empty store"));
    let engine = NativeEngine::open(&path, StoreOptions::default()).expect("engine");
    engine
        .execute("DELETE FROM _norn_meta WHERE key = 'binding:cell_id'", &[])
        .expect("legacy fixture");
    let mut store = CellStore::from_engine(engine, model()).expect("legacy local open");
    assert_eq!(store.cell_id().expect("unqualified"), None);
    assert!(matches!(
        store.bind("cell_id", a().as_str()),
        Err(CellError::BindingMismatch { .. })
    ));
    let receipt = store
        .commit(&CommitBatch::default())
        .expect("local commit")
        .receipt()
        .clone();
    drop(store);
    let store = CellStore::open(&path, model(), StoreOptions::default()).expect("legacy reopen");
    assert_eq!(store.read().receipt_at(1).expect("receipt"), Some(receipt));
    assert_eq!(store.cell_id().expect("unqualified"), None);
    assert!(matches!(
        store.export_record(1),
        Err(CellError::BindingMismatch { .. })
    ));
}
