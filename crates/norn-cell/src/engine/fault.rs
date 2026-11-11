//! Deterministic fault injection for simulation testing (`fault-injection`
//! feature, enabled only through dev-dependencies).

use super::{CellError, ReadIsolation, SqlEngine, SqlValue};

/// What an injected engine fault does (deterministic simulation testing).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineFaultKind {
    /// The host dies at the faulted statement: its transaction never
    /// commits, and the engine refuses every later call. The caller reopens
    /// the store from durable state with a fresh engine.
    Crash,
    /// The faulted statement fails with an I/O error: its transaction rolls
    /// back and the engine stays usable.
    Io,
    /// The storage's own COMMIT of the faulted transaction fails after its
    /// body succeeded: the storage rolls it back and throws its own error.
    /// No statement fails; only a storage that commits itself (the
    /// `DoStorageEmulator` given the switch) fires it, through
    /// [`FaultSwitch::take_commit_fault`].
    Commit,
}

/// One scheduled fault: SQL statement `at_statement` (counting from 0) of
/// transaction `transaction` (counting from 0) begun after
/// [`FaultSwitch::arm`]. A faulted transaction never commits: when its
/// closure would commit with fewer statements, the fault fires at the commit
/// point instead. A closure that fails on its own (a refusal) rolls back
/// without the fault firing. An [`EngineFaultKind::Commit`] fault ignores
/// `at_statement` and fails the storage's commit of that transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineFault {
    /// What the fault does.
    pub kind: EngineFaultKind,
    /// Which transaction after arming fails, counting from 0.
    pub transaction: u32,
    /// Which statement of that transaction fails, counting from 0.
    pub at_statement: u32,
}

/// The fault schedule shared by every [`FaultEngine`] opened over one store,
/// so a test arms it once and it survives reopening the store. Counting is
/// deterministic: statements and transactions in call order, nothing else.
#[derive(Debug, Clone, Default)]
pub struct FaultSwitch(std::rc::Rc<FaultState>);

#[derive(Debug, Default)]
struct FaultState {
    armed: std::cell::Cell<Option<EngineFault>>,
    transactions: std::cell::Cell<u32>,
    statements: std::cell::Cell<u32>,
    targeting: std::cell::Cell<bool>,
    fired: std::cell::Cell<bool>,
}

impl FaultSwitch {
    /// Arms `fault`, counting transactions and statements afresh.
    pub fn arm(&self, fault: EngineFault) {
        self.0.armed.set(Some(fault));
        self.0.transactions.set(0);
        self.0.statements.set(0);
        self.0.targeting.set(false);
        self.0.fired.set(false);
    }

    /// Disarms the switch and returns whether the armed fault fired.
    #[must_use = "the caller must branch on whether the fault fired"]
    pub fn disarm(&self) -> bool {
        self.0.armed.set(None);
        self.0.targeting.set(false);
        self.0.fired.replace(false)
    }

    /// Whether the armed fault has fired.
    #[must_use]
    pub fn fired(&self) -> bool {
        self.0.fired.get()
    }

    /// Fires an armed [`EngineFaultKind::Commit`] fault when the storage is
    /// about to commit its targeted transaction, and reports whether it did:
    /// the storage then fails that commit.
    #[must_use = "a fired commit fault must fail the commit"]
    pub fn take_commit_fault(&self) -> bool {
        let due = self.0.targeting.get()
            && !self.0.fired.get()
            && self
                .0
                .armed
                .get()
                .is_some_and(|fault| fault.kind == EngineFaultKind::Commit);
        if due {
            self.0.fired.set(true);
        }
        due
    }
}

/// An engine that fails on the schedule of its [`FaultSwitch`] and otherwise
/// delegates to `S`. It guarantees that a transaction the fault reaches is
/// rolled back, even when the caller swallows the statement's error.
#[derive(Debug)]
pub struct FaultEngine<S> {
    inner: S,
    switch: FaultSwitch,
    crashed: std::cell::Cell<bool>,
}

impl<S: SqlEngine> FaultEngine<S> {
    /// `inner`, failing on the schedule of `switch`.
    #[must_use]
    pub const fn new(inner: S, switch: FaultSwitch) -> Self {
        Self {
            inner,
            switch,
            crashed: std::cell::Cell::new(false),
        }
    }

