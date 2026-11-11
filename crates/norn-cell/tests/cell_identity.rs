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
        (
            "cell_identity_format".into(),
            norn_cell::cell_identity::FORMAT.into(),
        ),
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
    old.bindings.remove("cell_identity_format");
    assert!(matches!(
        old.advance(&norn_cell::export::ExportManifest::default()),
        Err(norn_cell::export::ExportError::IdentityMismatch)
    ));
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
        .execute("DELETE FROM _norn_meta WHERE key IN ('binding:cell_id', 'binding:cell_identity_format')", &[])
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

#[test]
fn export_refuses_identity_changed_under_an_open_handle() {
    use norn_cell::engine::SqlEngine as _;
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    let mut store =
        CellStore::open_bound(&path, model(), StoreOptions::default(), &a()).expect("source");
    store.commit(&CommitBatch::default()).expect("commit");
    NativeEngine::open(&path, StoreOptions::default())
        .expect("engine")
        .execute(
            "UPDATE _norn_meta SET value = ?1 WHERE key = 'binding:cell_id'",
            &[b().as_str().into()],
        )
        .expect("foreign identity fixture");
    assert!(matches!(
        store.export_record(1),
        Err(CellError::BindingMismatch { .. })
    ));
}

#[test]
fn unqualified_local_history_can_record_external_results() {
    use norn_cell::engine::SqlEngine as _;
    use norn_cell::{EffectRow, OutboxCommand};
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("legacy.sqlite");
    drop(CellStore::open(&path, model(), StoreOptions::default()).expect("empty store"));
    let engine = NativeEngine::open(&path, StoreOptions::default()).expect("engine");
    engine
        .execute("DELETE FROM _norn_meta WHERE key IN ('binding:cell_id', 'binding:cell_identity_format')", &[])
        .expect("legacy fixture");
    let mut store = CellStore::from_engine(engine, model()).expect("legacy open");
    store
        .commit(&CommitBatch {
            effects: vec![EffectRow::Enqueue {
                command: OutboxCommand {
                    command_id: "legacy-command".into(),
                    generation: 0,
                    idempotency_key: "legacy-operation".into(),
                    deadline: None,
                    command: norn_ir::RuntimeValue::Null,
                },
            }],
            ..CommitBatch::default()
        })
        .expect("legacy enqueue");
    assert!(
        store
            .record_performed("legacy-command", 0, &norn_ir::RuntimeValue::Bool(true))
            .expect("local result")
    );
    assert!(matches!(
        store.export_record(1),
        Err(CellError::BindingMismatch { .. })
    ));
    drop(store);
    let store = CellStore::open(&path, model(), StoreOptions::default()).expect("legacy reopen");
    assert_eq!(
        store
            .read()
            .head_command()
            .expect("command")
            .expect("pending")
            .result,
        Some(norn_ir::RuntimeValue::Bool(true))
    );
}

#[test]
fn unversioned_cell_named_binding_cannot_gain_portable_provenance() {
    use norn_cell::engine::SqlEngine as _;
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("legacy.sqlite");
    drop(CellStore::open_bound(&path, model(), StoreOptions::default(), &a()).expect("seed"));
    let engine = NativeEngine::open(&path, StoreOptions::default()).expect("engine");
    engine
        .execute(
            "DELETE FROM _norn_meta WHERE key = 'binding:cell_identity_format'",
            &[],
        )
        .expect("unversioned binding");
    let mut store = CellStore::from_engine(engine, model()).expect("legacy open");
    assert_eq!(store.cell_id().expect("identity"), None);
    assert!(
        store
            .bind("cell_identity_format", norn_cell::cell_identity::FORMAT)
            .is_err()
    );
    assert!(store.require_cell_id(&a()).is_err());
    store.commit(&CommitBatch::default()).expect("local commit");
    assert!(store.export_record(1).is_err());
    drop(store);
    assert_eq!(
        CellStore::open(&path, model(), StoreOptions::default())
            .expect("legacy reopen")
            .cell_id()
            .expect("identity"),
        None
    );
}

#[test]
fn reopen_refuses_a_later_journal_identity_after_metadata_is_restored() {
    use norn_cell::engine::SqlEngine as _;
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    let mut store =
        CellStore::open_bound(&path, model(), StoreOptions::default(), &a()).expect("source");
    store.commit(&CommitBatch::default()).expect("first commit");
    store
        .commit(&CommitBatch::default())
        .expect("same-cell control");
    drop(store);
    let mut store = CellStore::open_bound(&path, model(), StoreOptions::default(), &a())
        .expect("same-cell history reopens");
    let engine = NativeEngine::open(&path, StoreOptions::default()).expect("fixture engine");
    let change_identity = |identity: &CellId| {
        engine
            .execute(
                "UPDATE _norn_meta SET value = ?1 WHERE key = 'binding:cell_id'",
                &[identity.as_str().into()],
            )
            .expect("identity fixture");
    };
    change_identity(&b());
    store
        .commit(&CommitBatch::default())
        .expect("later foreign journal fixture");
    change_identity(&a());
    drop(store);
    assert!(matches!(
        CellStore::open_bound(&path, model(), StoreOptions::default(), &a()),
        Err(CellError::BindingMismatch { .. })
    ));
}

