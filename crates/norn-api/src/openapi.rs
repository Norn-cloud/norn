//! Generated `OpenAPI` components for the core IR and checker wire contract.
use crate::contracts::ContractSchemas;
use utoipa::{Modify, OpenApi};

/// The public core's schema contract; the cell store is embedded through Rust.
#[derive(Debug, OpenApi)]
#[openapi(paths(crate::actions::call_action_in_epoch), info(title = "Norn core contracts"), modifiers(&ContractSchemas), components(schemas(crate::refusal::Refusal, crate::contracts::NornCheckReceipt, crate::contracts::NornCheckerDiagnostics)))]
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

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "generated contract correspondence assertions"
)]
mod tests {
    #[test]
    fn action_input_and_receipt_are_generated_typed_contracts() {
        let document: serde_json::Value =
            serde_json::from_str(&super::openapi_json().unwrap()).unwrap();
        let schemas = &document["components"]["schemas"];
        assert_eq!(
            schemas["ActionRequest"]["properties"]["input"]["$ref"],
            "#/components/schemas/NornActionInput"
        );
        assert_eq!(
            schemas["ActionResponse"]["properties"]["record"]["$ref"],
            "#/components/schemas/NornActionCallRecord"
        );
        assert!(schemas["NornActionInput"]["additionalProperties"].is_object());
        let record = &schemas["NornActionCallRecord"]["properties"];
        for name in [
            "actionId",
            "actor",
            "turn",
            "object",
            "input",
            "effectiveInput",
            "derivedValues",
            "preconditionOutcomes",
            "rowOperations",
        ] {
            assert!(record.get(name).is_some(), "missing receipt field {name}");
        }
    }
}
