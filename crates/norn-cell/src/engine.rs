//! Synchronous SQLite execution supplied by the cell's host.
//!
//! Values own their data; no native driver or JavaScript type crosses this
//! boundary. A transaction runs the closure against the same engine, commits
//! on success and rolls back on error. Hosts must reject nested transactions.
//! Integer properties outside ±2^53 travel as exact text, with SQLite INTEGER
//! affinity on writes and text projections on reads; counters stay bounded.
//! The default `native` feature supplies [`NativeEngine`]; portable hosts
//! supply their own implementation (including durability and isolation).

use crate::CellError;

/// Largest SQLite integer carried exactly by a JavaScript number.
pub const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_992;
/// Maximum number of bindings in the admitted host subset.
pub const MAX_PARAMETERS: usize = 100;
/// Maximum SQL statement length in bytes in the admitted host subset.
pub const MAX_STATEMENT_BYTES: usize = 100_000;

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
}

#[cfg(feature = "native")]
impl NativeEngine {
    /// Open a file with the existing native journal and cache options.
    ///
    /// # Errors
    /// SQLite open or configuration errors.
    pub fn open(
        path: &std::path::Path,
        options: crate::store::StoreOptions,
    ) -> Result<Self, CellError> {
        let conn = rusqlite::Connection::open(path)?;
        let mode = match options.journal_mode {
            crate::store::JournalMode::Delete => "DELETE",
            crate::store::JournalMode::Wal => "WAL",
        };
        let _applied: String =
            conn.query_row(&format!("PRAGMA journal_mode = {mode}"), [], |row| {
                row.get(0)
            })?;
        conn.execute_batch("PRAGMA synchronous = FULL")?;
        if let Some(pages) = options.cache_pages {
            conn.execute_batch(&format!("PRAGMA cache_size = {pages}"))?;
        }
        conn.execute_batch("PRAGMA foreign_keys = ON")?;
        Self::configured(conn)
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

    fn configured(conn: rusqlite::Connection) -> Result<Self, CellError> {
        use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let user_sql = Arc::new(AtomicBool::new(false));
        let authorizer_flag = Arc::clone(&user_sql);
        // Let SQLite parse transaction controls, including quoted SQL and
        // multi-statement batches. Only the adapter may control transactions.
        conn.authorizer(Some(move |context: AuthContext<'_>| {
            if authorizer_flag.load(Ordering::Relaxed)
                && matches!(
                    context.action,
                    AuthAction::Transaction { .. } | AuthAction::Savepoint { .. }
                )
            {
                Authorization::Deny
            } else {
                Authorization::Allow
            }
        }))?;
        Ok(Self { conn, user_sql })
    }

    /// Close the native file before publishing a staged restore.
    ///
    /// # Errors
    /// SQLite close errors.
    pub fn close(self) -> Result<(), CellError> {
        self.conn.close().map_err(|(_conn, error)| error.into())
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

#[cfg(feature = "native")]
impl SqlEngine for NativeEngine {
    fn execute(&self, sql: &str, params: &[SqlValue]) -> Result<(), CellError> {
        validate_bindings(sql, params)?;
        let _user_sql = UserSql::enter(&self.user_sql);
        self.conn.execute(sql, rusqlite::params_from_iter(params))?;
        Ok(())
    }

    fn query(&self, sql: &str, params: &[SqlValue]) -> Result<Vec<Vec<SqlValue>>, CellError> {
        validate_bindings(sql, params)?;
        let _user_sql = UserSql::enter(&self.user_sql);
        let mut statement = self.conn.prepare(sql)?;
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
        validate_bindings(sql, &[])?;
        let _user_sql = UserSql::enter(&self.user_sql);
        self.conn.execute_batch(sql)?;
        Ok(())
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
