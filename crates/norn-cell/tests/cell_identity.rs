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

/// A host whose SQLite excludes `lower`, `hex` and `randomblob`, the functions
/// the default identity source calls. `fail` refuses the n-th storage boundary.
struct NoIdentityFunctions {
    inner: NativeEngine,
    step: std::rc::Rc<std::cell::Cell<usize>>,
    fail: Option<usize>,
}

impl NoIdentityFunctions {
    fn open(path: &std::path::Path, fail: Option<usize>) -> Self {
        Self {
            inner: NativeEngine::open(path, StoreOptions::default()).expect("engine"),
            step: std::rc::Rc::default(),
            fail,
        }
    }

    fn admit(&self, sql: &str) -> Result<(), CellError> {
        let step = self.step.get().saturating_add(1);
        self.step.set(step);
        if self.fail == Some(step) {
            return Err(CellError::Engine("storage fault".to_owned()));
        }
        let sql = sql.to_ascii_lowercase();
        for name in ["randomblob", "hex", "lower"] {
            let called = sql.match_indices(name).any(|(at, _)| {
                let (before, rest) = sql.split_at(at);
                !before.ends_with(|c: char| c.is_ascii_alphanumeric() || c == '_')
                    && rest
                        .strip_prefix(name)
                        .is_some_and(|tail| tail.trim_start().starts_with('('))
            });
            if called {
                return Err(CellError::Engine(format!("no such function: {name}")));
            }
        }
        Ok(())
    }
}

impl norn_cell::engine::SqlEngine for NoIdentityFunctions {
    fn read_isolation(&self) -> norn_cell::engine::ReadIsolation {
        self.inner.read_isolation()
    }
    fn execute(&self, sql: &str, params: &[norn_cell::engine::SqlValue]) -> Result<(), CellError> {
        self.admit(sql)?;
        self.inner.execute(sql, params)
    }
    fn query(
        &self,
        sql: &str,
        params: &[norn_cell::engine::SqlValue],
    ) -> Result<Vec<Vec<norn_cell::engine::SqlValue>>, CellError> {
        self.admit(sql)?;
        self.inner.query(sql, params)
    }
    fn execute_batch(&self, sql: &str) -> Result<(), CellError> {
        self.admit(sql)?;
        self.inner.execute_batch(sql)
    }
    fn transaction<T, X: From<CellError>>(&self, f: impl FnOnce() -> Result<T, X>) -> Result<T, X> {
        self.admit("")?;
        self.inner.transaction(f)
    }
}

const SCOPE: &str = "org_demo/dev";

fn provision_input() -> norn_cell::InstalledDefinitionInput {
    norn_cell::InstalledDefinitionInput {
        canonical_set: "[]".to_owned(),
        registry: Vec::new(),
        receipt_sha256: "synthetic".to_owned(),
        admission_receipt: None,
        readmission: None,
        diff: serde_json::json!({}),
        installed_at: 1,
        layout: None,
    }
}

fn provision(
    engine: NoIdentityFunctions,
    scope: &str,
    source: impl FnMut(&NoIdentityFunctions) -> Result<CellId, CellError>,
) -> Result<CellStore<NoIdentityFunctions>, CellError> {
    CellStore::provision_definitions_with_identity_source(
        engine,
        model(),
        &provision_input(),
        scope,
        source,
    )
}

/// Storage boundaries one successful fresh provisioning crosses.
fn provision_steps() -> usize {
    let dir = tempfile::tempdir().expect("directory");
    let engine = NoIdentityFunctions::open(&dir.path().join("count.sqlite"), None);
    let steps = std::rc::Rc::clone(&engine.step);
    provision(engine, SCOPE, |_| Ok(a())).expect("count provisioning");
    steps.get()
}

