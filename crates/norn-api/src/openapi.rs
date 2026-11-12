//! Generated `OpenAPI` components for the core IR and checker wire contract.
use crate::contracts::ContractSchemas;
use utoipa::openapi::security::{HttpAuthScheme, HttpBuilder, SecurityRequirement, SecurityScheme};
use utoipa::{Modify, OpenApi};

/// The security scheme's name in `components.securitySchemes`.
pub const HOST_TOKEN_SCHEME: &str = "hostToken";

/// Declares the host-token bearer scheme and requires it on every operation.
///
/// Every route in this document resolves its principal from the
/// `Authorization: Bearer` header and denies without one, so no operation is
/// exempt. A public route added later must opt out here, by name, and the
/// test below then names it too.
pub(crate) struct HostTokenSecurity;

impl Modify for HostTokenSecurity {
    fn modify(&self, openapi: &mut utoipa::openapi::OpenApi) {
        let scheme = HttpBuilder::new()
            .scheme(HttpAuthScheme::Bearer)
            .description(Some(
            "The host token for this cell, sent as `Authorization: Bearer <token>`. A published \
             host issues a short-lived signed token for one organization and environment; the \
             local development host prints a session token when it starts. A request without a \
             valid token is refused with 403 `authz.denied`.",
            ))
            .build();
        openapi
            .components
            .get_or_insert_with(utoipa::openapi::Components::new)
            .add_security_scheme(HOST_TOKEN_SCHEME, SecurityScheme::Http(scheme));
        let requirement = SecurityRequirement::new(HOST_TOKEN_SCHEME, Vec::<String>::new());
        for item in openapi.paths.paths.values_mut() {
            let fixed = [
                &mut item.get,
                &mut item.put,
                &mut item.post,
                &mut item.delete,
                &mut item.options,
                &mut item.head,
                &mut item.patch,
                &mut item.trace,
                &mut item.query,
            ];
            for operation in fixed
                .into_iter()
                .flatten()
                .chain(item.additional_operations.values_mut())
            {
                operation.security = Some(vec![requirement.clone()]);
            }
        }
    }
}

/// The public core's schema contract; the cell store is embedded through Rust.
#[derive(Debug, OpenApi)]
#[openapi(info(title = "Norn core contracts"), modifiers(&ContractSchemas, &HostTokenSecurity), paths(crate::actions::call_action, crate::question_routes::get_answer, crate::question_routes::get_numeric_trace, crate::question_routes::evaluate_question, crate::question_routes::trace_ad_hoc, crate::definition_routes::admit_definition, crate::definition_routes::install_definition, crate::definition_routes::retire_definition, crate::definition_routes::list_definitions, crate::definition_routes::get_definition), components(schemas(crate::actions::ActionRequest, crate::actions::ActionReceipt, crate::actions::ActionCommit, crate::actions::ActionCallOutcome, crate::refusal::Refusal, crate::refusal::RefusalDiagnostic, crate::refusal::RefusalSpan, crate::refusal::RefusalRestatement, crate::question_routes::EvaluateRequest, crate::question_routes::AdHocTraceRequest, crate::definition_routes::AdmitDefinitionRequest, crate::definition_routes::InstallDefinitionRequest, crate::definition_routes::RetireDefinitionRequest, crate::definition_routes::AdmitDefinitionResponse, crate::definition_routes::InstallDefinitionResponse, crate::definition_routes::DefinitionReceipt, crate::definition_routes::DefinitionPage, crate::definition_routes::DefinitionSummary, crate::definition_routes::DefinitionDocument, crate::definition_routes::SignoffRequest, crate::definition_routes::SignoffDocument, crate::contracts::NornCheckReceipt, crate::contracts::NornCheckerDiagnostics, crate::answers::NornQueryResult, crate::answers::NornHistoricalQuestionResult, crate::objects::NornObjectPage, crate::objects::NornObjectRecord, crate::objects::NornLinkPage, crate::objects::NornInstanceSourceProvenance)))]
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
        assert!(
            schemas["NornActionInput"]["additionalProperties"]["description"]
                .as_str()
                .unwrap()
                .contains("exact-decimal")
        );
        assert!(
            schemas["NornActionInput"]["description"]
                .as_str()
                .unwrap()
                .contains("travels as its exact decimal text")
        );
        assert!(schemas.get("NornActionJsonValue").is_none());
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

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "generated schema correspondence"
)]
mod security_contract_tests {
    /// Operations a caller may reach without a host token. None exist.
    const PUBLIC_OPERATIONS: &[&str] = &[];

    #[test]
    fn every_operation_requires_the_declared_host_token() {
        let document: serde_json::Value =
            serde_json::from_str(&super::openapi_json().unwrap()).unwrap();
        let scheme = &document["components"]["securitySchemes"][super::HOST_TOKEN_SCHEME];
        assert_eq!(scheme["type"], "http");
        assert_eq!(scheme["scheme"], "bearer");
        let required = serde_json::json!([{ super::HOST_TOKEN_SCHEME: [] }]);
        let mut seen = 0_usize;
        for (path, item) in document["paths"].as_object().unwrap() {
            for (method, operation) in item.as_object().unwrap() {
                let Some(id) = operation.get("operationId").and_then(|id| id.as_str()) else {
                    continue;
                };
                seen += 1;
                if PUBLIC_OPERATIONS.contains(&id) {
                    assert!(operation.get("security").is_none(), "{method} {path}");
                } else {
                    assert_eq!(operation["security"], required, "{method} {path}");
                }
            }
        }
        assert!(seen >= 10, "the document lists its operations: {seen}");
    }
}
