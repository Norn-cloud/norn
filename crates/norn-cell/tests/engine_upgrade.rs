//! ENGINE-UPGRADE-1 (#1494): a cell written by an older engine opens on this
//! one through the ordered migrations with its ledger and receipts intact,
//! and a cell this engine cannot serve refuses before anything touches it.
//!
//! `fixtures/engine-storage-v<N>.json` is a cell written by the last storage
//! version `N` engine (`tests/engine_fixture_record.rs` records it).
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

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use norn_cell::engine::{NativeEngine, ReadIsolation, SqlEngine, SqlValue};
use norn_cell::engine_release::{
    self, Admission, EngineStamp, EngineVersion, STAMP_FORMAT, STAMP_KEY, STORAGE_VERSION,
    StoredVersion,
};
use norn_cell::export::Exporter;
use norn_cell::test_door::TestCommit as _;
use norn_cell::{
    CellError, CellStore, CommitOutcome, InstalledDefinitionInput, JournalMode, LedgerHead, Model,
    RestoreError, StoreOptions,
};
use serde_json::Value;

const STORAGE_V1: &str = include_str!("fixtures/engine-storage-v1.json");
const STORAGE_V2: &str = include_str!("fixtures/engine-storage-v2.json");
const STORAGE_V3: &str = include_str!("fixtures/engine-storage-v3.json");
const STORAGE_V4: &str = include_str!("fixtures/engine-storage-v4.json");

/// Storage version 3 (#1483) adds the approval recovery tables, derived from
/// the ledger, to every older cell: `_norn_approved_call` gains
/// `approved_calls` (the backfill from the cell's approving decisions), and
/// `_norn_step_call` arrives empty, as no older commit owned a step call.
fn add_approval_recovery_tables(image: &mut Value, approved_calls: &Value) {
    let objects = [
        (
            "table",
            "_norn_approved_call",
            "CREATE TABLE \"_norn_approved_call\" (\n  \"world\" TEXT NOT NULL,\n  \"decision_id\" TEXT NOT NULL,\n  \"command_id\" TEXT NOT NULL,\n  \"run_id\" TEXT NOT NULL,\n  \"resolved_at_ms\" INTEGER NOT NULL,\n  \"refused_status\" INTEGER,\n  \"refused_code\" TEXT,\n  \"refused_category\" TEXT,\n  \"consumed_seq\" INTEGER,\n  PRIMARY KEY (\"world\", \"decision_id\")\n)",
        ),
        (
            "index",
            "_norn_approved_call/run",
            "CREATE INDEX \"_norn_approved_call/run\" ON \"_norn_approved_call\" (\"world\", \"run_id\", \"decision_id\")",
        ),
        (
            "index",
            "_norn_approved_call/consumed",
            "CREATE INDEX \"_norn_approved_call/consumed\" ON \"_norn_approved_call\" (\"world\", \"consumed_seq\")",
        ),
        (
            "table",
            "_norn_step_call",
            "CREATE TABLE \"_norn_step_call\" (\n  \"world\" TEXT NOT NULL,\n  \"commit_seq\" INTEGER NOT NULL,\n  \"run_id\" TEXT NOT NULL,\n  \"command_id\" TEXT NOT NULL,\n  \"generation\" INTEGER NOT NULL,\n  PRIMARY KEY (\"world\", \"commit_seq\")\n)",
        ),
    ];
    for (kind, name, sql) in objects {
        let table = if kind == "table" {
            name
        } else {
            "_norn_approved_call"
        };
        image["schema"]
            .as_array_mut()
            .expect("schema")
            .push(serde_json::json!({ "type": kind, "name": name, "table": table, "sql": sql }));
    }
    let tables = image["tables"].as_object_mut().expect("tables");
    tables.insert(
        "_norn_approved_call".to_owned(),
        serde_json::json!({
            "columns": ["rowid", "world", "decision_id", "command_id", "run_id", "resolved_at_ms", "refused_status", "refused_code", "refused_category", "consumed_seq"],
            "rows": approved_calls,
        }),
    );
    tables.insert(
        "_norn_step_call".to_owned(),
        serde_json::json!({
            "columns": ["rowid", "world", "commit_seq", "run_id", "command_id", "generation"],
            "rows": [],
        }),
    );
}

/// The cell's transparency root, as the open checkpoint records it.
fn receipts_root(store: &CellStore<NativeEngine>) -> String {
    use std::fmt::Write as _;
    store
        .transparency_checkpoint()
        .expect("root")
        .as_bytes()
        .iter()
        .fold(String::new(), |mut hex, byte| {
            write!(hex, "{byte:02x}").expect("hex");
            hex
        })
}

/// Norn1 hash of `value`, as the engine content-addresses its own data.
fn norn1<T: serde::Serialize>(value: &T) -> String {
    use norn_ir::canonical::{HashProfile, canonical_json_of};
    HashProfile::Norn1.hash_canonical_bytes(&canonical_json_of(value).expect("canonical"))
}

/// The open checkpoint's quantity digest, folded here independently over the
/// `(commit, receipt digest)` of each receipt that carries quantity (S1,
/// #1551).
fn quantity_receipts(store: &CellStore<NativeEngine>) -> String {
    store
        .read()
        .receipts()
        .expect("receipts")
        .iter()
        .filter(|receipt| receipt.body.quantity.is_some())
        .fold(
            norn1(&"norn.open-checkpoint.quantity/1"),
            |digest, receipt| {
                norn1(&(
                    digest.as_str(),
                    receipt.body.commit_seq,
                    receipt.hash.as_str(),
                ))
            },
        )
}

