#![allow(
    clippy::arithmetic_side_effects,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::too_many_lines,
    missing_docs,
    reason = "integration-test assertions"
)]
//! #1423: once a cell has installed definitions, later installs and the
//! writes between them never leave a cell its own open refuses. A creation's
//! ledger payload is proven against the stored snapshot under the model its
//! commit pins, never only under today's model. (Writes before the first
//! install, deletes, type removal and transitions are #1426.)

use norn_cell::test_door::TestCommit as _;
use std::collections::BTreeMap;

use norn_cell::{
    CellStore, CommitBatch, FactWriteRow, InstalledDefinitionInput, MAIN_WORLD, Model,
    RestoreError, StoreOptions,
};
use norn_ir::{ObjectTypeDef, RuntimeValue as Value};
use norn_testkit::proptest::prelude::*;
use serde_json::json;

const SCOPE: &str = "org_demo/dev";

fn item(extra: &[serde_json::Value]) -> serde_json::Value {
    let mut properties = vec![
        json!({ "id": "code", "type": { "kind": "text" } }),
        json!({ "id": "note", "type": { "kind": "text" }, "constraints": { "nullable": true } }),
        json!({ "id": "tier", "type": { "kind": "text" }, "default": "standard" }),
    ];
    properties.extend(extra.iter().cloned());
    json!({ "name": "install.Item", "properties": properties })
}

fn definitions(definition: &serde_json::Value) -> (Model, String) {
    let typed: ObjectTypeDef = serde_json::from_value(definition.clone()).expect("typed");
    let model = Model::from_definitions(&[typed], &[]).expect("model");
    let artifacts = json!([{
        "irVersion": "0.1", "artifactId": "install.Item.entity", "kind": "entity", "namespace": "install", "name": "Item", "version": "0.1.0", "lifecycle": "draft",
        "scope": { "orgId": "org_demo", "environmentId": "dev" }, "dependencies": [],
        "provenance": { "source": "authoring_sdk", "sourcePath": "test" },
        "payload": { "entityVersion": "2.0", "objectType": definition }
    }]);
    let canonical = Value::from_json(artifacts)
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

/// A creation that names only `code`: the writer fills `note` (null) and
/// `tier` (its default), so the stored snapshot is not the payload.
fn create(key: &str, at: u64) -> CommitBatch {
    let event = Value::from_json(json!({
        "op": "created", "key": { "objectType": "install.Item", "key": key },
        "values": { "code": key }
    }))
    .expect("event");
    CommitBatch {
        world: MAIN_WORLD.to_owned(),
        scope: SCOPE.to_owned(),
        logical_time: at,
        events: vec![event],
        writes: vec![FactWriteRow::Put {
            object_type: "install.Item".to_owned(),
            key: key.to_owned(),
            values: BTreeMap::from([("code".to_owned(), Value::from(key))]),
        }],
        ..CommitBatch::default()
    }
}

#[test]
fn a_creation_stays_provable_after_an_install_changes_the_model() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("cell.sqlite");
    let (first, first_set) = definitions(&item(&[]));
    // The migration of #1423: a new member with a default for live rows.
    let (second, second_set) = definitions(&item(&[
        json!({ "id": "started_on", "type": { "kind": "text" }, "default": "1970-01-01" }),
    ]));
    assert_ne!(first.model_hash(), second.model_hash());

    let mut store = CellStore::open(&path, first.clone(), StoreOptions::default()).expect("store");
    store.bind("scope", SCOPE).expect("bind");
    store
        .install_definitions(first, input(&first_set, 10), Some(0), SCOPE)
        .expect("first definitions");
    store
        .commit(&create("a", 15))
        .expect("created under the first model");
    store
        .install_definitions(second.clone(), input(&second_set, 20), Some(1), SCOPE)
        .expect("second definitions");
    store
        .commit(&create("b", 25))
        .expect("created under the second model");
    drop(store);

    let reopened = CellStore::open(&path, second, StoreOptions::default())
        .expect("a cell its own installs produced opens");
    let read = reopened.read();
    let ledger = read.ledger(0).expect("ledger");
    let proven = ledger
        .iter()
        .filter(|entry| entry.event.get("op").and_then(Value::as_str) == Some("created"))
        .map(|entry| read.verified_history_event(entry).expect("proof"))
        .collect::<Vec<_>>();
    assert_eq!(proven.len(), 2);
    assert!(proven.iter().all(Option::is_some), "{proven:?}");
}

