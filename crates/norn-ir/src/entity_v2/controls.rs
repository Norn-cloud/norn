//! Definition-owned controls and finite-check evidence; never authority grants.
use super::object::ObjectTypeName;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Controls belonging to the enclosing object type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Controls {
    /// Closed wire profile.
    pub format: ControlsFormat,
    /// Ordered sensitive action sequences.
    pub sensitive_paths: Vec<SensitivePath>,
}

/// Versioned controls vocabulary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum ControlsFormat {
    /// Alpha finite singleton-role checks.
    #[serde(rename = "norn.controls/1")]
    V1,
}

/// A sensitive action sequence on the enclosing object type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SensitivePath {
    /// Must equal the enclosing type, not an arbitrary external target.
    pub object_type: ObjectTypeName,
    /// Exact action identities in required order.
    pub actions: Vec<String>,
    /// Require concrete availability evidence in addition to separation.
    #[serde(default, skip_serializing_if = "super::property::is_false")]
    pub require_witness: bool,
}

/// Safety evidence relative to the declared finite model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ControlStatus {
    /// Separation holds over the complete overapproximation.
    Proven,
    /// A concrete violating model trace is established.
    Refuted,
    /// The check is incomplete or only a possible trace was found.
    Unknown,
}

/// Availability is independent of separation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Reachability {
    /// A concrete path has been witnessed.
    Witnessed,
    /// Only abstract reachability, or incomplete checking.
    Unknown,
    /// Even the complete overapproximation cannot complete the path.
    Unreachable,
}

/// One deterministic controls verdict carried beside ordinary diagnostics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[serde(try_from = "ControlVerdictWire")]
pub struct ControlVerdict {
    /// Effective object type being checked.
    pub object_type: ObjectTypeName,
    /// The declared sensitive sequence.
    pub actions: Vec<String>,
    /// Safety result; not a Lean or runtime-compliance claim.
    pub verdict: ControlStatus,
    /// Independent availability evidence.
    pub reachability: Reachability,
    /// Possible counterexample steps, including the unrestricted prefix.
    pub trace: Vec<String>,
    /// Evidence scope or reason the check remains unresolved.
    pub explanation: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ControlVerdictWire {
    object_type: ObjectTypeName,
    actions: Vec<String>,
    verdict: ControlStatus,
    reachability: Reachability,
    trace: Vec<String>,
    explanation: String,
}

impl TryFrom<ControlVerdictWire> for ControlVerdict {
    type Error = &'static str;
    fn try_from(value: ControlVerdictWire) -> Result<Self, Self::Error> {
        if value.reachability == Reachability::Witnessed && value.trace.is_empty() {
            return Err("witnessed controls require a nonempty concrete trace");
        }
        Ok(Self {
            object_type: value.object_type,
            actions: value.actions,
            verdict: value.verdict,
            reachability: value.reachability,
            trace: value.trace,
            explanation: value.explanation,
        })
    }
}
