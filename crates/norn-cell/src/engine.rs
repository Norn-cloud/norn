//! Synchronous SQLite execution supplied by the cell's host.
//!
//! Values own their data; no native driver or JavaScript type crosses this
//! boundary. A transaction runs the closure against the same engine, commits
//! on success and rolls back on error. Hosts must reject nested transactions.
//! Integer properties outside ±2^53 travel as exact text, with SQLite INTEGER
//! affinity on writes and text projections on reads; counters stay bounded.
//! The default `native` feature supplies [`NativeEngine`]; portable hosts
//! supply their own implementation (including durability and isolation).
//! With the test-only `fault-injection` feature, `FaultEngine` wraps any
//! engine to inject crashes and I/O errors on a deterministic schedule, for
//! simulation testing of the hosts above it. Production builds never compile it.
//! `run_transaction` is the crate's shared transaction glue of callback-style
//! hosts (`Transact`); with both features, `DoEngine` runs it natively over a
//! `transactionSync` emulator.

use crate::CellError;

/// The read revision of an engine whose connection is its database's only
/// writer ([`ReadIsolation::ExclusiveSynchronous`]): a count of every
/// statement that is not provably read-only, so equal counts outside a
/// transaction mean unchanged contents. The engine's own `query`, `execute`
/// and `execute_batch` note each statement; no caller bumps it.
#[derive(Debug, Default)]
pub struct WriteRevision(std::cell::Cell<u64>);

impl WriteRevision {
    /// A fresh connection's revision.
    #[must_use]
    pub const fn new() -> Self {
        Self(std::cell::Cell::new(0))
    }

    /// Note `sql` about to run: anything but one plain `SELECT` statement
    /// counts as a write, including statements the platform would refuse.
    pub fn note(&self, sql: &str) {
        if !read_only(sql) {
            self.bump();
        }
    }

    /// Note a statement batch: always a write.
    pub fn bump(&self) {
        self.0.set(self.0.get().wrapping_add(1));
    }

    /// The revision outside a transaction; `None` inside one, whose writes
    /// could still roll back.
    #[must_use]
    pub fn current(&self, in_transaction: bool) -> Option<(u64, u64)> {
        (!in_transaction).then(|| (self.0.get(), 0))
    }
}

const WRITE_KEYWORDS: &[&str] = &[
    "INSERT", "UPDATE", "DELETE", "REPLACE", "CREATE", "DROP", "ALTER", "PRAGMA", "ATTACH",
    "VACUUM", "REINDEX",
];

/// One `SELECT` statement: no second statement after a `;`, and no keyword
/// that writes. Conservative: anything unsure is not read-only.
fn read_only(sql: &str) -> bool {
    let text = sql.trim().trim_end_matches(';').trim_end();
    text.get(..6)
        .is_some_and(|head| head.eq_ignore_ascii_case("SELECT"))
        && !text.contains(';')
        && !text
            .split(|c: char| !c.is_ascii_alphanumeric() && c != '_')
            .any(|word| {
                WRITE_KEYWORDS
                    .iter()
                    .any(|keyword| word.eq_ignore_ascii_case(keyword))
            })
}

/// Largest SQLite integer carried exactly by a JavaScript number.
pub const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_992;
/// Maximum number of bindings in the admitted host subset.
pub const MAX_PARAMETERS: usize = 100;
/// Maximum SQL statement length in bytes in the admitted host subset.
pub const MAX_STATEMENT_BYTES: usize = 100_000;

/// How a sequence of reads on one engine knows it saw a single snapshot
/// (HOST-READ-2). Every engine declares one; there is no default, so a
/// wrapper cannot inherit a guarantee it does not provide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadIsolation {
    /// [`SqlEngine::read_cache_revision`] returns `Some` outside a
    /// transaction, and equal revisions imply unchanged database contents,
    /// including writes by other connections. A reader that finds `None`
    /// must refuse rather than assume stability.
    Revisioned,
    /// The engine's connection is the only one to its database and every read
    /// path runs synchronously, without yielding, so nothing can write between
    /// two reads of one call. Only the Durable Object engine declares this.
    ExclusiveSynchronous,
}

/// An owned SQLite value, shared by native and portable hosts.
#[derive(Debug, Clone, PartialEq)]
pub enum SqlValue {
    /// SQL NULL.
    Null,
    /// An integer in the inclusive range ±2^53.
    Integer(i64),
    /// A SQLite floating-point value; decimals and hashes use text instead.
    Real(f64),
    /// UTF-8 text.
    Text(String),
    /// Binary data.
    Blob(Vec<u8>),
}

impl From<&str> for SqlValue {
    fn from(value: &str) -> Self {
        Self::Text(value.to_owned())
    }
}

impl From<String> for SqlValue {
    fn from(value: String) -> Self {
        Self::Text(value)
    }
}

impl From<i64> for SqlValue {
    fn from(value: i64) -> Self {
        Self::Integer(value)
    }
}

impl From<i32> for SqlValue {
    fn from(value: i32) -> Self {
        Self::Integer(i64::from(value))
    }
}

impl From<u32> for SqlValue {
    fn from(value: u32) -> Self {
        Self::Integer(i64::from(value))
    }
}

impl From<bool> for SqlValue {
    fn from(value: bool) -> Self {
        Self::Integer(i64::from(value))
    }
}

impl<T: Into<Self>> From<Option<T>> for SqlValue {
    fn from(value: Option<T>) -> Self {
        value.map_or(Self::Null, Into::into)
    }
}

