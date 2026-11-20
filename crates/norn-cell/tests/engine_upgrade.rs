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
    CellError, CellStore, CommitOutcome, InstalledDefinitionInput, JournalMode, Model,
    RestoreError, StoreOptions,
};
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
        ..StoreOptions::default()
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
    assert_eq!(after, expected, "the upgrade writes the stamp alone");
    assert_eq!(
        engine_release::admission(&raw(&path)).expect("admission"),
        Admission::Current
    );

    // A reopen at the current version migrates nothing, and the upgraded
    // cell keeps committing on the chain it inherited.
    let store = open(&path).expect("reopen");
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
/// fork pin is past its head, refuses and stays byte-identical, unstamped.
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
        let dir = tempfile::tempdir().expect("directory");
        let path = dir.path().join("cell.sqlite");
        engine_fixture::load(&image, &path);
        let before = bytes(&path);
        let error = open(&path).expect_err(name);
        assert!(refused(&error), "{name}: unexpected refusal: {error}");
        assert_eq!(bytes(&path), before, "{name}: the file is byte-identical");
        assert_eq!(
            engine_release::stored_version(&raw(&path)).expect("version"),
            StoredVersion::Unstamped,
            "{name}: the stamp is not committed"
        );
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
/// another opener has written `stamp`: provisioning races that opener.
struct Racing {
    inner: NativeEngine,
    path: PathBuf,
    stamp: RefCell<Option<String>>,
    raced: RefCell<Option<Vec<u8>>>,
}

impl SqlEngine for Racing {
    fn read_isolation(&self) -> ReadIsolation {
        self.inner.read_isolation()
    }
    fn execute(&self, sql: &str, params: &[SqlValue]) -> Result<(), CellError> {
        self.inner.execute(sql, params)
    }
    fn query(&self, sql: &str, params: &[SqlValue]) -> Result<Vec<Vec<SqlValue>>, CellError> {
        self.inner.query(sql, params)
    }
    fn execute_batch(&self, sql: &str) -> Result<(), CellError> {
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
