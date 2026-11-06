//! `norn-cell-do`: one tenant cell per Workers Durable Object (#728, #819).
//!
//! Ring 1 host perimeter. The Durable Object is the cell's single writer and
//! its SQLite storage is the cell's engine ([`norn_cell::workers::WorkersEngine`]).
//! Everything here is portable Rust: the same [`server::handle`] dispatch runs
//! natively over `NativeEngine`, which is how native/DO parity is checked.
//!
//! - [`principal`]: the one seam that turns transport credentials into a Norn
//!   principal, and the server policy for fixture principals (until #817).
//! - [`seed`]: the public three-systems demo cell, provisioned idempotently.
//! - [`server`]: read-only fetch dispatch over `norn_api`'s shared services.

pub mod principal;
pub mod seed;
pub mod server;
