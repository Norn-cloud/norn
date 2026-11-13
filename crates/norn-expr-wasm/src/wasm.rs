//! The JavaScript binding: the same `evaluateExpression` the checker package exports.

use wasm_bindgen::prelude::wasm_bindgen;

/// `evaluateExpression(expr, inputJson, profile?)`: see [`crate::evaluate_expression_json`].
/// `inputJson` is raw JSON text so decimal tokens keep their exact values in JavaScript. Expected
/// refusals, including invalid input, are returned as typed JSON results.
#[wasm_bindgen(js_name = evaluateExpression)]
#[must_use]
#[allow(
    clippy::needless_pass_by_value,
    reason = "wasm-bindgen passes an optional string argument by value"
)]
pub fn evaluate_expression(expr: &str, input_json: &str, profile: Option<String>) -> String {
    crate::evaluate_expression_json(expr, input_json, profile.as_deref())
}
