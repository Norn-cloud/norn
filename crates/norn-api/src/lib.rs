//! Core IR and checker wire schemas, projected by the existing `OpenAPI` generator.
pub mod clock;
pub mod contracts;
pub mod openapi;
/// Stable host refusal wire contract.
pub mod refusal;
/// Explicit cell and request scope perimeter.
pub mod scope;

pub use clock::{Clock, ManualClock};

/// Generic MCP catalog and dispatch boundary.
pub mod mcp;

/// Shared default-deny host authorization.
pub mod authz;
/// Shared cell and definition epoch.
pub mod host;
/// Authorized read services shared by transports.
pub mod routes;
