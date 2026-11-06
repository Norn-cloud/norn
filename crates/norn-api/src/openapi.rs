//! Generated `OpenAPI` components for the core IR and checker wire contract.
use crate::contracts::ContractSchemas;
use utoipa::{Modify, OpenApi};

/// The public core's schema contract; the cell store is embedded through Rust.
#[derive(Debug, OpenApi)]
#[openapi(info(title = "Norn core contracts"), modifiers(&ContractSchemas), paths(crate::question_routes::get_answer, crate::question_routes::get_numeric_trace, crate::question_routes::evaluate_question, crate::question_routes::trace_ad_hoc, crate::definition_routes::admit_definition, crate::definition_routes::install_definition, crate::definition_routes::retire_definition, crate::definition_routes::list_definitions, crate::definition_routes::get_definition), components(schemas(crate::refusal::Refusal, crate::refusal::RefusalDiagnostic, crate::refusal::RefusalSpan, crate::refusal::RefusalRestatement, crate::question_routes::EvaluateRequest, crate::question_routes::AdHocTraceRequest, crate::definition_routes::AdmitDefinitionRequest, crate::definition_routes::InstallDefinitionRequest, crate::definition_routes::RetireDefinitionRequest, crate::definition_routes::AdmitDefinitionResponse, crate::definition_routes::InstallDefinitionResponse, crate::definition_routes::DefinitionReceipt, crate::definition_routes::DefinitionPage, crate::definition_routes::DefinitionSummary, crate::definition_routes::DefinitionDocument, crate::definition_routes::SignoffRequest, crate::definition_routes::SignoffDocument, crate::contracts::NornCheckReceipt, crate::contracts::NornCheckerDiagnostics, crate::answers::NornQueryResult, crate::answers::NornHistoricalQuestionResult, crate::objects::NornObjectPage, crate::objects::NornObjectRecord, crate::objects::NornLinkPage, crate::objects::NornInstanceSourceProvenance)))]
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
