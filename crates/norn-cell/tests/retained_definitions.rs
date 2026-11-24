#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::too_many_lines,
    missing_docs,
    reason = "integration-test crate"
)]
//! Retained (non-kernel) definitions ride the governed definitions install:
//! one revision per commit, compare-and-set per id, history kept, and the
//! revision travels in the export record so a restore rebuilds it.

use std::collections::BTreeMap;

use norn_cell::export::Exporter;
use norn_cell::retained::{
    RetainedDefinition, RetainedDefinitionChange, RetainedLifecycle, RetainedSignoff,
    payload_sha256,
};
use norn_cell::{CellError, CellStore, InstalledDefinitionInput, Model, StoreOptions};
use norn_ir::{ObjectTypeDef, RuntimeValue};
use serde_json::json;

const SCOPE: &str = "org_demo/dev";

fn definitions() -> (Model, String) {
    let definition = json!({ "name": "install.Item", "properties": [{ "id": "code", "type": { "kind": "text" } }] });
    let typed: ObjectTypeDef = serde_json::from_value(definition.clone()).expect("typed");
    let model = Model::from_definitions(&[typed], &[]).expect("model");
    let artifacts = json!([{
        "irVersion": "0.1", "artifactId": "install.Item.entity", "kind": "entity", "namespace": "install", "name": "Item", "version": "0.1.0", "lifecycle": "draft",
        "scope": { "orgId": "org_demo", "environmentId": "dev" }, "dependencies": [],
        "provenance": { "source": "authoring_sdk", "sourcePath": "test" },
        "payload": { "entityVersion": "2.0", "objectType": definition }
    }]);
    let canonical = RuntimeValue::from_json(artifacts)
        .expect("exact")
        .canonical_json()
        .expect("canonical");
    (model, String::from_utf8(canonical).expect("utf8"))
}

fn input(canonical_set: &str, at: u64) -> InstalledDefinitionInput {
    InstalledDefinitionInput {
        canonical_set: canonical_set.to_owned(),
        registry: Vec::new(),
        receipt_sha256: "a".repeat(64),
        admission_receipt: None,
        readmission: None,
        installed_at: at,
        diff: json!({ "added": [], "removed": [], "changed": [], "narrowing": [] }),
        layout: None,
    }
}

