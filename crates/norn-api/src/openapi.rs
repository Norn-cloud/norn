//! Generated `OpenAPI` components for the core IR and checker wire contract.
use crate::contracts::ContractSchemas;
use utoipa::{Modify, OpenApi};

/// The public core's schema contract; the cell store is embedded through Rust.
#[derive(Debug, OpenApi)]
#[openapi(info(title = "Norn core contracts"), modifiers(&ContractSchemas), components(schemas(crate::contracts::NornCheckReceipt, crate::contracts::NornCheckerDiagnostics)))]
pub struct ApiDoc;

/// Serialize the contract using the existing typed `OpenAPI` projection.
///
/// # Errors
/// Returns a serialization failure.
pub fn openapi_json() -> Result<String, serde_json::Error> {
    let mut doc = ApiDoc::openapi();
    ContractSchemas.modify(&mut doc);
    serde_json::to_string_pretty(&doc)
}