impl<T: Clone> From<&T> for SqlValue
where
    Self: From<T>,
{
    fn from(value: &T) -> Self {
        Self::from(value.clone())
    }
}

/// Validate the common statement and binding limits before crossing the seam.
///
/// # Errors
/// Refuses oversized SQL, excess parameters and inexact integers.
pub fn validate_bindings(sql: &str, params: &[SqlValue]) -> Result<(), CellError> {
    if sql.len() > MAX_STATEMENT_BYTES {
        return Err(CellError::OutOfRange("SQL statement bytes"));
    }
    if params.len() > MAX_PARAMETERS {
        return Err(CellError::OutOfRange("SQL parameter count"));
    }
    for value in params {
        validate_value(value)?;
    }
    Ok(())
}

/// Check that an integer is exactly representable across the host boundary.
///
/// # Errors
/// An integer outside ±2^53 is refused.
pub fn validate_value(value: &SqlValue) -> Result<(), CellError> {
    if let SqlValue::Integer(integer) = value
        && !(-MAX_SAFE_INTEGER..=MAX_SAFE_INTEGER).contains(integer)
    {
        return Err(CellError::OutOfRange("SQL integer"));
    }
    Ok(())
}

/// A host-owned synchronous SQL executor. All operations inside `transaction`
/// must use its active transaction on this engine, with no intervening writer.
pub trait SqlEngine {
    /// Execute a statement without relying on affected-row counts.
    ///
    /// # Errors
    /// Backend, constraint or host-subset errors.
    fn execute(&self, sql: &str, params: &[SqlValue]) -> Result<(), CellError>;

    /// Return rows in SQL order, with columns in projection order.
    ///
    /// # Errors
    /// Backend or host-subset errors. Returned integers must be exact to 2^53.
    fn query(&self, sql: &str, params: &[SqlValue]) -> Result<Vec<Vec<SqlValue>>, CellError>;

    /// Execute schema statements without bindings.
    ///
    /// # Errors
    /// Backend or host-subset errors.
    fn execute_batch(&self, sql: &str) -> Result<(), CellError>;

    /// Commit on `Ok`, roll back on `Err`; refuse nested transactions.
    /// The closure executes on this engine. Rollback also occurs during native
    /// unwinding. `X` preserves a guarded commit's typed admission refusal.
    ///
    /// # Errors
    /// The closure's error, or a backend begin/commit/rollback error.
    fn transaction<T, X: From<CellError>>(&self, f: impl FnOnce() -> Result<T, X>) -> Result<T, X>;

    /// SQLite `GLOB` patterns of tables the host platform keeps in the same
    /// database (Durable Object metadata, replication state). They are never
    /// cell state: a database holding only them is fresh. Native SQLite has none.
    fn platform_table_globs(&self) -> &'static [&'static str] {
        &[]
    }

    /// How a sequence of reads knows it saw one snapshot; required, so every
    /// engine states it.
    fn read_isolation(&self) -> ReadIsolation;

    /// Optional connection-local revision for disposable verified read caches.
    /// Equal revisions must imply unchanged database contents on this connection,
    /// including local writes/rollbacks and commits by other connections.
    /// Hosts without this capability return `None`; no cache survives their read boundary.
    /// Return `None` while uncommitted state could later roll back without
    /// changing the token. Native engines only offer revisions in autocommit mode.
    /// The second component alone counts commits by every other connection or
    /// process (native SQLite's `PRAGMA data_version`), and is constant on an
    /// engine whose connection is its database's only writer: equal values
    /// mean nothing but this connection wrote in between (ADMIT-CACHE-1).
    /// This is a host optimization, not an extension of admitted SQL.
    ///
    /// # Errors
    /// Backend revision-read failures refuse cached access.
    fn read_cache_revision(&self) -> Result<Option<(u64, u64)>, CellError> {
        Ok(None)
    }

    /// External-writer revision of the database (`PRAGMA data_version` on
    /// native SQLite, constant `Some(0)` on exclusive single-connection
    /// engines such as Durable Objects and Rivet), readable both inside and
    /// outside a transaction (OPEN-CHECKPOINT-1 S10, #1551).
    ///
    /// Equal values across two transactions prove no other connection wrote
    /// to the database file in between.
    ///
    /// # Errors
    /// Backend revision-read failures.
    fn external_data_version(&self) -> Result<Option<u64>, CellError> {
        Ok(match self.read_isolation() {
            ReadIsolation::ExclusiveSynchronous => Some(0),
            ReadIsolation::Revisioned => None,
        })
    }

    /// A store has accepted this engine's database. A host applies here the
    /// persistent settings it deferred at open, so that a refused open
    /// leaves the file exactly as found (ENGINE-UPGRADE-1, #1504).
    ///
    /// # Errors
    /// Backend errors applying the deferred settings.
    fn admitted(&self) -> Result<(), CellError> {
        Ok(())
    }

    /// OPEN-CHECKPOINT-1 (#1551 round 4): the host's open-checkpoint key,
    /// held outside the cell file and given by the host when it opens the
    /// engine. Only a checkpoint whose MAC verifies under it shortens an
    /// open; with none, every open verifies from genesis and the cell
    /// records no checkpoint.
    fn checkpoint_key(&self) -> Option<crate::open_checkpoint::CheckpointKey> {
        None
    }

    /// Run `f`, the trusted writes of one definitions install, as the only
    /// statements this engine lets write the retained definitions
    /// (`_norn_definition_*`, `_norn_retained_*`). Outside it a native engine
    /// refuses every such write, whichever statement, trigger or foreign-key
    /// action carries it (S1, #1547). An engine whose connection is its
    /// database's only writer guards nothing and runs `f`; a wrapping engine
    /// forwards to the engine it wraps.
    ///
    /// # Errors
    /// `f`'s own, or a nested call.
    fn definition_writes<T>(
        &self,
        f: impl FnOnce() -> Result<T, CellError>,
    ) -> Result<T, CellError> {
        f()
    }
    /// Decode the first row; absence is an error.
    ///
    /// # Errors
    /// Backend, missing-row or decoder errors.
    fn query_row<T>(
        &self,
        sql: &str,
        params: &[SqlValue],
        decode: impl FnOnce(&SqlRow) -> Result<T, CellError>,
    ) -> Result<T, CellError> {
        self.query_optional(sql, params, decode)?
            .ok_or_else(|| CellError::Engine("query returned no rows".to_owned()))
    }

    /// Decode the first row, or return `None` when absent.
    ///
    /// # Errors
    /// Backend or decoder errors.
    fn query_optional<T>(
        &self,
        sql: &str,
        params: &[SqlValue],
        decode: impl FnOnce(&SqlRow) -> Result<T, CellError>,
    ) -> Result<Option<T>, CellError> {
        self.query(sql, params)?
            .into_iter()
            .next()
            .map(|values| decode(&SqlRow(values)))
            .transpose()
    }
}

