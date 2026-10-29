#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs,
    reason = "integration-test assertions"
)]

//! Verified export → wipe → typed replay, and fail-closed tamper cases.

use std::collections::BTreeMap;

use norn_cell::export::{ExportError, ExportRecord, Exporter, MANIFEST_PATH, object_path};
use norn_cell::{
    CellStore, CommitBatch, Consumption, EffectRow, FactWriteRow, JournalMode, LinkChange, Model,
    OutboxCommand, RestoreError, StoreOptions, TimerEntry,
};
use norn_ir::RuntimeValue as Value;
use serde_json::json;

fn rv(value: serde_json::Value) -> Value {
    Value::from_json(value).expect("runtime value")
}
fn hash<T: serde::Serialize>(value: &T) -> String {
    norn_ir::canonical::HashProfile::Norn1
        .hash_canonical_bytes(&norn_ir::canonical::canonical_json_of(value).expect("canonical"))
}
fn model() -> Model {
    let def = serde_json::from_value(json!({
        "name":"restore.Item", "properties":[{"id":"name","type":{"kind":"text"}}],
        "links":[{"id":"next","target":"restore.Item","cardinality":{"to":"many","from":"many"}}]
    }))
    .expect("definition");
    Model::from_definitions(&[def], &[]).expect("model")
}

fn semantic_model(default: &str, target: &str) -> Model {
    let other = if target == "approved" {
        "declined"
    } else {
        "approved"
    };
    let def = serde_json::from_value(json!({
        "name":"restore.Item",
        "properties":[
            {"id":"name","type":{"kind":"text"},"default":default},
            {"id":"status","type":{"kind":"enum","values":[
                {"value":"draft"},{"value":"approved"},{"value":"declined"}
            ]},"default":"draft"}
        ],
        "lifecycles":[{"property":"status","initial":["draft"],
            "terminal":["approved","declined"],"transitions":[
                {"id":"finish","from":["draft"],"to":target},
                {"id":"other","from":["draft"],"to":other}
            ]}]
    }))
    .expect("definition");
    Model::from_definitions(&[def], &[]).expect("model")
}

async fn refuse_semantic_change(changed: Model, transition: bool) {
    let original = semantic_model("A", "approved");
    assert_eq!(original.ddl_hash(), changed.ddl_hash(), "identical DDL");
    assert_ne!(original.model_hash(), changed.model_hash());
    let dir = tempfile::tempdir().expect("directory");
    let target = dir.path().join("restored.sqlite");
    let mut store = CellStore::open_in_memory(original.clone()).expect("store");
    store.bind("scope", "org_demo/dev").expect("bind");
    let mut inputs = vec![CommitBatch {
        writes: vec![FactWriteRow::Put {
            object_type: "restore.Item".to_owned(),
            key: "a".to_owned(),
            values: BTreeMap::new(),
        }],
        ..batch(1)
    }];
    if transition {
        inputs.push(CommitBatch {
            facts: vec![
                serde_json::from_value(json!({
                    "op":"transition","objectType":"restore.Item","key":"a",
                    "lifecycle":"status","transition":"finish"
                }))
                .expect("transition"),
            ],
            ..batch(2)
        });
    }
    // Reproduce the old hole: these semantic changes leave receipts identical.
    let mut altered = CellStore::open_in_memory(changed.clone()).expect("changed store");
    for input in &inputs {
        let expected = store.commit(input).expect("original commit");
        assert_eq!(
            altered.commit(input).expect("changed commit").receipt(),
            expected.receipt()
        );
    }
    let original_object = store
        .read()
        .object("main", "restore.Item", "a")
        .expect("read");
    assert_ne!(
        original_object,
        altered
            .read()
            .object("main", "restore.Item", "a")
            .expect("read")
    );
    let mut exporter = Exporter::local(&dir.path().join("export")).expect("exporter");
    export_all(&store, &mut exporter).await;
    let manifest = exporter.verify().await.expect("verified export");
    assert_eq!(manifest.model_hash, original.model_hash());
    assert!(matches!(
        CellStore::restore_from(
            &mut exporter,
            &target,
            changed,
            "org_demo/dev",
            &bindings(),
            StoreOptions::default()
        )
        .await,
        Err(RestoreError::ModelDefinitionsMismatch)
    ));
    assert!(!target.exists());
    assert_eq!(
        std::fs::read_dir(dir.path()).expect("no staging").count(),
        1
    );
    CellStore::restore_from(
        &mut exporter,
        &target,
        original.clone(),
        "org_demo/dev",
        &bindings(),
        StoreOptions::default(),
    )
    .await
    .expect("matching definitions restore");
    let rebuilt = CellStore::open(&target, original, StoreOptions::default()).expect("reopen");
    assert_eq!(
        rebuilt.read().dump().expect("dump"),
        store.read().dump().expect("dump")
    );
}

