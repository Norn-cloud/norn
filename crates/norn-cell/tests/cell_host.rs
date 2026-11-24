#![cfg(feature = "native")]
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    missing_docs,
    reason = "integration-test crate"
)]

//! T4-CELL-2 (#2676): the native `CellHost` test driver over rusqlite.
//!
//! - The host-agnostic `CellHost` laws (`norn_testkit::cell_host`, T4-CELL-1)
//!   hold for [`SqliteCell`] over an in-memory store.
//! - Under injected crashes and I/O errors ([`FaultEngine`], law 6), commits
//!   and performs stay atomic and the reopened file refines the model; a
//!   mutant engine that commits a faulted transaction is caught.
//! - The same laws hold natively on the Durable Object transaction glue
//!   (`DoEngine` over the `transactionSync` emulator, DO SQL subset), with
//!   and without faults; mutant glue and storage are caught.
//! - Deterministic fixtures: commit and enqueue are atomic (a refused commit
//!   writes nothing); a timer is delivered when due, consumed once, and a
//!   stale incarnation is refused; a performed result is recorded once and
//!   survives a reopen without re-executing.
//! - Reopening the file recovers the ledger hash chain identically, and the
//!   same scenario run twice produces byte-identical stores.
//! - Forks (Core 08, FORK-PIN-1, WORLD-ISOLATE-1): WORLD-FORK, and the
//!   FORK-NONINTERFERE property against main run alone.

mod common;

use std::cell::Cell as FlagCell;
use std::fmt::Debug;
use std::path::{Path, PathBuf};

use norn_cell::engine::{
    DefaultEngine, DoEngine, DoStorage, DoStorageEmulator, EngineFault, EngineFaultKind,
    FaultEngine, FaultSwitch, MAX_PARAMETERS, MAX_SAFE_INTEGER, MAX_STATEMENT_BYTES, NativeEngine,
    SqlEngine, SqlValue, Thrown, Transact, validate_bindings, validate_value,
};
use norn_cell::{CellError, CellStore, JournalMode, Model, PerformClaim, SqliteCell, StoreOptions};
use norn_kernel::cell_host::JsonObject;
use norn_kernel::{
    CellCommit, CellHost, Command, CommandId, CommitError, CommitReceipt, Consumed, DueTimer,
    Effect, FactKey, FactMutation, FactState, FactVersion, FactWrite, FreshnessPolicy, Generation,
    IdempotencyKey, LedgerSeq, LinkKey, LinkMutation, LinkReadExpectation, LinkState, LinkWrite,
    LogicalTime, Observed, OwnedRowsCommitProof, Performed, PredicateObject, PredicateQuery,
    PredicateReadExpectation, PredicateReadMember, Timer, TimerId,
};
use norn_testkit::Settings;
use norn_testkit::cell_host::{
    CELL_FAULT_LABELS, CELL_HOST_LABELS, CELL_NETWORK_LABELS, CellOp, Delivery, Dispatching,
    FaultKind, FaultPlan, LawCommand, LawEvent, LawInput, LedgerEntry, Route, SimulatedCell,
    check_cell_host, check_link_override, law_command, law_key, law_responder, law_values,
    override_object_types, run_cell_host, run_cell_host_sequence,
};
use proptest::prelude::*;
use serde::{Deserialize, Serialize};
use serde_json::json;

// ------------------------------------------------------------------ laws

/// `LawInput` with serde, so the SQLite driver can store it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum WireInput {
    Timer(u8),
    Result(u8),
    Observed(u8),
}

impl From<&LawInput> for WireInput {
    fn from(input: &LawInput) -> Self {
        match input {
            LawInput::Timer(value) => Self::Timer(*value),
            LawInput::Result(value) => Self::Result(*value),
            LawInput::Observed(value) => Self::Observed(*value),
        }
    }
}

impl From<WireInput> for LawInput {
    fn from(input: WireInput) -> Self {
        match input {
            WireInput::Timer(value) => Self::Timer(value),
            WireInput::Result(value) => Self::Result(value),
            WireInput::Observed(value) => Self::Observed(value),
        }
    }
}

fn law_timer(timer: Timer<WireInput>) -> Timer<LawInput> {
    Timer {
        id: timer.id,
        due: timer.due,
        payload: timer.payload.into(),
    }
}

fn law_due(due: DueTimer<WireInput>) -> DueTimer<LawInput> {
    DueTimer {
        timer: law_timer(due.timer),
        incarnation: due.incarnation,
    }
}

/// The law types (`norn_testkit::cell_host::law_object_types`) as a store
/// model: `law.Thing` with its `v` property and its `rel` and `owner` links,
/// and `law.Child`.
fn law_model() -> Model {
    common::model_of(&[
        serde_json::from_value(json!({
            "name": "law.Thing",
            "properties": [{ "id": "v", "type": { "kind": "integer" } }],
            "links": [
                { "id": "rel", "target": "law.Thing", "cardinality": { "to": "many", "from": "many" } },
                { "id": "owner", "target": "law.Thing", "cardinality": { "to": "one", "from": "many" } }
            ]
        }))
        .expect("decodes"),
        serde_json::from_value(json!({ "name": "law.Child", "extends": "law.Thing" }))
            .expect("decodes"),
    ])
}

type LawCell<S> = SqliteCell<LawEvent, LawCommand, WireInput, S>;

fn law_cell_responder(command: &Command<LawCommand>) -> WireInput {
    WireInput::from(&law_responder(command))
}

/// A directory for a simulator file store: RAM-backed (`/dev/shm`) when it
/// exists and is writable, else the normal temporary directory. Simulator
/// stores are opened without fsync ([`NativeEngine::open_unsynced`]): their
/// crashes are injected (a transaction that never commits, a dropped
/// connection, a reopen from the same file), and disk durability under power
/// loss is out of scope here; it belongs to the nightly differential on real
/// runtimes (D79 D step 5).
fn simulator_dir() -> tempfile::TempDir {
    let shm = Path::new("/dev/shm");
    shm.is_dir()
        .then(|| tempfile::tempdir_in(shm).ok())
        .flatten()
        .unwrap_or_else(|| tempfile::tempdir().expect("tempdir"))
}

/// An engine the law driver opens. `faults` is the driver's fault schedule,
/// shared across reopens; only fault-injecting engines read it.
trait LawEngine: SqlEngine + Debug + Sized + 'static {
    /// Whether the engine injects the faults armed on `faults`.
    const FAULTS: bool = false;
    /// Whether the engine's storage commits itself and can fail that commit
    /// on an armed commit fault (semantics 6).
    const COMMIT_FAULTS: bool = false;
    fn open_file(
        path: &Path,
        options: StoreOptions,
        faults: &FaultSwitch,
    ) -> Result<Self, CellError>;
    fn open_memory(faults: &FaultSwitch) -> Result<Self, CellError>;
}

impl LawEngine for NativeEngine {
    fn open_file(
        path: &Path,
        options: StoreOptions,
        _faults: &FaultSwitch,
    ) -> Result<Self, CellError> {
        NativeEngine::open_unsynced(path, options)
    }

    fn open_memory(_faults: &FaultSwitch) -> Result<Self, CellError> {
        NativeEngine::open_in_memory()
    }
}

impl<S: LawEngine> LawEngine for FaultEngine<S> {
    const FAULTS: bool = true;
    const COMMIT_FAULTS: bool = S::COMMIT_FAULTS;

    fn open_file(
        path: &Path,
        options: StoreOptions,
        faults: &FaultSwitch,
    ) -> Result<Self, CellError> {
        S::open_file(path, options, faults).map(|inner| Self::new(inner, faults.clone()))
    }

    fn open_memory(faults: &FaultSwitch) -> Result<Self, CellError> {
        S::open_memory(faults).map(|inner| Self::new(inner, faults.clone()))
    }
}

/// A mutant engine for the crash law's self-test: once the armed fault has
/// fired, it commits the transaction anyway, so the writes before the fault
/// (or, at the commit point, all of them) survive the crash.
#[derive(Debug)]
struct CommitsAfterFault<S> {
    inner: S,
    faults: FaultSwitch,
}

impl<S: LawEngine> LawEngine for CommitsAfterFault<S> {
    fn open_file(
        path: &Path,
        options: StoreOptions,
        faults: &FaultSwitch,
    ) -> Result<Self, CellError> {
        S::open_file(path, options, faults).map(|inner| Self {
            inner,
            faults: faults.clone(),
        })
    }

    fn open_memory(faults: &FaultSwitch) -> Result<Self, CellError> {
        S::open_memory(faults).map(|inner| Self {
            inner,
            faults: faults.clone(),
        })
    }
}

impl<S: SqlEngine> SqlEngine for CommitsAfterFault<S> {
    fn read_isolation(&self) -> norn_cell::engine::ReadIsolation {
        self.inner.read_isolation()
    }

    fn read_cache_revision(&self) -> Result<Option<(u64, u64)>, CellError> {
        self.inner.read_cache_revision()
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
        self.inner
            .transaction(|| {
                let result = f();
                if result.is_err() && self.faults.fired() {
                    // The bug: commit what the faulted transaction wrote.
                    Ok(result)
                } else {
                    result.map(Ok)
                }
            })
            .and_then(|result| result)
    }
}

/// SQL subset available to a DO SQLite host. It delegates execution to the
/// native engine while rejecting native-only controls and reserved names.
#[derive(Debug)]
struct RestrictedEngine {
    native: NativeEngine,
    in_transaction: FlagCell<bool>,
}

const MAX_RESULT_ROW_BYTES: usize = 2_097_152;

impl RestrictedEngine {
    fn new(native: NativeEngine) -> Self {
        Self {
            native,
            in_transaction: FlagCell::new(false),
        }
    }
}

impl LawEngine for RestrictedEngine {
    fn open_file(
        path: &Path,
        options: StoreOptions,
        _faults: &FaultSwitch,
    ) -> Result<Self, CellError> {
        NativeEngine::open_unsynced(path, options).map(Self::new)
    }

    fn open_memory(_faults: &FaultSwitch) -> Result<Self, CellError> {
        NativeEngine::open_in_memory().map(Self::new)
    }
}

impl SqlEngine for RestrictedEngine {
    fn read_isolation(&self) -> norn_cell::engine::ReadIsolation {
        self.native.read_isolation()
    }
    fn definition_writes<T>(
        &self,
        f: impl FnOnce() -> Result<T, CellError>,
    ) -> Result<T, CellError> {
        self.native.definition_writes(f)
    }

    fn execute(&self, sql: &str, params: &[SqlValue]) -> Result<(), CellError> {
        validate_do_sql(sql, params)?;
        self.native.execute(sql, params)
    }

    fn query(&self, sql: &str, params: &[SqlValue]) -> Result<Vec<Vec<SqlValue>>, CellError> {
        validate_do_sql(sql, params)?;
        validate_do_rows(self.native.query(sql, params)?)
    }

    fn execute_batch(&self, sql: &str) -> Result<(), CellError> {
        validate_do_sql(sql, &[])?;
        self.native.execute_batch(sql)
    }

    fn transaction<T, X: From<CellError>>(&self, f: impl FnOnce() -> Result<T, X>) -> Result<T, X> {
        struct Reset<'a>(&'a FlagCell<bool>);
        impl Drop for Reset<'_> {
            fn drop(&mut self) {
                self.0.set(false);
            }
        }
        if self.in_transaction.replace(true) {
            return Err(CellError::Engine("nested transaction refused".to_owned()).into());
        }
        let reset = Reset(&self.in_transaction);
        let result = self.native.transaction(f);
        drop(reset);
        result
    }
}

/// Result rows a DO SQLite host can return: exact integers, bounded row bytes.
fn validate_do_rows(rows: Vec<Vec<SqlValue>>) -> Result<Vec<Vec<SqlValue>>, CellError> {
    for row in &rows {
        let bytes = row.iter().try_fold(0_usize, |total, value| {
            validate_value(value)?;
            let size = match value {
                SqlValue::Null => 0,
                SqlValue::Integer(_) | SqlValue::Real(_) => 8,
                SqlValue::Text(text) => text.len(),
                SqlValue::Blob(blob) => blob.len(),
            };
            total
                .checked_add(size)
                .ok_or(CellError::OutOfRange("SQL result row bytes"))
        })?;
        if bytes > MAX_RESULT_ROW_BYTES {
            return Err(CellError::OutOfRange("SQL result row bytes"));
        }
    }
    Ok(rows)
}

/// The DO SQL subset over an engine with its own transaction glue: SQL is
/// validated as by [`RestrictedEngine`], transactions are the engine's own.
#[derive(Debug)]
struct DoSubset<S>(S);

impl<S: LawEngine> LawEngine for DoSubset<S> {
    const COMMIT_FAULTS: bool = S::COMMIT_FAULTS;

    fn open_file(
        path: &Path,
        options: StoreOptions,
        faults: &FaultSwitch,
    ) -> Result<Self, CellError> {
        S::open_file(path, options, faults).map(Self)
    }

    fn open_memory(faults: &FaultSwitch) -> Result<Self, CellError> {
        S::open_memory(faults).map(Self)
    }
}

impl<S: SqlEngine> SqlEngine for DoSubset<S> {
    fn platform_table_globs(&self) -> &'static [&'static str] {
        self.0.platform_table_globs()
    }

    fn read_isolation(&self) -> norn_cell::engine::ReadIsolation {
        self.0.read_isolation()
    }

    fn read_cache_revision(&self) -> Result<Option<(u64, u64)>, CellError> {
        self.0.read_cache_revision()
    }

    fn definition_writes<T>(
        &self,
        f: impl FnOnce() -> Result<T, CellError>,
    ) -> Result<T, CellError> {
        self.0.definition_writes(f)
    }

    fn execute(&self, sql: &str, params: &[SqlValue]) -> Result<(), CellError> {
        validate_do_sql(sql, params)?;
        self.0.execute(sql, params)
    }

    fn query(&self, sql: &str, params: &[SqlValue]) -> Result<Vec<Vec<SqlValue>>, CellError> {
        validate_do_sql(sql, params)?;
        validate_do_rows(self.0.query(sql, params)?)
    }

    fn execute_batch(&self, sql: &str) -> Result<(), CellError> {
        validate_do_sql(sql, &[])?;
        self.0.execute_batch(sql)
    }

    fn transaction<T, X: From<CellError>>(&self, f: impl FnOnce() -> Result<T, X>) -> Result<T, X> {
        self.0.transaction(f)
    }
}

/// A Durable Object storage the law driver opens over a native connection.
/// Its commits fail on the driver's armed commit faults.
trait OpenDo: DoStorage + Debug + Sized + 'static {
    fn over(native: NativeEngine, faults: &FaultSwitch) -> Self;
}

impl OpenDo for DoStorageEmulator {
    fn over(native: NativeEngine, faults: &FaultSwitch) -> Self {
        Self::new(native).with_faults(faults.clone())
    }
}

impl<S: OpenDo> LawEngine for DoEngine<S> {
    const COMMIT_FAULTS: bool = true;

    fn open_file(
        path: &Path,
        options: StoreOptions,
        faults: &FaultSwitch,
    ) -> Result<Self, CellError> {
        NativeEngine::open_unsynced(path, options).map(|native| Self::new(S::over(native, faults)))
    }

    fn open_memory(faults: &FaultSwitch) -> Result<Self, CellError> {
        NativeEngine::open_in_memory().map(|native| Self::new(S::over(native, faults)))
    }
}

/// The Durable Object engine natively: the Workers transaction glue over the
/// `transactionSync` emulator, restricted to the DO SQL subset.
type DoLawEngine<S = DoStorageEmulator> = DoSubset<DoEngine<S>>;

/// Mutant glue input: it never recognises its own rollback marker, so every
/// rolled-back refusal is reported as an engine error.
#[derive(Debug)]
struct MisreadsRollback(DoStorageEmulator);

/// Mutant storage: it ignores the rollback marker, commits what the callback
/// wrote, and only then rethrows.
#[derive(Debug)]
struct CommitsOnRollback(DoStorageEmulator);

impl OpenDo for MisreadsRollback {
    fn over(native: NativeEngine, faults: &FaultSwitch) -> Self {
        Self(DoStorageEmulator::over(native, faults))
    }
}

impl OpenDo for CommitsOnRollback {
    fn over(native: NativeEngine, faults: &FaultSwitch) -> Self {
        Self(DoStorageEmulator::over(native, faults))
    }
}

/// Mutant storage: when its own commit fails it reports success anyway, so
/// the caller believes a rolled-back write committed (semantics 6).
#[derive(Debug)]
struct HidesFailedCommit(DoStorageEmulator);

impl OpenDo for HidesFailedCommit {
    fn over(native: NativeEngine, faults: &FaultSwitch) -> Self {
        Self(DoStorageEmulator::over(native, faults))
    }
}

impl Transact for HidesFailedCommit {
    type Thrown = Thrown;

    fn transaction_sync<F: FnMut() -> Result<(), Thrown>>(
        &self,
        callback: &mut F,
    ) -> Result<(), Thrown> {
        match self.0.transaction_sync(callback) {
            // The bug: the storage's own failure is reported as a commit.
            Err(Thrown::Storage(_)) => Ok(()),
            other => other,
        }
    }

    fn rollback_marker(&self) -> Thrown {
        self.0.rollback_marker()
    }

    fn is_rollback_marker(&self, thrown: &Thrown) -> bool {
        self.0.is_rollback_marker(thrown)
    }

    fn callback_reused(&self) -> Thrown {
        self.0.callback_reused()
    }

    fn engine_error(&self, thrown: &Thrown) -> CellError {
        self.0.engine_error(thrown)
    }
}

impl DoStorage for HidesFailedCommit {
    fn exec(&self, sql: &str, params: &[SqlValue]) -> Result<Vec<Vec<SqlValue>>, CellError> {
        self.0.exec(sql, params)
    }

    fn exec_batch(&self, sql: &str) -> Result<(), CellError> {
        self.0.exec_batch(sql)
    }

    fn definition_writes<T>(
        &self,
        f: impl FnOnce() -> Result<T, CellError>,
    ) -> Result<T, CellError> {
        self.0.definition_writes(f)
    }
}

impl Transact for MisreadsRollback {
    type Thrown = Thrown;

    fn transaction_sync<F: FnMut() -> Result<(), Thrown>>(
        &self,
        callback: &mut F,
    ) -> Result<(), Thrown> {
        self.0.transaction_sync(callback)
    }

    fn rollback_marker(&self) -> Thrown {
        self.0.rollback_marker()
    }

    fn is_rollback_marker(&self, _thrown: &Thrown) -> bool {
        // The bug.
        false
    }

    fn callback_reused(&self) -> Thrown {
        self.0.callback_reused()
    }

    fn engine_error(&self, thrown: &Thrown) -> CellError {
        self.0.engine_error(thrown)
    }
}

impl Transact for CommitsOnRollback {
    type Thrown = Thrown;

    fn transaction_sync<F: FnMut() -> Result<(), Thrown>>(
        &self,
        callback: &mut F,
    ) -> Result<(), Thrown> {
        let mut swallowed = None;
        // The bug: a throw does not stop the commit.
        let mut commit_anyway = || {
            callback().or_else(|thrown| {
                swallowed = Some(thrown);
                Ok(())
            })
        };
        let result = self.0.transaction_sync(&mut commit_anyway);
        swallowed.map_or(result, Err)
    }

    fn rollback_marker(&self) -> Thrown {
        self.0.rollback_marker()
    }

    fn is_rollback_marker(&self, thrown: &Thrown) -> bool {
        self.0.is_rollback_marker(thrown)
    }

    fn callback_reused(&self) -> Thrown {
        self.0.callback_reused()
    }

    fn engine_error(&self, thrown: &Thrown) -> CellError {
        self.0.engine_error(thrown)
    }
}

impl DoStorage for MisreadsRollback {
    fn exec(&self, sql: &str, params: &[SqlValue]) -> Result<Vec<Vec<SqlValue>>, CellError> {
        self.0.exec(sql, params)
    }

    fn exec_batch(&self, sql: &str) -> Result<(), CellError> {
        self.0.exec_batch(sql)
    }

    fn definition_writes<T>(
        &self,
        f: impl FnOnce() -> Result<T, CellError>,
    ) -> Result<T, CellError> {
        self.0.definition_writes(f)
    }
}

impl DoStorage for CommitsOnRollback {
    fn exec(&self, sql: &str, params: &[SqlValue]) -> Result<Vec<Vec<SqlValue>>, CellError> {
        self.0.exec(sql, params)
    }

    fn exec_batch(&self, sql: &str) -> Result<(), CellError> {
        self.0.exec_batch(sql)
    }

    fn definition_writes<T>(
        &self,
        f: impl FnOnce() -> Result<T, CellError>,
    ) -> Result<T, CellError> {
        self.0.definition_writes(f)
    }
}

/// Tokens and quoted identifiers are inspected; comments and string literals
/// are skipped so they cannot accidentally hide a forbidden operation.
fn validate_do_sql(sql: &str, params: &[SqlValue]) -> Result<(), CellError> {
    const FORBIDDEN: &[&str] = &[
        "begin",
        "commit",
        "rollback",
        "savepoint",
        "release",
        "pragma",
        "attach",
        "detach",
        "vacuum",
    ];
    validate_bindings(sql, params)?;
    let tokens = sql_tokens(sql);
    for (index, token) in tokens.iter().enumerate() {
        let Some(word) = token.strip_prefix("word:") else {
            continue;
        };
        if FORBIDDEN.contains(&word) || word.starts_with("_cf_") {
            return Err(CellError::Engine(format!("restricted SQL token `{word}`")));
        }
        if matches!(word, "changes" | "last_insert_rowid")
            && tokens
                .get(index.saturating_add(1))
                .is_some_and(|next| next == "(")
        {
            return Err(CellError::Engine(format!(
                "restricted SQL function `{word}`"
            )));
        }
    }
    Ok(())
}

/// Return SQL words, quoted identifiers and punctuation, ignoring comments
/// and string literals.
#[allow(
    clippy::arithmetic_side_effects,
    reason = "the test-only SQL lexer bounds-checks every byte offset"
)]
fn sql_tokens(sql: &str) -> Vec<String> {
    let bytes = sql.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            byte if byte.is_ascii_whitespace() => index += 1,
            b'-' if bytes.get(index + 1) == Some(&b'-') => {
                index += 2;
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index += 2;
                while index + 1 < bytes.len() && !(bytes[index] == b'*' && bytes[index + 1] == b'/')
                {
                    index += 1;
                }
                index = (index + 2).min(bytes.len());
            }
            b'\'' => {
                index += 1;
                while index < bytes.len() {
                    if bytes[index] == b'\'' {
                        index += 1;
                        if bytes.get(index) == Some(&b'\'') {
                            index += 1;
                        } else {
                            break;
                        }
                    } else {
                        index += 1;
                    }
                }
            }
            quote @ (b'"' | b'`' | b'[') => {
                let close = if quote == b'[' { b']' } else { quote };
                index += 1;
                let start = index;
                while index < bytes.len() {
                    if bytes[index] == close {
                        if bytes.get(index + 1) == Some(&close) {
                            index += 2;
                            continue;
                        }
                        break;
                    }
                    index += 1;
                }
                let end = index.min(bytes.len());
                let identifier = String::from_utf8_lossy(&bytes[start..end]).to_ascii_lowercase();
                tokens.push(format!("word:{identifier}"));
                if index < bytes.len() {
                    index += 1;
                }
            }
            byte if byte.is_ascii_alphanumeric() || byte == b'_' => {
                let start = index;
                index += 1;
                while index < bytes.len()
                    && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
                {
                    index += 1;
                }
                let word = String::from_utf8_lossy(&bytes[start..index]).to_ascii_lowercase();
                tokens.push(format!("word:{word}"));
            }
            byte => {
                tokens.push(char::from(byte).to_string());
                index += 1;
            }
        }
    }
    tokens
}

fn law_cell<S: LawEngine>(engine: S) -> Result<LawCell<S>, CellError> {
    let store = CellStore::from_engine(engine, law_model())?;
    SqliteCell::new(store, "law", law_cell_responder)
}

/// The SQL driver over the law vocabulary. With `REOPEN`, the store is a file
/// closed and reopened after every operation that writes, verifying the ledger
/// (which is also how it recovers from an injected crash). With `NETWORK`,
/// the law machine dispatches commands and delivers their results through
/// an unreliable network (law 7).
struct LawSqlite<const REOPEN: bool, S: LawEngine = DefaultEngine, const NETWORK: bool = false> {
    cell: LawCell<S>,
    file: Option<(tempfile::TempDir, PathBuf)>,
    /// The fault schedule every engine of this driver shares.
    faults: FaultSwitch,
    /// Whether the fault armed for the current operation fired.
    fired: bool,
}

impl<const REOPEN: bool, S: LawEngine, const NETWORK: bool> LawSqlite<REOPEN, S, NETWORK> {
    fn after_write(&mut self) {
        // The operation is over: its fault must not reach the reopen.
        if self.faults.disarm() {
            self.fired = true;
        }
        if let Some((_, path)) = &self.file {
            let now = self.cell.now();
            self.cell = law_cell(
                S::open_file(path, StoreOptions::default(), &self.faults).expect("reopens"),
            )
            .expect("rebuilds store");
            self.cell.advance_to(now);
        }
    }
}