/// An owned row with checked, typed column access.
#[derive(Debug, Clone, PartialEq)]
pub struct SqlRow(pub Vec<SqlValue>);

impl SqlRow {
    /// Decode a column at `index`.
    ///
    /// # Errors
    /// An absent column or incompatible value type.
    pub fn get<T: FromSqlValue>(&self, index: usize) -> Result<T, CellError> {
        T::from_sql_value(self.get_ref(index)?)
    }

    /// Borrow a column at `index`.
    ///
    /// # Errors
    /// An absent column.
    pub fn get_ref(&self, index: usize) -> Result<&SqlValue, CellError> {
        self.0
            .get(index)
            .ok_or_else(|| CellError::Engine(format!("invalid column index: {index}")))
    }
}

/// Checked decoding of one owned SQLite value.
pub trait FromSqlValue: Sized {
    /// Decode a value.
    ///
    /// # Errors
    /// An incompatible value type.
    fn from_sql_value(value: &SqlValue) -> Result<Self, CellError>;
}

impl FromSqlValue for i64 {
    fn from_sql_value(value: &SqlValue) -> Result<Self, CellError> {
        match value {
            SqlValue::Integer(integer) => Ok(*integer),
            SqlValue::Null | SqlValue::Real(_) | SqlValue::Text(_) | SqlValue::Blob(_) => {
                Err(CellError::Engine("expected an integer column".to_owned()))
            }
        }
    }
}

impl FromSqlValue for String {
    fn from_sql_value(value: &SqlValue) -> Result<Self, CellError> {
        match value {
            SqlValue::Text(text) => Ok(text.clone()),
            SqlValue::Null | SqlValue::Integer(_) | SqlValue::Real(_) | SqlValue::Blob(_) => {
                Err(CellError::Engine("expected a text column".to_owned()))
            }
        }
    }
}

impl<T: FromSqlValue> FromSqlValue for Option<T> {
    fn from_sql_value(value: &SqlValue) -> Result<Self, CellError> {
        match value {
            SqlValue::Null => Ok(None),
            SqlValue::Integer(_) | SqlValue::Real(_) | SqlValue::Text(_) | SqlValue::Blob(_) => {
                T::from_sql_value(value).map(Some)
            }
        }
    }
}

macro_rules! params {
    ($($value:expr),* $(,)?) => {
        &[$($crate::engine::SqlValue::from(&$value)),*]
    };
}
pub(crate) use params;

#[cfg(any(target_arch = "wasm32", feature = "fault-injection"))]
#[cfg_attr(
    not(feature = "fault-injection"),
    allow(
        unreachable_pub,
        reason = "public only when the fault-injection feature re-exports it"
    )
)]
mod transact;
#[cfg(feature = "fault-injection")]
pub use transact::{Transact, run_transaction};
#[cfg(all(target_arch = "wasm32", not(feature = "fault-injection")))]
pub(crate) use transact::{Transact, run_transaction};

/// Default engine on native hosts.
#[cfg(feature = "native")]
pub type DefaultEngine = NativeEngine;

/// A portable store must explicitly select a host engine.
#[cfg(not(feature = "native"))]
pub type DefaultEngine = NoEngine;

/// Uninhabited default in portable builds; hosts construct their own engine.
#[cfg(not(feature = "native"))]
#[derive(Debug)]
pub enum NoEngine {}

#[cfg(not(feature = "native"))]
impl SqlEngine for NoEngine {
    fn read_isolation(&self) -> ReadIsolation {
        match *self {}
    }

    fn execute(&self, _sql: &str, _params: &[SqlValue]) -> Result<(), CellError> {
        match *self {}
    }

    fn query(&self, _sql: &str, _params: &[SqlValue]) -> Result<Vec<Vec<SqlValue>>, CellError> {
        match *self {}
    }