/// The last export record at or before the head, as the open checkpoint
/// pins it (S2, #1551 round 2): its commit and digest, or none.
fn export_pin(store: &CellStore<NativeEngine>, commits: u64) -> (Value, Value) {
    (1..=commits)
        .rev()
        .find_map(|commit| {
            store
                .export_record(commit)
                .expect("export record")
                .map(|record| (commit.into(), record.hash().expect("record digest").into()))
        })
        .unwrap_or((Value::Null, Value::Null))
}

/// The open checkpoint's definitions digest, folded here independently over
/// every retained definition version (#1551 round 3).
fn definitions_digest(store: &CellStore<NativeEngine>) -> String {
    store
        .read()
        .installed_definition_versions()
        .expect("definition versions")
        .iter()
        .fold(
            norn1(&"norn.open-checkpoint.definitions/1"),
            |digest, version| norn1(&(digest.as_str(), norn1(version).as_str())),
        )
}

/// Storage version 5 (OPEN-CHECKPOINT-1, #1533) adds `_norn_open_checkpoint`,
/// and the upgrading open records in it the head it verified, with the root
/// of its receipts, the digest of its quantity receipts, the last export
/// record it covers and the digest of its definition versions: the one row
/// it writes besides the stamp. The row's MAC (#1551 round 4) is checked
/// here independently: HMAC-SHA256 under the test key over the row and the
/// cell's identity.
fn add_open_checkpoint(
    expected: &mut Value,
    after: &Value,
    head: &LedgerHead,
    root: &str,
    quantity: &str,
    pins: &((Value, Value), String),
) {
    let entry = after["schema"]
        .as_array()
        .expect("schema")
        .iter()
        .find(|entry| entry["name"] == "_norn_open_checkpoint")
        .expect("the upgrade adds the open checkpoint table")
        .clone();
    assert_eq!(entry["type"], "table");
    let ((export_seq, export_hash), definitions) = pins;
    let cell_id = after["tables"]["_norn_meta"]["rows"]
        .as_array()
        .expect("meta rows")
        .iter()
        .find(|row| row[1] == "binding:cell_id")
        .and_then(|row| row[2].as_str())
        .expect("the fixture is bound to a cell identity")
        .to_owned();
    let export = if export_seq.is_null() {
        Value::Null
    } else {
        serde_json::json!([export_seq, export_hash])
    };
    let message = serde_json::to_vec(&serde_json::json!([
        "norn.open-checkpoint.mac/1",
        cell_id,
        head.commits,
        head.seq,
        head.hash,
        root,
        null,
        quantity,
        export,
        definitions
    ]))
    .expect("mac message");
    let mac = {
        use hmac::{KeyInit as _, Mac as _};
        let mut mac = hmac::Hmac::<transparency_sha2::Sha256>::new_from_slice(
            b"norn.open-checkpoint.TEST-KEY.v1",
        )
        .expect("key");
        mac.update(&message);
        mac.finalize()
            .into_bytes()
            .iter()
            .fold(String::new(), |mut hex, byte| {
                use std::fmt::Write as _;
                let _infallible = write!(hex, "{byte:02x}");
                hex
            })
    };
    expected["schema"]
        .as_array_mut()
        .expect("schema")
        .push(entry);
    expected["tables"].as_object_mut().expect("tables").insert(
        "_norn_open_checkpoint".to_owned(),
        serde_json::json!({
            "columns": ["rowid", "id", "commits", "seq", "hash", "receipts_root", "boundary_seq", "boundary_commit", "quantity_receipts", "export_seq", "export_hash", "definitions", "mac"],
            "rows": [[1, 1, head.commits, head.seq, head.hash, root, null, null, quantity, export_seq, export_hash, definitions, mac]],
        }),
    );
}

/// The catalog is compared by name: a recreated index takes a new place.
fn by_name(image: &mut Value) {
    if let Some(schema) = image["schema"].as_array_mut() {
        schema.sort_by_key(|entry| entry["name"].to_string());
    }
}

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

/// Opens with the test key, as a keyed host opens (#1551 round 4).
fn keyed() -> StoreOptions {
    StoreOptions {
        checkpoint_key: Some(norn_cell::CheckpointKey::INSECURE_DEV_AND_TEST),
        ..StoreOptions::default()
    }
}

fn open(path: &Path) -> Result<CellStore<NativeEngine>, CellError> {
    CellStore::open(path, engine_fixture::model(), keyed())
}

/// Write `stamp` as another engine would, below this engine's admission.
fn set_stamp(path: &Path, stamp: &str) {
    let conn = rusqlite::Connection::open(path).expect("sqlite");
    conn.execute(
        "INSERT INTO \"_norn_meta\" (\"key\", \"value\") VALUES (?2, ?1) \
         ON CONFLICT (\"key\") DO UPDATE SET \"value\" = excluded.\"value\"",
        rusqlite::params![stamp, STAMP_KEY],
    )
    .expect("stamp");
}

