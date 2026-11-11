#![allow(
    clippy::expect_used,
    clippy::panic,
    missing_docs,
    reason = "test assertions"
)]

mod common;

use norn_cell::transparency::checkpoint_json;
use norn_cell::{CellStore, CommitBatch, CommitOutcome, CommitPoint, StoreOptions};
use serde_json::json;
use std::time::Instant;

fn batch(key: &str) -> CommitBatch {
    CommitBatch {
        scope: "org_demo/dev".into(),
        idempotency_key: Some(key.into()),
        events: vec![norn_ir::RuntimeValue::from_json(json!({"event": key})).expect("event")],
        ..CommitBatch::default()
    }
}

fn cell() -> CellStore {
    let mut cell = CellStore::open_in_memory(common::corpus_model()).expect("cell");
    cell.bind("scope", "org_demo/dev").expect("scope");
    cell
}

#[test]
fn runtime_proofs_reject_tampering_forks_and_truncation() {
    let mut store = cell();
    assert_eq!(
        checkpoint_json(&store.transparency_checkpoint().expect("empty root")).expect("JSON"),
        r#"{"tree_size":0,"timestamp":0,"sha256_root_hash":"47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU="}"#
    );
    assert!(store.transparency_inclusion(0).expect("query").is_none());
    assert!(store.transparency_consistency(0).expect("query").is_none());
    let receipt = match store.commit(&batch("first")).expect("commit") {
        CommitOutcome::Committed(receipt) => receipt,
        CommitOutcome::Replayed(_) => panic!("new commit"),
    };
    let old = store.transparency_checkpoint().expect("checkpoint");
    store.commit(&batch("second")).expect("append");
    let new = store.transparency_checkpoint().expect("checkpoint");
    let proof = store
        .transparency_inclusion(0)
        .expect("query")
        .expect("inclusion");
    assert!(new.verify_inclusion(&receipt.hash, 0, &proof).is_ok());
    assert!(new.verify_inclusion(&"tampered", 0, &proof).is_err());
    let consistency = store
        .transparency_consistency(old.num_leaves())
        .expect("query")
        .expect("consistency");
    assert!(new.verify_consistency(&old, &consistency).is_ok());
    assert!(old.verify_consistency(&new, &consistency).is_err());
    let mut fork = cell();
    fork.commit(&batch("rewritten")).expect("fork");
    fork.commit(&batch("second")).expect("append fork");
    assert!(
        fork.transparency_checkpoint()
            .expect("checkpoint")
            .verify_consistency(&old, &consistency)
            .is_err()
    );
    let same = store
        .transparency_consistency(2)
        .expect("query")
        .expect("equal size");
    assert!(new.verify_consistency(&new, &same).is_ok());
    assert!(
        fork.transparency_checkpoint()
            .expect("checkpoint")
            .verify_consistency(&new, &same)
            .is_err()
    );
    assert!(store.transparency_consistency(3).expect("query").is_none());
    assert!(store.transparency_inclusion(2).expect("query").is_none());
    assert_eq!(
        checkpoint_json(&new).expect("JSON"),
        checkpoint_json(&store.transparency_checkpoint().expect("checkpoint")).expect("JSON")
    );
    let value: serde_json::Value =
        serde_json::from_str(&checkpoint_json(&new).expect("JSON")).expect("decode");
    assert_eq!(value["tree_size"], 2);
    assert_eq!(value["timestamp"], 0);
}

#[test]
fn reopen_replay_and_rollback_preserve_the_commitment() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    let mut store =
        CellStore::open(&path, common::corpus_model(), StoreOptions::default()).expect("open");
    store.bind("scope", "org_demo/dev").expect("scope");
    store.commit(&batch("first")).expect("commit");
    let root = store.transparency_checkpoint().expect("checkpoint");
    store.commit(&batch("first")).expect("replay");
    assert_eq!(store.transparency_checkpoint().expect("checkpoint"), root);
    assert!(
        store
            .commit_probed(&batch("failed"), &mut |point| {
                if point == CommitPoint::ReceiptWritten {
                    Err(norn_cell::CellError::Engine("fault".into()))
                } else {
                    Ok(())
                }
            })
            .is_err()
    );
    assert_eq!(store.transparency_checkpoint().expect("checkpoint"), root);
    drop(store);
    let store =
        CellStore::open(&path, common::corpus_model(), StoreOptions::default()).expect("reopen");
    assert_eq!(store.transparency_checkpoint().expect("checkpoint"), root);
}

#[test]
fn another_handle_is_included_before_and_after_our_commit() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    let mut first =
        CellStore::open(&path, common::corpus_model(), StoreOptions::default()).expect("open");
    first.bind("scope", "org_demo/dev").expect("scope");
    let mut second =
        CellStore::open(&path, common::corpus_model(), StoreOptions::default()).expect("open");
    first.commit(&batch("one")).expect("commit");
    assert_eq!(
        second.transparency_checkpoint().expect("root"),
        first.transparency_checkpoint().expect("root")
    );
    first.commit(&batch("two")).expect("commit");
    second.commit(&batch("three")).expect("commit");
    assert_eq!(
        second.transparency_checkpoint().expect("root"),
        first.transparency_checkpoint().expect("root")
    );
    assert_eq!(
        second.transparency_checkpoint().expect("root").num_leaves(),
        3
    );
}