/// CELL-ID-1 on a host without the SQL identity functions: provisioning binds
/// the injected identity exactly once, atomically under any storage fault or
/// source failure; retries never consult a source; mismatches still refuse.
#[test]
fn provisioning_binds_an_injected_identity_where_sql_identity_functions_are_excluded() {
    use norn_testkit::proptest::prelude::*;
    let steps = provision_steps();
    let ids = "[0-9a-f]{32}".prop_map(|value| id(&value));
    let strategy = (
        ids.clone(),
        ids,
        prop::option::of(any::<prop::sample::Index>()),
        any::<bool>(),
    );
    norn_testkit::check_property(
        &norn_testkit::Settings::new(file!()).cases(48),
        &strategy,
        |(first, other, fault, source_fails)| {
            prop_assume!(first != other);
            let dir = tempfile::tempdir().expect("directory");
            let path = dir.path().join("tenant.sqlite");
            let open = || NoIdentityFunctions::open(&path, None);
            let fresh = || CellStore::engine_is_fresh(&open()).expect("fresh check");

            let default =
                CellStore::provision_definitions(open(), model(), &provision_input(), SCOPE);
            prop_assert!(
                matches!(&default, Err(CellError::Engine(m)) if m == "no such function: randomblob"),
                "default source on this host: {:?}",
                default.as_ref().err()
            );
            prop_assert!(fresh());
            // A storage fault leaves either a fresh engine or, past the commit
            // point (a lost reply), the complete provisioned cell; never a part.
            let mut committed = false;
            if let Some(index) = fault {
                let fail = index.index(steps).saturating_add(1);
                let engine = NoIdentityFunctions::open(&path, Some(fail));
                prop_assert!(provision(engine, SCOPE, |_| Ok(first.clone())).is_err());
                committed = !fresh();
            }
            if source_fails && !committed {
                let refused = provision(open(), SCOPE, |_| {
                    Err(CellError::Engine("identity service unavailable".to_owned()))
                });
                prop_assert!(
                    matches!(&refused, Err(CellError::Engine(m)) if m == "identity service unavailable")
                );
                prop_assert!(fresh());
            }

            let mut calls = 0;
            let store = provision(open(), SCOPE, |_| {
                calls += 1;
                Ok(first.clone())
            })?;
            prop_assert_eq!(calls, usize::from(!committed));
            prop_assert_eq!(store.cell_id()?, Some(first.clone()));
            let record = store.export_record(1)?.expect("provisioning record");
            prop_assert_eq!(
                record.bindings.get("cell_id"),
                Some(&String::from(first.clone()))
            );
            drop(store);

            let retried = provision(open(), SCOPE, |_| {
                calls += 1;
                Ok(other.clone())
            })?;
            prop_assert_eq!(
                calls,
                usize::from(!committed),
                "a retry never consults the identity source"
            );
            prop_assert_eq!(retried.cell_id()?, Some(first.clone()));
            prop_assert_eq!(retried.read().receipts()?.len(), 1);
            prop_assert!(
                matches!(
                    retried.require_cell_id(&other),
                    Err(CellError::BindingMismatch { .. })
                ),
                "another expected identity must refuse"
            );
            drop(retried);
            let default_retry =
                CellStore::provision_definitions(open(), model(), &provision_input(), SCOPE)?;
            prop_assert_eq!(default_retry.cell_id()?, Some(first.clone()));
            drop(default_retry);

            let changed = provision(open(), "other/dev", |_| {
                calls += 1;
                Ok(other.clone())
            });
            prop_assert!(matches!(changed, Err(CellError::IdempotencyConflict(_))));
            prop_assert_eq!(calls, usize::from(!committed));
            prop_assert!(
                matches!(
                    CellStore::from_engine_bound(open(), model(), &other),
                    Err(CellError::BindingMismatch { .. })
                ),
                "a bound reopen under another identity must refuse"
            );
            prop_assert_eq!(
                CellStore::from_engine_bound(open(), model(), &first)?.cell_id()?,
                Some(first)
            );
            Ok(())
        },
    );
}
