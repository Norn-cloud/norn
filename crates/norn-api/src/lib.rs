//! Core IR and checker wire schemas, projected by the existing `OpenAPI` generator.
/// Retained action admission and the kernel's action model types.
pub mod action_admission;
/// HOST-ACTION-1: the one governed action service shared by transports.
pub mod actions;
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

/// The `binding` retained-definition kind (CONNECT's tool bindings).
pub mod binding_definitions;
/// Retained (non-kernel) definitions: admit, install, retire, list, get.
pub mod definition_routes;
/// The `flow` retained-definition kind (automations lowered by `lowerFlow`).
pub mod flow_definitions;
/// Shared cell and definition epoch.
pub mod host;
/// RFC 8785 JSON canonicalization for connector manifest digests.
pub mod jcs;
/// Admission of a cell's full kernel artifact set.
pub mod kernel_set;
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
/// The generic sans-IO pull engine for declared HTTP sources (`norn.pull/1`).
pub mod pull;
/// Authorized exact question answers and numeric traces.
pub mod question_routes;
/// Authorized read services shared by transports.
pub mod routes;
/// In-cell source sync over the pull engine: claims, steps and run reports.
pub mod sources;
/// Native numeric trace schema projections.
pub mod trace_contract;

/// The trusted transport's identity seam: verified host tokens to Norn principals.
pub mod principal;
/// Scoped PDP authority for complete refusal diagnostics.
pub mod refusal_disclosure;
