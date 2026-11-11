//! The content-addressed copy-on-write page store.
//!
//! - **Pages** are stored once under their keyed address
//!   ([`crate::address`]). A write never replaces a stored page (L1): an
//!   address that is already present keeps its bytes.
//! - **A file** is a [`Root`]: its byte size plus the root of a Merkle radix
//!   page map, a 64-way tree from page index to page address whose nodes are
//!   content-addressed too. Writing a page path-copies the nodes above it and
//!   shares every other subtree, so the root address is the version id.
//! - **A fork** copies the parent's root: O(1), no page or node is copied
//!   (L2: later writes to either file change only that file's root).
//! - **Garbage collection** marks every page and node reachable from a live
//!   file's root and sweeps the rest (L3). Marking skips a subtree it has
//!   already seen, so forks that share most of their tree cost only their
//!   differences.
//!
//! Locking: each file has its own lock, and pages and nodes live in hashed
//! shards, so writers to different files never share a lock except briefly
//! a shard's. A collection excludes writers through one gate, held for
//! reading by every operation and for writing only by the collector.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

use crate::address::{Address, Domain, TenantKey};

/// Bytes per page. SQLite's default page size; the engine pins it.
pub const PAGE_SIZE: usize = 4096;
/// Children per page-map node.
const FANOUT: usize = 64;
/// Shards of the page and node maps.
const SHARDS: usize = 64;

/// A page-store failure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PageError {
    /// No file has this name.
    #[error("no page file {0:?}")]
    NoFile(String),
    /// A file with this name already exists.
    #[error("page file {0:?} already exists")]
    FileExists(String),
    /// A reachable address has no stored content: the store is corrupt.
    #[error("dangling page-store address {0:?}")]
    Dangling(Address),
    /// Different bytes produced an address already stored.
    #[error("page-store address collision at {0:?}")]
    Collision(Address),
    /// An offset, size or tree height is out of range.
    #[error("page-store {0} out of range")]
    OutOfRange(&'static str),
    /// A lock was poisoned by a panic while held.
    #[error("page-store lock poisoned")]
    Poisoned,
}

/// One file's version: its size and its page-map root. Two files with equal
/// roots hold equal bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Root {
    size: u64,
    height: u8,
    node: Option<Address>,
}

impl Root {
    /// The file's size in bytes.
    #[must_use]
    pub const fn size(&self) -> u64 {
        self.size
    }

    /// The address of the page map's root node; `None` for an empty map.
    #[must_use]
    pub const fn node(&self) -> Option<Address> {
        self.node
    }
}

/// What a collection found and freed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GcReport {
    /// Pages reachable from a live root, kept.
    pub pages_live: usize,
    /// Page-map nodes reachable from a live root, kept.
    pub nodes_live: usize,
    /// Unreachable pages freed.
    pub pages_freed: usize,
    /// Unreachable page-map nodes freed.
    pub nodes_freed: usize,
}

/// Stored content counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StoreStats {
    /// Unique pages stored.
    pub pages: usize,
    /// Unique page-map nodes stored.
    pub nodes: usize,
}

type Children = Arc<[Option<Address>]>;

#[derive(Default)]
struct Shard {
    pages: HashMap<Address, Arc<[u8]>>,
    nodes: HashMap<Address, (u8, Children)>,
}

/// One tenant's page store. Share it with [`Arc`].
pub struct PageStore {
    id: u64,
    key: TenantKey,
    shards: Box<[RwLock<Shard>]>,
    files: RwLock<BTreeMap<String, Arc<Mutex<Root>>>>,
    gate: RwLock<()>,
}

impl std::fmt::Debug for PageStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PageStore").finish_non_exhaustive()
    }
}

fn read<T>(lock: &RwLock<T>) -> Result<RwLockReadGuard<'_, T>, PageError> {
    lock.read().map_err(|_poisoned| PageError::Poisoned)
}

fn write<T>(lock: &RwLock<T>) -> Result<RwLockWriteGuard<'_, T>, PageError> {
    lock.write().map_err(|_poisoned| PageError::Poisoned)
}

fn lock<T>(mutex: &Mutex<T>) -> Result<MutexGuard<'_, T>, PageError> {
    mutex.lock().map_err(|_poisoned| PageError::Poisoned)
}

fn to_usize(value: u64, what: &'static str) -> Result<usize, PageError> {
    usize::try_from(value).map_err(|_overflow| PageError::OutOfRange(what))
}