impl<const REOPEN: bool, S: LawEngine, const NETWORK: bool> CellHost
    for LawSqlite<REOPEN, S, NETWORK>
{
    type Event = LawEvent;
    type Command = LawCommand;
    type Input = LawInput;
    type Error = CellError;

    fn load(&self, key: &FactKey) -> Result<FactState, CellError> {
        self.cell.load(key)
    }

    fn load_link(&self, key: &LinkKey) -> Result<LinkState, CellError> {
        self.cell.load_link(key)
    }

    fn commit(
        &mut self,
        commit: CellCommit<LawEvent, LawCommand, LawInput>,
    ) -> Result<CommitReceipt, CommitError<CellError>> {
        let effects = commit
            .effects
            .into_iter()
            .map(|effect| match effect {
                Effect::Perform(command) => Effect::Perform(command),
                Effect::Schedule(timer) => Effect::Schedule(Timer {
                    id: timer.id,
                    due: timer.due,
                    payload: WireInput::from(&timer.payload),
                }),
                Effect::CancelTimer(id) => Effect::CancelTimer(id),
            })
            .collect();
        let receipt = self.cell.commit(CellCommit {
            consumes: commit.consumes,
            reads: commit.reads,
            writes: commit.writes,
            events: commit.events,
            effects,
            links: commit.links,
            link_reads: commit.link_reads,
            predicate_reads: commit.predicate_reads,
        });
        self.after_write();
        receipt
    }

    fn commit_owned_rows(
        &mut self,
        commit: CellCommit<LawEvent, LawCommand, LawInput>,
        proof: OwnedRowsCommitProof,
    ) -> Result<CommitReceipt, CommitError<CellError>> {
        let effects = commit
            .effects
            .into_iter()
            .map(|effect| match effect {
                Effect::Perform(command) => Effect::Perform(command),
                Effect::Schedule(timer) => Effect::Schedule(Timer {
                    id: timer.id,
                    due: timer.due,
                    payload: WireInput::from(&timer.payload),
                }),
                Effect::CancelTimer(id) => Effect::CancelTimer(id),
            })
            .collect();
        let receipt = self.cell.commit_owned_rows(
            CellCommit {
                consumes: commit.consumes,
                reads: commit.reads,
                writes: commit.writes,
                events: commit.events,
                effects,
                links: commit.links,
                link_reads: commit.link_reads,
                predicate_reads: commit.predicate_reads,
            },
            proof,
        );
        self.after_write();
        receipt
    }

    fn perform(&mut self) -> Result<Option<Performed<LawInput>>, CellError> {
        let performed = self.cell.perform();
        // Reopen even after a failure: a crashed engine refuses every call.
        self.after_write();
        Ok(performed?.map(|performed| Performed {
            command: performed.command,
            generation: performed.generation,
            input: performed.input.into(),
        }))
    }

    fn observe(&mut self) -> Result<Option<Observed<LawInput>>, CellError> {
        Ok(self.cell.observe()?.map(|observed| Observed {
            id: observed.id,
            provenance: observed.provenance,
            input: observed.input.into(),
        }))
    }

    fn clock(&mut self) -> Result<Option<DueTimer<LawInput>>, CellError> {
        Ok(self.cell.clock()?.map(law_due))
    }

    fn publish(&mut self, receipt: &CommitReceipt) -> Result<(), CellError> {
        self.cell.publish(receipt)
    }
}

impl<const REOPEN: bool, S: LawEngine, const NETWORK: bool> SimulatedCell
    for LawSqlite<REOPEN, S, NETWORK>
{
    const COMMITS_PERFORM_CLAIMS: bool = true;
    const NETWORK: bool = NETWORK;
    /// Only a file store survives a crash: it reopens from the file. Only a
    /// storage that commits itself can fail its commit.
    const FAULT_KINDS: &'static [FaultKind] = match (S::FAULTS, REOPEN, S::COMMIT_FAULTS) {
        (false, _, _) => &[],
        (true, false, false) => &[FaultKind::IoError],
        (true, true, false) => &[FaultKind::IoError, FaultKind::Crash],
        (true, false, true) => &[FaultKind::IoError, FaultKind::CommitFails],
        (true, true, true) => &[FaultKind::IoError, FaultKind::Crash, FaultKind::CommitFails],
    };

    fn sim_new() -> Self {
        let faults = FaultSwitch::default();
        if REOPEN {
            let dir = simulator_dir();
            let path = dir.path().join("cell.sqlite");
            let cell =
                law_cell(S::open_file(&path, StoreOptions::default(), &faults).expect("opens"))
                    .expect("opens store");
            Self {
                cell,
                file: Some((dir, path)),
                faults,
                fired: false,
            }
        } else {
            Self {
                cell: law_cell(S::open_memory(&faults).expect("opens engine"))
                    .expect("opens store"),
                file: None,
                faults,
                fired: false,
            }
        }
    }

    fn sim_arm_fault(&mut self, plan: FaultPlan) {
        self.fired = false;
        self.faults.arm(EngineFault {
            kind: match plan.kind {
                FaultKind::Crash => EngineFaultKind::Crash,
                FaultKind::IoError => EngineFaultKind::Io,
                FaultKind::CommitFails => EngineFaultKind::Commit,
            },
            transaction: u32::from(plan.transaction),
            at_statement: u32::from(plan.at_statement),
        });
    }

    fn sim_disarm_fault(&mut self) -> bool {
        let fired = self.faults.disarm();
        std::mem::take(&mut self.fired) || fired
    }

    fn sim_dispatch(&mut self) -> Result<Option<Command<LawCommand>>, String> {
        let claimed = self.cell.claim_perform();
        self.after_write();
        match claimed.map_err(|error| error.to_string())? {
            Some(PerformClaim::Fresh(command)) => Ok(Some(command)),
            Some(PerformClaim::Recorded { .. } | PerformClaim::OutcomeUnknown(_)) | None => {
                Ok(None)
            }
        }
    }

    fn sim_deliver(
        &mut self,
        command: &Command<LawCommand>,
        input: &LawInput,
    ) -> Result<bool, String> {
        let recorded = self.cell.complete_perform(command, WireInput::from(input));
        self.after_write();
        recorded
            .map(|recorded| recorded.is_some())
            .map_err(|error| error.to_string())
    }

    fn sim_advance_to(&mut self, now: LogicalTime) {
        self.cell.advance_to(now);
    }

    fn sim_inject(&mut self, observed: Observed<LawInput>) {
        self.cell
            .inject(&Observed {
                id: observed.id,
                provenance: observed.provenance,
                input: WireInput::from(&observed.input),
            })
            .expect("ingests");
        self.after_write();
    }

    fn sim_executions(&self) -> u64 {
        self.cell.executions().expect("counts")
    }

    fn sim_outbox(&self) -> Vec<Command<LawCommand>> {
        self.cell.pending_commands().expect("outbox")
    }

    fn sim_timers(&self) -> Vec<DueTimer<LawInput>> {
        self.cell
            .pending_timers()
            .expect("timers")
            .into_iter()
            .map(law_due)
            .collect()
    }

    fn sim_ledger(&self) -> Vec<LedgerEntry<LawEvent>> {
        self.cell
            .commits()
            .expect("ledger")
            .into_iter()
            .map(|commit| LedgerEntry {
                seq: commit.seq,
                consumed: commit.consumed,
                written: commit.written,
                linked: commit.linked,
                events: commit.events,
            })
            .collect()
    }
}

#[test]
fn cell_host_laws_hold_for_the_sqlite_driver() {
    let coverage = check_cell_host::<LawSqlite<false>>(&Settings::new(file!()).cases(256));
    coverage.assert_covered(CELL_HOST_LABELS);
}

#[test]
fn cell_host_laws_hold_across_reopen() {
    let coverage = check_cell_host::<LawSqlite<true>>(&Settings::new(file!()).cases(256));
    coverage.assert_covered(CELL_HOST_LABELS);
}

#[test]
fn cell_host_laws_hold_for_the_restricted_engine() {
    let coverage =
        check_cell_host::<LawSqlite<false, RestrictedEngine>>(&Settings::new(file!()).cases(256));
    coverage.assert_covered(CELL_HOST_LABELS);
}

#[test]
fn cell_host_laws_hold_across_reopen_for_the_restricted_engine() {
    let coverage =
        check_cell_host::<LawSqlite<true, RestrictedEngine>>(&Settings::new(file!()).cases(256));
    coverage.assert_covered(CELL_HOST_LABELS);
}

/// Cases for the reopening fault tests: every write reopens the file, so the
/// fixed seed runs 16 cases; `PROPTEST_CASES` raises it for a soak.
const FAULT_REOPEN_CASES: u32 = 16;

/// Law 6 (D79 D step 1): commits and performs stay atomic when a write
/// transaction crashes or hits an I/O error at any statement, and the file
/// reopens to the model's state (swarm mode, faults armed).
#[test]
fn cell_host_laws_hold_under_injected_crashes() {
    let coverage = check_cell_host::<LawSqlite<true, FaultEngine<NativeEngine>>>(
        &Settings::new(file!()).cases(FAULT_REOPEN_CASES),
    );
    coverage.assert_covered(CELL_FAULT_LABELS);
    coverage.assert_covered(&["cell:fault.crash", "cell:fault.io_error"]);
}

#[test]
fn cell_host_laws_hold_under_injected_crashes_for_the_restricted_engine() {
    let coverage = check_cell_host::<LawSqlite<true, FaultEngine<RestrictedEngine>>>(
        &Settings::new(file!()).cases(FAULT_REOPEN_CASES),
    );
    coverage.assert_covered(CELL_FAULT_LABELS);
    coverage.assert_covered(&["cell:fault.crash", "cell:fault.io_error"]);
}

/// Law 6 without reopening: after an I/O error rolls a transaction back, the
/// same connection and store keep serving the model's state.
#[test]
fn cell_host_laws_hold_under_injected_io_errors_without_reopen() {
    let coverage =
        check_cell_host::<LawSqlite<false, FaultEngine<NativeEngine>>>(&Settings::new(file!()));
    coverage.assert_covered(CELL_FAULT_LABELS);
    coverage.assert_covered(&["cell:fault.io_error"]);
}

/// The law a write surviving its rollback breaks under the suite's fixed seed: a
/// faulted perform claim that should have rolled back is delivered or executed
/// anyway. Mutant tests name the law they must break, so a harness panic or an
/// unrelated law (fault wiring, say) never passes for the mutant being caught.
const SURVIVING_ROLLBACK_LAW: &str =
    "law violated: a perform whose claim or result is lost delivered or executed";

/// Crash rollback can fail: an engine that commits a faulted transaction lets
/// a partial (or whole) write survive the crash, and the law machine finds a
/// counterexample that arms a fault; under the suite's seed the surviving
/// write breaks the perform law (`SURVIVING_ROLLBACK_LAW`). It only needs to be caught, not
/// minimised, so it takes the one shrink step proptest-state-machine needs.
#[test]
fn cell_host_mutant_committing_after_a_fault_is_caught() {
    let failure = run_cell_host::<LawSqlite<true, FaultEngine<CommitsAfterFault<NativeEngine>>>>(
        &Settings::new(file!())
            .without_persistence()
            .max_shrink_iters(1),
    )
    .expect_err("an engine that commits a faulted transaction must fail the crash law");
    assert!(
        failure
            .transitions
            .iter()
            .any(|op| matches!(op, CellOp::Faulted(_))),
        "{failure}"
    );
    assert!(
        failure.reason.starts_with(SURVIVING_ROLLBACK_LAW),
        "{failure}"
    );
}

/// The host laws on the Durable Object transaction glue, run natively
/// (`DoEngine` over the `transactionSync` emulator, DO SQL subset).
#[test]
fn cell_host_laws_hold_for_the_do_transaction_glue() {
    let coverage =
        check_cell_host::<LawSqlite<false, DoLawEngine>>(&Settings::new(file!()).cases(64));
    coverage.assert_covered(CELL_HOST_LABELS);
}

/// Law 6 on the Durable Object transaction glue: a crash or I/O error at any
/// statement or commit point inside `transactionSync`, or a failure of the
/// storage's own commit (semantics 6), rolls everything back.
#[test]
fn cell_host_laws_hold_under_injected_crashes_for_the_do_transaction_glue() {
    let coverage = check_cell_host::<LawSqlite<true, FaultEngine<DoLawEngine>>>(
        &Settings::new(file!()).cases(FAULT_REOPEN_CASES),
    );
    coverage.assert_covered(CELL_FAULT_LABELS);
    coverage.assert_covered(&[
        "cell:fault.crash",
        "cell:fault.io_error",
        "cell:fault.commit_failed",
    ]);
}

/// Semantics 6 can fail: a storage that reports a failed commit as success
/// makes a rolled-back write look committed, and law 6 catches it on a
/// commit fault.
#[test]
fn cell_host_mutant_do_storage_hiding_a_failed_commit_is_caught() {
    let failure = run_cell_host::<LawSqlite<true, FaultEngine<DoLawEngine<HidesFailedCommit>>>>(
        &Settings::new(file!())
            .without_persistence()
            .max_shrink_iters(1),
    )
    .expect_err("a storage that hides a failed commit must fail the laws");
    assert!(
        failure.reason.starts_with(SURVIVING_ROLLBACK_LAW),
        "{failure}"
    );
    assert!(
        failure.transitions.iter().any(|op| matches!(
            op,
            CellOp::Faulted(faulted) if faulted.plan.kind == FaultKind::CommitFails
        )),
        "{failure}"
    );
}

/// Glue that does not recognise its rollback marker turns every typed
/// refusal into an engine error, and the commit law catches it.
#[test]
fn cell_host_mutant_do_glue_misreading_rollback_is_caught() {
    let failure = run_cell_host::<LawSqlite<false, DoLawEngine<MisreadsRollback>>>(
        &Settings::new(file!())
            .without_persistence()
            .max_shrink_iters(1),
    )
    .expect_err("glue that misreads its rollback marker must fail the laws");
    assert!(
        failure.reason.contains("commit outcome differs"),
        "{failure}"
    );
}

/// A storage that commits despite the rollback marker lets a faulted (or
/// refused) transaction's writes survive, and the laws catch it.
#[test]
fn cell_host_mutant_do_storage_committing_on_rollback_is_caught() {
    let failure = run_cell_host::<LawSqlite<true, FaultEngine<DoLawEngine<CommitsOnRollback>>>>(
        &Settings::new(file!())
            .without_persistence()
            .max_shrink_iters(1),
    )
    .expect_err("a storage that commits on rollback must fail the laws");
    // Its own law, not a harness panic or another law, on a run that rolled back
    // a faulted transaction.
    assert!(
        failure.reason.starts_with(SURVIVING_ROLLBACK_LAW),
        "{failure}"
    );
    assert!(
        failure
            .transitions
            .iter()
            .any(|op| matches!(op, CellOp::Faulted(_))),
        "{failure}"
    );
}

/// Cases for the responder-fault tests (law 7): file stores, crashes
/// included; `PROPTEST_CASES` raises it for a soak.
const RESPONDER_CASES: u32 = 12;

/// Law 7 on the SQLite driver: results duplicated, lost, delayed and
/// reordered by the outside world, observations redelivered, with crashes,
/// I/O errors and reopens in between.
#[test]
fn cell_host_laws_hold_under_responder_faults() {
    let coverage = check_cell_host::<LawSqlite<true, FaultEngine<NativeEngine>, true>>(
        &Settings::new(file!()).cases(RESPONDER_CASES),
    );
    coverage.assert_covered(CELL_NETWORK_LABELS);
    coverage.assert_covered(&["cell:fault.crash"]);
}

/// Law 7 on the Durable Object transaction glue, with its failed commits.
#[test]
fn cell_host_laws_hold_under_responder_faults_for_the_do_transaction_glue() {
    let coverage = check_cell_host::<LawSqlite<true, FaultEngine<DoLawEngine>, true>>(
        &Settings::new(file!()).cases(RESPONDER_CASES),
    );
    coverage.assert_covered(CELL_NETWORK_LABELS);
    coverage.assert_covered(&["cell:fault.crash", "cell:fault.commit_failed"]);
}

/// A mutant behind the network: it executes a command again when a result
/// is redelivered or duplicated.
const DOUBLE_EXECUTION: u8 = 0;
/// A mutant behind the network: it records a stale-generation result as the
/// current generation's.
const STALE_RESULT: u8 = 1;

/// A deliberately broken host behind the law machine's network (law 7),
/// over an in-memory store so it fails fast.
struct ResponderMutant<const BUG: u8> {
    inner: LawSqlite<false, NativeEngine, true>,
    /// Executions the bug added.
    again: u64,
}

impl<const BUG: u8> CellHost for ResponderMutant<BUG> {
    type Event = LawEvent;
    type Command = LawCommand;
    type Input = LawInput;
    type Error = CellError;

    fn load(&self, key: &FactKey) -> Result<FactState, CellError> {
        self.inner.load(key)
    }

    fn load_link(&self, key: &LinkKey) -> Result<LinkState, CellError> {
        self.inner.load_link(key)
    }

    fn commit(
        &mut self,
        commit: CellCommit<LawEvent, LawCommand, LawInput>,
    ) -> Result<CommitReceipt, CommitError<CellError>> {
        self.inner.commit(commit)
    }

    fn commit_owned_rows(
        &mut self,
        commit: CellCommit<LawEvent, LawCommand, LawInput>,
        proof: OwnedRowsCommitProof,
    ) -> Result<CommitReceipt, CommitError<CellError>> {
        self.inner.commit_owned_rows(commit, proof)
    }

    fn perform(&mut self) -> Result<Option<Performed<LawInput>>, CellError> {
        self.inner.perform()
    }

    fn observe(&mut self) -> Result<Option<Observed<LawInput>>, CellError> {
        self.inner.observe()
    }

    fn clock(&mut self) -> Result<Option<DueTimer<LawInput>>, CellError> {
        self.inner.clock()
    }

    fn publish(&mut self, receipt: &CommitReceipt) -> Result<(), CellError> {
        self.inner.publish(receipt)
    }
}

impl<const BUG: u8> SimulatedCell for ResponderMutant<BUG> {
    const COMMITS_PERFORM_CLAIMS: bool = true;
    const NETWORK: bool = true;

    fn sim_new() -> Self {
        Self {
            inner: LawSqlite::sim_new(),
            again: 0,
        }
    }

    fn sim_dispatch(&mut self) -> Result<Option<Command<LawCommand>>, String> {
        self.inner.sim_dispatch()
    }

    fn sim_deliver(
        &mut self,
        command: &Command<LawCommand>,
        input: &LawInput,
    ) -> Result<bool, String> {
        if self.inner.sim_deliver(command, input)? {
            return Ok(true);
        }
        if BUG == DOUBLE_EXECUTION {
            // The bug: a redelivered or duplicated result executes the
            // command again.
            self.again = self.again.saturating_add(1);
            return Ok(false);
        }
        // The bug: a stale-generation result is recorded as the current
        // generation's.
        match self.inner.sim_outbox().into_iter().next() {
            Some(head) if head.id == command.id && head.generation != command.generation => {
                self.inner.sim_deliver(&head, input)
            }
            Some(_) | None => Ok(false),
        }
    }

    fn sim_advance_to(&mut self, now: LogicalTime) {
        self.inner.sim_advance_to(now);
    }

    fn sim_inject(&mut self, observed: Observed<LawInput>) {
        self.inner.sim_inject(observed);
    }

    fn sim_executions(&self) -> u64 {
        self.inner.sim_executions().saturating_add(self.again)
    }

    fn sim_outbox(&self) -> Vec<Command<LawCommand>> {
        self.inner.sim_outbox()
    }

    fn sim_timers(&self) -> Vec<DueTimer<LawInput>> {
        self.inner.sim_timers()
    }

    fn sim_ledger(&self) -> Vec<LedgerEntry<LawEvent>> {
        self.inner.sim_ledger()
    }
}

/// Law 7 can fail: a host that executes a command again on a duplicated or
/// redelivered result is caught.
#[test]
fn cell_host_mutant_executing_a_redelivered_result_again_is_caught() {
    let failure = run_cell_host::<ResponderMutant<DOUBLE_EXECUTION>>(
        &Settings::new(file!())
            .without_persistence()
            .max_shrink_iters(1),
    )
    .expect_err("a host that executes a command twice must fail the laws");
    // Law 5 (refinement): the second execution leaves host state the model never reaches.
    assert!(
        failure
            .reason
            .contains("host state does not refine the cell model"),
        "{failure}"
    );
    assert!(
        failure
            .transitions
            .iter()
            .any(|op| matches!(op, CellOp::Deliver(_) | CellOp::Exchange(_))),
        "{failure}"
    );
}

/// Law 7 can fail: a host that records a stale-generation result is caught
/// on the fixed interleaving that makes one: enqueue `c0`, dispatch it,
/// re-issue it at generation 1 and dispatch again, then deliver the
/// generation-0 result. The healthy driver passes the same sequence.
#[test]
fn cell_host_mutant_recording_a_stale_result_is_caught() {
    let enqueue = |generation, payload| CellCommit {
        effects: vec![Effect::Perform(law_command(0, generation, payload))],
        ..CellCommit::empty()
    };
    let stale = law_command(0, 0, 7);
    let transitions = vec![
        CellOp::Commit(enqueue(0, 7)),
        CellOp::Dispatch(Box::new(Dispatching {
            prepare: None,
            reissue: Some(enqueue(1, 8)),
        })),
        CellOp::Deliver(Box::new(Delivery {
            input: law_responder(&stale),
            command: stale,
            route: Route::InOrder,
        })),
    ];
    // The healthy driver refuses the stale result on the same sequence.
    let healthy =
        run_cell_host_sequence::<LawSqlite<false, NativeEngine, true>>(transitions.clone());
    assert_eq!(healthy, Ok(()));
    let violation = run_cell_host_sequence::<ResponderMutant<STALE_RESULT>>(transitions)
        .expect_err("a host that records a stale result must fail the laws");
    assert!(
        violation.contains("law violated: a result is recorded only"),
        "{violation}"
    );
}

#[test]
fn restricted_sql_rejects_forbidden_tokens_and_reserved_identifiers() {
    for sql in [
        "BEGIN",
        "cOmMiT TRANSACTION",
        "/* safe */ rollback",
        "SAVEPOINT nested",
        "release nested",
        "PRAGMA foreign_keys",
        "ATTACH DATABASE 'x' AS x",
        "detach database x",
        "vacuum",
        "SELECT changes()",
        "SELECT last_insert_rowid ()",
        "SELECT 1; -- next statement\n PRAGMA cache_size",
        "CREATE TABLE \"_CF_private\" (id INTEGER)",
    ] {
        assert!(validate_do_sql(sql, &[]).is_err(), "accepted `{sql}`");
        let batch = format!("SELECT 1; {sql}");
        assert!(
            validate_do_sql(&batch, &[]).is_err(),
            "accepted forbidden SQL after a valid statement: `{sql}`"
        );
    }
    assert!(validate_do_sql("SELECT 'PRAGMA changes() _cf_hidden'", &[]).is_ok());
    assert!(validate_do_sql("SELECT 1 /* ATTACH _cf_hidden */", &[]).is_ok());
}

#[test]
fn restricted_engine_enforces_sql_limits_and_nested_transaction_rules() {
    let engine = RestrictedEngine::open_memory(&FaultSwitch::default()).expect("opens");
    assert!(validate_do_sql(&" ".repeat(MAX_STATEMENT_BYTES.saturating_add(1)), &[]).is_err());
    let params = vec![SqlValue::Null; MAX_PARAMETERS.saturating_add(1)];
    assert!(validate_do_sql("SELECT 1", &params).is_err());
    assert!(
        validate_do_sql(
            "SELECT ?1",
            &[SqlValue::Integer(MAX_SAFE_INTEGER.saturating_add(1))],
        )
        .is_err()
    );
    let oversized = SqlValue::from("x".repeat(MAX_RESULT_ROW_BYTES.saturating_add(1)));
    assert!(engine.query("SELECT ?1", &[oversized]).is_err());

    let nested = engine.transaction(|| {
        engine.transaction(|| Ok::<_, CellError>(()))?;
        Ok::<_, CellError>(())
    });
    assert!(nested.is_err());
}

