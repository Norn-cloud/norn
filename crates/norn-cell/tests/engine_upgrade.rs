//! ENGINE-UPGRADE-1 (#1494): a cell written by an older engine opens on this
//! one through the ordered migrations with its ledger and receipts intact,
//! and a cell this engine cannot serve refuses before anything touches it.
//!
//! `fixtures/engine-storage-v1.json` is a cell written by the last storage
//! version 1 engine (`tests/engine_fixture_record.rs` records it).
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

use std::path::{Path, PathBuf};

use norn_cell::engine::{NativeEngine, SqlEngine, SqlValue};
use norn_cell::engine_release::{
    self, Admission, EngineStamp, EngineVersion, STAMP_FORMAT, STAMP_KEY, STORAGE_VERSION,
    StoredVersion,
};
use norn_cell::{CellError, CellStore, CommitOutcome, StoreOptions};
use serde_json::Value;

const STORAGE_V1: &str = include_str!("fixtures/engine-storage-v1.json");

/// The v1 fixture, loaded into a new file.
fn v1_cell(dir: &Path) -> PathBuf {
    let image: Value = serde_json::from_str(STORAGE_V1).expect("fixture");
    assert_eq!(image["storage"], 1, "the fixture records storage version 1");
    let path = dir.join("cell.sqlite");
    engine_fixture::load(&image, &path);
    path
}

fn raw(path: &Path) -> NativeEngine {
    NativeEngine::open(path, StoreOptions::default()).expect("engine")
}

fn open(path: &Path) -> Result<CellStore<NativeEngine>, CellError> {
    CellStore::open(path, engine_fixture::model(), StoreOptions::default())
}

fn set_stamp(path: &Path, stamp: &str) {
    let engine = raw(path);
    engine
        .execute(
            "UPDATE \"_norn_meta\" SET \"value\" = ?1 WHERE \"key\" = ?2",
            &[SqlValue::from(stamp), SqlValue::from(STAMP_KEY)],
        )
        .expect("stamp");
    engine.close().expect("close");
}

/// The rows of `table` in `image`.
fn rows<'a>(image: &'a Value, table: &str) -> &'a Value {
    &image["tables"][table]["rows"]
}

/// Every way a host reads a cell before or while opening it refuses `check`
/// and leaves the file's contents as they were.
fn every_open_refuses(path: &Path, check: impl Fn(&CellError) -> bool) {
    let before = engine_fixture::image_of(path);
    let refusals = [
        open(path).err(),
        CellStore::<NativeEngine>::read_binding(&raw(path), "scope").err(),
        CellStore::<NativeEngine>::read_active_definitions(&raw(path)).err(),
        CellStore::from_engine(raw(path), engine_fixture::model()).err(),
        CellStore::diagnose_history(path).err(),
    ];
    for refusal in refusals {
        let refusal = refusal.expect("the open refuses");
        assert!(check(&refusal), "unexpected refusal: {refusal}");
    }
    assert_eq!(
        engine_fixture::image_of(path),
        before,
        "a refused open changes nothing"
    );
}

#[test]
fn a_storage_v1_cell_opens_on_this_engine_with_identical_receipts() {
    let dir = tempfile::tempdir().expect("directory");
    let path = v1_cell(dir.path());
    let before = engine_fixture::image_of(&path);
    assert_eq!(
        before,
        serde_json::from_str::<Value>(STORAGE_V1).expect("fixture"),
        "the image loads losslessly"
    );
    assert_eq!(
        engine_release::stored_version(&raw(&path)).expect("version"),
        StoredVersion::Unstamped
    );
    assert_eq!(
        engine_release::admission(&raw(&path)).expect("admission"),
        Admission::Upgrade { from: 1 }
    );

    // The open runs the migrations and verifies the whole ledger and every
    // receipt under the new engine.
    let mut store = open(&path).expect("a v1 cell opens on this engine");
    let head = store.read().verify_ledger().expect("the ledger verifies");
    assert_eq!(head.commits, 3);
    let receipts = store.read().receipts().expect("receipts");
    assert_eq!(receipts.len(), 3);
    // Every recorded commit replays to its recorded receipt.
    for (batch, receipt) in engine_fixture::batches().iter().zip(&receipts) {
        match store.commit(batch).expect("replay") {
            CommitOutcome::Replayed(replayed) => assert_eq!(&replayed, receipt),
            CommitOutcome::Committed(_) => panic!("a recorded commit was committed again"),
        }
    }
    assert_eq!(store.verified_head().expect("head"), head);
    drop(store);

    let after = engine_fixture::image_of(&path);
    for table in ["_norn_ledger", "_norn_receipt"] {
        assert!(
            rows(&before, table)
                .as_array()
                .is_some_and(|rows| !rows.is_empty())
        );
        assert_eq!(
            rows(&after, table),
            rows(&before, table),
            "{table} is unchanged"
        );
    }
    // Storage version 2 adds the stamp and changes nothing else. A later data
    // migration lists what it changes here.
    let stamp = serde_json::to_string(&EngineStamp::current()).expect("stamp");
    let mut expected = before.clone();
    let meta = expected["tables"]["_norn_meta"]["rows"]
        .as_array_mut()
        .expect("meta rows");
    let next_rowid = meta
        .iter()
        .filter_map(|row| row[0].as_i64())
        .max()
        .expect("meta rowid")
        + 1;
    meta.push(serde_json::json!([next_rowid, STAMP_KEY, stamp]));
    expected["storage"] = STORAGE_VERSION.into();
    assert_eq!(after, expected, "the upgrade writes the stamp alone");
    assert_eq!(
        engine_release::admission(&raw(&path)).expect("admission"),
        Admission::Current
    );

    // A reopen at the current version migrates nothing, and the upgraded
    // cell keeps committing on the chain it inherited.
    let mut store = open(&path).expect("reopen");
    assert_eq!(store.read().verify_ledger().expect("ledger"), head);
    drop(store);
    assert_eq!(
        engine_fixture::image_of(&path),
        after,
        "a current open writes nothing"
    );
    let mut store = open(&path).expect("reopen");
    let mut next = engine_fixture::batches()[1].clone();
    next.idempotency_key = Some("after-upgrade".to_owned());
    next.logical_time = 4_000;
    let CommitOutcome::Committed(receipt) = store.commit(&next).expect("commit") else {
        panic!("a new commit");
    };
    assert_eq!(receipt.body.commit_seq, 4);
    assert_eq!(store.read().verify_ledger().expect("ledger").commits, 4);
}