    /// The wrapped engine.
    #[must_use]
    pub const fn inner(&self) -> &S {
        &self.inner
    }

    fn error(kind: EngineFaultKind) -> CellError {
        CellError::Engine(match kind {
            EngineFaultKind::Crash => "injected fault: host crashed".to_owned(),
            EngineFaultKind::Io => "injected fault: disk I/O error".to_owned(),
            EngineFaultKind::Commit => "injected fault: COMMIT failed".to_owned(),
        })
    }

    fn fire(&self, kind: EngineFaultKind) -> CellError {
        self.switch.0.fired.set(true);
        if kind == EngineFaultKind::Crash {
            self.crashed.set(true);
        }
        Self::error(kind)
    }

    fn live(&self) -> Result<(), CellError> {
        if self.crashed.get() {
            Err(Self::error(EngineFaultKind::Crash))
        } else {
            Ok(())
        }
    }

    /// Counts one statement and fails it when it is the armed one.
    fn statement(&self) -> Result<(), CellError> {
        self.live()?;
        let state = &self.switch.0;
        let Some(fault) = state.armed.get() else {
            return Ok(());
        };
        if !state.targeting.get() || state.fired.get() || fault.kind == EngineFaultKind::Commit {
            return Ok(());
        }
        let index = state.statements.get();
        state.statements.set(index.saturating_add(1));
        if index == fault.at_statement {
            return Err(self.fire(fault.kind));
        }
        Ok(())
    }
}

impl<S: SqlEngine> SqlEngine for FaultEngine<S> {
    fn execute(&self, sql: &str, params: &[SqlValue]) -> Result<(), CellError> {
        self.statement()?;
        self.inner.execute(sql, params)
    }

    fn query(&self, sql: &str, params: &[SqlValue]) -> Result<Vec<Vec<SqlValue>>, CellError> {
        self.statement()?;
        self.inner.query(sql, params)
    }

    fn execute_batch(&self, sql: &str) -> Result<(), CellError> {
        self.statement()?;
        self.inner.execute_batch(sql)
    }

    fn transaction<T, X: From<CellError>>(&self, f: impl FnOnce() -> Result<T, X>) -> Result<T, X> {
        struct Targeting<'a>(&'a std::cell::Cell<bool>);
        impl Drop for Targeting<'_> {
            fn drop(&mut self) {
                self.0.set(false);
            }
        }
        self.live()?;
        let state = &self.switch.0;
        let Some(fault) = state.armed.get() else {
            return self.inner.transaction(f);
        };
        let index = state.transactions.get();
        state.transactions.set(index.saturating_add(1));
        if index != fault.transaction || state.fired.get() {
            return self.inner.transaction(f);
        }
        state.statements.set(0);
        state.targeting.set(true);
        let targeting = Targeting(&state.targeting);
        let result = self.inner.transaction(|| match f() {
            Err(error) => Err(error),
            // Never commit once the fault fired, even if the caller swallowed
            // the statement's error; otherwise this is the commit point.
            Ok(_) if state.fired.get() => Err(Self::error(fault.kind).into()),
            // The storage fails its own commit (still targeted until it returns).
            Ok(value) if fault.kind == EngineFaultKind::Commit => Ok(value),
            Ok(_) => Err(self.fire(fault.kind).into()),
        });
        drop(targeting);
        result
    }

    fn platform_table_globs(&self) -> &'static [&'static str] {
        self.inner.platform_table_globs()
    }

    fn read_isolation(&self) -> ReadIsolation {
        self.inner.read_isolation()
    }

    fn read_cache_revision(&self) -> Result<Option<(u64, u64)>, CellError> {
        self.live()?;
        self.inner.read_cache_revision()
    }
}

#[cfg(feature = "native")]
impl super::NativeEngine {
    /// [`super::NativeEngine::open`] with `PRAGMA synchronous = OFF`, for the
    /// simulator's file stores only (test-only `fault-injection` feature;
    /// production opens keep `FULL`). The simulator's crashes are injected by
    /// [`FaultEngine`] (the transaction never commits, the connection is
    /// dropped, the store reopens from the same file), so the harness needs
    /// no fsync. Disk durability under power loss is out of scope here; it
    /// belongs to the nightly differential on real runtimes (D79 D step 5).
    ///
    /// # Errors
    /// SQLite open or configuration errors.
    pub fn open_unsynced(
        path: &std::path::Path,
        options: crate::store::StoreOptions,
    ) -> Result<Self, CellError> {
        let engine = Self::open(path, options)?;
        engine.conn.execute_batch("PRAGMA synchronous = OFF")?;
        Ok(engine)
    }
}