fn to_u64(value: usize, what: &'static str) -> Result<u64, PageError> {
    u64::try_from(value).map_err(|_overflow| PageError::OutOfRange(what))
}

/// Pages one child of a node at `height` spans: 64^height.
fn span(height: u8) -> Result<u64, PageError> {
    64_u64
        .checked_pow(u32::from(height))
        .ok_or(PageError::OutOfRange("page-map height"))
}

/// Pages a tree of `height` holds: 64^(height + 1).
fn capacity(height: u8) -> Result<u64, PageError> {
    span(height)?
        .checked_mul(64)
        .ok_or(PageError::OutOfRange("page-map height"))
}

fn page_index(offset: u64) -> Result<u64, PageError> {
    offset
        .checked_div(to_u64(PAGE_SIZE, "page size")?)
        .ok_or(PageError::OutOfRange("offset"))
}

fn empty_children() -> Vec<Option<Address>> {
    vec![None; FANOUT]
}

impl PageStore {
    /// An empty store for one tenant.
    #[must_use]
    pub fn new(key: TenantKey) -> Arc<Self> {
        Arc::new_cyclic(|this| Self {
            id: crate::vfs::enroll(std::sync::Weak::clone(this)),
            key,
            shards: (0..SHARDS).map(|_| RwLock::default()).collect(),
            files: RwLock::default(),
            gate: RwLock::default(),
        })
    }

    /// The store's process-unique id, the first segment of its VFS paths.
    pub(crate) const fn id(&self) -> u64 {
        self.id
    }

    fn shard(&self, address: &Address) -> Result<&RwLock<Shard>, PageError> {
        self.shards
            .get(address.shard(SHARDS))
            .ok_or(PageError::OutOfRange("shard"))
    }

    fn file(&self, name: &str) -> Result<Arc<Mutex<Root>>, PageError> {
        read(&self.files)?
            .get(name)
            .cloned()
            .ok_or_else(|| PageError::NoFile(name.to_owned()))
    }

    // ------------------------------------------------------------ files

    /// Create an empty file.
    ///
    /// # Errors
    /// [`PageError::FileExists`], or a poisoned lock.
    pub fn create_file(&self, name: &str) -> Result<(), PageError> {
        let mut files = write(&self.files)?;
        if files.contains_key(name) {
            return Err(PageError::FileExists(name.to_owned()));
        }
        files.insert(name.to_owned(), Arc::default());
        Ok(())
    }

    /// Whether a file exists.
    ///
    /// # Errors
    /// A poisoned lock.
    pub fn exists(&self, name: &str) -> Result<bool, PageError> {
        Ok(read(&self.files)?.contains_key(name))
    }

    /// Fork `from` as a new file `to`: O(1), copying only the root.
    ///
    /// # Errors
    /// [`PageError::NoFile`], [`PageError::FileExists`], or a poisoned lock.
    pub fn fork_file(&self, from: &str, to: &str) -> Result<Root, PageError> {
        let _gate = read(&self.gate)?;
        let root = *lock(&*self.file(from)?)?;
        let mut files = write(&self.files)?;
        if files.contains_key(to) {
            return Err(PageError::FileExists(to.to_owned()));
        }
        files.insert(to.to_owned(), Arc::new(Mutex::new(root)));
        Ok(root)
    }

    /// Remove a file. Its unique pages are freed by the next collection.
    ///
    /// # Errors
    /// [`PageError::NoFile`], or a poisoned lock.
    pub fn delete_file(&self, name: &str) -> Result<(), PageError> {
        write(&self.files)?
            .remove(name)
            .map(|_root| ())
            .ok_or_else(|| PageError::NoFile(name.to_owned()))
    }

    /// A file's current root.
    ///
    /// # Errors
    /// [`PageError::NoFile`], or a poisoned lock.
    pub fn root(&self, name: &str) -> Result<Root, PageError> {
        Ok(*lock(&*self.file(name)?)?)
    }

    /// Read up to `buf.len()` bytes at `offset`; returns how many bytes the
    /// file holds there (fewer at its end). Holes read as zeros.
    ///
    /// # Errors
    /// [`PageError::NoFile`], a dangling address, or a poisoned lock.
    pub fn read(&self, name: &str, offset: u64, buf: &mut [u8]) -> Result<usize, PageError> {
        let _gate = read(&self.gate)?;
        let root = self.root(name)?;
        self.read_root(&root, offset, buf)
    }

