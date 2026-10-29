//! Typed ViewIR v0 root: the payload of a `derived` artifact (T1-IR-1d, #2509).
//!
//! The rulings' "View" primitive is carried on the wire by the existing
//! `derived` kind; the kind string is unchanged (folding View into Widget is
//! #2633). A derived view is pure, deterministic, and recompute-on-read over
//! Source-bearing entity/relation facts and other derived views; no IO, no
//! nondeterminism, no side effects.
//!
//! The shape mirrors today's validator exactly; nested objects keep unchecked
//! members in `additional` (see [`crate::members`]).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::members::{IrVersion01, present_non_null};
use crate::rule::JsonObject;

/// Primitive-basis classification of an artifact as Source, Derived, Mutation, or Effect.
/// Derived views must be pure, deterministic, and declared over Source facts and other views.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PrimitiveBasisClass {
    Source,
    Derived,
    Mutation,
    Effect,
}

/// Identifies a Source-bearing entity or relation, or another Derived view, that a
/// Derived view reads. References are exact-versioned to make the read-set computable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ViewInputRef {
    /// Artifact id of the source entity, relation, or derived view being read.
    pub artifact_id: String,
    /// Kind of the referenced artifact.
    pub kind: ViewInputKind,
    /// Exact version of the referenced artifact.
    pub version: String,
    /// Alias used in the calc expression (e.g. "invoice", "`tax_rate`"); unique per view.
    pub alias: String,
    /// Members the validator does not check, kept verbatim.
    #[serde(flatten)]
    pub additional: JsonObject,
}

/// Artifact kinds a derived view may read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ViewInputKind {
    Entity,
    Relation,
    Derived,
}

/// `mode: "current"` — resolve against latest committed facts (default).
/// `mode: "as_of_param"` — resolve against the fact log up to a caller-supplied timestamp.
///   Views with this mode MUST list an input named "`as_of`" in their read-set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TemporalScopeMode {
    Current,
    AsOfParam,
}

/// Optional time-dimension declaration for a Derived view.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TemporalScope {
    pub mode: TemporalScopeMode,
    /// Members the validator does not check, kept verbatim.
    #[serde(flatten)]
    pub additional: JsonObject,
}

/// Maintenance strategy. Only `recompute_on_read` is supported in v0.
/// `incremental` is reserved for a future IVM phase and MUST be rejected at extraction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ViewMaintenance {
    RecomputeOnRead,
    /// Reserved — not implemented. The checker rejects artifacts carrying this value.
    Incremental,
}

/// Serializable calc expression leaf for a Derived view.
///
/// In ViewIR v0 only the `field_projection` form is admitted. `effectful_ts`
/// is decodable so the checker can reject it (IO belongs in an Effect
/// artifact); `raw_promise` is never admitted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum CalcExpr {
    /// Simple field projection: select a non-empty subset of named fields from the primary input.
    FieldProjection {
        fields: Vec<String>,
        /// Members the validator does not check, kept verbatim.
        #[serde(flatten)]
        additional: JsonObject,
    },
    /// Effectful TS leaf reference. Rejected inside a Derived view.
    EffectfulTs { source_ref: String },
}

/// ViewIR v0 payload — the typed View root of a `derived` artifact.
///
/// Constraints enforced by `norn-checker`:
/// - `inputs` must be non-empty (a view with no inputs is a static constant — use a `setting`).
/// - `calc` must not be a `CalcExpr::EffectfulTs` variant (IO in a view is rejected).
/// - `maintenance` must be `recompute_on_read` (`incremental` is rejected as not-yet-supported).
/// - If `temporal_scope.mode == "as_of_param"`, one input alias must be "`as_of`".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ViewIrPayload {
    pub view_version: IrVersion01,
    pub view_id: String,
    pub inputs: Vec<ViewInputRef>,
    pub calc: CalcExpr,
    pub output_schema: JsonObject,
    pub maintenance: ViewMaintenance,
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "TemporalScope")]
    pub temporal_scope: Option<TemporalScope>,
    /// Scope dimensions the caller must supply (fail closed). Optional; when
    /// present, a non-empty list of non-empty strings (checker rule).
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Vec<String>")]
    pub scope_required: Option<Vec<String>>,
    /// Reserved extension slots keyed by `<name>/<semver>`; `null` is rejected.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "JsonObject")]
    pub extensions: Option<JsonObject>,
}
