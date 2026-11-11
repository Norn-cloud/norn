//! A content-addressed copy-on-write page VFS for SQLite (experiment, #3148).
//!
//! * Every page is stored once under `blake3(page)[..16]` in a shared,
//!   sharded page store; identical pages dedupe across all files (forks,
//!   tenants).
//! * A file is a persistent vector `pgno -> page hash` (`im::Vector`, an RRB
//!   tree with 64-wide nodes). Cloning it is O(1); a write path-copies
//!   O(log64 n) nodes.
//! * A fork is `committed.clone()` of the parent plus one metadata record:
//!   no page is copied, and reads in a fork take exactly the path main takes.
//! * Durable mode appends new unique pages to a pack file and root deltas /
//!   fork records to a metadata log, made durable by group-commit fdatasync.
//!
//! Limits (prototype): one connection per file, rollback journal in memory
//! (`journal_mode=MEMORY`, `locking_mode=EXCLUSIVE`), no WAL/shm, every page
//! resident in memory (cold reads from a pack or object store are not
//! measured here), no pack compaction after GC.

use std::collections::{HashMap, HashSet};
use std::ffi::{c_char, c_int, c_void, CStr};
use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use im::Vector;
use parking_lot::{Condvar, Mutex, RwLock};
use rusqlite::ffi;

pub const PAGE: usize = 4096;
pub type H = [u8; 16];
const SHARDS: usize = 256;

fn hash_page(page: &[u8]) -> H {
    let full = blake3::hash(page);
    let mut h = [0u8; 16];
    h.copy_from_slice(&full.as_bytes()[..16]);
    h
}

// ---------------------------------------------------------------- page store

pub struct Store {
    shards: Vec<RwLock<HashMap<H, Arc<[u8]>>>>,
    pub unique_pages: AtomicU64,
    pub page_writes: AtomicU64,
    pub dedup_hits: AtomicU64,
    durable: Option<Arc<Durable>>,
}

impl Store {
    fn shard(&self, h: &H) -> &RwLock<HashMap<H, Arc<[u8]>>> {
        &self.shards[h[0] as usize % SHARDS]
    }

    fn put(&self, page: &[u8]) -> H {
        let h = hash_page(page);
        self.page_writes.fetch_add(1, Ordering::Relaxed);
        let shard = self.shard(&h);
        if shard.read().contains_key(&h) {
            self.dedup_hits.fetch_add(1, Ordering::Relaxed);
            return h;
        }
        let mut w = shard.write();
        if w.contains_key(&h) {
            self.dedup_hits.fetch_add(1, Ordering::Relaxed);
            return h;
        }
        w.insert(h, Arc::from(page));
        drop(w);
        self.unique_pages.fetch_add(1, Ordering::Relaxed);
        if let Some(d) = &self.durable {
            d.append_page(&h, page);
        }
        h
    }

    fn get(&self, h: &H) -> Arc<[u8]> {
        self.shard(h).read().get(h).cloned().expect("dangling page hash")
    }

    pub fn bytes(&self) -> u64 {
        self.unique_pages.load(Ordering::Relaxed) * PAGE as u64
    }
}

// ------------------------------------------------------- durable pack + meta

pub struct Durable {
    pack: Mutex<File>,
    meta: Mutex<(File, u64)>, // (file, records written = ticket counter)
    state: Mutex<SyncState>,
    cv: Condvar,
    pub pack_bytes: AtomicU64,
    pub meta_bytes: AtomicU64,
    pub fsyncs: AtomicU64,
}

#[derive(Default)]
struct SyncState {
    durable: u64,
    syncing: bool,
}

