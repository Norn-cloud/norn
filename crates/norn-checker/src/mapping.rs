//! `MappingIR` checks (T8-MAP-1a, #2600): a mapping is well-formed on its
//! own (T2) and binds to its source schema and target model (T3).
//!
//! A mapping is the payload of a `mapping` artifact; [`check_mapping_artifacts`]
//! runs inside [`crate::check_artifact_set`] and checks every candidate
//! mapping against the set (and the installed base):
//!
//! - the provenance key `<artifactId>@<version>` is unambiguous: the id has
//!   no `@` and the version is a `SemVer` version;
//! - the bound `source_schema` artifact is in the set or the base at the
//!   bound version, with the bound hash (its Norn1 envelope hash, the one
//!   check receipts record), and checks clean;
//! - then [`check_mapping`] against that schema and the [`MappingModel`] of
//!   the set's `entity` artifacts.
//!
//! [`check_mapping_local`] is the per-document check:
//!
//! - the interface scope and the map list are non-empty;
//! - map ids are unique and in the member id grammar; every map targets an
//!   interface of the scope;
//! - within a map, a target member has at most one rule (property, link,
//!   lifecycle, or gap), except evidence-bound disjoint nullable link chunks;
//!   every link joins to a map of the mapping;
//! - every rule compiles as a ZEN Expression.
//!
//! [`check_mapping`] adds the bindings, against a
//! `SourceSchemaIR` document and a [`MappingModel`] (the `EntityIR` v2
//! definitions the targets live in):
//!
//! - every logical source is a declared dataset, a nested source is the
//!   child collection a `parent_child` constraint declares on its parent
//!   (the constraint's `collection` field), and every ignored field exists;
//! - every target names the `entity` artifact of an object type that
//!   implements the interface its scoped interface artifact defines;
//! - property rules name stored, writable, non-lifecycle properties; link
//!   rules name links whose target admits the joined map's object type;
//!   lifecycle rules name lifecycle properties; gaps name members;
//! - every rule type-checks under `norn-expr/4` in a source-only
//!   environment ([`ZenTypeEnv::input_only`]: `input` bound to the source
//!   dataset's fields, and no `actor`, `self`, clock, slice, or IO): a map
//!   guard is boolean; an unguarded subject and source record are text while
//!   guarded key rules may be nullable until the guard matches; direct
//!   property values use the parent input, nested property predicates and
//!   values use the declared child input; link keys are nullable text; and a
//!   lifecycle rule yields either a lifecycle state or a declared transition
//!   id, nullable in either form.
//!
//! The `input` record has one field per source field: a field with a
//! declared code set is that `enum`; otherwise its candidate type
//! (`decimal` reads as a number; `datetime` and `time` as text, since the
//! source may omit the offset; `json`, `binary`, and `unknown` are opaque),
//! nullable unless the source declares it required.

mod compile;
pub mod evidence;
mod set;
mod snapshot_lint;
pub mod vendor;

use std::collections::{BTreeMap, BTreeSet};

use norn_expr::{
    EnvOptions, Expression, FieldSource, RecordKind, RecordType, Type, ZenTypeEnv, check_expression,
};
use norn_ir::mapping::{
    EntitlementConditionMap, EntitlementCoverageStatus, EntitlementFieldPath,
    EntitlementGranteeKind, EntitlementJoinMode, EntitlementMap, MappingIr, NestedPropertySource,
    ObjectMap,
};
use norn_ir::source_schema::{ConstraintKind, NormalizedType, Origin, SourceField, SourceSchemaIr};
use norn_ir::{
    ArtifactEnvelope, EntityRoot, InterfaceName, MarkingId, ObjectTypeName, ResourceLimitProfile,
    ZenExpression,
};

use compile::compile_mapping;
pub use compile::{
    CompiledMapping, EntitlementCoverageReport, MappedSnapshot, OBJECT_ID_FIELD, SourceDocument,
};
pub use set::{check_mapping_artifacts, compile_mapping_artifact, mapping_key};
pub use snapshot_lint::lint_observed_deviations;

use crate::entity_artifacts::{Decoded, decode_v2};
use crate::entity_instance::EffectiveCatalog;
use crate::property_types::error;
use crate::receipt::{codes::ENVIRONMENT_UNDERIVABLE, derive_type_env};
use crate::{Diagnostic, Span};

/// Stable `mapping.validation.*` (T2) and `mapping.model.*` (T3) codes.
pub mod codes {
    /// The document does not decode as a `MappingIR` document.
    pub const INVALID_DOCUMENT: &str = "mapping.validation.invalid_document";
    /// The mapping's provenance key `<artifactId>@<version>` is ambiguous:
    /// the id contains `@`, or the version is not a `SemVer` version.
    pub const INVALID_KEY: &str = "mapping.validation.invalid_key";
    /// The interface scope or the map list is empty.
    pub const EMPTY_SCOPE: &str = "mapping.validation.empty_scope";
    /// A map id is not in the member id grammar.
    pub const INVALID_ID: &str = "mapping.validation.invalid_id";
    /// Two maps share an id.
    pub const DUPLICATE_MAP: &str = "mapping.validation.duplicate_map";
    /// A map targets an interface outside the mapping's scope.
    pub const INTERFACE_OUT_OF_SCOPE: &str = "mapping.validation.interface_out_of_scope";
    /// A target member has two rules in one map.
    pub const DUPLICATE_MEMBER: &str = "mapping.validation.duplicate_member";
    /// A link joins to a map the mapping does not declare.
    pub const UNKNOWN_TARGET_MAP: &str = "mapping.validation.unknown_target_map";
    /// A rule does not compile as a ZEN Expression.
    pub const INVALID_EXPRESSION: &str = "mapping.validation.invalid_expression";
    /// A lifecycle rule's `onDelete` names no terminal state of its
    /// lifecycle (#3013).
    pub const INVALID_ON_DELETE: &str = "mapping.model.invalid_on_delete";
    /// A lifecycle rule must specify exactly one of `state` or `transition`.
    pub const LIFECYCLE_RULE_SHAPE: &str = "mapping.validation.lifecycle_rule_shape";
    /// An observed lifecycle snapshot had no unique shortest declared path.
    pub const OBSERVED_OUT_OF_BAND: &str = "mapping.runtime.observed_out_of_band";
    /// A source or target artifact reference is absent from required dependencies.
    pub const UNDECLARED_DEPENDENCY: &str = "mapping.model.undeclared_dependency";
    /// The bound source schema is not a `source_schema` artifact of the set
    /// or the base at the bound version.
    pub const UNRESOLVED_SOURCE: &str = "mapping.model.unresolved_source";
    /// The bound source schema's hash differs from the binding.
    pub const SOURCE_HASH_MISMATCH: &str = "mapping.model.source_hash_mismatch";
    /// A scoped interface or target interface names no `entity` artifact
    /// defining an interface.
    pub const UNKNOWN_INTERFACE: &str = "mapping.model.unknown_interface";
    /// A logical source names an undeclared dataset.
    pub const UNKNOWN_DATASET: &str = "mapping.model.unknown_dataset";
    /// A nested source is not a declared child table of its parent.
    pub const INVALID_NESTING: &str = "mapping.model.invalid_nesting";
    /// An ignored field is not a field of the map's dataset.
    pub const UNKNOWN_FIELD: &str = "mapping.model.unknown_field";
    /// A target names no `entity` artifact defining an available object
    /// type.
    pub const UNKNOWN_OBJECT_TYPE: &str = "mapping.model.unknown_object_type";
    /// A target object type does not implement its scoped interface.
    pub const INTERFACE_NOT_IMPLEMENTED: &str = "mapping.model.interface_not_implemented";
    /// A rule or gap names no property or link of the target.
    pub const UNKNOWN_MEMBER: &str = "mapping.model.unknown_member";
    /// A property rule writes a computed (read-only) property.
    pub const READ_ONLY_MEMBER: &str = "mapping.model.read_only_member";
    /// A property rule sets a lifecycle property, or a lifecycle rule names
    /// a property without a lifecycle.
    pub const LIFECYCLE_MEMBER: &str = "mapping.model.lifecycle_member";
    /// A link joins to a map whose object type the link's target does not
    /// admit.
    pub const LINK_TARGET_MISMATCH: &str = "mapping.model.link_target_mismatch";
    /// A rule does not type-check against its source and target.
    pub const EXPRESSION_TYPE: &str = "mapping.model.expression_type";
    /// A mapped property's type has no action-input encoding.
    pub const UNENCODABLE_MEMBER: &str = "mapping.model.unencodable_member";
    /// A source record does not conform to its dataset's `input` type.
    pub const INVALID_RECORD: &str = "mapping.runtime.invalid_record";
    /// A rule failed to evaluate, or yielded a value of the wrong shape.
    pub const EVALUATION_FAILED: &str = "mapping.runtime.evaluation_failed";
    /// A subject or source-record rule yielded no non-empty string.
    pub const INVALID_KEY_VALUE: &str = "mapping.runtime.invalid_key";
    /// An entitlement map id is invalid or duplicated.
    pub const INVALID_ENTITLEMENT_MAP: &str = "mapping.validation.invalid_entitlement_map";
    /// An entitlement map refers to an undeclared source dataset or field.
    pub const UNKNOWN_ENTITLEMENT_FIELD: &str = "mapping.model.unknown_entitlement_field";
    /// An entitlement join is ambiguous, malformed, or not type-compatible.
    pub const INVALID_ENTITLEMENT_JOIN: &str = "mapping.model.invalid_entitlement_join";
    /// The grantee type is not the canonical type for its grantee kind.
    pub const INVALID_ENTITLEMENT_GRANTEE: &str = "mapping.model.invalid_entitlement_grantee";
    /// The condition's link does not admit its declared linked object type.
    pub const INVALID_ENTITLEMENT_CONDITION: &str = "mapping.model.invalid_entitlement_condition";
    /// A join-dependent entitlement map does not require complete snapshots.
    pub const INCOMPLETE_ENTITLEMENT_SNAPSHOT: &str =
        "mapping.validation.incomplete_entitlement_snapshot";
    /// A required relationship has no exact target key.
    pub const INVALID_REQUIRED_LINK: &str = "mapping.validation.invalid_required_link";

