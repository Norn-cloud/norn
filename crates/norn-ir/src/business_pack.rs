//! Typed `BusinessPack` root: the payload of a `business_pack` artifact
//! (T1-IR-1e, #2820).
//!
//! Today's validator accepts two forms, selected by `packKind`:
//!
//! - [`CompositionPackRoot`]: an exact-version set of artifacts that ship
//!   together (`packKind: "composition"`, or no `packKind` for IR `0.1`
//!   composition packs that predate the discriminator).
//! - [`OntologyPackRoot`]: a closed-world ontology profile — pinned sources,
//!   Norn-owned terms, reviewed external mappings, and closed promotion shapes
//!   (`packKind: "ontology"`). Semantic only: it never grants mutation
//!   authority.
//!
//! Both forms are closed at the top. The composition form's artifact
//! references keep unchecked members in a flattened `additional` map (see
//! [`crate::members`]); every ontology object is closed, because the ontology
//! validator rejects unknown members at every level. Identifier namespaces,
//! date/IRI/hash formats, uniqueness, cross-references, and numeric ranges are
//! checker rules.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::members::{IrVersion01, WholeNumber, one_of_forms, present_non_null};
use crate::rule::JsonObject;

/// Typed payload of a `business_pack` artifact.
///
/// The variants are distinguished by `packKind`, which the composition form
/// may omit, so the enum is untagged: the checker selects the form exactly as
/// the validator does (`packKind: "ontology"` → ontology form, otherwise the
/// composition form).
///
/// The published schema is a `oneOf`: the forms are disjoint because the
/// ontology form requires `packKind: "ontology"`, while the closed
/// composition form admits `packKind` only as `"composition"` or absent (the
/// legacy IR `0.1` form).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
#[schemars(transform = one_of_forms)]
pub enum BusinessPackRoot {
    Ontology(OntologyPackRoot),
    Composition(CompositionPackRoot),
}

/// The `packKind` discriminator of a business pack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BusinessPackKind {
    Composition,
    Ontology,
}

/// The only `packKind` literal of a composition pack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CompositionPackKind {
    #[default]
    Composition,
}

/// The only `packKind` literal of an ontology pack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OntologyPackKind {
    #[default]
    Ontology,
}

/// A composition pack: the exact artifact versions that ship together.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CompositionPackRoot {
    pub pack_id: String,
    pub pack_version: String,
    /// Omitted by IR `0.1` composition packs; newly authored packs emit it.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "CompositionPackKind")]
    pub pack_kind: Option<CompositionPackKind>,
    /// Capabilities the pack provides.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Vec<String>")]
    pub provides: Option<Vec<String>>,
    /// Non-empty list of the pack's artifacts.
    pub artifacts: Vec<PackArtifactRef>,
    /// Reserved extension slots keyed by `<name>/<semver>`; `null` is rejected.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "JsonObject")]
    pub extensions: Option<JsonObject>,
}

/// One exact artifact version in a composition pack.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PackArtifactRef {
    pub artifact_id: String,
    pub kind: PackArtifactKind,
    pub version: String,
    /// Members the validator does not check, kept verbatim.
    #[serde(flatten)]
    pub additional: JsonObject,
}

/// Artifact kinds a composition pack may contain (not `derived`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PackArtifactKind {
    Entity,
    Relation,
    Action,
    Rule,
    Connector,
    Widget,
    Customization,
    Setting,
    BusinessPack,
    Policy,
    /// A discovered source schema included in a mapping pack.
    SourceSchema,
    /// An interface-scoped source-to-Core mapping.
    Mapping,
    /// Scoped temporal law.
    Law,
    /// A claim about definitions.
    Claim,
    /// A governed ruling.
    Ruling,
}

/// An ontology pack: a closed-world, semantic-only ontology profile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OntologyPackRoot {
    /// In the opaque `norn.ontology-pack.*` namespace (checker rule).
    pub pack_id: String,
    pub pack_version: IrVersion01,
    pub pack_kind: OntologyPackKind,
    /// Non-empty; pinned upstream source releases.
    pub sources: Vec<OntologySource>,
    /// Non-empty; Norn-owned terms.
    pub terms: Vec<OntologyTerm>,
    /// Non-empty; reviewed mappings from terms to external IRIs.
    pub mappings: Vec<OntologyMapping>,
    /// Non-empty; one closed promotion shape per active term.
    pub shapes: Vec<OntologyShape>,
    /// Reserved extension slots keyed by `<name>/<semver>`; `null` is rejected.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "JsonObject")]
    pub extensions: Option<JsonObject>,
}

