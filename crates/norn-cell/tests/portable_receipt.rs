#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    missing_docs,
    reason = "boundary acceptance scenarios"
)]

use norn_cell::{CellStore, CommitBatch, EffectRow, Model, OutboxCommand, engine::NativeEngine};
use norn_ir::{
    RuntimeValue,
    cell_id::CellId,
    cell_reference::{CellReference, CommitProvenance, QualifiedCommit, ReferenceNamespace},
    function::FunctionInvocation,
};

fn id(value: u128) -> CellId {
    CellId::try_from(format!("{value:032x}")).expect("id")
}
fn store(value: u128) -> CellStore {
    CellStore::from_engine_bound(
        NativeEngine::open_in_memory().expect("engine"),
        Model::from_definitions(&[], &[]).expect("model"),
        &id(value),
    )
    .expect("store")
}
fn command(value: RuntimeValue) -> EffectRow {
    EffectRow::Enqueue {
        command: OutboxCommand {
            command_id: "same-command".into(),
            generation: 0,
            idempotency_key: "same-operation".into(),
            deadline: None,
            command: value,
        },
    }
}

#[test]
fn same_local_commit_and_operation_have_distinct_portable_identities() {
    let mut a = store(1);
    let mut b = store(2);
    a.bind("scope", "org_demo/dev").expect("scope");
    b.bind("scope", "org_demo/dev").expect("scope");
    let batch = CommitBatch {
        scope: "org_demo/dev".into(),
        effects: vec![command(RuntimeValue::Null)],
        ..CommitBatch::default()
    };
    let first = a.commit(&batch).expect("commit");
    let second = b.commit(&batch).expect("commit");
    assert_eq!(
        first.receipt(),
        second.receipt(),
        "local receipt format stays unchanged"
    );
    let first_portable = a.portable_receipt(1).expect("portable");
    let second_portable = b.portable_receipt(1).expect("portable");
    assert_ne!(
        first_portable.content_digest().expect("digest"),
        second_portable.content_digest().expect("digest")
    );
    let first_command = a.export_command(1, "same-command").expect("export");
    let second_command = b.export_command(1, "same-command").expect("export");
    assert_eq!(
        first_command.command, second_command.command,
        "local identities stay unchanged"
    );
    assert_ne!(
        first_command
            .deduplication
            .content_digest()
            .expect("digest"),
        second_command
            .deduplication
            .content_digest()
            .expect("digest")
    );
    let mut forged = a.export_record(1).expect("record").expect("present");
    forged
        .portable_receipt
        .as_mut()
        .expect("portable")
        .origin
        .cell = id(2);
    assert!(
        forged
            .advance(&norn_cell::export::ExportManifest::default())
            .is_err()
    );
}

#[test]
fn foreign_inbox_deduplication_uses_cell_and_world() {
    use norn_kernel::{
        CellCommit, CellHost as _, Consumed, LogicalTime, ObservationId, Observed, Provenance,
    };
    let mut target: norn_cell::SqliteCell<(), (), RuntimeValue> =
        norn_cell::SqliteCell::new(store(3), "", |_| RuntimeValue::Null).expect("host");
    let observed = Observed {
        id: ObservationId("same-id".into()),
        provenance: Provenance {
            source: "synthetic".into(),
            source_id: "same-id".into(),
            observed_at: LogicalTime::from_millis(0),
            mapping_version: "v1".into(),
        },
        input: RuntimeValue::Null,
    };
    let first = CellReference::new(
        id(1),
        "main".into(),
        ReferenceNamespace::Inbox,
        "same-id".into(),
    );
    let mut second = first.clone();
    second.cell = id(2);
    let mut fork = first.clone();
    fork.world = "fork".into();
    assert!(target.inject_foreign(&first, &observed).expect("first"));
    assert!(!target.inject_foreign(&first, &observed).expect("retry"));
    assert!(
        target
            .inject_foreign(&second, &observed)
            .expect("other cell")
    );
    assert!(
        target
            .inject_foreign(&fork, &observed)
            .expect("other world")
    );
    for origin in [&first, &second, &fork] {
        let delivery = target.observe().expect("observe").expect("pending");
        assert_eq!(
            delivery.id.0,
            origin.content_digest().expect("qualified id").0
        );
        target
            .commit(CellCommit {
                consumes: Some(Consumed::Observation { id: delivery.id }),
                ..CellCommit::empty()
            })
            .expect("consume qualified delivery");
    }
    assert!(target.observe().expect("empty inbox").is_none());
    assert!(
        !target
            .inject_foreign(&first, &observed)
            .expect("consumed retry")
    );
}