/// A row written before an install that adds a nullable member still reads
/// as the row its version's proof commits to: quality reads of it answer.
#[test]
fn a_row_stays_proven_after_an_install_adds_a_nullable_member() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("cell.sqlite");
    let (first, first_set) = definitions(&item(&[]));
    let (second, second_set) = definitions(&item(&[json!({
        "id": "remark", "type": { "kind": "text" }, "constraints": { "nullable": true }
    })]));

    let mut store = CellStore::open(&path, first.clone(), StoreOptions::default()).expect("store");
    store.bind("scope", SCOPE).expect("bind");
    store
        .install_definitions(first, input(&first_set, 10), Some(0), SCOPE)
        .expect("first definitions");
    store.commit(&create("a", 15)).expect("created");
    store
        .install_definitions(second, input(&second_set, 20), Some(1), SCOPE)
        .expect("second definitions");

    let read = store.read();
    let row = read
        .object(MAIN_WORLD, "install.Item", "a")
        .expect("read")
        .expect("row");
    assert_eq!(row.values.get("remark"), Some(&Value::Null));
    assert_eq!(
        read.observation_findings(MAIN_WORLD, "install.Item", "a")
            .expect("the row is the one its proof commits to"),
        []
    );
    assert!(
        !read
            .observation_is_quarantined(MAIN_WORLD, &row)
            .expect("quarantine")
    );
}

/// [`create`], with `note` holding a value.
fn create_noted(key: &str, at: u64) -> CommitBatch {
    let mut batch = create(key, at);
    batch.events = vec![
        Value::from_json(json!({
            "op": "created", "key": { "objectType": "install.Item", "key": key },
            "values": { "code": key, "note": "foo" }
        }))
        .expect("event"),
    ];
    if let Some(FactWriteRow::Put { values, .. }) = batch.writes.first_mut() {
        values.insert("note".to_owned(), Value::from("foo"));
    }
    batch
}

/// Edit every stored item row outside the cell: `set` is a SQL assignment.
fn edit_rows(path: &std::path::Path, set: &str) {
    use norn_cell::engine::{NativeEngine, SqlEngine as _};
    let engine = NativeEngine::open(path, StoreOptions::default()).expect("engine");
    let table: String = engine
        .query_row(
            "SELECT name FROM sqlite_master WHERE type = 'table' AND sql LIKE '%\"p:code\"%'",
            &[],
            |row| row.get(0),
        )
        .expect("the item table");
    engine
        .execute_batch(&format!("UPDATE \"{table}\" SET {set}"))
        .expect("edited");
}

/// Counting only held values never proves an edited row. A value the proof
/// holds and the row lost, a value the row holds where the proof holds none,
/// and a value in a member the proof never had (one installed later) are all
/// unproven corrections.
#[test]
fn a_row_edited_outside_the_cell_is_an_unproven_correction() {
    let (first, first_set) = definitions(&item(&[]));
    let (second, second_set) = definitions(&item(&[json!({
        "id": "remark", "type": { "kind": "text" }, "constraints": { "nullable": true }
    })]));
    // Whether the row was created holding a note, the edit, and whether the
    // install that adds `remark` came first.
    for (noted, set, grown) in [
        (true, "\"p:note\" = NULL", false),
        (false, "\"p:note\" = \"p:code\"", false),
        (false, "\"p:remark\" = \"p:code\"", true),
    ] {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("cell.sqlite");
        let mut store =
            CellStore::open(&path, first.clone(), StoreOptions::default()).expect("store");
        store.bind("scope", SCOPE).expect("bind");
        store
            .install_definitions(first.clone(), input(&first_set, 10), Some(0), SCOPE)
            .expect("first definitions");
        let created = if noted {
            create_noted("a", 15)
        } else {
            create("a", 15)
        };
        store.commit(&created).expect("created");
        let served = if grown {
            store
                .install_definitions(second.clone(), input(&second_set, 20), Some(1), SCOPE)
                .expect("second definitions");
            second.clone()
        } else {
            first.clone()
        };
        // Unedited, the row is the one its proof commits to.
        assert_eq!(
            store
                .read()
                .observation_findings(MAIN_WORLD, "install.Item", "a")
                .expect("proven"),
            [],
            "{set}"
        );
        drop(store);
        edit_rows(&path, set);
        let reopened = CellStore::open(&path, served, StoreOptions::default()).expect("reopened");
        let refused = reopened
            .read()
            .observation_findings(MAIN_WORLD, "install.Item", "a");
        assert!(
            matches!(refused, Err(norn_cell::CellError::UnprovenCorrection)),
            "{set}: {refused:?}"
        );
    }
}

