//! Core IR and checker wire schemas, projected by the existing `OpenAPI` generator.
/// Typed-question answer documents.
pub mod answers;
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

/// FLOW-DRIVER-1: perform dispatch and result turns of flow runs.
pub mod flow_driver;
/// Shared cell and definition epoch.
pub mod host;
/// Generic MCP catalog and dispatch boundary.
pub mod mcp;
/// Typed object, link and source-lineage reads for the Data Explorer.
pub mod objects;
/// Authorized exact question answers and numeric traces.
pub mod question_routes;
/// Authorized read services shared by transports.
pub mod routes;
/// Start, read and list runs of installed flows (RUN-ROUTES-1).
pub mod run_routes;
/// Native numeric trace schema projections.
pub mod trace_contract;

/// The trusted transport's identity seam: verified host tokens to Norn principals.
pub mod principal;
/// Scoped PDP authority for complete refusal diagnostics.
pub mod refusal_disclosure;