    /// Write `data` at `offset`, extending the file as needed.
    ///
    /// # Errors
    /// [`PageError::NoFile`], an address collision, a range overflow or a
    /// poisoned lock.
    pub fn write(&self, name: &str, offset: u64, data: &[u8]) -> Result<(), PageError> {
        let _gate = read(&self.gate)?;
        let file = self.file(name)?;
        let mut root = lock(&file)?;
        *root = self.write_root(*root, offset, data)?;
        Ok(())
    }

    /// Set a file's size; pages wholly beyond it leave the page map.
    ///
    /// # Errors
    /// [`PageError::NoFile`], a range overflow or a poisoned lock.
    pub fn truncate(&self, name: &str, size: u64) -> Result<(), PageError> {
        let _gate = read(&self.gate)?;
        let file = self.file(name)?;
        let mut root = lock(&file)?;
        *root = self.truncate_root(*root, size)?;
        Ok(())
    }

    // ------------------------------------------------------- persistent roots

    /// The page at `index` under `root`; `None` for a hole.
    ///
    /// # Errors
    /// A dangling address or a poisoned lock.
    pub fn page(&self, root: &Root, index: u64) -> Result<Option<Arc<[u8]>>, PageError> {
        if index >= capacity(root.height)? {
            return Ok(None);
        }
        let mut node = root.node;
        let mut height = root.height;
        let mut index = index;
        loop {
            let Some(address) = node else {
                return Ok(None);
            };
            let children = self.node(&address)?;
            let span = span(height)?;
            let slot = to_usize(index.checked_div(span).unwrap_or(0), "page index")?;
            let child = children.get(slot).copied().flatten();
            if height == 0 {
                return child.map(|page| self.page_bytes(&page)).transpose();
            }
            index = index.checked_rem(span).unwrap_or(0);
            height = height.saturating_sub(1);
            node = child;
        }
    }

    fn read_root(&self, root: &Root, offset: u64, buf: &mut [u8]) -> Result<usize, PageError> {
        let available = root.size.saturating_sub(offset);
        let wanted = to_usize(available.min(to_u64(buf.len(), "read")?), "read")?;
        let page_size = to_u64(PAGE_SIZE, "page size")?;
        let mut done = 0_usize;
        while done < wanted {
            let at = offset
                .checked_add(to_u64(done, "read")?)
                .ok_or(PageError::OutOfRange("offset"))?;
            let index = page_index(at)?;
            let within = to_usize(at.checked_rem(page_size).unwrap_or(0), "offset")?;
            let take = (PAGE_SIZE.saturating_sub(within)).min(wanted.saturating_sub(done));
            let end = done.saturating_add(take);
            let out = buf
                .get_mut(done..end)
                .ok_or(PageError::OutOfRange("read"))?;
            match self.page(root, index)? {
                Some(page) => out.copy_from_slice(
                    page.get(within..within.saturating_add(take))
                        .ok_or(PageError::OutOfRange("page"))?,
                ),
                None => out.fill(0),
            }
            done = end;
        }
        Ok(wanted)
    }

    fn write_root(&self, root: Root, offset: u64, data: &[u8]) -> Result<Root, PageError> {
        let page_size = to_u64(PAGE_SIZE, "page size")?;
        let end = offset
            .checked_add(to_u64(data.len(), "write")?)
            .ok_or(PageError::OutOfRange("offset"))?;
        let mut root = root;
        let mut done = 0_usize;
        while done < data.len() {
            let at = offset
                .checked_add(to_u64(done, "write")?)
                .ok_or(PageError::OutOfRange("offset"))?;
            let index = page_index(at)?;
            let within = to_usize(at.checked_rem(page_size).unwrap_or(0), "offset")?;
            let take = (PAGE_SIZE.saturating_sub(within)).min(data.len().saturating_sub(done));
            let mut page = if take == PAGE_SIZE {
                Vec::with_capacity(PAGE_SIZE)
            } else {
                self.page(&root, index)?
                    .map_or_else(|| vec![0; PAGE_SIZE], |page| page.to_vec())
            };
            let chunk = data
                .get(done..done.saturating_add(take))
                .ok_or(PageError::OutOfRange("write"))?;
            if take == PAGE_SIZE {
                page.extend_from_slice(chunk);
            } else {
                page.get_mut(within..within.saturating_add(take))
                    .ok_or(PageError::OutOfRange("page"))?
                    .copy_from_slice(chunk);
            }
            let address = self.put_page(&page)?;
            root = self.set_page(root, index, address)?;
            done = done.saturating_add(take);
        }
        root.size = root.size.max(end);
        Ok(root)
    }

