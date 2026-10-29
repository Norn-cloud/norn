//! `MappingIR`: reviewed, interface-scoped mappings from a discovered source
//! schema to `EntityIR` objects (T8-MAP-1a, #2600; ruling 2026-09-25 §27
//! "map interfaces, not systems"; decision record
//! `knowledge/architecture/build-plan-2026-09-09/source-discovery-decision.md`).
//!
//! The lowering is `SourceSchemaIR → MappingIR → EntityIR`: a
//! [`MappingIr`] reads records of the datasets a
//! [`crate::source_schema::SourceSchemaIr`] declares and produces objects of
//! `EntityIR` v2 object types that implement the interfaces it is scoped to.
//! Nothing maps a source system wholesale; a mapping names the interfaces it
//! covers.
//!
//! The expressive subset follows R2RML/RML: per [`ObjectMap`] (an RML
//! *triples map*) a logical source (a dataset, optionally nested in a parent
//! document's child table: the RML iterator), a subject rule (the object's
//! key), property maps, link maps (RML *referencing object maps*, joined to
//! another map by the target key), and explicit gaps. Lifecycle properties
//! are not written by property maps: a [`LifecycleMap`] emits the state the
//! source reports, and the kernel reconciles it through declared transitions
//! (ruling 2026-09-25 §2).
//!
//! **Transforms are ZEN Expressions, never code.** Every rule is a bounded
//! `norn-expr/4` expression over `input`, the source record (a closed record
//! of the dataset's fields), type-checked by `norn-checker`'s `mapping`
//! module against the target member's type. The transform id of a rule is
//! `<map id>.<member>` ([`ObjectMap::transform_id`]); every produced fact
//! records the source record, the mapping key, and the transform
//! (provenance, T8-MAP-1b).
//!
//! A mapping is the payload of a `mapping` artifact. It binds to exactly
//! one `source_schema` artifact by id, version, and hash, and names its
//! target interfaces and object types by the artifact ids of the `entity`
//! artifacts that define them (Norn Core, through the registry package's
//! dependencies). Transforms read only the source record: no actor, clock,
//! slice, or IO.

use schemars::{JsonSchema, Schema, schema_for};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::entitlement::EntitlementStatus;
use crate::members::present_non_null;
use crate::provenance::InstanceProvenance;
use crate::rule::JsonObject;
use crate::{LinkId, ObjectTypeName, PropertyId, RuntimeObject, ZenExpression};

/// The format tag of a [`MappingIr`] document.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum MappingFormat {
    /// `norn.mapping/1`.
    #[serde(rename = "norn.mapping/1")]
    V1,
}

/// An interface-scoped mapping from one source schema: the payload of a
/// `mapping` artifact. The envelope's `artifactId` and `version` identify
/// the mapping; its provenance key is `<artifactId>@<version>` (both
/// checked so the key is unambiguous). See the module docs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MappingIr {
    /// The format tag.
    pub format: MappingFormat,
    /// The one `source_schema` artifact the mapping reads, pinned by
    /// content hash.
    pub source: SourceBinding,
    /// Artifact ids of the `entity` artifacts defining the interfaces the
    /// mapping covers: every map targets one of them.
    pub interfaces: Vec<String>,
    /// The object maps, in evaluation order.
    pub maps: Vec<ObjectMap>,
    /// Declarative permission-row maps. These emit ordinary upsert actions
    /// for `core.Entitlement`; joins are applied only from a caller-declared
    /// complete snapshot of every `requiredSnapshot` dataset.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entitlements: Vec<EntitlementMap>,
    /// Explicit exact bindings from external account ids to canonical
    /// principals. This is a reviewed mapping configuration table, separate
    /// from source metadata; an empty table refuses principal resolution.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub identity_bindings: Vec<IdentityBinding>,
    /// Reserved extension slots keyed by `<name>/<semver>`; `null` is rejected.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "JsonObject")]
    pub extensions: Option<JsonObject>,
}

