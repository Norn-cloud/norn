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

fn function_envelope(invocation: RuntimeValue) -> RuntimeValue {
    norn_ir::runtime::encode(&norn_kernel::Command {
        id: norn_kernel::CommandId("same-command".into()),
        generation: norn_kernel::Generation(0),
        idempotency_key: norn_kernel::IdempotencyKey("same-operation".into()),
        freshness: norn_kernel::FreshnessPolicy::AppendOnly,
        deadline: None,
        payload: RuntimeValue::Object(std::collections::BTreeMap::from([
            (
                "type".into(),
                RuntimeValue::String("norn.function/v1".into()),
            ),
            ("invocation".into(), invocation),
        ])),
    })
    .expect("production Function envelope")
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
            function_envelope(norn_ir::runtime::encode(&invocation).expect("Function invocation")),
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

fn local_hash<T: serde::Serialize>(value: &T) -> String {
    norn_ir::canonical::HashProfile::Norn1.hash_canonical_bytes(
        &norn_ir::canonical::canonical_json_of(value).expect("canonical fixture"),
    )
}

#[test]
fn legacy_invocations_replay_locally_but_cannot_be_new_dispatches() {
    let mut target = store(1);
    let invocation: FunctionInvocation = serde_json::from_str(include_str!(
        "../../../fixtures/function/invocation.wasm.json"
    ))
    .expect("legacy");
    let nested =
        function_envelope(norn_ir::runtime::encode(&invocation).expect("legacy invocation"));
    let legacy_batch = CommitBatch {
        effects: vec![command(nested.clone())],
        ..CommitBatch::default()
    };
    let before = target.read().dump().expect("before");
    assert!(target.commit(&legacy_batch).is_err());
    assert_eq!(target.read().dump().expect("after"), before);
    // Construct a pre-envelope writer fixture; replay must retain these bytes.
    target
        .commit(&CommitBatch {
            effects: vec![command(RuntimeValue::Null)],
            ..CommitBatch::default()
        })
        .expect("seed");
    let mut old = target.export_record(1).expect("record").expect("present");
    old.batch = legacy_batch;
    old.receipt.body.request_hash = local_hash(&old.batch);
    old.receipt.hash = local_hash(&old.receipt.body);
    old.portable_receipt = None;
    let old_hash = old.hash().expect("old hash");
    let rebuilt = CellStore::rebuild_export_history(
        NativeEngine::open_in_memory().expect("engine"),
        &[old.clone()],
        &old_hash,
    )
    .expect("legacy replay");
    assert_eq!(rebuilt.export_record(1).expect("retained"), Some(old));
    assert_eq!(
        rebuilt
            .read()
            .head_command()
            .expect("head")
            .expect("pending")
            .command
            .command,
        nested
    );
    assert!(rebuilt.portable_receipt(1).is_err());
    assert!(rebuilt.export_command(1, "same-command").is_err());
}

#[test]
fn malformed_declared_function_envelope_refuses_without_effects() {
    let mut target = store(1);
    let before = target.read().dump().expect("before");
    assert!(
        target
            .commit(&CommitBatch {
                effects: vec![command(function_envelope(RuntimeValue::Null))],
                ..CommitBatch::default()
            })
            .is_err()
    );
    assert_eq!(target.read().dump().expect("after"), before);
}

#[test]
fn portable_export_validation_checks_function_cell_world_and_commit() {
    let mut source = store(1);
    source
        .commit(&CommitBatch {
            world: "main".into(),
            effects: vec![command(RuntimeValue::Null)],
            ..CommitBatch::default()
        })
        .expect("seed");
    let record = source.export_record(1).expect("record").expect("present");
    let mut invocation: FunctionInvocation = serde_json::from_str(include_str!(
        "../../../fixtures/function/invocation.wasm.json"
    ))
    .expect("invocation");
    let legacy = invocation.provenance_commit.clone();
    for (origin, accepted) in [
        (
            CommitProvenance::Qualified(QualifiedCommit::new(id(1), "main".into(), 1)),
            true,
        ),
        (
            CommitProvenance::Qualified(QualifiedCommit::new(id(2), "main".into(), 1)),
            false,
        ),
        (
            CommitProvenance::Qualified(QualifiedCommit::new(id(1), "fork".into(), 1)),
            false,
        ),
        (
            CommitProvenance::Qualified(QualifiedCommit::new(id(1), "main".into(), 2)),
            false,
        ),
        (legacy, false),
    ] {
        invocation.provenance_commit = origin;
        let mut fixture = record.clone();
        fixture.batch.effects = vec![command(function_envelope(
            norn_ir::runtime::encode(&invocation).expect("invocation"),
        ))];
        fixture.receipt.body.request_hash = local_hash(&fixture.batch);
        fixture.receipt.hash = local_hash(&fixture.receipt.body);
        fixture
            .portable_receipt
            .as_mut()
            .expect("portable")
            .receipt_hash = fixture.receipt.hash.clone();
        let result = fixture.advance(&norn_cell::export::ExportManifest::default());
        if accepted {
            result.expect("matching Function dispatch");
        } else {
            assert!(matches!(
                result,
                Err(norn_cell::export::ExportError::IdentityMismatch)
            ));
        }
    }
}

#[test]
fn exported_command_matches_the_retained_generation_and_commit() {
    let mut source = store(1);
    let enqueue = |generation, value| {
        let EffectRow::Enqueue { mut command } = command(RuntimeValue::String(value)) else {
            unreachable!()
        };
        command.generation = generation;
        EffectRow::Enqueue { command }
    };
    source
        .commit(&CommitBatch {
            effects: vec![enqueue(2, "first".into())],
            ..CommitBatch::default()
        })
        .expect("first");
    source
        .commit(&CommitBatch {
            effects: vec![enqueue(1, "ignored".into())],
            ..CommitBatch::default()
        })
        .expect("ignored older generation");
    assert!(source.export_command(2, "same-command").is_err());
    assert_eq!(
        source
            .export_command(1, "same-command")
            .expect("retained")
            .command
            .generation,
        2
    );
    source
        .commit(&CommitBatch {
            effects: vec![
                enqueue(3, "intermediate".into()),
                enqueue(4, "final".into()),
            ],
            ..CommitBatch::default()
        })
        .expect("two replacements");
    assert!(source.export_command(1, "same-command").is_err());
    let retained = source.export_command(3, "same-command").expect("final");
    assert_eq!(retained.command.generation, 4);
    assert_eq!(
        retained.command.command,
        RuntimeValue::String("final".into())
    );
}

#[test]
fn qualified_function_without_portable_envelope_refuses_export_and_replay() {
    let mut source = store(1);
    let mut invocation: FunctionInvocation = serde_json::from_str(include_str!(
        "../../../fixtures/function/invocation.wasm.json"
    ))
    .expect("invocation");
    invocation.provenance_commit =
        CommitProvenance::Qualified(QualifiedCommit::new(id(1), "main".into(), 1));
    let batch = CommitBatch {
        effects: vec![command(function_envelope(
            norn_ir::runtime::encode(&invocation).expect("invocation"),
        ))],
        ..CommitBatch::default()
    };
    source.commit(&batch).expect("qualified control");
    let mut record = source.export_record(1).expect("record").expect("present");
    record
        .advance(&norn_cell::export::ExportManifest::default())
        .expect("qualified export");
    let mut foreign = invocation.clone();
    foreign.provenance_commit =
        CommitProvenance::Qualified(QualifiedCommit::new(id(2), "fork".into(), 99));
    for invocation in [invocation, foreign] {
        record.batch.effects = vec![command(function_envelope(
            norn_ir::runtime::encode(&invocation).expect("invocation"),
        ))];
        record.receipt.body.request_hash = local_hash(&record.batch);
        record.receipt.hash = local_hash(&record.receipt.body);
        record.portable_receipt = None;
        assert!(matches!(
            record.advance(&norn_cell::export::ExportManifest::default()),
            Err(norn_cell::export::ExportError::IdentityMismatch)
        ));
        let hash = record.hash().expect("self-consistent fixture");
        assert!(
            CellStore::rebuild_export_history(
                NativeEngine::open_in_memory().expect("engine"),
                &[record.clone()],
                &hash
            )
            .is_err()
        );
    }
}
