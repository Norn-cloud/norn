//! `norn-pages`: a content-addressed copy-on-write page store under SQLite,
//! so forking a cell costs metadata, not a copy (Core 08, fork lifecycle and
//! isolation).
//!
//! Ring 1, native only. Durable Object and wasm hosts do not use a VFS; their
//! copy-on-write comes from object-store journal lineage, a later unit.
//!
//! - [`address`]: keyed per-tenant content addresses (keyed BLAKE3).
//! - [`store`]: the page store: pages stored once, a Merkle radix page map
//!   per file, O(1) file fork, garbage collection by reachability.
//! - [`vfs`]: the SQLite VFS over page stores (the FFI is `sqlite-plugin`'s).
//! - [`engine`]: [`CowEngine`], a native SQL engine on a page-store file that
//!   implements [`norn_cell::ForkEngine`].
//!
//! Laws, checked against the copying reference engine in `norn-cell`:
//! L1 a write never changes a stored page under its address; L2 a fork's root
//! is a snapshot; L3 collection never frees a page reachable from a live
//! root; L4 a fork reads exactly what a full copy would.

pub mod address;
pub mod engine;
pub mod store;
pub mod vfs;

pub use address::{Address, TenantKey};
pub use engine::CowEngine;
pub use store::{GcReport, PAGE_SIZE, PageError, PageStore, Root, StoreStats};