/// The members #1750 installs on live rows: each declares a default, which
/// the install materializes on the rows that were already there.
fn defaulted(grade: &str) -> [serde_json::Value; 2] {
    [
        json!({ "id": "grade", "type": { "kind": "text" }, "default": grade }),
        json!({ "id": "weight", "type": { "kind": "integer" }, "default": "3" }),
    ]
}

/// #1750: a row written before an install that adds members with declared
/// defaults holds those defaults, which its version's proof never named. It
/// is still the row that proof commits to, so quality reads of it answer,
/// also after a later install changes one default and keeps the
/// materialized value.
#[test]
fn a_row_stays_proven_after_an_install_adds_defaulted_members() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("cell.sqlite");
    let (first, first_set) = definitions(&item(&[]));
    let (second, second_set) = definitions(&item(&defaulted("standard")));
    let (third, third_set) = definitions(&item(&defaulted("premium")));

    let mut store = CellStore::open(&path, first.clone(), StoreOptions::default()).expect("store");
    store.bind("scope", SCOPE).expect("bind");
    store
        .install_definitions(first, input(&first_set, 10), Some(0), SCOPE)
        .expect("first definitions");
    store.commit(&create("a", 15)).expect("created");
    let proven = |store: &CellStore, grade: &str| {
        let read = store.read();
        let row = read
            .object(MAIN_WORLD, "install.Item", "a")
            .expect("read")
            .expect("row");
        assert_eq!(row.values.get("grade"), Some(&Value::from(grade)));
        let weight = serde_json::to_value(row.values.get("weight")).expect("json");
        assert_eq!(weight.to_string().trim_matches('"'), "3");
        assert_eq!(
            read.observation_findings(MAIN_WORLD, "install.Item", "a")
                .expect("the row is the one its proof commits to"),
            []
        );
        assert!(
            !read
                .observation_is_quarantined(MAIN_WORLD, &row)
                .expect("quarantine")
        );
        assert!(
            read.authoritative_object(MAIN_WORLD, "install.Item", "a")
                .expect("authoritative read")
                .is_some()
        );
    };
    store
        .install_definitions(second, input(&second_set, 20), Some(1), SCOPE)
        .expect("second definitions");
    proven(&store, "standard");
    // A row created under the second model names both members in its proof.
    store.commit(&create("b", 25)).expect("created");
    assert_eq!(
        store
            .read()
            .observation_findings(MAIN_WORLD, "install.Item", "b")
            .expect("proven"),
        []
    );
    store
        .install_definitions(third.clone(), input(&third_set, 30), Some(2), SCOPE)
        .expect("third definitions");
    proven(&store, "standard");
    drop(store);
    let reopened = CellStore::open(&path, third, StoreOptions::default()).expect("reopened");
    proven(&reopened, "standard");
}

