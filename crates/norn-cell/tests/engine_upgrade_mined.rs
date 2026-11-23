//! Mined invariants of the cell storage-upgrade path (#45 pilot, #1429).
//!
//! Each property is one claim read off the upgrade module's example tests
//! and source (`engine_release.rs`, `engine_upgrade.rs`), falsified over
//! generated inputs: storage versions and engine windows, stamps a release
//! may leave, journal modes, corrupt or valid ledgers, open paths and a
//! racing second opener. They are kept because they hold on this head.
//!
//! - M1 admission orders any cell version against any engine window.
//! - M2 a stamp this engine cannot read is an unknown version, shown bounded.
//! - M3 a refused open, on any open path and in any journal mode, leaves the
//!   file byte-identical, its journal mode unchanged and no WAL frame written.
//! - M4 a storage version 1 cell whose ledger or fork pin does not verify
//!   refuses for that reason and stays byte-identical and unstamped, in any
//!   journal mode.
//! - M5 an upgrade keeps the ledger and receipts, stamps this engine, and any
//!   sequence of later opens writes nothing to them.
//! - M6 provisioning against a second opener that restamps the cell before
//!   its transaction refuses and writes nothing.
//! - M7 a fork refuses a copy that is not at this engine's storage version;
//!   an unstamped copy as storage version 1.
//! - M8 every export record this engine commits names its storage version;
//!   records an older engine wrote name none.
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

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};

use norn_cell::engine::{NativeEngine, ReadIsolation, SqlEngine, SqlValue};
use norn_cell::engine_release::{
    self, Admission, EngineStamp, EngineVersion, STAMP_FORMAT, STAMP_KEY, STORAGE_VERSION,
    StoredVersion,
};
use norn_cell::test_door::TestCommit as _;
use norn_cell::{
    CellError, CellStore, CommitBatch, InstalledDefinitionInput, JournalMode, Model, StoreOptions,
};
use proptest::prelude::*;
use serde_json::Value;

const STORAGE_V1: &str = include_str!("fixtures/engine-storage-v1.json");

fn image_v1() -> Value {
    serde_json::from_str(STORAGE_V1).expect("fixture")
}

fn load(dir: &Path, image: &Value) -> PathBuf {
    let path = dir.join("cell.sqlite");
    engine_fixture::load(image, &path);
    path
}

fn options(mode: JournalMode) -> StoreOptions {
    StoreOptions {
        journal_mode: mode,
        ..StoreOptions::default()
    }
}

fn raw(path: &Path) -> NativeEngine {
    NativeEngine::open_read_only(path).expect("read-only engine")
}

fn bytes(path: &Path) -> Vec<u8> {
    std::fs::read(path).expect("cell file")
}

fn wal_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push("-wal");
    PathBuf::from(name)
}

/// The WAL's bytes, or `None` when there is no WAL file. Any other read
/// error fails the test instead of counting as absence.
fn wal_bytes(path: &Path) -> Option<Vec<u8>> {
    match std::fs::read(wal_path(path)) {
        Ok(wal) => Some(wal),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => panic!("reading the WAL: {error}"),
    }
}

/// The frames a WAL holds: a 32-byte header, then frames of a 24-byte header
/// and one page (the page size is the header's big-endian `u32` at 8).
fn wal_frames(wal: &[u8]) -> usize {
    let Some(page) = wal.get(8..12) else {
        return 0;
    };
    let page = u32::from_be_bytes(page.try_into().expect("four bytes"));
    wal.len().saturating_sub(32) / (24 + usize::try_from(page).expect("page size"))
}

/// The admission or refusal variant of `outcome` and the storage versions it
/// reports (cell, engine), so two outcomes compare by more than success.
fn verdict(outcome: &Result<Admission, CellError>) -> (String, Option<u32>, Option<u32>) {
    match outcome {
        Ok(Admission::Upgrade { from }) => ("upgrade".into(), Some(*from), None),
        Ok(admission) => (format!("{admission:?}"), None, None),
        Err(CellError::EngineUpgradeUnsupported { cell, engine, .. }) => {
            ("unsupported".into(), Some(*cell), Some(*engine))
        }
        Err(CellError::EngineDowngrade { cell, engine, .. }) => {
            ("downgrade".into(), Some(*cell), Some(*engine))
        }
        Err(CellError::EngineUnknownVersion { .. }) => ("unknown".into(), None, None),
        Err(other) => (format!("other: {other}"), None, None),
    }
}

