#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs,
    reason = "integration-test assertions"
)]

//! Per-commit export, independent integrity verification, and interrupted-upload recovery.

mod common;

use norn_cell::export::{Exporter, MANIFEST_PATH, object_path};
use norn_cell::{CellStore, CommitBatch, StoreOptions};
use serde_json::json;

#[tokio::test]
async fn cold_resume_requires_the_exact_local_journal_and_remote_head() {
    use norn_cell::export::ExportError;

    let directory = tempfile::tempdir().expect("export directory");
    let mut store = CellStore::open_in_memory(common::corpus_model()).expect("cell");
    store.bind("scope", "org_demo/dev").expect("scope");
    let input = batch(Some("resume"), vec![json!({"type":"created"})]);
    store.commit(&input).expect("commit");
    store
        .commit(&batch(None, vec![]))
        .expect("zero-event commit");
    let mut exporter = Exporter::local(directory.path()).expect("exporter");
    for seq in 1..=2 {
        exporter
            .append(&store.export_record(seq).expect("journal").expect("record"))
            .await
            .expect("publish");
    }
    let mut cold = Exporter::local(directory.path()).expect("cold exporter");
    let head = cold
        .resume_from_journal(|seq| store.export_record(seq))
        .await
        .expect("trusted journal resume");
    assert_eq!(head.head_commit_seq, 2);
    assert_eq!(cold.verify().await.expect("independent verification"), head);
    assert!(matches!(
        cold.resume_from_journal(|_| Ok(None)).await,
        Err(ExportError::SequenceMismatch)
    ));

    let mut foreign = CellStore::open_in_memory(common::corpus_model()).expect("foreign cell");
    foreign
        .bind("scope", "org_other/dev")
        .expect("foreign scope");
    let mut foreign_input = input.clone();
    foreign_input.scope = "org_other/dev".to_owned();
    foreign.commit(&foreign_input).expect("foreign commit");
    let mut foreign_empty = batch(None, vec![]);
    foreign_empty.scope = "org_other/dev".to_owned();
    foreign
        .commit(&foreign_empty)
        .expect("foreign empty commit");
    assert!(matches!(
        cold.resume_from_journal(|seq| foreign.export_record(seq))
            .await,
        Err(ExportError::ManifestMismatch)
    ));

    let path = directory
        .path()
        .join(object_path(&head.head_hash).expect("head key").as_ref());
    let original = std::fs::read(&path).expect("head bytes");
    let mut noncanonical = original.clone();
    noncanonical.push(b' ');
    std::fs::write(&path, noncanonical).expect("corrupt head object");
    assert!(matches!(
        cold.resume_from_journal(|seq| store.export_record(seq))
            .await,
        Err(ExportError::ContentAddressMismatch)
    ));
    std::fs::write(&path, original).expect("restore head object");
    assert_eq!(
        cold.resume_from_journal(|seq| store.export_record(seq))
            .await
            .expect("resume after repair"),
        head
    );
}

fn batch(key: Option<&str>, events: Vec<serde_json::Value>) -> CommitBatch {
    CommitBatch {
        world: "main".to_owned(),
        scope: "org_demo/dev".to_owned(),
        logical_time: 100,
        idempotency_key: key.map(str::to_owned),
        events: events
            .into_iter()
            .map(|event| norn_ir::RuntimeValue::from_json(event).expect("runtime value"))
            .collect(),
        ..CommitBatch::default()
    }
}