/// A checked, declarative projection of source permission rows into the
/// canonical entitlement record. It contains no source-specific Rust logic:
/// the source's joins, supported action meanings, identity bindings, and
/// unsupported-row coverage are all data.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EntitlementMap {
    /// Unique id of this map within the mapping.
    pub id: String,
    /// Alias used for the primary source dataset in expressions and field paths.
    pub source_alias: String,
    /// Primary permission dataset.
    pub source: LogicalSource,
    /// Other datasets joined by exact scalar equality. `anti` joins suppress
    /// a candidate when any matching record exists.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub joins: Vec<EntitlementJoin>,
    /// Dataset ids which must be explicitly marked complete before this map
    /// can emit any entitlement. This prevents a paginated/delta prefix from
    /// being interpreted as a complete authorization snapshot.
    pub required_snapshot: Vec<String>,
    /// Target business Norn type, checked against the mapped entity catalog.
    pub target_type: ObjectTypeName,
    /// Principal, role, or group expression. Principal keys are resolved
    /// only through the explicit identity binding table below.
    pub grantee: EntitlementGranteeMap,
    /// All predicates that must hold for this source row to be representable.
    /// False predicates produce no entitlement, never a widened one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub eligibility: Vec<ZenExpression>,
    /// A closed list of recognized action meanings and their source guards.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<EntitlementActionMap>,
    /// Exact scalar resource attributes. Each value is a checked expression
    /// over the joined row; an empty map means no additional scope constraint.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub scope_attributes: BTreeMap<String, ZenExpression>,
    /// The one relationship condition expressible by this source row.
    pub condition: EntitlementConditionMap,
    /// Source record id and version fields from the primary row. Joined row
    /// ids and versions are included in the generated stable identity and
    /// provenance when the joins are inner joins.
    pub source_record: String,
    /// Source version field on the primary row.
    pub source_version: String,
    /// Static coverage classification for this rule family.
    pub coverage: EntitlementCoverage,
    /// Imported entitlements are usually shadow until promoted after review.
    pub status: EntitlementStatus,
}

/// One exact equality join to another source dataset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EntitlementJoin {
    /// Flattened expression alias for an inner join, or a stable id for an
    /// anti-join.
    pub alias: String,
    /// Dataset to join.
    pub dataset: String,
    /// `inner` produces one candidate per matching row; `anti` suppresses
    /// candidates when a matching row exists.
    pub mode: EntitlementJoinMode,
    /// Optional cardinality guard. `atMostOne` suppresses the candidate when
    /// more than one record matches; a matching inner join is therefore
    /// exactly one. It is useful when the destination condition can encode
    /// only one source restriction.
    #[serde(default)]
    pub cardinality: EntitlementJoinCardinality,
    /// Equality predicates between previously bound aliases and fields in
    /// this dataset. All predicates must match.
    pub on: Vec<EntitlementJoinKey>,
    /// Record id and version fields, required for an inner join so generated
    /// entitlement identity and provenance cover every contributing row.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub record_id: Option<String>,
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub version: Option<String>,
}

/// Join direction; deliberately limited to matching rows or exclusion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum EntitlementJoinMode {
    /// Emit candidates for every matching row.
    Inner,
    /// Emit no candidate when any matching row exists.
    Anti,
}

/// Cardinality guard on a source join.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum EntitlementJoinCardinality {
    /// Keep all matching rows.
    #[default]
    Many,
    /// Keep a candidate only when at most one joined row matches.
    AtMostOne,
}

/// One equality between a bound flattened alias field and a field in the
/// dataset being joined.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EntitlementJoinKey {
    /// The already-bound alias and field (`source.role`, for example).
    pub left: EntitlementFieldPath,
    /// Field in the joined dataset.
    pub right: String,
}

/// A field path in a permission row join. In ZEN expressions, its equivalent
/// input member is `input.<alias>_<field>`; the compiler checks that member's
/// source type before accepting the expression.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EntitlementFieldPath {
    /// The primary alias or an earlier inner-join alias.
    pub alias: String,
    /// Source field name.
    pub field: String,
}

/// The typed grantee rule for a mapped entitlement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EntitlementGranteeMap {
    /// The canonical grantee variant.
    pub kind: EntitlementGranteeKind,
    /// Canonical Norn object type (`identity.Role`, `identity.Group`, or
    /// `party.Person`).
    pub object_type: ObjectTypeName,
    /// Source field path containing the grantee key.
    pub key: EntitlementFieldPath,
}

/// Grantee variant emitted by a declarative permission map.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum EntitlementGranteeKind {
    /// A role entity.
    Role,
    /// A group entity.
    Group,
    /// A canonical principal entity, resolved through `identityBindings`.
    Principal,
}

