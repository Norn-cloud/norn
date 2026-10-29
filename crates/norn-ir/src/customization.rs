//! Typed CustomizationIR root: the payload of a `customization` artifact
//! (T1-IR-1d, #2509).
//!
//! A customization is a governed overlay on one exact target artifact, with an
//! optional capability-scoped executable boundary. Kind consolidation (a diff
//! over an artifact set, ruling 2026-09-09 §7b) is #2633; until then this root
//! mirrors today's validator exactly and nested objects keep unchecked members
//! in `additional` (see [`crate::members`]).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::members::{IrVersion01, WholeNumber, present_non_null};
use crate::rule::JsonObject;

/// Typed payload of a `customization` artifact.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CustomizationRoot {
    pub customization_version: IrVersion01,
    pub customization_id: String,
    pub intent: CustomizationIntent,
    pub target_artifact: CustomizationTarget,
    pub overlay: CustomizationOverlay,
    pub approval: CustomizationApproval,
    /// Scope dimensions the caller must supply (fail closed).
    pub scope_required: Vec<String>,
    /// Required when the overlay contains a `custom_hook` operation.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "CustomizationExecutable")]
    pub executable: Option<CustomizationExecutable>,
    /// Reserved extension slots keyed by `<name>/<semver>`; `null` is rejected.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "JsonObject")]
    pub extensions: Option<JsonObject>,
}

/// Why the customization exists and who asked for it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CustomizationIntent {
    pub summary: String,
    pub requested_by: CustomizationRequester,
    /// Members the validator does not check, kept verbatim.
    #[serde(flatten)]
    pub additional: JsonObject,
}

/// Who requested a customization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CustomizationRequester {
    User,
    Agent,
}

/// The exact artifact a customization overlays.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CustomizationTarget {
    pub artifact_id: String,
    pub kind: CustomizationTargetKind,
    pub version: String,
    /// Members the validator does not check, kept verbatim.
    #[serde(flatten)]
    pub additional: JsonObject,
}

/// Artifact kinds a customization may target (not `setting` or `derived`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CustomizationTargetKind {
    Entity,
    Relation,
    Action,
    Rule,
    Connector,
    Widget,
    Customization,
    BusinessPack,
    Policy,
}

/// The overlay applied to the target.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CustomizationOverlay {
    pub kind: CustomizationOverlayKind,
    /// Non-empty, ordered overlay operations.
    pub operations: Vec<CustomizationOverlayOp>,
    /// Members the validator does not check, kept verbatim.
    #[serde(flatten)]
    pub additional: JsonObject,
}

/// What an overlay changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CustomizationOverlayKind {
    Field,
    Validation,
    Widget,
    Connector,
    Rule,
    Policy,
    Pack,
}

/// One overlay operation, tagged by `op`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "op", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum CustomizationOverlayOp {
    AddField {
        field: String,
        definition: JsonObject,
        #[serde(flatten)]
        additional: JsonObject,
    },
    UpdateField {
        field: String,
        definition: JsonObject,
        #[serde(flatten)]
        additional: JsonObject,
    },
    RemoveField {
        #[serde(flatten)]
        additional: JsonObject,
    },
    AddValidation {
        #[serde(flatten)]
        additional: JsonObject,
    },
    UpdateValidation {
        #[serde(flatten)]
        additional: JsonObject,
    },
    AddWidgetBinding {
        #[serde(flatten)]
        additional: JsonObject,
    },
    /// Requires the root's `executable` boundary declaration.
    CustomHook {
        #[serde(flatten)]
        additional: JsonObject,
    },
}

/// Approval state of a customization.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CustomizationApproval {
    pub state: CustomizationApprovalState,
    pub required: bool,
    /// Members the validator does not check, kept verbatim.
    #[serde(flatten)]
    pub additional: JsonObject,
}

/// Approval states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CustomizationApprovalState {
    Draft,
    Proposed,
    Approved,
    Active,
    Rejected,
}

/// The capability-scoped executable boundary of a customization.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CustomizationExecutable {
    pub boundary: ExecutableBoundary,
    pub input_schema: JsonObject,
    pub output_schema: JsonObject,
    pub error_schema: JsonObject,
    /// Non-empty capability scope.
    pub scope_capabilities: Vec<String>,
    /// Services the code needs.
    pub required_services: Vec<String>,
    pub resource_limits: ResourceLimits,
    /// Only `false` is admitted.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "bool")]
    pub ambient_credentials: Option<bool>,
    /// Only `false` is admitted.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "bool")]
    pub direct_database_access: Option<bool>,
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub source_ref: Option<String>,
    /// Members the validator does not check, kept verbatim.
    #[serde(flatten)]
    pub additional: JsonObject,
}

/// Where customization code runs.
///
/// `effectful_ts` and `wasm` are retired and do not decode: there is no
/// server-side custom TypeScript runtime (ruling 2026-09-25 §11), and inline
/// custom execution is rejected in v1 (ruling 2026-09-27 compute boundary
/// §1/§2, D33). Custom code is a pool Function invoked through `perform`
/// (T1-FN-1, #2776). The checker reports them as
/// `customization.validation.retired_boundary`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExecutableBoundary {
    TrustedRust,
    CapabilityRpc,
}

/// Resource ceilings of a customization executable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ResourceLimits {
    /// At least 1.
    pub max_wall_time_ms: WholeNumber,
    /// At least 1.
    pub max_memory_mb: WholeNumber,
    pub max_network_calls: WholeNumber,
    pub max_storage_bytes: WholeNumber,
    /// Members the validator does not check, kept verbatim.
    #[serde(flatten)]
    pub additional: JsonObject,
}