// ------------------------------------------------------------------ fixtures

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum OrderEvent {
    Placed { order: String },
    Expired { order: String },
    Charged { order: String, ok: bool },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Charge {
    order: String,
    amount: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
enum OrderInput {
    Expire { order: String },
    Charged { order: String, ok: bool },
}

type OrderCell = SqliteCell<OrderEvent, Charge, OrderInput>;
type OrderCommit = CellCommit<OrderEvent, Charge, OrderInput>;

fn order_model() -> Model {
    common::model_of(&[serde_json::from_value(json!({
        "name": "shop.Order",
        "properties": [
            { "id": "status", "type": { "kind": "enum", "values": [
                { "value": "placed" }, { "value": "paid" }, { "value": "expired" }
            ] } },
            { "id": "total", "type": { "kind": "decimal", "scale": 2 } }
        ]
    }))
    .expect("decodes")])
}

fn charge_responder(command: &Command<Charge>) -> OrderInput {
    OrderInput::Charged {
        order: command.payload.order.clone(),
        ok: true,
    }
}

fn open(path: &Path) -> OrderCell {
    SqliteCell::open(
        path,
        order_model(),
        StoreOptions {
            journal_mode: JournalMode::Wal,
            cache_pages: None,
        },
        "org_demo/dev",
        charge_responder,
    )
    .expect("opens")
}

fn order_key() -> FactKey {
    FactKey::new("shop.Order", "O-1")
}

fn order(status: &str) -> JsonObject {
    serde_json::from_value(json!({ "status": status, "total": 19.99 })).expect("values")
}

/// Turn 1: place the order, charge it, and arm its expiry timer — in one
/// commit.
fn place() -> OrderCommit {
    CellCommit {
        writes: vec![FactWrite {
            key: order_key(),
            expected: FactVersion::ABSENT,
            mutation: FactMutation::Put {
                values: order("placed"),
            },
        }],
        events: vec![OrderEvent::Placed {
            order: "O-1".to_owned(),
        }],
        effects: vec![
            Effect::Perform(Command {
                id: CommandId("charge/O-1".to_owned()),
                generation: Generation(0),
                idempotency_key: IdempotencyKey("charge/O-1/0".to_owned()),
                freshness: FreshnessPolicy::AppendOnly,
                deadline: Some(LogicalTime::from_millis(5_000)),
                payload: Charge {
                    order: "O-1".to_owned(),
                    amount: "19.99".to_owned(),
                },
            }),
            Effect::Schedule(Timer {
                id: TimerId("expire/O-1".to_owned()),
                due: LogicalTime::from_millis(60_000),
                payload: OrderInput::Expire {
                    order: "O-1".to_owned(),
                },
            }),
        ],
        ..CellCommit::empty()
    }
}

#[test]
fn sqlite_cell_persists_the_effective_default_from_a_whole_object_put() {
    let object_type: norn_ir::ObjectTypeDef = serde_json::from_value(json!({
        "name": "test.EffectiveDefault",
        "properties": [{
            "id": "amount",
            "type": { "kind": "integer" },
            "constraints": { "nullable": true },
            "default": "-1"
        }]
    }))
    .expect("definition decodes");
    let model = common::model_of(&[object_type]);
    let mut cell: OrderCell =
        SqliteCell::open_in_memory(model, "org_demo/dev", charge_responder).expect("opens");
    let key = FactKey::new("test.EffectiveDefault", "negative");
    cell.commit(CellCommit {
        writes: vec![FactWrite {
            key: key.clone(),
            expected: FactVersion::ABSENT,
            mutation: FactMutation::Put {
                values: JsonObject::new(),
            },
        }],
        ..CellCommit::empty()
    })
    .expect("persists a put with its effective default");
    assert_eq!(
        cell.load(&key).expect("loads").values,
        Some(serde_json::from_value(json!({ "amount": -1 })).expect("runtime value"))
    );
}

#[test]
fn sqlite_cell_evaluates_referenced_numeric_defaults_in_storage_encoding() {
    let property_type: norn_ir::PropertyTypeDef = serde_json::from_value(json!({
        "name": "test.Amount",
        "type": { "kind": "integer" }
    }))
    .expect("shared property type decodes");
    for default in [-1, 1] {
        let object_type: norn_ir::ObjectTypeDef = serde_json::from_value(json!({
            "name": "test.ReferencedDefault",
            "properties": [{
                "id": "amount",
                "type": { "kind": "ref", "name": "test.Amount" },
                "default": default.to_string()
            }],
            "invariants": [{
                "id": "not_negative_one",
                "expression": "self.amount != -1",
                "message": "amount cannot be negative one"
            }]
        }))
        .expect("object definition decodes");
        let model = Model::from_definitions(&[object_type], std::slice::from_ref(&property_type))
            .expect("referenced property projects");
        let mut cell: OrderCell =
            SqliteCell::open_in_memory(model, "org_demo/dev", charge_responder).expect("opens");
        let key = FactKey::new("test.ReferencedDefault", "defaulted");
        let result = cell.commit(CellCommit {
            writes: vec![FactWrite {
                key: key.clone(),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put {
                    values: JsonObject::new(),
                },
            }],
            ..CellCommit::empty()
        });
        if default == -1 {
            let error = result.expect_err("numeric equality must reject the negative default");
            assert_eq!(error.code(), "commit.invariant_violation");
            assert!(
                cell.load(&key)
                    .expect("loads refused object")
                    .values
                    .is_none()
            );
        } else {
            result.expect("positive referenced default passes the invariant");
            assert_eq!(
                cell.load(&key).expect("loads accepted default").values,
                Some(serde_json::from_value(json!({ "amount": default })).expect("runtime value"))
            );
        }
    }
}

#[test]
fn sqlite_cell_checks_and_persists_the_same_effective_default_values() {
    let object_type: norn_ir::ObjectTypeDef = serde_json::from_value(json!({
        "name": "test.EffectiveDefaultInvariant",
        "properties": [{
            "id": "amount",
            "type": { "kind": "integer" },
            "constraints": { "nullable": true },
            "default": "-1"
        }, {
            "id": "label", "type": { "kind": "text" }, "default": "initial"
        }],
        "invariants": [{
            "id": "nonnegative_amount",
            "expression": "(self.amount ?? 0) >= 0",
            "message": "amount must be nonnegative"
        }]
    }))
    .expect("definition decodes");
    let model = common::model_of(&[object_type]);
    let mut cell: OrderCell =
        SqliteCell::open_in_memory(model, "org_demo/dev", charge_responder).expect("opens");
    let key = FactKey::new("test.EffectiveDefaultInvariant", "negative");

    let omitted = cell
        .commit(CellCommit {
            writes: vec![FactWrite {
                key: key.clone(),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put {
                    values: JsonObject::new(),
                },
            }],
            ..CellCommit::empty()
        })
        .expect_err("the materialized -1 default violates the invariant");
    assert_eq!(omitted.code(), "commit.invariant_violation");
    assert!(
        cell.load(&key)
            .expect("loads absent object")
            .values
            .is_none()
    );

    cell.commit(CellCommit {
        writes: vec![FactWrite {
            key: key.clone(),
            expected: FactVersion::ABSENT,
            mutation: FactMutation::Put {
                values: serde_json::from_value(json!({ "amount": null }))
                    .expect("explicit null runtime value"),
            },
        }],
        ..CellCommit::empty()
    })
    .expect("explicit null satisfies the coalesce invariant");
    assert_eq!(
        cell.load(&key).expect("loads explicit null").values,
        Some(
            serde_json::from_value(json!({ "amount": null, "label": "initial" }))
                .expect("stored null")
        )
    );
    let state = cell
        .load(&key)
        .expect("loads before another property update");
    let mut values = state.values.expect("object is live");
    values.insert(
        "label".to_owned(),
        norn_ir::RuntimeValue::String("updated".to_owned()),
    );
    cell.commit(CellCommit {
        writes: vec![FactWrite {
            key: key.clone(),
            expected: state.version,
            mutation: FactMutation::Put { values },
        }],
        ..CellCommit::empty()
    })
    .expect("updating another property preserves the explicit null");
    assert_eq!(
        cell.load(&key).expect("loads updated object").values,
        Some(
            serde_json::from_value(json!({ "amount": null, "label": "updated" }))
                .expect("stored null after update")
        )
    );
}

#[test]
fn sqlite_cell_canonicalizes_raw_numeric_strings_before_invariants_and_storage() {
    let object_type: norn_ir::ObjectTypeDef = serde_json::from_value(json!({
        "name": "test.RawNumericStrings",
        "properties": [
            {"id": "item_id", "type": {"kind": "text"}},
            {"id": "amount", "type": {"kind": "integer"}},
            {"id": "rate", "type": {"kind": "decimal", "scale": 2}}
        ],
        "invariants": [
            {
                "id": "amount_not_negative_one",
                "expression": "self.amount != -1",
                "message": "amount cannot be negative one"
            },
            {
                "id": "rate_not_negative_one_point_two_five",
                "expression": "self.rate != -1.25",
                "message": "rate cannot be negative one point two five"
            }
        ]
    }))
    .expect("definition decodes");
    let model = common::model_of(&[object_type]);
    let mut cell: OrderCell =
        SqliteCell::open_in_memory(model, "org_demo/dev", charge_responder).expect("opens");

    for (key, values, invariant_id) in [
        (
            "negative-integer-string",
            json!({"item_id": "negative-integer-string", "amount": "-1", "rate": "1.25"}),
            "amount_not_negative_one",
        ),
        (
            "negative-decimal-string",
            json!({"item_id": "negative-decimal-string", "amount": "4", "rate": "-1.25"}),
            "rate_not_negative_one_point_two_five",
        ),
    ] {
        let key = FactKey::new("test.RawNumericStrings", key);
        let before = cell.store().read().dump().expect("dumps before refusal");
        let refused = cell
            .commit(CellCommit {
                writes: vec![FactWrite {
                    key: key.clone(),
                    expected: FactVersion::ABSENT,
                    mutation: FactMutation::Put {
                        values: serde_json::from_value(values).expect("runtime input values"),
                    },
                }],
                ..CellCommit::empty()
            })
            .expect_err("invariant sees the number represented by the input string");
        assert!(matches!(
            refused,
            CommitError::InvariantViolation {
                ref id,
                ref outcome,
                ..
            } if id.as_str() == invariant_id
                && matches!(outcome.as_ref(), norn_ir::DataQualityOutcome::NotSatisfied)
        ));
        assert!(
            cell.load(&key)
                .expect("loads absent object")
                .values
                .is_none()
        );
        assert_eq!(
            cell.store().read().dump().expect("dumps after refusal"),
            before
        );
    }

    let key = FactKey::new("test.RawNumericStrings", "positive-numeric-strings");
    cell.commit(CellCommit {
        writes: vec![FactWrite {
            key: key.clone(),
            expected: FactVersion::ABSENT,
            mutation: FactMutation::Put {
                values: serde_json::from_value(json!({
                    "item_id": "positive-numeric-strings",
                    "amount": "7",
                    "rate": "12.50"
                }))
                .expect("raw numeric string values"),
            },
        }],
        ..CellCommit::empty()
    })
    .expect("valid numeric strings are accepted");
    assert_eq!(
        cell.load(&key).expect("loads canonical values").values,
        Some(
            serde_json::from_value(json!({
                "item_id": "positive-numeric-strings",
                "amount": 7,
                "rate": 12.50
            }))
            .expect("canonical numeric values")
        )
    );
}

#[test]
fn commit_and_enqueue_are_one_transaction() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cell = open(&dir.path().join("cell.sqlite"));
    let receipt = cell.commit(place()).expect("commits");
    assert_eq!(receipt.seq, LedgerSeq(1));
    assert_eq!(receipt.written[0].version, FactVersion(1));
    assert_eq!(cell.pending_commands().expect("outbox").len(), 1);
    assert_eq!(cell.pending_timers().expect("timers").len(), 1);
    let commits = cell.commits().expect("ledger");
    assert_eq!(commits.len(), 1);
    assert_eq!(
        commits[0].events,
        [OrderEvent::Placed {
            order: "O-1".to_owned()
        }]
    );

    // The same turn again is stale: CAS refuses it and nothing is written —
    // no fact, no event, no second command, no timer change.
    let before = cell.store().read().dump().expect("dumps");
    let refused = cell.commit(place()).expect_err("stale");
    assert_eq!(refused.code(), "commit.conflict");
    assert_eq!(cell.store().read().dump().expect("dumps"), before);
}

#[test]
fn a_timer_fires_when_due_once_and_a_stale_incarnation_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cell = open(&dir.path().join("cell.sqlite"));
    cell.commit(place()).expect("commits");
    assert_eq!(cell.clock().expect("clock"), None, "not due yet");
    cell.advance_to(LogicalTime::from_millis(60_000));
    let due = cell.clock().expect("clock").expect("due");
    assert_eq!(due.incarnation, LedgerSeq(1));
    assert_eq!(
        cell.clock().expect("clock").as_ref(),
        Some(&due),
        "redelivered until consumed"
    );

    // Re-arm the timer (a new incarnation): the old delivery is stale.
    let rearm = CellCommit {
        effects: vec![Effect::Schedule(due.timer.clone())],
        ..CellCommit::empty()
    };
    cell.commit(rearm).expect("commits");
    let stale = CellCommit::empty().consuming(due.consumed());
    let refused = cell.commit(stale).expect_err("stale incarnation");
    assert!(matches!(refused, CommitError::NotPending { .. }));

    let due = cell.clock().expect("clock").expect("due");
    assert_eq!(due.incarnation, LedgerSeq(2));
    let expire = CellCommit {
        writes: vec![FactWrite {
            key: order_key(),
            expected: FactVersion(1),
            mutation: FactMutation::Put {
                values: order("expired"),
            },
        }],
        events: vec![OrderEvent::Expired {
            order: "O-1".to_owned(),
        }],
        ..CellCommit::empty()
    }
    .consuming(due.consumed());
    cell.commit(expire).expect("commits");
    assert_eq!(cell.clock().expect("clock"), None, "consumed once");
    assert_eq!(
        cell.load(&order_key()).expect("loads").values,
        Some(order("expired"))
    );
}

#[test]
fn a_due_timer_peek_survives_reopen_and_consumes_once() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("cell.sqlite");
    let mut cell = open(&path);
    cell.commit(place()).expect("commits");
    cell.advance_to(LogicalTime::from_millis(60_000));
    let peeked = cell.clock().expect("clock").expect("due timer");
    assert_eq!(peeked.incarnation, LedgerSeq(1));
    drop(cell);

    let mut cell = open(&path);
    cell.advance_to(LogicalTime::from_millis(60_000));
    let due = cell
        .clock()
        .expect("reopened clock")
        .expect("redelivered timer");
    assert_eq!(due, peeked, "the peeked timer remains the same incarnation");
    cell.commit(
        CellCommit {
            writes: vec![FactWrite {
                key: order_key(),
                expected: FactVersion(1),
                mutation: FactMutation::Put {
                    values: order("expired"),
                },
            }],
            events: vec![OrderEvent::Expired {
                order: "O-1".to_owned(),
            }],
            ..CellCommit::empty()
        }
        .consuming(due.consumed()),
    )
    .expect("consume due timer");
    assert_eq!(cell.clock().expect("clock after consume"), None);
    drop(cell);

    let mut cell = open(&path);
    cell.advance_to(LogicalTime::from_millis(60_000));
    assert_eq!(cell.clock().expect("reopened clock after consume"), None);
    assert_eq!(
        cell.load(&order_key()).expect("loads").values,
        Some(order("expired"))
    );
    let expired_events = cell
        .commits()
        .expect("ledger")
        .into_iter()
        .flat_map(|commit| commit.events)
        .filter(|event| matches!(event, OrderEvent::Expired { order } if order.as_str() == "O-1"))
        .count();
    assert_eq!(expired_events, 1, "the timer effect commits once");
}

#[test]
fn a_performed_result_is_recorded_once_and_survives_reopen() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("cell.sqlite");
    let mut cell = open(&path);
    cell.commit(place()).expect("commits");
    let performed = cell.perform().expect("performs").expect("a command");
    assert_eq!(
        performed.input,
        OrderInput::Charged {
            order: "O-1".to_owned(),
            ok: true
        }
    );
    assert_eq!(cell.executions().expect("runs"), 1);
    drop(cell);

    // A crash before the result is consumed: the reopened cell redelivers the
    // recorded result and does not charge again.
    let mut cell = open(&path);
    assert_eq!(cell.perform().expect("performs"), Some(performed.clone()));
    assert_eq!(cell.executions().expect("runs"), 1);

    let settle = CellCommit {
        writes: vec![FactWrite {
            key: order_key(),
            expected: FactVersion(1),
            mutation: FactMutation::Put {
                values: order("paid"),
            },
        }],
        events: vec![OrderEvent::Charged {
            order: "O-1".to_owned(),
            ok: true,
        }],
        ..CellCommit::empty()
    }
    .consuming(Consumed::Result {
        command: performed.command.clone(),
        generation: performed.generation,
    });
    cell.commit(settle).expect("commits");
    assert_eq!(cell.perform().expect("performs"), None, "consumed");

    // Re-issuing the same generation never re-executes.
    cell.commit(CellCommit {
        effects: vec![place().effects[0].clone()],
        ..CellCommit::empty()
    })
    .expect("commits");
    assert_eq!(cell.perform().expect("performs"), None);
    assert_eq!(cell.executions().expect("runs"), 1);
}

#[test]
fn a_claim_without_a_result_survives_reopen_as_outcome_unknown() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("cell.sqlite");
    let mut cell = open(&path);
    cell.commit(place()).expect("commits");
    let command = match cell.claim_perform().expect("claims") {
        Some(PerformClaim::Fresh(command)) => command,
        other => panic!("expected fresh claim, got {other:?}"),
    };
    assert!(
        cell.store()
            .read()
            .perform_claimed(&command.id.0, command.generation.0)
            .expect("claim reads")
    );
    drop(cell);

    let mut cell = open(&path);
    assert!(matches!(
        cell.claim_perform().expect("claim reads"),
        Some(PerformClaim::OutcomeUnknown(found)) if found == command
    ));
    assert!(matches!(
        cell.perform(),
        Err(CellError::PerformOutcomeUnknown {
            command_id,
            generation: 0
        }) if command_id == command.id.0
    ));
    let input = OrderInput::Charged {
        order: "O-1".to_owned(),
        ok: false,
    };
    let performed = cell
        .complete_perform(&command, input.clone())
        .expect("completion records")
        .expect("current generation");
    assert_eq!(performed.input, input);
    assert!(matches!(
        cell.claim_perform().expect("redelivers"),
        Some(PerformClaim::Recorded {
            command: found,
            performed: found_result,
        }) if found == command && found_result == performed
    ));
}

#[test]
fn a_late_completion_never_writes_a_replacement_generation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cell = open(&dir.path().join("cell.sqlite"));
    cell.commit(place()).expect("commits");
    let old = match cell.claim_perform().expect("claims") {
        Some(PerformClaim::Fresh(command)) => command,
        other => panic!("expected fresh claim, got {other:?}"),
    };
    let mut replacement = old.clone();
    replacement.generation = Generation(1);
    replacement.idempotency_key = IdempotencyKey("charge/O-1/1".to_owned());
    cell.commit(CellCommit {
        effects: vec![Effect::Perform(replacement.clone())],
        ..CellCommit::empty()
    })
    .expect("replaces generation");

    assert_eq!(
        cell.complete_perform(
            &old,
            OrderInput::Charged {
                order: "O-1".to_owned(),
                ok: true,
            },
        )
        .expect("stale completion is ignored"),
        None
    );
    assert!(matches!(
        cell.claim_perform().expect("claims replacement"),
        Some(PerformClaim::Fresh(found)) if found == replacement
    ));
}

/// The commit/timer/perform scenario, start to finish.
fn scenario(path: &Path) {
    let identity =
        norn_cell::cell_identity::CellId::try_from("00112233445566778899aabbccddeeff".to_owned())
            .expect("same logical cell in each deterministic replay");
    drop(
        CellStore::open_bound(path, order_model(), StoreOptions::default(), &identity)
            .expect("seed replay identity"),
    );
    let mut cell = open(path);
    cell.commit(place()).expect("commits");
    let performed = cell.perform().expect("performs").expect("a command");
    cell.commit(
        CellCommit {
            writes: vec![FactWrite {
                key: order_key(),
                expected: FactVersion(1),
                mutation: FactMutation::Put {
                    values: order("paid"),
                },
            }],
            events: vec![OrderEvent::Charged {
                order: "O-1".to_owned(),
                ok: true,
            }],
            effects: vec![Effect::CancelTimer(TimerId("expire/O-1".to_owned()))],
            ..CellCommit::empty()
        }
        .consuming(Consumed::Result {
            command: performed.command,
            generation: performed.generation,
        }),
    )
    .expect("commits");
}

#[test]
fn reopen_recovers_the_ledger_hash_chain_identically_and_runs_are_deterministic() {
    let first = tempfile::tempdir().expect("tempdir");
    let second = tempfile::tempdir().expect("tempdir");
    scenario(&first.path().join("cell.sqlite"));
    scenario(&second.path().join("cell.sqlite"));

    let a = open(&first.path().join("cell.sqlite"));
    let b = open(&second.path().join("cell.sqlite"));
    let head = a.store().read().verify_ledger().expect("verifies");
    assert_eq!((head.seq, head.commits), (2, 3));
    assert_eq!(b.store().read().verify_ledger().expect("verifies"), head);
    assert_eq!(
        a.store().read().dump().expect("dumps"),
        b.store().read().dump().expect("dumps"),
        "the same turns produce the same store"
    );
    assert_eq!(a.commits().expect("ledger"), b.commits().expect("ledger"));
    assert_eq!(
        a.load(&order_key()).expect("loads"),
        FactState {
            version: FactVersion(2),
            values: Some(order("paid")),
        }
    );
    assert_eq!(a.pending_commands().expect("outbox"), []);
    assert_eq!(a.pending_timers().expect("timers"), []);
}

fn open_hierarchy(path: &Path) -> OrderCell {
    let base = serde_json::from_value(json!({
        "name": "acme.Base",
        "properties": [{ "id": "code", "type": { "kind": "text" }, "constraints": { "nullable": true } }]
    }))
    .expect("decodes");
    let child = serde_json::from_value(json!({
        "name": "acme.Child",
        "extends": "acme.Base",
        "properties": [{ "id": "tag", "type": { "kind": "text" }, "constraints": { "nullable": true } }]
    }))
    .expect("decodes");
    SqliteCell::open(
        path,
        common::model_of(&[base, child]),
        StoreOptions::default(),
        "org_demo/dev",
        charge_responder,
    )
    .expect("opens")
}

/// `acme.Base/K` and `acme.Child/K` are one object row and one version
/// counter here, so one commit may not name both; each alone commits with a
/// receipt that matches what `load` then reports — also across reopen.
#[test]
fn hierarchy_aliases_in_one_commit_are_refused() {
    for reopen in [false, true] {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("cell.sqlite");
        let mut cell = open_hierarchy(&path);
        let key = |object_type: &str| FactKey::new(object_type, "K");
        let delete = |cell: &OrderCell, object_type: &str| FactWrite {
            key: key(object_type),
            expected: cell.load(&key(object_type)).expect("loads").version,
            mutation: FactMutation::Delete,
        };
        let before = cell.store().read().dump().expect("dumps");
        let refused = cell
            .commit(CellCommit {
                writes: vec![delete(&cell, "acme.Base"), delete(&cell, "acme.Child")],
                ..CellCommit::empty()
            })
            .expect_err("aliases");
        assert_eq!(refused.code(), "commit.duplicate_key");
        assert_eq!(cell.store().read().dump().expect("dumps"), before);

        for object_type in ["acme.Base", "acme.Child", "acme.Base"] {
            let write = delete(&cell, object_type);
            let receipt = cell
                .commit(CellCommit {
                    writes: vec![write],
                    ..CellCommit::empty()
                })
                .expect("commits");
            if reopen {
                cell = open_hierarchy(&path);
            }
            assert_eq!(
                receipt.written[0].version,
                cell.load(&key(object_type)).expect("loads").version,
                "reopen: {reopen}"
            );
        }
    }
}

fn open_linked_hierarchy(path: &Path) -> OrderCell {
    let base = serde_json::from_value(json!({
        "name": "acme.Base",
        "properties": [{ "id": "code", "type": { "kind": "text" }, "constraints": { "nullable": true } }],
        "links": [{ "id": "rel", "target": "acme.Base", "cardinality": { "to": "many", "from": "many" } }]
    }))
    .expect("decodes");
    let child = serde_json::from_value(json!({
        "name": "acme.Child",
        "extends": "acme.Base",
        "properties": [{ "id": "tag", "type": { "kind": "text" }, "constraints": { "nullable": true } }]
    }))
    .expect("decodes");
    let other = serde_json::from_value(json!({
        "name": "acme.Other",
        "properties": [{ "id": "note", "type": { "kind": "text" }, "constraints": { "nullable": true } }]
    }))
    .expect("decodes");
    SqliteCell::open(
        path,
        common::model_of(&[base, child, other]),
        StoreOptions::default(),
        "org_demo/dev",
        charge_responder,
    )
    .expect("opens")
}

fn open_narrowed_link_hierarchy(path: &Path) -> OrderCell {
    let source_base = serde_json::from_value(json!({
        "name": "acme.SourceBase",
        "properties": []
    }))
    .expect("decodes");
    let source_brand = serde_json::from_value(json!({
        "name": "acme.SourceBrand",
        "extends": "acme.SourceBase",
        "properties": [],
        "links": [{
            "id": "featured",
            "target": "acme.TargetBrand",
            "cardinality": { "to": "many", "from": "many" }
        }]
    }))
    .expect("decodes");
    let target_base = serde_json::from_value(json!({
        "name": "acme.TargetBase",
        "properties": []
    }))
    .expect("decodes");
    let target_brand = serde_json::from_value(json!({
        "name": "acme.TargetBrand",
        "extends": "acme.TargetBase",
        "properties": []
    }))
    .expect("decodes");
    SqliteCell::open(
        path,
        common::model_of(&[source_base, source_brand, target_base, target_brand]),
        StoreOptions::default(),
        "org_demo/dev",
        charge_responder,
    )
    .expect("opens")
}

