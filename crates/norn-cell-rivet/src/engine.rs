//! `RivetEngine`: [`SqlEngine`] over one Rivet actor's embedded SQLite.
//!
//! The seam is synchronous and Rivet's SQLite is async, so the engine lives on
//! the actor's cell thread ([`crate::thread`]) and blocks on each statement
//! from there. With `RivetKit`'s `sqlite-local` feature SQLite runs in this
//! process on Rivet's VFS; a commit returns once the VFS commit is fsynced on
//! a majority of the control plane's instances (#1130, spike section 1).
//!
//! A transaction is one `RivetKit` manual transaction (`begin_transaction`, then
//! `commit` or `rollback`). Norn refuses a nested transaction before the
//! platform sees it, and refuses SQL that would control a transaction itself.

use std::cell::RefCell;
use std::time::Duration;

use norn_cell::CellError;
use norn_cell::engine::{ReadIsolation, SqlEngine, SqlValue, WriteRevision, validate_bindings};
use rivetkit::{BindParam, ColumnValue, SqliteDb, SqliteTransaction};
use tokio::runtime::Handle;

/// How long one commit's transaction may stay open before `RivetKit` expires
/// it. A provisioning set is the longest ordinary commit (thousands of
/// statements, well under a second); a conformance hold spins at most 40 s.
const TRANSACTION_TIMEOUT: Duration = Duration::from_secs(120);

/// Tables the host platform keeps in the actor's own database: `RivetKit`'s
/// internal storage (state, KV, queue, schedules) and this host's alarm and
/// scope rows ([`crate::alarm`]). They are never cell state.
pub const PLATFORM_TABLE_GLOBS: &[&str] = &["_rivet*", "__rivet*", "_norn_host_*"];

/// A hook into every statement the engine runs, for the host conformance
/// seams only (the `conformance` module); production builds install none.
pub trait StatementHook {
    /// A transaction began.
    fn begin(&self);
    /// A statement ran; `in_transaction` when inside [`SqlEngine::transaction`].
    fn ran(&self, in_transaction: bool);
    /// The outermost transaction's body returned `Ok` and is about to commit.
    fn before_commit(&self);
}

/// One actor's SQLite, driven from its cell thread.
pub struct RivetEngine {
    db: SqliteDb,
    runtime: Handle,
    active: RefCell<Option<SqliteTransaction>>,
    revision: WriteRevision,
    hook: Option<std::rc::Rc<dyn StatementHook>>,
}

impl std::fmt::Debug for RivetEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RivetEngine")
            .field("in_transaction", &self.active.borrow().is_some())
            .finish_non_exhaustive()
    }
}

impl RivetEngine {
    /// The engine over `db`. `runtime` runs `RivetKit`'s futures; the calling
    /// thread must not be one of its workers (it blocks on them).
    #[must_use]
    pub fn new(db: SqliteDb, runtime: Handle) -> Self {
        Self {
            db,
            runtime,
            active: RefCell::new(None),
            revision: WriteRevision::new(),
            hook: None,
        }
    }

    /// The same engine with a statement hook (conformance seams).
    #[must_use]
    pub fn with_hook(mut self, hook: std::rc::Rc<dyn StatementHook>) -> Self {
        self.hook = Some(hook);
        self
    }

    fn block<T>(&self, future: impl Future<Output = anyhow::Result<T>>) -> Result<T, CellError> {
        self.runtime
            .block_on(future)
            .map_err(|error| backend(&error))
    }

    fn note(&self) {
        if let Some(hook) = &self.hook {
            hook.ran(self.active.borrow().is_some());
        }
    }

    fn run(&self, sql: &str, params: &[SqlValue]) -> Result<Vec<Vec<SqlValue>>, CellError> {
        validate_bindings(sql, params)?;
        refuse_transaction_control(sql)?;
        self.revision.note(sql);
        let bindings = params.iter().map(encode).collect::<Vec<_>>();
        let active = self.active.borrow().clone();
        let result = match active {
            Some(tx) => self.block(tx.execute(sql, Some(bindings))),
            None => self.block(self.db.execute(sql, Some(bindings))),
        };
        self.note();
        result?
            .rows
            .into_iter()
            .map(|row| row.into_iter().map(decode).collect())
            .collect()
    }
}