/// Store `stamp` as a BLOB: the column is not STRICT, so SQLite keeps it.
fn set_blob_stamp(path: &Path, stamp: &[u8]) {
    let conn = rusqlite::Connection::open(path).expect("sqlite");
    conn.execute(
        "UPDATE \"_norn_meta\" SET \"value\" = ?1 WHERE \"key\" = ?2",
        rusqlite::params![stamp, STAMP_KEY],
    )
    .expect("stamp");
}

/// Remove the stamp, as a cell written before it existed has none.
fn unstamp(path: &Path) {
    let conn = rusqlite::Connection::open(path).expect("sqlite");
    conn.execute(
        "DELETE FROM \"_norn_meta\" WHERE \"key\" = ?1",
        rusqlite::params![STAMP_KEY],
    )
    .expect("unstamp");
}

fn newer_stamp() -> String {
    format!(
        r#"{{"format":"{STAMP_FORMAT}","storage":{},"writer":"drop-99"}}"#,
        STORAGE_VERSION + 1
    )
}

fn bytes(path: &Path) -> Vec<u8> {
    std::fs::read(path).expect("cell file")
}

fn wal() -> StoreOptions {
    StoreOptions {
        journal_mode: JournalMode::Wal,
        ..keyed()
    }
}

fn journal_mode(path: &Path) -> String {
    let rows = NativeEngine::open_read_only(path)
        .expect("read-only engine")
        .query("PRAGMA journal_mode", &[])
        .expect("journal mode");
    let SqlValue::Text(mode) = &rows[0][0] else {
        panic!("journal mode {:?}", rows[0][0]);
    };
    mode.to_ascii_lowercase()
}

/// The rows of `table` in `image`.
fn rows<'a>(image: &'a Value, table: &str) -> &'a Value {
    &image["tables"][table]["rows"]
}

/// Every way a host reads a cell before or while opening it refuses `check`
/// and leaves the file's contents as they were.
fn every_open_refuses(path: &Path, check: impl Fn(&CellError) -> bool) {
    let before = engine_fixture::image_of(path);
    let file = bytes(path);
    let read_only = || NativeEngine::open_read_only(path).expect("read-only engine");
    let refusals = [
        NativeEngine::open(path, StoreOptions::default()).err(),
        NativeEngine::open(path, wal()).err(),
        open(path).err(),
        CellStore::<NativeEngine>::read_binding(&read_only(), "scope").err(),
        CellStore::<NativeEngine>::read_active_definitions(&read_only()).err(),
        CellStore::from_engine(read_only(), engine_fixture::model()).err(),
        CellStore::diagnose_history(path).err(),
    ];
    for refusal in refusals {
        let refusal = refusal.expect("the open refuses");
        assert!(check(&refusal), "unexpected refusal: {refusal}");
    }
    assert_eq!(
        bytes(path),
        file,
        "a refused open leaves the file byte-identical"
    );
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
    let root = receipts_root(&store);
    let quantity = quantity_receipts(&store);
    let pins = (export_pin(&store, head.commits), definitions_digest(&store));
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
    // Storage version 2 adds the stamp and changes nothing else; storage
    // version 3 adds the approval recovery tables; storage version 4
    // rewrites no row. A later data migration lists what it changes here.
    let stamp = serde_json::to_string(&EngineStamp::current()).expect("stamp");
    let mut expected = before;
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
    // #1469 replaces the retained definitions' one-revision-per-version index
    // with one revision per id per version, on every open (`retained.rs`).
    let schema = expected["schema"].as_array_mut().expect("schema");
    schema.retain(|entry| entry["name"] != "_norn_retained_definition/version");
    schema.push(serde_json::json!({
        "type": "index",
        "name": "_norn_retained_definition/version_id",
        "table": "_norn_retained_definition",
        "sql": "CREATE UNIQUE INDEX \"_norn_retained_definition/version_id\" ON \"_norn_retained_definition\" (\"definitions_version\", \"id\")"
    }));
    // The v1 fixture holds no decision, so nothing is backfilled.
    add_approval_recovery_tables(&mut expected, &serde_json::json!([]));
    add_open_checkpoint(&mut expected, &after, &head, &root, &quantity, &pins);
    let mut catalog = after;
    by_name(&mut catalog);
    by_name(&mut expected);
    assert_eq!(
        catalog, expected,
        "the upgrade writes the stamp and the listed index change alone"
    );
    assert_eq!(
        engine_release::admission(&raw(&path)).expect("admission"),
        Admission::Current
    );

    // A reopen at the current version migrates nothing, and the upgraded
    // cell keeps committing on the chain it inherited.
    let store = open(&path).expect("reopen");
    assert_eq!(store.read().verify_ledger().expect("ledger"), head);
    drop(store);
    let mut reopened = engine_fixture::image_of(&path);
    by_name(&mut reopened);
    assert_eq!(reopened, catalog, "a current open writes nothing");
    let mut store = open(&path).expect("reopen");
    // A new idempotency key waits for the history scrub (#1551 round 4),
    // which the host's due-work clock runs.
    assert_eq!(store.scrub_to_end(), norn_cell::ScrubStatus::Idle);
    let mut next = engine_fixture::batches()[1].clone();
    next.idempotency_key = Some("after-upgrade".to_owned());
    next.logical_time = 4_000;
    let CommitOutcome::Committed(receipt) = store.commit(&next).expect("commit") else {
        panic!("a new commit");
    };
    assert_eq!(receipt.body.commit_seq, 4);
    assert_eq!(store.read().verify_ledger().expect("ledger").commits, 4);
}