#[tokio::test]
async fn changed_default_with_identical_ddl_refuses_before_replay() {
    refuse_semantic_change(semantic_model("B", "approved"), false).await;
}

#[tokio::test]
async fn changed_lifecycle_transition_with_identical_ddl_refuses_before_replay() {
    refuse_semantic_change(semantic_model("A", "declined"), true).await;
}

#[tokio::test]
async fn exports_without_model_hash_and_unsupported_versions_are_refused() {
    let dir = tempfile::tempdir().expect("directory");
    let export = dir.path().join("export");
    let target = dir.path().join("restored.sqlite");
    let record = fixture(&export).await;
    let original_manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(export.join(MANIFEST_PATH)).expect("manifest"))
            .expect("decode");
    for (remove_hash, change_record) in [(true, false), (true, true), (false, false), (false, true)]
    {
        let mut manifest = original_manifest.clone();
        let mut value = serde_json::to_value(&record).expect("record");
        let changed = if change_record {
            &mut value
        } else {
            &mut manifest
        };
        if remove_hash {
            changed.as_object_mut().expect("object").remove("modelHash");
        } else {
            changed["format"] = json!("norn.cell.export/2");
        }
        let address = hash(&value);
        std::fs::write(
            export.join(object_path(&address).expect("path").as_ref()),
            norn_ir::canonical::canonical_json_of(&value).expect("canonical"),
        )
        .expect("object");
        manifest["headHash"] = json!(address);
        std::fs::write(
            export.join(MANIFEST_PATH),
            serde_json::to_vec(&manifest).expect("json"),
        )
        .expect("manifest");
        let error = restore(&export, &target)
            .await
            .expect_err("incompatible export");
        if remove_hash {
            assert!(
                matches!(error, RestoreError::Export(ExportError::Json(_))),
                "{error}"
            );
        } else {
            assert!(
                matches!(error, RestoreError::Export(ExportError::UnsupportedFormat)),
                "{error}"
            );
        }
        assert!(!target.exists());
        assert_eq!(
            std::fs::read_dir(dir.path()).expect("no staging").count(),
            1
        );
    }
}
fn batch(seq: u64) -> CommitBatch {
    CommitBatch {
        world: "main".to_owned(),
        scope: "org_demo/dev".to_owned(),
        logical_time: seq.saturating_mul(100),
        idempotency_key: Some(format!("commit-{seq}")),
        events: vec![rv(json!({"type":"test","seq":seq}))],
        ..CommitBatch::default()
    }
}
fn bindings() -> BTreeMap<String, String> {
    BTreeMap::from([("scope".to_owned(), "org_demo/dev".to_owned())])
}
fn command(generation: u64) -> OutboxCommand {
    OutboxCommand {
        command_id: "command".to_owned(),
        generation,
        idempotency_key: format!("external-{generation}"),
        deadline: Some(1000),
        command: rv(json!({"payload":"test"})),
    }
}
fn timer(id: &str) -> TimerEntry {
    TimerEntry {
        timer_id: id.to_owned(),
        due: 500,
        timer: rv(json!({"input":"timer"})),
    }
}
async fn export_all(store: &CellStore, exporter: &mut Exporter) {
    let manifest = exporter.verify().await.expect("verify");
    let start = manifest.head_commit_seq.saturating_add(1);
    for seq in start..=store.export_commit_count().expect("count") {
        exporter
            .append(&store.export_record(seq).expect("journal").expect("record"))
            .await
            .expect("append");
    }
}

