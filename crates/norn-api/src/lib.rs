//! Core IR and checker wire schemas, projected by the existing `OpenAPI` generator.
/// Default-deny server authorization for host operations and kernel action requests.
pub mod authz;
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
