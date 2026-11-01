//! Core IR and checker wire schemas, projected by the existing `OpenAPI` generator.
pub mod contracts;
/// Checked definition admission.
pub mod defs;
/// Tenant cell host and definition epochs.
pub mod host;
pub mod openapi;
/// Stable host refusal wire contract.
pub mod refusal;
/// Explicit cell and request scope perimeter.
pub mod scope;
