//! [`CowEngine`]: a native SQL engine whose database lives in a
//! [`PageStore`], with O(1) forks.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use norn_cell::CellError;
use norn_cell::engine::{NativeEngine, ReadIsolation, SqlEngine, SqlValue};
use norn_cell::fork::ForkEngine;

use crate::store::{PageError, PageStore, Root};
use crate::vfs;

static NEXT_FILE: AtomicU64 = AtomicU64::new(1);

fn page_error(error: &PageError) -> CellError {
    CellError::Engine(format!("page store: {error}"))
}

/// Owns one page-store file; removing the file on drop makes its unique
/// pages unreachable, so the next collection frees them (discard).
#[derive(Debug)]
struct FileLease {
    store: Arc<PageStore>,
    file: String,
}

impl Drop for FileLease {
    fn drop(&mut self) {
        if let Err(error) = self.store.delete_file(&self.file) {
            tracing::warn!(%error, file = %self.file, "page-store file was not released");
        }
    }
}

/// A [`NativeEngine`] over a page-store file. SQL, transactions and the
/// authorizer are the native engine's; only storage differs. Forking copies
/// the file's root: no page is copied, and the fork reads exactly what a full
/// copy would (L4, checked against the copying reference engine).
///
/// SQLite runs with exclusive locking and an in-memory rollback journal, one
/// connection per file. This tier is in memory; durability is a later unit.
#[derive(Debug)]
pub struct CowEngine {
    // Field order is drop order: close the connection, then release the file.
    native: NativeEngine,
    lease: FileLease,
}

impl CowEngine {
    /// A new, empty database in `store`.
    ///
    /// # Errors
    /// VFS registration, page-store or SQLite open errors.
    pub fn create(store: &Arc<PageStore>) -> Result<Self, CellError> {
        let file = Self::next_file();
        store
            .create_file(&file)
            .map_err(|error| page_error(&error))?;
        Self::open(store, file)
    }

    fn next_file() -> String {
        format!("db{}", NEXT_FILE.fetch_add(1, Ordering::Relaxed))
    }

    fn open(store: &Arc<PageStore>, file: String) -> Result<Self, CellError> {
        // Lease first, so a failed open still releases the file.
        let lease = FileLease {
            store: Arc::clone(store),
            file,
        };
        vfs::ensure_registered().map_err(|code| {
            CellError::Engine(format!("norn-pages VFS registration failed: {code}"))
        })?;
        let native =
            NativeEngine::open_with_vfs(&vfs::path(store.id(), &lease.file), vfs::VFS_NAME)?;
        Ok(Self { native, lease })
    }

    /// The database's current root: equal roots hold equal bytes.
    ///
    /// # Errors
    /// A page-store failure.
    pub fn root(&self) -> Result<Root, CellError> {
        self.lease
            .store
            .root(&self.lease.file)
            .map_err(|error| page_error(&error))
    }

    /// The page store this database lives in.
    #[must_use]
    pub const fn store(&self) -> &Arc<PageStore> {
        &self.lease.store
    }
}

impl ForkEngine for CowEngine {
    fn fork_engine(&self) -> Result<Self, CellError> {
        if !self.native.is_autocommit() {
            return Err(CellError::Engine(
                "a fork copies committed state; a transaction is open".to_owned(),
            ));
        }
        let file = Self::next_file();
        self.lease
            .store
            .fork_file(&self.lease.file, &file)
            .map_err(|error| page_error(&error))?;
        Self::open(&self.lease.store, file)
    }
}

impl SqlEngine for CowEngine {
    fn execute(&self, sql: &str, params: &[SqlValue]) -> Result<(), CellError> {
        self.native.execute(sql, params)
    }

    fn query(&self, sql: &str, params: &[SqlValue]) -> Result<Vec<Vec<SqlValue>>, CellError> {
        self.native.query(sql, params)
    }

    fn execute_batch(&self, sql: &str) -> Result<(), CellError> {
        self.native.execute_batch(sql)
    }

    fn transaction<T, X: From<CellError>>(&self, f: impl FnOnce() -> Result<T, X>) -> Result<T, X> {
        self.native.transaction(f)
    }

    fn platform_table_globs(&self) -> &'static [&'static str] {
        self.native.platform_table_globs()
    }

    fn read_isolation(&self) -> ReadIsolation {
        self.native.read_isolation()
    }

    fn read_cache_revision(&self) -> Result<Option<(u64, u64)>, CellError> {
        self.native.read_cache_revision()
    }
}