impl Durable {
    fn open(dir: &Path) -> std::io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        let open = |n: &str| {
            std::fs::OpenOptions::new().create(true).truncate(true).write(true).open(dir.join(n))
        };
        Ok(Self {
            pack: Mutex::new(open("pages.pack")?),
            meta: Mutex::new((open("roots.log")?, 0)),
            state: Mutex::new(SyncState::default()),
            cv: Condvar::new(),
            pack_bytes: AtomicU64::new(0),
            meta_bytes: AtomicU64::new(0),
            fsyncs: AtomicU64::new(0),
        })
    }

    fn append_page(&self, h: &H, page: &[u8]) {
        let mut p = self.pack.lock();
        p.write_all(h).unwrap();
        p.write_all(page).unwrap();
        self.pack_bytes.fetch_add((h.len() + page.len()) as u64, Ordering::Relaxed);
    }

    /// Append one metadata record and return its durability ticket.
    fn append_meta(&self, record: &[u8]) -> u64 {
        let mut m = self.meta.lock();
        m.0.write_all(&(record.len() as u32).to_le_bytes()).unwrap();
        m.0.write_all(record).unwrap();
        m.1 += 1;
        self.meta_bytes.fetch_add(4 + record.len() as u64, Ordering::Relaxed);
        m.1
    }

    /// Group commit: one fdatasync of pack then log covers every waiter whose
    /// record was written before the sync began.
    fn wait_durable(&self, ticket: u64) {
        let mut s = self.state.lock();
        loop {
            if s.durable >= ticket {
                return;
            }
            if s.syncing {
                self.cv.wait(&mut s);
                continue;
            }
            s.syncing = true;
            drop(s);
            let target = self.meta.lock().1;
            // Pack first: a root never becomes durable before its pages.
            let pack = self.pack.lock().try_clone().unwrap();
            pack.sync_data().unwrap();
            let meta = self.meta.lock().0.try_clone().unwrap();
            meta.sync_data().unwrap();
            self.fsyncs.fetch_add(1, Ordering::Relaxed);
            s = self.state.lock();
            s.syncing = false;
            s.durable = s.durable.max(target);
            self.cv.notify_all();
        }
    }
}

// ------------------------------------------------------------------ files

pub struct FileState {
    working: Mutex<Vector<H>>,
    committed: Mutex<Vector<H>>,
    pub commits: AtomicU64,
}

impl FileState {
    fn new(root: Vector<H>) -> Self {
        Self {
            working: Mutex::new(root.clone()),
            committed: Mutex::new(root),
            commits: AtomicU64::new(0),
        }
    }
    pub fn pages(&self) -> usize {
        self.committed.lock().len()
    }
}

pub struct Engine {
    pub store: Store,
    files: RwLock<HashMap<String, Arc<FileState>>>,
    pub durable: Option<Arc<Durable>>,
    zero: H,
}

static ENGINE: OnceLock<Engine> = OnceLock::new();

pub fn engine() -> &'static Engine {
    ENGINE.get().expect("vfs not registered")
}

impl Engine {
    /// Fork `parent`'s last committed version into `child`: metadata only.
    pub fn fork(&self, parent: &str, child: &str) -> (Duration, Vector<H>) {
        let t = Instant::now();
        let p = self.files.read().get(parent).cloned().expect("no parent");
        let (root, at) = {
            let c = p.committed.lock();
            (c.clone(), p.commits.load(Ordering::Acquire))
        };
        self.files.write().insert(child.to_owned(), Arc::new(FileState::new(root.clone())));
        if let Some(d) = &self.durable {
            let mut rec = vec![b'F'];
            rec.extend_from_slice(&(child.len() as u16).to_le_bytes());
            rec.extend_from_slice(child.as_bytes());
            rec.extend_from_slice(&(parent.len() as u16).to_le_bytes());
            rec.extend_from_slice(parent.as_bytes());
            rec.extend_from_slice(&at.to_le_bytes());
            let ticket = d.append_meta(&rec);
            d.wait_durable(ticket);
        }
        (t.elapsed(), root)
    }

    /// Pages whose content differs between two versions (the fork's delta).
    pub fn diverged(a: &Vector<H>, b: &Vector<H>) -> usize {
        let common = a.len().min(b.len());
        let mut n = a.len().max(b.len()) - common;
        for (x, y) in a.iter().zip(b.iter()) {
            if x != y {
                n += 1;
            }
        }
        n
    }

    pub fn committed(&self, name: &str) -> Vector<H> {
        self.file(name).expect("file").committed.lock().clone()
    }