#[test]
fn a_failed_open_rolls_the_upgrade_back() {
    let dir = tempfile::tempdir().expect("directory");
    let path = v1_cell(dir.path());
    let before = engine_fixture::image_of(&path);
    let other = norn_cell::Model::from_definitions(&[], &[]).expect("model");
    assert!(matches!(
        CellStore::open(&path, other, StoreOptions::default()),
        Err(CellError::SchemaMismatch { .. })
    ));
    assert_eq!(engine_fixture::image_of(&path), before);
    assert_eq!(
        engine_release::stored_version(&raw(&path)).expect("version"),
        StoredVersion::Unstamped
    );
}

#[test]
fn a_new_cell_is_stamped_with_this_engine() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    drop(open(&path).expect("new cell"));
    let StoredVersion::Stamped(stamp) =
        engine_release::stored_version(&raw(&path)).expect("version")
    else {
        panic!("a new cell is stamped");
    };
    let stamp: EngineStamp = serde_json::from_str(&stamp).expect("stamp");
    assert_eq!(stamp, EngineStamp::current());
    assert_eq!(stamp.storage, STORAGE_VERSION);
}

#[test]
fn an_older_engine_refuses_a_cell_this_engine_upgraded() {
    let dir = tempfile::tempdir().expect("directory");
    let path = v1_cell(dir.path());
    drop(open(&path).expect("upgrade"));
    // The storage version 1 engine's window, against the upgraded cell.
    let previous = EngineVersion {
        storage: 1,
        upgrades_from: 1,
    };
    let stored = engine_release::stored_version(&raw(&path)).expect("version");
    assert!(matches!(
        engine_release::admit(&stored, previous),
        Err(CellError::EngineDowngrade {
            cell: 2,
            engine: 1,
            ..
        })
    ));

    // This engine, against a cell the next storage version wrote: every open
    // path refuses with the registered code and names the writer.
    let next = STORAGE_VERSION + 1;
    set_stamp(
        &path,
        &format!(r#"{{"format":"{STAMP_FORMAT}","storage":{next},"writer":"drop-99"}}"#),
    );
    every_open_refuses(&path, |refusal| {
        matches!(
            refusal,
            CellError::EngineDowngrade { cell, writer, engine }
                if *cell == next && writer == "drop-99" && *engine == STORAGE_VERSION
        ) && refusal
            .to_string()
            .starts_with(norn_checker::ENGINE_DOWNGRADE)
    });
}

#[test]
fn an_unknown_future_engine_stamp_refuses() {
    let dir = tempfile::tempdir().expect("directory");
    let path = v1_cell(dir.path());
    drop(open(&path).expect("upgrade"));
    for stamp in [
        // A stamp encoding from a future release.
        r#"{"format":"norn.cell.engine/2","storage":7,"writer":"drop-120","layout":"columnar"}"#,
        // A version past any this engine can count.
        r#"{"format":"norn.cell.engine/1","storage":99999999999,"writer":"drop-120"}"#,
        "not a stamp",
    ] {
        set_stamp(&path, stamp);
        every_open_refuses(&path, |refusal| {
            matches!(refusal, CellError::EngineUnknownVersion { .. })
                && refusal
                    .to_string()
                    .starts_with(norn_checker::ENGINE_UNKNOWN_VERSION)
        });
    }
}