/// A recognized source action and the checked predicate that admits it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EntitlementActionMap {
    /// Exact Norn action id.
    pub action: String,
    /// Boolean predicate over the joined source row.
    pub when: ZenExpression,
}

/// A typed canonical entitlement condition whose dynamic key is sourced from
/// one joined row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum EntitlementConditionMap {
    /// No additional relationship condition.
    None,
    /// The target's named link must point to the acting principal.
    OwnerOnly {
        /// Target-side owner link, checked against the target type.
        link: LinkId,
    },
    /// The target's named link must point to this exact canonical object.
    LinkedRecord {
        /// Target-side link, checked against the target type.
        link: LinkId,
        /// Exact type of the linked record.
        target_type: ObjectTypeName,
        /// Source field path holding the linked Norn object's key.
        target_key: EntitlementFieldPath,
    },
}

/// One reviewer-approved mapping from a source principal key to a canonical
/// Norn object reference. No email or username inference is permitted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IdentityBinding {
    /// Exact source key, compared without normalization or inference.
    pub source_key: String,
    /// Canonical Norn reference type.
    pub object_type: ObjectTypeName,
    /// Canonical object key from the trusted identity snapshot.
    pub key: String,
    /// Review evidence for this binding.
    pub evidence: String,
}

/// Generic mapping coverage category.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EntitlementCoverage {
    /// Whether this rule is exact, narrowed, or missing relative to its
    /// declared source feature family.
    pub status: EntitlementCoverageStatus,
    /// Human-readable reason, including unsupported source constructs.
    pub reason: String,
}

/// Coverage category for imported permission features.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum EntitlementCoverageStatus {
    /// Every declared source case represented by this rule has a proven
    /// equivalent Norn entitlement.
    Exact,
    /// This rule represents only a conservative subset of the source cases.
    Narrowed,
    /// No Norn entitlement is emitted for this source feature.
    Missing,
}

impl EntitlementFieldPath {
    /// The closed `input` record member used to typecheck and evaluate this
    /// field path.
    #[must_use]
    pub fn input_field(&self) -> String {
        format!("{}_{}", self.alias, self.field)
    }
}

/// The `source_schema` artifact a mapping binds to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceBinding {
    /// The source schema's `artifactId`.
    pub schema_id: String,
    /// The source schema's artifact `version`.
    pub version: String,
    /// Lower-case hex SHA-256 of the source schema artifact (the Norn1
    /// canonical envelope hash `norn-checker` records in check receipts).
    pub sha256: String,
}

/// Authored vendor qualification carried in `norn.mapping.vendor/1.0.0`.
/// API access terms are evidence, never redistribution permission. The bound
/// source schema contains only mapped field requirements, not vendor metadata.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VendorMappingContract {
    /// Exact source system name reported by the tenant discovery adapter.
    pub system: String,
    /// Exact supported releases or immutable source revision hashes.
    pub versions: Vec<String>,
    /// Evidence of the API access terms; independent of artifact provenance.
    pub api_terms: crate::source_schema::EvidenceRef,
}

/// An explicitly generic authored registry mapping makes no vendor parity claim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct GenericMappingContract {
    /// Only the unqualified generic contract is admitted.
    pub parity: GenericMappingParity,
}

/// Generic mappings do not certify compatibility with vendor releases.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GenericMappingParity {
    /// No vendor-version parity assertion.
    Unqualified,
}

/// How one dataset's records become objects of one object type (an RML
/// triples map).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ObjectMap {
    /// Unique id of the map within the mapping, in the member id grammar
    /// (`sales_order`); the prefix of its transform ids.
    pub id: String,
    /// Where the records come from.
    pub source: LogicalSource,
    /// What the records become.
    pub target: MapTarget,
    /// Whether this map applies to the source record. A false result skips
    /// the record before evaluating its subject or source identity. Omitted
    /// when the map applies to every record.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub when: Option<ZenExpression>,
    /// The object's key: a text expression over `input` (the subject rule).
    pub subject: ZenExpression,
    /// The source record's identity in the source system (`input.name`): a
    /// text expression over `input`, recorded as provenance. Several source
    /// identities may resolve to one object; this is never a key property.
    pub source_record: ZenExpression,
    /// Property maps. Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub properties: Vec<PropertyMap>,
    /// Link maps. Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<LinkMap>,
    /// Observed lifecycle snapshots. Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lifecycles: Vec<LifecycleMap>,
    /// Target members deliberately left unmapped, with a reason. Omitted
    /// when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gaps: Vec<MappingGap>,
    /// Source fields deliberately not read, with a reason. Omitted when
    /// empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignored_fields: Vec<IgnoredField>,
}