    /// A principal key is not backed by an explicit reviewed binding.
    pub const INVALID_IDENTITY_BINDING: &str = "mapping.validation.invalid_identity_binding";
}

/// Decode a `MappingIR` document from its wire JSON and run
/// [`check_mapping_local`].
///
/// # Errors
///
/// [`codes::INVALID_DOCUMENT`] when the JSON does not decode, or every local
/// finding.
pub fn decode_mapping(value: &serde_json::Value) -> Result<MappingIr, Vec<Diagnostic>> {
    let mapping: MappingIr = serde_json::from_value(value.clone()).map_err(|decode| {
        vec![error(
            codes::INVALID_DOCUMENT,
            format!("the document is not a MappingIR document: {decode}"),
            Span::pointer(""),
        )]
    })?;
    let findings = check_mapping_local(&mapping);
    if findings.is_empty() {
        Ok(mapping)
    } else {
        Err(findings)
    }
}

/// The per-document (T2) check of a mapping. Empty when it is valid. See
/// the module docs.
#[must_use]
#[allow(
    clippy::too_many_lines,
    reason = "local MappingIR validation reports independent structural findings in one pass"
)]
pub fn check_mapping_local(mapping: &MappingIr) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for (empty, member) in [
        (mapping.interfaces.is_empty(), "interfaces"),
        (mapping.maps.is_empty(), "maps"),
    ] {
        if empty {
            out.push(error(
                codes::EMPTY_SCOPE,
                format!("{member} must not be empty"),
                Span::from_segments([member]),
            ));
        }
    }
    let ids: BTreeSet<&str> = mapping.maps.iter().map(|map| map.id.as_str()).collect();
    let mut seen = BTreeSet::new();
    for (index, map) in mapping.maps.iter().enumerate() {
        let valid = member_id(&map.id);
        if !valid {
            out.push(error(
                codes::INVALID_ID,
                format!("map id {:?} is not a member id (`[a-z][a-z0-9_]*`)", map.id),
                map_at(index, &["id"]),
            ));
        }
        if valid && !seen.insert(map.id.as_str()) {
            out.push(error(
                codes::DUPLICATE_MAP,
                format!("map id {:?} is declared twice", map.id),
                map_at(index, &["id"]),
            ));
        }
        if !mapping.interfaces.contains(&map.target.interface) {
            out.push(error(
                codes::INTERFACE_OUT_OF_SCOPE,
                format!(
                    "map {:?} targets interface artifact {:?}, which is not in the mapping's interfaces",
                    map.id, map.target.interface
                ),
                map_at(index, &["target", "interface"]),
            ));
        }
        check_map_local(mapping, map, index, &ids, &mut out);
    }
    let mut entitlement_ids = BTreeSet::new();
    for (index, map) in mapping.entitlements.iter().enumerate() {
        if !member_id(&map.id) || !entitlement_ids.insert(map.id.as_str()) {
            out.push(error(
                codes::INVALID_ENTITLEMENT_MAP,
                format!("entitlement map id {:?} is invalid or duplicated", map.id),
                entitlement_at(index, &[]),
            ));
        }
        if !member_id(&map.source_alias) {
            out.push(error(
                codes::INVALID_ENTITLEMENT_MAP,
                format!("sourceAlias {:?} is not a member id", map.source_alias),
                entitlement_at(index, &["sourceAlias"]),
            ));
        }
        match (map.coverage.status, map.actions.is_empty()) {
            (EntitlementCoverageStatus::Missing, false) => out.push(error(
                codes::INVALID_ENTITLEMENT_MAP,
                "a Missing coverage entry is declarative only and cannot declare granting actions"
                    .to_owned(),
                entitlement_at(index, &["actions"]),
            )),
            (EntitlementCoverageStatus::Exact | EntitlementCoverageStatus::Narrowed, true) => {
                out.push(error(
                    codes::INVALID_ENTITLEMENT_MAP,
                    "an emitting entitlement map must declare at least one granting action"
                        .to_owned(),
                    entitlement_at(index, &["actions"]),
                ));
            }
            _ => {}
        }
        let mut aliases = BTreeSet::from([map.source_alias.as_str()]);
        for (join_index, join) in map.joins.iter().enumerate() {
            if !member_id(&join.alias)
                || (join.mode == EntitlementJoinMode::Inner && !aliases.insert(join.alias.as_str()))
                || join.on.is_empty()
                || (join.mode == EntitlementJoinMode::Inner
                    && (join.record_id.is_none() || join.version.is_none()))
                || (join.mode == EntitlementJoinMode::Anti
                    && (join.record_id.is_some() || join.version.is_some()))
            {
                out.push(error(
                    codes::INVALID_ENTITLEMENT_JOIN,
                    format!(
                        "join {:?} has an invalid alias, key, or provenance shape",
                        join.alias
                    ),
                    entitlement_at(index, &["joins", &join_index.to_string()]),
                ));
            }
        }
        let required: BTreeSet<&str> = map.required_snapshot.iter().map(String::as_str).collect();
        if !required.contains(map.source.dataset.as_str())
            || map
                .joins
                .iter()
                .any(|join| !required.contains(join.dataset.as_str()))
        {
            out.push(error(
                codes::INCOMPLETE_ENTITLEMENT_SNAPSHOT,
                "every entitlement source and join dataset must be in requiredSnapshot".to_owned(),
                entitlement_at(index, &["requiredSnapshot"]),
            ));
        }
        for (expression, location) in entitlement_expressions(map, index) {
            if let Err(compile) = Expression::compile(expression.as_str()) {
                out.push(error(
                    codes::INVALID_EXPRESSION,
                    format!("{:?} does not compile: {compile}", expression.as_str()),
                    location,
                ));
            }
        }
    }
    out
}