fn temporal_link_cell() -> OrderCell {
    let base: norn_ir::ObjectTypeDef = serde_json::from_value(json!({
        "name": "acme.TemporalBase",
        "links": [{
            "id": "rel",
            "target": "acme.TemporalBase",
            "cardinality": { "to": "many", "from": "many" },
            "properties": [
                {
                    "id": "service_date",
                    "type": { "kind": "date" },
                    "default": "2024-02-29"
                },
                {
                    "id": "occurred_at",
                    "type": { "kind": "timestamp", "timezone": "utc" },
                    "default": "2024-02-29T12:30:00+02:30"
                },
                {
                    "id": "shared_at",
                    "type": { "kind": "ref", "name": "acme.Instant" },
                    "default": "2024-02-29T12:30:00+02:30"
                }
            ]
        }]
    }))
    .expect("base definition decodes");
    let child: norn_ir::ObjectTypeDef = serde_json::from_value(json!({
        "name": "acme.TemporalChild",
        "extends": "acme.TemporalBase",
        "links": [{
            "id": "rel",
            "target": "acme.TemporalChild",
            "cardinality": { "to": "one", "from": "one" }
        }]
    }))
    .expect("child definition decodes");
    let instant: norn_ir::PropertyTypeDef = serde_json::from_value(json!({
        "name": "acme.Instant",
        "type": { "kind": "timestamp", "timezone": "offset" }
    }))
    .expect("shared property type decodes");
    let model =
        Model::from_definitions(&[base, child], &[instant]).expect("temporal link model projects");
    SqliteCell::open_in_memory(model, "org_demo/dev", charge_responder).expect("opens")
}

#[test]
fn raw_link_writes_admit_and_normalize_temporal_properties() {
    let mut cell = temporal_link_cell();
    let put = |object_type: &str, key: &str| FactWrite {
        key: FactKey::new(object_type, key),
        expected: FactVersion::ABSENT,
        mutation: FactMutation::Put {
            values: JsonObject::new(),
        },
    };
    cell.commit(CellCommit {
        writes: vec![
            put("acme.TemporalChild", "S1"),
            put("acme.TemporalChild", "T1"),
            put("acme.TemporalChild", "S2"),
            put("acme.TemporalChild", "T2"),
            put("acme.TemporalChild", "S3"),
            put("acme.TemporalBase", "B"),
        ],
        ..CellCommit::empty()
    })
    .expect("creates endpoints");

    let add = |source: &str, target: &str, values: JsonObject| LinkWrite {
        key: LinkKey::new(
            FactKey::new("acme.TemporalBase", source),
            "rel",
            FactKey::new("acme.TemporalBase", target),
        ),
        expected: FactVersion::ABSENT,
        mutation: LinkMutation::Add { values },
    };
    let before = cell.store().read().dump().expect("dumps");
    for (property, value) in [
        ("service_date", "2025-02-29"),
        ("occurred_at", "2025-02-29T10:00:00+00:00"),
    ] {
        let submitted = if property == "service_date" {
            json!({ "service_date": value })
        } else {
            json!({ "occurred_at": value })
        };
        let values = serde_json::from_value(submitted).expect("link property values decode");
        let refused = cell
            .commit(CellCommit {
                links: vec![add("S1", "T1", values)],
                ..CellCommit::empty()
            })
            .expect_err("invalid temporal link property is refused");
        assert!(
            matches!(&refused, CommitError::EffectiveValues { .. }),
            "{refused:?}"
        );
        assert_eq!(cell.store().read().dump().expect("dumps"), before);
    }

    // The source and target are concrete TemporalChild objects, while this
    // raw host call names both through TemporalBase. The child's narrowed
    // link contract is still used by admission.
    let narrowed = cell
        .commit(CellCommit {
            links: vec![add("S3", "B", JsonObject::new())],
            ..CellCommit::empty()
        })
        .expect_err("the concrete child link only targets TemporalChild");
    assert_eq!(narrowed.code(), "commit.link_target", "{narrowed:?}");
    assert_eq!(cell.store().read().dump().expect("dumps"), before);

    let explicit_values = serde_json::from_value(json!({
        "service_date": "2024-02-29",
        "occurred_at": "2024-02-29T12:30:00+02:30"
    }))
    .expect("explicit temporal properties decode");
    cell.commit(CellCommit {
        links: vec![add("S1", "T1", explicit_values)],
        ..CellCommit::empty()
    })
    .expect("valid temporal link properties are admitted");
    cell.commit(CellCommit {
        links: vec![add("S2", "T2", JsonObject::new())],
        ..CellCommit::empty()
    })
    .expect("link defaults are admitted");

    for (source, target) in [("S1", "T1"), ("S2", "T2")] {
        let key = LinkKey::new(
            FactKey::new("acme.TemporalBase", source),
            "rel",
            FactKey::new("acme.TemporalBase", target),
        );
        let stored = serde_json::to_value(
            cell.load_link(&key)
                .expect("loads link")
                .values
                .expect("link exists"),
        )
        .expect("serializes link values");
        assert_eq!(stored["service_date"], json!("2024-02-29"));
        assert_eq!(stored["occurred_at"], json!("2024-02-29T10:00:00Z"));
        assert_eq!(stored["shared_at"], json!("2024-02-29T10:00:00Z"));
    }
}

/// #3013: a link write names a target type its declaration admits, and an
/// added link's endpoint deleted in the same commit through another type of
/// its hierarchy is caught: the link would dangle.
#[test]
fn link_targets_are_typed_and_hierarchy_deletes_cannot_dangle_them() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cell = open_linked_hierarchy(&dir.path().join("cell.sqlite"));
    let put = |object_type: &str, key: &str| FactWrite {
        key: FactKey::new(object_type, key),
        expected: FactVersion::ABSENT,
        mutation: FactMutation::Put {
            values: JsonObject::new(),
        },
    };
    cell.commit(CellCommit {
        writes: vec![
            put("acme.Base", "S"),
            put("acme.Child", "T"),
            put("acme.Other", "T"),
        ],
        ..CellCommit::empty()
    })
    .expect("creates");
    let add = |target_type: &str| LinkWrite {
        key: LinkKey::new(
            FactKey::new("acme.Base", "S"),
            "rel",
            FactKey::new(target_type, "T"),
        ),
        expected: FactVersion::ABSENT,
        mutation: LinkMutation::Add {
            values: JsonObject::new(),
        },
    };
    let before = cell.store().read().dump().expect("dumps");
    // `rel` targets acme.Base: an unrelated type sharing the id is refused.
    let refused = cell
        .commit(CellCommit {
            links: vec![add("acme.Other")],
            ..CellCommit::empty()
        })
        .expect_err("unrelated target type");
    assert_eq!(refused.code(), "commit.link_target", "{refused:?}");
    // Deleting acme.Base/T while adding a link to acme.Child/T: one row.
    let delete = FactWrite {
        key: FactKey::new("acme.Base", "T"),
        expected: cell
            .load(&FactKey::new("acme.Base", "T"))
            .expect("loads")
            .version,
        mutation: FactMutation::Delete,
    };
    let dangling = cell
        .commit(CellCommit {
            writes: vec![delete],
            links: vec![add("acme.Child")],
            ..CellCommit::empty()
        })
        .expect_err("the link would dangle");
    assert_eq!(dangling.code(), "commit.dangling_link");
    assert_eq!(cell.store().read().dump().expect("dumps"), before);
}

fn create_link_read_objects(cell: &mut OrderCell, keys: &[FactKey]) {
    cell.commit(CellCommit {
        writes: keys
            .iter()
            .map(|key| FactWrite {
                key: key.clone(),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put {
                    values: JsonObject::new(),
                },
            })
            .collect(),
        ..CellCommit::empty()
    })
    .expect("objects created");
}

/// A complete link-slice read protects both an initially empty adjacency set
/// from phantoms and a previously live target from remove/re-add ABA.
#[test]
fn bounded_link_reads_refuse_phantoms_and_version_changes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cell = open_linked_hierarchy(&dir.path().join("cell.sqlite"));
    let source = FactKey::new("acme.Base", "S");
    let target = FactKey::new("acme.Child", "A");
    let other_target = FactKey::new("acme.Base", "Z");
    create_link_read_objects(
        &mut cell,
        &[source.clone(), target.clone(), other_target.clone()],
    );

    let empty = cell
        .read_link_targets(&source, "rel", 8, 1024)
        .expect("reads bounded adjacency")
        .expect("host supports bounded adjacency");
    assert_eq!(empty, []);
    let link = LinkKey::new(source.clone(), "rel", target);
    let add = |expected| LinkWrite {
        key: link.clone(),
        expected: FactVersion(expected),
        mutation: LinkMutation::Add {
            values: JsonObject::new(),
        },
    };
    cell.commit(CellCommit {
        links: vec![
            add(0),
            LinkWrite {
                key: LinkKey::new(source.clone(), "rel", other_target),
                expected: FactVersion::ABSENT,
                mutation: LinkMutation::Add {
                    values: JsonObject::new(),
                },
            },
        ],
        ..CellCommit::empty()
    })
    .expect("link added after empty snapshot");
    let phantom = cell
        .commit(CellCommit {
            link_reads: vec![LinkReadExpectation {
                source: source.clone(),
                link: "rel".to_owned(),
                targets: empty,
            }],
            ..CellCommit::empty()
        })
        .expect_err("the formerly empty adjacency changed");
    assert_eq!(phantom.code(), "commit.link_read_conflict");

    let observed = cell
        .read_link_targets(&source, "rel", 8, 1024)
        .expect("reads bounded adjacency")
        .expect("host supports bounded adjacency");
    assert_eq!(observed.len(), 2);
    assert_eq!(observed[0].key, link);
    assert_eq!(observed[0].key.target.key, "A");
    assert_eq!(observed[1].key.target.key, "Z");
    cell.commit(CellCommit {
        link_reads: vec![LinkReadExpectation {
            source: source.clone(),
            link: "rel".to_owned(),
            targets: observed.clone(),
        }],
        ..CellCommit::empty()
    })
    .expect("unchanged adjacency commits");

    cell.commit(CellCommit {
        links: vec![LinkWrite {
            key: link.clone(),
            expected: observed[0].version,
            mutation: LinkMutation::Remove,
        }],
        ..CellCommit::empty()
    })
    .expect("link removed");
    cell.commit(CellCommit {
        links: vec![add(observed[0].version.0 + 1)],
        ..CellCommit::empty()
    })
    .expect("same target re-added with a new version");
    let aba = cell
        .commit(CellCommit {
            link_reads: vec![LinkReadExpectation {
                source,
                link: "rel".to_owned(),
                targets: observed,
            }],
            ..CellCommit::empty()
        })
        .expect_err("the link version changed even though membership returned");
    assert_eq!(aba.code(), "commit.link_read_conflict");
}

/// Link CAS preserves concrete source and target subtype semantics even
/// though commit-time identities are canonicalized to hierarchy roots.
#[test]
fn bounded_link_reads_keep_subtype_owned_declarations_and_targets() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cell = open_narrowed_link_hierarchy(&dir.path().join("cell.sqlite"));
    let source = FactKey::new("acme.SourceBrand", "S");
    let target = FactKey::new("acme.TargetBrand", "T");
    create_link_read_objects(&mut cell, &[source.clone(), target.clone()]);
    let link = LinkKey::new(source.clone(), "featured", target);
    cell.commit(CellCommit {
        links: vec![LinkWrite {
            key: link,
            expected: FactVersion::ABSENT,
            mutation: LinkMutation::Add {
                values: JsonObject::new(),
            },
        }],
        ..CellCommit::empty()
    })
    .expect("narrowed target is admitted");

    let observed = cell
        .read_link_targets(&source, "featured", 8, 1024)
        .expect("reads bounded adjacency")
        .expect("host supports bounded adjacency");
    assert_eq!(observed.len(), 1);
    assert_eq!(
        observed[0].key.source.object_type.as_str(),
        "acme.SourceBrand"
    );
    assert_eq!(
        observed[0].key.target.object_type.as_str(),
        "acme.TargetBrand"
    );
    cell.commit(CellCommit {
        link_reads: vec![LinkReadExpectation {
            source,
            link: "featured".to_owned(),
            targets: observed,
        }],
        ..CellCommit::empty()
    })
    .expect("the concrete subtype target set remains current");
}

/// #3013: `acme.Base/S -rel-> T` and `acme.Child/S -rel-> T` are one link
/// row and one version here, so one commit may not write both; deleting an
/// endpoint finds its live link whichever type of the hierarchy names it,
/// and the delete commits once the same commit removes the link by the key
/// it was added with.
#[test]
fn link_aliases_are_refused_and_deletes_find_typed_links() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cell = open_linked_hierarchy(&dir.path().join("cell.sqlite"));
    let put = |object_type: &str, key: &str| FactWrite {
        key: FactKey::new(object_type, key),
        expected: FactVersion::ABSENT,
        mutation: FactMutation::Put {
            values: JsonObject::new(),
        },
    };
    let rel = |source_type: &str| {
        LinkKey::new(
            FactKey::new(source_type, "S"),
            "rel",
            FactKey::new("acme.Base", "T"),
        )
    };
    let add = |source_type: &str, expected: u64| LinkWrite {
        key: rel(source_type),
        expected: FactVersion(expected),
        mutation: LinkMutation::Add {
            values: JsonObject::new(),
        },
    };
    cell.commit(CellCommit {
        writes: vec![put("acme.Child", "S"), put("acme.Base", "T")],
        ..CellCommit::empty()
    })
    .expect("creates");
    let before = cell.store().read().dump().expect("dumps");
    let refused = cell
        .commit(CellCommit {
            links: vec![add("acme.Base", 0), add("acme.Child", 0)],
            ..CellCommit::empty()
        })
        .expect_err("aliases");
    assert_eq!(refused.code(), "commit.duplicate_link");
    assert_eq!(cell.store().read().dump().expect("dumps"), before);

    let receipt = cell
        .commit(CellCommit {
            links: vec![add("acme.Child", 0)],
            ..CellCommit::empty()
        })
        .expect("links");
    assert_eq!(receipt.linked[0].version, FactVersion(1));
    // Either alias reads the one row.
    assert_eq!(
        cell.load_link(&rel("acme.Base")).expect("loads").version,
        FactVersion(1)
    );

    // Deleting the source, named through its base type, finds the link.
    let delete = FactWrite {
        key: FactKey::new("acme.Base", "S"),
        expected: cell
            .load(&FactKey::new("acme.Base", "S"))
            .expect("loads")
            .version,
        mutation: FactMutation::Delete,
    };
    let dangling = cell
        .commit(CellCommit {
            writes: vec![delete.clone()],
            ..CellCommit::empty()
        })
        .expect_err("a live link would dangle");
    assert_eq!(dangling.code(), "commit.dangling_link");
    cell.commit(CellCommit {
        writes: vec![delete],
        links: vec![LinkWrite {
            key: rel("acme.Child"),
            expected: FactVersion(1),
            mutation: LinkMutation::Remove,
        }],
        ..CellCommit::empty()
    })
    .expect("delete with the link removed");
    assert!(!cell.load_link(&rel("acme.Child")).expect("loads").exists());
}

/// #3018 review: a narrowing override binds however the source is named,
/// exactly as on the reference host.
#[test]
fn link_overrides_bind_through_a_supertype_name() {
    let mut cell: OrderCell = SqliteCell::open_in_memory(
        common::model_of(&override_object_types()),
        "org_demo/dev",
        charge_responder,
    )
    .expect("opens");
    check_link_override(&mut cell);
}

#[test]
fn link_overrides_bind_through_a_supertype_name_on_restricted_engine() {
    let engine = RestrictedEngine::open_memory(&FaultSwitch::default()).expect("opens engine");
    let store = CellStore::from_engine(engine, common::model_of(&override_object_types()))
        .expect("opens store");
    let mut cell: SqliteCell<OrderEvent, Charge, OrderInput, RestrictedEngine> =
        SqliteCell::new(store, "org_demo/dev", charge_responder).expect("creates host");
    check_link_override(&mut cell);
}

/// #3018 review: sibling types that each declare `rel` get a table each,
/// yet a link's compare-and-swap version stays canonical: stale versions
/// are refused and the receipt's version is the stored one.
#[test]
fn sibling_link_tables_keep_one_canonical_version() {
    let root =
        serde_json::from_value(json!({ "name": "acme.Root", "properties": [] })).expect("decodes");
    let sibling = |name: &str| {
        serde_json::from_value(json!({
            "name": name,
            "extends": "acme.Root",
            "properties": [],
            "links": [{ "id": "rel", "target": "acme.Root", "cardinality": { "to": "many", "from": "many" } }]
        }))
        .expect("decodes")
    };
    let mut cell: OrderCell = SqliteCell::open_in_memory(
        common::model_of(&[root, sibling("acme.A"), sibling("acme.B")]),
        "org_demo/dev",
        charge_responder,
    )
    .expect("opens");
    cell.commit(CellCommit {
        writes: ["s", "t"]
            .into_iter()
            .map(|key| FactWrite {
                key: FactKey::new("acme.B", key),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put {
                    values: JsonObject::new(),
                },
            })
            .collect(),
        ..CellCommit::empty()
    })
    .expect("creates");
    let key = LinkKey::new(
        FactKey::new("acme.B", "s"),
        "rel",
        FactKey::new("acme.B", "t"),
    );
    let mut change = |expected: u64, add: bool| {
        cell.commit(CellCommit {
            links: vec![LinkWrite {
                key: key.clone(),
                expected: FactVersion(expected),
                mutation: if add {
                    LinkMutation::Add {
                        values: JsonObject::new(),
                    }
                } else {
                    LinkMutation::Remove
                },
            }],
            ..CellCommit::empty()
        })
        .map(|receipt| receipt.linked[0].version.0)
        .map_err(|error| error.code())
    };
    assert_eq!(change(0, true), Ok(1));
    assert_eq!(change(1, false), Ok(2));
    assert_eq!(change(0, true), Err("commit.link_conflict"));
    assert_eq!(change(1, true), Err("commit.link_conflict"));
    assert_eq!(change(2, true), Ok(3));
    let state = cell.load_link(&key).expect("loads");
    assert_eq!(state.version, FactVersion(3));
    assert!(state.exists());
}

/// #3018 review: sibling declarations of `rel` to unrelated roots are
/// different links even between the same keys: changing `A/s -rel-> X/t`
/// leaves the version of `B/s -rel-> Y/t` alone, as on the reference host.
#[test]
fn sibling_links_to_unrelated_targets_keep_separate_versions() {
    let def = |value: serde_json::Value| serde_json::from_value(value).expect("decodes");
    let sibling = |name: &str, target: &str| {
        def(json!({
            "name": name,
            "extends": "acme.Root",
            "properties": [],
            "links": [{ "id": "rel", "target": target, "cardinality": { "to": "many", "from": "many" } }]
        }))
    };
    let mut cell: OrderCell = SqliteCell::open_in_memory(
        common::model_of(&[
            def(json!({ "name": "acme.Root", "properties": [] })),
            def(json!({ "name": "acme.X", "properties": [] })),
            def(json!({ "name": "acme.Y", "properties": [] })),
            sibling("acme.A", "acme.X"),
            sibling("acme.B", "acme.Y"),
        ]),
        "org_demo/dev",
        charge_responder,
    )
    .expect("opens");
    let put = |object_type: &str| FactWrite {
        key: FactKey::new(object_type, if object_type == "acme.B" { "s" } else { "t" }),
        expected: FactVersion::ABSENT,
        mutation: FactMutation::Put {
            values: JsonObject::new(),
        },
    };
    cell.commit(CellCommit {
        writes: vec![put("acme.B"), put("acme.Y")],
        ..CellCommit::empty()
    })
    .expect("creates");
    let link = |source: &str, target: &str, add: bool| LinkWrite {
        key: LinkKey::new(FactKey::new(source, "s"), "rel", FactKey::new(target, "t")),
        expected: FactVersion::ABSENT,
        mutation: if add {
            LinkMutation::Add {
                values: JsonObject::new(),
            }
        } else {
            LinkMutation::Remove
        },
    };
    let removed = cell
        .commit(CellCommit {
            links: vec![link("acme.A", "acme.X", false)],
            ..CellCommit::empty()
        })
        .expect("removing an absent link bumps its version");
    assert_eq!(removed.linked[0].version, FactVersion(1));
    let added = cell
        .commit(CellCommit {
            links: vec![link("acme.B", "acme.Y", true)],
            ..CellCommit::empty()
        })
        .expect("a different link is still at version 0");
    assert_eq!(added.linked[0].version, FactVersion(1));
    let key = LinkKey::new(
        FactKey::new("acme.B", "s"),
        "rel",
        FactKey::new("acme.Y", "t"),
    );
    assert_eq!(cell.load_link(&key).expect("loads").version, FactVersion(1));
}

/// #1527 S2: a link add reads both ends' adjacency, and a host error on
/// that read refuses the commit in every build. Here `C`'s existing `A -rel->
/// C` row in the link index names a link table that does not exist. A
/// many-to-many add `B -rel-> C` checks no count at either end, yet the read
/// of `C`'s adjacency fails, so the commit refuses and writes nothing.
#[test]
fn a_link_add_refuses_when_an_end_adjacency_read_fails() {
    let thing = serde_json::from_value(json!({
        "name": "acme.Thing",
        "links": [{ "id": "rel", "target": "acme.Thing", "cardinality": { "to": "many", "from": "many" } }]
    }))
    .expect("decodes");
    let model = common::model_of(&[thing]);
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("cell.sqlite");
    let open = || -> OrderCell {
        SqliteCell::open(
            &path,
            model.clone(),
            StoreOptions::default(),
            "org_demo/dev",
            charge_responder,
        )
        .expect("opens")
    };
    let put = |key: &str| FactWrite {
        key: FactKey::new("acme.Thing", key),
        expected: FactVersion::ABSENT,
        mutation: FactMutation::Put {
            values: JsonObject::new(),
        },
    };
    let rel = |source: &str, target: &str| {
        LinkKey::new(
            FactKey::new("acme.Thing", source),
            "rel",
            FactKey::new("acme.Thing", target),
        )
    };
    let add = |source: &str, target: &str| CellCommit {
        links: vec![LinkWrite {
            key: rel(source, target),
            expected: FactVersion::ABSENT,
            mutation: LinkMutation::Add {
                values: JsonObject::new(),
            },
        }],
        ..CellCommit::empty()
    };
    let mut cell = open();
    cell.commit(CellCommit {
        writes: vec![put("A"), put("B"), put("C")],
        ..CellCommit::empty()
    })
    .expect("creates");
    cell.commit(add("A", "C")).expect("links A to C");
    drop(cell);
    let raw = rusqlite::Connection::open(&path).expect("corruption fixture");
    let rows = raw
        .execute(
            "UPDATE _norn_link_version SET link_table = 'missing_link_table' WHERE source = 'A' AND target = 'C'",
            [],
        )
        .expect("points the index at a missing table");
    assert_eq!(rows, 1);
    drop(raw);

    let mut cell = open();
    let refused = cell.commit(add("B", "C"));
    assert!(matches!(refused, Err(CommitError::Host(_))), "{refused:?}");
    assert_eq!(
        cell.load_link(&rel("B", "C")).expect("loads").version,
        FactVersion::ABSENT
    );
}

/// #3013: a subtype that narrows an inherited link's target is honoured:
/// with `Child.rel` narrowed to `acme.Child`, `Child/S -rel-> Base/T` is
/// refused while `Base/S -rel-> Base/T` and `Child/S -rel-> Child/U` commit.
#[test]
fn a_narrowed_link_target_is_enforced() {
    let base = serde_json::from_value(json!({
        "name": "acme.Base",
        "properties": [{ "id": "code", "type": { "kind": "text" }, "constraints": { "nullable": true } }],
        "links": [{ "id": "rel", "target": "acme.Base", "cardinality": { "to": "many", "from": "many" } }]
    }))
    .expect("decodes");
    let child = serde_json::from_value(json!({
        "name": "acme.Child",
        "extends": "acme.Base",
        "properties": [{ "id": "tag", "type": { "kind": "text" }, "constraints": { "nullable": true } }],
        "links": [{ "id": "rel", "target": "acme.Child", "cardinality": { "to": "many", "from": "many" } }]
    }))
    .expect("decodes");
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cell: OrderCell = SqliteCell::open(
        &dir.path().join("cell.sqlite"),
        common::model_of(&[base, child]),
        StoreOptions::default(),
        "org_demo/dev",
        charge_responder,
    )
    .expect("opens");
    let put = |object_type: &str, key: &str| FactWrite {
        key: FactKey::new(object_type, key),
        expected: FactVersion::ABSENT,
        mutation: FactMutation::Put {
            values: JsonObject::new(),
        },
    };
    cell.commit(CellCommit {
        writes: vec![
            put("acme.Base", "B"),
            put("acme.Child", "S"),
            put("acme.Base", "T"),
            put("acme.Child", "U"),
        ],
        ..CellCommit::empty()
    })
    .expect("creates");
    let add = |source: (&str, &str), target: (&str, &str)| LinkWrite {
        key: LinkKey::new(
            FactKey::new(source.0, source.1),
            "rel",
            FactKey::new(target.0, target.1),
        ),
        expected: FactVersion::ABSENT,
        mutation: LinkMutation::Add {
            values: JsonObject::new(),
        },
    };
    let refused = cell
        .commit(CellCommit {
            links: vec![add(("acme.Child", "S"), ("acme.Base", "T"))],
            ..CellCommit::empty()
        })
        .expect_err("Child.rel targets acme.Child only");
    assert_eq!(refused.code(), "commit.link_target", "{refused:?}");
    cell.commit(CellCommit {
        links: vec![
            add(("acme.Base", "B"), ("acme.Base", "T")),
            add(("acme.Child", "S"), ("acme.Child", "U")),
        ],
        ..CellCommit::empty()
    })
    .expect("admitted targets commit");
}