impl ObjectMap {
    /// The transform id of the rule for `member` of this map
    /// (`sales_order.total_amount`); `subject` and `source_record` name the
    /// key and source identity rules.
    #[must_use]
    pub fn transform_id(&self, member: &str) -> String {
        format!("{}.{member}", self.id)
    }
}

/// The records an [`ObjectMap`] reads.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LogicalSource {
    /// The `SourceSchemaIR` dataset id whose records are read.
    pub dataset: String,
    /// When the records are a child table nested in a parent document (the
    /// RML iterator): the parent dataset and its field holding the rows.
    /// Omitted: the dataset's records are read directly.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "NestedSource")]
    pub nested_in: Option<NestedSource>,
}

/// A parent document field holding a child dataset's rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NestedSource {
    /// The parent dataset id.
    pub dataset: String,
    /// The parent's field holding the rows (a Frappe `Table` field).
    pub field: String,
}

/// The object type an [`ObjectMap`] produces, and the scoped interface it
/// implements.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MapTarget {
    /// Artifact id of the `entity` artifact defining the object type of the
    /// produced objects.
    pub entity: String,
    /// Artifact id of the `entity` artifact defining the interface (of the
    /// mapping's scope) the object type implements.
    pub interface: String,
}

/// One property rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PropertyMap {
    /// The target property (stored, never computed or a lifecycle property).
    pub property: PropertyId,
    /// The value: an expression over the parent `input`, or over each
    /// matching child row's `input` when `nested` is present.
    pub value: ZenExpression,
    /// The parent collection and child-row predicate for a nested pivot.
    /// Exactly one matching row supplies `value`; more than one is refused.
    /// Omitted for a direct property expression.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "NestedPropertySource")]
    pub nested: Option<NestedPropertySource>,
}

/// A source-schema-declared child collection whose unique matching row
/// supplies one parent property value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NestedPropertySource {
    /// The child dataset id.
    pub dataset: String,
    /// The parent dataset field holding rows of `dataset`.
    pub field: String,
    /// A non-null boolean expression over a child row's `input`.
    #[serde(rename = "where")]
    pub predicate: ZenExpression,
}

/// One link rule: the produced object links to the object another map
/// produces with this key (an RML referencing object map with its join).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LinkMap {
    /// The target link.
    pub link: LinkId,
    /// The id of the map producing the linked objects; its object type must
    /// be admitted by the link's target.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub target_map: Option<String>,
    /// Resolve the `key` expression through the MappingIR `identityBindings`
    /// table instead of treating it as a Norn object key.
    #[serde(default, skip_serializing_if = "is_false")]
    pub identity_binding: bool,
    /// The linked object's key: a nullable text expression over `input`
    /// (`null`: no link). Reviewed reference evidence may bind several
    /// disjoint nullable chunks for one link. They resolve once per record;
    /// multiple non-null chunks refuse rather than emitting duplicate effects.
    pub key: ZenExpression,
}

/// One lifecycle observation rule of a lifecycle property.
///
/// Exactly one of `state` and `transition` is required. `state` names the
/// observed target state directly. `transition` is the legacy form: the
/// declared transition id is resolved to its destination state before the
/// snapshot is handed to the kernel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LifecycleMap {
    /// The lifecycle property.
    pub property: PropertyId,
    /// The observed target state: a nullable expression over `input` whose
    /// non-null values are states of the property's lifecycle.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub state: Option<ZenExpression>,
    /// Legacy form naming the observed declared transition. Its destination
    /// state becomes the lifecycle snapshot. `null` means no observation.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub transition: Option<ZenExpression>,
    /// The terminal state a source delete of the record means (`cancelled`),
    /// reached by observed transitions when the lifecycle declares a unique
    /// shortest path to it; otherwise the deleted object is tombstoned
    /// (#3013). Omitted when the mapping declares none.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub on_delete: Option<String>,
}