/// What a native `transactionSync` emulator and its callback throw.
#[cfg(feature = "native")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Thrown {
    /// Thrown by the callback, rethrown unchanged after the rollback.
    Callback(String),
    /// Thrown by the storage itself: a refused nested `transactionSync`, or a
    /// failed begin, commit or rollback.
    Storage(String),
}

#[cfg(feature = "native")]
impl From<CellError> for Thrown {
    fn from(error: CellError) -> Self {
        Self::Storage(error.to_string())
    }
}

/// A native Durable Object storage: `sql.exec` plus `transactionSync`.
#[cfg(feature = "native")]
pub trait DoStorage: super::Transact {
    /// `storage.sql.exec`: runs `sql` and returns its rows.
    ///
    /// # Errors
    /// Backend or host-subset errors.
    fn exec(&self, sql: &str, params: &[SqlValue]) -> Result<Vec<Vec<SqlValue>>, CellError>;

    /// `storage.sql.exec` of several schema statements without bindings.
    ///
    /// # Errors
    /// Backend or host-subset errors.
    fn exec_batch(&self, sql: &str) -> Result<(), CellError>;
}

/// The rollback marker [`DoStorageEmulator`]'s glue throws (as the Workers
/// host does).
#[cfg(feature = "native")]
const ROLLBACK_MARKER: &str = "norn:rollback";

/// A native emulator of Durable Object storage over rusqlite, for running the
/// host laws on the Durable Object transaction glue ([`super::run_transaction`])
/// natively. Its assumed `transactionSync` semantics, which the Workers engine
/// relies on:
///
/// 1. The callback runs synchronously, exactly once, inside one SQLite
///    transaction; every `sql.exec` it makes joins that transaction.
/// 2. If the callback throws (returns `Err`), everything it wrote is rolled
///    back and `transactionSync` rethrows the callback's value unchanged.
/// 3. If the callback returns normally, the transaction commits and
///    `transactionSync` returns normally.
/// 4. A `transactionSync` inside another one is refused by the storage with
///    its own thrown value ([`Thrown::Storage`]), before any SQL; the glue
///    refuses nesting before reaching the storage, so this is defensive.
/// 5. Transaction-control SQL (`BEGIN`, `COMMIT`, `ROLLBACK`, `SAVEPOINT`,
///    `RELEASE`) is refused inside or outside a transaction: only the storage
///    opens and ends transactions (the native engine's authorizer denies it).
/// 6. If the commit (or the rollback) itself fails, the transaction is rolled
///    back and `transactionSync` throws the storage's error, not the
///    callback's value; the glue reports it as an engine error. Given a
///    [`FaultSwitch`] ([`DoStorageEmulator::with_faults`]), an armed
///    [`EngineFaultKind::Commit`] fault fails the commit of its targeted
///    transaction this way, after the callback succeeded.
///
/// So the glue's caller sees its own typed error on a rollback (the callback
/// threw the marker), and a [`CellError::Engine`] for a storage failure. The
/// emulator does not model Workers specifics outside the transaction (JS
/// values, output gates, multi-statement `exec` cursors): `exec` runs one
/// statement and returns its rows, `exec_batch` runs schema batches.
#[cfg(feature = "native")]
#[derive(Debug)]
pub struct DoStorageEmulator {
    native: super::NativeEngine,
    in_transaction: std::cell::Cell<bool>,
    faults: Option<FaultSwitch>,
}

#[cfg(feature = "native")]
impl DoStorageEmulator {
    /// The emulator over `native`.
    #[must_use]
    pub const fn new(native: super::NativeEngine) -> Self {
        Self {
            native,
            in_transaction: std::cell::Cell::new(false),
            faults: None,
        }
    }

    /// The same emulator failing its commits on `faults`' armed
    /// [`EngineFaultKind::Commit`] faults (semantics 6).
    #[must_use]
    pub fn with_faults(mut self, faults: FaultSwitch) -> Self {
        self.faults = Some(faults);
        self
    }
}

