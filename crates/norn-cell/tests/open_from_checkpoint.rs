//! OPEN-CHECKPOINT-1 (#1533): an open verifies the history after the cell's
//! last verified checkpoint, and a tampered history is still refused: a
//! checkpoint that no longer anchors the ledger, a replaced receipt it
//! covers, or any change after it refuses the open, and an old row changed
//! in place is refused when it is read and by a verification from genesis.
#![cfg(feature = "native")]
#![allow(
    clippy::arithmetic_side_effects,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs,
    reason = "integration-test crate"
)]

#[path = "support/engine_fixture.rs"]
mod engine_fixture;

use std::collections::BTreeMap;
use std::path::Path;

use norn_cell::engine::{NativeEngine, SqlEngine, SqlValue};
use norn_cell::test_door::TestCommit as _;
use norn_cell::{CellError, CellStore, CommitBatch, FactWriteRow, MAIN_WORLD, StoreOptions};
use norn_ir::RuntimeValue;

/// Commits apart the engine records a checkpoint (`open_checkpoint::EVERY`).
const EVERY: u64 = 128;

fn open(path: &Path) -> Result<CellStore<NativeEngine>, CellError> {
    CellStore::open(path, engine_fixture::model(), StoreOptions::default())
}

fn raw(path: &Path) -> NativeEngine {
    NativeEngine::open(path, StoreOptions::default()).expect("engine")
}

fn put(n: u64) -> CommitBatch {
    CommitBatch {
        world: MAIN_WORLD.to_owned(),
        scope: engine_fixture::SCOPE.to_owned(),
        logical_time: 10_000 + n,
        idempotency_key: Some(format!("put-{n}")),
        writes: vec![FactWriteRow::Put {
            object_type: "fixture.Item".to_owned(),
            key: format!("c{}", n % 7),
            values: BTreeMap::from([(
                "qty".to_owned(),
                RuntimeValue::from(i64::try_from(n).expect("qty")),
            )]),
        }],
        ..CommitBatch::default()
    }
}

/// A cell of `commits` commits (the fixture's history, then puts).
fn cell(path: &Path, commits: u64) {
    engine_fixture::write_cell(path);
    let mut store = open(path).expect("open");
    for n in engine_fixture::COMMITS..commits {
        store.commit(&put(n)).expect("commit");
    }
    assert_eq!(store.read().verify_ledger().expect("ledger").commits, commits);
}

fn checkpoint(path: &Path) -> Option<(u64, u64, String, String)> {
    raw(path)
        .query(
            "SELECT commits, seq, hash, receipts_root FROM _norn_open_checkpoint WHERE id = 1",
            &[],
        )
        .expect("checkpoint")
        .into_iter()
        .next()
        .map(|row| match &row[..] {
            [
                SqlValue::Integer(commits),
                SqlValue::Integer(seq),
                SqlValue::Text(hash),
                SqlValue::Text(root),
            ] => (
                u64::try_from(*commits).expect("commits"),
                u64::try_from(*seq).expect("seq"),
                hash.clone(),
                root.clone(),
            ),
            other => panic!("checkpoint row {other:?}"),
        })
}

fn execute(path: &Path, sql: &str) {
    raw(path).execute(sql, &[]).expect("tamper");
}

fn refused(path: &Path) -> String {
    match open(path) {
        Ok(_) => panic!("a tampered history opened"),
        Err(error) => error.to_string(),
    }
}

#[test]
fn every_hundred_twenty_eighth_commit_records_the_verified_head() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    cell(&path, EVERY - 1);
    assert_eq!(checkpoint(&path), None, "no checkpoint before {EVERY} commits");
    cell_more(&path, EVERY - 1, 2 * EVERY + 3);
    let (commits, seq, hash, _root) = checkpoint(&path).expect("a checkpoint");
    assert_eq!(commits, 2 * EVERY, "the last multiple of {EVERY}");
    let store = open(&path).expect("open from the checkpoint");
    let entry = store
        .read()
        .ledger_page(seq.saturating_sub(1), 1)
        .expect("ledger");
    assert_eq!(entry[0].hash, hash, "it names the ledger head at its commit");
    assert_eq!(store.read().verify_ledger().expect("ledger").commits, 2 * EVERY + 3);
}