fn open_entries(path: &Path) -> OrderCell {
    let entry = serde_json::from_value(json!({
        "name": "books.Entry",
        "properties": [
            { "id": "account", "type": { "kind": "text" } },
            { "id": "booked_on", "type": { "kind": "date" } },
            { "id": "posted_at", "type": { "kind": "timestamp", "timezone": "offset" },
                "constraints": { "nullable": true } },
            { "id": "note", "type": { "kind": "text" }, "constraints": { "nullable": true } }
        ]
    }))
    .expect("decodes");
    SqliteCell::open(
        path,
        common::model_of(&[entry]),
        StoreOptions::default(),
        "org_demo/dev",
        charge_responder,
    )
    .expect("opens")
}

fn put_entry(cell: &mut OrderCell, id: &str, values: serde_json::Value) {
    let key = FactKey::new("books.Entry", id);
    let expected = cell.load(&key).expect("loads").version;
    cell.commit(CellCommit {
        writes: vec![FactWrite {
            key,
            expected,
            mutation: FactMutation::Put {
                values: serde_json::from_value(values).expect("values"),
            },
        }],
        ..CellCommit::empty()
    })
    .expect("entry written");
}

fn november(account: &str, end_inclusive: bool) -> PredicateQuery {
    serde_json::from_value(json!({
        "objectType": "books.Entry",
        "filter": { "account": account },
        "window": { "property": "booked_on", "kind": "date",
            "start": "2026-11-01", "end": "2026-11-30",
            "startInclusive": true, "endInclusive": end_inclusive }
    }))
    .expect("predicate")
}

fn read_members(cell: &OrderCell, predicate: &PredicateQuery) -> Vec<PredicateReadMember> {
    cell.read_predicate(predicate, 16, u64::MAX)
        .expect("reads")
        .expect("the store enumerates bounded types")
        .iter()
        .map(PredicateObject::member)
        .collect()
}

fn commit_read(
    cell: &mut OrderCell,
    predicate: &PredicateQuery,
    members: &[PredicateReadMember],
) -> Result<(), String> {
    cell.commit(CellCommit {
        predicate_reads: vec![PredicateReadExpectation {
            predicate: predicate.clone(),
            members: members.to_vec(),
        }],
        ..CellCommit::empty()
    })
    .map(|_| ())
    .map_err(|error| error.code().to_owned())
}

fn seeded_entries(path: &Path) -> OrderCell {
    let mut cell = open_entries(path);
    for (id, account, on) in [
        ("E1", "A", "2026-11-01"),
        ("E2", "A", "2026-11-30"),
        ("E3", "B", "2026-11-05"),
        ("E4", "A", "2026-12-01"),
    ] {
        put_entry(
            &mut cell,
            id,
            json!({ "account": account, "booked_on": on }),
        );
    }
    cell
}

fn ids(members: &[PredicateReadMember]) -> Vec<String> {
    members
        .iter()
        .map(|member| member.key.key.clone())
        .collect()
}

const CONFLICT: &str = "commit.predicate_read_conflict";

/// #1105/#1104: equality terms and each declared window edge select the
/// members; writes outside the predicate never conflict.
#[test]
fn predicate_reads_select_by_declared_edges_and_ignore_writes_outside() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cell = seeded_entries(&dir.path().join("cell.sqlite"));
    let exclusive = november("A", false);
    let inclusive = november("A", true);
    assert_eq!(ids(&read_members(&cell, &exclusive)), ["E1"]);
    assert_eq!(ids(&read_members(&cell, &inclusive)), ["E1", "E2"]);

    let members = read_members(&cell, &inclusive);
    assert_eq!(commit_read(&mut cell, &inclusive, &members), Ok(()));
    put_entry(
        &mut cell,
        "E3",
        json!({ "account": "B", "booked_on": "2026-11-06" }),
    );
    put_entry(
        &mut cell,
        "E4",
        json!({ "account": "A", "booked_on": "2026-12-02" }),
    );
    assert_eq!(commit_read(&mut cell, &inclusive, &members), Ok(()));
}

/// Completeness: an object that starts matching between the read and the
/// commit (a phantom insert, or an update that moves it in) conflicts.
#[test]
fn predicate_reads_conflict_when_a_phantom_starts_matching() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cell = seeded_entries(&dir.path().join("cell.sqlite"));
    let predicate = november("A", true);

    let members = read_members(&cell, &predicate);
    put_entry(
        &mut cell,
        "E5",
        json!({ "account": "A", "booked_on": "2026-11-10" }),
    );
    assert_eq!(
        commit_read(&mut cell, &predicate, &members),
        Err(CONFLICT.to_owned()),
        "a phantom insert inside the window"
    );

    let members = read_members(&cell, &predicate);
    put_entry(
        &mut cell,
        "E3",
        json!({ "account": "A", "booked_on": "2026-11-06" }),
    );
    assert_eq!(
        commit_read(&mut cell, &predicate, &members),
        Err(CONFLICT.to_owned()),
        "an update that moves an object into the predicate"
    );
}

/// Membership revalidation: a member that leaves the set (moved out or
/// deleted) or changes version conflicts.
#[test]
fn predicate_reads_conflict_when_a_member_leaves_or_changes_version() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cell = seeded_entries(&dir.path().join("cell.sqlite"));
    let predicate = november("A", true);

    let members = read_members(&cell, &predicate);
    put_entry(
        &mut cell,
        "E1",
        json!({ "account": "A", "booked_on": "2026-10-31" }),
    );
    assert_eq!(
        commit_read(&mut cell, &predicate, &members),
        Err(CONFLICT.to_owned()),
        "a member moved out of the window"
    );

    let members = read_members(&cell, &predicate);
    put_entry(
        &mut cell,
        "E2",
        json!({ "account": "A", "booked_on": "2026-11-30", "note": "x" }),
    );
    assert_eq!(
        commit_read(&mut cell, &predicate, &members),
        Err(CONFLICT.to_owned()),
        "a member changed version in place"
    );

    let members = read_members(&cell, &predicate);
    let key = FactKey::new("books.Entry", "E2");
    let expected = cell.load(&key).expect("loads").version;
    cell.commit(CellCommit {
        writes: vec![FactWrite {
            key,
            expected,
            mutation: FactMutation::Delete,
        }],
        ..CellCommit::empty()
    })
    .expect("member deleted");
    assert_eq!(
        commit_read(&mut cell, &predicate, &members),
        Err(CONFLICT.to_owned()),
        "a member deleted"
    );
    let members = read_members(&cell, &predicate);
    assert_eq!(members, [], "{members:?}");
    assert_eq!(commit_read(&mut cell, &predicate, &members), Ok(()));
}

#[test]
fn predicate_reads_bound_members_and_compare_timestamps_by_instant() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cell = open_entries(&dir.path().join("cell.sqlite"));
    for (id, posted_at) in [
        ("T1", "2026-11-01T01:30:00+02:00"),
        ("T2", "2026-11-01T00:00:00Z"),
        ("T3", "2026-10-31T23:59:59Z"),
    ] {
        put_entry(
            &mut cell,
            id,
            json!({ "account": "A", "booked_on": "2026-11-01", "posted_at": posted_at }),
        );
    }
    let predicate: PredicateQuery = serde_json::from_value(json!({
        "objectType": "books.Entry",
        "window": { "property": "posted_at", "kind": "timestamp",
            "start": "2026-10-31T23:00:00Z", "end": "2026-11-01T00:00:00Z",
            "startInclusive": false, "endInclusive": false }
    }))
    .expect("predicate");
    // T1 is 23:30Z and T3 23:59:59Z; T2 sits on the exclusive end.
    let members: Vec<String> = cell
        .read_predicate(&predicate, 16, u64::MAX)
        .expect("reads")
        .expect("complete")
        .into_iter()
        .map(|object| object.key.key)
        .collect();
    assert_eq!(members, ["T1", "T3"]);
    let bounded = cell
        .read_predicate(&predicate, 1, u64::MAX)
        .expect("reads")
        .expect("complete");
    assert_eq!(bounded.len(), 2, "one over the bound lets callers refuse");
    assert!(
        cell.read_predicate(&predicate, 16, 16)
            .expect("reads")
            .is_none(),
        "a byte bound smaller than the candidates fails the read closed"
    );
}

#[test]
fn malformed_predicate_reads_are_refused_before_the_host_is_read() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut cell = open_entries(&dir.path().join("cell.sqlite"));
    put_entry(
        &mut cell,
        "E1",
        json!({ "account": "A", "booked_on": "2026-11-02" }),
    );
    put_entry(
        &mut cell,
        "E2",
        json!({ "account": "A", "booked_on": "2026-11-03" }),
    );
    let predicate = november("A", true);
    let members = read_members(&cell, &predicate);
    let invalid = Err("commit.invalid_predicate_read".to_owned());

    let mut reversed = members.clone();
    reversed.reverse();
    assert_eq!(commit_read(&mut cell, &predicate, &reversed), invalid);

    let mut backwards = predicate.clone();
    if let Some(window) = backwards.window.as_mut() {
        std::mem::swap(&mut window.start, &mut window.end);
    }
    assert_eq!(commit_read(&mut cell, &backwards, &[]), invalid);

    let mut stranger = members.clone();
    stranger[0].key = FactKey::new("shop.Order", "E1");
    assert_eq!(commit_read(&mut cell, &predicate, &stranger), invalid);

    let twice = cell
        .commit(CellCommit {
            predicate_reads: vec![
                PredicateReadExpectation {
                    predicate: predicate.clone(),
                    members: members.clone(),
                },
                PredicateReadExpectation { predicate, members },
            ],
            ..CellCommit::empty()
        })
        .expect_err("one predicate is read once");
    assert_eq!(twice.code(), "commit.invalid_predicate_read");
}

fn parity_definitions() -> (Vec<norn_ir::ObjectTypeDef>, Vec<norn_ir::InterfaceDef>) {
    let types = serde_json::from_value(json!([{
        "name": "books.Entry",
        "implements": ["books.Dated"],
        "properties": [
            { "id": "account", "type": { "kind": "text" } },
            { "id": "booked_on", "type": { "kind": "date" } }
        ]
    }]))
    .expect("types");
    let interfaces =
        serde_json::from_value(json!([{ "name": "books.Dated" }])).expect("interfaces");
    (types, interfaces)
}

/// Sol on #1118: the SQLite cell and the memory cell agree on predicate
/// reads: identical members and versions for an object type, and an
/// interface predicate is unsupported on both, at read and at commit.
#[test]
fn predicate_reads_agree_across_hosts_and_refuse_interfaces_on_both() {
    let (types, interfaces) = parity_definitions();
    let dir = tempfile::tempdir().expect("tempdir");
    let mut native = SqliteCell::open(
        &dir.path().join("cell.sqlite"),
        Model::from_definitions_with_interfaces(&types, &[], &interfaces).expect("model"),
        StoreOptions::default(),
        "org_demo/dev",
        charge_responder,
    )
    .expect("opens");
    let mut memory = norn_testkit::cell_host::MemoryCell::<OrderEvent, Charge, OrderInput>::new(
        charge_responder,
    )
    .with_types(types)
    .with_interfaces(interfaces);
    for (id, account, on) in [
        ("E1", "A", "2026-11-01"),
        ("E2", "A", "2026-11-30"),
        ("E3", "B", "2026-11-05"),
        ("E4", "A", "2026-12-01"),
    ] {
        let write = || CellCommit {
            writes: vec![FactWrite {
                key: FactKey::new("books.Entry", id),
                expected: FactVersion::ABSENT,
                mutation: FactMutation::Put {
                    values: serde_json::from_value(json!({ "account": account, "booked_on": on }))
                        .expect("values"),
                },
            }],
            ..CellCommit::empty()
        };
        native.commit(write()).expect("native write");
        memory.commit(write()).expect("memory write");
    }

    let concrete = november("A", true);
    let members = |objects: Option<Vec<PredicateObject>>| -> Vec<PredicateReadMember> {
        objects
            .expect("an object type is enumerable")
            .iter()
            .map(PredicateObject::member)
            .collect()
    };
    let from_native = members(
        native
            .read_predicate(&concrete, 16, u64::MAX)
            .expect("native read"),
    );
    let from_memory = members(
        memory
            .read_predicate(&concrete, 16, u64::MAX)
            .expect("memory read"),
    );
    assert_eq!(ids(&from_native), ["E1", "E2"]);
    assert_eq!(from_native, from_memory, "members and versions agree");

    let interface: PredicateQuery =
        serde_json::from_value(json!({ "objectType": "books.Dated" })).expect("predicate");
    assert_eq!(
        native
            .read_predicate(&interface, 16, u64::MAX)
            .expect("native read"),
        None
    );
    assert_eq!(
        memory
            .read_predicate(&interface, 16, u64::MAX)
            .expect("memory read"),
        None
    );
    let pinned = || CellCommit {
        predicate_reads: vec![PredicateReadExpectation {
            predicate: interface.clone(),
            members: Vec::new(),
        }],
        ..CellCommit::empty()
    };
    let native_refusal = native.commit(pinned()).expect_err("native refuses");
    let memory_refusal = memory.commit(pinned()).expect_err("memory refuses");
    assert_eq!(native_refusal.code(), "commit.invalid_predicate_read");
    assert_eq!(memory_refusal.code(), native_refusal.code());

    let pinned_concrete = |members: &[PredicateReadMember]| CellCommit {
        predicate_reads: vec![PredicateReadExpectation {
            predicate: concrete.clone(),
            members: members.to_vec(),
        }],
        ..CellCommit::empty()
    };
    native
        .commit(pinned_concrete(&from_native))
        .expect("native commits an unchanged read");
    memory
        .commit(pinned_concrete(&from_memory))
        .expect("memory commits an unchanged read");
}

// ------------------------------------------------------------------ forks

/// WORLD-FORK (#624): a fork of a cell with a pending payment refuses to
/// perform it before any claim, evaluates a simulated fixture result instead,
/// and main is unchanged and still performs for real.
#[test]
fn world_fork_refuses_perform_and_evaluates_a_simulated_result() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("cell.sqlite");
    let mut main = open(&path);
    main.commit(place()).expect("commits");
    let before = main.store().image().expect("image");
    let base = main.store().verified_head().expect("head");

    let escaped = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let counter = std::sync::Arc::clone(&escaped);
    let store = main.store().fork("what-if", &base).expect("forks");
    let mut fork: OrderCell = SqliteCell::new(store, "org_demo/dev", move |command| {
        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        charge_responder(command)
    })
    .expect("serves the fork");
    let pin = fork.store().fork_pin().expect("pinned").clone();
    assert_eq!(
        (pin.world.as_str(), pin.parent.as_str(), &pin.base),
        ("what-if", "main", &base)
    );

    let forked = fork.store().image().expect("image");
    assert!(matches!(
        fork.perform(),
        Err(CellError::WorldPerformForbidden)
    ));
    assert_eq!(fork.store().image().expect("image"), forked, "no claim");
    let pending = fork.pending_commands().expect("outbox");
    assert_eq!(pending.len(), 1, "the payment stays pending, undispatched");

    let simulated = fork
        .simulate_perform(|command| OrderInput::Charged {
            order: command.payload.order.clone(),
            ok: false,
        })
        .expect("simulates")
        .expect("the head has no result");
    assert_eq!(fork.perform().expect("redelivers"), Some(simulated.clone()));
    fork.commit(
        CellCommit {
            events: vec![OrderEvent::Charged {
                order: "O-1".to_owned(),
                ok: false,
            }],
            ..CellCommit::empty()
        }
        .consuming(Consumed::Result {
            command: simulated.command,
            generation: simulated.generation,
        }),
    )
    .expect("the fork evaluates the fixture result");
    assert_eq!(fork.perform().expect("consumed"), None);
    assert_eq!(escaped.load(std::sync::atomic::Ordering::SeqCst), 0);

    assert_eq!(
        main.store().image().expect("image"),
        before,
        "main unchanged"
    );
    assert!(matches!(
        main.simulate_perform(charge_responder),
        Err(CellError::WorldSimulationForbidden)
    ));
    assert_eq!(main.executions().expect("runs"), 0);
    drop(main);
    let mut main = open(&path);
    assert_eq!(main.store().image().expect("image"), before);
    assert!(main.perform().expect("performs").is_some());
    assert_eq!(main.executions().expect("runs"), 1);
}

/// How many fork slots a trace uses.
const FORK_SLOTS: u8 = 3;

/// One generated write: put `value` on law object `key`, recording `value` as
/// an event and, with `enqueue`, a command.
#[derive(Debug, Clone, Copy)]
struct ForkPut {
    key: u8,
    value: u8,
    enqueue: bool,
}

#[derive(Debug, Clone, Copy)]
enum ForkOp {
    Main(ForkPut),
    MainPerform,
    MainClaim,
    MainComplete,
    MainSimulate,
    Create { slot: u8, from: Option<u8> },
    CreateStale { from: Option<u8> },
    CreateInvalid { from: Option<u8>, own: bool },
    Commit(u8, ForkPut),
    Perform(u8),
    Deliver(u8, u8),
    Evidence(u8),
    Simulate(u8, u8),
    Discard(u8),
}

fn fork_put() -> impl Strategy<Value = ForkPut> {
    (
        0..norn_testkit::cell_host::LAW_UNIVERSE,
        any::<u8>(),
        any::<bool>(),
    )
        .prop_map(|(key, value, enqueue)| ForkPut {
            key,
            value,
            enqueue,
        })
}

fn fork_op() -> impl Strategy<Value = ForkOp> {
    let slot = 0..FORK_SLOTS;
    let from = proptest::option::of(0..FORK_SLOTS);
    prop_oneof![
        4 => fork_put().prop_map(ForkOp::Main),
        2 => Just(ForkOp::MainPerform),
        1 => Just(ForkOp::MainClaim),
        1 => Just(ForkOp::MainComplete),
        1 => Just(ForkOp::MainSimulate),
        3 => (slot.clone(), from.clone()).prop_map(|(slot, from)| ForkOp::Create { slot, from }),
        1 => from.clone().prop_map(|from| ForkOp::CreateStale { from }),
        1 => (from, any::<bool>()).prop_map(|(from, own)| ForkOp::CreateInvalid { from, own }),
        4 => (slot.clone(), fork_put()).prop_map(|(slot, put)| ForkOp::Commit(slot, put)),
        2 => slot.clone().prop_map(ForkOp::Perform),
        1 => (slot.clone(), any::<u8>()).prop_map(|(slot, value)| ForkOp::Deliver(slot, value)),
        2 => slot.clone().prop_map(ForkOp::Evidence),
        2 => (slot.clone(), any::<u8>()).prop_map(|(slot, value)| ForkOp::Simulate(slot, value)),
        1 => slot.prop_map(ForkOp::Discard),
    ]
}

type LawNative = LawCell<NativeEngine>;

/// Commit `write`; the outcome, formatted for comparison.
fn put(cell: &mut LawNative, write: ForkPut) -> String {
    let key = law_key(write.key);
    let expected = cell.load(&key).expect("loads").version;
    let generation = cell.store().verified_head().expect("head").commits;
    let effects = if write.enqueue {
        vec![Effect::Perform(law_command(
            write.key,
            generation,
            write.value,
        ))]
    } else {
        Vec::new()
    };
    let outcome = cell.commit(CellCommit {
        writes: vec![FactWrite {
            key,
            expected,
            mutation: FactMutation::Put {
                values: law_values(write.value),
            },
        }],
        events: vec![write.value],
        effects,
        ..CellCommit::empty()
    });
    format!("{outcome:?}")
}

fn consume(cell: &mut LawNative, performed: &Performed<WireInput>) {
    cell.commit(CellCommit::empty().consuming(Consumed::Result {
        command: performed.command.clone(),
        generation: performed.generation,
    }))
    .expect("consumes the delivered result");
}

/// Perform main's head command, and consume what it delivers.
fn perform_main(cell: &mut LawNative) -> String {
    // A claim whose outcome is unknown refuses here, identically in both
    // mains. Any other refusal on main is a bug, never an equal outcome.
    let performed = match cell.perform() {
        Ok(performed) => performed,
        Err(error @ CellError::PerformOutcomeUnknown { .. }) => return format!("{error}"),
        Err(error) => panic!("main's perform refused: {error:?}"),
    };
    if let Some(performed) = &performed {
        consume(cell, performed);
    }
    format!("{performed:?}")
}

/// Claim main's head command without completing it (Fable S9: a fork taken
/// between claim and result inherits an outcome-unknown claim).
fn claim_main(cell: &mut LawNative) -> String {
    match cell.claim_perform() {
        Ok(Some(norn_cell::PerformClaim::Fresh(command))) => format!("claimed {}", command.id.0),
        Ok(other) => format!("{other:?}"),
        Err(error) => panic!("main's claim refused: {error:?}"),
    }
}

/// Complete main's claimed head command with the responder's answer, then
/// consume it.
fn complete_main(cell: &mut LawNative) -> String {
    let Some(command) = cell.pending_commands().expect("outbox").into_iter().next() else {
        return "nothing pending".to_owned();
    };
    let input = law_cell_responder(&command);
    match cell.complete_perform(&command, input) {
        Ok(Some(performed)) => {
            consume(cell, &performed);
            format!("completed {}", command.id.0)
        }
        Ok(None) => "not the claimed head".to_owned(),
        Err(error) => panic!("main's completion refused: {error:?}"),
    }
}

/// The instance one operation may change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Touched {
    Nothing,
    Main,
    Fork(u8),
}

impl Touched {
    /// Whether this may change the instance keyed `instance` (`None` is main).
    fn reaches(self, instance: Option<u8>) -> bool {
        match (self, instance) {
            (Self::Main, None) => true,
            (Self::Fork(slot), Some(other)) => slot == other,
            (Self::Nothing | Self::Fork(_), None) | (Self::Nothing | Self::Main, Some(_)) => false,
        }
    }
}

struct ForkTrace {
    main: LawNative,
    /// Main run alone: the same main operations, no fork operations (t∖F).
    alone: LawNative,
    forks: std::collections::BTreeMap<u8, LawNative>,
    /// Each fork's main horizon, tracked by the trace itself: the main commit
    /// count when its lineage left main, inherited by forks of forks.
    horizons: std::collections::BTreeMap<u8, u64>,
    executed: std::sync::Arc<std::sync::atomic::AtomicU64>,
    alone_executed: std::sync::Arc<std::sync::atomic::AtomicU64>,
    escaped: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

fn counting_cell(
    store: CellStore<NativeEngine>,
    counter: &std::sync::Arc<std::sync::atomic::AtomicU64>,
) -> LawNative {
    let counter = std::sync::Arc::clone(counter);
    SqliteCell::new(store, "law", move |command| {
        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        law_cell_responder(command)
    })
    .expect("serves the cell")
}

impl ForkTrace {
    fn new() -> Self {
        let executed = std::sync::Arc::default();
        let alone_executed = std::sync::Arc::default();
        // Main and main-alone are one logical cell (CELL-ID-1), so their
        // images compare byte for byte.
        let identity = norn_cell::cell_identity::CellId::try_from(
            "00112233445566778899aabbccddeeff".to_owned(),
        )
        .expect("a cell id");
        let store = || {
            CellStore::from_engine_bound(
                NativeEngine::open_in_memory().expect("opens"),
                law_model(),
                &identity,
            )
            .expect("opens store")
        };
        Self {
            main: counting_cell(store(), &executed),
            alone: counting_cell(store(), &alone_executed),
            forks: std::collections::BTreeMap::new(),
            horizons: std::collections::BTreeMap::new(),
            executed,
            alone_executed,
            escaped: std::sync::Arc::default(),
        }
    }

    fn parent(&self, from: Option<u8>) -> &LawNative {
        from.and_then(|slot| self.forks.get(&slot))
            .unwrap_or(&self.main)
    }

    fn images(&self) -> std::collections::BTreeMap<Option<u8>, norn_cell::StoreImage> {
        let mut images = std::collections::BTreeMap::from([(
            None,
            self.main.store().image().expect("main image"),
        )]);
        for (slot, fork) in &self.forks {
            images.insert(Some(*slot), fork.store().image().expect("fork image"));
        }
        images
    }