fn check_map_local(
    mapping: &MappingIr,
    map: &ObjectMap,
    index: usize,
    ids: &BTreeSet<&str>,
    out: &mut Vec<Diagnostic>,
) {
    let claims = map
        .properties
        .iter()
        .enumerate()
        .map(|(rule, entry)| (entry.property.as_str(), "properties", rule, "property"))
        .chain(
            map.links
                .iter()
                .enumerate()
                .map(|(rule, entry)| (entry.link.as_str(), "links", rule, "link")),
        )
        .chain(
            map.lifecycles
                .iter()
                .enumerate()
                .map(|(rule, entry)| (entry.property.as_str(), "lifecycles", rule, "property")),
        )
        .chain(
            map.gaps
                .iter()
                .enumerate()
                .map(|(rule, entry)| (entry.member.as_str(), "gaps", rule, "member")),
        );
    let reviewed_links: BTreeSet<_> =
        evidence::read_evidence(mapping)
            .ok()
            .flatten()
            .map_or_else(BTreeSet::new, |evidence| {
                evidence
                    .reviewed_references
                    .into_iter()
                    .filter(|reference| reference.dataset == map.source.dataset)
                    .map(|reference| reference.link)
                    .collect()
            });
    let mut members = BTreeMap::new();
    for (member, list, rule, field) in claims {
        if let Some(previous) = members.insert(member, list)
            && !(list == "links" && previous == "links" && reviewed_links.contains(member))
        {
            out.push(error(
                codes::DUPLICATE_MEMBER,
                format!("map {:?} has two rules for member {member:?}", map.id),
                rule_at(index, list, rule, field),
            ));
        }
    }
    for (rule, link) in map.links.iter().enumerate() {
        if link.target_map.is_some() == link.identity_binding
            || link
                .target_map
                .as_deref()
                .is_some_and(|target_map| !ids.contains(target_map))
        {
            out.push(error(
                codes::UNKNOWN_TARGET_MAP,
                format!(
                    "link {} must name exactly one declared target map or identity binding",
                    link.link.as_str(),
                ),
                rule_at(index, "links", rule, "targetMap"),
            ));
        }
    }
    for (rule, lifecycle) in map.lifecycles.iter().enumerate() {
        if lifecycle.state.is_some() == lifecycle.transition.is_some() {
            out.push(error(
                codes::LIFECYCLE_RULE_SHAPE,
                format!(
                    "map {:?} lifecycle {} must specify exactly one of state or transition",
                    map.id,
                    lifecycle.property.as_str()
                ),
                rule_at(index, "lifecycles", rule, "state"),
            ));
        }
    }
    for (expression, at) in expressions(map, index) {
        if let Err(compile) = Expression::compile(expression.as_str()) {
            out.push(error(
                codes::INVALID_EXPRESSION,
                format!("{:?} does not compile: {compile}", expression.as_str()),
                at,
            ));
        }
    }
}

/// Every expression of a map, with its location.
fn expressions(map: &ObjectMap, index: usize) -> Vec<(&ZenExpression, Span)> {
    let mut out = Vec::new();
    if let Some(when) = &map.when {
        out.push((when, map_at(index, &["when"])));
    }
    out.push((&map.subject, map_at(index, &["subject"])));
    out.push((&map.source_record, map_at(index, &["sourceRecord"])));
    out.extend(
        map.properties
            .iter()
            .enumerate()
            .map(|(rule, entry)| (&entry.value, rule_at(index, "properties", rule, "value"))),
    );
    out.extend(
        map.properties
            .iter()
            .enumerate()
            .filter_map(|(rule, entry)| {
                entry.nested.as_ref().map(|nested| {
                    (
                        &nested.predicate,
                        property_nested_at(index, rule, &["nested", "where"]),
                    )
                })
            }),
    );
    out.extend(
        map.links
            .iter()
            .enumerate()
            .map(|(rule, entry)| (&entry.key, rule_at(index, "links", rule, "key"))),
    );
    for (rule, entry) in map.lifecycles.iter().enumerate() {
        if let Some(state) = &entry.state {
            out.push((state, rule_at(index, "lifecycles", rule, "state")));
        }
        if let Some(transition) = &entry.transition {
            out.push((transition, rule_at(index, "lifecycles", rule, "transition")));
        }
    }
    out
}

/// The `EntityIR` v2 definitions a mapping's targets are checked against:
/// the ZEN Expression type environment and, per object type, its lineage,
/// implemented interfaces, and lifecycle transitions.
#[derive(Debug, Clone)]
pub struct MappingModel {
    env: ZenTypeEnv,
    types: BTreeMap<String, ModelType>,
    /// Object type names by the artifact id of the `entity` artifact
    /// defining them.
    object_types: BTreeMap<String, ObjectTypeName>,
    /// Interface names by the artifact id of the `entity` artifact defining
    /// them.
    interfaces: BTreeMap<String, InterfaceName>,
}

/// What a mapping needs to know about one object type beyond its record.
#[derive(Debug, Clone, Default)]
struct ModelType {
    /// The defining `entity` artifact's id and version.
    artifact: (String, String),
    lineage: Vec<ObjectTypeName>,
    interfaces: Vec<InterfaceName>,
    /// Effective markings by property, including type, interface, ref, and
    /// computed-input propagation.
    markings: BTreeMap<String, Vec<MarkingId>>,
    /// The states and declared transitions by lifecycle property.
    lifecycles: BTreeMap<String, ModelLifecycle>,
}

#[derive(Debug, Clone, Default)]
struct ModelLifecycle {
    states: Vec<String>,
    terminal: Vec<String>,
    transitions: Vec<String>,
    transition_destinations: BTreeMap<String, String>,
}