fn cell_more(path: &Path, from: u64, to: u64) {
    let mut store = open(path).expect("open");
    for n in from..to {
        store.commit(&put(n)).expect("commit");
    }
}

#[test]
fn an_open_from_the_checkpoint_admits_the_same_cell_a_genesis_open_admits() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    cell(&path, EVERY + 10);
    assert!(checkpoint(&path).is_some());
    let from_checkpoint = open(&path).expect("open").verified_head().expect("head");
    execute(&path, "DELETE FROM _norn_open_checkpoint");
    let from_genesis = open(&path).expect("open").verified_head().expect("head");
    assert_eq!(from_checkpoint, from_genesis);
    // An open without a checkpoint verifies from genesis and writes nothing.
    assert_eq!(checkpoint(&path), None);
}

#[test]
fn a_checkpoint_that_no_longer_anchors_the_ledger_refuses_the_open() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    cell(&path, EVERY + 10);
    // A head rewritten with its whole chain no longer matches the checkpoint.
    execute(
        &path,
        "UPDATE _norn_open_checkpoint SET hash = '0000000000000000000000000000000000000000000000000000000000000000'",
    );
    assert!(refused(&path).contains("checkpoint"), "{}", refused(&path));
}

#[test]
fn a_replaced_receipt_the_checkpoint_covers_refuses_the_open() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    cell(&path, EVERY + 10);
    // Commit 2's receipt digest, replaced as if rewritten with its own hash.
    execute(
        &path,
        "UPDATE _norn_receipt SET receipt_hash = 'ff' || substr(receipt_hash, 3) WHERE commit_seq = 2",
    );
    assert!(refused(&path).contains("root"), "{}", refused(&path));
}

#[test]
fn any_change_after_the_checkpoint_refuses_the_open() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    cell(&path, EVERY + 10);
    let fresh = dir.path().join("fresh.sqlite");
    std::fs::copy(&path, &fresh).expect("copy");
    // A ledger event after the checkpoint.
    execute(
        &path,
        &format!("UPDATE _norn_ledger SET hash = 'ff' || substr(hash, 3) WHERE commit_seq = {}", EVERY + 5),
    );
    refused(&path);
    // A value version after the checkpoint.
    execute(
        &fresh,
        &format!(
            "UPDATE _norn_fact_values SET hash = 'ff' || substr(hash, 3) WHERE commit_seq = {}",
            EVERY + 5
        ),
    );
    refused(&fresh);
}

#[test]
fn an_old_row_changed_in_place_is_refused_when_read_and_from_genesis() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    cell(&path, EVERY + 10);
    let row = raw(&path)
        .query(
            "SELECT world, object_type, id, version FROM _norn_fact_values WHERE commit_seq = 7",
            &[],
        )
        .expect("row");
    let [SqlValue::Text(world), SqlValue::Text(object_type), SqlValue::Text(id), SqlValue::Integer(version)] =
        &row[0][..]
    else {
        panic!("fact row {row:?}");
    };
    let version = u64::try_from(*version).expect("version");
    execute(
        &path,
        "UPDATE _norn_fact_values SET hash = 'ff' || substr(hash, 3) WHERE commit_seq = 7",
    );
    // The open verifies what came after the checkpoint; the old version it
    // does not re-read is refused when it is read ...
    let store = open(&path).expect("the open verifies from the checkpoint");
    assert!(
        store
            .read()
            .object_at_version(world, object_type, id, version)
            .is_err(),
        "a changed historical version is never served"
    );
    drop(store);
    // ... and a verification from genesis (an open with no checkpoint, as a
    // scrub runs it) refuses the cell.
    execute(&path, "DELETE FROM _norn_open_checkpoint");
    refused(&path);
}