/// A cell the last storage version 2 engine wrote (stamped, with #1469's
/// index already in place) opens on this engine with its ledger and receipts
/// intact; the upgrade runs steps 3 and 4, restamps it and adds the approval
/// recovery tables,
/// backfilling `_norn_approved_call` from the direct call the v2 engine
/// parked and approved: row for row what this engine records replaying the
/// same history into a new cell.
#[test]
fn a_storage_v2_cell_opens_on_this_engine_with_identical_receipts() {
    let image: Value = serde_json::from_str(STORAGE_V2).expect("fixture");
    assert_eq!(image["storage"], 2, "the fixture records storage version 2");
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    engine_fixture::load(&image, &path);
    let before = engine_fixture::image_of(&path);
    assert_eq!(before, image, "the image loads losslessly");
    assert_eq!(
        engine_release::admission(&raw(&path)).expect("admission"),
        Admission::Upgrade { from: 2 }
    );

    assert!(
        rows(&before, "_norn_decision")
            .as_array()
            .is_some_and(|decisions| decisions.iter().any(|row| row[2]
                == engine_fixture::APPROVAL_DECISION
                && row[3] == engine_fixture::APPROVAL_COMMAND
                && row[7] == "decided"
                && row[8] == "approve")),
        "the v2 engine recorded the approving decision"
    );

    let mut store = open(&path).expect("a v2 cell opens on this engine");
    let root = receipts_root(&store);
    let quantity = quantity_receipts(&store);
    let head = store.read().verify_ledger().expect("the ledger verifies");
    let pins = (export_pin(&store, head.commits), definitions_digest(&store));
    assert_eq!(head.commits, engine_fixture::COMMITS);
    let receipts = store.read().receipts().expect("receipts");
    assert_eq!(
        u64::try_from(receipts.len()).expect("count"),
        engine_fixture::COMMITS
    );
    for (batch, receipt) in engine_fixture::batches().iter().zip(&receipts) {
        match store.commit(batch).expect("replay") {
            CommitOutcome::Replayed(replayed) => assert_eq!(&replayed, receipt),
            CommitOutcome::Committed(_) => panic!("a recorded commit was committed again"),
        }
    }
    drop(store);

    let after = engine_fixture::image_of(&path);
    for table in ["_norn_ledger", "_norn_receipt"] {
        assert_eq!(
            rows(&after, table),
            rows(&before, table),
            "{table} is unchanged"
        );
    }
    // This engine, replaying the same history into a new cell, records the
    // approval as it commits; the upgrade must backfill exactly those rows.
    let replayed = dir.path().join("replayed.sqlite");
    engine_fixture::write_cell(&replayed);
    let replayed = engine_fixture::image_of(&replayed);
    let approved = rows(&replayed, "_norn_approved_call");
    assert_eq!(
        approved.as_array().map(Vec::len),
        Some(1),
        "a fresh cell records the one approved call: {approved}"
    );
    assert_eq!(approved[0][2], engine_fixture::APPROVAL_DECISION);
    assert_eq!(
        rows(&after, "_norn_approved_call"),
        approved,
        "the upgrade backfills the approved call row for row"
    );
    assert_eq!(
        rows(&after, "_norn_step_call"),
        rows(&replayed, "_norn_step_call")
    );
    // The step restamps the existing `engine` row in place and adds the
    // approval recovery tables; nothing else changes.
    let stamp = serde_json::to_string(&EngineStamp::current()).expect("stamp");
    let mut expected = before;
    let stamp_row = expected["tables"]["_norn_meta"]["rows"]
        .as_array_mut()
        .expect("meta rows")
        .iter_mut()
        .find(|row| row[1] == STAMP_KEY)
        .expect("the v2 cell is stamped");
    stamp_row[2] = stamp.into();
    expected["storage"] = STORAGE_VERSION.into();
    add_approval_recovery_tables(&mut expected, approved);
    add_open_checkpoint(&mut expected, &after, &head, &root, &quantity, &pins);
    let mut catalog = after;
    by_name(&mut catalog);
    by_name(&mut expected);
    assert_eq!(
        catalog, expected,
        "the upgrade writes the stamp and the approval recovery tables alone"
    );
    assert_eq!(
        engine_release::admission(&raw(&path)).expect("admission"),
        Admission::Current
    );
}