impl MappingModel {
    /// The model of a set of `entity` artifacts (base layer included).
    /// Object types unavailable in the effective catalog (because of a
    /// finding on an artifact they depend on) implement nothing, so a
    /// mapping that targets one fails closed.
    ///
    /// # Errors
    ///
    /// The type environment's derivation errors, as
    /// `artifact_set.expression.environment_underivable` findings.
    pub fn from_artifacts(artifacts: &[ArtifactEnvelope]) -> Result<Self, Vec<Diagnostic>> {
        let env = derive_type_env(artifacts, &[], &ResourceLimitProfile::norn_limits_v1())
            .map_err(|errors| {
                errors
                    .into_iter()
                    .map(|reason| {
                        error(
                            ENVIRONMENT_UNDERIVABLE,
                            format!(
                                "the mapping model's type environment cannot be derived: {reason}"
                            ),
                            Span::pointer(""),
                        )
                    })
                    .collect::<Vec<_>>()
            })?;
        let catalog = EffectiveCatalog::from_artifacts(artifacts);
        let mut types = BTreeMap::new();
        for artifact in artifacts {
            if let Some(Ok(object_type)) = catalog.object_type(&artifact.artifact_id) {
                let lifecycles = object_type
                    .lifecycles
                    .iter()
                    .map(|effective| {
                        let lifecycle = &effective.lifecycle;
                        let transitions: Vec<String> = lifecycle
                            .transitions
                            .iter()
                            .map(|transition| transition.id.as_str().to_owned())
                            .collect();
                        let transition_destinations = lifecycle
                            .transitions
                            .iter()
                            .map(|transition| {
                                (transition.id.as_str().to_owned(), transition.to.clone())
                            })
                            .collect();
                        (
                            lifecycle.property.as_str().to_owned(),
                            ModelLifecycle {
                                states: effective.states.clone(),
                                terminal: lifecycle.terminal.clone(),
                                transitions,
                                transition_destinations,
                            },
                        )
                    })
                    .collect();
                types.insert(
                    object_type.name.as_str().to_owned(),
                    ModelType {
                        artifact: (artifact.artifact_id.clone(), artifact.version.clone()),
                        lineage: object_type.lineage.clone(),
                        interfaces: object_type.interfaces.clone(),
                        markings: object_type
                            .properties
                            .iter()
                            .map(|property| {
                                (property.id.as_str().to_owned(), property.markings.clone())
                            })
                            .collect(),
                        lifecycles,
                    },
                );
            }
        }
        let mut object_types = BTreeMap::new();
        let mut interfaces = BTreeMap::new();
        for artifact in artifacts {
            if let Decoded::Root(root) = decode_v2(artifact) {
                match *root {
                    EntityRoot::V2(root) => {
                        object_types.insert(artifact.artifact_id.clone(), root.object_type.name);
                    }
                    EntityRoot::V2Interface(root) => {
                        interfaces.insert(artifact.artifact_id.clone(), root.interface.name);
                    }
                    EntityRoot::V2Extension(_)
                    | EntityRoot::V2PropertyTypes(_)
                    | EntityRoot::V2Markings(_)
                    | EntityRoot::V1(_) => {}
                }
            }
        }
        Ok(Self {
            env,
            types,
            object_types,
            interfaces,
        })
    }

    /// The object type the `entity` artifact `artifact_id` defines.
    #[must_use]
    pub fn object_type_of(&self, artifact_id: &str) -> Option<&ObjectTypeName> {
        self.object_types.get(artifact_id)
    }

    /// The interface the `entity` artifact `artifact_id` defines.
    #[must_use]
    pub fn interface_of(&self, artifact_id: &str) -> Option<&InterfaceName> {
        self.interfaces.get(artifact_id)
    }

    /// The id and version of the `entity` artifact defining the available
    /// object type `object_type`.
    #[must_use]
    pub fn entity_artifact(&self, object_type: &str) -> Option<(&str, &str)> {
        self.types
            .get(object_type)
            .map(|model| (model.artifact.0.as_str(), model.artifact.1.as_str()))
    }

    /// Effective markings of a property on an available object type.
    #[must_use]
    pub fn property_markings(&self, object_type: &str, property: &str) -> Option<&[MarkingId]> {
        self.types
            .get(object_type)
            .and_then(|model| model.markings.get(property))
            .map(Vec::as_slice)
    }

    /// The model's ZEN Expression type environment (no `input` bound).
    #[must_use]
    pub const fn env(&self) -> &ZenTypeEnv {
        &self.env
    }

    /// The transition ids of the lifecycle of `property` on `object_type`,
    /// when that property has one.
    #[must_use]
    pub fn transitions(&self, object_type: &str, property: &str) -> Option<&[String]> {
        self.types
            .get(object_type)
            .and_then(|model| model.lifecycles.get(property))
            .map(|lifecycle| lifecycle.transitions.as_slice())
    }

    /// The terminal states of the lifecycle of `property` on
    /// `object_type`, when it has one.
    pub(crate) fn lifecycle_terminal(
        &self,
        object_type: &str,
        property: &str,
    ) -> Option<&[String]> {
        self.types
            .get(object_type)
            .and_then(|model| model.lifecycles.get(property))
            .map(|lifecycle| lifecycle.terminal.as_slice())
    }

    /// The effective enum states of the lifecycle of `property` on
    /// `object_type`, when that property has one.
    #[must_use]
    pub(crate) fn lifecycle_states(&self, object_type: &str, property: &str) -> Option<&[String]> {
        self.types
            .get(object_type)
            .and_then(|model| model.lifecycles.get(property))
            .map(|lifecycle| lifecycle.states.as_slice())
    }

    /// The destination state of the declared transition, when it exists.
    #[must_use]
    pub(crate) fn transition_destination(
        &self,
        object_type: &str,
        property: &str,
        transition: &str,
    ) -> Option<&str> {
        self.types
            .get(object_type)
            .and_then(|model| model.lifecycles.get(property))
            .and_then(|lifecycle| lifecycle.transition_destinations.get(transition))
            .map(String::as_str)
    }

    /// Whether an object of type `produced` may be the target of a link
    /// whose declared target is `target`: the same type, a subtype of it, or
    /// a type implementing it (an interface target).
    #[must_use]
    pub fn admits(&self, target: &str, produced: &str) -> bool {
        produced == target
            || self.types.get(produced).is_some_and(|model| {
                model.lineage.iter().any(|name| name.as_str() == target)
                    || model.interfaces.iter().any(|name| name.as_str() == target)
            })
    }

    fn implements(&self, object_type: &str, interface: &InterfaceName) -> bool {
        self.types
            .get(object_type)
            .is_some_and(|model| model.interfaces.contains(interface))
    }
}

/// The full check of a mapping against the `SourceSchemaIR` document it
/// reads and the model its targets live in: [`check_mapping_local`] plus
/// the bindings (T3). Empty when it is valid. See the module docs.
#[must_use]
pub fn check_mapping(
    mapping: &MappingIr,
    source: &SourceSchemaIr,
    model: &MappingModel,
) -> Vec<Diagnostic> {
    let mut out = check_mapping_local(mapping);
    out.extend(vendor::check_vendor_mapping(mapping, source));
    let reference_check = evidence::read_evidence(mapping).and_then(|evidence| {
        evidence.map_or(Ok(()), |evidence| {
            evidence::validate_reviewed_reference_maps(mapping, &evidence, source)
        })
    });
    if let Err(reason) = reference_check {
        out.push(error(
            codes::INVALID_DOCUMENT,
            reason,
            Span::pointer("/extensions"),
        ));
    }
    crate::source_schema::check_source_identity(source, &mut out);
    for (index, interface) in mapping.interfaces.iter().enumerate() {
        if model.interface_of(interface).is_none() {
            out.push(error(
                codes::UNKNOWN_INTERFACE,
                format!("{interface:?} is not an entity artifact defining an interface"),
                Span::from_segments(["interfaces", index.to_string().as_str()]),
            ));
        }
    }
    for (index, map) in mapping.maps.iter().enumerate() {
        check_map(mapping, map, index, source, model, &mut out);
    }
    let mut binding_keys = BTreeSet::new();
    for (index, binding) in mapping.identity_bindings.iter().enumerate() {
        if !exact_entitlement_value(&binding.source_key)
            || !exact_entitlement_value(&binding.key)
            || binding.key.len() > norn_ir::identifier_limits::MAX_OBJECT_KEY_BYTES
            || binding.evidence.trim().is_empty()
            || binding.object_type.as_str() != "party.Person"
            || !binding_keys.insert(binding.source_key.as_str())
        {
            out.push(error(
                codes::INVALID_IDENTITY_BINDING,
                "identity binding needs a unique exact source key, a party.Person reference, and review evidence"
                    .to_owned(),
                Span::from_segments(["identityBindings", index.to_string().as_str()]),
            ));
        }
    }
    for (index, map) in mapping.entitlements.iter().enumerate() {
        check_entitlement_map(
            map,
            index,
            source,
            model,
            &mapping.identity_bindings,
            &mut out,
        );
    }
    out
}