#[test]
#[ignore = "bounded cost measurement; run explicitly with --ignored --nocapture"]
fn measure_append_and_proofs() {
    let mut tree =
        ct_merkle::mem_backed_tree::MemoryBackedTree::<transparency_sha2::Sha256, String>::new();
    let digest = "norn1:".to_owned() + &"a".repeat(64);
    let start = Instant::now();
    for _ in 0..4096 {
        tree.push(digest.clone());
    }
    eprintln!(
        "4096 library appends: {:?}; retained leaves: {}",
        start.elapsed(),
        tree.len()
    );
    let mut store = cell();
    let start = Instant::now();
    for index in 0..256 {
        store
            .commit(&batch(&format!("entry-{index}")))
            .expect("commit");
    }
    let append = start.elapsed();
    let start = Instant::now();
    let proof = store
        .transparency_inclusion(128)
        .expect("query")
        .expect("proof");
    let inclusion = start.elapsed();
    let start = Instant::now();
    let consistency = store
        .transparency_consistency(127)
        .expect("query")
        .expect("proof");
    eprintln!(
        "256 full SQL commits: {append:?}; inclusion: {inclusion:?}, {} bytes; consistency: {:?}, {} bytes",
        proof.as_bytes().len(),
        start.elapsed(),
        consistency.as_bytes().len()
    );
}

#[test]
fn cache_read_failure_cannot_turn_a_durable_commit_into_an_error() {
    use norn_cell::{
        CellError,
        engine::{NativeEngine, SqlEngine, SqlValue},
    };
    use std::{cell::Cell, rc::Rc};
    struct FaultEngine(NativeEngine, Rc<Cell<bool>>);
    impl SqlEngine for FaultEngine {
        fn read_isolation(&self) -> norn_cell::engine::ReadIsolation {
            self.0.read_isolation()
        }
        fn execute(&self, sql: &str, params: &[SqlValue]) -> Result<(), CellError> {
            self.0.execute(sql, params)
        }
        fn execute_batch(&self, sql: &str) -> Result<(), CellError> {
            self.0.execute_batch(sql)
        }
        fn query(&self, sql: &str, params: &[SqlValue]) -> Result<Vec<Vec<SqlValue>>, CellError> {
            if self.1.get() && sql.starts_with("SELECT receipt_hash FROM _norn_receipt") {
                return Err(CellError::Engine("injected cache read failure".into()));
            }
            self.0.query(sql, params)
        }
        fn transaction<T, X: From<CellError>>(
            &self,
            f: impl FnOnce() -> Result<T, X>,
        ) -> Result<T, X> {
            self.0.transaction(f)
        }
    }
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    let mut first =
        CellStore::open(&path, common::corpus_model(), StoreOptions::default()).expect("open");
    first.bind("scope", "org_demo/dev").expect("scope");
    let fail = Rc::new(Cell::new(false));
    let engine = NativeEngine::open(&path, StoreOptions::default()).expect("engine");
    let mut second = CellStore::from_engine(
        FaultEngine(engine, Rc::clone(&fail)),
        common::corpus_model(),
    )
    .expect("open");
    first.commit(&batch("one")).expect("commit");
    fail.set(true);
    assert!(matches!(
        second
            .commit(&batch("two"))
            .expect("durable commit still succeeds"),
        CommitOutcome::Committed(_)
    ));
    assert!(second.transparency_checkpoint().is_err());
    fail.set(false);
    assert_eq!(
        second
            .transparency_checkpoint()
            .expect("catch up")
            .num_leaves(),
        2
    );
    assert_eq!(
        second.transparency_checkpoint().expect("root"),
        first.transparency_checkpoint().expect("root")
    );
}

#[test]
fn untrusted_proof_inputs_return_errors_without_panicking() {
    use norn_cell::transparency::{Checkpoint, ConsistencyProof, InclusionProof};
    let inclusion = InclusionProof::try_from_bytes(vec![]).expect("empty path");
    let consistency = ConsistencyProof::try_from_bytes(vec![]).expect("empty path");
    let checkpoint = |size| Checkpoint::new([0; 32], size);
    // Sol's exact input, and both operands of the consistency boundary.
    assert!(
        checkpoint(u64::MAX)
            .verify_inclusion(&"leaf", 1_u64 << 63, &inclusion)
            .is_err()
    );
    for size in [(1_u64 << 63) + 1, u64::MAX] {
        assert!(
            checkpoint(size)
                .verify_inclusion(&"leaf", 0, &inclusion)
                .is_err()
        );
        assert!(
            checkpoint(size)
                .verify_consistency(&checkpoint(1_u64 << 63), &consistency)
                .is_err()
        );
        assert!(
            checkpoint(1_u64 << 63)
                .verify_consistency(&checkpoint(size), &consistency)
                .is_err()
        );
        assert!(
            checkpoint(size)
                .verify_consistency(&checkpoint(size), &consistency)
                .is_err()
        );
    }
    for (size, index) in [(0, 0), (1, 1), (2, 0), (1_u64 << 63, (1_u64 << 63) - 1)] {
        assert!(
            checkpoint(size)
                .verify_inclusion(&"leaf", index, &inclusion)
                .is_err()
        );
    }
    for (old, new) in [(0, 1), (2, 1), (1, 2)] {
        assert!(
            checkpoint(new)
                .verify_consistency(&checkpoint(old), &consistency)
                .is_err()
        );
    }
    assert!(
        checkpoint(1_u64 << 63)
            .verify_consistency(&checkpoint(1_u64 << 63), &consistency)
            .is_ok()
    );
    for bytes in [vec![0], vec![0; 65 * 32]] {
        assert!(InclusionProof::try_from_bytes(bytes.clone()).is_err());
        assert!(ConsistencyProof::try_from_bytes(bytes).is_err());
    }
}