fn change(
    id: &str,
    revision: u64,
    lifecycle: RetainedLifecycle,
    title: &str,
    expected: Option<u64>,
) -> RetainedDefinitionChange {
    let payload = format!(r#"{{"tiles":[],"title":"{title}"}}"#);
    RetainedDefinitionChange {
        definition: RetainedDefinition {
            kind: "dashboard".to_owned(),
            id: id.to_owned(),
            revision,
            lifecycle,
            title: title.to_owned(),
            payload_sha256: payload_sha256(&payload),
            payload,
            signoff: None,
            authority: None,
        },
        expected_revision: expected,
    }
}

#[tokio::test]
async fn revisions_install_atomically_compare_and_set_and_survive_restore() {
    let directory = tempfile::tempdir().expect("directory");
    let (model, canonical) = definitions();
    let mut store = CellStore::open_in_memory(model.clone()).expect("store");
    store.bind("scope", SCOPE).expect("bind");
    store
        .install_definitions(model.clone(), input(&canonical, 10), Some(0), SCOPE)
        .expect("kernel definitions");

    let mut first = change(
        "studio.dashboard.a",
        1,
        RetainedLifecycle::Active,
        "A",
        None,
    );
    first.definition.signoff = Some(RetainedSignoff {
        signer: "user:u1".to_owned(),
        signed_at: 20,
        restatement_sha256: "b".repeat(64),
    });
    let installed = store
        .install_retained_definition(
            model.clone(),
            &input(&canonical, 20),
            Some(1),
            SCOPE,
            &first,
        )
        .expect("first revision");
    assert_eq!(installed.version, 2);
    assert_eq!(installed.canonical_set, canonical);

    // Fable S6: a revision that is not a flow cannot change the model.
    let other_model = Model::from_definitions(&[], &[]).expect("another model");
    let remodelled = change(
        "studio.dashboard.m",
        1,
        RetainedLifecycle::Active,
        "M",
        None,
    );
    assert!(matches!(
        store.install_retained_definition(
            other_model,
            &input(&canonical, 21),
            Some(2),
            SCOPE,
            &remodelled
        ),
        Err(CellError::DefinitionReceiptInvalid(message))
            if message.contains("not a flow installs over the active model")
    ));

    // An unused id is required without an expected revision.
    let again = change(
        "studio.dashboard.a",
        1,
        RetainedLifecycle::Active,
        "A2",
        None,
    );
    assert!(matches!(
        store.install_retained_definition(
            model.clone(),
            &input(&canonical, 21),
            Some(2),
            SCOPE,
            &again
        ),
        Err(CellError::RetainedDefinitionConflict {
            expected: None,
            actual: Some(1),
            ..
        })
    ));
    // A stale expected revision conflicts and changes nothing.
    let stale = change(
        "studio.dashboard.a",
        3,
        RetainedLifecycle::Active,
        "A3",
        Some(2),
    );
    assert!(matches!(
        store.install_retained_definition(
            model.clone(),
            &input(&canonical, 22),
            Some(2),
            SCOPE,
            &stale
        ),
        Err(CellError::RetainedDefinitionConflict {
            actual: Some(1),
            ..
        })
    ));
    assert_eq!(
        store
            .read()
            .installed_definitions(None)
            .expect("read")
            .expect("active")
            .version,
        2
    );

    let second = change(
        "studio.dashboard.a",
        2,
        RetainedLifecycle::Active,
        "A2",
        Some(1),
    );
    store
        .install_retained_definition(
            model.clone(),
            &input(&canonical, 30),
            Some(2),
            SCOPE,
            &second,
        )
        .expect("second revision");
    let other = change(
        "studio.dashboard.b",
        1,
        RetainedLifecycle::Active,
        "B",
        None,
    );
    store
        .install_retained_definition(
            model.clone(),
            &input(&canonical, 40),
            Some(3),
            SCOPE,
            &other,
        )
        .expect("other id");
    let retired = change(
        "studio.dashboard.b",
        2,
        RetainedLifecycle::Retired,
        "B",
        Some(1),
    );
    store
        .install_retained_definition(
            model.clone(),
            &input(&canonical, 50),
            Some(4),
            SCOPE,
            &retired,
        )
        .expect("retire");

    let reader = store.read();
    let latest = reader
        .retained_definition("studio.dashboard.a", None)
        .expect("read")
        .expect("latest");
    assert_eq!(latest.definition, second.definition);
    assert_eq!(latest.definitions_version, 3);
    assert_eq!(latest.recorded_at, 30);
    assert_eq!(
        reader
            .retained_definition("studio.dashboard.a", Some(1))
            .expect("read")
            .expect("history")
            .definition,
        first.definition
    );
    let active = reader
        .retained_definitions("dashboard", false, None, 10)
        .expect("list");
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].definition.id, "studio.dashboard.a");
    let all = reader
        .retained_definitions("dashboard", true, None, 10)
        .expect("list");
    assert_eq!(all.len(), 2);
    assert_eq!(all[1].definition.lifecycle, RetainedLifecycle::Retired);
    let page = reader
        .retained_definitions("dashboard", true, Some("studio.dashboard.a"), 1)
        .expect("page");
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].definition.id, "studio.dashboard.b");
    assert_eq!(
        reader
            .retained_definitions("agent", true, None, 10)
            .expect("kind"),
        []
    );

    // The export journal carries each revision; a restore rebuilds them.
    let records: Vec<_> = (1..=5)
        .map(|seq| store.export_record(seq).expect("journal").expect("record"))
        .collect();
    assert_eq!(
        records[1]
            .definition_install
            .as_ref()
            .expect("install")
            .retained,
        Some(first)
    );
    let mut exporter = Exporter::local(&directory.path().join("export")).expect("exporter");
    for record in &records {
        exporter.append(record).await.expect("append");
    }
    exporter.verify().await.expect("verify");
    let restored_path = directory.path().join("restored.sqlite");
    // CELL-ID-RESTORE: restore names the source's logical identity.
    let bindings = BTreeMap::from([
        ("scope".to_owned(), SCOPE.to_owned()),
        ("cell_id".to_owned(), records[0].bindings["cell_id"].clone()),
        (
            "cell_identity_format".to_owned(),
            norn_cell::cell_identity::FORMAT.to_owned(),
        ),
    ]);
    CellStore::restore_from(
        &mut exporter,
        &restored_path,
        model.clone(),
        SCOPE,
        &bindings,
        StoreOptions::default(),
    )
    .await
    .expect("restore");
    let restored = CellStore::open(&restored_path, model, StoreOptions::default()).expect("reopen");
    assert_eq!(
        restored
            .read()
            .retained_definitions("dashboard", true, None, 10)
            .expect("restored list"),
        all
    );
}