#[allow(
    clippy::too_many_lines,
    reason = "entitlement validation accumulates related cross-field findings in one pass"
)]
fn check_entitlement_map(
    map: &EntitlementMap,
    index: usize,
    source: &SourceSchemaIr,
    model: &MappingModel,
    identity_bindings: &[norn_ir::mapping::IdentityBinding],
    out: &mut Vec<Diagnostic>,
) {
    if map.coverage.status != EntitlementCoverageStatus::Missing
        && (!exact_entitlement_value(&source.source.system)
            || source.source.system.len() > compile::MAX_ENTITLEMENT_EVIDENCE_VALUE_BYTES)
    {
        out.push(error(
            codes::INVALID_ENTITLEMENT_MAP,
            "an emitting entitlement map requires an exact source system of at most 1024 bytes"
                .to_owned(),
            entitlement_at(index, &["source"]),
        ));
    }
    for dataset in &map.required_snapshot {
        if input_fields(source, dataset).is_none() {
            out.push(error(
                codes::UNKNOWN_DATASET,
                format!(
                    "required snapshot dataset {dataset:?} is not declared by the source schema"
                ),
                entitlement_at(index, &["requiredSnapshot"]),
            ));
        }
    }
    if map.source.nested_in.is_some() {
        out.push(error(
            codes::INVALID_ENTITLEMENT_MAP,
            "nested entitlement sources are unsupported; complete snapshots must supply direct source dataset rows"
                .to_owned(),
            entitlement_at(index, &["source", "nestedIn"]),
        ));
    }
    let text_type = Type::text();
    let mut datasets = BTreeMap::new();
    datasets.insert(map.source_alias.as_str(), map.source.dataset.as_str());
    let mut seen_aliases = BTreeSet::from([map.source_alias.as_str()]);
    let mut fields = BTreeMap::new();
    let add_fields = |alias: &str,
                      dataset: &str,
                      fields: &mut BTreeMap<String, Type>,
                      out: &mut Vec<Diagnostic>,
                      at: Span| {
        let Some(source_fields) = input_fields(source, dataset) else {
            out.push(error(
                codes::UNKNOWN_DATASET,
                format!("dataset {dataset:?} is not declared by the source schema"),
                at,
            ));
            return;
        };
        for (field, ty) in source_fields {
            let flattened = format!("{alias}_{field}");
            if fields.insert(flattened.clone(), ty).is_some() {
                out.push(error(
                    codes::INVALID_ENTITLEMENT_MAP,
                    format!("flattened alias field {flattened:?} is ambiguous"),
                    at.clone(),
                ));
            }
        }
    };
    add_fields(
        &map.source_alias,
        &map.source.dataset,
        &mut fields,
        out,
        entitlement_at(index, &["source"]),
    );
    for (join_index, join) in map.joins.iter().enumerate() {
        let join_at = entitlement_at(index, &["joins", &join_index.to_string()]);
        if !seen_aliases.insert(join.alias.as_str()) {
            out.push(error(
                codes::INVALID_ENTITLEMENT_JOIN,
                format!("join alias {:?} is duplicated", join.alias),
                join_at.clone(),
            ));
        }
        let Some(right_fields) = input_fields(source, &join.dataset) else {
            out.push(error(
                codes::UNKNOWN_DATASET,
                format!(
                    "dataset {:?} is not declared by the source schema",
                    join.dataset
                ),
                join_at.clone(),
            ));
            continue;
        };
        for (key_index, key) in join.on.iter().enumerate() {
            let key_at = entitlement_at(
                index,
                &[
                    "joins",
                    &join_index.to_string(),
                    "on",
                    &key_index.to_string(),
                ],
            );
            let left = path_type(&key.left, &datasets, source);
            let right = right_fields.get(&key.right);
            if left.is_none() || right.is_none() || !text_compatible(left.as_ref(), right) {
                out.push(error(
                    codes::INVALID_ENTITLEMENT_JOIN,
                    format!(
                        "join key {}.{} must be a declared nullable or required text field, and the right side {:?} must also be text",
                        key.left.alias, key.left.field, key.right
                    ),
                    key_at,
                ));
            }
        }
        if join.mode == EntitlementJoinMode::Inner {
            add_fields(&join.alias, &join.dataset, &mut fields, out, join_at);
            datasets.insert(join.alias.as_str(), join.dataset.as_str());
        }
        let provenance_is_text = |field: Option<&str>| {
            field.is_some_and(|field| {
                right_fields
                    .get(field)
                    .is_some_and(|ty| text_compatible(Some(ty), Some(&text_type)))
            })
        };
        match join.mode {
            EntitlementJoinMode::Inner
                if !provenance_is_text(join.record_id.as_deref())
                    || !provenance_is_text(join.version.as_deref()) =>
            {
                out.push(error(
                    codes::UNKNOWN_ENTITLEMENT_FIELD,
                    "inner join provenance fields must be declared text fields".to_owned(),
                    entitlement_at(index, &["joins", &join_index.to_string()]),
                ));
            }
            EntitlementJoinMode::Inner | EntitlementJoinMode::Anti => {}
        }
    }
    let Some(source_fields) = input_fields(source, &map.source.dataset) else {
        out.push(error(
            codes::UNKNOWN_DATASET,
            format!(
                "dataset {:?} is not declared by the source schema",
                map.source.dataset
            ),
            entitlement_at(index, &["source", "dataset"]),
        ));
        return;
    };
    for (field, label) in [
        (map.source_record.as_str(), "sourceRecord"),
        (map.source_version.as_str(), "sourceVersion"),
    ] {
        if source_fields
            .get(field)
            .is_none_or(|ty| !text_compatible(Some(ty), Some(&text_type)))
        {
            out.push(error(
                codes::UNKNOWN_ENTITLEMENT_FIELD,
                format!(
                    "primary dataset {:?} has no text field {field:?}",
                    map.source.dataset,
                ),
                entitlement_at(index, &[label]),
            ));
        }
    }
    for path in std::iter::once(&map.grantee.key).chain(match &map.condition {
        EntitlementConditionMap::LinkedRecord { target_key, .. } => Some(target_key),
        EntitlementConditionMap::None | EntitlementConditionMap::OwnerOnly { .. } => None,
    }) {
        let field_type = path_type(path, &datasets, source);
        if field_type
            .as_ref()
            .is_none_or(|ty| !text_compatible(Some(ty), Some(&text_type)))
        {
            out.push(error(
                codes::UNKNOWN_ENTITLEMENT_FIELD,
                format!(
                    "field path {}.{} does not name a bound text field",
                    path.alias, path.field
                ),
                entitlement_at(index, &["grantee", "key"]),
            ));
        }
    }
    let target_name = map.target_type.as_str();
    let Some(target_record) = model.env.record(target_name) else {
        out.push(error(
            codes::UNKNOWN_OBJECT_TYPE,
            format!("target type {target_name:?} is not in the checked entity model"),
            entitlement_at(index, &["targetType"]),
        ));
        return;
    };
    if model.entity_artifact(target_name).is_none() {
        out.push(error(
            codes::UNKNOWN_OBJECT_TYPE,
            format!("target type {target_name:?} has no entity artifact"),
            entitlement_at(index, &["targetType"]),
        ));
    }
    let required_grantee_type = match map.grantee.kind {
        EntitlementGranteeKind::Role => "identity.Role",
        EntitlementGranteeKind::Group => "identity.Group",
        EntitlementGranteeKind::Principal => "party.Person",
    };
    if map.grantee.object_type.as_str() != required_grantee_type
        || model
            .entity_artifact(map.grantee.object_type.as_str())
            .is_none()
    {
        out.push(error(
            codes::INVALID_ENTITLEMENT_GRANTEE,
            format!(
                "{:?} grantees must reference {required_grantee_type}",
                map.grantee.kind
            ),
            entitlement_at(index, &["grantee", "objectType"]),
        ));
    }
    if map.grantee.kind == EntitlementGranteeKind::Principal
        && identity_bindings
            .iter()
            .any(|binding| binding.object_type.as_str() != required_grantee_type)
    {
        out.push(error(
            codes::INVALID_IDENTITY_BINDING,
            "principal bindings must resolve to party.Person".to_owned(),
            entitlement_at(index, &["grantee"]),
        ));
    }
    check_entitlement_link(&map.condition, target_record, model, index, out);
    let env = match ZenTypeEnv::input_only(fields, EnvOptions::default()) {
        Ok(env) => env,
        Err(reason) => {
            out.push(error(
                ENVIRONMENT_UNDERIVABLE,
                format!(
                    "entitlement map {:?} cannot bind its joined input: {reason}",
                    map.id
                ),
                entitlement_at(index, &["source"]),
            ));
            return;
        }
    };
    let boolean = Type::Bool;
    for (expr_index, expression) in map.eligibility.iter().enumerate() {
        typed(
            expression,
            &boolean,
            &env,
            &entitlement_at(index, &["eligibility", &expr_index.to_string()]),
            out,
        );
    }
    for (action_index, action) in map.actions.iter().enumerate() {
        if !valid_action_id(&action.action) {
            out.push(error(
                codes::INVALID_ENTITLEMENT_MAP,
                format!(
                    "action id {:?} is not an exact dotted action identifier",
                    action.action
                ),
                entitlement_at(index, &["actions", &action_index.to_string(), "action"]),
            ));
        }
        typed(
            &action.when,
            &boolean,
            &env,
            &entitlement_at(index, &["actions", &action_index.to_string(), "when"]),
            out,
        );
    }
    for (attribute, expression) in &map.scope_attributes {
        if !member_id(attribute) {
            out.push(error(
                codes::INVALID_ENTITLEMENT_MAP,
                format!("scope attribute {attribute:?} is not a member id"),
                entitlement_at(index, &["scopeAttributes", attribute]),
            ));
        }
        if !target_record.fields.get(attribute).is_some_and(|field| {
            matches!(field.source, FieldSource::Property { .. })
                && projected_string_compatible(&field.ty)
        }) {
            out.push(error(
                codes::INVALID_ENTITLEMENT_MAP,
                format!(
                    "scope attribute {attribute:?} must name a projected text property of {target_name:?}"
                ),
                entitlement_at(index, &["scopeAttributes", attribute]),
            ));
        }
        typed(
            expression,
            &Type::text(),
            &env,
            &entitlement_at(index, &["scopeAttributes", attribute]),
            out,
        );
    }
}