    fn set_page(&self, root: Root, index: u64, page: Address) -> Result<Root, PageError> {
        let mut height = root.height;
        let mut node = root.node;
        while index >= capacity(height)? {
            if let Some(child) = node {
                let mut children = empty_children();
                if let Some(first) = children.first_mut() {
                    *first = Some(child);
                }
                node = Some(self.put_node(height.saturating_add(1), children)?);
            }
            height = height
                .checked_add(1)
                .ok_or(PageError::OutOfRange("page-map height"))?;
        }
        let node = self.set_in(node, height, index, page)?;
        Ok(Root {
            size: root.size,
            height,
            node: Some(node),
        })
    }

    fn set_in(
        &self,
        node: Option<Address>,
        height: u8,
        index: u64,
        page: Address,
    ) -> Result<Address, PageError> {
        let mut children = match node {
            Some(address) => self.node(&address)?.to_vec(),
            None => empty_children(),
        };
        let span = span(height)?;
        let slot = to_usize(index.checked_div(span).unwrap_or(0), "page index")?;
        let child = children
            .get_mut(slot)
            .ok_or(PageError::OutOfRange("page index"))?;
        *child = Some(if height == 0 {
            page
        } else {
            self.set_in(
                *child,
                height.saturating_sub(1),
                index.checked_rem(span).unwrap_or(0),
                page,
            )?
        });
        self.put_node(height, children)
    }

    fn truncate_root(&self, root: Root, size: u64) -> Result<Root, PageError> {
        let page_size = to_u64(PAGE_SIZE, "page size")?;
        let keep = size.div_ceil(page_size);
        let node = match root.node {
            Some(node) if keep < capacity(root.height)? => {
                self.clear_from(node, root.height, keep)?
            }
            other => other,
        };
        let mut truncated = Root {
            size,
            height: root.height,
            node,
        };
        // A later extension must read zeros past the cut, not stale bytes.
        let tail = to_usize(size.checked_rem(page_size).unwrap_or(0), "size")?;
        let last = page_index(size)?;
        if tail != 0
            && let Some(page) = self.page(&truncated, last)?
        {
            let mut page = page.to_vec();
            page.get_mut(tail..)
                .ok_or(PageError::OutOfRange("page"))?
                .fill(0);
            let address = self.put_page(&page)?;
            truncated = self.set_page(truncated, last, address)?;
        }
        Ok(truncated)
    }

    /// The node with every page at or after `first` removed; `None` if empty.
    fn clear_from(
        &self,
        node: Address,
        height: u8,
        first: u64,
    ) -> Result<Option<Address>, PageError> {
        if first == 0 {
            return Ok(None);
        }
        let mut children = self.node(&node)?.to_vec();
        let span = span(height)?;
        let slot = to_usize(first.checked_div(span).unwrap_or(0), "page index")?;
        let rest = first.checked_rem(span).unwrap_or(0);
        for (at, child) in children.iter_mut().enumerate() {
            if at > slot || (at == slot && (height == 0 || rest == 0)) {
                *child = None;
            } else if at == slot
                && let Some(address) = *child
            {
                *child = self.clear_from(address, height.saturating_sub(1), rest)?;
            }
        }
        if children.iter().all(Option::is_none) {
            return Ok(None);
        }
        self.put_node(height, children).map(Some)
    }

    // ------------------------------------------------------ content storage

    fn put_page(&self, bytes: &[u8]) -> Result<Address, PageError> {
        let address = self.key.address(Domain::Page, &[bytes]);
        let mut shard = write(self.shard(&address)?)?;
        match shard.pages.get(&address) {
            // L1: a stored page is never replaced.
            Some(stored) if &**stored == bytes => {}
            Some(_) => return Err(PageError::Collision(address)),
            None => {
                shard.pages.insert(address, Arc::from(bytes));
            }
        }
        Ok(address)
    }

    fn put_node(&self, height: u8, children: Vec<Option<Address>>) -> Result<Address, PageError> {
        let encoded = encode_node(height, &children);
        let address = self.key.address(Domain::Node, &[&encoded]);
        let mut shard = write(self.shard(&address)?)?;
        match shard.nodes.get(&address) {
            Some((stored_height, stored)) if *stored_height == height && **stored == *children => {}
            Some(_) => return Err(PageError::Collision(address)),
            None => {
                shard.nodes.insert(address, (height, Arc::from(children)));
            }
        }
        Ok(address)
    }