#[test]
fn publication_phases_share_the_native_envelope_validation() {
    use norn_cell::export::{ExportError, ExportManifest};

    let mut store = CellStore::open_in_memory(common::corpus_model()).expect("cell");
    store.bind("scope", "org_demo/dev").expect("scope");
    store
        .commit(&batch(None, vec![json!({"type":"created"})]))
        .expect("first");
    store.commit(&batch(None, vec![])).expect("empty batch");
    let first = store
        .export_record(1)
        .expect("journal")
        .expect("first record");
    let second = store
        .export_record(2)
        .expect("journal")
        .expect("second record");
    let genesis = ExportManifest::from_bytes(None).expect("absent manifest");
    let head = first.advance(&genesis).expect("first manifest");
    let next = second
        .advance(&head)
        .expect("empty batch advances commit head");
    assert_eq!(next.head_commit_seq, 2);
    assert_eq!(next.head_ledger_seq, head.head_ledger_seq);
    assert_eq!(
        ExportManifest::from_bytes(Some(&next.bytes().expect("manifest bytes")))
            .expect("read checkpoint"),
        next
    );
    second
        .validate_manifest(&next)
        .expect("recovered manifest matches journal");
    assert!(matches!(
        first.validate_manifest(&next),
        Err(ExportError::ManifestMismatch)
    ));
    assert!(matches!(
        second.advance(&genesis),
        Err(ExportError::SequenceMismatch)
    ));
    assert!(matches!(
        first.advance(&next),
        Err(ExportError::SequenceMismatch)
    ));
    let mut bytes = second.bytes().expect("immutable bytes");
    second.verify_bytes(&bytes).expect("exact immutable object");
    bytes.push(b' ');
    assert!(matches!(
        second.verify_bytes(&bytes),
        Err(ExportError::ContentAddressMismatch)
    ));
    let mut forged_genesis = genesis;
    forged_genesis.scope = "org_other/dev".to_owned();
    assert!(matches!(
        ExportManifest::from_bytes(Some(&forged_genesis.bytes().expect("forged bytes"))),
        Err(ExportError::ManifestMismatch)
    ));
}

#[tokio::test]
async fn export_verifies_original_ledger_and_empty_event_commits_independently() {
    let directory = tempfile::tempdir().expect("export directory");
    let mut store = CellStore::open_in_memory(common::corpus_model()).expect("store");
    let batches = [
        batch(
            Some("one"),
            vec![json!({"type":"created", "amount":"12.30"})],
        ),
        batch(None, vec![]),
        batch(None, vec![]),
        batch(
            Some("four"),
            vec![json!({"type":"changed"}), json!({"type":"done"})],
        ),
    ];
    let mut exporter = Exporter::local(directory.path()).expect("exporter");
    for input in &batches {
        let outcome = store.commit(input).expect("commit");
        let seq = outcome.receipt().body.commit_seq;
        let record = store.export_record(seq).expect("journal").expect("record");
        let head = exporter.append(&record).await.expect("upload");
        assert_eq!(head.head_commit_seq, seq);
        assert!(
            directory
                .path()
                .join(
                    object_path(&record.hash().expect("hash"))
                        .expect("path")
                        .as_ref()
                )
                .exists()
        );
    }
    let expected = store.read().verify_ledger().expect("cell chain");
    drop(store);
    let head = Exporter::local(directory.path())
        .expect("independent exporter")
        .verify()
        .await
        .expect("independent verification");
    assert_eq!(head.head_commit_seq, 4);
    assert_eq!(head.head_ledger_seq, expected.seq);
    assert_eq!(head.head_ledger_hash, expected.hash);
    assert_eq!(
        std::fs::read_dir(directory.path().join("commits"))
            .expect("objects")
            .count(),
        4
    );
}

#[tokio::test]
async fn changed_blob_missing_blob_and_modified_manifest_are_detected() {
    let directory = tempfile::tempdir().expect("export directory");
    let mut store = CellStore::open_in_memory(common::corpus_model()).expect("store");
    store
        .commit(&batch(None, vec![json!({"type":"created"})]))
        .expect("commit");
    let record = store.export_record(1).expect("journal").expect("record");
    let mut exporter = Exporter::local(directory.path()).expect("exporter");
    exporter.append(&record).await.expect("upload");
    let path = directory.path().join(
        object_path(&record.hash().expect("hash"))
            .expect("path")
            .as_ref(),
    );
    let original = std::fs::read(&path).expect("blob");
    let mut value: serde_json::Value = serde_json::from_slice(&original).expect("json");
    value["batch"]["events"][0]["type"] = json!("tampered");
    std::fs::write(&path, value.to_string()).expect("tamper");
    assert!(exporter.verify().await.is_err());
    std::fs::remove_file(&path).expect("remove blob");
    assert!(exporter.verify().await.is_err());
    std::fs::write(&path, original).expect("restore blob");
    exporter.verify().await.expect("intact");
    let manifest_path = directory.path().join(MANIFEST_PATH);
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).expect("manifest")).expect("json");
    manifest["headLedgerSeq"] = json!(100);
    std::fs::write(manifest_path, manifest.to_string()).expect("tamper manifest");
    assert!(exporter.verify().await.is_err());
}

