//! JSON ABI entry points for `LAW-HONESTY-1` and `RED-TEAM-1`.

use crate::checker::{AbiError, envelopes_or_invalid, parse_array};

/// LAW-HONESTY-1: classify every law artifact in `artifacts_json` with honest
/// runtime enforcement (`enforced` vs `declared-only`) and proof status.
///
/// # Errors
///
/// Invalid JSON or undecodable artifact envelopes.
pub fn law_honesty_json(artifacts_json: &str) -> Result<String, AbiError> {
    let artifacts = parse_array("artifacts", artifacts_json)?;
    let envelopes = envelopes_or_invalid("artifacts", &artifacts)?;
    let statuses: Vec<serde_json::Value> =
        norn_checker::law_honesty::classify_law_honesty(&envelopes)
            .iter()
            .map(norn_checker::law_honesty::LawReceiptEntry::to_json_value)
            .collect();
    serde_json::to_string(&statuses).map_err(|error| AbiError::Receipt(error.to_string()))
}

/// RED-TEAM-1: execute terminating `norn-datalog` scope-confinement, external
/// settlement receipt-binding, recursive lifecycle `SoD`, and unconstrained-write
/// checks over `artifacts_json`.
///
/// # Errors
///
/// Invalid JSON or undecodable artifact envelopes.
pub fn analyze_red_team_json(artifacts_json: &str) -> Result<String, AbiError> {
    let artifacts = parse_array("artifacts", artifacts_json)?;
    let envelopes = envelopes_or_invalid("artifacts", &artifacts)?;
    let report = norn_checker::red_team::analyze_red_team(&envelopes).to_json_value();
    serde_json::to_string(&report).map_err(|error| AbiError::Receipt(error.to_string()))
}