    /// Apply `op`; return the instance it may change.
    #[allow(
        clippy::too_many_lines,
        reason = "one arm per generated operation, each with its expected refusal"
    )]
    fn apply(&mut self, op: ForkOp) -> Touched {
        match op {
            ForkOp::Main(write) => {
                assert_eq!(put(&mut self.main, write), put(&mut self.alone, write));
                Touched::Main
            }
            ForkOp::MainPerform => {
                assert_eq!(perform_main(&mut self.main), perform_main(&mut self.alone));
                Touched::Main
            }
            ForkOp::MainClaim => {
                assert_eq!(claim_main(&mut self.main), claim_main(&mut self.alone));
                Touched::Main
            }
            ForkOp::MainComplete => {
                assert_eq!(
                    complete_main(&mut self.main),
                    complete_main(&mut self.alone)
                );
                Touched::Main
            }
            ForkOp::MainSimulate => {
                assert!(matches!(
                    self.main.simulate_perform(|_| WireInput::Result(0)),
                    Err(CellError::WorldSimulationForbidden)
                ));
                Touched::Nothing
            }
            ForkOp::Create { slot, from } => {
                if self.forks.contains_key(&slot) {
                    return Touched::Nothing;
                }
                let parent = self.parent(from);
                let parent_world = parent
                    .store()
                    .fork_pin()
                    .map_or_else(|| "main".to_owned(), |pin| pin.world.clone());
                let base = parent.store().verified_head().expect("head");
                let horizon = from
                    .and_then(|from| self.horizons.get(&from).copied())
                    .unwrap_or(base.commits);
                let world = format!("fork-{slot}");
                let store = parent.store().fork(&world, &base).expect("forks");
                // L4 at creation: the fork reads exactly what its parent holds.
                assert_eq!(
                    store.image().expect("image").without_fork_pin(),
                    parent.store().image().expect("image").without_fork_pin()
                );
                assert_eq!(
                    store
                        .fork_pin()
                        .map(|pin| (&pin.world, &pin.parent, &pin.base)),
                    Some((&world, &parent_world, &base))
                );
                let fork = counting_cell(store, &self.escaped);
                self.forks.insert(slot, fork);
                self.horizons.insert(slot, horizon);
                Touched::Fork(slot)
            }
            ForkOp::CreateStale { from } => {
                let parent = self.parent(from);
                let mut base = parent.store().verified_head().expect("head");
                base.commits = base.commits.saturating_add(1);
                assert!(matches!(
                    parent.store().fork("stale", &base),
                    Err(CellError::ForkStaleBase { .. })
                ));
                Touched::Nothing
            }
            ForkOp::CreateInvalid { from, own } => {
                let parent = self.parent(from);
                let world = match (own, parent.store().fork_pin()) {
                    (true, Some(pin)) => pin.world.clone(),
                    (false, _) => String::new(),
                    (true, None) => "main".to_owned(),
                };
                let base = parent.store().verified_head().expect("head");
                assert!(matches!(
                    parent.store().fork(&world, &base),
                    Err(CellError::ForkInvalidWorld)
                ));
                Touched::Nothing
            }
            ForkOp::Commit(slot, write) => {
                let Some(fork) = self.forks.get_mut(&slot) else {
                    return Touched::Nothing;
                };
                // Fork commits are ordinary commits of the fork's own state.
                put(fork, write);
                Touched::Fork(slot)
            }
            ForkOp::Perform(slot) => {
                let Some(fork) = self.forks.get_mut(&slot) else {
                    return Touched::Nothing;
                };
                let pending = !fork.pending_commands().expect("outbox").is_empty();
                let before = fork.store().image().expect("image");
                match fork.perform() {
                    Err(CellError::WorldPerformForbidden) => {
                        assert!(pending, "refused with nothing to perform");
                        assert_eq!(fork.store().image().expect("image"), before);
                    }
                    // Only a simulated result is ever delivered in a fork.
                    Ok(Some(performed)) => consume(fork, &performed),
                    Ok(None) => assert!(
                        !pending,
                        "a pending command was neither refused nor delivered"
                    ),
                    Err(other) => panic!("fork perform failed: {other}"),
                }
                Touched::Fork(slot)
            }
            ForkOp::Deliver(slot, value) => {
                // A real external result never lands in a fork, whatever the
                // inherited rows' world label says.
                let Some(fork) = self.forks.get_mut(&slot) else {
                    return Touched::Nothing;
                };
                assert!(matches!(
                    fork.complete_perform(&law_command(0, 0, value), WireInput::Result(value)),
                    Err(CellError::WorldPerformForbidden)
                ));
                Touched::Nothing
            }
            ForkOp::Evidence(slot) => {
                // FORK-EVIDENCE-1: records main committed before the lineage
                // left it export; records any fork in the lineage minted, and
                // the fork's receipt log, never do, at every nesting depth.
                let Some(fork) = self.forks.get(&slot) else {
                    return Touched::Nothing;
                };
                let store = fork.store();
                let horizon = self.horizons.get(&slot).copied();
                let head = store.export_commit_count().expect("head");
                assert_eq!(
                    store.world(),
                    norn_kernel::World::Fork {
                        id: norn_kernel::WorldId(format!("fork-{slot}")),
                    }
                );
                for seq in 1..=head {
                    let exported = store.export_record(seq);
                    if horizon.is_some_and(|horizon| seq > horizon) {
                        assert!(matches!(exported, Err(CellError::ForkEvidenceForbidden)));
                    } else {
                        assert!(matches!(exported, Ok(Some(_))), "inherited record {seq}");
                    }
                }
                assert!(matches!(
                    store.transparency_checkpoint(),
                    Err(CellError::ForkEvidenceForbidden)
                ));
                Touched::Nothing
            }
            ForkOp::Simulate(slot, value) => {
                let Some(fork) = self.forks.get_mut(&slot) else {
                    return Touched::Nothing;
                };
                if let Some(performed) = fork
                    .simulate_perform(|_| WireInput::Result(value))
                    .expect("a fork simulates")
                {
                    consume(fork, &performed);
                }
                Touched::Fork(slot)
            }
            ForkOp::Discard(slot) => {
                self.forks.remove(&slot);
                self.horizons.remove(&slot);
                Touched::Nothing
            }
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// FORK-NONINTERFERE, WORLD-ISOLATE-1 and FORK-EVIDENCE-1 (Core 08,
    /// FORK-ISOLATE 1–2, L2, L4, L5, L6), sampled over generated traces
    /// against the copying reference fork: main equals main run alone after
    /// every operation (non-interference); no fork reaches the outside world
    /// or records a real result although its inherited rows say `main` (no
    /// escape, decided by the pin); a fork exports only inherited records and
    /// never its receipt log; every operation changes only the instance it
    /// targets, so a fork is a snapshot of its base plus its own commits; and a
    /// new fork reads exactly its parent.
    #[test]
    fn forks_never_interfere_with_main_or_each_other(
        ops in proptest::collection::vec(fork_op(), 1..40)
    ) {
        let mut trace = ForkTrace::new();
        let mut images = trace.images();
        for op in ops {
            let target = trace.apply(op);
            let after = trace.images();
            for (instance, image) in &after {
                if !target.reaches(*instance) && let Some(before) = images.get(instance) {
                    prop_assert_eq!(image, before, "{:?} changed {:?}", op, instance);
                }
            }
            prop_assert_eq!(
                &after[&None],
                &trace.alone.store().image().expect("image"),
                "main diverged from main alone after {:?}", op
            );
            prop_assert_eq!(
                trace.executed.load(std::sync::atomic::Ordering::SeqCst),
                trace.alone_executed.load(std::sync::atomic::Ordering::SeqCst)
            );
            prop_assert_eq!(trace.escaped.load(std::sync::atomic::Ordering::SeqCst), 0);
            images = after;
        }
    }
}

// =========================================================================
// Round-2 adversarial regression tests for PR #1628 (Blockers 1, 2, 4, 5)
// =========================================================================

fn r2_put_fact<S: SqlEngine>(
    cell: &mut SqliteCell<norn_kernel::FactEvent, JsonObject, JsonObject, S>,
    object_type: &str,
    key: &str,
    values: serde_json::Value,
) {
    let fact_key = FactKey::new(object_type, key);
    let expected = cell.load(&fact_key).expect("load").version;
    let mut commit = CellCommit::empty();
    commit.writes.push(FactWrite {
        key: fact_key,
        expected,
        mutation: FactMutation::Put {
            values: serde_json::from_value(values).expect("values"),
        },
    });
    cell.commit(commit).expect("seed fact");
}

struct R2AllowAll;

impl norn_kernel::ActionAuthorizer for R2AllowAll {
    fn authorize(
        &self,
        _: &norn_kernel::ActionAuthzRequest<'_>,
    ) -> Result<(), norn_kernel::ActionDenial> {
        Ok(())
    }
    fn authorize_read(
        &self,
        _: &norn_kernel::ActionReadAuthzRequest<'_>,
    ) -> Result<(), norn_kernel::ActionDenial> {
        Ok(())
    }
    fn authorize_read_type(
        &self,
        _: &norn_kernel::ActionReadTypeAuthzRequest<'_>,
    ) -> Result<(), norn_kernel::ActionDenial> {
        Ok(())
    }
}

fn r2_turn(id: &str) -> norn_kernel::DecisionTurn {
    norn_kernel::DecisionTurn {
        id: id.to_owned(),
        now: LogicalTime::from_millis(1_000),
        world: norn_kernel::World::Main,
        consumes: None,
    }
}

fn r2_iface_fixture() -> (
    Model,
    SqliteCell<norn_kernel::FactEvent, JsonObject, JsonObject>,
    norn_kernel::FactSlice,
) {
    let definitions: Vec<norn_ir::ObjectTypeDef> = serde_json::from_value(json!([
        {
            "name": "iface.Node",
            "properties": [{"id": "id", "type": {"kind": "text"}}],
            "links": [{
                "id": "rel",
                "target": "iface.Party",
                "cardinality": {"to": "many", "from": "many"},
                "properties": [{"id": "note", "type": {"kind": "text"}}]
            }]
        },
        {"name": "iface.TargetA", "implements": ["iface.Party"], "properties": [{"id": "id", "type": {"kind": "text"}}]},
        {"name": "iface.TargetAChild", "extends": "iface.TargetA", "properties": []},
        {"name": "iface.TargetB", "implements": ["iface.Party"], "properties": [{"id": "id", "type": {"kind": "text"}}]}
    ]))
    .expect("definitions");
    let interface: norn_ir::InterfaceDef =
        serde_json::from_value(json!({"name": "iface.Party", "properties": []}))
            .expect("interface");
    let model =
        Model::from_definitions_with_interfaces(&definitions, &[], &[interface]).expect("model");
    let store = CellStore::open_in_memory(model.clone()).expect("store");
    let mut cell: SqliteCell<norn_kernel::FactEvent, JsonObject, JsonObject> = SqliteCell::new(
        store,
        "org_demo/dev",
        |cmd: &norn_kernel::Command<JsonObject>| cmd.payload.clone(),
    )
    .expect("cell");
    r2_put_fact(&mut cell, "iface.Node", "S1", json!({"id": "S1"}));
    r2_put_fact(&mut cell, "iface.TargetAChild", "K1", json!({"id": "K1"}));
    r2_put_fact(&mut cell, "iface.TargetB", "K1", json!({"id": "K1"}));
    r2_put_fact(&mut cell, "iface.TargetB", "K2", json!({"id": "K2"}));
    let slice = norn_kernel::load_slice(
        &cell,
        [
            FactKey::new("iface.Node", "S1"),
            FactKey::new("iface.TargetAChild", "K1"),
            FactKey::new("iface.TargetB", "K1"),
            FactKey::new("iface.TargetB", "K2"),
        ],
    )
    .expect("slice");
    (model, cell, slice)
}

/// Blocker 1: distinct subtypes implementing a shared target interface sharing
/// a raw target key can never collide, bypass CAS, leak values, or delete each
/// other's physical edge.
#[test]
fn r2_blocker1_interface_target_subtypes_unify_cas_and_physical_link_identity() {
    let (model, mut cell, slice) = r2_iface_fixture();
    let catalog = model.type_catalog();

    // 1. Add S1 --rel--> TargetAChild/K1.
    let add_a: norn_ir::DecisionOutput = serde_json::from_value(json!({
        "facts": [],
        "links": [{
            "op": "add", "sourceType": "iface.Node", "sourceKey": "S1",
            "link": "rel", "targetType": "iface.TargetAChild", "targetKey": "K1",
            "values": {"note": "from-a"}
        }],
        "commands": [], "timers": []
    }))
    .expect("add_a");
    norn_kernel::commit_decision(&mut cell, &catalog, &slice, &r2_turn("t1"), &add_a)
        .expect("add TargetAChild/K1");

    let key_a = LinkKey::new(
        FactKey::new("iface.Node", "S1"),
        "rel",
        FactKey::new("iface.TargetAChild", "K1"),
    );
    let key_b = LinkKey::new(
        FactKey::new("iface.Node", "S1"),
        "rel",
        FactKey::new("iface.TargetB", "K1"),
    );
    let state_a = cell.load_link(&key_a).expect("load_link A");
    assert_eq!(state_a.version, FactVersion(1));
    assert_eq!(
        state_a
            .values
            .as_ref()
            .and_then(|v| v.get("note"))
            .and_then(norn_ir::RuntimeValue::as_str),
        Some("from-a")
    );
    // Point lookup for TargetB/K1 must NOT return TargetA's values.
    let state_b = cell.load_link(&key_b).expect("load_link B");
    assert_eq!(state_b.version, FactVersion(0));
    assert!(state_b.values.is_none());

    // 2. Remove(TargetB/K1) and Add(TargetB/K1) must BOTH refuse and leave TargetAChild/K1 intact.
    for payload in [
        json!({"op": "remove", "sourceType": "iface.Node", "sourceKey": "S1", "link": "rel", "targetType": "iface.TargetB", "targetKey": "K1"}),
        json!({"op": "add", "sourceType": "iface.Node", "sourceKey": "S1", "link": "rel", "targetType": "iface.TargetB", "targetKey": "K1", "values": {"note": "clobber"}}),
    ] {
        let out: norn_ir::DecisionOutput = serde_json::from_value(
            json!({"facts": [], "links": [payload], "commands": [], "timers": []}),
        )
        .expect("out");
        assert!(
            norn_kernel::commit_decision(&mut cell, &catalog, &slice, &r2_turn("t2"), &out)
                .is_err()
        );
        let intact = cell.load_link(&key_a).expect("intact");
        assert_eq!(intact.version, FactVersion(1));
        assert_eq!(
            intact
                .values
                .as_ref()
                .and_then(|v| v.get("note"))
                .and_then(norn_ir::RuntimeValue::as_str),
            Some("from-a")
        );
    }

    // 3. Adding TargetB with a distinct key K2 succeeds alongside TargetAChild/K1.
    let add_b_k2: norn_ir::DecisionOutput = serde_json::from_value(json!({
        "facts": [],
        "links": [{"op": "add", "sourceType": "iface.Node", "sourceKey": "S1", "link": "rel", "targetType": "iface.TargetB", "targetKey": "K2", "values": {"note": "from-b"}}],
        "commands": [], "timers": []
    }))
    .expect("add_b_k2");
    norn_kernel::commit_decision(&mut cell, &catalog, &slice, &r2_turn("t4"), &add_b_k2)
        .expect("distinct target key succeeds");

    // 4. Same-hierarchy alias (TargetA/K1 for TargetAChild/K1) enforces CAS: stale v0 refuses.
    let mut stale_alias = CellCommit::empty();
    stale_alias.links.push(LinkWrite {
        key: LinkKey::new(
            FactKey::new("iface.Node", "S1"),
            "rel",
            FactKey::new("iface.TargetA", "K1"),
        ),
        expected: FactVersion(0),
        mutation: LinkMutation::Remove,
    });
    assert!(matches!(
        cell.commit(stale_alias),
        Err(CommitError::LinkConflict {
            expected: FactVersion(0),
            current: FactVersion(1),
            ..
        })
    ));
}

fn r2_action_call_type() -> norn_ir::ObjectTypeDef {
    let mut object: norn_ir::ObjectTypeDef =
        serde_json::from_value(json!({"name": norn_kernel::ACTION_CALL_TYPE})).expect("audit type");
    object.properties = [
        "actionId",
        "actionArtifactId",
        "actionVersion",
        "effectKind",
        "object",
        "input",
        "effectiveInput",
        "derivedValues",
        "preconditionOutcomes",
        "parameterPins",
        "ruleBinding",
        "trace",
        "actor",
        "turn",
        "transition",
        "from",
        "to",
        "rowOperations",
    ]
    .into_iter()
    .map(|id| norn_ir::Property {
        id: norn_ir::PropertyId(id.into()),
        description: None,
        property_type: norn_ir::PropertyType::JsonExt { schema: None },
        constraints: norn_ir::PropertyConstraints {
            nullable: true,
            ..norn_ir::PropertyConstraints::default()
        },
        default: None,
        markings: Vec::new(),
        computed: None,
        protected: false,
    })
    .collect();
    object
}

fn r2_ifc_fixture() -> (
    Model,
    SqliteCell<norn_kernel::FactEvent, JsonObject, JsonObject>,
) {
    let mut definitions: Vec<norn_ir::ObjectTypeDef> = serde_json::from_value(json!([
        {
            "name": "ifc.Secret",
            "properties": [
                {"id": "id", "type": {"kind": "text"}},
                {"id": "ssn", "type": {"kind": "text"}, "markings": ["pii.ssn"]}
            ]
        },
        {
            "name": "ifc.Public",
            "properties": [
                {"id": "id", "type": {"kind": "text"}},
                {"id": "status", "type": {"kind": "enum", "values": [{"value": "open"}, {"value": "done"}]}}
            ],
            "lifecycles": [{
                "property": "status",
                "initial": ["open"],
                "terminal": ["done"],
                "transitions": [
                    {"id": "finish", "from": ["open"], "to": "done"}
                ]
            }],
            "links": [{"id": "linked", "target": "ifc.Public", "cardinality": {"to": "many", "from": "many"}}]
        }
    ]))
    .expect("definitions");
    definitions.push(r2_action_call_type());
    let model = common::model_of(&definitions);
    let store = CellStore::open_in_memory(model.clone()).expect("store");
    let mut cell: SqliteCell<norn_kernel::FactEvent, JsonObject, JsonObject> = SqliteCell::new(
        store,
        "org_demo/dev",
        |cmd: &norn_kernel::Command<JsonObject>| cmd.payload.clone(),
    )
    .expect("cell");
    r2_put_fact(
        &mut cell,
        "ifc.Secret",
        "SEC1",
        json!({"id": "SEC1", "ssn": "123-45-6789"}),
    );
    r2_put_fact(
        &mut cell,
        "ifc.Public",
        "P1",
        json!({"id": "P1", "status": "open"}),
    );
    r2_put_fact(
        &mut cell,
        "ifc.Public",
        "P2",
        json!({"id": "P2", "status": "open"}),
    );
    (model, cell)
}

fn r2_ifc_leak_rule() -> norn_kernel::SynchronousRule {
    let rule_root: norn_ir::RuleRoot = serde_json::from_value(json!({
        "ruleId": "ifc.leak_rule",
        "ruleVersion": "2.0",
        "contract": "decide",
        "scopeRequired": ["orgId", "environmentId"],
        "inputSchema": {
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "slice": {
                    "type": "object",
                    "additionalProperties": false,
                    "properties": {
                        "secret": {"type": "object", "additionalProperties": false, "properties": {"id": {"type": "string"}, "ssn": {"type": "string"}}, "required": ["id", "ssn"]},
                        "target": {"type": "object", "additionalProperties": false, "properties": {"id": {"type": "string"}, "status": {"type": "string"}}, "required": ["id", "status"]},
                        "other": {"type": "object", "additionalProperties": false, "properties": {"id": {"type": "string"}, "status": {"type": "string"}}, "required": ["id", "status"]}
                    },
                    "required": ["target", "other"]
                },
                "event": {"type": "object", "additionalProperties": false, "properties": {"input": {"type": "object", "additionalProperties": false, "properties": {"id": {"type": "string"}, "p1": {"type": "string"}, "p2": {"type": "string"}, "sec": {"type": "string"}}, "required": ["id", "p1", "p2", "sec"]}, "actor": {"type": "string"}, "operationId": {"type": "string"}}, "required": ["input", "actor", "operationId"]}
            },
            "required": ["slice", "event"]
        },
        "outputSchema": {
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "facts": {"type": "array", "items": {"type": "object", "additionalProperties": false, "properties": {}}},
                "links": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "additionalProperties": false,
                        "properties": {
                            "op": {"type": "string", "enum": ["remove"]},
                            "sourceType": {"type": "string", "enum": ["ifc.Public"]},
                            "sourceKey": {"type": "string"},
                            "link": {"type": "string", "enum": ["linked"]},
                            "targetType": {"type": "string", "enum": ["ifc.Public"]},
                            "targetKey": {"type": "string"}
                        },
                        "required": ["op", "sourceType", "sourceKey", "link", "targetType", "targetKey"]
                    }
                },
                "commands": {"type": "array", "items": {"type": "object", "additionalProperties": false, "properties": {}}},
                "timers": {"type": "array", "items": {"type": "object", "additionalProperties": false, "properties": {}}}
            },
            "required": ["facts", "links", "commands", "timers"]
        },
        "graph": {
            "contentType": "application/vnd.gorules.decision",
            "nodes": [
                {"id": "in", "name": "input", "type": "inputNode"},
                {
                    "id": "decision", "name": "decision", "type": "decisionTableNode",
                    "content": {
                        "hitPolicy": "first",
                        "inputs": [{"id": "c", "name": "c", "type": "expression"}],
                        "outputs": [
                            {"id": "f", "name": "facts", "field": "facts"},
                            {"id": "l", "name": "links", "field": "links"},
                            {"id": "cm", "name": "commands", "field": "commands"},
                            {"id": "t", "name": "timers", "field": "timers"}
                        ],
                        "rules": [{
                            "_id": "r0", "c": "", "f": "[]",
                            "l": "[{op: 'remove', sourceType: 'ifc.Public', sourceKey: event.input.p1, link: 'linked', targetType: 'ifc.Public', targetKey: event.input.p2}]",
                            "cm": "[]", "t": "[]"
                        }]
                    }
                },
                {"id": "out", "name": "output", "type": "outputNode"}
            ],
            "edges": [
                {"id": "e0", "sourceId": "in", "targetId": "decision", "type": "edge"},
                {"id": "e1", "sourceId": "decision", "targetId": "out", "type": "edge"}
            ]
        }
    }))
    .expect("rule");
    norn_kernel::SynchronousRule::admit(
        norn_ir::ActionRuleRef {
            artifact_id: "ifc.leak_rule.rule".to_owned(),
            version: "1.0.0".to_owned(),
        },
        &rule_root,
        &std::collections::BTreeMap::new(),
        norn_kernel::DecisionBinding {
            profile_id: "norn.limits/1".to_owned(),
            environment_hash: "env-ifc".to_owned(),
        },
    )
    .expect("admit rule")
}

fn r2_ifc_link_output(op: &str, src: &str, dst: &str) -> norn_ir::DecisionOutput {
    serde_json::from_value(json!({
        "facts": [],
        "links": [{"op": op, "sourceType": "ifc.Public", "sourceKey": src, "link": "linked", "targetType": "ifc.Public", "targetKey": dst}],
        "commands": [], "timers": []
    }))
    .expect("link output")
}