fn journal_mode(path: &Path) -> String {
    let conn =
        rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("sqlite");
    let mode: String = conn
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .expect("journal mode");
    mode.to_ascii_lowercase()
}

/// Write `stamp` (or remove it) as another engine would, below admission.
fn set_stamp(path: &Path, stamp: Option<&str>) {
    let conn = rusqlite::Connection::open(path).expect("sqlite");
    match stamp {
        Some(stamp) => conn.execute(
            "INSERT INTO \"_norn_meta\" (\"key\", \"value\") VALUES (?1, ?2) \
             ON CONFLICT (\"key\") DO UPDATE SET \"value\" = excluded.\"value\"",
            rusqlite::params![STAMP_KEY, stamp],
        ),
        None => conn.execute(
            "DELETE FROM \"_norn_meta\" WHERE \"key\" = ?1",
            rusqlite::params![STAMP_KEY],
        ),
    }
    .expect("stamp");
}

fn set_journal_mode(path: &Path, mode: JournalMode) {
    let conn = rusqlite::Connection::open(path).expect("sqlite");
    let pragma = match mode {
        JournalMode::Delete => "DELETE",
        JournalMode::Wal => "WAL",
    };
    let _mode: String = conn
        .query_row(&format!("PRAGMA journal_mode = {pragma}"), [], |row| {
            row.get(0)
        })
        .expect("journal mode");
}