/// #1750: accepting an installed default proves nothing else. A member the
/// proof never named that holds any other value, and any value of a member
/// the row's version committed to that was changed or erased, are unproven
/// corrections, before and after a later install changes the default.
#[test]
fn an_installed_default_does_not_prove_an_edited_row() {
    let (first, first_set) = definitions(&item(&[]));
    let (second, second_set) = definitions(&item(&defaulted("standard")));
    let (third, third_set) = definitions(&item(&defaulted("premium")));
    for (set, changed_default) in [
        // The installed member holds a value that is not its default.
        ("\"p:grade\" = 'forged'", false),
        ("\"p:weight\" = 4", false),
        // Neither the introducing default nor the active one.
        ("\"p:grade\" = 'forged'", true),
        // A member the row's version committed to is changed or erased.
        ("\"p:code\" = 'forged'", false),
        ("\"p:tier\" = 'forged'", true),
        ("\"p:note\" = NULL", false),
        ("\"p:note\" = 'forged'", true),
    ] {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("cell.sqlite");
        let mut store =
            CellStore::open(&path, first.clone(), StoreOptions::default()).expect("store");
        store.bind("scope", SCOPE).expect("bind");
        store
            .install_definitions(first.clone(), input(&first_set, 10), Some(0), SCOPE)
            .expect("first definitions");
        store.commit(&create_noted("a", 15)).expect("created");
        store
            .install_definitions(second.clone(), input(&second_set, 20), Some(1), SCOPE)
            .expect("second definitions");
        let served = if changed_default {
            store
                .install_definitions(third.clone(), input(&third_set, 30), Some(2), SCOPE)
                .expect("third definitions");
            third.clone()
        } else {
            second.clone()
        };
        assert_eq!(
            store
                .read()
                .observation_findings(MAIN_WORLD, "install.Item", "a")
                .expect("proven"),
            [],
            "{set}"
        );
        drop(store);
        edit_rows(&path, set);
        let reopened = CellStore::open(&path, served, StoreOptions::default()).expect("reopened");
        let refused = reopened
            .read()
            .observation_findings(MAIN_WORLD, "install.Item", "a");
        assert!(
            matches!(refused, Err(norn_cell::CellError::UnprovenCorrection)),
            "{set} (changed default: {changed_default}): {refused:?}"
        );
    }
}

#[derive(Debug, Clone)]
enum Step {
    /// Create `key`, its payload naming only `code` or every member.
    Create { key: u8, partial: bool },
    /// Change `code` of `key`; its payload is the record as read.
    Modify { key: u8 },
    /// Install the next model: one more member with a default.
    Install,
    /// Drop the store and open the file again.
    Reopen,
}

fn step() -> impl Strategy<Value = Step> {
    prop_oneof![
        (0_u8..4, any::<bool>()).prop_map(|(key, partial)| Step::Create { key, partial }),
        (0_u8..4).prop_map(|key| Step::Modify { key }),
        Just(Step::Install),
        Just(Step::Reopen),
    ]
}

/// The model after `installs` migrations: `extra1`..`extraN`, each defaulted.
fn migrated(installs: usize) -> (Model, String) {
    let extra = (1..=installs)
        .map(|n| json!({ "id": format!("extra{n}"), "type": { "kind": "text" }, "default": format!("d{n}") }))
        .collect::<Vec<_>>();
    definitions(&item(&extra))
}

