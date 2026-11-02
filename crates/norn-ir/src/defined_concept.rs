//! Defined concepts classify existing identities; membership grants no write authority.
use crate::members::present_non_null;
use crate::{JsonObject, ResourceLimitProfile, ZenExpression, definition_catalog::MemberRef};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// GRAPH-DAG-1 / GRAPH-CLASS-1: a pack-owned, pure classification declaration.
/// Semantic admission must resolve every pin and type-check the predicate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DefinedConceptRoot {
    /// Qualified classification identity, distinct from the base object kind.
    pub id: String,
    /// Definition version; must match its artifact envelope.
    pub version: String,
    /// Exact type member in an entity artifact required by this artifact.
    pub base: MemberRef,
    /// A pure Boolean expression over the authorized base snapshot.
    pub predicate: ZenExpression,
    /// Exact member dependencies, including other classifications.
    pub dependencies: Vec<MemberRef>,
    /// Existing resource vocabulary and units, capped by the admitted profile.
    pub bounds: ResourceLimitProfile,
    /// Reserved versioned extension slots, as on other artifact roots.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "JsonObject")]
    pub extensions: Option<JsonObject>,
}