/// A pinned upstream source release of an ontology pack.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OntologySource {
    /// In the lowercase `source.*` namespace, unique within the pack.
    pub source_id: String,
    /// A strict immutable version, dated-standard release, or content pin.
    pub release_id: String,
    /// `YYYY-MM-DD`.
    pub released_at: String,
    /// `YYYY-MM-DD`.
    pub retrieved_at: String,
    /// Absolute HTTPS URL.
    pub canonical_url: String,
    pub license: String,
    pub redistribution: SourceRedistribution,
    /// Required (non-empty) for vendored sources.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "Vec<OntologySourceFile>")]
    pub files: Option<Vec<OntologySourceFile>>,
}

/// Whether a source is only referenced or vendored into the repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SourceRedistribution {
    ReferenceOnly,
    Vendored,
}

/// A hash-pinned file of a vendored source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OntologySourceFile {
    pub path: String,
    /// 64 lowercase hexadecimal characters.
    pub sha256: String,
}

/// A Norn-owned ontology term.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OntologyTerm {
    /// An opaque lowercase `norn.*` identifier, unique within the pack.
    pub term_id: String,
    pub label: String,
    pub definition: String,
    pub term_kind: OntologyTermKind,
    pub status: OntologyTermStatus,
    pub semantic_authority: SemanticAuthority,
    /// Resolves within the pack to a compatible, acyclic parent.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub parent_term_id: Option<String>,
    /// Required exactly on `superseded` terms; names an active term.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub superseded_by: Option<String>,
}

/// What an ontology term denotes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OntologyTermKind {
    Class,
    Property,
    ValueObject,
    ConceptSchemeRoot,
}

/// Lifecycle status of an ontology term.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OntologyTermStatus {
    Active,
    Deprecated,
    Superseded,
}

/// Ontology terms are semantic only: they never grant mutation authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SemanticAuthority {
    SemanticOnly,
}

/// A reviewed mapping from a Norn term to an external IRI of a pinned source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OntologyMapping {
    /// Unique within the pack.
    pub mapping_id: String,
    pub term_id: String,
    pub source_id: String,
    /// Absolute HTTPS IRI.
    pub external_iri: String,
    pub relation: MappingRelation,
    pub justification: String,
    /// Non-empty.
    pub evidence_refs: Vec<String>,
    /// 0 through 10000 (checker rule).
    pub confidence_basis_points: WholeNumber,
    pub reviewed_by: String,
    /// UTC RFC 3339.
    pub reviewed_at: String,
    /// Names a mapping of the same term and source in the pack.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub superseded_by_mapping_id: Option<String>,
    /// Who ratified the mapping: a non-empty string, required on
    /// `exact_match` mappings and optional on the other relations (checker
    /// rule).
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub ratified_by: Option<String>,
    /// When the mapping was ratified: a UTC RFC 3339 string, required on
    /// `exact_match` mappings and optional on the other relations (checker
    /// rule).
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub ratified_at: Option<String>,
}

/// SKOS-style mapping relations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MappingRelation {
    ExactMatch,
    CloseMatch,
    BroadMatch,
    NarrowMatch,
    RelatedMatch,
}

/// A closed promotion shape for one active term.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OntologyShape {
    /// Unique within the pack.
    pub shape_id: String,
    pub term_id: String,
    /// Must be `true` (checker rule).
    pub closed: bool,
    /// Non-empty.
    pub fields: Vec<OntologyShapeField>,
}

/// One field of an ontology shape.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OntologyShapeField {
    /// Unique within the shape.
    pub field_id: String,
    pub value_type: ShapeValueType,
    /// Required exactly on `term_ref` fields.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub target_term_id: Option<String>,
    pub min_count: WholeNumber,
    /// At least `minCount` (checker rule).
    pub max_count: WholeNumber,
}

/// Value types of an ontology shape field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ShapeValueType {
    String,
    Boolean,
    Integer,
    Decimal,
    DateTime,
    Iri,
    TermRef,
    ExternalIdentifier,
    CategoryRef,
}
