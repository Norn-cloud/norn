//! The renderer side of the Widget UX contract (#1762): what a renderer must
//! implement to draw an admitted contract, for renderers that decide before they
//! draw (`@norn/react` holds its own walk to this answer).

use serde_json::json;

use crate::AbiError;

/// The renderer capabilities a Widget UX contract requires (UI-VIEW-1, L6,
/// `norn_ui_checker::ui_sdk::renderer_capabilities`): every view kind,
/// component kind, UI SDK primitive, behaviour, effect, event source and
/// outcome its pages use, and the agent capabilities every renderer needs, as
/// sorted `<family>:<name>` strings. A renderer that lacks one must refuse
/// the contract.
///
/// Returns a JSON array of strings.
///
/// # Errors
///
/// [`AbiError::InvalidInput`] when `ux_json` is not a decodable `WidgetUx`
/// document.
pub fn renderer_capabilities_json(ux_json: &str) -> Result<String, AbiError> {
    let ux: norn_ir::WidgetUx =
        serde_json::from_str(ux_json).map_err(|error| AbiError::InvalidInput {
            argument: "ux",
            detail: error.to_string(),
        })?;
    let required: Vec<String> = norn_ui_checker::ui_sdk::renderer_capabilities(&ux)
        .into_iter()
        .collect();
    Ok(json!(required).to_string())
}