fn check_entitlement_link(
    condition: &EntitlementConditionMap,
    target_record: &RecordType,
    model: &MappingModel,
    index: usize,
    out: &mut Vec<Diagnostic>,
) {
    let (link, expected_target) = match condition {
        EntitlementConditionMap::None => return,
        EntitlementConditionMap::OwnerOnly { link } => (link, "party.Person"),
        EntitlementConditionMap::LinkedRecord {
            link, target_type, ..
        } => (link, target_type.as_str()),
    };
    let actual_target = target_record
        .fields
        .get(link.as_str())
        .filter(|field| matches!(field.source, FieldSource::Link { .. }))
        .and_then(|field| record_name(&field.ty));
    if actual_target.is_none_or(|actual| !model.admits(actual, expected_target)) {
        out.push(error(
            codes::INVALID_ENTITLEMENT_CONDITION,
            format!(
                "condition link {} must admit the required target type {expected_target:?}",
                link.as_str()
            ),
            entitlement_at(index, &["condition", "link"]),
        ));
    }
}

fn path_type(
    path: &EntitlementFieldPath,
    aliases: &BTreeMap<&str, &str>,
    source: &SourceSchemaIr,
) -> Option<Type> {
    let dataset = aliases.get(path.alias.as_str())?;
    input_fields(source, dataset)?.remove(&path.field)
}

fn text_compatible(left: Option<&Type>, right: Option<&Type>) -> bool {
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "The foreign non-exhaustive enum requires a catch-all; unknown variants retain the existing fallback"
    )]
    fn text(ty: &Type) -> bool {
        match ty {
            Type::Optional(inner) => text(inner),
            Type::Text { .. } => true,
            _ => false,
        }
    }
    left.is_some_and(text) && right.is_some_and(text)
}

fn exact_entitlement_value(value: &str) -> bool {
    if crate::source_schema::source_identity_has_control_chars(value) {
        return false;
    }
    let value = value.trim();
    !value.is_empty() && value != "*"
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "The foreign non-exhaustive enum requires a catch-all; unknown variants retain the existing fallback"
)]
fn projected_string_compatible(ty: &Type) -> bool {
    match ty {
        Type::Optional(inner) => projected_string_compatible(inner),
        Type::Text { .. }
        | Type::Timestamp { .. }
        | Type::Date
        | Type::Duration
        | Type::Enum { .. } => true,
        _ => false,
    }
}

fn entitlement_expressions(map: &EntitlementMap, index: usize) -> Vec<(&ZenExpression, Span)> {
    map.eligibility
        .iter()
        .enumerate()
        .map(|(rule, expression)| {
            (
                expression,
                entitlement_at(index, &["eligibility", &rule.to_string()]),
            )
        })
        .chain(map.actions.iter().enumerate().map(|(rule, action)| {
            (
                &action.when,
                entitlement_at(index, &["actions", &rule.to_string(), "when"]),
            )
        }))
        .chain(map.scope_attributes.iter().map(|(attribute, expression)| {
            (
                expression,
                entitlement_at(index, &["scopeAttributes", attribute]),
            )
        }))
        .collect()
}

fn valid_action_id(id: &str) -> bool {
    !id.is_empty() && id.split('.').all(member_id)
}

fn entitlement_at(index: usize, rest: &[&str]) -> Span {
    let index = index.to_string();
    Span::from_segments(
        ["entitlements", index.as_str()]
            .into_iter()
            .chain(rest.iter().copied()),
    )
}

/// The `input` record type of `dataset`: one field per source field (see
/// the module docs). `None` when the dataset is not declared.
#[must_use]
pub fn input_fields(source: &SourceSchemaIr, dataset: &str) -> Option<BTreeMap<String, Type>> {
    source.dataset(dataset)?;
    Some(
        source
            .fields_of(dataset)
            .map(|field| (field.name.clone(), input_type(source, field)))
            .collect(),
    )
}

fn input_type(source: &SourceSchemaIr, field: &SourceField) -> Type {
    let declared = source.code_sets.iter().find(|set| {
        set.origin == Origin::Declared && set.dataset == field.dataset && set.field == field.name
    });
    let base = match declared {
        Some(set) => Type::enumeration(set.values.iter().map(|value| value.value.clone())),
        None => match field.type_candidate.normalized {
            NormalizedType::String
            | NormalizedType::Text
            | NormalizedType::Reference
            | NormalizedType::Datetime
            | NormalizedType::Time => Type::text(),
            NormalizedType::Integer => Type::Integer,
            NormalizedType::Decimal => Type::Number,
            NormalizedType::Boolean => Type::Bool,
            NormalizedType::Date => Type::Date,
            NormalizedType::Duration => Type::Duration,
            NormalizedType::Json | NormalizedType::Binary | NormalizedType::Unknown => Type::Json,
        },
    };
    if field.required == Some(true) {
        base
    } else {
        base.optional()
    }
}

