//! The JavaScript exports (`wasm32` only). Each is a thin wrapper over the
//! crate-level function of the same meaning; an [`crate::AbiError`] becomes a
//! thrown JavaScript `Error` with the same message.

use wasm_bindgen::prelude::*;

#[cfg(feature = "checker")]
/// `checkArtifactSet(artifactsJson, contextJson)`: see
/// [`crate::check_artifact_set_json`].
///
/// # Errors
///
/// Throws on invalid input.
#[wasm_bindgen(js_name = checkArtifactSet)]
pub fn check_artifact_set(artifacts_json: &str, context_json: &str) -> Result<String, JsError> {
    crate::check_artifact_set_json(artifacts_json, context_json).map_err(JsError::from)
}

#[cfg(feature = "checker")]
/// `admitQuestion(artifactsJson, metricsJson, questionJson)`: see
/// [`crate::admit_question_json`].
///
/// # Errors
///
/// Throws on invalid input.
#[wasm_bindgen(js_name = admitQuestion)]
pub fn admit_question(
    artifacts_json: &str,
    metrics_json: &str,
    question_json: &str,
) -> Result<String, JsError> {
    crate::admit_question_json(artifacts_json, metrics_json, question_json).map_err(JsError::from)
}

#[cfg(feature = "checker")]
/// `lifecycleDiagrams(artifactsJson)`: see [`crate::lifecycle_diagrams_json`].
///
/// # Errors
///
/// Throws on invalid input.
#[wasm_bindgen(js_name = lifecycleDiagrams)]
pub fn lifecycle_diagrams(artifacts_json: &str) -> Result<String, JsError> {
    crate::lifecycle_diagrams_json(artifacts_json).map_err(JsError::from)
}

#[cfg(feature = "checker")]
/// `checkExpression(expr, envArtifactsJson, expectedType, selfType?)`: see
/// [`crate::check_expression_json`].
///
/// # Errors
///
/// Throws on invalid input.
#[wasm_bindgen(js_name = checkExpression)]
#[allow(
    clippy::needless_pass_by_value,
    reason = "wasm-bindgen passes an optional string argument by value"
)]
pub fn check_expression(
    expr: &str,
    env_artifacts_json: &str,
    expected_type: &str,
    self_type: Option<String>,
) -> Result<String, JsError> {
    crate::check_expression_json(
        expr,
        env_artifacts_json,
        expected_type,
        self_type.as_deref(),
    )
    .map_err(JsError::from)
}

/// `evaluateExpression(expr, inputJson, profile?)`: see
/// [`crate::evaluate_expression_json`]. `inputJson` is raw JSON text so decimal
/// tokens keep their exact values in JavaScript.
/// Expected refusals, including invalid input, are returned as typed JSON
/// results.
#[wasm_bindgen(js_name = evaluateExpression)]
#[must_use]
#[allow(
    clippy::needless_pass_by_value,
    reason = "wasm-bindgen passes an optional string argument by value"
)]
pub fn evaluate_expression(expr: &str, input_json: &str, profile: Option<String>) -> String {
    crate::evaluate_expression_json(expr, input_json, profile.as_deref())
}

#[cfg(feature = "checker")]
/// `canonicalRuntimeValue(json)`: see [`crate::canonical_runtime_value_json`].
///
/// # Errors
///
/// Throws on invalid input.
#[wasm_bindgen(js_name = canonicalRuntimeValue)]
pub fn canonical_runtime_value(json: &str) -> Result<String, JsError> {
    crate::canonical_runtime_value_json(json).map_err(JsError::from)
}

#[cfg(feature = "checker")]
/// `functionContract(manifestJson, invocationJson?, receiptJson?)`: see
/// [`crate::function_contract_json`].
///
/// # Errors
///
/// Throws on invalid input.
#[wasm_bindgen(js_name = functionContract)]
#[allow(
    clippy::needless_pass_by_value,
    reason = "wasm-bindgen passes optional string arguments by value"
)]
pub fn function_contract(
    manifest_json: &str,
    invocation_json: Option<String>,
    receipt_json: Option<String>,
) -> Result<String, JsError> {
    crate::function_contract_json(
        manifest_json,
        invocation_json.as_deref(),
        receipt_json.as_deref(),
    )
    .map_err(JsError::from)
}

#[cfg(feature = "checker")]
/// `checkAgent(agentJson, contextJson)`: see [`crate::check_agent_json`].
///
/// # Errors
///
/// Throws on invalid input.
#[wasm_bindgen(js_name = checkAgent)]
pub fn check_agent(agent_json: &str, context_json: &str) -> Result<String, JsError> {
    crate::check_agent_json(agent_json, context_json).map_err(JsError::from)
}

#[cfg(feature = "checker")]
/// `toolPort(schemaJson)`: see [`crate::tool_port_json`].
///
/// # Errors
///
/// Throws on invalid input.
#[wasm_bindgen(js_name = toolPort)]
pub fn tool_port(schema_json: &str) -> Result<String, JsError> {
    crate::tool_port_json(schema_json).map_err(JsError::from)
}

#[cfg(feature = "checker")]
/// `toolEdge(sourceSchemaJson, targetSchemaJson)`: see [`crate::tool_edge_json`].
///
/// # Errors
///
/// Throws on invalid input.
#[wasm_bindgen(js_name = toolEdge)]
pub fn tool_edge(source_schema_json: &str, target_schema_json: &str) -> Result<String, JsError> {
    crate::tool_edge_json(source_schema_json, target_schema_json).map_err(JsError::from)
}

#[cfg(feature = "checker")]
/// `version()`: see [`crate::version_json`].
#[wasm_bindgen]
#[must_use]
pub fn version() -> String {
    crate::version_json()
}

#[cfg(feature = "checker")]
/// Read-only `explainArtifactSet` shared with the native CLI.
/// # Errors
/// Throws on malformed transport input.
#[wasm_bindgen(js_name = explainArtifactSet)]
pub fn explain_artifact_set(
    artifacts_json: &str,
    context_json: &str,
    kind: &str,
    id: &str,
    options_json: &str,
) -> Result<String, JsError> {
    crate::explain_artifact_set_json(artifacts_json, context_json, kind, id, options_json)
        .map_err(JsError::from)
}

#[cfg(feature = "checker")]
/// `restateQuestion(questionJson, metricsJson)`: see
/// [`crate::restate_question_json`].
///
/// # Errors
///
/// Throws on invalid input.
#[wasm_bindgen(js_name = restateQuestion)]
pub fn restate_question(question_json: &str, metrics_json: &str) -> Result<String, JsError> {
    crate::restate_question_json(question_json, metrics_json).map_err(JsError::from)
}

#[cfg(feature = "checker")]
/// `restateDashboard(payloadJson, metricsJson)`: see
/// [`crate::restate_dashboard_json`].
///
/// # Errors
///
/// Throws on invalid input.
#[wasm_bindgen(js_name = restateDashboard)]
pub fn restate_dashboard(payload_json: &str, metrics_json: &str) -> Result<String, JsError> {
    crate::restate_dashboard_json(payload_json, metrics_json).map_err(JsError::from)
}
