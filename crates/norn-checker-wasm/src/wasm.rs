//! The JavaScript exports (`wasm32` only). Each is a thin wrapper over the
//! crate-level function of the same meaning; an [`crate::AbiError`] becomes a
//! thrown JavaScript `Error` with the same message.

use wasm_bindgen::prelude::*;

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

/// `lifecycleDiagrams(artifactsJson)`: see [`crate::lifecycle_diagrams_json`].
///
/// # Errors
///
/// Throws on invalid input.
#[wasm_bindgen(js_name = lifecycleDiagrams)]
pub fn lifecycle_diagrams(artifacts_json: &str) -> Result<String, JsError> {
    crate::lifecycle_diagrams_json(artifacts_json).map_err(JsError::from)
}

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

/// `canonicalRuntimeValue(json)`: see [`crate::canonical_runtime_value_json`].
///
/// # Errors
///
/// Throws on invalid input.
#[wasm_bindgen(js_name = canonicalRuntimeValue)]
pub fn canonical_runtime_value(json: &str) -> Result<String, JsError> {
    crate::canonical_runtime_value_json(json).map_err(JsError::from)
}

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

/// `version()`: see [`crate::version_json`].
#[wasm_bindgen]
#[must_use]
pub fn version() -> String {
    crate::version_json()
}
