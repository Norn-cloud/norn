//! Core IR and checker wire schemas, projected by the existing `OpenAPI` generator.
/// Default-deny host authorization.
pub mod authz;
pub mod clock;
pub mod contracts;
/// Checked definition admission.
pub mod defs;
/// Tenant cell host and definition epochs.
pub mod host;
pub mod openapi;
/// Stable host refusal wire contract.
pub mod refusal;
/// Authorized definition reads and HTTP adapter.
pub mod routes;
/// Explicit cell and request scope perimeter.
pub mod scope;

/// Governed action transport adapters.
pub mod actions;

#[cfg(all(feature = "http-host", not(target_arch = "wasm32")))]
mod ui;
pub use clock::{Clock, ManualClock};

/// Generic MCP catalog and dispatch boundary.
pub mod mcp;