/// Blocker 2a: IFC — a decision reading a classified fact cannot emit a dynamic
/// link (Add or Remove) between unmarked endpoints, even when creating endpoints.
#[test]
fn r2_blocker2a_classified_decision_cannot_emit_dynamic_link_between_unmarked_endpoints() {
    let (model, mut cell) = r2_ifc_fixture();
    let catalog = model.type_catalog();
    let classified_slice = norn_kernel::load_slice(
        &cell,
        [
            FactKey::new("ifc.Secret", "SEC1"),
            FactKey::new("ifc.Public", "P1"),
            FactKey::new("ifc.Public", "P2"),
        ],
    )
    .expect("slice");
    let add_link = r2_ifc_link_output("add", "P1", "P2");
    let refusal = norn_kernel::commit_decision(
        &mut cell,
        &catalog,
        &classified_slice,
        &r2_turn("t1"),
        &add_link,
    )
    .expect_err("classified decision must not link unmarked endpoints");
    assert!(matches!(
        refusal,
        norn_kernel::DecisionRefusal::Host(CellError::MarkingDeclassificationInvalid(_))
    ));
    let edge = LinkKey::new(
        FactKey::new("ifc.Public", "P1"),
        "linked",
        FactKey::new("ifc.Public", "P2"),
    );
    assert!(!cell.load_link(&edge).expect("load_link").exists());

    let public_slice = norn_kernel::load_slice(
        &cell,
        [
            FactKey::new("ifc.Public", "P1"),
            FactKey::new("ifc.Public", "P2"),
        ],
    )
    .expect("public");
    norn_kernel::commit_decision(
        &mut cell,
        &catalog,
        &public_slice,
        &r2_turn("t2"),
        &add_link,
    )
    .expect("unclassified add");
    assert!(cell.load_link(&edge).expect("exists").exists());

    let remove_link = r2_ifc_link_output("remove", "P1", "P2");
    let refusal = norn_kernel::commit_decision(
        &mut cell,
        &catalog,
        &classified_slice,
        &r2_turn("t3"),
        &remove_link,
    )
    .expect_err("classified decision must not remove link between unmarked endpoints");
    assert!(matches!(
        refusal,
        norn_kernel::DecisionRefusal::Host(CellError::MarkingDeclassificationInvalid(_))
    ));
    assert!(cell.load_link(&edge).expect("still exists").exists());

    let create_and_link: norn_ir::DecisionOutput = serde_json::from_value(json!({
        "facts": [
            {"op": "create", "objectType": "ifc.Public", "key": "C1", "values": {"id": "C1", "status": "open"}},
            {"op": "create", "objectType": "ifc.Public", "key": "C2", "values": {"id": "C2", "status": "open"}}
        ],
        "links": [{"op": "add", "sourceType": "ifc.Public", "sourceKey": "C1", "link": "linked", "targetType": "ifc.Public", "targetKey": "C2"}],
        "commands": [], "timers": []
    }))
    .expect("create_and_link");
    let create_slice = norn_kernel::load_slice(
        &cell,
        [
            FactKey::new("ifc.Secret", "SEC1"),
            FactKey::new("ifc.Public", "C1"),
            FactKey::new("ifc.Public", "C2"),
        ],
    )
    .expect("slice");
    let refusal = norn_kernel::commit_decision(
        &mut cell,
        &catalog,
        &create_slice,
        &r2_turn("t5"),
        &create_and_link,
    )
    .expect_err("classified decision must still refuse dynamic link materialization");
    assert!(matches!(
        refusal,
        norn_kernel::DecisionRefusal::Host(CellError::MarkingDeclassificationInvalid(_))
    ));
}

/// Blocker 2b: IFC — `commit_action` with `synchronousRule` reading a classified
/// fact in its slice cannot emit dynamic links between unmarked endpoints.
#[test]
fn r2_blocker2b_classified_action_cannot_emit_dynamic_link_between_unmarked_endpoints() {
    let (model, mut cell) = r2_ifc_fixture();
    let catalog = model.type_catalog();
    let public_slice = norn_kernel::load_slice(
        &cell,
        [
            FactKey::new("ifc.Public", "P1"),
            FactKey::new("ifc.Public", "P2"),
        ],
    )
    .expect("public");
    let add_link: norn_ir::DecisionOutput = serde_json::from_value(json!({
        "facts": [],
        "links": [{"op": "add", "sourceType": "ifc.Public", "sourceKey": "P1", "link": "linked", "targetType": "ifc.Public", "targetKey": "P2"}],
        "commands": [], "timers": []
    }))
    .expect("add");
    norn_kernel::commit_decision(
        &mut cell,
        &catalog,
        &public_slice,
        &r2_turn("t1"),
        &add_link,
    )
    .expect("seed link");

    let sync_rule = r2_ifc_leak_rule();
    let action: norn_ir::ActionRoot = serde_json::from_value(json!({
        "actionId": "ifc.leak_action",
        "scopeRequired": ["orgId", "environmentId"],
        "idempotent": true,
        "ledgered": true,
        "inputSchema": {
            "type": "object",
            "additionalProperties": false,
            "properties": {"id": {"type": "string"}, "p1": {"type": "string"}, "p2": {"type": "string"}, "sec": {"type": "string"}},
            "required": ["id", "p1", "p2", "sec"]
        },
        "outputSchema": {"type": "object", "additionalProperties": false, "properties": {}},
        "slice": [
            {"name": "secret", "objectType": "ifc.Secret", "key": {"property": {"from": "input", "property": "sec"}}},
            {"name": "target", "objectType": "ifc.Public", "key": {"property": {"from": "input", "property": "p1"}}},
            {"name": "other", "objectType": "ifc.Public", "key": {"property": {"from": "input", "property": "p2"}}}
        ],
        "synchronousRule": {"artifactId": "ifc.leak_rule.rule", "version": "1.0.0"},
        "writes": [{"entityKind": "ifc.Public", "change": "modified"}],
        "effectProgram": {
            "kind": "fire_transition",
            "objectIdField": "id",
            "objectKind": "ifc.Public",
            "entityArtifactId": "ifc.Public.entity",
            "entityArtifactVersion": "1.0.0",
            "lifecycle": "status",
            "transition": "finish"
        }
    }))
    .expect("action");
    let action_catalog = model
        .type_catalog()
        .with_action_rules(std::slice::from_ref(&sync_rule));
    let request = norn_kernel::ActionCall {
        action: &action,
        action_artifact_id: "ifc.leak_action.action",
        action_version: "1.0.0",
        scope: std::collections::BTreeMap::from([
            ("orgId".to_owned(), "org_demo".to_owned()),
            ("environmentId".to_owned(), "dev".to_owned()),
        ]),
        actor: "operator".to_owned(),
        input: serde_json::from_value(json!({"id": "P1", "p1": "P1", "p2": "P2", "sec": "SEC1"}))
            .expect("input"),
        idempotency_key: "ifc-act-1".to_owned(),
        object_type: None,
        turn: r2_turn("t4"),
        trace: None,
    };
    let refusal = norn_kernel::commit_action(&mut cell, &action_catalog, &request, &R2AllowAll)
        .expect_err("action reading Secret must not remove link between unmarked endpoints");
    assert!(
        matches!(
            refusal,
            norn_kernel::ActionRefusal::Decision(norn_kernel::DecisionRefusal::Host(
                CellError::MarkingDeclassificationInvalid(_)
            ))
        ),
        "expected MarkingDeclassificationInvalid, got {refusal:?}"
    );
    let edge = LinkKey::new(
        FactKey::new("ifc.Public", "P1"),
        "linked",
        FactKey::new("ifc.Public", "P2"),
    );
    assert!(cell.load_link(&edge).expect("edge preserved").exists());
}

/// Blocker 4: `StoreReader::link` / `SqliteCell::load_link` executes a direct
/// SQL point lookup (`WHERE _world = ?1 AND _source = ?2 AND _target = ?3`)
/// without scanning unrelated targets.
#[test]
fn r2_blocker4_load_link_uses_point_sql_lookup_without_scanning_unrelated_targets() {
    use std::cell::RefCell;
    use std::rc::Rc;

    #[derive(Debug)]
    struct SpyEngine {
        inner: NativeEngine,
        queries: Rc<RefCell<Vec<String>>>,
    }

    impl SqlEngine for SpyEngine {
        fn read_isolation(&self) -> norn_cell::engine::ReadIsolation {
            self.inner.read_isolation()
        }
        fn read_cache_revision(&self) -> Result<Option<(u64, u64)>, CellError> {
            self.inner.read_cache_revision()
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
            self.queries.borrow_mut().push(sql.to_owned());
            self.inner.query(sql, params)
        }
        fn execute_batch(&self, sql: &str) -> Result<(), CellError> {
            self.inner.execute_batch(sql)
        }
        fn transaction<T, X: From<CellError>>(
            &self,
            f: impl FnOnce() -> Result<T, X>,
        ) -> Result<T, X> {
            self.inner.transaction(f)
        }
    }

    let queries = Rc::new(RefCell::new(Vec::new()));
    let spy = SpyEngine {
        inner: NativeEngine::open_in_memory().expect("engine"),
        queries: Rc::clone(&queries),
    };
    let store = CellStore::from_engine(spy, law_model()).expect("store");
    let mut cell: SqliteCell<norn_kernel::FactEvent, JsonObject, JsonObject, SpyEngine> =
        SqliteCell::new(
            store,
            "org_demo/dev",
            |cmd: &norn_kernel::Command<JsonObject>| cmd.payload.clone(),
        )
        .expect("cell");

    r2_put_fact(&mut cell, "law.Thing", "S", json!({"v": 0}));
    let mut link_commit = CellCommit::empty();
    for idx in 0..40 {
        let tid = format!("T{idx:02}");
        r2_put_fact(&mut cell, "law.Thing", &tid, json!({"v": idx}));
        link_commit.links.push(LinkWrite {
            key: LinkKey::new(
                FactKey::new("law.Thing", "S"),
                "rel",
                FactKey::new("law.Thing", tid),
            ),
            expected: FactVersion(0),
            mutation: LinkMutation::Add {
                values: JsonObject::new(),
            },
        });
    }
    cell.commit(link_commit).expect("40 links");

    queries.borrow_mut().clear();
    let hit = cell
        .load_link(&LinkKey::new(
            FactKey::new("law.Thing", "S"),
            "rel",
            FactKey::new("law.Thing", "T25"),
        ))
        .expect("hit");
    assert_eq!(hit.version, FactVersion(1));
    assert!(hit.exists());
    let miss = cell
        .load_link(&LinkKey::new(
            FactKey::new("law.Thing", "S"),
            "rel",
            FactKey::new("law.Thing", "T99"),
        ))
        .expect("miss");
    assert_eq!(miss.version, FactVersion(0));
    assert!(!miss.exists());

    let link_table_queries: Vec<String> = queries
        .borrow()
        .iter()
        .filter(|sql| sql.contains("\"_source\"") && !sql.contains("_norn_link_version"))
        .cloned()
        .collect();
    assert!(
        !link_table_queries.is_empty()
            && link_table_queries
                .iter()
                .all(|sql| sql.contains("\"_target\" = ?3")),
        "every link table lookup during load_link must filter by _target = ?3; got {link_table_queries:?}"
    );
}

/// Blocker 5: nullable properties are represented identically by SQL predicate
/// reads and point reads so reading the same row through BOTH paths in one
/// action reconciles cleanly without `StalePreconditionRead`.
#[test]
fn r2_blocker5_nullable_row_reconciles_across_predicate_and_point_slice_reads() {
    let mut definitions: Vec<norn_ir::ObjectTypeDef> = serde_json::from_value(json!([{
        "name": "doc.Task",
        "properties": [
            {"id": "task_id", "type": {"kind": "text"}},
            {"id": "group", "type": {"kind": "text"}},
            {"id": "note", "type": {"kind": "text"}, "constraints": {"nullable": true}}
        ]
    }]))
    .expect("definitions");
    definitions.push(r2_action_call_type());
    let model = common::model_of(&definitions);
    let store = CellStore::open_in_memory(model.clone()).expect("store");
    let mut cell: SqliteCell<norn_kernel::FactEvent, JsonObject, JsonObject> = SqliteCell::new(
        store,
        "org_demo/dev",
        |cmd: &norn_kernel::Command<JsonObject>| cmd.payload.clone(),
    )
    .expect("cell");

    r2_put_fact(
        &mut cell,
        "doc.Task",
        "T1",
        json!({"task_id": "T1", "group": "alpha", "note": null}),
    );

    // Confirm canonical equality between point read and predicate read.
    let point = cell
        .load_bounded(&FactKey::new("doc.Task", "T1"), 65_536)
        .expect("point")
        .expect("fits");
    let predicate: PredicateQuery =
        serde_json::from_value(json!({"objectType": "doc.Task", "filter": {"group": "alpha"}}))
            .expect("predicate");
    let matches = cell
        .read_predicate(&predicate, 10, 65_536)
        .expect("read_predicate")
        .expect("admitted");
    assert_eq!(matches.len(), 1);
    assert_eq!(point.state.values.as_ref(), Some(&matches[0].values));
    assert_eq!(
        matches[0].values.get("note"),
        Some(&norn_ir::RuntimeValue::Null)
    );

    // Run an action that reads T1 via BOTH SliceKey::Predicate AND point SliceKey::Input.
    let action: norn_ir::ActionRoot = serde_json::from_value(json!({
        "actionId": "doc.task.reconcile",
        "scopeRequired": ["orgId", "environmentId"],
        "idempotent": true,
        "ledgered": true,
        "inputSchema": {
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "task_id": {"type": "string"},
                "group": {"type": "string"},
                "note": {"type": "string"}
            },
            "required": ["task_id", "group", "note"]
        },
        "outputSchema": {"type": "object", "additionalProperties": false, "properties": {}},
        "slice": [
            {"name": "by_group", "objectType": "doc.Task", "key": {"predicate": {"filter": {"group": "group"}, "maxItems": 10}}},
            {"name": "by_id", "objectType": "doc.Task", "key": {"property": {"from": "input", "property": "task_id"}}}
        ],
        "preconditions": ["len(slice.by_group) == 1"],
        "writes": [{"entityKind": "doc.Task", "change": "modified"}],
        "effectProgram": {
            "kind": "upsert_input_object",
            "objectIdField": "task_id",
            "objectKind": "doc.Task",
            "entityArtifactId": "doc.Task.entity",
            "entityArtifactVersion": "1.0.0"
        }
    }))
    .expect("action");
    let request = norn_kernel::ActionCall {
        action: &action,
        action_artifact_id: "doc.task.reconcile.action",
        action_version: "1.0.0",
        scope: std::collections::BTreeMap::from([
            ("orgId".to_owned(), "org_demo".to_owned()),
            ("environmentId".to_owned(), "dev".to_owned()),
        ]),
        actor: "operator".to_owned(),
        input: serde_json::from_value(json!({"task_id": "T1", "group": "alpha", "note": "ok"}))
            .expect("input"),
        idempotency_key: "reconcile-1".to_owned(),
        object_type: None,
        turn: norn_kernel::DecisionTurn {
            id: "turn-reconcile".to_owned(),
            now: LogicalTime::from_millis(2_000),
            world: norn_kernel::World::Main,
            consumes: None,
        },
        trace: None,
    };
    let catalog = model.type_catalog();
    let outcome = norn_kernel::commit_action(&mut cell, &catalog, &request, &R2AllowAll)
        .expect("nullable row must reconcile cleanly across predicate and point reads");
    assert!(matches!(
        outcome,
        norn_kernel::ActionOutcome::Committed { .. }
    ));
}

fn r3_blocker1_iface_model() -> (Model, String) {
    let iface: norn_ir::InterfaceDef = serde_json::from_value(json!({
        "name": "iface.Target",
        "properties": [{"id": "id", "type": {"kind": "text"}}]
    }))
    .expect("interface");
    let defs: Vec<norn_ir::ObjectTypeDef> = serde_json::from_value(json!([
        {"name": "iface.Alpha", "implements": ["iface.Target"], "properties": [{"id": "id", "type": {"kind": "text"}}]},
        {"name": "iface.Beta", "implements": ["iface.Target"], "properties": [{"id": "id", "type": {"kind": "text"}}]},
        {"name": "iface.Source", "properties": [{"id": "id", "type": {"kind": "text"}}], "links": [{"id": "ref_target", "target": "iface.Target", "cardinality": {"to": "many", "from": "many"}}]}
    ]))
    .expect("definitions");
    let model = Model::from_definitions_with_interfaces(&defs, &[], &[iface]).expect("model");
    let table_name = model
        .schema()
        .link_tables
        .iter()
        .find(|table| {
            table.source.as_str() == "iface.Source" && table.link.as_str() == "ref_target"
        })
        .expect("link table")
        .table
        .clone();
    (model, table_name)
}

/// R3 Blocker 1: a legacy physical link row lacking `_norn_link_version`
/// metadata on an interface-target link is isolated across unrelated target
/// hierarchies and fails closed when multiple implementing roots share the key.
#[test]
fn r3_blocker1_legacy_link_row_without_version_metadata_is_isolated_across_interface_targets() {
    let (model, table_name) = r3_blocker1_iface_model();
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("legacy.sqlite");
    let store = CellStore::open(&path, model, StoreOptions::default()).expect("store");
    let mut cell: SqliteCell<norn_kernel::FactEvent, JsonObject, JsonObject> = SqliteCell::new(
        store,
        "org_demo/dev",
        |cmd: &norn_kernel::Command<JsonObject>| cmd.payload.clone(),
    )
    .expect("cell");

    r2_put_fact(&mut cell, "iface.Source", "S1", json!({"id": "S1"}));
    r2_put_fact(&mut cell, "iface.Alpha", "K1", json!({"id": "K1"}));

    let raw = NativeEngine::open(&path, StoreOptions::default()).expect("raw engine");
    raw.execute(
        &format!(
            "INSERT INTO \"{table_name}\" (\"_world\", \"_source\", \"_target\") VALUES ('main', 'S1', 'K1')"
        ),
        &[],
    )
    .expect("legacy row");

    let alpha_edge = LinkKey::new(
        FactKey::new("iface.Source", "S1"),
        "ref_target",
        FactKey::new("iface.Alpha", "K1"),
    );
    let beta_edge = LinkKey::new(
        FactKey::new("iface.Source", "S1"),
        "ref_target",
        FactKey::new("iface.Beta", "K1"),
    );

    assert!(cell.load_link(&alpha_edge).expect("alpha").exists());
    assert!(!cell.load_link(&beta_edge).expect("beta").exists());

    r2_put_fact(&mut cell, "iface.Beta", "K1", json!({"id": "K1"}));
    assert!(
        !cell
            .load_link(&alpha_edge)
            .expect("ambiguous alpha")
            .exists()
    );
    assert!(!cell.load_link(&beta_edge).expect("ambiguous beta").exists());

    for mutation in [
        LinkMutation::Add {
            values: JsonObject::new(),
        },
        LinkMutation::Remove,
    ] {
        let commit = CellCommit {
            links: vec![LinkWrite {
                key: beta_edge.clone(),
                expected: FactVersion(0),
                mutation,
            }],
            ..CellCommit::empty()
        };
        assert!(matches!(
            cell.commit(commit),
            Err(CommitError::EffectiveValues { .. })
        ));
    }
}

/// R3 Blocker 2: an action whose fresh aggregate precondition reads a
/// classified object (with NO point/predicate slice entry for the secret)
/// cannot emit a dynamic link between unmarked endpoints.
#[test]
fn r3_blocker2_classified_aggregate_precondition_cannot_emit_dynamic_link() {
    let (model, mut cell) = r2_ifc_fixture();
    let catalog = model.type_catalog();
    let public_slice = norn_kernel::load_slice(
        &cell,
        [
            FactKey::new("ifc.Public", "P1"),
            FactKey::new("ifc.Public", "P2"),
        ],
    )
    .expect("public");
    norn_kernel::commit_decision(
        &mut cell,
        &catalog,
        &public_slice,
        &r2_turn("t1"),
        &r2_ifc_link_output("add", "P1", "P2"),
    )
    .expect("seed link");

    let sync_rule = r2_ifc_leak_rule();
    let action: norn_ir::ActionRoot = serde_json::from_value(json!({
        "actionId": "ifc.agg_leak_action",
        "scopeRequired": ["orgId", "environmentId"],
        "idempotent": true,
        "ledgered": true,
        "inputSchema": {
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "id": {"type": "string"},
                "p1": {"type": "string"},
                "p2": {"type": "string"},
                "sec": {"type": "string"}
            },
            "required": ["id", "p1", "p2", "sec"]
        },
        "outputSchema": {"type": "object", "additionalProperties": false, "properties": {}},
        "slice": [
            {"name": "target", "objectType": "ifc.Public", "key": {"property": {"from": "input", "property": "p1"}}},
            {"name": "other", "objectType": "ifc.Public", "key": {"property": {"from": "input", "property": "p2"}}}
        ],
        "aggregates": [{
            "name": "secret_count",
            "objectType": "ifc.Secret",
            "filter": {"id": "sec"},
            "aggregate": {"kind": "count"},
            "maxItems": 10
        }],
        "preconditions": ["aggregates.secret_count >= 1"],
        "synchronousRule": {"artifactId": "ifc.leak_rule.rule", "version": "1.0.0"},
        "writes": [{"entityKind": "ifc.Public", "change": "modified"}],
        "effectProgram": {
            "kind": "fire_transition",
            "objectIdField": "id",
            "objectKind": "ifc.Public",
            "entityArtifactId": "ifc.Public.entity",
            "entityArtifactVersion": "1.0.0",
            "lifecycle": "status",
            "transition": "finish"
        }
    }))
    .expect("action");
    let action_catalog = model
        .type_catalog()
        .with_action_rules(std::slice::from_ref(&sync_rule));
    let request = norn_kernel::ActionCall {
        action: &action,
        action_artifact_id: "ifc.agg_leak_action.action",
        action_version: "1.0.0",
        scope: std::collections::BTreeMap::from([
            ("orgId".to_owned(), "org_demo".to_owned()),
            ("environmentId".to_owned(), "dev".to_owned()),
        ]),
        actor: "operator".to_owned(),
        input: serde_json::from_value(json!({"id": "P1", "p1": "P1", "p2": "P2", "sec": "SEC1"}))
            .expect("input"),
        idempotency_key: "ifc-agg-act-1".to_owned(),
        object_type: None,
        turn: r2_turn("t2"),
        trace: None,
    };
    let refusal = norn_kernel::commit_action(&mut cell, &action_catalog, &request, &R2AllowAll)
        .expect_err("classified aggregate precondition must refuse dynamic link");
    assert!(
        matches!(
            refusal,
            norn_kernel::ActionRefusal::Decision(norn_kernel::DecisionRefusal::Host(
                CellError::MarkingDeclassificationInvalid(_)
            ))
        ),
        "expected MarkingDeclassificationInvalid, got {refusal:?}"
    );
    let edge = LinkKey::new(
        FactKey::new("ifc.Public", "P1"),
        "linked",
        FactKey::new("ifc.Public", "P2"),
    );
    assert!(cell.load_link(&edge).expect("edge preserved").exists());
}

struct R3HideRow3;
impl norn_kernel::ActionAuthorizer for R3HideRow3 {
    fn authorize(
        &self,
        _: &norn_kernel::ActionAuthzRequest<'_>,
    ) -> Result<(), norn_kernel::ActionDenial> {
        Ok(())
    }
    fn authorize_read(
        &self,
        request: &norn_kernel::ActionReadAuthzRequest<'_>,
    ) -> Result<(), norn_kernel::ActionDenial> {
        if request.target.key == "R3" {
            return Err(norn_kernel::ActionDenial::Invisible);
        }
        Ok(())
    }
    fn authorize_read_type(
        &self,
        _: &norn_kernel::ActionReadTypeAuthzRequest<'_>,
    ) -> Result<(), norn_kernel::ActionDenial> {
        Ok(())
    }
}

fn r3_blocker4_sqlite_actions() -> [norn_ir::ActionRoot; 2] {
    let make = |id: &str, extra: serde_json::Value| -> norn_ir::ActionRoot {
        let mut obj = json!({
            "actionId": id,
            "scopeRequired": ["orgId", "environmentId"],
            "idempotent": true,
            "ledgered": true,
            "inputSchema": {
                "type": "object",
                "additionalProperties": false,
                "properties": {"item_id": {"type": "string"}, "grp": {"type": "string"}},
                "required": ["item_id", "grp"]
            },
            "outputSchema": {"type": "object", "additionalProperties": false, "properties": {}},
            "writes": [{"entityKind": "doc.Item", "change": "created"}],
            "effectProgram": {
                "kind": "upsert_input_object",
                "objectIdField": "item_id",
                "objectKind": "doc.Item",
                "entityArtifactId": "doc.Item.entity",
                "entityArtifactVersion": "1.0.0"
            }
        });
        for (k, v) in extra.as_object().expect("object") {
            obj.as_object_mut()
                .expect("object")
                .insert(k.clone(), v.clone());
        }
        serde_json::from_value(obj).expect("action")
    };
    [
        make(
            "doc.item.pred_check",
            json!({"slice": [{"name": "rows", "objectType": "doc.Item", "key": {"predicate": {"filter": {"grp": "grp"}, "maxItems": 1}}}]}),
        ),
        make(
            "doc.item.agg_check",
            json!({
                "aggregates": [{"name": "cnt", "objectType": "doc.Item", "filter": {"grp": "grp"}, "aggregate": {"kind": "count"}, "maxItems": 1}],
                "preconditions": ["aggregates.cnt <= 1"]
            }),
        ),
    ]
}