fn identity_bindings(cell_id: Option<&str>) -> BTreeMap<String, String> {
    let mut bindings = BTreeMap::from([
        ("scope".to_owned(), "org_demo/dev".to_owned()),
        (
            "cell_identity_format".to_owned(),
            norn_cell::cell_identity::FORMAT.to_owned(),
        ),
    ]);
    if let Some(cell_id) = cell_id {
        bindings.insert("cell_id".to_owned(), cell_id.to_owned());
    }
    bindings
}

fn file_dump(path: &std::path::Path) -> BTreeMap<String, Vec<String>> {
    CellStore::open(path, model(), StoreOptions::default())
        .expect("reopen")
        .read()
        .dump()
        .expect("dump")
}

/// CELL-ID-RESTORE: a same-scope sibling's identity, a missing identity and a
/// malformed identity all refuse before a destination exists; the source is
/// unchanged and the accepted restore reproduces the source's complete state.
#[tokio::test]
async fn cell_identity_restore_refuses_other_identities_and_preserves_state() {
    let dir = tempfile::tempdir().expect("directory");
    let source_path = dir.path().join("source.sqlite");
    let mut source = CellStore::open_bound(&source_path, model(), StoreOptions::default(), &a())
        .expect("source");
    let sibling = CellStore::open_in_memory(model()).expect("same-scope sibling");
    source.bind("scope", "org_demo/dev").expect("scope");
    let sibling_id = sibling.cell_id().expect("id").expect("bound");
    assert_ne!(sibling_id, a(), "same-scope cells cannot alias");
    let mut exporter = Exporter::local(&dir.path().join("export")).expect("exporter");
    for _ in 0..2 {
        let seq = source
            .commit(&CommitBatch {
                scope: "org_demo/dev".into(),
                ..CommitBatch::default()
            })
            .expect("commit")
            .receipt()
            .body
            .commit_seq;
        let record = source.export_record(seq).expect("read").expect("record");
        exporter.append(&record).await.expect("export");
    }
    let source_state = source.read().dump().expect("dump");
    drop(source);
    let target = dir.path().join("restored.sqlite");
    for bindings in [
        identity_bindings(Some(sibling_id.as_str())),
        identity_bindings(Some(b().as_str())),
        identity_bindings(None),
        identity_bindings(Some("NOT-A-CELL-ID")),
    ] {
        assert!(matches!(
            CellStore::restore_from(
                &mut exporter,
                &target,
                model(),
                "org_demo/dev",
                &bindings,
                StoreOptions::default(),
            )
            .await,
            Err(RestoreError::IdentityMismatch)
        ));
        assert!(!target.exists(), "a refused restore publishes nothing");
    }
    assert_eq!(file_dump(&source_path), source_state);
    CellStore::restore_from(
        &mut exporter,
        &target,
        model(),
        "org_demo/dev",
        &identity_bindings(Some(a().as_str())),
        StoreOptions::default(),
    )
    .await
    .expect("same identity restore");
    let restored = CellStore::open(&target, model(), StoreOptions::default()).expect("restored");
    assert_eq!(restored.cell_id().expect("id"), Some(a()));
    assert_eq!(restored.read().dump().expect("dump"), source_state);
}

/// CELL-ID-OPEN after restore: reopening the restored file under another
/// identity refuses without writing, never calls the identity source, and a
/// second restore onto the published file refuses; the file's complete state is
/// unchanged by every refusal.
#[tokio::test]
async fn cell_identity_restore_then_reopen_under_another_identity_writes_nothing() {
    let dir = tempfile::tempdir().expect("directory");
    let mut source = CellStore::open_in_memory(model()).expect("source");
    source.bind("scope", "org_demo/dev").expect("scope");
    let source_id = source.cell_id().expect("id").expect("bound");
    source
        .commit(&CommitBatch {
            scope: "org_demo/dev".into(),
            ..CommitBatch::default()
        })
        .expect("commit");
    let record = source.export_record(1).expect("read").expect("record");
    let mut exporter = Exporter::local(&dir.path().join("export")).expect("exporter");
    exporter.append(&record).await.expect("export");
    let target = dir.path().join("restored.sqlite");
    let bindings = identity_bindings(Some(source_id.as_str()));
    CellStore::restore_from(
        &mut exporter,
        &target,
        model(),
        "org_demo/dev",
        &bindings,
        StoreOptions::default(),
    )
    .await
    .expect("restore");
    let restored_state = file_dump(&target);
    assert_eq!(restored_state, source.read().dump().expect("dump"));
    let other = if source_id == a() { b() } else { a() };

    assert!(matches!(
        CellStore::open_bound(&target, model(), StoreOptions::default(), &other),
        Err(CellError::BindingMismatch { .. })
    ));
    assert_eq!(file_dump(&target), restored_state);

    let mut calls = 0;
    let engine = NativeEngine::open(&target, StoreOptions::default()).expect("engine");
    let reopened = CellStore::from_engine_with_identity_source(engine, model(), |_| {
        calls += 1;
        Ok(other.clone())
    })
    .expect("reopen");
    assert_eq!(calls, 0, "reopen never consults the identity source");
    assert_eq!(reopened.cell_id().expect("id"), Some(source_id.clone()));
    drop(reopened);
    assert_eq!(file_dump(&target), restored_state);

    assert!(matches!(
        CellStore::restore_from(
            &mut exporter,
            &target,
            model(),
            "org_demo/dev",
            &bindings,
            StoreOptions::default(),
        )
        .await,
        Err(RestoreError::TargetExists(_))
    ));
    assert_eq!(file_dump(&target), restored_state);
}