fn event(value: &serde_json::Value) -> Value {
    Value::from_json(value.clone()).expect("event")
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(48))]

    /// After the first install, any interleaving of admitted creations,
    /// modifications, definition installs and reopens leaves a cell that
    /// opens, with every creation and modification still proven by the shared
    /// history accessor.
    #[test]
    fn every_admitted_interleaving_reopens(steps in prop::collection::vec(step(), 1..24)) {
        let directory = tempfile::tempdir().expect("directory");
        let path = directory.path().join("cell.sqlite");
        let mut installs = 1_usize;
        let (model, set) = migrated(0);
        let mut store = CellStore::open(&path, model.clone(), StoreOptions::default()).expect("store");
        store.bind("scope", SCOPE).expect("bind");
        store.install_definitions(model, input(&set, 1), Some(0), SCOPE).expect("first definitions");
        let mut clock = 10_u64;
        for step in steps {
            clock += 10;
            match step {
                Step::Create { key, partial } => {
                    let key = format!("k{key}");
                    if store.read().object(MAIN_WORLD, "install.Item", &key).expect("read").is_some() {
                        continue;
                    }
                    let mut values = serde_json::Map::from_iter([("code".to_owned(), json!(key))]);
                    if !partial {
                        values.insert("note".to_owned(), serde_json::Value::Null);
                        values.insert("tier".to_owned(), json!("standard"));
                        for n in 1..installs {
                            values.insert(format!("extra{n}"), json!(format!("d{n}")));
                        }
                    }
                    let mut batch = create(&key, clock);
                    batch.events = vec![event(&json!({ "op": "created",
                        "key": { "objectType": "install.Item", "key": key }, "values": values }))];
                    store.commit(&batch).expect("an admitted creation");
                }
                Step::Modify { key } => {
                    let key = format!("k{key}");
                    let Some(record) = store.read().object(MAIN_WORLD, "install.Item", &key).expect("read") else {
                        continue;
                    };
                    let code = format!("{key}-{clock}");
                    let before = record.values.clone();
                    let mut after = before.clone();
                    after.insert("code".to_owned(), Value::from(code.as_str()));
                    let mut batch = create(&key, clock);
                    batch.writes = vec![FactWriteRow::Put {
                        object_type: "install.Item".to_owned(),
                        key: key.clone(),
                        values: BTreeMap::from([("code".to_owned(), Value::from(code.as_str()))]),
                    }];
                    let mut modified = event(&json!({ "op": "modified",
                        "key": { "objectType": "install.Item", "key": key },
                        "beforeVersion": record.version, "afterVersion": record.version + 1,
                        "changedProperties": ["code"] }));
                    let fields = modified.as_object_mut().expect("event object");
                    fields.insert("before".to_owned(), Value::Object(before));
                    fields.insert("after".to_owned(), Value::Object(after));
                    batch.events = vec![modified];
                    store.commit(&batch).expect("an admitted modification");
                }
                Step::Install => {
                    let (model, set) = migrated(installs);
                    store
                        .install_definitions(model, input(&set, clock), Some(u64::try_from(installs).expect("small")), SCOPE)
                        .expect("an admitted install");
                    installs += 1;
                }
                Step::Reopen => {
                    drop(store);
                    store = CellStore::open(&path, migrated(installs - 1).0, StoreOptions::default())
                        .expect("a cell its own admitted history produced opens");
                }
            }
        }
        drop(store);
        let store = CellStore::open(&path, migrated(installs - 1).0, StoreOptions::default())
            .expect("a cell its own admitted history produced opens");
        let read = store.read();
        for entry in read.ledger(0).expect("ledger") {
            if matches!(entry.event.get("op").and_then(Value::as_str), Some("created" | "modified")) {
                prop_assert!(read.verified_history_event(&entry).expect("proof").is_some(), "{:?}", entry.event);
            }
        }
    }
}

/// The operator path for a cell whose open refuses, whatever the cause: the
/// diagnosis names the first refused entry without writing the file, and the
/// restore rebuilds the proven prefix into a new file that opens. A journal
/// or receipt table that lost its tail, or all three logs at once, reads as a
/// cut too, never as a fully proven history.
#[tokio::test]
async fn a_refused_cell_is_diagnosed_and_restored_to_its_proven_prefix() {
    for damage in [
        "snapshot",
        "journal",
        "receipt",
        "altered-receipt",
        "ledger-tail",
        "altered-event",
        "all-logs",
    ] {
        refused_cell_restores(damage).await;
    }
}

