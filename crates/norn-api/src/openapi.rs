//! Generated `OpenAPI` components for the core IR and checker wire contract.
use crate::contracts::ContractSchemas;
use utoipa::{Modify, OpenApi};

/// The public core's schema contract; the cell store is embedded through Rust.
#[derive(Debug, OpenApi)]
#[openapi(info(title = "Norn core contracts"), modifiers(&ContractSchemas), components(schemas(crate::refusal::Refusal, crate::contracts::NornCheckReceipt, crate::contracts::NornCheckerDiagnostics)))]
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
    reason = "generated schema correspondence"
)]
mod action_contract_tests {
    #[test]
    fn generated_action_values_and_receipts_retain_typed_fields() {
        let document: serde_json::Value =
            serde_json::from_str(&super::openapi_json().unwrap()).unwrap();
        let schemas = &document["components"]["schemas"];
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
            assert!(record.get(name).is_some(), "typed receipt field {name}");
        }
        assert_eq!(
            record["object"]["$ref"],
            "#/components/schemas/NornActionFactKey"
        );
        assert!(record["rowOperations"]["items"].is_object());
    }
}