    pub fn drop_file(&self, name: &str) {
        self.files.write().remove(name);
        if let Some(d) = &self.durable {
            let mut rec = vec![b'D'];
            rec.extend_from_slice(name.as_bytes());
            d.append_meta(&rec);
        }
    }

    pub fn file(&self, name: &str) -> Option<Arc<FileState>> {
        self.files.read().get(name).cloned()
    }

    pub fn file_count(&self) -> usize {
        self.files.read().len()
    }

    /// Mark-and-sweep by reachability from every live root.
    pub fn gc(&self) -> (u64, u64, Duration) {
        let t = Instant::now();
        let mut live: HashSet<H> = HashSet::new();
        live.insert(self.zero);
        for f in self.files.read().values() {
            for h in f.committed.lock().iter() {
                live.insert(*h);
            }
            for h in f.working.lock().iter() {
                live.insert(*h);
            }
        }
        let mut freed = 0u64;
        for s in &self.store.shards {
            let mut w = s.write();
            let before = w.len();
            w.retain(|h, _| live.contains(h));
            freed += (before - w.len()) as u64;
        }
        self.store.unique_pages.fetch_sub(freed, Ordering::Relaxed);
        (live.len() as u64, freed, t.elapsed())
    }
}

// ------------------------------------------------------------- sqlite glue

#[repr(C)]
struct CowFile {
    base: ffi::sqlite3_file,
    handle: *mut Handle,
}

enum Handle {
    Main { state: Arc<FileState>, dirty: Vec<u32> },
    Mem { data: Vec<u8> },
}

unsafe fn handle<'a>(f: *mut ffi::sqlite3_file) -> &'a mut Handle {
    &mut *(*(f as *mut CowFile)).handle
}

unsafe extern "C" fn x_close(f: *mut ffi::sqlite3_file) -> c_int {
    let cf = f as *mut CowFile;
    if !(*cf).handle.is_null() {
        drop(Box::from_raw((*cf).handle));
        (*cf).handle = std::ptr::null_mut();
    }
    ffi::SQLITE_OK
}

unsafe extern "C" fn x_read(f: *mut ffi::sqlite3_file, buf: *mut c_void, amt: c_int, off: i64) -> c_int {
    let out = std::slice::from_raw_parts_mut(buf as *mut u8, amt as usize);
    match handle(f) {
        Handle::Mem { data } => {
            let off = off as usize;
            let n = data.len().saturating_sub(off).min(out.len());
            out[..n].copy_from_slice(&data[off..off + n]);
            if n < out.len() {
                out[n..].fill(0);
                return ffi::SQLITE_IOERR_SHORT_READ;
            }
            ffi::SQLITE_OK
        }
        Handle::Main { state, .. } => {
            let e = engine();
            let root = state.working.lock();
            let mut done = 0usize;
            let mut pos = off as usize;
            let mut short = false;
            while done < out.len() {
                let pgno = pos / PAGE;
                let within = pos % PAGE;
                let n = (PAGE - within).min(out.len() - done);
                match root.get(pgno) {
                    Some(h) => {
                        let page = e.store.get(h);
                        out[done..done + n].copy_from_slice(&page[within..within + n]);
                    }
                    None => {
                        out[done..done + n].fill(0);
                        short = true;
                    }
                }
                done += n;
                pos += n;
            }
            if short { ffi::SQLITE_IOERR_SHORT_READ } else { ffi::SQLITE_OK }
        }
    }
}