async fn refused_cell_restores(damage: &str) {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("cell.sqlite");
    let (model, set) = migrated(0);
    let mut store = CellStore::open(&path, model.clone(), StoreOptions::default()).expect("store");
    store.bind("scope", SCOPE).expect("bind");
    store
        .install_definitions(model.clone(), input(&set, 10), Some(0), SCOPE)
        .expect("definitions");
    for (n, key) in ["a", "b", "c"].into_iter().enumerate() {
        store
            .commit(&create(key, 20 + u64::try_from(n).expect("small")))
            .expect("created");
    }
    drop(store);
    // Stand-in for any unprovable commit: the last creation's snapshot no
    // longer matches its commitment.
    // Or: the journal lost its last record, so a restore can rebuild no
    // further than commit 3.
    let raw = rusqlite::Connection::open(&path).expect("operator seam");
    let residue: String = raw
        .query_row(
            "SELECT record FROM _norn_export_journal WHERE commit_seq = 4",
            [],
            |row| row.get(0),
        )
        .expect("journal row");
    assert!(residue.starts_with("norn.cell.export-residue/1:"));
    if damage == "snapshot" {
        raw.execute(
            "UPDATE _norn_fact_values SET hash = ?1 WHERE commit_seq = 4",
            [&"0".repeat(64)],
        )
        .expect("tamper");
    } else if damage == "journal" {
        raw.execute("DELETE FROM _norn_export_journal WHERE commit_seq = 4", [])
            .expect("tamper");
    } else if damage == "receipt" {
        // Or the receipts lost their last row.
        raw.execute("DELETE FROM _norn_receipt WHERE commit_seq = 4", [])
            .expect("tamper");
    } else if damage == "altered-receipt" {
        // Commit 4's journal row is a residue (EXPORT-RESIDUE-1): each change
        // to a row it is rebuilt from cuts the provable journal there too.
        raw.execute(
            "UPDATE _norn_receipt SET receipt = replace(receipt, '\"logicalTime\":22', '\"logicalTime\":23') WHERE commit_seq = 4",
            [],
        )
        .expect("tamper");
    } else if damage == "ledger-tail" {
        raw.execute("DELETE FROM _norn_ledger WHERE commit_seq = 4", [])
            .expect("tamper");
    } else if damage == "altered-event" {
        raw.execute(
            "UPDATE _norn_ledger SET event = replace(event, '\"c\"', '\"z\"') WHERE commit_seq = 4",
            [],
        )
        .expect("tamper");
    } else {
        // Or all three logs lost commit 4 together; the clock still names it.
        for table in ["_norn_receipt", "_norn_ledger", "_norn_export_journal"] {
            raw.execute(&format!("DELETE FROM {table} WHERE commit_seq = 4"), [])
                .expect("tamper");
        }
    }
    drop(raw);
    if damage == "snapshot" {
        assert!(CellStore::open(&path, model.clone(), StoreOptions::default()).is_err());
    }
    let before = std::fs::read(&path).expect("source bytes");

    let diagnosis = CellStore::diagnose_history(&path).expect("diagnosis");
    assert_eq!(diagnosis.head_commit, 4, "{damage}");
    assert_eq!(diagnosis.proven_through, 3, "{damage}");
    // Commit 4's journal row is a residue rebuilt from its receipt and ledger
    // span (EXPORT-RESIDUE-1), so a lost or changed one ends the journal there.
    if matches!(damage, "receipt" | "altered-receipt" | "altered-event") {
        assert_eq!(diagnosis.journal_commits, 3);
        let unproven = diagnosis
            .first_unproven
            .as_ref()
            .expect("the refused entry");
        assert_eq!(
            (unproven.commit_seq, unproven.op.as_deref()),
            (4, Some("created"))
        );
    } else if matches!(damage, "journal" | "ledger-tail" | "all-logs") {
        assert_eq!(diagnosis.journal_commits, 3);
        assert_eq!(diagnosis.first_unproven, None);
    } else {
        assert_eq!(diagnosis.journal_commits, 4);
        let unproven = diagnosis
            .first_unproven
            .as_ref()
            .expect("the refused entry");
        assert_eq!(
            (unproven.commit_seq, unproven.op.as_deref()),
            (4, Some("created"))
        );
    }

    let target = directory.path().join("restored.sqlite");
    let staging = directory.path().join("staging");
    // A cut the operator has not named is refused, whatever else holds.
    for through in [None, Some(4), Some(2)] {
        let refused = CellStore::restore_proven_prefix(
            &path,
            &target,
            &staging,
            through,
            StoreOptions::default(),
        )
        .await;
        assert!(
            matches!(
                refused,
                Err(RestoreError::PrefixNotAcknowledged {
                    proven_through: 3,
                    head_commit: 4
                })
            ),
            "{through:?}: {refused:?}"
        );
    }
    assert!(!target.exists() && !staging.exists(), "{damage}");
    let restored = CellStore::restore_proven_prefix(
        &path,
        &target,
        &staging,
        Some(3),
        StoreOptions::default(),
    )
    .await
    .expect("restore");
    assert_eq!(restored.diagnosis, diagnosis);
    assert_eq!(restored.report.ledger_head.commits, 3);
    assert_eq!(
        std::fs::read(&path).expect("source bytes"),
        before,
        "the source is only read"
    );

    let reopened = CellStore::open(&target, model, StoreOptions::default())
        .expect("the restored prefix opens");
    let read = reopened.read();
    for (key, present) in [("a", true), ("b", true), ("c", false)] {
        assert_eq!(
            read.object(MAIN_WORLD, "install.Item", key)
                .expect("read")
                .is_some(),
            present,
            "{key}"
        );
    }
}