    fn execute_batch(&self, _sql: &str) -> Result<(), CellError> {
        match *self {}
    }

    fn transaction<T, X: From<CellError>>(
        &self,
        _f: impl FnOnce() -> Result<T, X>,
    ) -> Result<T, X> {
        match *self {}
    }
}

/// Native SQLite adapter. Native journal/cache configuration and transaction
/// control stay here; shared store code uses only [`SqlEngine`].
#[cfg(feature = "native")]
#[derive(Debug)]
pub struct NativeEngine {
    conn: rusqlite::Connection,
    user_sql: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Set only inside [`SqlEngine::definition_writes`]: the retained
    /// definitions are writable while it is.
    definitions: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Inside [`SqlEngine::definition_writes`], the table the statement being
    /// prepared writes: the first write SQLite reports for it. Every later
    /// write it reports to another table (a foreign-key action, a REPLACE's
    /// cascade) is refused (S1, #1547). Cleared before each statement.
    target: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    /// The journal mode an open asked for, applied once a store admits the file.
    journal_mode: std::cell::Cell<Option<&'static str>>,
    /// The host's open-checkpoint key ([`SqlEngine::checkpoint_key`]).
    checkpoint_key: Option<crate::open_checkpoint::CheckpointKey>,
}

#[cfg(feature = "native")]
impl NativeEngine {
    /// Open a file with the existing native journal and cache options.
    ///
    /// ENGINE-UPGRADE-1 (#1494, #1504): the journal mode is stored in the
    /// file, so it is set only once a store accepts the file
    /// ([`SqlEngine::admitted`]): a cell refused for its storage version,
    /// ledger or any other check keeps the mode it was found in.
    ///
    /// # Errors
    /// SQLite open or configuration errors, or the storage-version refusals
    /// of [`crate::engine_release::admission`].
    pub fn open(
        path: &std::path::Path,
        options: crate::store::StoreOptions,
    ) -> Result<Self, CellError> {
        let mut engine = Self::configured(rusqlite::Connection::open(path)?)?;
        engine.checkpoint_key = options.checkpoint_key;
        crate::engine_release::admission(&engine)?;
        let conn = &engine.conn;
        engine.journal_mode.set(Some(match options.journal_mode {
            crate::store::JournalMode::Delete => "DELETE",
            crate::store::JournalMode::Wal => "WAL",
        }));
        conn.execute_batch("PRAGMA synchronous = FULL")?;
        match options.cache_pages {
            Some(pages) => conn.execute_batch(&format!("PRAGMA cache_size = {pages}"))?,
            // INGEST-CACHE-1: a page of an import writes megabytes (its
            // journal record alone is about 2 MB for 87 rows); in SQLite's
            // default 2 MiB cache that transaction spilled pages to the file
            // before its commit and read them back. The cache only grows to
            // what the cell touches.
            None => conn.execute_batch(&format!(
                "PRAGMA cache_size = -{}",
                crate::store::DEFAULT_CACHE_KIB
            ))?,
        }
        conn.execute_batch("PRAGMA foreign_keys = ON")?;
        Ok(engine)
    }

    /// Open an existing file read-only, for an operator's diagnosis (#1423):
    /// the main file is never written and a missing one is not created, the
    /// journal mode is left as stored, and a hot journal is refused rather
    /// than rolled back. A WAL file may still get `-wal`/`-shm` sidecars.
    ///
    /// # Errors
    /// SQLite open or configuration errors.
    pub fn open_read_only(path: &std::path::Path) -> Result<Self, CellError> {
        let conn = rusqlite::Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        Self::configured(conn)
    }

