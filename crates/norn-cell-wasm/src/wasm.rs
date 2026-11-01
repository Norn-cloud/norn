//! The JavaScript exports (`wasm32` only). Each is a thin wrapper over the
//! crate-level function of the same meaning; an [`crate::AbiError`] becomes a
//! thrown JavaScript `Error` with the same message.

use wasm_bindgen::prelude::*;

/// `decodeCommit(commitJson, modelJson)`: see [`crate::decode_commit_json`].
///
/// # Errors
///
/// Throws on invalid input.
#[wasm_bindgen(js_name = decodeCommit)]
pub fn decode_commit(commit_json: &str, model_json: &str) -> Result<String, JsError> {
    crate::decode_commit_json(commit_json, model_json).map_err(JsError::from)
}

/// `checkCommit(commitJson, viewJson, modelJson)`: see
/// [`crate::check_commit_json`].
///
/// # Errors
///
/// Throws on invalid input.
#[wasm_bindgen(js_name = checkCommit)]
pub fn check_commit(
    commit_json: &str,
    view_json: &str,
    model_json: &str,
) -> Result<String, JsError> {
    crate::check_commit_json(commit_json, view_json, model_json).map_err(JsError::from)
}

/// `resolveKey(requestJson, modelJson)`: see [`crate::resolve_key_json`].
///
/// # Errors
///
/// Throws on invalid input.
#[wasm_bindgen(js_name = resolveKey)]
pub fn resolve_key(request_json: &str, model_json: &str) -> Result<String, JsError> {
    crate::resolve_key_json(request_json, model_json).map_err(JsError::from)
}

/// `decodeObserved(observedJson)`: see [`crate::decode_observed_json`].
///
/// # Errors
///
/// Throws on invalid input.
#[wasm_bindgen(js_name = decodeObserved)]
pub fn decode_observed(observed_json: &str) -> Result<String, JsError> {
    crate::decode_observed_json(observed_json).map_err(JsError::from)
}

/// `validateInput(inputJson)`: see [`crate::validate_input_json`].
///
/// # Errors
///
/// Throws on invalid input.
#[wasm_bindgen(js_name = validateInput)]
pub fn validate_input(input_json: &str) -> Result<(), JsError> {
    crate::validate_input_json(input_json).map_err(JsError::from)
}

/// `version()`: see [`crate::version_json`].
#[wasm_bindgen]
#[must_use]
pub fn version() -> String {
    crate::version_json()
}
