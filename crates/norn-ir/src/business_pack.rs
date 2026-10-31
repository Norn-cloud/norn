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
//! - [`OntologyLinkRoot`]: checked term correspondences from one ontology
//!   pack into another, such as a system pack into the canon
//!   (`packKind: "link"`, norn#56). Semantic only: executable transforms stay
//!   `mapping` artifacts, which a correspondence may name but never embeds.
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
    Link(OntologyLinkRoot),
    Composition(CompositionPackRoot),
}

/// The `packKind` discriminator of a business pack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BusinessPackKind {
    Composition,
    Ontology,
    Link,
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
    /// Non-empty; one closed promotion shape per active shaped term.
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
    /// How the pack uses the source (norn#56 source manifest).
    pub import_mode: SourceImportMode,
    /// The importer that read the source. Required for `import` (checker
    /// rule); a reviewed hand mapping has none.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "SourceImporter")]
    pub importer: Option<SourceImporter>,
    /// Required (non-empty) for vendored sources. A source the pack imports
    /// or maps from is content-pinned: by these hashes or by a commit or
    /// hash in `releaseId` (checker rule).
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

/// How a pack uses one of its sources.
///
/// `import` takes content into the pack's own terms, so its licence must
/// permit redistribution. `map_only` only maps Norn terms to the source; no
/// source content enters the pack (the clean-room mode for copyleft and
/// API-terms sources). `study_only` informs review; no mapping may cite it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SourceImportMode {
    Import,
    MapOnly,
    StudyOnly,
}

/// The importer that read a source: an id and its strict version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceImporter {
    /// A lowercase dotted identifier (`norn.importers.frappe`).
    pub name: String,
    /// A strict semantic version.
    pub version: String,
}

/// A hash-pinned file of a source: a repository path for vendored files,
/// or the absolute HTTPS URL of the pinned bytes otherwise.
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
    /// The active class term a `property` or `action` term belongs to
    /// (required on those, optional on `predicate`, absent on shaped kinds;
    /// checker rule).
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub domain_term_id: Option<String>,
}

/// What an ontology term denotes. `class`, `value_object` and
/// `concept_scheme_root` terms take one closed promotion shape; `property`,
/// `action` and `predicate` terms are vocabulary handles and take none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OntologyTermKind {
    Class,
    Property,
    ValueObject,
    ConceptSchemeRoot,
    /// A business operation on its domain class (posting an invoice).
    Action,
    /// A condition over canon state, such as an admission refusal.
    Predicate,
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

/// The only `packKind` literal of a link pack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OntologyLinkKind {
    #[default]
    Link,
}

/// A link pack: checked correspondences from the terms of one ontology pack
/// (`fromPack`) to the terms of another (`toPack`). Every active term of the
/// source pack is accounted for, by a correspondence or a residual
/// (registry rule).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OntologyLinkRoot {
    /// In the opaque `norn.link.*` namespace (checker rule).
    pub pack_id: String,
    pub pack_version: IrVersion01,
    pub pack_kind: OntologyLinkKind,
    /// The pack whose terms are mapped.
    pub from_pack: LinkedPack,
    /// The pack the terms map into.
    pub to_pack: LinkedPack,
    /// Non-empty.
    pub correspondences: Vec<TermCorrespondence>,
    /// Source terms with no correspondence, and why.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub residuals: Vec<LinkResidual>,
}

/// One linked pack, pinned by its artifact id and exact artifact version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LinkedPack {
    pub pack_id: String,
    pub version: String,
}

/// One checked correspondence from a source-pack term to target-pack terms.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TermCorrespondence {
    /// Unique within the link.
    pub correspondence_id: String,
    /// An active term of `fromPack`.
    pub from_term_id: String,
    pub relation: CorrespondenceRelation,
    /// Active terms of `toPack`: exactly one, or one or more for `derived`.
    pub to_term_ids: Vec<String>,
    pub justification: String,
    /// Non-empty; at least one names a mapped source of `fromPack`'s
    /// manifest (`source.*`), the others are absolute HTTPS IRIs.
    pub evidence_refs: Vec<String>,
    /// 0 through 10000 (checker rule).
    pub confidence_basis_points: WholeNumber,
    pub reviewed_by: String,
    /// UTC RFC 3339.
    pub reviewed_at: String,
    /// The `mapping` artifact that executes a `derived` correspondence, when
    /// one exists. The correspondence is semantic either way.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub transform_ref: Option<String>,
    /// The ruling that settled a divergence this correspondence resolves.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub ruling_ref: Option<String>,
}

/// How a source term relates to its target terms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CorrespondenceRelation {
    /// Same meaning and extension.
    Equivalent,
    /// Every instance of the source term is an instance of the target term.
    SubsumedBy,
    /// Every instance of the target term is an instance of the source term.
    Subsumes,
    /// The source term's value is a function of the target terms.
    Derived,
}

/// A source term the link does not map.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LinkResidual {
    /// An active term of `fromPack`.
    pub term_id: String,
    pub reason: ResidualReason,
    pub note: String,
}

/// Why a source term has no correspondence (norn-platform#2986; lead
/// ruling 2026-10-31 for `non_conforming`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ResidualReason {
    /// Detail specific to the source system; the canon drops it.
    SystemSpecific,
    /// The target lacks a term for it; the canon should grow one.
    CanonGap,
    /// Source behaviour that breaks a target law (editing posted data in
    /// place). The law holds; the link flags the behaviour and never maps it.
    NonConforming,
}