#[tokio::test]
async fn restart_reuses_uploaded_object_and_catches_up_without_gaps_or_duplicates() {
    let directory = tempfile::tempdir().expect("directory");
    let path = directory.path().join("cell.sqlite");
    let export_dir = directory.path().join("export");
    let mut store =
        CellStore::open(&path, common::corpus_model(), StoreOptions::default()).expect("store");
    let first = batch(Some("one"), vec![json!({"type":"first"})]);
    store.commit(&first).expect("commit");
    let record = store.export_record(1).expect("journal").expect("record");
    let mut exporter = Exporter::local(&export_dir).expect("exporter");
    exporter.verify().await.expect("empty checkpoint");
    // Force failure AFTER object publication but BEFORE manifest publication.
    std::fs::create_dir(export_dir.join(MANIFEST_PATH)).expect("block manifest");
    assert!(exporter.append(&record).await.is_err());
    assert!(
        export_dir
            .join(
                object_path(&record.hash().expect("hash"))
                    .expect("path")
                    .as_ref()
            )
            .exists()
    );
    drop(exporter);
    store
        .commit(&batch(Some("two"), vec![]))
        .expect("commit while unavailable");
    drop(store);
    std::fs::remove_dir(export_dir.join(MANIFEST_PATH)).expect("storage repaired");
    let mut store =
        CellStore::open(&path, common::corpus_model(), StoreOptions::default()).expect("reopen");
    store.commit(&first).expect("idempotent redelivery");
    let mut restarted = Exporter::local(&export_dir).expect("restarted exporter");
    assert_eq!(
        restarted
            .verify()
            .await
            .expect("no published manifest")
            .head_commit_seq,
        0
    );
    for seq in 1..=2 {
        let record = store.export_record(seq).expect("journal").expect("record");
        restarted.append(&record).await.expect("resume");
        restarted
            .append(&record)
            .await
            .expect("retry acknowledgement");
    }
    let head = restarted.verify().await.expect("verify resumed");
    assert_eq!(head.head_commit_seq, 2);
    assert_eq!(head.head_ledger_seq, 1);
    assert!(store.export_record(3).expect("journal").is_none());
    assert_eq!(
        std::fs::read_dir(export_dir.join("commits"))
            .expect("objects")
            .count(),
        2
    );
}

#[tokio::test]
async fn hash_chained_envelope_rejects_changed_receipts_scope_and_sequences() {
    let directory = tempfile::tempdir().expect("directory");
    let mut store = CellStore::open_in_memory(common::corpus_model()).expect("store");
    store
        .commit(&batch(None, vec![json!({"type":"first"})]))
        .expect("commit");
    store
        .commit(&batch(None, vec![json!({"type":"second"})]))
        .expect("commit");
    let first = store.export_record(1).expect("journal").expect("record");
    let second = store.export_record(2).expect("journal").expect("record");
    let mut exporter = Exporter::local(directory.path()).expect("exporter");
    assert!(exporter.append(&second).await.is_err(), "no genesis gap");
    exporter.append(&first).await.expect("first");
    let mut changed = second.clone();
    changed.receipt.hash = "0".repeat(64);
    assert!(exporter.append(&changed).await.is_err());
    let mut changed = second.clone();
    changed
        .bindings
        .insert("scope".to_owned(), "another tenant".to_owned());
    assert!(exporter.append(&changed).await.is_err());
    let mut changed = second.clone();
    changed.entries[0].prev_hash = "0".repeat(64);
    assert!(exporter.append(&changed).await.is_err());
    let mut changed = second.clone();
    changed.model_hash = "1".repeat(64);
    assert!(matches!(
        exporter.append(&changed).await,
        Err(norn_cell::export::ExportError::IdentityMismatch)
    ));
    exporter.append(&second).await.expect("valid second");
    assert_eq!(
        exporter.verify().await.expect("verify").model_hash,
        store.model().model_hash()
    );
}
