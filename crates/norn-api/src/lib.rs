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

/// Shared cell and definition epoch.
pub mod host;
/// Generic MCP catalog and dispatch boundary.
pub mod mcp;
/// HOST-READ-4: an object's link neighbourhood and its source lineage.
pub mod object_links;
/// HOST-READ-3: the object grid query grammar and its evaluation.
pub mod object_query;
/// HOST-READ-2: authorized typed object pages and objects.
pub mod object_routes;
/// Typed object, link and source-lineage reads for the Data Explorer.
pub mod objects;
/// Authorized exact question answers and numeric traces.
pub mod question_routes;
/// Authorized read services shared by transports.
pub mod routes;
/// Native numeric trace schema projections.
pub mod trace_contract;

/// The trusted transport's identity seam: verified host tokens to Norn principals.
pub mod principal;
/// Scoped PDP authority for complete refusal diagnostics.
pub mod refusal_disclosure;