#[cfg(feature = "native")]
impl super::Transact for DoStorageEmulator {
    type Thrown = Thrown;

    fn transaction_sync<F: FnMut() -> Result<(), Thrown>>(
        &self,
        callback: &mut F,
    ) -> Result<(), Thrown> {
        struct Open<'a>(&'a std::cell::Cell<bool>);
        impl Drop for Open<'_> {
            fn drop(&mut self) {
                self.0.set(false);
            }
        }
        if self.in_transaction.get() {
            return Err(Thrown::Storage(
                "transactionSync cannot be nested".to_owned(),
            ));
        }
        self.in_transaction.set(true);
        let open = Open(&self.in_transaction);
        // Commit on Ok; on Err roll back and rethrow; a begin, commit or
        // rollback failure becomes the storage's own throw. An injected
        // commit failure rolls back the same way and throws as the storage.
        let result = self.native.transaction(|| {
            callback()?;
            if self
                .faults
                .as_ref()
                .is_some_and(FaultSwitch::take_commit_fault)
            {
                return Err(Thrown::Storage("injected fault: COMMIT failed".to_owned()));
            }
            Ok(())
        });
        drop(open);
        result
    }

    fn rollback_marker(&self) -> Thrown {
        Thrown::Callback(ROLLBACK_MARKER.to_owned())
    }

    fn is_rollback_marker(&self, thrown: &Thrown) -> bool {
        matches!(thrown, Thrown::Callback(value) if value == ROLLBACK_MARKER)
    }

    fn callback_reused(&self) -> Thrown {
        Thrown::Callback("transaction callback reused".to_owned())
    }

    fn engine_error(&self, thrown: &Thrown) -> CellError {
        CellError::Engine(format!("DO SQLite: {thrown:?}"))
    }
}

#[cfg(feature = "native")]
impl DoStorage for DoStorageEmulator {
    fn exec(&self, sql: &str, params: &[SqlValue]) -> Result<Vec<Vec<SqlValue>>, CellError> {
        self.native.query(sql, params)
    }

    fn exec_batch(&self, sql: &str) -> Result<(), CellError> {
        self.native.execute_batch(sql)
    }
}

/// The Durable Object engine natively: the Workers engine's transaction glue
/// ([`super::run_transaction`]) over a [`DoStorage`], by default the
/// [`DoStorageEmulator`]. SQL goes straight to `sql.exec`, as on Workers.
#[cfg(feature = "native")]
#[derive(Debug)]
pub struct DoEngine<S = DoStorageEmulator> {
    storage: S,
    active: std::cell::Cell<bool>,
}

#[cfg(feature = "native")]
impl<S: DoStorage> DoEngine<S> {
    /// The engine over `storage`.
    #[must_use]
    pub const fn new(storage: S) -> Self {
        Self {
            storage,
            active: std::cell::Cell::new(false),
        }
    }

    /// The storage under the glue.
    #[must_use]
    pub const fn storage(&self) -> &S {
        &self.storage
    }
}

#[cfg(feature = "native")]
impl<S: DoStorage> SqlEngine for DoEngine<S> {
    /// As on Workers.
    fn platform_table_globs(&self) -> &'static [&'static str] {
        &["_cf_*", "__cf_*", "_litestream_*", "__miniflare_*"]
    }

    /// As on Workers: one connection, every read path synchronous.
    fn read_isolation(&self) -> ReadIsolation {
        ReadIsolation::ExclusiveSynchronous
    }

    fn execute(&self, sql: &str, params: &[SqlValue]) -> Result<(), CellError> {
        self.query(sql, params).map(|_| ())
    }

    fn query(&self, sql: &str, params: &[SqlValue]) -> Result<Vec<Vec<SqlValue>>, CellError> {
        super::validate_bindings(sql, params)?;
        self.storage.exec(sql, params)
    }

    fn execute_batch(&self, sql: &str) -> Result<(), CellError> {
        super::validate_bindings(sql, &[])?;
        self.storage.exec_batch(sql)
    }

    fn transaction<T, X: From<CellError>>(&self, f: impl FnOnce() -> Result<T, X>) -> Result<T, X> {
        super::run_transaction(&self.storage, &self.active, f)
    }
}