    /// Run `f` in one deferred read transaction, then end it without
    /// committing anything: one snapshot for a read-only pass.
    ///
    /// # Errors
    /// A nested call, SQLite errors, or `f`'s own.
    pub(crate) fn read_transaction<T>(
        &self,
        f: impl FnOnce() -> Result<T, CellError>,
    ) -> Result<T, CellError> {
        if !self.conn.is_autocommit() {
            return Err(CellError::Engine("nested transaction refused".to_owned()));
        }
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Deferred,
        )?;
        let out = f();
        let ended = tx.rollback();
        // `f`'s own error wins over a failure to end the read.
        let value = out?;
        ended?;
        Ok(value)
    }

    /// Open an in-memory database with foreign keys enforced.
    ///
    /// # Errors
    /// SQLite open or configuration errors.
    pub fn open_in_memory() -> Result<Self, CellError> {
        let conn = rusqlite::Connection::open_in_memory()?;
        conn.execute_batch("PRAGMA foreign_keys = ON")?;
        Self::configured(conn)
    }

    fn configured(mut conn: rusqlite::Connection) -> Result<Self, CellError> {
        use rusqlite::config::DbConfig;
        use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        // A double-quoted name that matches no column must be an error, never a
        // string literal: a stale property column name would otherwise read back
        // as its own text.
        conn.set_db_config(DbConfig::SQLITE_DBCONFIG_DQS_DML, false)?;
        conn.set_db_config(DbConfig::SQLITE_DBCONFIG_DQS_DDL, false)?;
        // The cell runs a bounded set of statement shapes; keep them prepared.
        conn.set_prepared_statement_cache_capacity(512);
        let user_sql = Arc::new(AtomicBool::new(false));
        let authorizer_flag = Arc::clone(&user_sql);
        let definitions = Arc::new(AtomicBool::new(false));
        let installing = Arc::clone(&definitions);
        let target = Arc::new(std::sync::Mutex::new(None::<String>));
        let statement_target = Arc::clone(&target);
        // Let SQLite parse transaction controls, including quoted SQL and
        // multi-statement batches. Only the adapter may control transactions.
        conn.authorizer(Some(move |context: AuthContext<'_>| {
            // S1 (#1547): the retained definitions are written by an install
            // alone, inside `definition_writes`. Any other write to them
            // (a statement, a trigger, or a foreign-key action, which SQLite
            // reports with no accessor) was added from outside: the schema is
            // shared by every connection to the file, and such a write would
            // ride this connection's own commit, which `data_version` does
            // not count, past the retained-actions cache. A trigger is never
            // the install's, so it is refused even then. SQLite names tables
            // case-insensitively, and so does this check.
            //
            // Inside the scope, a statement writes only the table it names:
            // SQLite reports a statement's own write before any it codes on
            // the statement's behalf, so the first write reported while the
            // statement is prepared is its target, and every write to
            // another table (a foreign-key action into a retained table, or
            // anywhere else) is refused, as is any trigger.
            let written = if let AuthAction::Insert { table_name }
            | AuthAction::Update { table_name, .. }
            | AuthAction::Delete { table_name } = context.action
            {
                Some(table_name)
            } else {
                None
            };
            let refused_write = written.is_some_and(|table| {
                if !installing.load(Ordering::Relaxed) {
                    return retained_table(table);
                }
                if context.accessor.is_some() {
                    return true;
                }
                // SQLite's own schema tables, which DDL writes as it runs,
                // carry no row of a statement's: they neither name nor
                // break its target.
                if table
                    .get(..7)
                    .is_some_and(|head| head.eq_ignore_ascii_case("sqlite_"))
                {
                    return false;
                }
                let Ok(mut target) = statement_target.lock() else {
                    return true;
                };
                match target.as_deref() {
                    None => {
                        *target = Some(table.to_owned());
                        false
                    }
                    Some(own) => !own.eq_ignore_ascii_case(table),
                }
            });
            if refused_write
                || (authorizer_flag.load(Ordering::Relaxed)
                    && matches!(
                        context.action,
                        AuthAction::Transaction { .. } | AuthAction::Savepoint { .. }
                    ))
            {
                Authorization::Deny
            } else {
                Authorization::Allow
            }
        }))?;
        Ok(Self {
            conn,
            user_sql,
            definitions,
            target,
            journal_mode: std::cell::Cell::new(None),
            checkpoint_key: None,
        })
    }

    /// The same engine holding the host's open-checkpoint key (an in-memory
    /// engine, which has no [`crate::StoreOptions`]).
    #[must_use]
    pub fn with_checkpoint_key(
        mut self,
        key: Option<crate::open_checkpoint::CheckpointKey>,
    ) -> Self {
        self.checkpoint_key = key;
        self
    }

    /// Close the native file before publishing a staged restore.
    ///
    /// # Errors
    /// SQLite close errors.
    pub fn close(self) -> Result<(), CellError> {
        self.conn.close().map_err(|(_conn, error)| error.into())
    }
}

/// The copying reference fork (FORK-PIN-1): SQLite's online backup copies
/// every committed page into a new in-memory database with the same
/// configuration. It is the oracle a copy-on-write engine must match.
#[cfg(feature = "native")]
impl crate::fork::ForkEngine for NativeEngine {
    fn fork_engine(&self) -> Result<Self, CellError> {
        if !self.conn.is_autocommit() {
            return Err(CellError::Engine(
                "a fork copies committed state; a transaction is open".to_owned(),
            ));
        }
        let mut conn = rusqlite::Connection::open_in_memory()?;
        {
            let backup = rusqlite::backup::Backup::new(&self.conn, &mut conn)?;
            if backup.step(-1)? != rusqlite::backup::StepResult::Done {
                return Err(CellError::Engine(
                    "fork copy did not complete in one step".to_owned(),
                ));
            }
        }
        conn.execute_batch("PRAGMA foreign_keys = ON")?;
        // A fork keeps the host's key: its checkpoint is the base's.
        Ok(Self::configured(conn)?.with_checkpoint_key(self.checkpoint_key))
    }
}

/// Test support: copy committed state to a file, as a fork copies it.
#[cfg(all(test, feature = "native"))]
impl NativeEngine {
    pub(crate) fn backup_to_file(&self, path: &std::path::Path) -> Result<(), CellError> {
        let mut file = rusqlite::Connection::open(path)?;
        let done = {
            let backup = rusqlite::backup::Backup::new(&self.conn, &mut file)?;
            backup.step(-1)?
        };
        if done == rusqlite::backup::StepResult::Done {
            Ok(())
        } else {
            Err(CellError::Engine(
                "backup did not complete in one step".to_owned(),
            ))
        }
    }
}

/// Whether `table` is one of the retained definition tables, named in any
/// ASCII case, as SQLite resolves it.
#[cfg(feature = "native")]
fn retained_table(table: &str) -> bool {
    ["_norn_definition_", "_norn_retained_"]
        .iter()
        .any(|prefix| {
            table
                .get(..prefix.len())
                .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
        })
}

/// Ends a [`SqlEngine::definition_writes`] scope, also on unwinding: the
/// retained definitions are refused again, and every statement prepared
/// inside the scope (authorized while they were writable) leaves the cache,
/// so none is reused outside it.
#[cfg(feature = "native")]
struct DefinitionWrites<'a> {
    engine: &'a NativeEngine,
}