#[tokio::test]
async fn legacy_replay_never_mints_portable_receipts_or_rehashes_original_evidence() {
    let mut source = store(1);
    source.commit(&CommitBatch::default()).expect("commit");
    let mut old = source.export_record(1).expect("record").expect("present");
    old.portable_receipt = None; // Exactly the pre-envelope serialized shape.
    let old_bytes = norn_ir::canonical::canonical_json_of(&old).expect("old bytes");
    let old_hash = old.hash().expect("old hash");
    let mut rebuilt = CellStore::rebuild_export_history(
        NativeEngine::open_in_memory().expect("engine"),
        &[old.clone()],
        &old_hash,
    )
    .expect("replay");
    assert!(rebuilt.portable_receipt(1).is_err());
    let retained = rebuilt.export_record(1).expect("record").expect("present");
    assert_eq!(
        norn_ir::canonical::canonical_json_of(&retained).expect("retained bytes"),
        old_bytes
    );
    assert_eq!(retained.hash().expect("retained hash"), old_hash);
    assert_eq!(
        rebuilt.read().receipt_at(1).expect("local receipt"),
        Some(old.receipt)
    );
    rebuilt.commit(&CommitBatch::default()).expect("new commit");
    assert!(rebuilt.portable_receipt(2).is_ok());
    assert!(rebuilt.portable_receipt(1).is_err());
    let first = rebuilt.export_record(1).expect("first").expect("present");
    let second = rebuilt.export_record(2).expect("second").expect("present");
    let dir = tempfile::tempdir().expect("directory");
    let mut exporter =
        norn_cell::export::Exporter::local(&dir.path().join("export")).expect("exporter");
    exporter.append(&first).await.expect("old record");
    exporter.append(&second).await.expect("new record");
    let target = dir.path().join("restored.sqlite");
    CellStore::restore_from(
        &mut exporter,
        &target,
        Model::from_definitions(&[], &[]).expect("model"),
        "",
        &first.bindings,
        norn_cell::StoreOptions::default(),
    )
    .await
    .expect("mixed history restore");
    let restored = CellStore::open_bound(
        &target,
        Model::from_definitions(&[], &[]).expect("model"),
        norn_cell::StoreOptions::default(),
        &id(1),
    )
    .expect("reopen");
    assert_eq!(restored.export_record(1).expect("first"), Some(first));
    assert_eq!(restored.export_record(2).expect("second"), Some(second));
    assert!(restored.portable_receipt(1).is_err());
    assert_eq!(
        restored.portable_receipt(2).expect("new receipt"),
        rebuilt.portable_receipt(2).expect("original new receipt")
    );
}

#[test]
fn wrong_function_origin_refuses_before_any_effect() {
    let mut target = store(1);
    let mut invocation: FunctionInvocation = serde_json::from_str(include_str!(
        "../../../fixtures/function/invocation.wasm.json"
    ))
    .expect("invocation");
    for (cell, world, commit) in [(id(2), "main", 1), (id(1), "fork", 1), (id(1), "main", 2)] {
        invocation.provenance_commit =
            CommitProvenance::Qualified(QualifiedCommit::new(cell, world.into(), commit));
        let before = target.read().dump().expect("before");
        let wrapped = norn_kernel::Command {
            id: norn_kernel::CommandId("same-command".into()),
            generation: norn_kernel::Generation(0),
            idempotency_key: norn_kernel::IdempotencyKey("same-operation".into()),
            freshness: norn_kernel::FreshnessPolicy::AppendOnly,
            deadline: None,
            payload: invocation.clone(),
        };
        for payload in [
            norn_ir::runtime::encode(&invocation).expect("direct invocation"),
            norn_ir::runtime::encode(&wrapped).expect("wrapped invocation"),
        ] {
            let batch = CommitBatch {
                world: "main".into(),
                effects: vec![command(payload)],
                ..CommitBatch::default()
            };
            assert!(target.commit(&batch).is_err());
            assert_eq!(
                target.read().dump().expect("after"),
                before,
                "refusal leaves no effects"
            );
        }
    }
    invocation.provenance_commit =
        CommitProvenance::Qualified(QualifiedCommit::new(id(1), "main".into(), 1));
    target
        .commit(&CommitBatch {
            world: "main".into(),
            effects: vec![command(
                norn_ir::runtime::encode(&invocation).expect("payload"),
            )],
            ..CommitBatch::default()
        })
        .expect("local qualified dispatch");
    target
        .export_command(1, "same-command")
        .expect("portable dispatch");
}

#[test]
fn legacy_function_invocation_remains_local_only_even_in_a_new_commit() {
    let mut target = store(1);
    let invocation: FunctionInvocation = serde_json::from_str(include_str!(
        "../../../fixtures/function/invocation.wasm.json"
    ))
    .expect("legacy");
    let nested = RuntimeValue::Object(std::collections::BTreeMap::from([(
        "invocation".into(),
        norn_ir::runtime::encode(&invocation).expect("payload"),
    )]));
    target
        .commit(&CommitBatch {
            effects: vec![command(nested)],
            ..CommitBatch::default()
        })
        .expect("local dispatch");
    assert!(target.export_command(1, "same-command").is_err());
}
