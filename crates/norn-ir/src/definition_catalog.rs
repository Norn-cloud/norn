//! The definition catalog (norn#62, Astra plan 2026-10-30): a read-only,
//! versioned projection of admitted IR as relations over definition members.
//!
//! Canonical IR is Norn's metamodel; the catalog is not a second one. It is
//! generated from admitted artifacts by `norn-checker`
//! (`norn_checker::definition_catalog`), never authored, and it is no
//! artifact kind: nothing admits a catalog, so a pack cannot redefine what
//! the checker sees. Rules over definitions (claims, rulings, derived-state
//! and coherence checks) read these relations instead of re-walking payloads.
//!
//! Every reference binds to an exact artifact identity and a stable member:
//! a [`MemberRef`] is `(artifactId, version, member)`, and the catalog's
//! [`CatalogArtifact`] table holds each artifact's kind and Norn1 semantic
//! hash (the checker receipt's per-artifact hash), so a row can always be
//! traced to the bytes it was projected from.
//!
//! The relations ([`DefinitionCatalog`]):
//!
//! - **Type**: object types and interfaces, with supertype and interfaces;
//! - **Property**: declared properties (an extension's are its own rows),
//!   whether each is computed, and a lifecycle property's states;
//! - **Link**: declared links and their target types;
//! - **Transition**: declared lifecycle transitions with their source and
//!   target states;
//! - **Writes**: authored writes: an action's effect program, and a decide
//!   graph's fact-change schema (every combination it admits, an
//!   over-approximation);
//! - **DependsOn**: resolved envelope dependencies and pack membership;
//! - **Mapping**: what each object map of a `mapping` artifact observes into
//!   which target member (observed, never authored);
//! - **Law**: each law's subject and response binding.
//!
//! Rows are sorted and unique, so two projections of the same set are equal
//! and serialize to the same bytes.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ArtifactKind;
use crate::mapping::GapReason;

/// The format tag of a [`DefinitionCatalog`].
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
pub enum CatalogFormat {
    /// `norn.catalog/1`.
    #[serde(rename = "norn.catalog/1")]
    V1,
}

/// One artifact of the projected set.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CatalogArtifact {
    pub artifact_id: String,
    pub version: String,
    pub kind: ArtifactKind,
    /// Norn1 semantic envelope hash, as in the checker receipt.
    pub sha256: String,
    /// Whether the artifact is a candidate (`false`: installed base).
    pub candidate: bool,
}

/// Exact identity of an artifact: one version of one id.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ArtifactKey {
    pub artifact_id: String,
    pub version: String,
}

/// A member of a definition. Names are the IR's own: an object type or
/// interface name, a property, link, lifecycle-property, or transition id,
/// an object-map id, a law id.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum Member {
    /// The artifact as a whole (an action, a decision, a mapping).
    Artifact {},
    /// An object type or interface.
    Type { type_name: String },
    /// A property of a type.
    Property { type_name: String, property: String },
    /// A link of a type.
    Link { type_name: String, link: String },
    /// A state of a type's lifecycle (a value of its lifecycle property).
    State {
        type_name: String,
        lifecycle: String,
        state: String,
    },
    /// A declared transition of a type's lifecycle.
    Transition {
        type_name: String,
        lifecycle: String,
        transition: String,
    },
    /// An object map of a mapping.
    ObjectMap { map: String },
    /// A law.
    Law { law: String },
}

impl Member {
    /// The stable member id within its artifact (`artifact`,
    /// `type:commerce.SalesOrder`, `property:commerce.SalesOrder#status`,
    /// `transition:commerce.SalesOrder#status/invoice`, …). Type names are
    /// dotted, member ids are not, so `#` and `/` never collide.
    #[must_use]
    pub fn id(&self) -> String {
        match self {
            Self::Artifact {} => "artifact".to_owned(),
            Self::Type { type_name } => format!("type:{type_name}"),
            Self::Property {
                type_name,
                property,
            } => format!("property:{type_name}#{property}"),
            Self::Link { type_name, link } => format!("link:{type_name}#{link}"),
            Self::State {
                type_name,
                lifecycle,
                state,
            } => format!("state:{type_name}#{lifecycle}/{state}"),
            Self::Transition {
                type_name,
                lifecycle,
                transition,
            } => format!("transition:{type_name}#{lifecycle}/{transition}"),
            Self::ObjectMap { map } => format!("map:{map}"),
            Self::Law { law } => format!("law:{law}"),
        }
    }
}

/// A member of one exact artifact.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MemberRef {
    pub artifact_id: String,
    pub version: String,
    pub member: Member,
}

/// What a type row defines.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum TypeKind {
    ObjectType,
    Interface,
}

/// **Type**: one object type or interface.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TypeRow {
    pub at: MemberRef,
    pub kind: TypeKind,
    /// Supertypes: an object type's `extends`, an interface's `extends`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extends: Vec<String>,
    /// Interfaces an object type implements directly.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub implements: Vec<String>,
}

/// **Property**: one declared property.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PropertyRow {
    pub at: MemberRef,
    /// Computed from a ZEN Expression: derived and read-only.
    pub computed: bool,
    /// The states, when a lifecycle of the declaring type owns the property.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lifecycle: Option<LifecycleStates>,
}