unsafe extern "C" fn x_write(f: *mut ffi::sqlite3_file, buf: *const c_void, amt: c_int, off: i64) -> c_int {
    let src = std::slice::from_raw_parts(buf as *const u8, amt as usize);
    match handle(f) {
        Handle::Mem { data } => {
            let off = off as usize;
            if data.len() < off + src.len() {
                data.resize(off + src.len(), 0);
            }
            data[off..off + src.len()].copy_from_slice(src);
            ffi::SQLITE_OK
        }
        Handle::Main { state, dirty } => {
            let e = engine();
            let mut root = state.working.lock();
            let mut done = 0usize;
            let mut pos = off as usize;
            while done < src.len() {
                let pgno = pos / PAGE;
                let within = pos % PAGE;
                let n = (PAGE - within).min(src.len() - done);
                let h = if within == 0 && n == PAGE {
                    e.store.put(&src[done..done + PAGE])
                } else {
                    let mut page = vec![0u8; PAGE];
                    if let Some(old) = root.get(pgno) {
                        page.copy_from_slice(&e.store.get(old));
                    }
                    page[within..within + n].copy_from_slice(&src[done..done + n]);
                    e.store.put(&page)
                };
                while root.len() < pgno {
                    root.push_back(e.zero);
                }
                if pgno == root.len() {
                    root.push_back(h);
                } else {
                    root.set(pgno, h);
                }
                if e.durable.is_some() {
                    dirty.push(pgno as u32);
                }
                done += n;
                pos += n;
            }
            ffi::SQLITE_OK
        }
    }
}

unsafe extern "C" fn x_truncate(f: *mut ffi::sqlite3_file, size: i64) -> c_int {
    match handle(f) {
        Handle::Mem { data } => data.truncate(size as usize),
        Handle::Main { state, .. } => {
            let pages = (size as usize).div_ceil(PAGE);
            state.working.lock().truncate(pages);
        }
    }
    ffi::SQLITE_OK
}

/// Commit point: publish the working root as the committed version.
unsafe extern "C" fn x_sync(f: *mut ffi::sqlite3_file, _flags: c_int) -> c_int {
    if let Handle::Main { state, dirty } = handle(f) {
        let e = engine();
        let root = state.working.lock().clone();
        let ticket = e.durable.as_ref().map(|d| {
            dirty.sort_unstable();
            dirty.dedup();
            let mut rec = Vec::with_capacity(1 + 8 + dirty.len() * 20);
            rec.push(b'C');
            rec.extend_from_slice(&(root.len() as u64).to_le_bytes());
            for &p in dirty.iter() {
                if let Some(h) = root.get(p as usize) {
                    rec.extend_from_slice(&p.to_le_bytes());
                    rec.extend_from_slice(h);
                }
            }
            dirty.clear();
            d.append_meta(&rec)
        });
        *state.committed.lock() = root;
        state.commits.fetch_add(1, Ordering::Release);
        if let (Some(d), Some(t)) = (&e.durable, ticket) {
            d.wait_durable(t);
        }
    }
    ffi::SQLITE_OK
}

unsafe extern "C" fn x_file_size(f: *mut ffi::sqlite3_file, out: *mut i64) -> c_int {
    *out = match handle(f) {
        Handle::Mem { data } => data.len() as i64,
        Handle::Main { state, .. } => (state.working.lock().len() * PAGE) as i64,
    };
    ffi::SQLITE_OK
}

unsafe extern "C" fn x_lock(_f: *mut ffi::sqlite3_file, _l: c_int) -> c_int {
    ffi::SQLITE_OK
}
unsafe extern "C" fn x_check_reserved(_f: *mut ffi::sqlite3_file, out: *mut c_int) -> c_int {
    *out = 0;
    ffi::SQLITE_OK
}
unsafe extern "C" fn x_file_control(_f: *mut ffi::sqlite3_file, _op: c_int, _a: *mut c_void) -> c_int {
    ffi::SQLITE_NOTFOUND
}
unsafe extern "C" fn x_sector_size(_f: *mut ffi::sqlite3_file) -> c_int {
    PAGE as c_int
}
unsafe extern "C" fn x_device_characteristics(_f: *mut ffi::sqlite3_file) -> c_int {
    0
}

static IO_METHODS: ffi::sqlite3_io_methods = ffi::sqlite3_io_methods {
    iVersion: 1,
    xClose: Some(x_close),
    xRead: Some(x_read),
    xWrite: Some(x_write),
    xTruncate: Some(x_truncate),
    xSync: Some(x_sync),
    xFileSize: Some(x_file_size),
    xLock: Some(x_lock),
    xUnlock: Some(x_lock),
    xCheckReservedLock: Some(x_check_reserved),
    xFileControl: Some(x_file_control),
    xSectorSize: Some(x_sector_size),
    xDeviceCharacteristics: Some(x_device_characteristics),
    xShmMap: None,
    xShmLock: None,
    xShmBarrier: None,
    xShmUnmap: None,
    xFetch: None,
    xUnfetch: None,
};

