//! Generated `OpenAPI` components for the core IR and checker wire contract.
use crate::contracts::ContractSchemas;
use utoipa::{Modify, OpenApi};

/// The public core's schema contract; the cell store is embedded through Rust.
#[derive(Debug, OpenApi)]
#[openapi(info(title = "Norn core contracts"), modifiers(&ContractSchemas), paths(crate::question_routes::get_answer, crate::question_routes::get_numeric_trace, crate::question_routes::evaluate_question, crate::question_routes::trace_ad_hoc, crate::run_routes::start_run, crate::run_routes::get_run, crate::run_routes::list_runs), components(schemas(crate::run_routes::StartRunRequest, crate::run_routes::RunDocument, crate::run_routes::RunFlow, crate::run_routes::RunPage, crate::refusal::Refusal, crate::refusal::RefusalDiagnostic, crate::refusal::RefusalSpan, crate::question_routes::EvaluateRequest, crate::question_routes::AdHocTraceRequest, crate::contracts::NornCheckReceipt, crate::contracts::NornCheckerDiagnostics, crate::answers::NornQueryResult, crate::answers::NornHistoricalQuestionResult, crate::objects::NornObjectPage, crate::objects::NornObjectRecord, crate::objects::NornLinkPage, crate::objects::NornInstanceSourceProvenance)))]
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