    fn page_bytes(&self, address: &Address) -> Result<Arc<[u8]>, PageError> {
        read(self.shard(address)?)?
            .pages
            .get(address)
            .cloned()
            .ok_or(PageError::Dangling(*address))
    }

    fn node(&self, address: &Address) -> Result<Children, PageError> {
        read(self.shard(address)?)?
            .nodes
            .get(address)
            .map(|(_height, children)| Arc::clone(children))
            .ok_or(PageError::Dangling(*address))
    }

    // ------------------------------------------------------------ collection

    /// L3: free every page and node no live file's root reaches. Writers wait
    /// for the collection; readers of other stores do not.
    ///
    /// # Errors
    /// A dangling address (a corrupt store) or a poisoned lock; nothing is
    /// freed then.
    pub fn collect_garbage(&self) -> Result<GcReport, PageError> {
        let _gate = write(&self.gate)?;
        let roots: Vec<Root> = read(&self.files)?
            .values()
            .map(|root| lock(root).map(|root| *root))
            .collect::<Result<_, _>>()?;
        let mut nodes = HashSet::new();
        let mut pages = HashSet::new();
        let mut stack: Vec<(Address, u8)> = roots
            .iter()
            .filter_map(|root| root.node.map(|node| (node, root.height)))
            .collect();
        while let Some((address, height)) = stack.pop() {
            if !nodes.insert(address) {
                continue;
            }
            for child in self.node(&address)?.iter().flatten() {
                if height == 0 {
                    pages.insert(*child);
                } else if !nodes.contains(child) {
                    stack.push((*child, height.saturating_sub(1)));
                }
            }
        }
        let mut report = GcReport {
            pages_live: pages.len(),
            nodes_live: nodes.len(),
            ..GcReport::default()
        };
        for shard in &*self.shards {
            let mut shard = write(shard)?;
            let (before_pages, before_nodes) = (shard.pages.len(), shard.nodes.len());
            shard.pages.retain(|address, _| pages.contains(address));
            shard.nodes.retain(|address, _| nodes.contains(address));
            report.pages_freed = report
                .pages_freed
                .saturating_add(before_pages.saturating_sub(shard.pages.len()));
            report.nodes_freed = report
                .nodes_freed
                .saturating_add(before_nodes.saturating_sub(shard.nodes.len()));
        }
        Ok(report)
    }

    /// Unique pages and nodes currently stored.
    ///
    /// # Errors
    /// A poisoned lock.
    pub fn stats(&self) -> Result<StoreStats, PageError> {
        let mut stats = StoreStats::default();
        for shard in &*self.shards {
            let shard = read(shard)?;
            stats.pages = stats.pages.saturating_add(shard.pages.len());
            stats.nodes = stats.nodes.saturating_add(shard.nodes.len());
        }
        Ok(stats)
    }

    /// L1 check: every stored page and node is stored under the address of
    /// its own content.
    ///
    /// # Errors
    /// [`PageError::Collision`] naming the first entry whose content does not
    /// match its address, or a poisoned lock.
    pub fn verify(&self) -> Result<(), PageError> {
        for shard in &*self.shards {
            let shard = read(shard)?;
            for (address, bytes) in &shard.pages {
                if self.key.address(Domain::Page, &[bytes]) != *address {
                    return Err(PageError::Collision(*address));
                }
            }
            for (address, (height, children)) in &shard.nodes {
                let encoded = encode_node(*height, children);
                if self.key.address(Domain::Node, &[&encoded]) != *address {
                    return Err(PageError::Collision(*address));
                }
            }
        }
        Ok(())
    }

    /// Every file's name and current root.
    ///
    /// # Errors
    /// A poisoned lock.
    pub fn files(&self) -> Result<Vec<(String, Root)>, PageError> {
        read(&self.files)?
            .iter()
            .map(|(name, root)| lock(root).map(|root| (name.clone(), *root)))
            .collect()
    }
}

/// A node's MAC input: its height, then each child slot as absent (0) or
/// present (1 and the 32 address bytes).
fn encode_node(height: u8, children: &[Option<Address>]) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(children.len().saturating_mul(33).saturating_add(1));
    encoded.push(height);
    for child in children {
        match child {
            Some(address) => {
                encoded.push(1);
                encoded.extend_from_slice(address.as_bytes());
            }
            None => encoded.push(0),
        }
    }
    encoded
}