/// A cell the last storage version 3 engine wrote (#1483's approval recovery
/// tables in place, holding the approved direct call it recorded as it
/// committed) opens on this engine with its ledger and receipts intact. Step
/// 4 (MAP-DERIVATION-1, #1488) rewrites nothing: the upgrade restamps the
/// cell and changes no other row, table or index.
#[test]
fn a_storage_v3_cell_opens_on_this_engine_with_identical_receipts() {
    let image: Value = serde_json::from_str(STORAGE_V3).expect("fixture");
    assert_eq!(image["storage"], 3, "the fixture records storage version 3");
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    engine_fixture::load(&image, &path);
    let before = engine_fixture::image_of(&path);
    assert_eq!(before, image, "the image loads losslessly");
    assert_eq!(
        engine_release::admission(&raw(&path)).expect("admission"),
        Admission::Upgrade { from: 3 }
    );
    let approved = rows(&before, "_norn_approved_call");
    assert_eq!(
        approved.as_array().map(Vec::len),
        Some(1),
        "the v3 engine recorded the one approved call: {approved}"
    );
    assert_eq!(approved[0][2], engine_fixture::APPROVAL_DECISION);
    assert_eq!(approved[0][3], engine_fixture::APPROVAL_COMMAND);

    let mut store = open(&path).expect("a v3 cell opens on this engine");
    let head = store.read().verify_ledger().expect("the ledger verifies");
    let root = receipts_root(&store);
    let quantity = quantity_receipts(&store);
    let pins = (export_pin(&store, head.commits), definitions_digest(&store));
    assert_eq!(head.commits, engine_fixture::COMMITS);
    let receipts = store.read().receipts().expect("receipts");
    assert_eq!(
        u64::try_from(receipts.len()).expect("count"),
        engine_fixture::COMMITS
    );
    for (batch, receipt) in engine_fixture::batches().iter().zip(&receipts) {
        match store.commit(batch).expect("replay") {
            CommitOutcome::Replayed(replayed) => assert_eq!(&replayed, receipt),
            CommitOutcome::Committed(_) => panic!("a recorded commit was committed again"),
        }
    }
    assert_eq!(store.verified_head().expect("head"), head);
    drop(store);

    let after = engine_fixture::image_of(&path);
    for table in [
        "_norn_ledger",
        "_norn_receipt",
        "_norn_approved_call",
        "_norn_step_call",
    ] {
        assert_eq!(
            rows(&after, table),
            rows(&before, table),
            "{table} is unchanged"
        );
    }
    // This engine, replaying the same history into a new cell, records the
    // same approval recovery rows the v3 engine did.
    let replayed = dir.path().join("replayed.sqlite");
    engine_fixture::write_cell(&replayed);
    let replayed = engine_fixture::image_of(&replayed);
    for table in ["_norn_approved_call", "_norn_step_call"] {
        assert_eq!(
            rows(&after, table),
            rows(&replayed, table),
            "{table} matches a cell this engine writes"
        );
    }
    // The step restamps the existing `engine` row in place; it adds no
    // table or index and rewrites no row.
    let stamp = serde_json::to_string(&EngineStamp::current()).expect("stamp");
    let mut expected = before;
    let stamp_row = expected["tables"]["_norn_meta"]["rows"]
        .as_array_mut()
        .expect("meta rows")
        .iter_mut()
        .find(|row| row[1] == STAMP_KEY)
        .expect("the v3 cell is stamped");
    stamp_row[2] = stamp.into();
    expected["storage"] = STORAGE_VERSION.into();
    add_open_checkpoint(&mut expected, &after, &head, &root, &quantity, &pins);
    let mut catalog = after;
    by_name(&mut catalog);
    by_name(&mut expected);
    assert_eq!(catalog, expected, "the upgrade writes the stamp alone");
    assert_eq!(
        engine_release::admission(&raw(&path)).expect("admission"),
        Admission::Current
    );

    // A reopen at the current version writes nothing.
    let store = open(&path).expect("reopen");
    assert_eq!(store.read().verify_ledger().expect("ledger"), head);
    drop(store);
    let mut reopened = engine_fixture::image_of(&path);
    by_name(&mut reopened);
    assert_eq!(reopened, catalog, "a current open writes nothing");
}