/// A retained revision never changes the kernel: installing one over another
/// set or receipt is refused, so every definitions version a retained save
/// commits has the kernel identity (receipt) of the one before it.
#[test]
fn a_retained_revision_cannot_change_the_kernel_set_or_receipt() {
    let (model, canonical) = definitions();
    let mut store = CellStore::open_in_memory(model.clone()).expect("store");
    store.bind("scope", SCOPE).expect("bind");
    store
        .install_definitions(model, input(&canonical, 10), Some(0), SCOPE)
        .expect("kernel definitions");
    let dashboard = change(
        "studio.dashboard.k",
        1,
        RetainedLifecycle::Active,
        "K",
        None,
    );
    let mut other_receipt = input(&canonical, 20);
    other_receipt.receipt_sha256 = "c".repeat(64);
    let mut other_set = input(&canonical, 20);
    other_set.canonical_set = "[]".to_owned();
    for changed in [other_receipt, other_set] {
        assert!(matches!(
            store.install_retained_definition(
                store.model().clone(),
                &changed,
                Some(1),
                SCOPE,
                &dashboard
            ),
            Err(CellError::DefinitionReceiptInvalid(_))
        ));
    }
    let installed = store
        .install_retained_definition(
            store.model().clone(),
            &input(&canonical, 20),
            Some(1),
            SCOPE,
            &dashboard,
        )
        .expect("unchanged kernel");
    let before = store
        .read()
        .installed_definitions(Some(1))
        .expect("read")
        .expect("v1");
    assert_eq!(installed.version, 2);
    assert_eq!(installed.receipt_sha256, before.receipt_sha256);
}

/// A cell provisioned before the retained table existed gains it when the
/// provisioning path reopens it, so retained installs and reads work.
#[test]
fn provisioning_reopen_adds_the_retained_table_to_an_older_file() {
    use norn_cell::engine::{NativeEngine, SqlEngine};

    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("cell.sqlite");
    let (model, canonical) = definitions();
    let kernel = input(&canonical, 10);
    let open = || NativeEngine::open(&path, StoreOptions::default()).expect("engine");
    drop(
        CellStore::provision_definitions(open(), model.clone(), &kernel, SCOPE)
            .expect("fresh provisioning"),
    );

    // An older file: the retained table is absent.
    let engine = open();
    engine
        .definition_writes(|| engine.execute_batch(r#"DROP TABLE "_norn_retained_definition""#))
        .expect("drop retained table");
    drop(engine);

    let mut store = CellStore::provision_definitions(open(), model, &kernel, SCOPE)
        .expect("provisioning reopen of an older file");
    let dashboard = change(
        "studio.dashboard.old",
        1,
        RetainedLifecycle::Active,
        "Old",
        None,
    );
    store
        .install_retained_definition(
            store.model().clone(),
            &input(&canonical, 20),
            Some(1),
            SCOPE,
            &dashboard,
        )
        .expect("retained install on the reopened file");
    let listed = store
        .read()
        .retained_definitions("dashboard", false, None, 10)
        .expect("retained listing on the reopened file");
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].definition.id, "studio.dashboard.old");
}

/// #1469: a kernel set and the retained revisions riding it commit in one
/// transaction. One revision that is not the next of its id refuses the
/// whole install: the set stays the active one and no revision is written.
/// A good install commits both, and its journal record replays on restore.
#[tokio::test]
async fn riding_revisions_publish_with_their_set_or_not_at_all() {
    let directory = tempfile::tempdir().expect("directory");
    let (model, canonical) = definitions();
    let mut store = CellStore::open_in_memory(model.clone()).expect("store");
    store.bind("scope", SCOPE).expect("bind");
    store
        .install_definitions(model.clone(), input(&canonical, 10), Some(0), SCOPE)
        .expect("kernel definitions");
    let next = canonical.replace("\"0.1.0\"", "\"0.2.0\"");
    assert_ne!(next, canonical);
    let gate = change("lab.gate", 1, RetainedLifecycle::Active, "G", None);
    let stale = change("lab.other", 2, RetainedLifecycle::Active, "O", Some(1));
    assert!(matches!(
        store.install_definitions_riding(
            model.clone(),
            &input(&next, 20),
            Some(1),
            SCOPE,
            &[gate.clone(), stale]
        ),
        Err(CellError::RetainedDefinitionConflict { .. })
    ));
    let active = store
        .read()
        .installed_definitions(None)
        .expect("read")
        .expect("active");
    assert_eq!((active.version, &active.canonical_set), (1, &canonical));
    assert!(
        store
            .read()
            .retained_definition("lab.gate", None)
            .expect("read")
            .is_none(),
        "no revision of a refused install is written"
    );

    let other = change("lab.other", 1, RetainedLifecycle::Active, "O", None);
    let installed = store
        .install_definitions_riding(
            model.clone(),
            &input(&next, 30),
            Some(1),
            SCOPE,
            &[gate.clone(), other.clone()],
        )
        .expect("set and revisions");
    assert_eq!((installed.version, &installed.canonical_set), (2, &next));
    for id in ["lab.gate", "lab.other"] {
        let record = store
            .read()
            .retained_definition(id, None)
            .expect("read")
            .expect("installed");
        assert_eq!(record.definitions_version, 2, "{id}");
    }

    let records: Vec<_> = (1..=2)
        .map(|seq| store.export_record(seq).expect("journal").expect("record"))
        .collect();
    assert_eq!(
        records[1]
            .definition_install
            .as_ref()
            .expect("install")
            .riding,
        vec![gate, other]
    );
    let mut exporter = Exporter::local(&directory.path().join("export")).expect("exporter");
    for record in &records {
        exporter.append(record).await.expect("append");
    }
    exporter.verify().await.expect("verify");
    let restored_path = directory.path().join("restored.sqlite");
    let bindings = BTreeMap::from([
        ("scope".to_owned(), SCOPE.to_owned()),
        ("cell_id".to_owned(), records[0].bindings["cell_id"].clone()),
        (
            "cell_identity_format".to_owned(),
            norn_cell::cell_identity::FORMAT.to_owned(),
        ),
    ]);
    CellStore::restore_from(
        &mut exporter,
        &restored_path,
        model.clone(),
        SCOPE,
        &bindings,
        StoreOptions::default(),
    )
    .await
    .expect("restore");
    let restored = CellStore::open(&restored_path, model, StoreOptions::default()).expect("reopen");
    assert_eq!(
        restored
            .read()
            .retained_definitions("dashboard", false, None, 10)
            .expect("restored list")
            .len(),
        2
    );
}