fn check_map(
    mapping: &MappingIr,
    map: &ObjectMap,
    index: usize,
    source: &SourceSchemaIr,
    model: &MappingModel,
    out: &mut Vec<Diagnostic>,
) {
    let Some(fields) = check_source(map, index, source, out) else {
        return;
    };
    let Some((object_type, record)) = target_record(map, index, model, out) else {
        return;
    };
    let object_type = object_type.as_str();
    let env = match ZenTypeEnv::input_only(fields, EnvOptions::default()) {
        Ok(env) => env,
        Err(reason) => {
            out.push(error(
                ENVIRONMENT_UNDERIVABLE,
                format!("map {:?} cannot bind its input: {reason}", map.id),
                map_at(index, &["source"]),
            ));
            return;
        }
    };
    let text = Type::text();
    // A guarded map evaluates its key rules only after the predicate matches.
    // This permits a nullable key field on skipped records while preserving
    // the runtime requirement that every matched key is non-empty text.
    let key_type = if map.when.is_some() {
        text.optional()
    } else {
        text
    };
    if let Some(when) = &map.when {
        typed(when, &Type::Bool, &env, &map_at(index, &["when"]), out);
    }
    typed(
        &map.subject,
        &key_type,
        &env,
        &map_at(index, &["subject"]),
        out,
    );
    typed(
        &map.source_record,
        &key_type,
        &env,
        &map_at(index, &["sourceRecord"]),
        out,
    );
    let property_context = PropertyCheckContext {
        index,
        parent_dataset: map.source.dataset.as_str(),
        source,
        model,
        object_type,
        record,
        env: &env,
    };
    for (rule, entry) in map.properties.iter().enumerate() {
        check_property_map(entry, rule, &property_context, out);
    }
    let target = Target {
        object_type,
        record,
    };
    check_links(mapping, map, index, &target, &env, model, out);
    check_lifecycles_and_gaps(map, index, &target, &env, model, out);
}

struct PropertyCheckContext<'a> {
    index: usize,
    parent_dataset: &'a str,
    source: &'a SourceSchemaIr,
    model: &'a MappingModel,
    object_type: &'a str,
    record: &'a RecordType,
    env: &'a ZenTypeEnv,
}

fn check_property_map(
    entry: &norn_ir::mapping::PropertyMap,
    rule: usize,
    context: &PropertyCheckContext<'_>,
    out: &mut Vec<Diagnostic>,
) {
    let property = entry.property.as_str();
    let at = rule_at(context.index, "properties", rule, "property");
    match context
        .record
        .fields
        .get(property)
        .map(|field| (&field.source, field))
    {
        Some((FieldSource::Property { read_only: false }, field)) => {
            if context
                .model
                .transitions(context.object_type, property)
                .is_some()
            {
                out.push(error(
                    codes::LIFECYCLE_MEMBER,
                    format!("{property} of {} has a lifecycle; map it with a lifecycle rule, never a set", context.object_type),
                    at,
                ));
                return;
            }
            if let Some(nested) = &entry.nested {
                if let Some(child_env) = nested_property_env(
                    nested,
                    context.parent_dataset,
                    context.index,
                    rule,
                    context.source,
                    out,
                ) {
                    typed(
                        &nested.predicate,
                        &Type::Bool,
                        &child_env,
                        &property_nested_at(context.index, rule, &["nested", "where"]),
                        out,
                    );
                    typed(
                        &entry.value,
                        &field.ty,
                        &child_env,
                        &rule_at(context.index, "properties", rule, "value"),
                        out,
                    );
                }
            } else {
                let value_at = rule_at(context.index, "properties", rule, "value");
                typed(&entry.value, &field.ty, context.env, &value_at, out);
            }
        }
        Some((FieldSource::Property { read_only: true }, _)) => out.push(error(
            codes::READ_ONLY_MEMBER,
            format!(
                "{property} of {} is computed and read-only",
                context.object_type
            ),
            at,
        )),
        _ => out.push(unknown_member(
            property,
            context.object_type,
            "property",
            at,
        )),
    }
}

fn nested_property_env(
    nested: &NestedPropertySource,
    parent_dataset: &str,
    index: usize,
    rule: usize,
    source: &SourceSchemaIr,
    out: &mut Vec<Diagnostic>,
) -> Option<ZenTypeEnv> {
    let relation = source.constraints.iter().any(|constraint| {
        constraint.kind == ConstraintKind::ParentChild
            && constraint.dataset == nested.dataset
            && constraint.collection.as_deref() == Some(nested.field.as_str())
            && constraint
                .target
                .as_ref()
                .is_some_and(|target| target.dataset == parent_dataset)
    });
    if !relation {
        out.push(error(
            codes::INVALID_NESTING,
            format!(
                "dataset {:?} is not the child collection {:?} a parent_child constraint declares on {:?}",
                nested.dataset, nested.field, parent_dataset
            ),
            property_nested_at(index, rule, &["nested"]),
        ));
        return None;
    }
    if source.dataset(parent_dataset).is_none() {
        out.push(error(
            codes::UNKNOWN_DATASET,
            format!("parent dataset {parent_dataset:?} is not declared by the source schema"),
            property_nested_at(index, rule, &["nested", "dataset"]),
        ));
        return None;
    }
    let Some(parent_field) = source.field(parent_dataset, &nested.field) else {
        out.push(error(
            codes::UNKNOWN_FIELD,
            format!(
                "parent dataset {parent_dataset:?} has no collection field {:?}",
                nested.field
            ),
            property_nested_at(index, rule, &["nested", "field"]),
        ));
        return None;
    };
    let collection_is_json = parent_field.type_candidate.normalized == NormalizedType::Json;
    if !collection_is_json {
        out.push(error(
            codes::INVALID_NESTING,
            format!(
                "parent collection field {:?} on {parent_dataset:?} must have JSON type",
                nested.field
            ),
            property_nested_at(index, rule, &["nested", "field"]),
        ));
        return None;
    }
    let Some(fields) = input_fields(source, &nested.dataset) else {
        out.push(error(
            codes::UNKNOWN_DATASET,
            format!(
                "dataset {:?} is not declared by the source schema",
                nested.dataset
            ),
            property_nested_at(index, rule, &["nested", "dataset"]),
        ));
        return None;
    };
    match ZenTypeEnv::input_only(fields, EnvOptions::default()) {
        Ok(env) => Some(env),
        Err(reason) => {
            out.push(error(
                ENVIRONMENT_UNDERIVABLE,
                format!("nested property map cannot bind child input: {reason}"),
                property_nested_at(index, rule, &["nested"]),
            ));
            None
        }
    }
}

/// A map's resolved target: its object type name and record.
struct Target<'m> {
    object_type: &'m str,
    record: &'m RecordType,
}

fn check_lifecycles_and_gaps(
    map: &ObjectMap,
    index: usize,
    target: &Target<'_>,
    env: &ZenTypeEnv,
    model: &MappingModel,
    out: &mut Vec<Diagnostic>,
) {
    let Target {
        object_type,
        record,
    } = *target;
    for (rule, entry) in map.lifecycles.iter().enumerate() {
        let property = entry.property.as_str();
        if let Some(transitions) = model.transitions(object_type, property) {
            match (&entry.state, &entry.transition) {
                (Some(state), None) => {
                    let states = model
                        .lifecycle_states(object_type, property)
                        .unwrap_or_default();
                    let expected = Type::enumeration(states.iter().cloned()).optional();
                    let at = rule_at(index, "lifecycles", rule, "state");
                    typed(state, &expected, env, &at, out);
                }
                (None, Some(transition)) => {
                    let expected = Type::enumeration(transitions.iter().cloned()).optional();
                    let at = rule_at(index, "lifecycles", rule, "transition");
                    typed(transition, &expected, env, &at, out);
                }
                _ => {}
            }
            if let Some(state) = &entry.on_delete {
                let terminal = model
                    .lifecycle_terminal(object_type, property)
                    .unwrap_or_default();
                if !terminal.contains(state) {
                    out.push(error(
                        codes::INVALID_ON_DELETE,
                        format!(
                            "onDelete {state:?} of {property} is not a terminal state of {object_type}'s lifecycle ({terminal:?})"
                        ),
                        rule_at(index, "lifecycles", rule, "onDelete"),
                    ));
                }
            }
        } else {
            out.push(error(
                codes::LIFECYCLE_MEMBER,
                format!("{property} of {object_type} has no lifecycle"),
                rule_at(index, "lifecycles", rule, "property"),
            ));
        }
    }
    for (rule, gap) in map.gaps.iter().enumerate() {
        let member = record.fields.get(gap.member.as_str());
        if !member.is_some_and(|field| {
            matches!(
                field.source,
                FieldSource::Property { .. } | FieldSource::Link { .. }
            )
        }) {
            out.push(unknown_member(
                &gap.member,
                object_type,
                "property or link",
                rule_at(index, "gaps", rule, "member"),
            ));
        }
    }
}