/// A cell the last storage version 4 engine wrote opens on this engine with
/// its ledger and receipts intact. Step 5 (OPEN-CHECKPOINT-1, #1533) adds
/// `_norn_open_checkpoint`: the upgrading open verifies the whole history
/// and records its head and receipt root there, the one row it writes
/// besides the stamp. The next open verifies from that checkpoint and
/// writes nothing.
#[test]
fn a_storage_v4_cell_opens_on_this_engine_with_identical_receipts() {
    let image: Value = serde_json::from_str(STORAGE_V4).expect("fixture");
    assert_eq!(image["storage"], 4, "the fixture records storage version 4");
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("cell.sqlite");
    engine_fixture::load(&image, &path);
    let before = engine_fixture::image_of(&path);
    assert_eq!(before, image, "the image loads losslessly");
    assert_eq!(
        engine_release::admission(&raw(&path)).expect("admission"),
        Admission::Upgrade { from: 4 }
    );

    let mut store = open(&path).expect("a v4 cell opens on this engine");
    let head = store.read().verify_ledger().expect("the ledger verifies");
    let root = receipts_root(&store);
    let quantity = quantity_receipts(&store);
    let pins = (export_pin(&store, head.commits), definitions_digest(&store));
    assert_eq!(head.commits, engine_fixture::COMMITS);
    let receipts = store.read().receipts().expect("receipts");
    for (batch, receipt) in engine_fixture::batches().iter().zip(&receipts) {
        match store.commit(batch).expect("replay") {
            CommitOutcome::Replayed(replayed) => assert_eq!(&replayed, receipt),
            CommitOutcome::Committed(_) => panic!("a recorded commit was committed again"),
        }
    }
    assert_eq!(store.verified_head().expect("head"), head);
    drop(store);

    let after = engine_fixture::image_of(&path);
    let stamp = serde_json::to_string(&EngineStamp::current()).expect("stamp");
    let mut expected = before;
    let stamp_row = expected["tables"]["_norn_meta"]["rows"]
        .as_array_mut()
        .expect("meta rows")
        .iter_mut()
        .find(|row| row[1] == STAMP_KEY)
        .expect("the v4 cell is stamped");
    stamp_row[2] = stamp.into();
    expected["storage"] = STORAGE_VERSION.into();
    add_open_checkpoint(&mut expected, &after, &head, &root, &quantity, &pins);
    let mut catalog = after;
    by_name(&mut catalog);
    by_name(&mut expected);
    assert_eq!(
        catalog, expected,
        "the upgrade writes the stamp and the open checkpoint alone"
    );

    // A reopen verifies from the checkpoint and writes nothing.
    let store = open(&path).expect("reopen");
    assert_eq!(store.read().verify_ledger().expect("ledger"), head);
    drop(store);
    let mut reopened = engine_fixture::image_of(&path);
    by_name(&mut reopened);
    assert_eq!(reopened, catalog, "a current open writes nothing");
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
fn the_v1_window_does_not_admit_a_cell_this_engine_upgraded() {
    let dir = tempfile::tempdir().expect("directory");
    let path = v1_cell(dir.path());
    drop(open(&path).expect("upgrade"));
    // Simulated: this engine's admission with the storage version 1 window,
    // against the upgraded cell. The shipped version 1 engines read no stamp,
    // so they cannot refuse it themselves (`docs/platform/engine-upgrades.md`).
    let previous = EngineVersion {
        storage: 1,
        upgrades_from: 1,
    };
    let stored = engine_release::stored_version(&raw(&path)).expect("version");
    assert!(matches!(
        engine_release::admit(&stored, previous),
        Err(CellError::EngineDowngrade {
            cell: STORAGE_VERSION,
            engine: 1,
            ..
        })
    ));
}

#[test]
fn every_open_refuses_a_cell_a_newer_engine_wrote() {
    let dir = tempfile::tempdir().expect("directory");
    let path = v1_cell(dir.path());
    drop(open(&path).expect("upgrade"));
    // This engine, against a cell the next storage version wrote: every open
    // path refuses with the registered code and names the writer.
    let next = STORAGE_VERSION + 1;
    set_stamp(&path, &newer_stamp());
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
    // A stamp that is not text is as unreadable as malformed JSON.
    let stamp = serde_json::to_string(&EngineStamp::current()).expect("stamp");
    set_blob_stamp(&path, stamp.as_bytes());
    every_open_refuses(&path, |refusal| {
        matches!(refusal, CellError::EngineUnknownVersion { .. })
            && refusal
                .to_string()
                .starts_with(norn_checker::ENGINE_UNKNOWN_VERSION)
    });
}

/// How a refused open is recognised.
type Refused = fn(&CellError) -> bool;

fn ledger_refused(error: &CellError) -> bool {
    matches!(error, CellError::LedgerCorrupt { .. })
}

fn pin_refused(error: &CellError) -> bool {
    matches!(error, CellError::Encoding(reason) if reason.contains("past the store's head"))
}

/// U1: an open commits its upgrade only after every check that can refuse
/// the file has passed. A v1 cell whose ledger does not verify, or whose
/// fork pin is past its head, refuses and stays byte-identical, unstamped,
/// in its stored journal mode, with default and with WAL options.
#[test]
fn a_v1_cell_that_fails_verification_stays_byte_identical() {
    let image: Value = serde_json::from_str(STORAGE_V1).expect("fixture");
    let mut tampered_ledger = image.clone();
    // The second event's text no longer hashes to its chained hash.
    tampered_ledger["tables"]["_norn_ledger"]["rows"][1][4] =
        r#"{"key":"a3","type":"fixture.deleted"}"#.into();
    let mut pinned_past_head = image;
    let meta = pinned_past_head["tables"]["_norn_meta"]["rows"]
        .as_array_mut()
        .expect("meta rows");
    let mut rowid = meta
        .iter()
        .filter_map(|row| row[0].as_i64())
        .max()
        .expect("meta rowid");
    let base = serde_json::json!({"seq": 99, "hash": "0".repeat(64), "commits": 99}).to_string();
    for (key, value) in [
        ("fork:world", "what-if"),
        ("fork:parent", "main"),
        ("fork:base", base.as_str()),
        ("fork:main_horizon", "99"),
    ] {
        rowid += 1;
        meta.push(serde_json::json!([rowid, key, value]));
    }
    let cases: [(&str, Value, Refused); 2] = [
        ("ledger", tampered_ledger, ledger_refused),
        ("fork pin", pinned_past_head, pin_refused),
    ];
    for (name, image, refused) in cases {
        // A refused open keeps the stored journal mode, even one that asked for WAL.
        for (mode, options) in [("delete", StoreOptions::default()), ("wal", wal())] {
            let dir = tempfile::tempdir().expect("directory");
            let path = dir.path().join("cell.sqlite");
            engine_fixture::load(&image, &path);
            assert_eq!(
                journal_mode(&path),
                "delete",
                "{name}: the fixture is DELETE"
            );
            let before = bytes(&path);
            let error = CellStore::open(&path, engine_fixture::model(), options).expect_err(name);
            assert!(
                refused(&error),
                "{name}/{mode}: unexpected refusal: {error}"
            );
            assert_eq!(
                bytes(&path),
                before,
                "{name}/{mode}: the file is byte-identical"
            );
            assert_eq!(
                journal_mode(&path),
                "delete",
                "{name}/{mode}: the journal mode is unchanged"
            );
            assert!(
                !dir.path().join("cell.sqlite-wal").exists(),
                "{name}/{mode}: no WAL sidecar"
            );
            assert_eq!(
                engine_release::stored_version(&raw(&path)).expect("version"),
                StoredVersion::Unstamped,
                "{name}/{mode}: the stamp is not committed"
            );
        }
    }
}

/// U3: the native engine sets the file's journal mode only after admitting
/// its storage version. A newer DELETE-mode cell opened with WAL options
/// refuses, and its bytes and journal mode are unchanged.
#[test]
fn a_newer_cell_refuses_a_journal_mode_change() {
    let dir = tempfile::tempdir().expect("directory");
    let path = v1_cell(dir.path());
    drop(open(&path).expect("upgrade"));
    set_stamp(&path, &newer_stamp());
    assert_eq!(journal_mode(&path), "delete");
    let before = bytes(&path);
    assert!(matches!(
        NativeEngine::open(&path, wal()),
        Err(CellError::EngineDowngrade { .. })
    ));
    assert!(matches!(
        CellStore::open(&path, engine_fixture::model(), wal()),
        Err(CellError::EngineDowngrade { .. })
    ));
    assert_eq!(bytes(&path), before, "the file is byte-identical");
    assert_eq!(
        journal_mode(&path),
        "delete",
        "the journal mode is unchanged"
    );
    assert!(!dir.path().join("cell.sqlite-wal").exists());
}

/// An engine over the cell file whose first transaction starts only after
/// another opener has written `stamp`: provisioning races that opener. It
/// counts the schema statements it is asked to run.
struct Racing {
    inner: NativeEngine,
    path: PathBuf,
    stamp: RefCell<Option<String>>,
    raced: RefCell<Option<Vec<u8>>>,
    ddl: std::cell::Cell<usize>,
}

impl Racing {
    fn count_ddl(&self, sql: &str) {
        let head = sql.trim_start().get(..6).unwrap_or_default();
        if ["CREATE", "ALTER ", "DROP T", "DROP I"]
            .iter()
            .any(|verb| head.eq_ignore_ascii_case(verb))
        {
            self.ddl.set(self.ddl.get() + 1);
        }
    }
}

impl SqlEngine for Racing {
    fn read_isolation(&self) -> ReadIsolation {
        self.inner.read_isolation()
    }
    fn execute(&self, sql: &str, params: &[SqlValue]) -> Result<(), CellError> {
        self.count_ddl(sql);
        self.inner.execute(sql, params)
    }
    fn query(&self, sql: &str, params: &[SqlValue]) -> Result<Vec<Vec<SqlValue>>, CellError> {
        self.inner.query(sql, params)
    }
    fn execute_batch(&self, sql: &str) -> Result<(), CellError> {
        self.count_ddl(sql);
        self.inner.execute_batch(sql)
    }
    fn transaction<T, X: From<CellError>>(&self, f: impl FnOnce() -> Result<T, X>) -> Result<T, X> {
        if let Some(stamp) = self.stamp.borrow_mut().take() {
            set_stamp(&self.path, &stamp);
            self.raced.replace(Some(bytes(&self.path)));
        }
        self.inner.transaction(f)
    }
}

fn provision_input() -> InstalledDefinitionInput {
    InstalledDefinitionInput {
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

fn provision<E: SqlEngine>(engine: E) -> Result<CellStore<E>, CellError> {
    CellStore::provision_definitions(
        engine,
        Model::from_definitions(&[], &[])?,
        &provision_input(),
        engine_fixture::SCOPE,
    )
}

/// U2: provisioning an existing cell admits it and runs the migrations at
/// the start of its write transaction. A second opener that makes the cell
/// newer after provisioning's first check is refused with nothing written;
/// with no race, an existing v1 cell is upgraded and stamped.
#[test]
fn provisioning_admits_and_upgrades_inside_its_transaction() {
    let dir = tempfile::tempdir().expect("directory");
    let path = dir.path().join("tenant.sqlite");
    drop(provision(raw(&path)).expect("first provisioning"));
    unstamp(&path);
    assert_eq!(
        engine_release::stored_version(&raw(&path)).expect("version"),
        StoredVersion::Unstamped
    );

    let racing = Racing {
        inner: raw(&path),
        path: path.clone(),
        stamp: RefCell::new(Some(newer_stamp())),
        raced: RefCell::default(),
        ddl: std::cell::Cell::new(0),
    };
    let raced = std::rc::Rc::new(racing);
    let refused = provision(RcEngine(std::rc::Rc::clone(&raced)));
    assert!(
        matches!(
            refused,
            Err(CellError::EngineDowngrade { cell, .. }) if cell == STORAGE_VERSION + 1
        ),
        "{:?}",
        refused.err()
    );
    let after_race = raced.raced.borrow().clone().expect("the opener raced");
    assert_eq!(bytes(&path), after_race, "provisioning wrote nothing");
    assert_eq!(
        raced.ddl.get(),
        0,
        "the raced admission refuses before any schema statement runs"
    );

    unstamp(&path);
    let store = provision(raw(&path)).expect("retried provisioning");
    drop(store);
    assert_eq!(
        engine_release::admission(&raw(&path)).expect("admission"),
        Admission::Current,
        "provisioning upgraded and stamped the v1 cell"
    );
}

/// Shares one [`Racing`] engine so the test can read what it saw.
struct RcEngine(std::rc::Rc<Racing>);

impl SqlEngine for RcEngine {
    fn read_isolation(&self) -> ReadIsolation {
        self.0.read_isolation()
    }
    fn execute(&self, sql: &str, params: &[SqlValue]) -> Result<(), CellError> {
        self.0.execute(sql, params)
    }
    fn query(&self, sql: &str, params: &[SqlValue]) -> Result<Vec<Vec<SqlValue>>, CellError> {
        self.0.query(sql, params)
    }
    fn execute_batch(&self, sql: &str) -> Result<(), CellError> {
        self.0.execute_batch(sql)
    }
    fn transaction<T, X: From<CellError>>(&self, f: impl FnOnce() -> Result<T, X>) -> Result<T, X> {
        self.0.transaction(f)
    }
}

/// U5: a fork admits the copied snapshot's storage version before it reads
/// the copy's head or writes its pin.
#[test]
fn a_fork_refuses_a_copy_another_engine_made_newer() {
    let dir = tempfile::tempdir().expect("directory");
    let path = v1_cell(dir.path());
    let store = open(&path).expect("upgrade");
    let head = store.verified_head().expect("head");
    set_stamp(&path, &newer_stamp());
    assert!(matches!(
        store.fork("what-if", &head),
        Err(CellError::EngineDowngrade { .. })
    ));
}

async fn export_all(store: &CellStore<NativeEngine>, exporter: &mut Exporter) {
    let start = exporter.verify().await.expect("verify").head_commit_seq + 1;
    for seq in start..=store.export_commit_count().expect("count") {
        exporter
            .append(&store.export_record(seq).expect("journal").expect("record"))
            .await
            .expect("append");
    }
}

/// The v1 fixture upgraded, and a verified export of its legacy records.
async fn legacy_export(dir: &Path) -> (CellStore<NativeEngine>, Exporter) {
    let path = v1_cell(dir);
    let store = open(&path).expect("upgrade");
    let mut exporter = Exporter::local(&dir.join("legacy")).expect("exporter");
    export_all(&store, &mut exporter).await;
    (store, exporter)
}

/// U4: a legacy export, whose records carry no storage version, restores as
/// storage version 1: its records keep no version and the restored file is
/// at this engine's.
#[tokio::test]
async fn a_legacy_export_restores_as_storage_version_1() {
    let dir = tempfile::tempdir().expect("directory");
    let (_store, mut exporter) = legacy_export(dir.path()).await;
    let manifest = exporter.verify().await.expect("verified");
    let target = dir.path().join("restored.sqlite");
    CellStore::restore_from(
        &mut exporter,
        &target,
        engine_fixture::model(),
        engine_fixture::SCOPE,
        &manifest.bindings,
        StoreOptions::default(),
    )
    .await
    .expect("a legacy export restores");
    let restored = open(&target).expect("restored cell");
    for seq in 1..=3 {
        let record = restored
            .export_record(seq)
            .expect("journal")
            .expect("record");
        assert_eq!(
            record.storage_version, None,
            "a replayed record keeps no version"
        );
    }
    drop(restored);
    assert_eq!(
        engine_release::admission(&raw(&target)).expect("admission"),
        Admission::Current
    );
}

/// U4: export records carry the storage version that committed them, inside
/// their content address; an export a newer engine wrote refuses before
/// anything is staged.
#[tokio::test]
async fn an_export_a_newer_engine_wrote_refuses_before_staging() {
    let dir = tempfile::tempdir().expect("directory");
    let (mut store, _legacy) = legacy_export(dir.path()).await;
    let mut next = engine_fixture::batches()[1].clone();
    next.idempotency_key = Some("after-upgrade".to_owned());
    next.logical_time = 4_000;
    store.commit(&next).expect("commit");
    let mut records: Vec<_> = (1..=4)
        .map(|seq| store.export_record(seq).expect("journal").expect("record"))
        .collect();
    assert_eq!(records[3].storage_version, Some(STORAGE_VERSION));

    // The same history as a newer engine would export it: every record names
    // the newer version inside its content address, and the chain verifies.
    let mut previous = norn_cell::system::GENESIS_HASH.to_owned();
    for record in &mut records {
        record.storage_version = Some(STORAGE_VERSION + 1);
        record.previous = previous;
        previous = record.hash().expect("hash");
    }
    let mut newer = Exporter::local(&dir.path().join("newer")).expect("exporter");
    for record in &records {
        newer.append(record).await.expect("append");
    }
    let manifest = newer.verify().await.expect("the newer export verifies");
    let staging = dir.path().join("target");
    std::fs::create_dir(&staging).expect("target directory");
    let refused = CellStore::restore_from(
        &mut newer,
        &staging.join("restored.sqlite"),
        engine_fixture::model(),
        engine_fixture::SCOPE,
        &manifest.bindings,
        StoreOptions::default(),
    )
    .await;
    assert!(
        matches!(
            refused,
            Err(RestoreError::Cell(CellError::EngineDowngrade { cell, .. }))
                if cell == STORAGE_VERSION + 1
        ),
        "{:?}",
        refused.err()
    );
    assert_eq!(
        std::fs::read_dir(&staging)
            .expect("target directory")
            .count(),
        0,
        "nothing is staged"
    );
}