/// An engine whose first read after an armed transaction commits fails,
/// as a lost connection between the commit and any readback would.
struct ReadbackFault {
    inner: norn_cell::engine::NativeEngine,
    armed: std::rc::Rc<std::cell::Cell<bool>>,
    tripped: std::rc::Rc<std::cell::Cell<bool>>,
}

impl norn_cell::engine::SqlEngine for ReadbackFault {
    fn read_isolation(&self) -> norn_cell::engine::ReadIsolation {
        self.inner.read_isolation()
    }
    fn definition_writes<T>(
        &self,
        f: impl FnOnce() -> Result<T, CellError>,
    ) -> Result<T, CellError> {
        self.inner.definition_writes(f)
    }

    fn execute(&self, sql: &str, params: &[norn_cell::engine::SqlValue]) -> Result<(), CellError> {
        self.inner.execute(sql, params)
    }
    fn query(
        &self,
        sql: &str,
        params: &[norn_cell::engine::SqlValue],
    ) -> Result<Vec<Vec<norn_cell::engine::SqlValue>>, CellError> {
        if self.tripped.replace(false) {
            return Err(CellError::Engine("readback fault".to_owned()));
        }
        self.inner.query(sql, params)
    }
    fn execute_batch(&self, sql: &str) -> Result<(), CellError> {
        self.inner.execute_batch(sql)
    }
    fn transaction<T, X: From<CellError>>(&self, f: impl FnOnce() -> Result<T, X>) -> Result<T, X> {
        let value = self.inner.transaction(f)?;
        if self.armed.replace(false) {
            self.tripped.set(true);
        }
        Ok(value)
    }
}

/// #1469 S1: an install answers the revision its own transaction committed,
/// never a separate readback. A read that fails after the commit cannot turn
/// a committed install into a refusal, so a host never keeps serving the
/// old revision's policy over the new set.
#[test]
fn an_install_answers_the_revision_its_transaction_committed() {
    let (model, canonical) = definitions();
    let armed = std::rc::Rc::new(std::cell::Cell::new(false));
    let tripped = std::rc::Rc::new(std::cell::Cell::new(false));
    let engine = ReadbackFault {
        inner: norn_cell::engine::NativeEngine::open_in_memory().expect("engine"),
        armed: std::rc::Rc::clone(&armed),
        tripped: std::rc::Rc::clone(&tripped),
    };
    let mut store = CellStore::from_engine(engine, model.clone()).expect("store");
    store.bind("scope", SCOPE).expect("bind");
    store
        .install_definitions(model.clone(), input(&canonical, 10), Some(0), SCOPE)
        .expect("kernel definitions");
    let next = canonical.replace("\"0.1.0\"", "\"0.2.0\"");
    let gate = change("lab.gate", 1, RetainedLifecycle::Active, "G", None);
    armed.set(true);
    let installed = store
        .install_definitions_riding(model, &input(&next, 20), Some(1), SCOPE, &[gate])
        .expect("the committed install is answered");
    assert_eq!((installed.version, &installed.canonical_set), (2, &next));
    tripped.set(false);
    let active = store
        .read()
        .installed_definitions(None)
        .expect("read")
        .expect("active");
    assert_eq!(active, installed, "the answer is the committed revision");
}