fn check_links(
    mapping: &MappingIr,
    map: &ObjectMap,
    index: usize,
    target: &Target<'_>,
    env: &ZenTypeEnv,
    model: &MappingModel,
    out: &mut Vec<Diagnostic>,
) {
    let Target {
        object_type,
        record,
    } = *target;
    for (rule, entry) in map.links.iter().enumerate() {
        let link = entry.link.as_str();
        let link_target = record
            .fields
            .get(link)
            .filter(|field| matches!(field.source, FieldSource::Link { .. }))
            .and_then(|field| record_name(&field.ty));
        let Some(link_target) = link_target else {
            out.push(unknown_member(
                link,
                object_type,
                "link",
                rule_at(index, "links", rule, "link"),
            ));
            continue;
        };
        let produced = if let Some(target_map) = &entry.target_map {
            mapping
                .map(target_map)
                .and_then(|joined| model.object_type_of(&joined.target.entity))
                .map(ObjectTypeName::as_str)
        } else if entry.identity_binding {
            Some("party.Person")
        } else {
            None
        };
        if let Some(produced) = produced
            && !model.admits(link_target, produced)
        {
            out.push(error(
                codes::LINK_TARGET_MISMATCH,
                format!("link {link} of {object_type} targets {link_target}, which does not admit {produced}"),
                rule_at(index, "links", rule, "targetMap"),
            ));
        }
        let expected = Type::text().optional();
        typed(
            &entry.key,
            &expected,
            env,
            &rule_at(index, "links", rule, "key"),
            out,
        );
    }
}

/// The record a link field's type names (through `Optional` and `List`).
#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "The foreign non-exhaustive enum requires a catch-all; unknown variants retain the existing fallback"
)]
fn record_name(ty: &Type) -> Option<&str> {
    match ty {
        Type::Record(name) => Some(name.as_str()),
        Type::Optional(inner) | Type::List(inner) => record_name(inner),
        _ => None,
    }
}

fn check_source(
    map: &ObjectMap,
    index: usize,
    source: &SourceSchemaIr,
    out: &mut Vec<Diagnostic>,
) -> Option<BTreeMap<String, Type>> {
    let dataset = map.source.dataset.as_str();
    let Some(fields) = input_fields(source, dataset) else {
        out.push(error(
            codes::UNKNOWN_DATASET,
            format!("dataset {dataset:?} is not declared by the source schema"),
            map_at(index, &["source", "dataset"]),
        ));
        return None;
    };
    if let Some(nested) = &map.source.nested_in {
        // The nested field must be the child collection the source declares
        // (#2933): a parent field that merely exists (`grand_total`) is not.
        let owned = source.constraints.iter().any(|constraint| {
            constraint.kind == ConstraintKind::ParentChild
                && constraint.dataset == dataset
                && constraint.collection.as_deref() == Some(nested.field.as_str())
                && constraint
                    .target
                    .as_ref()
                    .is_some_and(|target| target.dataset == nested.dataset)
        });
        if !owned {
            out.push(error(
                codes::INVALID_NESTING,
                format!(
                    "dataset {dataset:?} is not the child collection {:?} a parent_child constraint declares on {:?}",
                    nested.field, nested.dataset
                ),
                map_at(index, &["source", "nestedIn"]),
            ));
        }
    }
    for (rule, ignored) in map.ignored_fields.iter().enumerate() {
        if !fields.contains_key(&ignored.field) {
            out.push(error(
                codes::UNKNOWN_FIELD,
                format!("dataset {dataset:?} has no field {:?}", ignored.field),
                rule_at(index, "ignoredFields", rule, "field"),
            ));
        }
    }
    Some(fields)
}

fn target_record<'m>(
    map: &ObjectMap,
    index: usize,
    model: &'m MappingModel,
    out: &mut Vec<Diagnostic>,
) -> Option<(&'m ObjectTypeName, &'m RecordType)> {
    let entity = map.target.entity.as_str();
    let Some((object_type, record)) = model.object_type_of(entity).and_then(|name| {
        model
            .env
            .record(name.as_str())
            .filter(|record| record.kind == RecordKind::Object)
            .map(|record| (name, record))
    }) else {
        out.push(error(
            codes::UNKNOWN_OBJECT_TYPE,
            format!("{entity:?} is not an entity artifact defining an available object type"),
            map_at(index, &["target", "entity"]),
        ));
        return None;
    };
    let implements = model
        .interface_of(&map.target.interface)
        .is_some_and(|interface| model.implements(object_type.as_str(), interface));
    if !implements {
        out.push(error(
            codes::INTERFACE_NOT_IMPLEMENTED,
            format!(
                "{} does not implement the interface artifact {:?}",
                object_type.as_str(),
                map.target.interface
            ),
            map_at(index, &["target", "interface"]),
        ));
    }
    Some((object_type, record))
}

/// Type-check one rule; a rule that does not compile is a local finding.
fn typed(
    expression: &ZenExpression,
    expected: &Type,
    env: &ZenTypeEnv,
    at: &Span,
    out: &mut Vec<Diagnostic>,
) {
    let Ok(compiled) = Expression::compile(expression.as_str()) else {
        return;
    };
    if let Err(findings) = check_expression(&compiled, env, expected) {
        out.extend(findings.into_iter().map(|finding| {
            error(
                codes::EXPRESSION_TYPE,
                format!(
                    "{:?}: {} ({})",
                    expression.as_str(),
                    finding.message,
                    finding.code
                ),
                at.clone(),
            )
        }));
    }
}

fn unknown_member(member: &str, object_type: &str, what: &str, at: Span) -> Diagnostic {
    error(
        codes::UNKNOWN_MEMBER,
        format!("{object_type} has no {what} {member:?}"),
        at,
    )
}

/// Whether `id` is in the member id grammar `[a-z][a-z0-9_]*`.
fn member_id(id: &str) -> bool {
    let mut chars = id.chars();
    chars.next().is_some_and(|first| first.is_ascii_lowercase())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn map_at(index: usize, rest: &[&str]) -> Span {
    let index = index.to_string();
    Span::from_segments(
        ["maps", index.as_str()]
            .into_iter()
            .chain(rest.iter().copied()),
    )
}

fn rule_at(index: usize, list: &str, rule: usize, member: &str) -> Span {
    map_at(index, &[list, rule.to_string().as_str(), member])
}

fn property_nested_at(index: usize, rule: usize, rest: &[&str]) -> Span {
    let index = index.to_string();
    let rule = rule.to_string();
    Span::from_segments(
        ["maps", index.as_str(), "properties", rule.as_str()]
            .into_iter()
            .chain(rest.iter().copied()),
    )
}