/// One lifecycle state observed in a source record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ObservedLifecycleSnapshot {
    /// The lifecycle property.
    pub property: PropertyId,
    /// The target state the source record reports.
    pub state: String,
    /// The source observation and rule that produced this snapshot.
    pub provenance: InstanceProvenance,
}

/// Why an observed lifecycle target cannot be reached by one unique shortest
/// path from the current state (or the lifecycle's initial state).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotDeviationReason {
    /// No declared transition path reaches the observed target.
    NoDeclaredPath,
    /// More than one shortest declared transition path reaches the target.
    AmbiguousShortestPath,
}

/// Why a target member is not mapped.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum GapReason {
    /// The source has no data for it.
    NotInSource,
    /// The member takes its declared default.
    Defaulted,
    /// Mapping it is deferred to a later version.
    Deferred,
    /// Another map or a computation supplies it.
    DerivedElsewhere,
}

/// A target member deliberately left unmapped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MappingGap {
    /// The target property or link id.
    pub member: String,
    /// Why.
    pub reason: GapReason,
    /// A reviewer's note.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub note: Option<String>,
}

/// A source field an [`ObjectMap`] deliberately does not read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IgnoredField {
    /// The source field name.
    pub field: String,
    /// Why it is not read.
    pub reason: String,
}

/// One call of an action compiled from a mapping (T8-MAP-1b): the observe
/// upsert of a mapped object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActionInvocation {
    /// The compiled action's `actionId`.
    pub action_id: String,
    /// The action input, conforming to the action's input schema, as exact
    /// runtime values (#2770): decimals keep their digits.
    pub input: RuntimeObject,
}

/// A link between two mapped objects: the `norn-cell` link change
/// `add { objectType, link, source, target }` (an `EntityIR` v2 link has no
/// action effect yet).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LinkPatch {
    /// The source object's type.
    pub object_type: ObjectTypeName,
    /// The source object's key.
    pub key: String,
    /// The link id on the source type.
    pub link: LinkId,
    /// The linked object's type (the joined map's object type).
    pub target_type: ObjectTypeName,
    /// The linked object's key.
    pub target_key: String,
    /// The source observation and exact link rule that produced this patch.
    pub provenance: InstanceProvenance,
}

/// What a source delete of one record means under one [`ObjectMap`]
/// (#3013): the object it removes and, when the map declares `onDelete`
/// states, the terminal lifecycle states it reaches.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MappedDelete {
    /// The map id.
    pub map: String,
    /// The object's type.
    pub object_type: ObjectTypeName,
    /// The object's key.
    pub key: String,
    /// The `onDelete` state of each lifecycle that declares one. Omitted
    /// when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lifecycle_snapshots: Vec<ObservedLifecycleSnapshot>,
    /// Where the delete came from.
    pub provenance: InstanceProvenance,
}

/// What one source record produces under one [`ObjectMap`] (T8-MAP-1b).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MappedRecord {
    /// The map id.
    pub map: String,
    /// The produced object's type.
    pub object_type: ObjectTypeName,
    /// The produced object's key.
    pub key: String,
    /// The observe upsert.
    pub invocations: Vec<ActionInvocation>,
    /// Observed target states of lifecycle properties. The kernel reconciles
    /// them against the current state by declared transitions. Omitted when
    /// empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lifecycle_snapshots: Vec<ObservedLifecycleSnapshot>,
    /// Links to other mapped objects. Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<LinkPatch>,
    /// Nullable to-one link rules that explicitly observed no target.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cleared_links: Vec<LinkId>,
    /// The provenance of each produced property or observed lifecycle rule.
    pub property_provenance: BTreeMap<PropertyId, InstanceProvenance>,
    /// Where the record came from and its subject rule.
    pub provenance: InstanceProvenance,
}

impl MappingIr {
    /// The map with id `id`.
    #[must_use]
    pub fn map(&self, id: &str) -> Option<&ObjectMap> {
        self.maps.iter().find(|map| map.id == id)
    }
}

/// Emit the JSON Schema of a [`MappingIr`] document.
#[must_use]
pub fn mapping_ir_schema() -> Schema {
    schema_for!(MappingIr)
}

fn is_false(value: &bool) -> bool {
    !*value
}
