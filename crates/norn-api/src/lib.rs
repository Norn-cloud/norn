//! Core IR and checker wire schemas, projected by the existing `OpenAPI` generator.
pub mod contracts;
/// Retained admitted-definition data.
pub mod defs;
/// Admitted-definition wire projection.
pub mod install;
pub mod openapi;
/// Stable host refusal wire contract.
pub mod refusal;
/// Explicit cell and request scope perimeter.
pub mod scope;