/// R3 Blocker 4 (SqliteCell): with `maxItems = 1` and `[readable_1, readable_2, hidden_3]`,
/// both `SliceKey::Predicate` and fresh `ActionAggregate` conceal via
/// `ActionRefusal::TargetNotFound` rather than leaking cardinality via bound errors.
#[test]
fn r3_blocker4_sqlite_hidden_row_past_max_items_plus_one_conceals_as_target_not_found() {
    let mut defs: Vec<norn_ir::ObjectTypeDef> = serde_json::from_value(json!([{
        "name": "doc.Item",
        "properties": [
            {"id": "item_id", "type": {"kind": "text"}},
            {"id": "grp", "type": {"kind": "text"}}
        ]
    }]))
    .expect("defs");
    defs.push(r2_action_call_type());
    let model = common::model_of(&defs);
    let store = CellStore::open_in_memory(model.clone()).expect("store");
    let mut cell: SqliteCell<norn_kernel::FactEvent, JsonObject, JsonObject> = SqliteCell::new(
        store,
        "org_demo/dev",
        |cmd: &norn_kernel::Command<JsonObject>| cmd.payload.clone(),
    )
    .expect("cell");
    for id in ["R1", "R2", "R3"] {
        r2_put_fact(
            &mut cell,
            "doc.Item",
            id,
            json!({"item_id": id, "grp": "g"}),
        );
    }
    let actions = r3_blocker4_sqlite_actions();
    let catalog = model.type_catalog();
    for (idx, action) in actions.iter().enumerate() {
        let request = norn_kernel::ActionCall {
            action,
            action_artifact_id: if idx == 0 {
                "doc.item.pred_check.action"
            } else {
                "doc.item.agg_check.action"
            },
            action_version: "1.0.0",
            scope: std::collections::BTreeMap::from([
                ("orgId".to_owned(), "org_demo".to_owned()),
                ("environmentId".to_owned(), "dev".to_owned()),
            ]),
            actor: "operator".to_owned(),
            input: serde_json::from_value(json!({"item_id": format!("W{idx}"), "grp": "g"}))
                .expect("input"),
            idempotency_key: format!("r3-b4-sqlite-{idx}"),
            object_type: None,
            turn: r2_turn(&format!("t-b4-{idx}")),
            trace: None,
        };
        let refusal = norn_kernel::commit_action(&mut cell, &catalog, &request, &R3HideRow3)
            .expect_err("hidden row past maxItems + 1 must conceal");
        assert!(
            matches!(refusal, norn_kernel::ActionRefusal::TargetNotFound),
            "case {idx}: expected TargetNotFound, got {refusal:?}"
        );
    }
}

#[derive(Debug)]
struct R3SpyEngine {
    inner: NativeEngine,
    queries: std::rc::Rc<std::cell::RefCell<Vec<String>>>,
}

impl SqlEngine for R3SpyEngine {
    fn read_isolation(&self) -> norn_cell::engine::ReadIsolation {
        self.inner.read_isolation()
    }
    fn read_cache_revision(&self) -> Result<Option<(u64, u64)>, CellError> {
        self.inner.read_cache_revision()
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
        self.queries.borrow_mut().push(sql.to_owned());
        self.inner.query(sql, params)
    }
    fn execute_batch(&self, sql: &str) -> Result<(), CellError> {
        self.inner.execute_batch(sql)
    }
    fn transaction<T, X: From<CellError>>(&self, f: impl FnOnce() -> Result<T, X>) -> Result<T, X> {
        self.inner.transaction(f)
    }
}

/// R3 Blocker 5: incoming `from: one` cardinality checks use bounded SQL
/// (`WHERE "_world" = ?1 AND "_target" = ?2 ORDER BY "_source" LIMIT ?3`)
/// without loading full endpoint adjacency.
#[test]
fn r3_blocker5_from_one_cardinality_uses_bounded_sql_proof() {
    let defs: Vec<norn_ir::ObjectTypeDef> = serde_json::from_value(json!([
        {"name": "cap.Slot", "properties": [{"id": "id", "type": {"kind": "text"}}]},
        {"name": "cap.Owner", "properties": [{"id": "id", "type": {"kind": "text"}}], "links": [{"id": "owns", "target": "cap.Slot", "cardinality": {"to": "many", "from": "one"}}]}
    ]))
    .expect("defs");
    let queries = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let spy = R3SpyEngine {
        inner: NativeEngine::open_in_memory().expect("engine"),
        queries: std::rc::Rc::clone(&queries),
    };
    let store = CellStore::from_engine(spy, common::model_of(&defs)).expect("store");
    let mut cell: SqliteCell<norn_kernel::FactEvent, JsonObject, JsonObject, R3SpyEngine> =
        SqliteCell::new(
            store,
            "org_demo/dev",
            |cmd: &norn_kernel::Command<JsonObject>| cmd.payload.clone(),
        )
        .expect("cell");

    r2_put_fact(&mut cell, "cap.Owner", "O1", json!({"id": "O1"}));
    r2_put_fact(&mut cell, "cap.Owner", "O2", json!({"id": "O2"}));
    r2_put_fact(&mut cell, "cap.Slot", "T1", json!({"id": "T1"}));

    let add_link = |owner: &str| CellCommit {
        links: vec![LinkWrite {
            key: LinkKey::new(
                FactKey::new("cap.Owner", owner),
                "owns",
                FactKey::new("cap.Slot", "T1"),
            ),
            expected: FactVersion(0),
            mutation: LinkMutation::Add {
                values: JsonObject::new(),
            },
        }],
        ..CellCommit::empty()
    };
    cell.commit(add_link("O1")).expect("first owner succeeds");

    queries.borrow_mut().clear();
    assert!(matches!(
        cell.commit(add_link("O2")),
        Err(CommitError::LinkCardinality { .. })
    ));

    let recorded = queries.borrow();
    assert!(
        recorded.iter().any(|sql| {
            sql.contains("INDEXED BY")
                && sql.contains("\"_target\" = ?2")
                && sql.contains("LIMIT ?3")
                && !sql.contains("ORDER BY")
        }),
        "expected indexed bounded incoming LIMIT query without ORDER BY; got {recorded:?}"
    );
    assert!(
        !recorded.iter().any(|sql| {
            sql.contains("\"_target\" = ?2")
                && !sql.contains("LIMIT")
                && !sql.contains("_norn_link_version")
        }),
        "must not execute unbounded incoming link scan; got {recorded:?}"
    );
}

/// R4 Blocker 1: an ordinary embedded decision passing a pending `mapping-bundle:`
/// observation ID in `DecisionTurn.consumes` with classified point or predicate
/// reads MUST still fail closed in `check_link_read_markings`.
#[test]
fn r4_blocker1_decision_spoofing_mapping_bundle_observation_still_refuses_classified_link() {
    let (model, mut cell) = r2_ifc_fixture();
    let catalog = model.type_catalog();
    let spoof_id = norn_kernel::ObservationId("mapping-bundle:spoof-digest".to_owned());
    cell.inject(&Observed {
        id: spoof_id.clone(),
        provenance: norn_kernel::Provenance {
            source: "external".to_owned(),
            source_id: "spoof-1".to_owned(),
            observed_at: LogicalTime::from_millis(500),
            mapping_version: "1.0.0".to_owned(),
        },
        input: JsonObject::new(),
    })
    .expect("inject pending mapping-bundle observation");

    let spoof_turn = norn_kernel::DecisionTurn {
        id: "t-spoof".to_owned(),
        now: LogicalTime::from_millis(1_000),
        world: norn_kernel::World::Main,
        consumes: Some(Consumed::Observation {
            id: spoof_id.clone(),
        }),
    };
    let classified_slice = norn_kernel::load_slice(
        &cell,
        [
            FactKey::new("ifc.Secret", "SEC1"),
            FactKey::new("ifc.Public", "P1"),
            FactKey::new("ifc.Public", "P2"),
        ],
    )
    .expect("slice");
    let refusal = norn_kernel::commit_decision(
        &mut cell,
        &catalog,
        &classified_slice,
        &spoof_turn,
        &r2_ifc_link_output("add", "P1", "P2"),
    )
    .expect_err("decision spoofing mapping-bundle observation must refuse");
    assert!(
        matches!(
            refusal,
            norn_kernel::DecisionRefusal::Host(CellError::MarkingDeclassificationInvalid(_))
        ),
        "expected MarkingDeclassificationInvalid, got {refusal:?}"
    );

    // Also verify a commit carrying classified predicate_reads + spoofed consumes.
    let predicate: PredicateQuery =
        serde_json::from_value(json!({"objectType": "ifc.Secret", "filter": {"id": "SEC1"}}))
            .expect("predicate");
    let pred_commit = CellCommit {
        consumes: Some(Consumed::Observation { id: spoof_id }),
        predicate_reads: vec![norn_kernel::PredicateReadExpectation {
            predicate,
            members: vec![norn_kernel::PredicateReadMember {
                key: FactKey::new("ifc.Secret", "SEC1"),
                version: FactVersion(1),
            }],
        }],
        links: vec![LinkWrite {
            key: LinkKey::new(
                FactKey::new("ifc.Public", "P1"),
                "linked",
                FactKey::new("ifc.Public", "P2"),
            ),
            expected: FactVersion(0),
            mutation: LinkMutation::Add {
                values: JsonObject::new(),
            },
        }],
        ..CellCommit::empty()
    };
    assert!(matches!(
        cell.commit(pred_commit),
        Err(CommitError::Host(
            CellError::MarkingDeclassificationInvalid(_)
        ))
    ));
    let edge = LinkKey::new(
        FactKey::new("ifc.Public", "P1"),
        "linked",
        FactKey::new("ifc.Public", "P2"),
    );
    assert!(!cell.load_link(&edge).expect("no link written").exists());
}

/// R4 Blocker 2: reusing a tombstoned physical link slot in the same physical
/// table for a different target root is refused so the prior target root can
/// never re-issue version 1 and let a stale CAS pass.
#[test]
fn r4_blocker2_tombstoned_physical_slot_rejects_cross_root_reuse_and_prevents_version_replay() {
    let (_model, mut cell, _slice) = r2_iface_fixture();
    let key_a = LinkKey::new(
        FactKey::new("iface.Node", "S1"),
        "rel",
        FactKey::new("iface.TargetAChild", "K1"),
    );
    let key_b = LinkKey::new(
        FactKey::new("iface.Node", "S1"),
        "rel",
        FactKey::new("iface.TargetB", "K1"),
    );

    let link_commit = |key: &LinkKey, expected: u64, mutation: LinkMutation| CellCommit {
        links: vec![LinkWrite {
            key: key.clone(),
            expected: FactVersion(expected),
            mutation,
        }],
        ..CellCommit::empty()
    };
    let add = || LinkMutation::Add {
        values: serde_json::from_value(json!({"note": "v"})).expect("note"),
    };

    // 1. Add RootA/K1 -> v1, then Remove RootA/K1 -> v2 (tombstone).
    let receipt_add = cell
        .commit(link_commit(&key_a, 0, add()))
        .expect("add RootA/K1");
    assert_eq!(receipt_add.linked[0].version, FactVersion(1));
    let receipt_rm = cell
        .commit(link_commit(&key_a, 1, LinkMutation::Remove))
        .expect("remove RootA/K1");
    assert_eq!(receipt_rm.linked[0].version, FactVersion(2));

    // 2. Attempting Add or Remove of RootB/K1 in the same physical table refuses.
    for mutation in [add(), LinkMutation::Remove] {
        assert!(matches!(
            cell.commit(link_commit(&key_b, 0, mutation)),
            Err(CommitError::EffectiveValues { .. })
        ));
    }

    // 3. Stale CAS expecting original v1 on RootA/K1 still fails with LinkConflict(2).
    assert!(matches!(
        cell.commit(link_commit(&key_a, 1, add())),
        Err(CommitError::LinkConflict {
            expected: FactVersion(1),
            current: FactVersion(2),
            ..
        })
    ));

    // 4. Re-adding RootA/K1 at v2 advances monotonically to v3 (never replays v1).
    let receipt_readd = cell
        .commit(link_commit(&key_a, 2, add()))
        .expect("re-add RootA/K1");
    assert_eq!(receipt_readd.linked[0].version, FactVersion(3));
}

/// R4/R5 Blocker 3: SQLite `EXPLAIN QUERY PLAN` proves `verify_link_endpoint_tables`
/// and incoming `has_link_source` seek directly via `INDEXED BY` on BOTH non-unique
/// (`from: many`) and unique (`from: one`) link tables without full-world scans.
#[test]
fn r4_blocker3_explain_query_plan_seeks_directly_without_world_scan() {
    let defs: Vec<norn_ir::ObjectTypeDef> = serde_json::from_value(json!([
        {"name": "cap.Slot", "properties": [{"id": "id", "type": {"kind": "text"}}]},
        {"name": "cap.Owner", "properties": [{"id": "id", "type": {"kind": "text"}}], "links": [
            {"id": "owns", "target": "cap.Slot", "cardinality": {"to": "many", "from": "one"}},
            {"id": "watches", "target": "cap.Slot", "cardinality": {"to": "many", "from": "many"}}
        ]}
    ]))
    .expect("defs");
    let model = common::model_of(&defs);
    let table_for = |link: &str| {
        model
            .schema()
            .link_tables
            .iter()
            .find(|t| t.source.as_str() == "cap.Owner" && t.link.as_str() == link)
            .expect("link table")
            .table
            .clone()
    };
    let (unique_table, non_unique_table) = (table_for("owns"), table_for("watches"));
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("eqp.sqlite");
    let store = CellStore::open(&path, model, StoreOptions::default()).expect("store");
    let mut cell: SqliteCell<norn_kernel::FactEvent, JsonObject, JsonObject> = SqliteCell::new(
        store,
        "org_demo/dev",
        |cmd: &norn_kernel::Command<JsonObject>| cmd.payload.clone(),
    )
    .expect("cell");
    r2_put_fact(&mut cell, "cap.Owner", "O1", json!({"id": "O1"}));
    r2_put_fact(&mut cell, "cap.Slot", "T1", json!({"id": "T1"}));
    let raw = NativeEngine::open(&path, StoreOptions::default()).expect("raw");
    let explain = |sql: &str, args: &[SqlValue]| -> String {
        raw.query(&format!("EXPLAIN QUERY PLAN {sql}"), args)
            .expect("explain")
            .into_iter()
            .filter_map(|row| match row.get(3) {
                Some(SqlValue::Text(detail)) => Some(detail.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" | ")
    };

    for (col, idx, val) in [
        ("source", "_norn_link_version/source_order", "O1"),
        ("target", "_norn_link_version/target_order", "T1"),
    ] {
        let plan = explain(
            &format!(
                "SELECT DISTINCT \"link_table\" FROM \"_norn_link_version\" INDEXED BY \"{idx}\" WHERE \"world\" = ?1 AND \"{col}\" = ?2"
            ),
            &[
                SqlValue::Text("main".to_owned()),
                SqlValue::Text(val.to_owned()),
            ],
        );
        assert!(
            plan.contains("SEARCH _norn_link_version USING COVERING INDEX")
                && plan.contains(&format!("world=? AND {col}=?"))
                && !plan.contains("SCAN"),
            "expected covering index seek on (world, {col}); got: {plan}"
        );
    }
    for table_name in [&non_unique_table, &unique_table] {
        let probe_plan = explain(&format!("SELECT 1 FROM \"{table_name}\" LIMIT 1"), &[]);
        assert!(
            !probe_plan.contains("WHERE") && !probe_plan.contains("TEMP B-TREE"),
            "{probe_plan}"
        );
        let incoming = explain(
            &format!(
                "SELECT \"_source\" FROM \"{table_name}\" INDEXED BY \"{table_name}/target\" WHERE \"_world\" = ?1 AND \"_target\" = ?2 LIMIT ?3"
            ),
            &[
                SqlValue::Text("main".to_owned()),
                SqlValue::Text("T1".to_owned()),
                SqlValue::Integer(2),
            ],
        );
        assert!(
            incoming.contains("SEARCH")
                && incoming.contains(&format!("{table_name}/target"))
                && incoming.contains("_world=? AND _target=?")
                && !incoming.contains("SCAN")
                && !incoming.contains("TEMP B-TREE"),
            "expected direct target-index seek on {table_name}; got: {incoming}"
        );
    }
}

type R7TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn r7_predicate_ifc_fixture()
-> R7TestResult<SqliteCell<norn_kernel::FactEvent, JsonObject, JsonObject>> {
    let defs: Vec<norn_ir::ObjectTypeDef> = serde_json::from_value(json!([
        {
            "name": "pifc.Record",
            "properties": [
                {"id": "id", "type": {"kind": "text"}},
                {"id": "status", "type": {"kind": "text"}},
                {"id": "tag", "type": {"kind": "text"}},
                {"id": "ssn", "type": {"kind": "text"}, "markings": ["pii.ssn"]},
                {"id": "booked_on", "type": {"kind": "date"}, "markings": ["pii.date"]},
                {"id": "salary", "type": {"kind": "text"}, "markings": ["pii.salary"]}
            ]
        },
        {
            "name": "pifc.Public",
            "properties": [
                {"id": "id", "type": {"kind": "text"}},
                {"id": "note", "type": {"kind": "text"}}
            ],
            "links": [{
                "id": "linked",
                "target": "pifc.Public",
                "cardinality": {"to": "many", "from": "many"}
            }]
        }
    ]))?;
    let model = common::model_of(&defs);
    let store = CellStore::open_in_memory(model)?;
    let mut cell = SqliteCell::new(
        store,
        "org_demo/dev",
        |cmd: &norn_kernel::Command<JsonObject>| cmd.payload.clone(),
    )?;
    for id in ["P1", "P2"] {
        cell.commit(CellCommit {
            writes: vec![FactWrite {
                key: FactKey::new("pifc.Public", id),
                expected: FactVersion(0),
                mutation: FactMutation::Put {
                    values: serde_json::from_value(json!({"id": id, "note": "init"}))?,
                },
            }],
            ..CellCommit::empty()
        })?;
    }
    Ok(cell)
}

fn r7_empty_pred_link_commit(
    predicate: PredicateQuery,
    remove: bool,
) -> CellCommit<norn_kernel::FactEvent, JsonObject, JsonObject> {
    CellCommit {
        predicate_reads: vec![norn_kernel::PredicateReadExpectation {
            predicate,
            members: Vec::new(),
        }],
        links: vec![LinkWrite {
            key: LinkKey::new(
                FactKey::new("pifc.Public", "P1"),
                "linked",
                FactKey::new("pifc.Public", "P2"),
            ),
            expected: FactVersion(u64::from(remove)),
            mutation: if remove {
                LinkMutation::Remove
            } else {
                LinkMutation::Add {
                    values: JsonObject::new(),
                }
            },
        }],
        ..CellCommit::empty()
    }
}

fn r7_empty_pred_fact_commit(
    predicate: PredicateQuery,
    out_id: &str,
) -> R7TestResult<CellCommit<norn_kernel::FactEvent, JsonObject, JsonObject>> {
    Ok(CellCommit {
        predicate_reads: vec![norn_kernel::PredicateReadExpectation {
            predicate,
            members: Vec::new(),
        }],
        writes: vec![FactWrite {
            key: FactKey::new("pifc.Public", out_id),
            expected: FactVersion(0),
            mutation: FactMutation::Put {
                values: serde_json::from_value(json!({"id": out_id, "note": "derived"}))?,
            },
        }],
        ..CellCommit::empty()
    })
}

fn r7_put_record(
    cell: &mut SqliteCell<norn_kernel::FactEvent, JsonObject, JsonObject>,
    id: &str,
    expected: u64,
    tag: &str,
    ssn: &str,
    reads: Vec<norn_kernel::FactExpectation>,
) -> R7TestResult {
    cell.commit(CellCommit {
        reads,
        writes: vec![FactWrite {
            key: FactKey::new("pifc.Record", id),
            expected: FactVersion(expected),
            mutation: FactMutation::Put {
                values: serde_json::from_value(json!({
                    "id": id, "status": "open", "tag": tag,
                    "ssn": ssn, "booked_on": "2026-01-15", "salary": "900"
                }))?,
            },
        }],
        ..CellCommit::empty()
    })?;
    Ok(())
}

fn r7_note_markings(
    cell: &SqliteCell<norn_kernel::FactEvent, JsonObject, JsonObject>,
    id: &str,
) -> R7TestResult<Vec<String>> {
    let marks = cell
        .store()
        .read()
        .effective_markings("main", "pifc.Public", id, None)?
        .ok_or("public markings missing")?;
    marks
        .get("note")
        .cloned()
        .ok_or_else(|| "note missing".into())
}

/// R7 Blocker: empty predicate results over schema-classified filter fields
/// (on both 0-row empty tables and non-empty tables) and classified window/
/// temporal fields propagate IFC markings to fact writes and refuse links.
#[test]
fn r7_empty_predicate_over_classified_filter_window_and_empty_table_propagates_ifc() -> R7TestResult
{
    let mut cell = r7_predicate_ifc_fixture()?;
    let ssn_pred: PredicateQuery =
        serde_json::from_value(json!({"objectType": "pifc.Record", "filter": {"ssn": "000"}}))?;

    // 1. Empty table (0 rows) over schema-classified filter field -> refuses link & taints fact.
    let err = cell.commit(r7_empty_pred_link_commit(ssn_pred.clone(), false));
    assert!(matches!(
        err,
        Err(CommitError::Host(
            CellError::MarkingDeclassificationInvalid(_)
        ))
    ));
    cell.commit(r7_empty_pred_fact_commit(ssn_pred.clone(), "O1")?)?;
    assert!(
        r7_note_markings(&cell, "O1")?
            .iter()
            .any(|l| l == "pii.ssn")
    );

    // 2. Non-empty table, 0 matching rows over schema-classified filter -> refuses link & taints fact.
    r7_put_record(&mut cell, "R1", 0, "plain", "111-11-1111", Vec::new())?;
    let err = cell.commit(r7_empty_pred_link_commit(ssn_pred.clone(), false));
    assert!(matches!(
        err,
        Err(CommitError::Host(
            CellError::MarkingDeclassificationInvalid(_)
        ))
    ));
    cell.commit(r7_empty_pred_fact_commit(ssn_pred, "O2")?)?;
    assert!(
        r7_note_markings(&cell, "O2")?
            .iter()
            .any(|l| l == "pii.ssn")
    );

    // 3. Classified window/temporal field (`booked_on`) with 0 matches -> refuses link & taints fact.
    let window_pred: PredicateQuery = serde_json::from_value(json!({
        "objectType": "pifc.Record",
        "window": {
            "property": "booked_on", "kind": "date",
            "start": "2027-01-01", "end": "2027-12-31",
            "startInclusive": true, "endInclusive": true
        }
    }))?;
    let err = cell.commit(r7_empty_pred_link_commit(window_pred.clone(), false));
    assert!(matches!(
        err,
        Err(CommitError::Host(
            CellError::MarkingDeclassificationInvalid(_)
        ))
    ));
    cell.commit(r7_empty_pred_fact_commit(window_pred, "O3")?)?;
    assert!(
        r7_note_markings(&cell, "O3")?
            .iter()
            .any(|l| l == "pii.date")
    );
    Ok(())
}

/// R7 Blocker: negative control (unclassified filter field does not over-taint
/// from unrelated schema-classified or dynamically marked fields on non-matching
/// rows) + dynamically marked filter field with unmarked occurrence propagates IFC.
#[test]
fn r7_empty_predicate_dynamic_marking_and_unclassified_negative_control() -> R7TestResult {
    let mut cell = r7_predicate_ifc_fixture()?;
    r7_put_record(&mut cell, "R1", 0, "plain", "111-11-1111", Vec::new())?;
    r7_put_record(&mut cell, "R2", 0, "plain", "222-22-2222", Vec::new())?;
    // Update ONLY `R2.tag` while reading `R1`: `R2`'s occurrence and `status`
    // remain unmarked, while schema-unmarked `R2.tag` dynamically inherits `pii.ssn`.
    r7_put_record(
        &mut cell,
        "R2",
        1,
        "secret-derived",
        "222-22-2222",
        vec![norn_kernel::FactExpectation {
            key: FactKey::new("pifc.Record", "R1"),
            expected: FactVersion(1),
        }],
    )?;
    let r2_occ = cell
        .store()
        .read()
        .fact_occurrence_markings("main", "pifc.Record", "R2", None)?
        .ok_or("R2 occurrence missing")?;
    assert!(
        r2_occ.is_empty(),
        "R2 occurrence must stay unmarked: {r2_occ:?}"
    );
    let r2_marks = cell
        .store()
        .read()
        .effective_markings("main", "pifc.Record", "R2", None)?
        .ok_or("R2 markings missing")?;
    assert!(r2_marks.get("status").is_some_and(Vec::is_empty));
    assert!(
        r2_marks
            .get("tag")
            .is_some_and(|m| m.iter().any(|l| l == "pii.ssn"))
    );

    // Negative control: empty predicate on unclassified `status` must NOT over-taint
    // from unrelated `salary`/`ssn` or dynamically marked `R2.tag`.
    let status_pred: PredicateQuery = serde_json::from_value(
        json!({"objectType": "pifc.Record", "filter": {"status": "closed"}}),
    )?;
    let mut clean_commit = r7_empty_pred_fact_commit(status_pred.clone(), "CLEAN")?;
    clean_commit
        .links
        .extend(r7_empty_pred_link_commit(status_pred, false).links);
    cell.commit(clean_commit)?;
    assert!(r7_note_markings(&cell, "CLEAN")?.is_empty());

    // Empty predicate on dynamically marked `tag` refuses link & propagates pii.ssn.
    let tag_pred: PredicateQuery = serde_json::from_value(
        json!({"objectType": "pifc.Record", "filter": {"tag": "no-match"}}),
    )?;
    let err = cell.commit(r7_empty_pred_link_commit(tag_pred.clone(), true));
    assert!(matches!(
        err,
        Err(CommitError::Host(
            CellError::MarkingDeclassificationInvalid(_)
        ))
    ));
    cell.commit(r7_empty_pred_fact_commit(tag_pred, "DYN")?)?;
    assert!(
        r7_note_markings(&cell, "DYN")?
            .iter()
            .any(|l| l == "pii.ssn")
    );
    Ok(())
}