fn stamp_text(format: &str, storage: &str, writer: &str) -> String {
    format!(r#"{{"format":"{format}","storage":{storage},"writer":"{writer}"}}"#)
}

/// A stamp this engine must refuse, as another release could leave it.
#[derive(Debug, Clone)]
enum Refusing {
    /// A newer storage version, `STORAGE_VERSION + n`.
    Newer(u32),
    /// A future stamp encoding.
    FutureFormat(u32),
    /// Storage version 0, negative, fractional, string or past `u32`.
    BadNumber(&'static str),
    /// Not a stamp at all.
    Garbage(String),
}

impl Refusing {
    fn text(&self) -> String {
        match self {
            Self::Newer(n) => {
                stamp_text(STAMP_FORMAT, &(STORAGE_VERSION + n).to_string(), "drop-99")
            }
            Self::FutureFormat(n) => stamp_text("norn.cell.engine/2", &n.to_string(), "drop-120"),
            Self::BadNumber(number) => stamp_text(STAMP_FORMAT, number, "dev"),
            Self::Garbage(text) => text.clone(),
        }
    }

    fn matches(&self, error: &CellError) -> bool {
        match self {
            Self::Newer(_) => matches!(error, CellError::EngineDowngrade { .. }),
            Self::FutureFormat(_) | Self::BadNumber(_) | Self::Garbage(_) => {
                matches!(error, CellError::EngineUnknownVersion { .. })
            }
        }
    }
}

fn refusing() -> impl Strategy<Value = Refusing> {
    prop_oneof![
        (1_u32..1_000).prop_map(Refusing::Newer),
        (1_u32..10).prop_map(Refusing::FutureFormat),
        prop::sample::select(vec!["0", "-1", "2.5", "\"2\"", "99999999999", "null"])
            .prop_map(Refusing::BadNumber),
        "[a-z {}:\",0-9]{0,40}"
            .prop_filter("not a readable stamp", |text| {
                serde_json::from_str::<EngineStamp>(text).is_err()
            })
            .prop_map(Refusing::Garbage),
    ]
}

fn journal() -> impl Strategy<Value = JournalMode> {
    prop_oneof![Just(JournalMode::Delete), Just(JournalMode::Wal)]
}

/// Every way a host reads a cell before or while opening it.
#[derive(Debug, Clone, Copy)]
enum OpenPath {
    Native(bool),
    Store(bool),
    ReadBinding,
    ReadActiveDefinitions,
    FromEngine,
    DiagnoseHistory,
    Provision,
}

fn open_path() -> impl Strategy<Value = OpenPath> {
    prop_oneof![
        any::<bool>().prop_map(OpenPath::Native),
        any::<bool>().prop_map(OpenPath::Store),
        Just(OpenPath::ReadBinding),
        Just(OpenPath::ReadActiveDefinitions),
        Just(OpenPath::FromEngine),
        Just(OpenPath::DiagnoseHistory),
        Just(OpenPath::Provision),
    ]
}

fn mode_of(wal: bool) -> JournalMode {
    if wal {
        JournalMode::Wal
    } else {
        JournalMode::Delete
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

/// The refusal of opening `path` along `how`, or `None` if it opened.
fn try_open(path: &Path, how: OpenPath) -> Option<CellError> {
    match how {
        OpenPath::Native(wal) => NativeEngine::open(path, options(mode_of(wal))).err(),
        OpenPath::Store(wal) => {
            CellStore::open(path, engine_fixture::model(), options(mode_of(wal))).err()
        }
        OpenPath::ReadBinding => CellStore::<NativeEngine>::read_binding(&raw(path), "scope").err(),
        OpenPath::ReadActiveDefinitions => {
            CellStore::<NativeEngine>::read_active_definitions(&raw(path)).err()
        }
        OpenPath::FromEngine => CellStore::from_engine(raw(path), engine_fixture::model()).err(),
        OpenPath::DiagnoseHistory => CellStore::diagnose_history(path).err(),
        // Straight to provisioning's own admission check, past any open.
        OpenPath::Provision => provision(raw(path)).err(),
    }
}

fn ledger_and_receipts(image: &Value) -> (Value, Value) {
    (
        image["tables"]["_norn_ledger"]["rows"].clone(),
        image["tables"]["_norn_receipt"]["rows"].clone(),
    )
}

/// A ledger or pin the open must refuse.
#[derive(Debug, Clone)]
enum Corruption {
    /// Replace event `row`'s text with another valid event.
    EventText(usize, String),
    /// Replace event `row`'s chained hash.
    EventHash(usize, u8),
    /// A fork pin whose base is `n` commits past the head.
    PinPastHead(u64),
}

fn corruption() -> impl Strategy<Value = Corruption> {
    prop_oneof![
        (0_usize..2, "[a-z]{1,8}").prop_map(|(row, key)| Corruption::EventText(
            row,
            format!(r#"{{"key":"{key}","type":"fixture.tampered"}}"#)
        )),
        (0_usize..2, any::<u8>()).prop_map(|(row, byte)| Corruption::EventHash(row, byte)),
        (1_u64..1_000).prop_map(Corruption::PinPastHead),
    ]
}

fn corrupt(mut image: Value, corruption: &Corruption) -> Value {
    match corruption {
        Corruption::EventText(row, text) => {
            image["tables"]["_norn_ledger"]["rows"][*row][4] = text.clone().into();
        }
        Corruption::EventHash(row, byte) => {
            let columns = image["tables"]["_norn_ledger"]["rows"][*row]
                .as_array_mut()
                .expect("ledger row");
            // The chained hash is the one 64-hex text column of an event row.
            let column = columns
                .iter_mut()
                .find(|value| {
                    value.as_str().is_some_and(|text| {
                        text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit())
                    })
                })
                .expect("a chained hash column");
            let mut hash = column.as_str().expect("hash").to_owned();
            let replacement = format!("{byte:02x}");
            let flipped = if hash.starts_with(&replacement) {
                format!("{:02x}", byte ^ 0xff)
            } else {
                replacement
            };
            hash.replace_range(0..2, &flipped);
            *column = hash.into();
        }
        Corruption::PinPastHead(past) => {
            let meta = image["tables"]["_norn_meta"]["rows"]
                .as_array_mut()
                .expect("meta rows");
            let mut rowid = meta
                .iter()
                .filter_map(|row| row[0].as_i64())
                .max()
                .expect("meta rowid");
            let commits = 3 + past;
            let base =
                serde_json::json!({"seq": commits, "hash": "0".repeat(64), "commits": commits})
                    .to_string();
            // The horizon is the base's own, so the pin's one fault is a base
            // past the head (a horizon past the base refuses for another reason).
            let horizon = commits.to_string();
            for (key, value) in [
                ("fork:world", "what-if"),
                ("fork:parent", "main"),
                ("fork:base", base.as_str()),
                ("fork:main_horizon", horizon.as_str()),
            ] {
                rowid += 1;
                meta.push(serde_json::json!([rowid, key, value]));
            }
        }
    }
    image
}

/// An engine whose first transaction starts only after another opener has
/// written (or removed) `stamp`: the opener races provisioning.
struct Racing {
    inner: NativeEngine,
    path: PathBuf,
    /// The stamp the opener writes, or `None` to remove it.
    stamp: Option<String>,
    fired: Cell<bool>,
    raced: RefCell<Option<Vec<u8>>>,
}

impl SqlEngine for Racing {
    fn read_isolation(&self) -> ReadIsolation {
        self.inner.read_isolation()
    }
    fn definition_writes<T>(
        &self,
        f: impl FnOnce() -> Result<T, CellError>,
    ) -> Result<T, CellError> {
        self.inner.definition_writes(f)
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
        if !self.fired.replace(true) {
            set_stamp(&self.path, self.stamp.as_deref());
            self.raced.replace(Some(bytes(&self.path)));
        }
        self.inner.transaction(f)
    }
}

/// Shares one [`Racing`] engine so the property can read what it saw.
struct Shared(std::rc::Rc<Racing>);

impl SqlEngine for Shared {
    fn read_isolation(&self) -> ReadIsolation {
        self.0.read_isolation()
    }
    fn definition_writes<T>(
        &self,
        f: impl FnOnce() -> Result<T, CellError>,
    ) -> Result<T, CellError> {
        self.0.definition_writes(f)
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

fn config(cases: u32) -> ProptestConfig {
    ProptestConfig {
        cases,
        failure_persistence: None,
        ..ProptestConfig::default()
    }
}

proptest! {
    #![proptest_config(config(256))]

    /// M1: a cell at `cell` against the window `floor..=storage`.
    #[test]
    fn m1_admission_orders_any_version_against_any_window(
        floor in 1_u32..20,
        span in 0_u32..20,
        cell in 0_u32..60,
        writer in "[a-z0-9-]{0,12}",
    ) {
        let engine = EngineVersion { storage: floor + span, upgrades_from: floor };
        let stored = StoredVersion::Stamped(stamp_text(STAMP_FORMAT, &cell.to_string(), &writer));
        let outcome = engine_release::admit(&stored, engine);
        match outcome {
            Err(CellError::EngineUnknownVersion { .. }) => prop_assert_eq!(cell, 0),
            Err(CellError::EngineDowngrade { cell: found, writer: named, engine: storage }) => {
                prop_assert!(cell > engine.storage);
                prop_assert_eq!((found, storage), (cell, engine.storage));
                prop_assert_eq!(named, writer);
            }
            Err(CellError::EngineUpgradeUnsupported { cell: found, .. }) => {
                prop_assert!(cell != 0 && cell < floor);
                prop_assert_eq!(found, cell);
            }
            Ok(Admission::Current) => prop_assert_eq!(cell, engine.storage),
            Ok(Admission::Upgrade { from }) => {
                prop_assert!(floor <= cell && cell < engine.storage);
                prop_assert_eq!(from, cell);
            }
            other => prop_assert!(false, "unexpected admission {:?}", other),
        }
        // An unstamped cell is storage version 1 in every window: the same
        // admission or refusal, reporting the same versions.
        let unstamped = engine_release::admit(&StoredVersion::Unstamped, engine);
        let stamped_v1 = engine_release::admit(
            &StoredVersion::Stamped(stamp_text(STAMP_FORMAT, "1", "x")),
            engine,
        );
        prop_assert_eq!(verdict(&unstamped), verdict(&stamped_v1));
    }

    /// M2: any text that is not a version this engine can order refuses as
    /// unknown, and the refusal shows at most 128 characters of it.
    #[test]
    fn m2_an_unreadable_stamp_is_an_unknown_version(text in ".{0,300}") {
        let readable = serde_json::from_str::<EngineStamp>(&text)
            .is_ok_and(|stamp| stamp.format == STAMP_FORMAT && stamp.storage > 0);
        let outcome = engine_release::admit(&StoredVersion::Stamped(text), engine_release::THIS_ENGINE);
        if !readable {
            match outcome {
                Err(CellError::EngineUnknownVersion { found }) => {
                    prop_assert!(found.chars().count() <= 128);
                }
                other => prop_assert!(false, "unreadable stamp admitted as {:?}", other),
            }
        }
    }
}

proptest! {
    #![proptest_config(config(48))]

    /// M3: a cell this engine refuses is left exactly as it was found.
    #[test]
    fn m3_a_refused_open_leaves_the_file_byte_identical(
        stamp in refusing(),
        file_mode in journal(),
        upgraded_first in any::<bool>(),
        how in open_path(),
    ) {
        let dir = tempfile::tempdir().expect("directory");
        let path = load(dir.path(), &image_v1());
        if upgraded_first {
            drop(CellStore::open(&path, engine_fixture::model(), StoreOptions::default()).expect("upgrade"));
        }
        set_journal_mode(&path, file_mode);
        set_stamp(&path, Some(&stamp.text()));
        let mode_before = journal_mode(&path);
        // The file and its WAL, read immediately around the open: no SQLite
        // probe between the two reads.
        let (before, wal_before) = (bytes(&path), wal_bytes(&path));
        let refusal = try_open(&path, how);
        let (after, wal_after) = (bytes(&path), wal_bytes(&path));
        prop_assert!(refusal.is_some(), "{:?} opened a cell stamped {:?}", how, stamp);
        let refusal = refusal.expect("refused");
        prop_assert!(stamp.matches(&refusal), "{:?}: unexpected refusal {}", how, refusal);
        prop_assert!(after == before, "{:?} changed the bytes of a refused cell", how);
        // SQLite may create (or remove) an empty WAL for a reader. Once either
        // read holds a frame, the WAL must be byte-identical across the open.
        // Two reads see only the final state: a write the open undoes before
        // it returns is outside what this property can observe.
        let frames = |wal: &Option<Vec<u8>>| wal.as_deref().map_or(0, wal_frames);
        prop_assert!(
            (frames(&wal_before) == 0 && frames(&wal_after) == 0) || wal_after == wal_before,
            "{:?} changed the WAL's frames", how
        );
        prop_assert_eq!(journal_mode(&path), mode_before, "{:?} changed the journal mode", how);
    }

    /// M4: a v1 cell that fails verification refuses, unstamped and
    /// byte-identical, whatever journal mode the open asks for (#1525).
    #[test]
    fn m4_a_v1_cell_that_fails_verification_is_untouched(
        corruption in corruption(),
        wal in any::<bool>(),
    ) {
        let dir = tempfile::tempdir().expect("directory");
        let path = load(dir.path(), &corrupt(image_v1(), &corruption));
        let before = bytes(&path);
        let opened = CellStore::open(&path, engine_fixture::model(), options(mode_of(wal)));
        let Err(refusal) = opened else {
            return Err(TestCaseError::fail(format!("a corrupt v1 cell opened: {corruption:?}")));
        };
        let expected = match corruption {
            Corruption::EventText(..) | Corruption::EventHash(..) => {
                matches!(refusal, CellError::LedgerCorrupt { .. })
            }
            Corruption::PinPastHead(_) => matches!(
                &refusal,
                CellError::Encoding(reason) if reason.contains("past the store's head")
            ),
        };
        prop_assert!(expected, "{:?}: unexpected refusal {}", corruption, refusal);
        prop_assert!(bytes(&path) == before, "a refused upgrade changed the file: {:?}", corruption);
        prop_assert_eq!(
            engine_release::stored_version(&raw(&path)).expect("version"),
            StoredVersion::Unstamped
        );
    }

    /// M5: an upgrade keeps ledger and receipts, stamps this engine, and any
    /// later sequence of opens leaves both unchanged.
    #[test]
    fn m5_upgrades_keep_evidence_and_later_opens_write_none(
        modes in prop::collection::vec(any::<bool>(), 1..5),
    ) {
        let dir = tempfile::tempdir().expect("directory");
        let path = load(dir.path(), &image_v1());
        let evidence = ledger_and_receipts(&engine_fixture::image_of(&path));
        let mut after_first = None;
        for wal in modes {
            drop(CellStore::open(&path, engine_fixture::model(), options(mode_of(wal))).expect("open"));
            let image = engine_fixture::image_of(&path);
            prop_assert_eq!(&ledger_and_receipts(&image), &evidence);
            let tables = image["tables"].clone();
            match &after_first {
                None => after_first = Some(tables),
                Some(first) => prop_assert_eq!(&tables, first, "a current open wrote rows"),
            }
        }
        prop_assert_eq!(engine_release::admission(&raw(&path)).expect("admission"), Admission::Current);
        let StoredVersion::Stamped(stamp) = engine_release::stored_version(&raw(&path)).expect("version") else {
            panic!("an upgraded cell is stamped");
        };
        prop_assert_eq!(serde_json::from_str::<EngineStamp>(&stamp).expect("stamp"), EngineStamp::current());
    }

    /// M6: provisioning races a second opener that changes the cell's
    /// stamp just before provisioning's first transaction. A stamp this
    /// engine cannot serve refuses with nothing written after the race; a
    /// racer that removes the stamp (storage version 1) leaves a cell that
    /// provisioning upgrades and stamps inside that transaction.
    #[test]
    fn m6_provisioning_admits_inside_its_transaction(
        start_unstamped in any::<bool>(),
        stamp in prop::option::of(refusing()),
    ) {
        let dir = tempfile::tempdir().expect("directory");
        let path = dir.path().join("tenant.sqlite");
        drop(provision(NativeEngine::open(&path, StoreOptions::default()).expect("engine")).expect("first provisioning"));
        if start_unstamped {
            set_stamp(&path, None);
        }
        let racing = std::rc::Rc::new(Racing {
            inner: NativeEngine::open(&path, StoreOptions::default()).expect("engine"),
            path: path.clone(),
            stamp: stamp.as_ref().map(Refusing::text),
            fired: Cell::new(false),
            raced: RefCell::default(),
        });
        let outcome = provision(Shared(std::rc::Rc::clone(&racing)));
        let raced = racing.raced.borrow().clone();
        prop_assert!(raced.is_some(), "provisioning ran no transaction");
        match (&stamp, outcome) {
            (Some(stamp), Err(refusal)) => {
                prop_assert!(stamp.matches(&refusal), "unexpected refusal {}", refusal);
                prop_assert!(bytes(&path) == raced.expect("raced"), "provisioning wrote after the race");
            }
            (Some(stamp), Ok(_)) => prop_assert!(false, "provisioning served a cell restamped {:?}", stamp),
            (None, Ok(store)) => {
                drop(store);
                prop_assert_eq!(engine_release::admission(&raw(&path)).expect("admission"), Admission::Current);
            }
            (None, Err(refusal)) => prop_assert!(false, "an unstamped cell refused: {}", refusal),
        }
    }

    /// M7: a fork refuses a copy that is not at this engine's storage
    /// version (newer, unreadable, or never migrated), and leaves the
    /// source file as it was.
    #[test]
    fn m7_a_fork_refuses_a_copy_not_at_this_version(
        stamp in prop::option::of(refusing()),
    ) {
        let dir = tempfile::tempdir().expect("directory");
        let path = load(dir.path(), &image_v1());
        let store = CellStore::open(&path, engine_fixture::model(), StoreOptions::default()).expect("upgrade");
        let head = store.verified_head().expect("head");
        set_stamp(&path, stamp.as_ref().map(Refusing::text).as_deref());
        let before = bytes(&path);
        let fork = store.fork("what-if", &head);
        prop_assert!(fork.is_err(), "forked a copy stamped {:?}", stamp);
        match (&stamp, &fork) {
            (Some(stamp), Err(refusal)) => {
                prop_assert!(stamp.matches(refusal), "unexpected refusal {}", refusal);
            }
            // An unstamped copy is storage version 1, never migrated.
            (None, Err(refusal)) => prop_assert!(
                matches!(
                    refusal,
                    CellError::EngineUpgradeUnsupported { cell: 1, engine, .. }
                        if *engine == STORAGE_VERSION
                ),
                "unexpected refusal {}", refusal
            ),
            (_, Ok(_)) => {}
        }
        prop_assert!(bytes(&path) == before, "the fork changed its source");
    }

    /// M8: the export records of commits this engine makes name its storage
    /// version; the records the storage version 1 engine wrote name none.
    #[test]
    fn m8_export_records_name_the_storage_version_that_wrote_them(
        quantities in prop::collection::vec(0_i64..1_000, 1..4),
    ) {
        let dir = tempfile::tempdir().expect("directory");
        let path = load(dir.path(), &image_v1());
        let mut store = CellStore::open(&path, engine_fixture::model(), StoreOptions::default()).expect("upgrade");
        let template: CommitBatch = engine_fixture::batches()[1].clone();
        for (index, qty) in quantities.iter().enumerate() {
            let mut next = template.clone();
            next.idempotency_key = Some(format!("mined-{index}"));
            next.logical_time = 10_000 + u64::try_from(index).expect("index");
            if let Some(norn_cell::FactWriteRow::Put { values, .. }) = next.writes.first_mut() {
                values.insert("qty".to_owned(), norn_ir::RuntimeValue::from(*qty));
            }
            store.commit(&next).expect("commit");
        }
        let count = store.export_commit_count().expect("count");
        prop_assert_eq!(count, 3 + u64::try_from(quantities.len()).expect("len"));
        for seq in 1..=count {
            let record = store.export_record(seq).expect("journal").expect("record");
            let version = serde_json::to_value(&record).expect("record json")["storageVersion"].clone();
            if seq <= 3 {
                prop_assert_eq!(version, Value::Null, "record {} of the v1 engine", seq);
            } else {
                prop_assert_eq!(version, Value::from(STORAGE_VERSION), "record {} of this engine", seq);
            }
        }
    }
}