#[cfg(feature = "native")]
impl Drop for DefinitionWrites<'_> {
    fn drop(&mut self) {
        self.engine.next_statement();
        self.engine
            .definitions
            .store(false, std::sync::atomic::Ordering::Relaxed);
        self.engine.conn.flush_prepared_statement_cache();
    }
}

#[cfg(feature = "native")]
impl NativeEngine {
    /// Inside a definition-writes scope, forget the previous statement's
    /// target before the next statement is prepared (S1, #1547).
    fn next_statement(&self) {
        if self.definitions.load(std::sync::atomic::Ordering::Relaxed)
            && let Ok(mut target) = self.target.lock()
        {
            *target = None;
        }
    }
}

#[cfg(feature = "native")]
struct UserSql<'a>(&'a std::sync::atomic::AtomicBool);

#[cfg(feature = "native")]
impl<'a> UserSql<'a> {
    fn enter(flag: &'a std::sync::atomic::AtomicBool) -> Self {
        flag.store(true, std::sync::atomic::Ordering::Relaxed);
        Self(flag)
    }
}

#[cfg(feature = "native")]
impl Drop for UserSql<'_> {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::Relaxed);
    }
}

#[cfg(feature = "native")]
impl rusqlite::types::ToSql for SqlValue {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        use rusqlite::types::{ToSqlOutput, ValueRef};
        Ok(ToSqlOutput::Borrowed(match self {
            Self::Null => ValueRef::Null,
            Self::Integer(value) => ValueRef::Integer(*value),
            Self::Real(value) => ValueRef::Real(*value),
            Self::Text(value) => ValueRef::Text(value.as_bytes()),
            Self::Blob(value) => ValueRef::Blob(value),
        }))
    }
}

/// Counts one native statement for the commit-path counters (#1421): the
/// statement, the rows it changed, and its bound bytes when it changed any.
/// A zero-sized no-op without the `counters` feature.
#[cfg(feature = "native")]
struct StatementCount<'a> {
    #[cfg(feature = "counters")]
    conn: &'a rusqlite::Connection,
    #[cfg(feature = "counters")]
    before: u64,
    #[cfg(feature = "counters")]
    bytes: u64,
    #[cfg(not(feature = "counters"))]
    conn: std::marker::PhantomData<&'a rusqlite::Connection>,
}

#[cfg(feature = "native")]
impl<'a> StatementCount<'a> {
    #[inline]
    #[cfg_attr(
        not(feature = "counters"),
        allow(unused_variables, reason = "counting is compiled out")
    )]
    fn start(conn: &'a rusqlite::Connection, params: &[SqlValue]) -> Self {
        #[cfg(feature = "counters")]
        {
            norn_ir::counters::bump(norn_ir::counters::Counter::SqlStatements);
            let bytes = params
                .iter()
                .map(|value| match value {
                    SqlValue::Null => 0,
                    SqlValue::Integer(_) | SqlValue::Real(_) => 8,
                    SqlValue::Text(text) => u64::try_from(text.len()).unwrap_or(u64::MAX),
                    SqlValue::Blob(blob) => u64::try_from(blob.len()).unwrap_or(u64::MAX),
                })
                .fold(0_u64, u64::saturating_add);
            Self {
                conn,
                before: conn.total_changes(),
                bytes,
            }
        }
        #[cfg(not(feature = "counters"))]
        Self {
            conn: std::marker::PhantomData,
        }
    }
}

#[cfg(all(feature = "native", feature = "counters"))]
impl Drop for StatementCount<'_> {
    fn drop(&mut self) {
        let rows = self.conn.total_changes().saturating_sub(self.before);
        norn_ir::counters::add(norn_ir::counters::Counter::RowsWritten, rows);
        if rows > 0 {
            norn_ir::counters::add(norn_ir::counters::Counter::BytesWritten, self.bytes);
        }
    }
}

#[cfg(feature = "native")]
impl SqlEngine for NativeEngine {
    fn read_isolation(&self) -> ReadIsolation {
        ReadIsolation::Revisioned
    }

    fn checkpoint_key(&self) -> Option<crate::open_checkpoint::CheckpointKey> {
        self.checkpoint_key
    }

    fn admitted(&self) -> Result<(), CellError> {
        if let Some(mode) = self.journal_mode.get() {
            let _applied: String =
                self.conn
                    .query_row(&format!("PRAGMA journal_mode = {mode}"), [], |row| {
                        row.get(0)
                    })?;
            self.journal_mode.set(None);
        }
        Ok(())
    }

    fn definition_writes<T>(
        &self,
        f: impl FnOnce() -> Result<T, CellError>,
    ) -> Result<T, CellError> {
        if self
            .definitions
            .swap(true, std::sync::atomic::Ordering::Relaxed)
        {
            return Err(CellError::Engine(
                "nested definition writes refused".to_owned(),
            ));
        }
        let _scope = DefinitionWrites { engine: self };
        self.next_statement();
        f()
    }

    fn read_cache_revision(&self) -> Result<Option<(u64, u64)>, CellError> {
        if !self.conn.is_autocommit() {
            return Ok(None);
        }
        let Some(external) = self.external_data_version()? else {
            return Ok(None);
        };
        Ok(Some((self.conn.total_changes(), external)))
    }