unsafe extern "C" fn x_open(
    _vfs: *mut ffi::sqlite3_vfs,
    name: *const c_char,
    f: *mut ffi::sqlite3_file,
    flags: c_int,
    out_flags: *mut c_int,
) -> c_int {
    let cf = f as *mut CowFile;
    (*cf).base.pMethods = std::ptr::null();
    let h = if name.is_null() || flags & ffi::SQLITE_OPEN_MAIN_DB == 0 {
        Handle::Mem { data: Vec::new() }
    } else {
        let name = CStr::from_ptr(name).to_string_lossy().into_owned();
        let e = engine();
        let existing = e.files.read().get(&name).cloned();
        let state = match existing {
            Some(s) => s,
            None if flags & ffi::SQLITE_OPEN_CREATE != 0 => {
                let s = Arc::new(FileState::new(Vector::new()));
                e.files.write().insert(name, s.clone());
                s
            }
            None => return ffi::SQLITE_CANTOPEN,
        };
        // A fresh connection sees the last committed version.
        *state.working.lock() = state.committed.lock().clone();
        Handle::Main { state, dirty: Vec::new() }
    };
    (*cf).handle = Box::into_raw(Box::new(h));
    (*cf).base.pMethods = &IO_METHODS;
    if !out_flags.is_null() {
        *out_flags = flags;
    }
    ffi::SQLITE_OK
}

unsafe extern "C" fn x_delete(_vfs: *mut ffi::sqlite3_vfs, _name: *const c_char, _sync: c_int) -> c_int {
    ffi::SQLITE_OK
}

unsafe extern "C" fn x_access(_vfs: *mut ffi::sqlite3_vfs, name: *const c_char, _flags: c_int, out: *mut c_int) -> c_int {
    let name = CStr::from_ptr(name).to_string_lossy();
    *out = engine().files.read().contains_key(name.as_ref()) as c_int;
    ffi::SQLITE_OK
}

unsafe extern "C" fn x_full_pathname(_vfs: *mut ffi::sqlite3_vfs, name: *const c_char, n: c_int, out: *mut c_char) -> c_int {
    let src = CStr::from_ptr(name).to_bytes_with_nul();
    if src.len() > n as usize {
        return ffi::SQLITE_CANTOPEN;
    }
    std::ptr::copy_nonoverlapping(src.as_ptr() as *const c_char, out, src.len());
    ffi::SQLITE_OK
}

/// Register the `cow` VFS. `durable_dir` enables the pack + metadata log.
pub fn register(durable_dir: Option<&Path>) {
    let durable = durable_dir.map(|d| Arc::new(Durable::open(d).expect("durable dir")));
    let zero_page = vec![0u8; PAGE];
    let store = Store {
        shards: (0..SHARDS).map(|_| RwLock::new(HashMap::new())).collect(),
        unique_pages: AtomicU64::new(0),
        page_writes: AtomicU64::new(0),
        dedup_hits: AtomicU64::new(0),
        durable: durable.clone(),
    };
    let zero = store.put(&zero_page);
    let ok = ENGINE.set(Engine { store, files: RwLock::new(HashMap::new()), durable, zero }).is_ok();
    assert!(ok, "register once");
    unsafe {
        let default = ffi::sqlite3_vfs_find(std::ptr::null());
        assert!(!default.is_null());
        let mut vfs: ffi::sqlite3_vfs = *default;
        vfs.szOsFile = std::mem::size_of::<CowFile>() as c_int;
        vfs.pNext = std::ptr::null_mut();
        vfs.zName = c"cow".as_ptr();
        vfs.pAppData = std::ptr::null_mut();
        vfs.xOpen = Some(x_open);
        vfs.xDelete = Some(x_delete);
        vfs.xAccess = Some(x_access);
        vfs.xFullPathname = Some(x_full_pathname);
        let rc = ffi::sqlite3_vfs_register(Box::leak(Box::new(vfs)), 0);
        assert_eq!(rc, ffi::SQLITE_OK);
    }
}
