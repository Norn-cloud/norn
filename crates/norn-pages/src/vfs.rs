//! The SQLite VFS that serves page-store files.
//!
//! One process-wide VFS named [`VFS_NAME`] serves every [`PageStore`]: a main
//! database path is `/<store id>/<file>`, and the store is looked up in a
//! registry of live stores. Journals and temporary files are private
//! in-memory buffers; the engine runs SQLite with an in-memory journal and
//! exclusive locking, one connection per file, so file locks are no-ops.
//!
//! The FFI lives in `sqlite-plugin`; this module is safe Rust.

use std::collections::BTreeMap;
use std::ffi::CString;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use sqlite_plugin::flags::{AccessFlags, LockLevel, OpenKind, OpenOpts};
use sqlite_plugin::vars;
use sqlite_plugin::vfs::{RegisterOpts, Vfs, VfsHandle, VfsResult, register_static};

use crate::store::{PageError, PageStore};

/// The registered VFS name.
pub const VFS_NAME: &str = "norn-pages";

static STORES: Mutex<BTreeMap<u64, Weak<PageStore>>> = Mutex::new(BTreeMap::new());
static NEXT_STORE: AtomicU64 = AtomicU64::new(1);
static REGISTERED: OnceLock<Result<(), i32>> = OnceLock::new();

/// Register the VFS once per process.
pub(crate) fn ensure_registered() -> Result<(), i32> {
    *REGISTERED.get_or_init(|| {
        let name = CString::new(VFS_NAME).map_err(|_nul| vars::SQLITE_INTERNAL)?;
        register_static(
            name,
            PagesVfs,
            RegisterOpts {
                make_default: false,
            },
        )
        .map(|_logger| ())
    })
}

/// Give a new store its process-unique id and make it reachable by path.
pub(crate) fn enroll(store: Weak<PageStore>) -> u64 {
    let id = NEXT_STORE.fetch_add(1, Ordering::Relaxed);
    if let Ok(mut stores) = STORES.lock() {
        stores.retain(|_id, store| store.strong_count() > 0);
        stores.insert(id, store);
    }
    id
}

/// The VFS path of `file` in the store with id `store`.
pub(crate) fn path(store: u64, file: &str) -> String {
    format!("/{store}/{file}")
}

fn resolve(path: &str) -> Option<(Arc<PageStore>, String)> {
    let (id, file) = path.strip_prefix('/')?.split_once('/')?;
    let id: u64 = id.parse().ok()?;
    let store = STORES.lock().ok()?.get(&id)?.upgrade()?;
    Some((store, file.to_owned()))
}

fn io(error: &PageError, code: i32) -> i32 {
    tracing::warn!(%error, "norn-pages VFS operation failed");
    code
}

/// An open file: a page-store file, or a private in-memory buffer.
pub(crate) enum Handle {
    Store { store: Arc<PageStore>, file: String },
    Memory { data: Vec<u8> },
}

impl VfsHandle for Handle {
    fn readonly(&self) -> bool {
        false
    }

    fn in_memory(&self) -> bool {
        true
    }
}

struct PagesVfs;

fn offset(value: usize) -> VfsResult<u64> {
    u64::try_from(value).map_err(|_overflow| vars::SQLITE_IOERR)
}

impl Vfs for PagesVfs {
    type Handle = Handle;

    fn open(&self, path: Option<&str>, opts: OpenOpts) -> VfsResult<Handle> {
        if let (OpenKind::MainDb, Some(path)) = (opts.kind(), path) {
            // The engine creates or forks the file before opening it.
            let (store, file) = resolve(path).ok_or(vars::SQLITE_CANTOPEN)?;
            if !store
                .exists(&file)
                .map_err(|error| io(&error, vars::SQLITE_CANTOPEN))?
            {
                return Err(vars::SQLITE_CANTOPEN);
            }
            return Ok(Handle::Store { store, file });
        }
        Ok(Handle::Memory { data: Vec::new() })
    }

    fn delete(&self, _path: &str) -> VfsResult<()> {
        // Only journals and temporary files are deleted by name, and they
        // live in their handles.
        Ok(())
    }

    fn access(&self, path: &str, _flags: AccessFlags) -> VfsResult<bool> {
        Ok(resolve(path).is_some_and(|(store, file)| store.exists(&file).unwrap_or(false)))
    }

    fn file_size(&self, handle: &mut Handle) -> VfsResult<usize> {
        match handle {
            Handle::Store { store, file } => {
                let root = store
                    .root(file)
                    .map_err(|error| io(&error, vars::SQLITE_IOERR_FSTAT))?;
                usize::try_from(root.size()).map_err(|_overflow| vars::SQLITE_IOERR_FSTAT)
            }
            Handle::Memory { data } => Ok(data.len()),
        }
    }

    fn truncate(&self, handle: &mut Handle, size: usize) -> VfsResult<()> {
        match handle {
            Handle::Store { store, file } => store
                .truncate(file, offset(size)?)
                .map_err(|error| io(&error, vars::SQLITE_IOERR_TRUNCATE)),
            Handle::Memory { data } => {
                data.resize(size, 0);
                Ok(())
            }
        }
    }

    fn write(&self, handle: &mut Handle, at: usize, bytes: &[u8]) -> VfsResult<usize> {
        match handle {
            Handle::Store { store, file } => store
                .write(file, offset(at)?, bytes)
                .map(|()| bytes.len())
                .map_err(|error| io(&error, vars::SQLITE_IOERR_WRITE)),
            Handle::Memory { data } => {
                let end = at
                    .checked_add(bytes.len())
                    .ok_or(vars::SQLITE_IOERR_WRITE)?;
                if data.len() < end {
                    data.resize(end, 0);
                }
                data.get_mut(at..end)
                    .ok_or(vars::SQLITE_IOERR_WRITE)?
                    .copy_from_slice(bytes);
                Ok(bytes.len())
            }
        }
    }

    fn read(&self, handle: &mut Handle, at: usize, buf: &mut [u8]) -> VfsResult<usize> {
        match handle {
            Handle::Store { store, file } => store
                .read(file, offset(at)?, buf)
                .map_err(|error| io(&error, vars::SQLITE_IOERR_READ)),
            Handle::Memory { data } => {
                let available = data.get(at..).unwrap_or(&[]);
                let count = available.len().min(buf.len());
                buf.get_mut(..count)
                    .ok_or(vars::SQLITE_IOERR_READ)?
                    .copy_from_slice(available.get(..count).ok_or(vars::SQLITE_IOERR_READ)?);
                Ok(count)
            }
        }
    }

    fn lock(&self, _handle: &mut Handle, _level: LockLevel) -> VfsResult<()> {
        Ok(())
    }

    fn unlock(&self, _handle: &mut Handle, _level: LockLevel) -> VfsResult<()> {
        Ok(())
    }

    fn check_reserved_lock(&self, _handle: &mut Handle) -> VfsResult<bool> {
        Ok(false)
    }

    fn close(&self, _handle: Handle) -> VfsResult<()> {
        Ok(())
    }
}