fn backend(error: &anyhow::Error) -> CellError {
    CellError::Engine(format!("Rivet SQLite: {error:#}"))
}

fn encode(value: &SqlValue) -> BindParam {
    match value {
        SqlValue::Null => BindParam::Null,
        SqlValue::Integer(value) => BindParam::Integer(*value),
        SqlValue::Real(value) => BindParam::Float(*value),
        SqlValue::Text(value) => BindParam::Text(value.clone()),
        SqlValue::Blob(value) => BindParam::Blob(value.clone()),
    }
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "every engine decodes through a fallible step; Rivet's values are all representable"
)]
fn decode(value: ColumnValue) -> Result<SqlValue, CellError> {
    Ok(match value {
        ColumnValue::Null => SqlValue::Null,
        ColumnValue::Integer(value) => SqlValue::Integer(value),
        ColumnValue::Float(value) => SqlValue::Real(value),
        ColumnValue::Text(value) => SqlValue::Text(value),
        ColumnValue::Blob(value) => SqlValue::Blob(value),
    })
}

/// Refuses SQL that controls a transaction: only the adapter may, as on
/// native SQLite (its authorizer) and the Durable Object (the platform). Each
/// statement's first keyword is read past comments, outside quoted text.
fn refuse_transaction_control(sql: &str) -> Result<(), CellError> {
    const CONTROL: &[&str] = &["BEGIN", "COMMIT", "END", "ROLLBACK", "SAVEPOINT", "RELEASE"];
    for statement in statements(sql) {
        let word = first_keyword(statement).to_ascii_uppercase();
        if CONTROL.contains(&word.as_str()) {
            return Err(CellError::Engine(
                "transaction control is the adapter's".to_owned(),
            ));
        }
    }
    Ok(())
}

/// `sql` split at each `;` outside quotes and comments.
fn statements(sql: &str) -> Vec<&str> {
    let bytes = sql.as_bytes();
    let mut out = Vec::new();
    let mut start = 0;
    let mut index = 0;
    while let Some(&byte) = bytes.get(index) {
        let next = bytes.get(index.saturating_add(1)).copied();
        match byte {
            b'\'' | b'"' | b'`' => {
                index = skip_until(bytes, index.saturating_add(1), byte);
            }
            b'[' => {
                index = skip_until(bytes, index.saturating_add(1), b']');
            }
            b'-' if next == Some(b'-') => {
                index = skip_until(bytes, index, b'\n');
            }
            b'/' if next == Some(b'*') => {
                index = skip_block_comment(bytes, index.saturating_add(2));
            }
            b';' => {
                out.push(sql.get(start..index).unwrap_or_default());
                start = index.saturating_add(1);
                index = start;
            }
            _ => {
                index = index.saturating_add(1);
            }
        }
    }
    out.push(sql.get(start..).unwrap_or_default());
    out
}

/// The index just past the next `end` at or after `from` (or the end).
fn skip_until(bytes: &[u8], from: usize, end: u8) -> usize {
    bytes
        .iter()
        .skip(from)
        .position(|&byte| byte == end)
        .map_or(bytes.len(), |offset| {
            from.saturating_add(offset).saturating_add(1)
        })
}

fn skip_block_comment(bytes: &[u8], from: usize) -> usize {
    let mut index = from;
    while let Some(&byte) = bytes.get(index) {
        if byte == b'*' && bytes.get(index.saturating_add(1)) == Some(&b'/') {
            return index.saturating_add(2);
        }
        index = index.saturating_add(1);
    }
    bytes.len()
}

/// The statement's first word, past whitespace and comments.
fn first_keyword(statement: &str) -> &str {
    let mut rest = statement;
    loop {
        rest = rest.trim_start();
        if let Some(line) = rest.strip_prefix("--") {
            rest = line.split_once('\n').map_or("", |(_, after)| after);
        } else if let Some(block) = rest.strip_prefix("/*") {
            rest = block.split_once("*/").map_or("", |(_, after)| after);
        } else {
            break;
        }
    }
    let end = rest
        .find(|c: char| !c.is_ascii_alphanumeric() && c != '_')
        .unwrap_or(rest.len());
    rest.get(..end).unwrap_or_default()
}

