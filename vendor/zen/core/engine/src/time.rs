//! Norn: monotonic clock used for the informational `performance` / trace timings.
//!
//! `std::time::Instant` panics on `wasm32-unknown-unknown`; there we use `web-time`
//! (backed by `performance.now()`). On Cloudflare Workers `performance.now()` only
//! advances across I/O, so timings measured inside synchronous evaluation read as ~0.
//! Treat them as informational only; never use them for deadlines or metering.

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub(crate) use std::time::Instant;
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
pub(crate) use web_time::Instant;