/// The state set of a lifecycle property.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LifecycleStates {
    /// The property's enum values, in presentation order.
    pub states: Vec<String>,
    pub initial: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub terminal: Vec<String>,
}

/// **Link**: one declared link.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LinkRow {
    pub at: MemberRef,
    /// The linked object type.
    pub target: String,
}

/// **Transition**: one declared transition.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TransitionRow {
    pub at: MemberRef,
    pub from: Vec<String>,
    pub to: String,
}

/// How an authored write reaches its target.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum WriteVia {
    /// An action's `fire_transition` effect.
    FireTransition,
    /// An action's `upsert_input_object` effect.
    UpsertObject,
    /// An action's `mutate_parent_with_rows` effect.
    MutateRows,
    /// A decide graph's `transition` fact change.
    FactTransition,
    /// A decide graph's `set` fact change.
    FactSet,
    /// A decide graph's `create`, `patch`, or `delete` fact change.
    FactObject,
}

/// **Writes**: an authored write of a definition member.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WritesRow {
    pub writer: MemberRef,
    /// A transition, a property, or (for whole-object writes) a type.
    pub target: MemberRef,
    pub via: WriteVia,
}

/// Why one artifact depends on another.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum DependencyVia {
    /// A resolved envelope `dependencies` entry.
    Dependency,
    /// A composition pack's `payload.artifacts` member.
    PackMember,
}

/// **DependsOn**: one resolved reference between artifacts.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DependsOnRow {
    pub from: ArtifactKey,
    pub to: ArtifactKey,
    pub via: DependencyVia,
}

/// What an object map does for one target member.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum MapRule {
    /// The map produces objects of the target type (one row per map).
    Object {},
    /// A property rule.
    Property {},
    /// A link rule.
    Link {},
    /// An observed lifecycle snapshot.
    Lifecycle {},
    /// A declared gap.
    Gap { reason: GapReason },
}

/// **Mapping**: one target member an object map observes, or leaves as a gap.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MappingRow {
    pub map: MemberRef,
    /// The source dataset the map reads.
    pub dataset: String,
    /// The target object type (for [`MapRule::Object`]), or one of its
    /// properties or links, in the artifact that declares it.
    pub target: MemberRef,
    pub rule: MapRule,
}

/// **Law**: one law and the definition its response binds.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LawRow {
    pub at: MemberRef,
    /// The subject object type.
    pub subject: String,
    /// The bound transition, invariant owner, or action; absent for a
    /// predicate response.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binds: Option<LawBinding>,
}

/// The definition a law's response names.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum LawBinding {
    /// A lifecycle transition of a type.
    Transition {
        type_name: String,
        lifecycle: String,
        transition: String,
    },
    /// An invariant of a type.
    Invariant {
        type_name: String,
        invariant: String,
    },
    /// An action artifact.
    Action { artifact_id: String },
}

/// The catalog: every relation over one admitted artifact set. See the
/// module docs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DefinitionCatalog {
    pub format: CatalogFormat,
    pub artifacts: Vec<CatalogArtifact>,
    pub types: Vec<TypeRow>,
    pub properties: Vec<PropertyRow>,
    pub links: Vec<LinkRow>,
    pub transitions: Vec<TransitionRow>,
    pub writes: Vec<WritesRow>,
    pub depends_on: Vec<DependsOnRow>,
    pub mappings: Vec<MappingRow>,
    pub laws: Vec<LawRow>,
}

impl DefinitionCatalog {
    /// The artifact a key names.
    #[must_use]
    pub fn artifact(&self, artifact_id: &str, version: &str) -> Option<&CatalogArtifact> {
        self.artifacts
            .iter()
            .find(|artifact| artifact.artifact_id == artifact_id && artifact.version == version)
    }

    /// Whether `member` is a member of the catalog: an artifact, type,
    /// property, link, transition, object map, or law row, or a state of a
    /// lifecycle property row.
    #[must_use]
    pub fn contains(&self, member: &MemberRef) -> bool {
        match &member.member {
            Member::Artifact {} => self
                .artifact(&member.artifact_id, &member.version)
                .is_some(),
            Member::Type { .. } => self.types.iter().any(|row| &row.at == member),
            Member::Property { .. } => self.properties.iter().any(|row| &row.at == member),
            Member::Transition { .. } => self.transitions.iter().any(|row| &row.at == member),
            Member::ObjectMap { .. } => self.mappings.iter().any(|row| &row.map == member),
            Member::Law { .. } => self.laws.iter().any(|row| &row.at == member),
            Member::Link { .. } => self.links.iter().any(|row| &row.at == member),
            Member::State {
                type_name,
                lifecycle,
                state,
            } => self.properties.iter().any(|row| {
                row.at.artifact_id == member.artifact_id
                    && row.at.version == member.version
                    && row.at.member
                        == (Member::Property {
                            type_name: type_name.clone(),
                            property: lifecycle.clone(),
                        })
                    && row
                        .lifecycle
                        .as_ref()
                        .is_some_and(|states| states.states.contains(state))
            }),
        }
    }
}