    fn external_data_version(&self) -> Result<Option<u64>, CellError> {
        let _counted = StatementCount::start(&self.conn, &[]);
        let external: i64 = self
            .conn
            .query_row("PRAGMA data_version", [], |row| row.get(0))?;
        let external = u64::try_from(external)
            .map_err(|_negative| CellError::OutOfRange("SQLite cache revision"))?;
        Ok(Some(external))
    }

    fn execute(&self, sql: &str, params: &[SqlValue]) -> Result<(), CellError> {
        let _counted = StatementCount::start(&self.conn, params);
        validate_bindings(sql, params)?;
        let _user_sql = UserSql::enter(&self.user_sql);
        self.next_statement();
        // Every statement is prepared under the same authorizer state (the
        // user-SQL flag is set for every call; a definition-writes scope
        // flushes what it prepared when it ends), so a cached statement was
        // authorized exactly as a fresh one would be; a denied statement
        // fails to prepare and is never cached.
        self.conn
            .prepare_cached(sql)?
            .execute(rusqlite::params_from_iter(params))?;
        Ok(())
    }

    fn query(&self, sql: &str, params: &[SqlValue]) -> Result<Vec<Vec<SqlValue>>, CellError> {
        let _counted = StatementCount::start(&self.conn, params);
        validate_bindings(sql, params)?;
        let _user_sql = UserSql::enter(&self.user_sql);
        self.next_statement();
        let mut statement = self.conn.prepare_cached(sql)?;
        let width = statement.column_count();
        let mut rows = statement.query(rusqlite::params_from_iter(params))?;
        let mut result = Vec::new();
        while let Some(row) = rows.next()? {
            let mut values = Vec::with_capacity(width);
            for index in 0..width {
                use rusqlite::types::ValueRef;
                let value = match row.get_ref(index)? {
                    ValueRef::Null => SqlValue::Null,
                    ValueRef::Integer(value) => SqlValue::Integer(value),
                    ValueRef::Real(value) => SqlValue::Real(value),
                    ValueRef::Text(value) => SqlValue::Text(
                        std::str::from_utf8(value)
                            .map_err(|error| CellError::Engine(error.to_string()))?
                            .to_owned(),
                    ),
                    ValueRef::Blob(value) => SqlValue::Blob(value.to_vec()),
                };
                validate_value(&value)?;
                values.push(value);
            }
            result.push(values);
        }
        Ok(result)
    }

    fn execute_batch(&self, sql: &str) -> Result<(), CellError> {
        let _counted = StatementCount::start(&self.conn, &[]);
        validate_bindings(sql, &[])?;
        let _user_sql = UserSql::enter(&self.user_sql);
        if !self.definitions.load(std::sync::atomic::Ordering::Relaxed) {
            self.conn.execute_batch(sql)?;
            return Ok(());
        }
        // Inside a definition-writes scope each statement of the batch is
        // prepared with its own target, as `execute_batch` steps it: once.
        let mut batch = rusqlite::Batch::new(&self.conn, sql);
        loop {
            self.next_statement();
            let Some(mut statement) =
                rusqlite::fallible_iterator::FallibleIterator::next(&mut batch)?
            else {
                return Ok(());
            };
            let mut rows = statement.raw_query();
            while rows.next()?.is_some() {}
        }
    }

    fn transaction<T, X: From<CellError>>(&self, f: impl FnOnce() -> Result<T, X>) -> Result<T, X> {
        if !self.conn.is_autocommit() {
            return Err(CellError::Engine("nested transaction refused".to_owned()).into());
        }
        // RAII rolls back on error and unwinding. SQL in the closure uses the
        // same connection while this immediate transaction owns its writer.
        let tx = rusqlite::Transaction::new_unchecked(
            &self.conn,
            rusqlite::TransactionBehavior::Immediate,
        )
        .map_err(CellError::from)?;
        match f() {
            Ok(value) => {
                tx.commit().map_err(CellError::from)?;
                Ok(value)
            }
            Err(error) => {
                tx.rollback().map_err(CellError::from)?;
                Err(error)
            }
        }
    }
}

#[cfg(feature = "fault-injection")]
mod fault;
#[cfg(all(feature = "fault-injection", feature = "native"))]
pub use fault::{DoEngine, DoStorage, DoStorageEmulator, Thrown};
#[cfg(feature = "fault-injection")]
pub use fault::{EngineFault, EngineFaultKind, FaultEngine, FaultSwitch};

/// S1 (#1547): the retained definitions are written by an install alone,
/// whatever carries the write.
#[cfg(all(test, feature = "native"))]
mod retained_writes {
    #![allow(clippy::expect_used, reason = "test")]

    use super::{NativeEngine, SqlEngine as _};