fn initial_batch() -> CommitBatch {
    CommitBatch {
        writes: ["a", "b"]
            .into_iter()
            .map(|key| FactWriteRow::Put {
                object_type: "restore.Item".to_owned(),
                key: key.to_owned(),
                values: BTreeMap::from([("name".to_owned(), rv(json!(key)))]),
            })
            .collect(),
        links: vec![LinkChange::Add {
            object_type: "restore.Item".to_owned(),
            link: "next".to_owned(),
            source: "a".to_owned(),
            target: "b".to_owned(),
            values: BTreeMap::new(),
            target_type: None,
        }],
        effects: vec![
            EffectRow::Enqueue {
                command: command(0),
            },
            EffectRow::Schedule {
                timer: timer("consume"),
            },
            EffectRow::Schedule {
                timer: timer("pending"),
            },
        ],
        ..batch(1)
    }
}

#[tokio::test]
async fn wipe_restore_preserves_all_tables_receipts_links_and_export_continuation() {
    for journal_mode in [JournalMode::Delete, JournalMode::Wal] {
        let dir = tempfile::tempdir().expect("directory");
        let path = dir.path().join("cell.sqlite");
        let options = StoreOptions {
            journal_mode,
            cache_pages: Some(10),
        };
        let mut store = CellStore::open(&path, model(), options).expect("store");
        store.bind("scope", "org_demo/dev").expect("binding");
        let first = initial_batch();
        store.commit(&first).expect("first");
        // An older generation can have a recorded outcome that was not consumed.
        let exact =
            Value::from_json_text(r#"{"amount":9007199254740993.123456789}"#).expect("exact");
        store
            .record_performed("command", 0, &exact)
            .expect("old outcome");
        store
            .commit(&CommitBatch {
                effects: vec![EffectRow::Enqueue {
                    command: command(1),
                }],
                events: vec![],
                ..batch(2)
            })
            .expect("replace generation");
        store
            .record_performed("command", 1, &exact)
            .expect("outcome");
        store
            .commit(&CommitBatch {
                consumes: Some(Consumption::Result {
                    command_id: "command".to_owned(),
                    generation: 1,
                }),
                ..batch(3)
            })
            .expect("consume result");
        // Consume observations out of ingestion order; their positions must survive.
        store.inject("first", &exact).expect("inject first");
        store.inject("second", &exact).expect("inject second");
        for (seq, id) in [(4, "second"), (5, "first")] {
            store
                .commit(&CommitBatch {
                    consumes: Some(Consumption::Observation {
                        observation_id: id.to_owned(),
                    }),
                    ..batch(seq)
                })
                .expect("consume observation");
        }
        store
            .commit(&CommitBatch {
                consumes: Some(Consumption::Timer {
                    timer_id: "consume".to_owned(),
                    incarnation: 1,
                }),
                ..batch(6)
            })
            .expect("consume timer");
        let expected = store.read().dump().expect("all tables");
        let head = store.read().verify_ledger().expect("head");
        let mut exporter = Exporter::local(&dir.path().join("export")).expect("exporter");
        export_all(&store, &mut exporter).await;
        let manifest = exporter.verify().await.expect("verify");
        drop(store);
        std::fs::remove_file(&path).expect("wipe");
        let report = CellStore::restore_from(
            &mut exporter,
            &path,
            model(),
            "org_demo/dev",
            &bindings(),
            options,
        )
        .await
        .expect("restore");
        assert_eq!(report.manifest, manifest);
        assert_eq!(report.ledger_head, head);
        assert_eq!(report.fenced_commands, 0);
        let mut restored = CellStore::open(&path, model(), options).expect("reopen");
        assert_eq!(
            restored.read().dump().expect("all restored tables"),
            expected
        );
        assert_eq!(
            restored
                .commit(&first)
                .expect("retry")
                .receipt()
                .body
                .commit_seq,
            1
        );
        restored.commit(&batch(7)).expect("append after restore");
        export_all(&restored, &mut exporter).await;
        assert_eq!(
            exporter.verify().await.expect("continued").head_commit_seq,
            7
        );
    }
}

#[tokio::test]
async fn restore_replays_original_link_creation_across_property_replacement() {
    let directory = tempfile::tempdir().expect("directory");
    let export = directory.path().join("export");
    let target = directory.path().join("restored.sqlite");
    let mut store = CellStore::open_in_memory(model()).expect("store");
    store.bind("scope", "org_demo/dev").expect("binding");
    let mut first = initial_batch();
    // This fixture exercises link replay; pending commands intentionally gain
    // host-local perform fences during restore and belong to separate tests.
    first.effects.clear();
    store.commit(&first).expect("first add");
    store
        .commit(&CommitBatch {
            links: first.links.clone(),
            ..batch(2)
        })
        .expect("replacement");
    let expected = store.read().dump().expect("tables");
    let mut exporter = Exporter::local(&export).expect("exporter");
    export_all(&store, &mut exporter).await;
    restore(&export, &target).await.expect("restore");
    let restored = CellStore::open(&target, model(), StoreOptions::default()).expect("reopen");
    assert_eq!(restored.read().dump().expect("replayed tables"), expected);
    let page = restored
        .read()
        .links_touching_page(
            "main",
            "restore.Item",
            "a",
            norn_cell::read::LinkDirection::Out,
            None,
            None,
            10,
        )
        .expect("links");
    assert_eq!(page.items[0].version, 2);
    assert_eq!(
        restored
            .read()
            .link_creation_commits("main", &page.items)
            .expect("creation"),
        vec![1]
    );
}

async fn fixture(dir: &std::path::Path) -> ExportRecord {
    let mut store = CellStore::open_in_memory(model()).expect("store");
    store.bind("scope", "org_demo/dev").expect("bind");
    store
        .commit(&CommitBatch {
            writes: vec![FactWriteRow::Put {
                object_type: "restore.Item".to_owned(),
                key: "a".to_owned(),
                values: BTreeMap::from([("name".to_owned(), rv(json!("a")))]),
            }],
            ..batch(1)
        })
        .expect("commit");
    let record = store.export_record(1).expect("journal").expect("record");
    Exporter::local(dir)
        .expect("exporter")
        .append(&record)
        .await
        .expect("append");
    record
}
async fn restore(
    dir: &std::path::Path,
    target: &std::path::Path,
) -> Result<norn_cell::RestoreReport, RestoreError> {
    CellStore::restore_from(
        &mut Exporter::local(dir).expect("exporter"),
        target,
        model(),
        "org_demo/dev",
        &bindings(),
        StoreOptions::default(),
    )
    .await
}
fn write_record(dir: &std::path::Path, record: &ExportRecord, old_hash: &str) {
    std::fs::write(
        dir.join(object_path(old_hash).expect("path").as_ref()),
        norn_ir::canonical::canonical_json_of(record).expect("canonical"),
    )
    .expect("write");
}

#[tokio::test]
async fn tampered_object_missing_object_broken_previous_receipt_and_sequence_are_typed_refusals() {
    let dir = tempfile::tempdir().expect("directory");
    let export = dir.path().join("export");
    let target = dir.path().join("restored.sqlite");
    let original = fixture(&export).await;
    let address = original.hash().expect("hash");
    let mut tampered = original.clone();
    tampered.batch.logical_time += 1;
    write_record(&export, &tampered, &address);
    assert!(matches!(
        restore(&export, &target).await,
        Err(RestoreError::Export(ExportError::ContentAddressMismatch))
    ));
    assert!(!target.exists());
    std::fs::remove_file(export.join(object_path(&address).expect("path").as_ref()))
        .expect("remove");
    assert!(matches!(
        restore(&export, &target).await,
        Err(RestoreError::Export(ExportError::Storage(_)))
    ));
    assert!(!target.exists());
    // Each altered record has a valid content address: verify must inspect the chain.
    for (variant, change) in [
        (0, "previous"),
        (1, "receipt"),
        (2, "sequence"),
        (3, "event link"),
    ] {
        let mut changed = original.clone();
        match variant {
            0 => changed.previous = "1".repeat(64),
            1 => changed.receipt.hash = "1".repeat(64),
            2 => changed.receipt.body.commit_seq = 2,
            _ => changed.entries[0].prev_hash = "1".repeat(64),
        }
        let changed_hash = changed.hash().expect("hash");
        write_record(&export, &changed, &changed_hash);
        let mut manifest: norn_cell::export::ExportManifest =
            serde_json::from_slice(&std::fs::read(export.join(MANIFEST_PATH)).expect("manifest"))
                .expect("decode");
        manifest.head_hash = changed_hash;
        std::fs::write(
            export.join(MANIFEST_PATH),
            serde_json::to_vec(&manifest).expect("json"),
        )
        .expect("manifest");
        let error = restore(&export, &target).await.expect_err(change);
        assert!(
            match variant {
                0 => matches!(error, RestoreError::Export(ExportError::GenesisMismatch)),
                1 => matches!(error, RestoreError::Export(ExportError::ReceiptMismatch)),
                2 => matches!(error, RestoreError::Export(ExportError::SequenceMismatch)),
                _ => matches!(error, RestoreError::Export(ExportError::LedgerMismatch)),
            },
            "{change}: {error}"
        );
        assert!(!target.exists());
    }
}

#[tokio::test]
async fn wrong_model_scope_bindings_existing_target_and_self_consistent_false_receipt_refuse() {
    let dir = tempfile::tempdir().expect("directory");
    let export = dir.path().join("export");
    let target = dir.path().join("restored.sqlite");
    let original = fixture(&export).await;
    let mut exporter = Exporter::local(&export).expect("exporter");
    let other_model = Model::from_definitions(&[], &[]).expect("empty model");
    assert!(matches!(
        CellStore::restore_from(
            &mut exporter,
            &target,
            other_model,
            "org_demo/dev",
            &bindings(),
            StoreOptions::default()
        )
        .await,
        Err(RestoreError::ModelMismatch)
    ));
    assert!(matches!(
        CellStore::restore_from(
            &mut exporter,
            &target,
            model(),
            "other/dev",
            &bindings(),
            StoreOptions::default()
        )
        .await,
        Err(RestoreError::IdentityMismatch)
    ));
    assert!(matches!(
        CellStore::restore_from(
            &mut exporter,
            &target,
            model(),
            "org_demo/dev",
            &BTreeMap::new(),
            StoreOptions::default()
        )
        .await,
        Err(RestoreError::IdentityMismatch)
    ));
    assert!(!target.exists());
    std::fs::write(&target, "keep").expect("existing");
    assert!(matches!(
        restore(&export, &target).await,
        Err(RestoreError::TargetExists(_))
    ));
    assert_eq!(std::fs::read_to_string(&target).expect("preserved"), "keep");
    std::fs::remove_file(&target).expect("remove");
    let mut false_receipt = original.clone();
    false_receipt.receipt.body.written[0].version += 1;
    false_receipt.receipt.hash = hash(&false_receipt.receipt.body);
    let false_export = dir.path().join("false-receipt");
    Exporter::local(&false_export)
        .expect("exporter")
        .append(&false_receipt)
        .await
        .expect("self-consistent envelope");
    assert!(matches!(
        restore(&false_export, &target).await,
        Err(RestoreError::ReceiptMismatch(1))
    ));
    assert!(!target.exists());
    assert_eq!(
        std::fs::read_dir(dir.path())
            .expect("cleaned staging")
            .count(),
        2
    );
    let mut wrong_ddl = original;
    wrong_ddl.ddl_sql.push_str("\nDROP TABLE dangerous;");
    wrong_ddl.ddl_hash = hash(&wrong_ddl.ddl_sql);
    let ddl_export = dir.path().join("wrong-ddl");
    let mut exporter = Exporter::local(&ddl_export).expect("exporter");
    assert!(matches!(
        exporter.append(&wrong_ddl).await,
        Err(ExportError::IdentityMismatch)
    ));
    assert!(!ddl_export.join(MANIFEST_PATH).exists());
    assert!(!ddl_export.join("commits").exists());
    assert!(!target.exists());
}

#[tokio::test]
async fn unknown_external_outcomes_are_fenced_across_reopen_and_generation_replacement() {
    use norn_kernel::CellHost as _;
    let dir = tempfile::tempdir().expect("directory");
    let export = dir.path().join("export");
    let target = dir.path().join("restored.sqlite");
    let mut original = CellStore::open_in_memory(model()).expect("store");
    original.bind("scope", "org_demo/dev").expect("bind");
    original
        .commit(&CommitBatch {
            effects: vec![EffectRow::Enqueue {
                command: command(0),
            }],
            ..batch(1)
        })
        .expect("enqueue");
    original
        .record_performed(
            "command",
            0,
            &rv(json!({"outsideEffectAlreadyHappened":true})),
        )
        .expect("unexported outcome");
    original
        .inject("unconsumed", &rv(json!({"notInPrefix":true})))
        .expect("unexported inbox");
    let mut exporter = Exporter::local(&export).expect("exporter");
    export_all(&original, &mut exporter).await;
    let report = restore(&export, &target).await.expect("restore");
    assert_eq!(report.fenced_commands, 1);
    for replace in [false, true] {
        let mut restored =
            CellStore::open(&target, model(), StoreOptions::default()).expect("open");
        assert!(
            restored
                .read()
                .pending_observations()
                .expect("inbox")
                .is_empty()
        );
        assert!(restored.read().runs("command").expect("runs").is_empty());
        if replace {
            restored
                .commit(&CommitBatch {
                    effects: vec![EffectRow::Enqueue {
                        command: command(1),
                    }],
                    ..batch(2)
                })
                .expect("replace");
        }
        let mut cell = norn_cell::SqliteCell::<Value, Value, Value>::new(
            restored,
            "org_demo/dev",
            |_command| panic!("fenced command must not perform"),
        )
        .expect("host");
        assert!(cell.perform().expect("fenced").is_none());
    }
}

#[tokio::test]
async fn exported_perform_claim_is_replayed_but_restore_fence_stays_first() {
    use norn_kernel::CellHost as _;

    let dir = tempfile::tempdir().expect("directory");
    let export = dir.path().join("export");
    let target = dir.path().join("restored.sqlite");
    let mut original = CellStore::open_in_memory(model()).expect("store");
    original.bind("scope", "org_demo/dev").expect("bind");
    original
        .commit(&CommitBatch {
            effects: vec![EffectRow::Enqueue {
                command: command(0),
            }],
            ..batch(1)
        })
        .expect("enqueue");
    original
        .commit(&CommitBatch {
            world: "main".to_owned(),
            scope: "org_demo/dev".to_owned(),
            logical_time: 100,
            effects: vec![EffectRow::ClaimPerform {
                command_id: "command".to_owned(),
                generation: 0,
            }],
            ..CommitBatch::default()
        })
        .expect("claim");
    assert!(
        original
            .read()
            .perform_claimed("command", 0)
            .expect("claim reads")
    );
    let claim_record = original
        .export_record(2)
        .expect("journal reads")
        .expect("claim is journaled");
    assert_eq!(claim_record.receipt.body.event_count, 0);
    assert!(matches!(
        claim_record.batch.effects.as_slice(),
        [EffectRow::ClaimPerform {
            command_id,
            generation: 0,
        }] if command_id == "command"
    ));

    let mut exporter = Exporter::local(&export).expect("exporter");
    export_all(&original, &mut exporter).await;
    let report = restore(&export, &target).await.expect("restore");
    assert_eq!(report.fenced_commands, 1);

    // Reopening retains both the replayed claim and the host-local restore
    // fence. The fence is checked before decoding or exposing claim state.
    for _ in 0..2 {
        let restored = CellStore::open(&target, model(), StoreOptions::default()).expect("reopen");
        assert!(
            restored
                .read()
                .perform_claimed("command", 0)
                .expect("replayed claim reads")
        );
        let mut cell = norn_cell::SqliteCell::<Value, Value, Value>::new(
            restored,
            "org_demo/dev",
            |_command| panic!("restored claim must remain fenced"),
        )
        .expect("host");
        assert!(cell.claim_perform().expect("fenced claim").is_none());
        assert!(cell.perform().expect("fenced perform").is_none());
    }
}

#[tokio::test]
async fn missing_intermediate_commit_reordered_head_and_changed_manifest_are_refused() {
    let dir = tempfile::tempdir().expect("directory");
    let export = dir.path().join("export");
    let target = dir.path().join("restored.sqlite");
    let mut store = CellStore::open_in_memory(model()).expect("store");
    store.bind("scope", "org_demo/dev").expect("bind");
    for seq in 1..=3 {
        store.commit(&batch(seq)).expect("commit");
    }
    let mut exporter = Exporter::local(&export).expect("exporter");
    export_all(&store, &mut exporter).await;
    let manifest = exporter.verify().await.expect("manifest");
    let middle = store.export_record(2).expect("journal").expect("middle");
    let middle_path = export.join(
        object_path(&middle.hash().expect("hash"))
            .expect("path")
            .as_ref(),
    );
    std::fs::remove_file(&middle_path).expect("missing intermediate commit");
    assert!(matches!(
        restore(&export, &target).await,
        Err(RestoreError::Export(ExportError::Storage(_)))
    ));
    assert!(!target.exists());
    write_record(&export, &middle, &middle.hash().expect("hash"));
    let mut reordered = manifest.clone();
    reordered.head_hash = store
        .export_record(1)
        .expect("journal")
        .expect("first")
        .hash()
        .expect("hash");
    std::fs::write(
        export.join(MANIFEST_PATH),
        serde_json::to_vec(&reordered).expect("json"),
    )
    .expect("reorder");
    assert!(matches!(
        restore(&export, &target).await,
        Err(RestoreError::Export(ExportError::SequenceMismatch))
    ));
    assert!(!target.exists());
    let mut changed_head = manifest;
    changed_head.head_ledger_seq = 99;
    std::fs::write(
        export.join(MANIFEST_PATH),
        serde_json::to_vec(&changed_head).expect("json"),
    )
    .expect("manifest");
    assert!(matches!(
        restore(&export, &target).await,
        Err(RestoreError::Export(ExportError::ManifestMismatch))
    ));
    assert!(!target.exists());
}

#[tokio::test]
async fn self_consistent_export_with_wrong_timer_preimage_fails_replay_and_cleans_staging() {
    let dir = tempfile::tempdir().expect("directory");
    let export = dir.path().join("export");
    let target = dir.path().join("restored.sqlite");
    let mut store = CellStore::open_in_memory(model()).expect("store");
    store.bind("scope", "org_demo/dev").expect("bind");
    store
        .commit(&CommitBatch {
            effects: vec![EffectRow::Schedule {
                timer: timer("consume"),
            }],
            ..batch(1)
        })
        .expect("schedule");
    store
        .commit(&CommitBatch {
            consumes: Some(Consumption::Timer {
                timer_id: "consume".to_owned(),
                incarnation: 1,
            }),
            ..batch(2)
        })
        .expect("consume");
    let first = store.export_record(1).expect("journal").expect("first");
    let mut second = store.export_record(2).expect("journal").expect("second");
    second.consumed_input = Some(rv(json!({
        "kind": "timer", "commitSeq": 1,
        "timer": {"timerId": "consume", "due": 999, "timer": {"input": "timer"}}
    })));
    let mut exporter = Exporter::local(&export).expect("exporter");
    exporter.append(&first).await.expect("first");
    exporter
        .append(&second)
        .await
        .expect("self-consistent export envelope");
    assert!(matches!(
        restore(&export, &target).await,
        Err(RestoreError::InputMismatch(2))
    ));
    assert!(!target.exists());
    assert_eq!(
        std::fs::read_dir(dir.path())
            .expect("staging removed")
            .count(),
        1
    );
}