/// Ends the open transaction on every exit, unwinding included: whatever is
/// still active when this drops is rolled back.
struct Active<'a>(&'a RivetEngine);

impl Drop for Active<'_> {
    fn drop(&mut self) {
        let open = self.0.active.borrow_mut().take();
        if let Some(tx) = open {
            // A rollback that fails leaves the transaction to RivetKit's
            // expiry; nothing of it was committed.
            if let Err(error) = self.0.runtime.block_on(tx.rollback()) {
                tracing::warn!(error = %error, "rollback failed; the transaction expires");
            }
        }
    }
}

impl SqlEngine for RivetEngine {
    fn platform_table_globs(&self) -> &'static [&'static str] {
        PLATFORM_TABLE_GLOBS
    }

    /// Every request to the actor runs on its one cell thread, in order, and
    /// no other code writes cell tables (`RivetKit` writes only its own), so
    /// nothing writes between two reads of one call.
    fn read_isolation(&self) -> ReadIsolation {
        ReadIsolation::ExclusiveSynchronous
    }

    /// The cell thread is the cell tables' only writer, so a count of the
    /// writes it ran is a revision.
    fn read_cache_revision(&self) -> Result<Option<(u64, u64)>, CellError> {
        Ok(self.revision.current(self.active.borrow().is_some()))
    }

    fn execute(&self, sql: &str, params: &[SqlValue]) -> Result<(), CellError> {
        self.run(sql, params).map(|_| ())
    }

    fn query(&self, sql: &str, params: &[SqlValue]) -> Result<Vec<Vec<SqlValue>>, CellError> {
        self.run(sql, params)
    }

    fn execute_batch(&self, sql: &str) -> Result<(), CellError> {
        validate_bindings(sql, &[])?;
        refuse_transaction_control(sql)?;
        self.revision.bump();
        let active = self.active.borrow().clone();
        let result = match active {
            Some(tx) => self.block(tx.exec(sql)),
            None => self.block(self.db.exec(sql)),
        };
        self.note();
        result.map(|_| ())
    }

    fn transaction<T, X: From<CellError>>(&self, f: impl FnOnce() -> Result<T, X>) -> Result<T, X> {
        if self.active.borrow().is_some() {
            return Err(CellError::Engine("nested transaction refused".to_owned()).into());
        }
        let tx = self.block(self.db.begin_transaction(Some(TRANSACTION_TIMEOUT)))?;
        *self.active.borrow_mut() = Some(tx);
        let guard = Active(self);
        self.revision.bump();
        if let Some(hook) = &self.hook {
            hook.begin();
        }
        let value = f()?;
        if let Some(hook) = &self.hook {
            hook.before_commit();
        }
        let tx = self.active.borrow_mut().take();
        drop(guard);
        let Some(tx) = tx else {
            return Err(
                CellError::Engine("the transaction ended inside its body".to_owned()).into(),
            );
        };
        if let Err(error) = self.runtime.block_on(tx.commit()) {
            // A commit whose outcome is unknown fails closed in Rivet's VFS
            // and the actor restarts; the caller retries idempotently.
            if let Err(rollback) = self.runtime.block_on(tx.rollback()) {
                tracing::warn!(error = %rollback, "rollback after a failed commit failed");
            }
            return Err(backend(&error).into());
        }
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::{first_keyword, refuse_transaction_control, statements};

    #[test]
    fn splits_outside_quotes_and_comments() {
        assert_eq!(
            statements("SELECT ';'; -- a;b\nSELECT 2 /* ; */; SELECT [x;y]"),
            vec!["SELECT ';'", " -- a;b\nSELECT 2 /* ; */", " SELECT [x;y]"]
        );
    }

    #[test]
    fn refuses_transaction_control_anywhere_in_a_batch() {
        assert!(refuse_transaction_control("COMMIT").is_err());
        assert!(refuse_transaction_control("  /* c */ rollback to x").is_err());
        assert!(refuse_transaction_control("CREATE TABLE t (a); SAVEPOINT s").is_err());
        assert!(refuse_transaction_control("SELECT CASE WHEN 1 THEN 2 END").is_ok());
        assert!(refuse_transaction_control("INSERT INTO t VALUES ('BEGIN;')").is_ok());
        assert_eq!(first_keyword("-- x\n  END"), "END");
    }
}