    /// A native cell file and, through another connection to it (an
    /// out-of-process editor), a parent table whose rows a foreign-key action
    /// cascades into a retained receipt table, and a retained table the
    /// editor created under a mixed-case name (SQLite reports the name as
    /// stored), which a trigger writes.
    fn edited() -> (tempfile::TempDir, NativeEngine) {
        let dir = tempfile::tempdir().expect("directory");
        let path = dir.path().join("cell.sqlite");
        let engine =
            NativeEngine::open(&path, crate::store::StoreOptions::default()).expect("engine opens");
        rusqlite::Connection::open(&path)
            .expect("editor connection")
            .execute_batch(
                r#"CREATE TABLE "editor_parent" ("id" INTEGER PRIMARY KEY);
                CREATE TABLE "editor_log" ("id" INTEGER PRIMARY KEY);
                CREATE TABLE "_norn_definition_admission_receipt" (
                  "version" INTEGER PRIMARY KEY,
                  "receipt" TEXT NOT NULL,
                  "parent" INTEGER REFERENCES "editor_parent" ("id") ON DELETE CASCADE
                );
                INSERT INTO "editor_parent" VALUES (1);
                INSERT INTO "_norn_definition_admission_receipt" VALUES (1, 'receipt', 1);
                CREATE TABLE "_NORN_Retained_Definition" ("id" TEXT PRIMARY KEY);
                INSERT INTO "_NORN_Retained_Definition" VALUES ('kept');
                CREATE TRIGGER "editor_rewrite" AFTER INSERT ON "editor_log" BEGIN
                  DELETE FROM "_norn_retained_definition";
                END;"#,
            )
            .expect("editor schema");
        (dir, engine)
    }

    fn receipts(engine: &NativeEngine) -> i64 {
        engine
            .query_row(
                r#"SELECT COUNT(*) FROM "_norn_definition_admission_receipt""#,
                &[],
                |row| row.get(0),
            )
            .expect("count")
    }

    #[test]
    fn a_foreign_key_cascade_into_a_retained_table_is_refused() {
        let (_dir, engine) = edited();
        assert!(
            engine
                .execute(r#"DELETE FROM "editor_parent""#, &[])
                .is_err(),
            "the cascade would delete a retained receipt on this connection"
        );
        assert_eq!(receipts(&engine), 1);
    }

    #[test]
    fn a_retained_table_named_in_another_case_is_refused() {
        let (_dir, engine) = edited();
        assert!(
            engine
                .execute(r#"INSERT INTO "editor_log" VALUES (1)"#, &[])
                .is_err(),
            "the trigger writes a retained table stored under a mixed-case name"
        );
        assert!(
            engine
                .execute(r#"DELETE FROM "_norn_retained_definition""#, &[])
                .is_err()
        );
        let kept: i64 = engine
            .query_row(
                r#"SELECT COUNT(*) FROM "_norn_retained_definition""#,
                &[],
                |row| row.get(0),
            )
            .expect("count");
        assert_eq!(kept, 1);
    }
}

/// S1 (#1547): an install's own writes, and only while it runs.
#[cfg(all(test, feature = "native"))]
mod definition_writes {
    #![allow(clippy::expect_used, reason = "test")]

    use super::{NativeEngine, SqlEngine as _, SqlValue};

    #[test]
    fn only_a_definition_writes_scope_writes_and_its_statements_are_not_reused() {
        let engine = NativeEngine::open_in_memory().expect("engine");
        engine
            .execute_batch(
                r#"CREATE TABLE "_norn_definition_active" ("id" INTEGER PRIMARY KEY, "version" INTEGER NOT NULL)"#,
            )
            .expect("table");
        let write = r#"INSERT INTO "_norn_definition_active" ("id", "version") VALUES (?1, 1)"#;
        assert!(engine.execute(write, &[SqlValue::Integer(1)]).is_err());
        engine
            .definition_writes(|| engine.execute(write, &[SqlValue::Integer(1)]))
            .expect("an install writes");
        // The statement prepared inside the scope is not served from the
        // cache outside it: it is authorized again, and refused.
        assert!(engine.execute(write, &[SqlValue::Integer(2)]).is_err());
        assert!(
            engine
                .definition_writes(|| engine.definition_writes(|| Ok(())))
                .is_err(),
            "scopes do not nest"
        );
        assert!(engine.execute(write, &[SqlValue::Integer(3)]).is_err());
        let rows: i64 = engine
            .query_row(
                r#"SELECT COUNT(*) FROM "_norn_definition_active""#,
                &[],
                |row| row.get(0),
            )
            .expect("count");
        assert_eq!(rows, 1);
    }

    #[test]
    fn definition_writes_clears_target_on_drop_and_steps_multi_row_batch_statements() {
        let engine = NativeEngine::open_in_memory().expect("engine");
        engine
            .execute_batch(
                r#"CREATE TABLE "_norn_definition_active" ("id" INTEGER PRIMARY KEY, "version" INTEGER NOT NULL)"#,
            )
            .expect("table");
        engine
            .definition_writes(|| {
                engine.execute_batch(
                    r#"INSERT INTO "_norn_definition_active" ("id", "version") VALUES (1, 1), (2, 1) RETURNING "id""#,
                )
            })
            .expect("multi-row batch succeeds");
        assert!(engine.target.lock().expect("target").is_none());
        let rows: i64 = engine
            .query_row(
                r#"SELECT COUNT(*) FROM "_norn_definition_active""#,
                &[],
                |row| row.get(0),
            )
            .expect("count");
        assert_eq!(rows, 2);
    }

    #[test]
    fn read_only_sql_classification_handles_case_and_write_keywords() {
        assert!(super::read_only("  select 1 ; "));
        assert!(super::read_only("SELECT id FROM items WHERE name = 'ok'"));
        assert!(!super::read_only("SELECT 1; SELECT 2"));
        assert!(!super::read_only("SELECT * FROM t; DELETE FROM t"));
        assert!(!super::read_only("INSERT INTO t VALUES (1)"));
        assert!(!super::read_only(
            "SELECT 1 WHERE EXISTS (UPDATE t SET a = 1)"
        ));
        assert!(!super::read_only("—SELECT 1"));
    }
}
