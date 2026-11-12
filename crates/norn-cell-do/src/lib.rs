//! `norn-cell-do`: one tenant cell per Workers Durable Object (#728, #819).
//!
//! Ring 1 host perimeter. The Durable Object is the cell's single writer and
//! its SQLite storage is the cell's engine (`norn_cell::workers::WorkersEngine`, wasm32 only).
//! Everything here is portable Rust: the same [`server::handle`] dispatch runs
//! natively over `NativeEngine`, which is how native/DO parity is checked.
//!
//! - [`principal`]: the one seam that turns transport credentials into a Norn
//!   principal, and the server policy for fixture principals (until #817).
//! - [`analytics`]: the scrap-yield cell and its admitted question runtime.
//! - [`policy`]: HOST-READ-1 data-read decisions by the kernel PDP over an
//!   `AccessIR` policy compiled against the cell model's Cedar projection.
//! - [`questions`]: ad-hoc questions over the metrics the active revision
//!   derives, for the dev host and a hosted cell alike.
//! - [`seed`]: the public three-systems demo cell, provisioned idempotently.
//! - [`server`]: read-only fetch dispatch over `norn_api`'s shared services.
//! - [`mcp`]: `POST /v1/mcp`, the cell's catalog as MCP tools whose calls
//!   re-enter [`server`] as the same caller.
//! - [`base`]: the trusted base this host compiles in.
//! - [`tenant`]: one scope's cell, vacant until `POST /v1/cell:provision`
//!   (HOST-PROVISION-1) creates it, then served and reopened on restart.
//! - `dev` (native only): `norn-dev-host`, the developer loop over a
//!   project's own SQLite cell, serving the same dispatch on loopback (#207).
//! - [`olap`]: the cell's link to its tenant analytics engine (D102).
//! - `olapd` (native, `duckdb`): `norn-olapd`, the tenant analytics engine
//!   the cell host reaches for analytical questions and loads (D102).

pub mod analytics;
pub mod base;
#[cfg(not(target_arch = "wasm32"))]
pub mod dev;
pub mod mcp;
pub mod olap;
#[cfg(all(feature = "duckdb", not(target_arch = "wasm32")))]
pub mod olapd;
pub mod policy;
pub mod principal;
pub mod questions;
mod runs;
pub mod seed;
pub mod server;
pub mod tenant;
#[cfg(target_arch = "wasm32")]
mod wasm;
