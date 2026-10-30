// Deterministic core policy (#3299); host adapters retain their own arithmetic/collections.
#![deny(
    clippy::float_arithmetic,
    clippy::integer_division,
    clippy::modulo_arithmetic
)]
// TODO(rust-safety): staged downgrades from the workspace lint law
// (docs/platform/rust-safety-baseline.md). norn-ir is the kernel crate and is
// clean at the deny tier; these warn-tier downgrades remain. Counts as of
// 2026-08-19: missing_docs 183, clippy::pedantic ~44 (doc_markdown 9,
// too_many_lines 7, must_use_candidate 5, misc). Burn down, then delete.
#![allow(
    missing_docs,
    reason = "TODO(rust-safety): 183 undocumented public items; document then remove"
)]
#![allow(
    clippy::pedantic,
    reason = "TODO(rust-safety): ~44 pedantic findings; fix then remove"
)]
#![deny(
    clippy::match_wildcard_for_single_variants,
    clippy::format_collect,
    clippy::format_push_string,
    clippy::inefficient_to_string,
    clippy::large_types_passed_by_value,
    clippy::needless_pass_by_value,
    clippy::large_futures
)]

use std::collections::{BTreeMap, BTreeSet};
use std::marker::PhantomData;

use schemars::{JsonSchema, Schema, schema_for};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

pub mod access;
pub mod action;
pub mod business_pack;
pub mod connector;
pub mod customization;
pub mod data_quality;
pub mod decision_output;
pub mod deontic;
pub mod entitlement;
pub mod entity;
pub mod entity_v2;
pub mod fact_change;
pub mod flow_source_map;
pub mod function;
pub mod identifier_limits;
pub mod jdm;
pub mod layout;
pub mod limit_profile;
pub mod lineage;
pub mod mapping;
pub mod mapping_evidence;
pub mod marking;
pub mod members;
pub mod metric;
pub mod osi;
#[allow(
    missing_docs,
    reason = "exact posting wire contract moved unchanged from accounting"
)]
pub mod posting;
pub mod provenance;
pub mod quantity_ledger;
pub mod rule;
pub mod runtime;
pub mod sealing;
pub mod setting;
pub mod source_schema;
pub mod technical_view;
pub mod theorem_input;
pub mod view;
pub mod widget;
pub mod widget_ux;
pub use access::{
    AccessIr, ApprovalPolicyRoot, ObligationPolicyRoot, PolicyObligation, PolicyRoot,
};
pub use action::{
    AccessRef, ActionCommand, ActionCommandChildMerge, ActionCommandOperation, ActionCompensation,
    ActionCreateLink, ActionInputDerivation, ActionQuantityEffect, ActionQuantityOperation,
    ActionQuantityParameter, ActionRoot, ActionUiEffect, ActionUiEffects, ActionWrite, McpTool,
    McpToolAnnotations, WriteChange,
};
pub use business_pack::{
    BusinessPackKind, BusinessPackRoot, CompositionPackKind, CompositionPackRoot, MappingRelation,
    OntologyMapping, OntologyPackKind, OntologyPackRoot, OntologyShape, OntologyShapeField,
    OntologySource, OntologySourceFile, OntologyTerm, OntologyTermKind, OntologyTermStatus,
    PackArtifactKind, PackArtifactRef, SemanticAuthority, ShapeValueType, SourceRedistribution,
};
pub use connector::{
    ApiKeyFormat, ConnectorAuth, ConnectorChildMergeProfile, ConnectorCommand,
    ConnectorConflictCategory, ConnectorConflictRule, ConnectorCursor, ConnectorCustomStepHook,
    ConnectorIdempotencyProfile, ConnectorKeysetProfile, ConnectorLifecycleProfile,
    ConnectorLookup, ConnectorLookupFilter, ConnectorLookupValue, ConnectorMapping,
    ConnectorPageMode, ConnectorReceiptContract, ConnectorResource, ConnectorRoot,
    ConnectorSupportedVersions, ConnectorVersionComponent, DeadLetterPolicy, HttpMethod,
};
pub use customization::{
    CustomizationApproval, CustomizationExecutable, CustomizationIntent, CustomizationOverlay,
    CustomizationOverlayOp, CustomizationRoot, CustomizationTarget, ResourceLimits,
};
pub use data_quality::{
    DATA_QUALITY_FINDING_VERSION, DataQualityFinding, DataQualityFindingStatus, DataQualityLineage,
    DataQualityOutcome, DataQualityRuleRef, DataQualitySeverity, DataQualitySubject,
    InvariantOwner, data_quality_finding_schema, data_quality_lineage_schema,
    data_quality_subject_schema,
};
pub use decision_output::{DecisionOutput, TIMER_AFTER_MILLIS_MEMBER, TIMER_ID_MEMBER};
pub use entitlement::{
    Entitlement, EntitlementCondition, EntitlementGrantee, EntitlementSource, EntitlementStatus,
    ObjectRef,
};
pub use entity::{
    ENTITY_IR_V2_DEFINITION_MEMBERS, ENTITY_IR_V2_VERSION, EntityAuthorityDomain,
    EntityExtensionRootV2, EntityFieldV1, EntityInterfaceRootV2, EntityIrVersion,
    EntityMutationAuthority, EntityRoot, EntityRootV1, EntityRootV2, MarkingCatalogRootV2,
    PropertyTypeCatalogRootV2, RelationCardinalityV1, RelationRoot,
};
pub use entity_v2::extension::{
    EXTENSION_MEMBER_SEPARATOR, EXTENSION_NAMESPACE_PATTERN, ExtensionNamespace,
    ObjectTypeExtension,
};
pub use entity_v2::interface::{InterfaceDef, InterfaceName, InterfaceProperty};
pub use entity_v2::invariant::{ConstraintClass, Invariant, InvariantId};
pub use entity_v2::lifecycle::{
    FiringRefusal, Lifecycle, LifecyclePathProperty, Transition, TransitionId,
    codes as lifecycle_codes, declared_lifecycle_properties, effective_lifecycles,
};
pub use entity_v2::link::{InterfaceLink, Link, LinkCardinality, LinkId, Multiplicity};
pub use entity_v2::naming_series::{
    NamingCounterSeed, NamingGapPolicy, NamingResetPolicy, NamingSeriesIR,
};
pub use entity_v2::object::{
    ComponentNormalization, DefaultValue, IdentityRule, KeyName, KeySpec, MEMBER_ID_PATTERN,
    Normalization, OBJECT_TYPE_NAME_PATTERN, ObjectTypeDef, ObjectTypeName, OwnedChildCollection,
    Property, PropertyId, ResolutionPolicyName,
};
pub use entity_v2::property::{
    DECIMAL_LITERAL_PATTERN, DecimalLiteral, EnumValue, PROPERTY_TYPE_NAME_PATTERN,
    PropertyConstraints, PropertyKind, PropertyType, PropertyTypeDef, PropertyTypeName,
    RecordMember, TextFormat, TimezonePolicy, UnitRef, UnitSystem,
};
pub use entity_v2::status::{CALENDAR_DATE_PATTERN, CalendarDate, ResourceStatus};
pub use entity_v2::{
    interface_def_schema, object_type_def_schema, object_type_extension_schema,
    property_type_def_schema,
};
pub use fact_change::{
    FACT_CHANGE_MEMBERS, FACT_CHANGE_REQUIRED_MEMBERS, FactChange, FactChangeOp, fact_change_schema,
};
pub use flow_source_map::{
    FlowSourceMap, FlowSourceMapArtifactRef, FlowSourceMapEdge, FlowSourceMapEdgeKind,
    FlowSourceMapFlow, FlowSourceMapFormat, FlowSourceMapSourceLocation, FlowSourceMapStep,
    FlowSourceMapStepKind, flow_source_map_schema,
};
pub use jdm::{JdmGraph, JdmNode, JdmNodeType};
pub use layout::{
    ArtifactLayout, CanvasPosition, ColumnLayout, EdgeLayout, LAYOUT_SIDECAR_VERSION, LayoutField,
    LayoutSidecar, LayoutViewport, NodeLayout, RowLayout,
};
pub use limit_profile::{
    DecideBudget, EnvelopeLimits, HelperLimits, LIMIT_PROFILE_HASH_PROFILE, LimitProfileRef,
    MemoryLimits, NORN_LIMITS_PROFILE_ID, NORN_LIMITS_V1_SHA256, NORN_LIMITS_V1_VERSION,
    ResourceLimitProfile, SliceLimits, TurnBudget,
};
pub use lineage::{
    DecisionRef, LineageCommit, LineageInput, LineageLink, LineageObject, LineageObservation,
    LineageTrigger, commit_stable_id, object_stable_id,
};
pub use marking::{MARKING_ID_PATTERN, MarkingId, MarkingIr, MarkingSeverity, MaskPolicy};
pub use members::{EmptyList, IrVersion01, JsonFalse, ProposalMetadata, WholeNumber};
pub use metric::{MetricDefinition, QuestionIR, metric_ir_schema, question_ir_schema};
pub use osi::MetricIR;
pub use provenance::{
    FactRef, InstanceProvenance, MappingVersion, ProvenancedInstance, SourceRecordRef,
    UTC_TIMESTAMP_PATTERN, UtcTimestamp, instance_provenance_schema,
};
pub use rule::{
    DECIDE_INPUT_MEMBERS, DECIDE_OUTPUT_MEMBERS, JsonObject, MAX_RULE_CAUSAL_DEPTH,
    RULE_IR_V1_VERSION, RULE_IR_V2_VERSION, RuleContractV2, RuleDecisionV1, RuleRoot,
    RuleSourceCommand, RuleTrigger, SliceDeclaration, SliceKey,
};
pub use runtime::{Decimal, RuntimeObject, RuntimeValue, RuntimeValueError};
pub use sealing::{
    CustomizationOverlayOperation, ExtensionContributionKind, ExtensionPointDefinition,
    SEAL_PRECEDENCE_LADDER, SEALING_IR_VERSION, SealAnnotation, SealError, SealInput, SealOutcome,
    SealValidationResult, SealableNodeKind,
};
pub use setting::{
    SettingReview, SettingReviewState, SettingRoot, SettingSupersedes, SettingValue,
    SettingValueType,
};
pub use technical_view::{LineageRelation, TECHNICAL_NAMESPACE, TechnicalType, is_technical_type};
pub use theorem_input::{
    SourceRef, SourceSpan, TheoremAuthority, TheoremConclusion, TheoremConclusionModality,
    TheoremEvidenceKind, TheoremEvidenceLabel, TheoremEvidenceTarget, TheoremInput,
    TheoremInputSchemaVersion, TheoremPremise, TheoremPremiseKind, TheoremSemanticProfile,
    TheoremStateModel, TheoremVariable, TheoremVariableDomain, TheoremVariableType,
};
pub use view::{
    CalcExpr, PrimitiveBasisClass, TemporalScope, TemporalScopeMode, ViewInputKind, ViewInputRef,
    ViewIrPayload, ViewMaintenance,
};
pub use widget::{
    WidgetActionBinding, WidgetDataDependency, WidgetPermission, WidgetProjectionHints,
    WidgetQuery, WidgetRoot, WidgetViewState,
};
pub use widget_ux::{
    ArtifactRef as WidgetArtifactRef, LocaleText, PropertyRef, WIDGET_UX_V1_VERSION,
    WidgetActionConfirmation, WidgetActionLocation, WidgetActionPlacement, WidgetActionSeverity,
    WidgetApp, WidgetComponent, WidgetConfirmationStrength, WidgetConflictBehavior,
    WidgetContextParam, WidgetDataBinding, WidgetDataQuery, WidgetDismissalMethod,
    WidgetEffectHandler, WidgetExpressionInputs, WidgetFieldControl, WidgetFieldFormat,
    WidgetFieldPresentation, WidgetFieldTarget, WidgetFilter, WidgetFilterOperator,
    WidgetFilterValue, WidgetForm, WidgetFormMode, WidgetFormSubmit, WidgetKeyValue,
    WidgetNativeProjection, WidgetNavigationDestination, WidgetNavigationItem, WidgetNode,
    WidgetOutcomeState, WidgetOverride, WidgetOverrideValue, WidgetPage, WidgetPageBounds,
    WidgetPageRef, WidgetRoute, WidgetRouteParameter, WidgetSection, WidgetSectionKind,
    WidgetSelectionMode, WidgetShellHandlers, WidgetSort, WidgetSortDirection, WidgetUx,
    WidgetUxActionBinding, WidgetValueType, WidgetVersionConflictBinding, ZenCondition,
};

/// Representation-independent JSON number classification (issue #2440).
/// Re-exported so downstream kernel crates share one number rule without a
/// direct `norn-canonical` edge.
pub use norn_canonical::{JsonNumber, JsonNumberKind, json_tree_from_value};

/// Norn1 canonical JSON and hash profiles, re-exported so ring-0 crates that
/// content-address their own declarative data (the `norn-expr` type
/// environment, T1-EXPR-2) use the one canonical form without a direct
/// `norn-canonical` edge (SKELETON.md: `norn-expr ← zen-expression, ir`).
pub mod canonical {
    pub use norn_canonical::{
        CanonicalJsonError, HashProfile, JsonValue, canonical_json_of, canonicalize_json,
        domain_hash_hex, legacy_hash_value, nfc, parse_json, parse_json_verbatim, serialize,
    };
}

pub mod verification;
pub use verification::{
    LawIR, LawModality, LawProvenance, LawResponse, LawRoot, LawScope, LawSeverity, LawStateScope,
    LawTiming,
};

pub const SUPPORTED_IR_VERSION: &str = "0.1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
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
    /// ViewIR v0 — pure, deterministic, recompute-on-read computed view.
    /// Declared over Source-bearing entity/relation facts and other Derived views.
    /// No IO, no nondeterminism, no side effects.
    Derived,
    /// SourceSchemaIR (T8-SRC-1): what discovery found in a source system.
    /// Evidence, never mapped directly to the ontology.
    SourceSchema,
    /// MappingIR (T8-MAP-1a): an interface-scoped mapping from one source
    /// schema to EntityIR objects.
    Mapping,
    /// Scoped temporal laws over ZEN predicates.
    Law,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactLifecycle {
    Draft,
    Proposed,
    Approved,
    Active,
    Deprecated,
    Superseded,
    Rejected,
}

/// Returns whether the direct authoring/catalog path may persist this lifecycle.
///
/// Authority-bearing definitions may be drafted or proposed directly, but the
/// Approved/Active states are reserved for the governed validation, review, and
/// apply state machine. This function is deliberately pure so the production
/// guard can be extracted into a proof assistant.
#[must_use]
pub const fn direct_artifact_write_allowed(
    kind: &ArtifactKind,
    lifecycle: &ArtifactLifecycle,
) -> bool {
    let authority_bearing = matches!(
        kind,
        ArtifactKind::Entity
            | ArtifactKind::Relation
            | ArtifactKind::Action
            | ArtifactKind::Rule
            | ArtifactKind::Connector
            | ArtifactKind::Customization
            | ArtifactKind::Setting
            | ArtifactKind::BusinessPack
            | ArtifactKind::Policy
            | ArtifactKind::Widget
            | ArtifactKind::Derived
            | ArtifactKind::Mapping
            | ArtifactKind::Law
    );
    let grants_runtime_authority = matches!(
        lifecycle,
        ArtifactLifecycle::Approved | ArtifactLifecycle::Active
    );

    !(authority_bearing && grants_runtime_authority)
}

/// Decide the final hard-invariant mutation authority truth table.
///
/// The engine derives these three facts from closed `EntityIR` and a typed
/// mutation source. Keeping the final decision pure and representation-free
/// lets CI extract the exact production predicate into a proof assistant:
///
/// - generic authority admits only a generic action;
/// - domain authority admits only a domain command with the exact same domain.
///
/// This predicate does not parse an authority descriptor and does not establish
/// that a caller derived the facts correctly. Those are separate runtime
/// obligations checked by the engine.
#[must_use]
pub const fn entity_mutation_authority_admits(
    authority_is_generic: bool,
    source_is_generic: bool,
    domains_match: bool,
) -> bool {
    (authority_is_generic && source_is_generic)
        || (!authority_is_generic && !source_is_generic && domains_match)
}

/// Exact representation-free admission predicate for a decision-table match set.
///
/// The closed RuleIR evaluator derives these facts while evaluating rows in declaration order.
/// `FIRST` admits the first available match; `UNIQUE` admits a match only when no second row also
/// matched. Default-output handling is deliberately outside this predicate because it applies
/// only when `has_match` is false. Keeping this final decision Boolean and pure lets CI extract
/// the exact production predicate into F* without trusting the surrounding JSON decoder.
#[must_use]
pub const fn rule_hit_policy_admits_match(
    first_hit_policy: bool,
    has_match: bool,
    has_multiple_matches: bool,
) -> bool {
    has_match && (first_hit_policy || !has_multiple_matches)
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ScopeRef {
    pub org_id: Option<String>,
    pub tenant_id: Option<String>,
    pub workspace_id: Option<String>,
    pub environment_id: Option<String>,
    pub business_unit_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactRef {
    pub artifact_id: String,
    pub kind: ArtifactKind,
    pub version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactDependency {
    pub artifact_id: String,
    pub kind: ArtifactKind,
    pub version: Option<String>,
    pub required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProvenanceSource {
    AuthoringSdk,
    Agent,
    Migration,
    Import,
    System,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ArtifactProvenance {
    pub source: ProvenanceSource,
    pub source_path: Option<String>,
    pub actor_id: Option<String>,
    pub created_at: Option<String>,
}

// Generic JSON carried inside IR payloads.
//
// Deserialization goes through `norn_canonical::deserialize_json_tree`
// (number rule: `norn_canonical::JsonNumber`) rather than a derived untagged visitor, so a
// number decodes to the same `f64` whether or not
// `serde_json/arbitrary_precision` is enabled anywhere in the build graph
// (issue #2440). With that feature a derived untagged enum sees numbers as a
// private single-entry map and silently decodes them as `Object`.
//
// `Deserialize` keeps each number's origin as `serde_json` reports it: JSON
// text decodes like the historical default-profile parse, and
// `serde_json::from_value` of an in-memory `Value` keeps the constructed bits
// (see `norn_canonical::deserialize_json_tree`). `json_tree_from_value` is the
// explicit in-memory conversion; `JsonValueDeserializer` decodes typed data
// from a `JsonValue` with exact `f64`s.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
#[serde(untagged)]
pub enum JsonValue {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<JsonValue>),
    Object(BTreeMap<String, JsonValue>),
}

impl<'de> Deserialize<'de> for JsonValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        norn_canonical::deserialize_json_tree(deserializer)
    }
}

/// A `serde` deserializer over an owned [`JsonValue`].
///
/// Decoding typed data straight from IR JSON (instead of
/// `serde_json::to_value` + `serde_json::from_value`) keeps every number's
/// exact `f64` and never reaches `serde_json`'s number representation, so the
/// result cannot depend on `serde_json/arbitrary_precision` (issue #2440).
/// Numbers are visited as `f64`, matching what `serde_json::from_value`
/// produced for `to_value` of this type in the default profile.
pub struct JsonValueDeserializer<E> {
    value: JsonValue,
    marker: std::marker::PhantomData<E>,
}

impl<'de, E: serde::de::Error> serde::de::IntoDeserializer<'de, E> for JsonValue {
    type Deserializer = JsonValueDeserializer<E>;

    fn into_deserializer(self) -> Self::Deserializer {
        JsonValueDeserializer {
            value: self,
            marker: std::marker::PhantomData,
        }
    }
}

impl<'de, E: serde::de::Error> serde::Deserializer<'de> for JsonValueDeserializer<E> {
    type Error = E;

    fn deserialize_any<V>(self, visitor: V) -> Result<V::Value, E>
    where
        V: serde::de::Visitor<'de>,
    {
        match self.value {
            JsonValue::Null => visitor.visit_unit(),
            JsonValue::Bool(value) => visitor.visit_bool(value),
            JsonValue::Number(value) => visitor.visit_f64(value),
            JsonValue::String(value) => visitor.visit_string(value),
            JsonValue::Array(values) => {
                let mut access = serde::de::value::SeqDeserializer::new(values.into_iter());
                let output = visitor.visit_seq(&mut access)?;
                access.end()?;
                Ok(output)
            }
            JsonValue::Object(entries) => {
                let mut access = serde::de::value::MapDeserializer::new(entries.into_iter());
                let output = visitor.visit_map(&mut access)?;
                access.end()?;
                Ok(output)
            }
        }
    }

    fn deserialize_option<V>(self, visitor: V) -> Result<V::Value, E>
    where
        V: serde::de::Visitor<'de>,
    {
        match self.value {
            JsonValue::Null => visitor.visit_none(),
            JsonValue::Bool(_)
            | JsonValue::Number(_)
            | JsonValue::String(_)
            | JsonValue::Array(_)
            | JsonValue::Object(_) => visitor.visit_some(self),
        }
    }

    fn deserialize_newtype_struct<V>(self, _name: &'static str, visitor: V) -> Result<V::Value, E>
    where
        V: serde::de::Visitor<'de>,
    {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_enum<V>(
        self,
        _name: &'static str,
        _variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, E>
    where
        V: serde::de::Visitor<'de>,
    {
        use serde::de::IntoDeserializer;
        match self.value {
            JsonValue::String(variant) => visitor.visit_enum(variant.into_deserializer()),
            JsonValue::Object(entries) if entries.len() == 1 => {
                visitor.visit_enum(serde::de::value::MapAccessDeserializer::new(
                    serde::de::value::MapDeserializer::new(entries.into_iter()),
                ))
            }
            JsonValue::Null
            | JsonValue::Bool(_)
            | JsonValue::Number(_)
            | JsonValue::Array(_)
            | JsonValue::Object(_) => Err(serde::de::Error::invalid_type(
                serde::de::Unexpected::Other("non-enum JSON value"),
                &"a string or single-key object",
            )),
        }
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string
        bytes byte_buf unit unit_struct seq tuple tuple_struct map struct
        identifier ignored_any
    }
}

impl norn_canonical::JsonTree for JsonValue {
    fn null() -> Self {
        Self::Null
    }

    fn boolean(value: bool) -> Self {
        Self::Bool(value)
    }

    fn number(value: norn_canonical::JsonNumber) -> Self {
        Self::Number(value.as_f64())
    }

    fn string(value: String) -> Self {
        Self::String(value)
    }

    fn array(values: Vec<Self>) -> Self {
        Self::Array(values)
    }

    fn object(entries: Vec<(String, Self)>) -> Self {
        Self::Object(entries.into_iter().collect())
    }
}

/// Source text of one ZEN Expression (the IR's single expression language,
/// ruling 2026-09-25 §3): a decision-table cell, switch condition, expression
/// assignment, or action precondition.
///
/// Stored verbatim; parsing and type checking belong to `norn-expr` and
/// `norn-checker`.
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct ZenExpression(pub String);

impl ZenExpression {
    /// The expression source.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Closed, declarative authority-bearing operations admitted by ActionIR.
///
/// This is intentionally an enum rather than an open callback or expression
/// language. Every added variant expands the trusted execution surface and
/// therefore requires validation, correspondence tests, and proof obligations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum ActionEffectProgram {
    /// Derive exactly one object upsert from the already schema-checked action
    /// input and a pinned Entity artifact identity.
    UpsertInputObject {
        object_id_field: String,
        object_kind: String,
        entity_artifact_id: String,
        entity_artifact_version: String,
        /// Additional admission rule for an existing object. Omitted for a
        /// normal upsert, which may create or update while the lifecycle is
        /// writable.
        #[serde(default, skip_serializing_if = "UpsertWriteMode::is_upsert")]
        write_mode: UpsertWriteMode,
    },
    /// Fire one declared transition of the object the action input names
    /// (`objectIdField`), pinned by Entity artifact identity. `lifecycle` is
    /// the lifecycle property and `transition` the transition id it declares;
    /// the lifecycle owns the source states, the target state, and the
    /// guards, so the effect writes that one property and nothing else
    /// (T3-LIFE-3, #2867).
    FireTransition {
        object_id_field: String,
        object_kind: String,
        entity_artifact_id: String,
        entity_artifact_version: String,
        lifecycle: PropertyId,
        transition: TransitionId,
    },
    /// Atomically mutate a parent and its declared ordered child rows.
    /// Membership and order are checked against the host's protected
    /// `norn.OwnedRows` manifest, never against a caller-provided row list.
    MutateParentWithRows {
        object_id_field: String,
        object_kind: String,
        entity_artifact_id: String,
        entity_artifact_version: String,
        /// Input object holding parent property changes.
        parent_values_field: String,
        /// Input array holding closed row operations.
        row_ops_field: String,
        /// Input canonical decimal-string expected parent version.
        parent_version_field: String,
        /// Optional input bool requesting a parent delete.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_delete_field: Option<String>,
        /// Trusted row conditions and derivations per owned collection.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        collections: Vec<OwnedCollectionAction>,
    },
}

/// Trusted row-level conditions and derivations for one owned collection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OwnedCollectionAction {
    /// The collection link declared by the target entity type.
    pub id: LinkId,
    /// Boolean ZEN expressions evaluated against every touched row.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub preconditions: Vec<ZenExpression>,
    /// Trusted ZEN expressions used to derive row values.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub derive: BTreeMap<PropertyId, ZenExpression>,
}

/// Write guard for an action's object upsert.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum UpsertWriteMode {
    /// Create or update the target, subject to lifecycle immutability.
    #[default]
    Upsert,
    /// The target must not exist.
    CreateOnly,
    /// The target must exist in the declared initial state of this lifecycle.
    DraftOnly {
        /// The lifecycle property that owns the draft state.
        lifecycle: PropertyId,
        /// The lifecycle's declared initial state used as the draft state.
        state: String,
    },
}

impl UpsertWriteMode {
    /// Whether this is the default mode that need not be serialized.
    #[must_use]
    pub const fn is_upsert(&self) -> bool {
        matches!(self, Self::Upsert)
    }
}

/// The object an [`ActionEffectProgram`] writes: the input field that names
/// it, its object kind, and the pinned `entity` artifact that defines the
/// kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActionEffectTarget<'a> {
    pub object_id_field: &'a str,
    pub object_kind: &'a str,
    pub entity_artifact_id: &'a str,
    pub entity_artifact_version: &'a str,
}

impl ActionEffectProgram {
    /// The object the program writes.
    #[must_use]
    pub fn target(&self) -> ActionEffectTarget<'_> {
        match self {
            Self::UpsertInputObject {
                object_id_field,
                object_kind,
                entity_artifact_id,
                entity_artifact_version,
                ..
            }
            | Self::FireTransition {
                object_id_field,
                object_kind,
                entity_artifact_id,
                entity_artifact_version,
                ..
            } => ActionEffectTarget {
                object_id_field,
                object_kind,
                entity_artifact_id,
                entity_artifact_version,
            },
            Self::MutateParentWithRows {
                object_id_field,
                object_kind,
                entity_artifact_id,
                entity_artifact_version,
                ..
            } => ActionEffectTarget {
                object_id_field,
                object_kind,
                entity_artifact_id,
                entity_artifact_version,
            },
        }
    }

    /// How the program may change its target: the write-set entries it
    /// needs. An upsert may create or modify; a transition only modifies.
    #[must_use]
    pub const fn changes(&self) -> &'static [WriteChange] {
        match self {
            Self::UpsertInputObject {
                write_mode: UpsertWriteMode::CreateOnly,
                ..
            } => &[WriteChange::Created],
            Self::UpsertInputObject {
                write_mode: UpsertWriteMode::DraftOnly { .. },
                ..
            } => &[WriteChange::Modified],
            Self::UpsertInputObject { .. } => &[WriteChange::Created, WriteChange::Modified],
            Self::FireTransition { .. } => &[WriteChange::Modified],
            Self::MutateParentWithRows {
                parent_delete_field: Some(_),
                ..
            } => &[
                WriteChange::Created,
                WriteChange::Modified,
                WriteChange::Deleted,
            ],
            Self::MutateParentWithRows { .. } => &[WriteChange::Created, WriteChange::Modified],
        }
    }
}

impl JsonValue {
    /// Return `Some(&str)` if this is a `JsonValue::String`, else `None`.
    pub fn as_string(&self) -> Option<&str> {
        match self {
            JsonValue::String(s) => Some(s.as_str()),
            JsonValue::Null
            | JsonValue::Bool(_)
            | JsonValue::Number(_)
            | JsonValue::Array(_)
            | JsonValue::Object(_) => None,
        }
    }

    /// Return `Some(&[JsonValue])` if this is a `JsonValue::Array`, else `None`.
    pub fn as_array(&self) -> Option<&[JsonValue]> {
        match self {
            JsonValue::Array(a) => Some(a.as_slice()),
            JsonValue::Null
            | JsonValue::Bool(_)
            | JsonValue::Number(_)
            | JsonValue::String(_)
            | JsonValue::Object(_) => None,
        }
    }

    /// Return `Some(&BTreeMap)` if this is a `JsonValue::Object`, else `None`.
    pub fn as_object(&self) -> Option<&BTreeMap<String, JsonValue>> {
        match self {
            JsonValue::Object(o) => Some(o),
            JsonValue::Null
            | JsonValue::Bool(_)
            | JsonValue::Number(_)
            | JsonValue::String(_)
            | JsonValue::Array(_) => None,
        }
    }
}

/// A declared payload field, retaining the distinction between a missing key
/// and an explicitly supplied JSON `null` while legacy diagnostics remain
/// JSON-path based.
///
/// The value is stored as the raw wire JSON so decoding stays lenient (a
/// malformed member becomes a `norn-checker` diagnostic, not a decode error).
/// `T` is the member's typed contract: it is used only for the derived JSON
/// Schema, and `norn-checker` decodes the raw value into `T` to report
/// path-based diagnostics. Every member declares its contract (T1-IR-2): there
/// is no untyped default, so the reserved `extensions` slot is the only
/// generic JSON member of a payload root.
///
/// # Lenient wire roots, strict typed roots
///
/// Each kind has two decoders, and both are deliberate:
///
/// - The **lenient wire root** (`EntityPayload`, `ActionPayload`, …; built by
///   `define_payload_root!`) accepts any JSON object. It stores each declared
///   member as its raw wire value and records unknown members and a malformed
///   `extensions` slot instead of failing. This is what [`ArtifactEnvelope`]
///   decodes into, so a malformed payload still yields an envelope and
///   `norn-checker` reports every problem as a located diagnostic with its
///   registered code and message. Encoding writes the same sorted object back,
///   so wire bytes and hashes never depend on the typed model.
/// - The **strict typed root** (`EntityRoot`, `ActionRoot`, `SettingRoot`,
///   `PolicyRoot`, …) is the contract: derived `deny_unknown_fields` serde
///   types whose members carry their real types. `norn-checker` decodes an
///   accepted payload into it (`check_typed_root`), and the published JSON
///   Schema is derived from it (directly, or per member through `T`).
///
/// Replacing the lenient root with derive-strict decoding would turn the
/// per-member diagnostics into one opaque decode error per envelope (a
/// diagnostic contract change), so the lenient root stays the wire decoder
/// and the strict root stays the typed view of an accepted payload.
pub struct PayloadField<T> {
    value: Option<JsonValue>,
    contract: PhantomData<fn() -> T>,
}

impl<T> PayloadField<T> {
    /// A member that was not present on the wire.
    #[must_use]
    pub const fn absent() -> Self {
        Self {
            value: None,
            contract: PhantomData,
        }
    }

    /// A member that was present on the wire with this raw JSON value.
    #[must_use]
    pub const fn present(value: JsonValue) -> Self {
        Self {
            value: Some(value),
            contract: PhantomData,
        }
    }

    const fn from_option(value: Option<JsonValue>) -> Self {
        Self {
            value,
            contract: PhantomData,
        }
    }

    /// Whether the member was absent on the wire.
    #[must_use]
    pub const fn is_absent(&self) -> bool {
        self.value.is_none()
    }

    /// Borrow the declared JSON value when this field was present on the wire.
    #[must_use]
    pub const fn as_value(&self) -> Option<&JsonValue> {
        self.value.as_ref()
    }
}

impl<T> Default for PayloadField<T> {
    fn default() -> Self {
        Self::absent()
    }
}

impl<T> Clone for PayloadField<T> {
    fn clone(&self) -> Self {
        Self::from_option(self.value.clone())
    }
}

impl<T> PartialEq for PayloadField<T> {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
    }
}

impl<T> std::fmt::Debug for PayloadField<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.value {
            None => formatter.write_str("Absent"),
            Some(value) => formatter.debug_tuple("Present").field(value).finish(),
        }
    }
}

impl<T: JsonSchema> JsonSchema for PayloadField<T> {
    fn inline_schema() -> bool {
        T::inline_schema()
    }

    fn schema_name() -> std::borrow::Cow<'static, str> {
        T::schema_name()
    }

    fn schema_id() -> std::borrow::Cow<'static, str> {
        T::schema_id()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> Schema {
        T::json_schema(generator)
    }
}

impl<T> Serialize for PayloadField<T> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match &self.value {
            None => serializer.serialize_unit(),
            Some(value) => value.serialize(serializer),
        }
    }
}

impl<'de, T> Deserialize<'de> for PayloadField<T> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        JsonValue::deserialize(deserializer).map(Self::present)
    }
}

macro_rules! define_payload_root {
    (
        $(#[$container:meta])*
        $name:ident {
            required {
                $($required_field:ident => $required_wire_name:literal as $required_ty:ty),*
                $(,)?
            }
            optional {
                $($optional_field:ident => $optional_wire_name:literal as $optional_ty:ty),*
                $(,)?
            }
        }
    ) => {
        // Closed payload root for one artifact kind. Wire encoding goes through the kind
        // root's JSON object: decoding is lenient so the existing validators keep reporting
        // unknown fields and malformed `extensions` as path-based diagnostics, and encoding
        // is the sorted legacy object so canonical and raw artifact hashes are unchanged.
        // The JSON Schema is derived from the declared fields and is closed; each member
        // declared `as Type` publishes `Type` as its schema contract while its storage
        // stays the raw wire value. A root whose members depend on each other (forms,
        // tagged values) publishes its strict typed root instead (`schemars(with)`).
        #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
        #[serde(
            from = "BTreeMap<String, JsonValue>",
            into = "JsonValue",
            rename_all = "camelCase",
            deny_unknown_fields
        )]
        #[schemars(!from, !into)]
        $(#[$container])*
        pub struct $name {
            $(
                // No `default`: schemars marks these members required. Serde itself decodes
                // through `from_json_object`, which keeps a missing member as `Absent` so the
                // validators report it.
                #[serde(skip_serializing_if = "PayloadField::is_absent")]
                pub $required_field: PayloadField<$required_ty>,
            )*
            $(
                #[serde(default, skip_serializing_if = "PayloadField::is_absent")]
                pub $optional_field: PayloadField<$optional_ty>,
            )*
            /// Reserved extension slots keyed by `<name>/<semver>`.
            #[serde(default, skip_serializing_if = "Option::is_none")]
            #[schemars(with = "BTreeMap<String, JsonValue>")]
            pub extensions: Option<BTreeMap<String, JsonValue>>,
            #[schemars(skip)]
            #[serde(skip)]
            unknown_fields: BTreeSet<String>,
            #[serde(skip)]
            #[schemars(skip)]
            invalid_extensions: bool,
        }

        impl From<BTreeMap<String, JsonValue>> for $name {
            fn from(object: BTreeMap<String, JsonValue>) -> Self {
                Self::from_json_object(object)
            }
        }

        impl From<$name> for JsonValue {
            fn from(root: $name) -> Self {
                root.to_json_value()
            }
        }

        impl $name {
            fn from_json_object(mut object: BTreeMap<String, JsonValue>) -> Self {
                let (extensions, invalid_extensions) = match object.remove("extensions") {
                    Some(JsonValue::Object(extensions)) => (Some(extensions), false),
                    Some(_) => (None, true),
                    None => (None, false),
                };

                $(
                    let $required_field = PayloadField::from_option(object.remove($required_wire_name));
                )*
                $(
                    let $optional_field = PayloadField::from_option(object.remove($optional_wire_name));
                )*

                Self {
                    $($required_field,)*
                    $($optional_field,)*
                    extensions,
                    unknown_fields: object.into_keys().collect(),
                    invalid_extensions,
                }
            }

            #[must_use]
            pub fn to_json_value(&self) -> JsonValue {
                let mut object = BTreeMap::new();
                $(
                    if let Some(value) = self.$required_field.as_value() {
                        object.insert($required_wire_name.to_string(), value.clone());
                    }
                )*
                $(
                    if let Some(value) = self.$optional_field.as_value() {
                        object.insert($optional_wire_name.to_string(), value.clone());
                    }
                )*
                if let Some(extensions) = &self.extensions {
                    object.insert(
                        "extensions".to_string(),
                        JsonValue::Object(extensions.clone()),
                    );
                }
                JsonValue::Object(object)
            }

            /// Top-level payload members that are not declared by this closed root.
            ///
            /// Decoding is lenient so the checker can report them as path-based
            /// diagnostics.
            #[must_use]
            pub const fn unknown_fields(&self) -> &BTreeSet<String> {
                &self.unknown_fields
            }

            /// Whether the wire `extensions` member was present but not an object map.
            #[must_use]
            pub const fn has_invalid_extensions(&self) -> bool {
                self.invalid_extensions
            }
        }
    };
}

define_payload_root!(
    // The lenient `entity` wire root declares the members of both EntityIR
    // forms. Its published JSON Schema is the strict typed root `EntityRoot`
    // (v1 or v2, selected by `entityVersion`), which norn-checker decodes it
    // into; the doc comment below is that schema's description.
    /// The payload of an `entity` artifact: EntityIR v1 (`objectKind` and a
    /// `fields` map; no `entityVersion`) or EntityIR v2 (`entityVersion`
    /// "2.0" and exactly one definition: `objectType`, `interface`,
    /// `objectTypeExtension`, `propertyTypes`, or `markings`). V1 fields and
    /// V2 definitions can both declare default-empty marking lists.
    #[schemars(with = "EntityRoot")]
    EntityPayload {
        required {
            object_kind => "objectKind" as String,
            fields => "fields" as BTreeMap<String, EntityFieldV1>
        }
        optional {
            mutation_authority => "mutationAuthority" as EntityMutationAuthority,
            overlay_sources => "overlaySources" as Vec<String>,
            entity_version => "entityVersion" as EntityIrVersion,
            object_type => "objectType" as ObjectTypeDef,
            interface => "interface" as InterfaceDef,
            object_type_extension => "objectTypeExtension" as ObjectTypeExtension,
            property_types => "propertyTypes" as Vec<PropertyTypeDef>,
            markings => "markings" as JsonValue
        }
    }
);

impl EntityPayload {
    /// Whether the payload is in the EntityIR v2 form (`entityVersion` is
    /// present, whatever its value; the checker validates the value).
    #[must_use]
    pub const fn is_entity_ir_v2(&self) -> bool {
        !self.entity_version.is_absent()
    }
}

// RelationIR v1 ([`RelationRoot`]); v2 relations are links on the entity root.
define_payload_root!(RelationPayload {
    required { from => "from" as String, to => "to" as String }
    optional { cardinality => "cardinality" as RelationCardinalityV1 }
});

define_payload_root!(ActionPayload {
    required {
        action_id => "actionId" as String,
        scope_required => "scopeRequired" as Vec<String>,
        idempotent => "idempotent" as bool,
        ledgered => "ledgered" as bool,
        input_schema => "inputSchema" as JsonObject,
        output_schema => "outputSchema" as JsonObject
    }
    optional {
        effect_program => "effectProgram" as ActionEffectProgram,
        quantity_effect => "quantityEffect" as ActionQuantityEffect,
        create_links => "createLinks" as Vec<ActionCreateLink>,
        title => "title" as String,
        description => "description" as String,
        ledger_category => "ledgerCategory" as String,
        preconditions => "preconditions" as Vec<ZenExpression>,
        slice => "slice" as Vec<SliceDeclaration>,
        derive => "derive" as Vec<ActionInputDerivation>,
        commands => "commands" as Vec<ActionCommand>,
        writes => "writes" as Vec<ActionWrite>,
        compensation => "compensation" as ActionCompensation,
        access => "access" as AccessRef,
        ui_effects => "uiEffects" as ActionUiEffects
    }
});

define_payload_root!(RulePayload {
    required { rule_id => "ruleId" as String }
    optional {
        rule_version => "ruleVersion" as String,
        input_schema => "inputSchema" as JsonObject,
        output_schema => "outputSchema" as JsonObject,
        scope_required => "scopeRequired" as Vec<String>,
        decision => "decision" as RuleDecisionV1,
        graph => "graph" as JdmGraph,
        contract => "contract" as RuleContractV2,
        triggers => "triggers" as Vec<RuleTrigger>,
        slice => "slice" as Vec<SliceDeclaration>,
        source_commands => "sourceCommands" as Vec<RuleSourceCommand>,
        defaults => "defaults" as JsonObject,
        applies_to => "appliesTo" as String,
        expression => "expression" as String,
        severity => "severity" as String,
        side_effects => "sideEffects" as JsonFalse,
        required_services => "requiredServices" as EmptyList
    }
});

define_payload_root!(ConnectorPayload {
    required {
        connector_version => "connectorVersion" as IrVersion01,
        auth => "auth" as ConnectorAuth,
        resources => "resources" as Vec<ConnectorResource>,
        mappings => "mappings" as Vec<ConnectorMapping>
    }
    optional {
        cursor => "cursor" as ConnectorCursor,
        dead_letter_policy => "deadLetterPolicy" as DeadLetterPolicy,
        commands => "commands" as Vec<ConnectorCommand>,
        supported_versions => "supportedVersions" as ConnectorSupportedVersions,
        custom_step_hook_ids => "customStepHookIds" as Vec<String>,
        custom_step_hooks => "customStepHooks" as Vec<ConnectorCustomStepHook>,
        proposal => "proposal" as ProposalMetadata
    }
});

define_payload_root!(WidgetPayload {
    required {
        widget_version => "widgetVersion" as IrVersion01,
        widget_id => "widgetId" as String,
        data_dependencies => "dataDependencies" as Vec<WidgetDataDependency>,
        view_states => "viewStates" as Vec<WidgetViewState>,
        action_bindings => "actionBindings" as Vec<WidgetActionBinding>,
        permissions => "permissions" as Vec<WidgetPermission>
    }
    optional {
        projection_hints => "projectionHints" as WidgetProjectionHints,
        proposal => "proposal" as ProposalMetadata,
        ux => "ux" as WidgetUx
    }
});

define_payload_root!(CustomizationPayload {
    required {
        customization_version => "customizationVersion" as IrVersion01,
        customization_id => "customizationId" as String,
        intent => "intent" as CustomizationIntent,
        target_artifact => "targetArtifact" as CustomizationTarget,
        overlay => "overlay" as CustomizationOverlay,
        approval => "approval" as CustomizationApproval,
        scope_required => "scopeRequired" as Vec<String>
    }
    optional { executable => "executable" as CustomizationExecutable }
});

define_payload_root!(
    // The lenient `setting` wire root. Its published JSON Schema is the strict
    // typed root `SettingRoot`, whose `value` is typed by `valueType`; the
    // per-member contract of `value` here is any JSON because it depends on
    // its sibling. The doc comment below is that schema's description.
    /// The payload of a `setting` artifact: one reviewed, effective-dated
    /// value whose JSON type is fixed by `valueType`.
    #[schemars(with = "SettingRoot")]
    SettingPayload {
    required {
        setting_version => "settingVersion" as IrVersion01,
        setting_key => "settingKey" as String,
        value_type => "valueType" as SettingValueType,
        value => "value" as JsonValue,
        effective_from => "effectiveFrom" as String,
        review => "review" as SettingReview,
        scope_required => "scopeRequired" as Vec<String>
    }
    optional {
        effective_to => "effectiveTo" as String,
        supersedes => "supersedes" as SettingSupersedes
    }
    }
);

define_payload_root!(
    // The lenient `business_pack` wire root declares the members of both pack
    // forms. Its published JSON Schema is the strict typed root
    // `BusinessPackRoot`, whose forms are told apart by `packKind`; the doc
    // comment below is that schema's description.
    /// The payload of a `business_pack` artifact: a composition pack
    /// (`packKind: "composition"`, or omitted by IR `0.1` packs; `provides`,
    /// `artifacts`) or an ontology pack (`packKind: "ontology"`; `sources`,
    /// `terms`, `mappings`, `shapes`).
    #[schemars(with = "BusinessPackRoot")]
    BusinessPackPayload {
    required { pack_id => "packId" as String, pack_version => "packVersion" as String }
    optional {
        pack_kind => "packKind" as BusinessPackKind,
        provides => "provides" as Vec<String>,
        artifacts => "artifacts" as Vec<PackArtifactRef>,
        sources => "sources" as Vec<OntologySource>,
        terms => "terms" as Vec<OntologyTerm>,
        mappings => "mappings" as Vec<OntologyMapping>,
        shapes => "shapes" as Vec<OntologyShape>
    }
}
);

define_payload_root!(
    // The lenient `policy` wire root declares the members of all three Access
    // forms. Its published JSON Schema is the strict typed root `PolicyRoot`
    // (access, obligations, or approval form); the doc comment below is that
    // schema's description.
    /// The payload of a `policy` (Access) artifact: the `AccessIR` form
    /// (`accessVersion` present), the versioned obligations form (any of
    /// `policyVersion`, `obligations`, `scopeRequired` present), or the
    /// pre-versioned approval form.
    #[schemars(with = "PolicyRoot")]
    PolicyPayload {
        required { policy_id => "policyId" as String }
        optional {
            policy_version => "policyVersion" as IrVersion01,
            scope_required => "scopeRequired" as Vec<String>,
            obligations => "obligations" as Vec<PolicyObligation>,
            applies_to => "appliesTo" as String,
            review_required_when => "reviewRequiredWhen" as String,
            approval_roles => "approvalRoles" as Vec<String>,
            access_version => "accessVersion" as IrVersion01,
            rules => "rules" as Vec<access::AccessRule>,
            generic_rules => "genericRules" as Vec<access::GenericAccessRule>
        }
    }
);

// The View root ([`ViewIrPayload`]).
define_payload_root!(
    /// The payload of a `source_schema` artifact: a discovered source schema
    /// ([`source_schema::SourceSchemaIr`], T8-SRC-1).
    #[schemars(with = "source_schema::SourceSchemaIr")]
    SourceSchemaPayload {
        required {
            format => "format" as source_schema::SourceSchemaFormat,
            source => "source" as source_schema::SourceIdentity,
            observed_at => "observedAt" as String,
            adapter => "adapter" as source_schema::SourceAdapter,
            raw_metadata => "rawMetadata" as source_schema::EvidenceRef
        }
        optional {
            datasets => "datasets" as Vec<source_schema::SourceDataset>,
            fields => "fields" as Vec<source_schema::SourceField>,
            constraints => "constraints" as Vec<source_schema::SourceConstraint>,
            profiles => "profiles" as Vec<source_schema::SourceProfile>,
            code_sets => "codeSets" as Vec<source_schema::CodeSet>,
            lineage => "lineage" as Vec<source_schema::SourceLineage>
        }
    }
);

define_payload_root!(
    /// The payload of a `mapping` artifact: an interface-scoped mapping
    /// ([`mapping::MappingIr`], T8-MAP-1a).
    #[schemars(with = "mapping::MappingIr")]
    MappingPayload {
        required {
            format => "format" as mapping::MappingFormat,
            source => "source" as mapping::SourceBinding,
            interfaces => "interfaces" as Vec<String>,
            maps => "maps" as Vec<mapping::ObjectMap>
        }
        optional {
            entitlements => "entitlements" as Vec<mapping::EntitlementMap>,
            identity_bindings => "identityBindings" as Vec<mapping::IdentityBinding>
        }
    }
);

define_payload_root!(
    /// The payload of a `law` artifact, strictly projected from LawIR.
    #[schemars(with = "LawIR")]
    LawPayload {
        required {
            law_version => "lawVersion" as IrVersion01,
            id => "id" as String,
            owner => "owner" as String,
            severity => "severity" as LawSeverity,
            rationale => "rationale" as String,
            provenance => "provenance" as LawProvenance,
            scope => "scope" as LawScope,
            subject => "subject" as String,
            modality => "modality" as LawModality,
            response => "response" as LawResponse
        }
        optional { condition => "condition" as ZenExpression, timing => "timing" as LawTiming }
    }
);

define_payload_root!(DerivedPayload {
    required {
        view_version => "viewVersion" as IrVersion01,
        view_id => "viewId" as String,
        inputs => "inputs" as Vec<ViewInputRef>,
        calc => "calc" as CalcExpr,
        output_schema => "outputSchema" as JsonObject,
        maintenance => "maintenance" as ViewMaintenance
    }
    optional {
        temporal_scope => "temporalScope" as TemporalScope,
        scope_required => "scopeRequired" as Vec<String>
    }
});

/// The closed root set for canonical artifact payloads, adjacently tagged by `kind`.
///
/// On its own the enum encodes as `{"kind": "<kind>", "payload": {..}}`. Flattened into
/// the envelope wire form that is exactly the existing `ArtifactEnvelope` shape: the
/// envelope's `kind` is the tag and its `payload` is the kind root's object, so legacy
/// envelopes keep their wire bytes and hashes. Use [`ArtifactPayload::to_json_value`]
/// for the bare payload object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", content = "payload", rename_all = "snake_case")]
pub enum ArtifactPayload {
    Entity(EntityPayload),
    Relation(RelationPayload),
    Action(ActionPayload),
    Rule(RulePayload),
    Connector(ConnectorPayload),
    Widget(WidgetPayload),
    Customization(CustomizationPayload),
    Setting(SettingPayload),
    BusinessPack(BusinessPackPayload),
    Policy(PolicyPayload),
    Derived(DerivedPayload),
    SourceSchema(SourceSchemaPayload),
    Mapping(MappingPayload),
    Law(LawPayload),
}

/// The wire payload root must be a JSON object before it can select a closed kind root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PayloadRootDecodeError;

impl std::fmt::Display for PayloadRootDecodeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("artifact payload must be a JSON object")
    }
}

impl std::error::Error for PayloadRootDecodeError {}

impl ArtifactPayload {
    pub fn from_kind_json(
        kind: &ArtifactKind,
        value: JsonValue,
    ) -> Result<Self, PayloadRootDecodeError> {
        let JsonValue::Object(object) = value else {
            return Err(PayloadRootDecodeError);
        };

        Ok(Self::from_kind_object(kind, object))
    }

    /// Construct a closed root from an already checked JSON object.
    #[must_use]
    pub fn from_kind_object(kind: &ArtifactKind, object: BTreeMap<String, JsonValue>) -> Self {
        match kind {
            ArtifactKind::Entity => Self::Entity(EntityPayload::from_json_object(object)),
            ArtifactKind::Relation => Self::Relation(RelationPayload::from_json_object(object)),
            ArtifactKind::Action => Self::Action(ActionPayload::from_json_object(object)),
            ArtifactKind::Rule => Self::Rule(RulePayload::from_json_object(object)),
            ArtifactKind::Connector => Self::Connector(ConnectorPayload::from_json_object(object)),
            ArtifactKind::Widget => Self::Widget(WidgetPayload::from_json_object(object)),
            ArtifactKind::Customization => {
                Self::Customization(CustomizationPayload::from_json_object(object))
            }
            ArtifactKind::Setting => Self::Setting(SettingPayload::from_json_object(object)),
            ArtifactKind::BusinessPack => {
                Self::BusinessPack(BusinessPackPayload::from_json_object(object))
            }
            ArtifactKind::Policy => Self::Policy(PolicyPayload::from_json_object(object)),
            ArtifactKind::Derived => Self::Derived(DerivedPayload::from_json_object(object)),
            ArtifactKind::SourceSchema => {
                Self::SourceSchema(SourceSchemaPayload::from_json_object(object))
            }
            ArtifactKind::Mapping => Self::Mapping(MappingPayload::from_json_object(object)),
            ArtifactKind::Law => Self::Law(LawPayload::from_json_object(object)),
        }
    }

    /// The artifact kind whose closed root this payload is.
    #[must_use]
    pub const fn kind(&self) -> ArtifactKind {
        match self {
            Self::Entity(_) => ArtifactKind::Entity,
            Self::Relation(_) => ArtifactKind::Relation,
            Self::Action(_) => ArtifactKind::Action,
            Self::Rule(_) => ArtifactKind::Rule,
            Self::Connector(_) => ArtifactKind::Connector,
            Self::Widget(_) => ArtifactKind::Widget,
            Self::Customization(_) => ArtifactKind::Customization,
            Self::Setting(_) => ArtifactKind::Setting,
            Self::BusinessPack(_) => ArtifactKind::BusinessPack,
            Self::Policy(_) => ArtifactKind::Policy,
            Self::Derived(_) => ArtifactKind::Derived,
            Self::SourceSchema(_) => ArtifactKind::SourceSchema,
            Self::Mapping(_) => ArtifactKind::Mapping,
            Self::Law(_) => ArtifactKind::Law,
        }
    }

    /// The bare payload object, as it appears under the envelope's `payload` key.
    #[must_use]
    pub fn to_json_value(&self) -> JsonValue {
        match self {
            Self::Entity(payload) => payload.to_json_value(),
            Self::Relation(payload) => payload.to_json_value(),
            Self::Action(payload) => payload.to_json_value(),
            Self::Rule(payload) => payload.to_json_value(),
            Self::Connector(payload) => payload.to_json_value(),
            Self::Widget(payload) => payload.to_json_value(),
            Self::Customization(payload) => payload.to_json_value(),
            Self::Setting(payload) => payload.to_json_value(),
            Self::BusinessPack(payload) => payload.to_json_value(),
            Self::Policy(payload) => payload.to_json_value(),
            Self::Derived(payload) => payload.to_json_value(),
            Self::SourceSchema(payload) => payload.to_json_value(),
            Self::Mapping(payload) => payload.to_json_value(),
            Self::Law(payload) => payload.to_json_value(),
        }
    }

    /// Top-level payload members that are not declared by the selected closed root.
    #[must_use]
    pub const fn unknown_fields(&self) -> &BTreeSet<String> {
        match self {
            Self::Entity(payload) => payload.unknown_fields(),
            Self::Relation(payload) => payload.unknown_fields(),
            Self::Action(payload) => payload.unknown_fields(),
            Self::Rule(payload) => payload.unknown_fields(),
            Self::Connector(payload) => payload.unknown_fields(),
            Self::Widget(payload) => payload.unknown_fields(),
            Self::Customization(payload) => payload.unknown_fields(),
            Self::Setting(payload) => payload.unknown_fields(),
            Self::BusinessPack(payload) => payload.unknown_fields(),
            Self::Policy(payload) => payload.unknown_fields(),
            Self::Derived(payload) => payload.unknown_fields(),
            Self::SourceSchema(payload) => payload.unknown_fields(),
            Self::Mapping(payload) => payload.unknown_fields(),
            Self::Law(payload) => payload.unknown_fields(),
        }
    }

    /// Whether the wire `extensions` member was present but not an object map.
    #[must_use]
    pub const fn has_invalid_extensions(&self) -> bool {
        match self {
            Self::Entity(payload) => payload.has_invalid_extensions(),
            Self::Relation(payload) => payload.has_invalid_extensions(),
            Self::Action(payload) => payload.has_invalid_extensions(),
            Self::Rule(payload) => payload.has_invalid_extensions(),
            Self::Connector(payload) => payload.has_invalid_extensions(),
            Self::Widget(payload) => payload.has_invalid_extensions(),
            Self::Customization(payload) => payload.has_invalid_extensions(),
            Self::Setting(payload) => payload.has_invalid_extensions(),
            Self::BusinessPack(payload) => payload.has_invalid_extensions(),
            Self::Policy(payload) => payload.has_invalid_extensions(),
            Self::Derived(payload) => payload.has_invalid_extensions(),
            Self::SourceSchema(payload) => payload.has_invalid_extensions(),
            Self::Mapping(payload) => payload.has_invalid_extensions(),
            Self::Law(payload) => payload.has_invalid_extensions(),
        }
    }

    /// Transitional JSON view for callers not yet migrated to their typed root.
    #[deprecated(
        note = "migrate callers to ArtifactPayload variants; remove after norn-engine payload access is typed"
    )]
    #[must_use]
    pub fn as_json_value(&self) -> JsonValue {
        self.to_json_value()
    }
}

impl PartialEq<JsonValue> for ArtifactPayload {
    fn eq(&self, other: &JsonValue) -> bool {
        self.to_json_value() == *other
    }
}

impl PartialEq<ArtifactPayload> for JsonValue {
    fn eq(&self, other: &ArtifactPayload) -> bool {
        *self == other.to_json_value()
    }
}

/// A canonical artifact. Decoding goes through the wire form, whose flattened
/// [`ArtifactPayload`] pairs `kind` with the matching closed payload root; the JSON Schema
/// is derived from that same wire form (one `oneOf` branch per kind).
///
/// The wire `kind` is not stored separately: it is the tag of [`Self::payload`], read with
/// [`Self::kind`], so an envelope whose kind disagrees with its payload root cannot be
/// constructed. Wire bytes and hashes are unchanged (the serializer writes the tag back as
/// `kind`).
#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
#[serde(from = "ArtifactEnvelopeWire")]
pub struct ArtifactEnvelope {
    pub ir_version: String,
    pub artifact_id: String,
    pub namespace: String,
    pub name: String,
    pub version: String,
    pub lifecycle: ArtifactLifecycle,
    pub scope: ScopeRef,
    pub dependencies: Vec<ArtifactDependency>,
    pub provenance: ArtifactProvenance,
    pub payload: ArtifactPayload,
}

impl ArtifactEnvelope {
    /// The artifact kind: the tag of its closed payload root.
    #[must_use]
    pub const fn kind(&self) -> ArtifactKind {
        self.payload.kind()
    }
}

impl Serialize for ArtifactEnvelope {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        ArtifactEnvelopeSerializer {
            ir_version: &self.ir_version,
            artifact_id: &self.artifact_id,
            kind: self.kind(),
            namespace: &self.namespace,
            name: &self.name,
            version: &self.version,
            lifecycle: &self.lifecycle,
            scope: ScopeRefWire(&self.scope),
            dependencies: &self.dependencies,
            provenance: ArtifactProvenanceWire(&self.provenance),
            // The bare kind-root object; `kind` above is the tag.
            payload: self.payload.to_json_value(),
        }
        .serialize(serializer)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ArtifactEnvelopeSerializer<'a> {
    ir_version: &'a str,
    artifact_id: &'a str,
    kind: ArtifactKind,
    namespace: &'a str,
    name: &'a str,
    version: &'a str,
    lifecycle: &'a ArtifactLifecycle,
    scope: ScopeRefWire<'a>,
    dependencies: &'a [ArtifactDependency],
    provenance: ArtifactProvenanceWire<'a>,
    payload: JsonValue,
}

struct ScopeRefWire<'a>(&'a ScopeRef);

impl Serialize for ScopeRefWire<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        // Existing authoring fixtures omit unset optional fields. Persisted snapshots historically
        // encode those fields as CBOR `null`, so retain that binary form for their stable hashes.
        if serializer.is_human_readable() {
            ScopeRefJson::from(self.0).serialize(serializer)
        } else {
            self.0.serialize(serializer)
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ScopeRefJson<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    org_id: Option<&'a String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tenant_id: Option<&'a String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    workspace_id: Option<&'a String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    environment_id: Option<&'a String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    business_unit_id: Option<&'a String>,
}

impl<'a> From<&'a ScopeRef> for ScopeRefJson<'a> {
    fn from(scope: &'a ScopeRef) -> Self {
        Self {
            org_id: scope.org_id.as_ref(),
            tenant_id: scope.tenant_id.as_ref(),
            workspace_id: scope.workspace_id.as_ref(),
            environment_id: scope.environment_id.as_ref(),
            business_unit_id: scope.business_unit_id.as_ref(),
        }
    }
}

struct ArtifactProvenanceWire<'a>(&'a ArtifactProvenance);

impl Serialize for ArtifactProvenanceWire<'_> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        if serializer.is_human_readable() {
            ArtifactProvenanceJson::from(self.0).serialize(serializer)
        } else {
            self.0.serialize(serializer)
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ArtifactProvenanceJson<'a> {
    source: &'a ProvenanceSource,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_path: Option<&'a String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    actor_id: Option<&'a String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    created_at: Option<&'a String>,
}

impl<'a> From<&'a ArtifactProvenance> for ArtifactProvenanceJson<'a> {
    fn from(provenance: &'a ArtifactProvenance) -> Self {
        Self {
            source: &provenance.source,
            source_path: provenance.source_path.as_ref(),
            actor_id: provenance.actor_id.as_ref(),
            created_at: provenance.created_at.as_ref(),
        }
    }
}

#[derive(JsonSchema)]
#[serde(rename_all = "camelCase")]
#[schemars(rename = "ArtifactEnvelope")]
struct ArtifactEnvelopeWire {
    ir_version: String,
    artifact_id: String,
    namespace: String,
    name: String,
    version: String,
    lifecycle: ArtifactLifecycle,
    scope: ScopeRef,
    dependencies: Vec<ArtifactDependency>,
    provenance: ArtifactProvenance,
    /// `kind` plus the matching closed `payload` root (adjacently tagged).
    #[serde(flatten)]
    payload: ArtifactPayload,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ArtifactEnvelopeWireDecoded {
    ir_version: String,
    artifact_id: String,
    namespace: String,
    name: String,
    version: String,
    lifecycle: ArtifactLifecycle,
    scope: ScopeRef,
    dependencies: Vec<ArtifactDependency>,
    provenance: ArtifactProvenance,
    #[serde(flatten)]
    payload: ArtifactPayload,
}

impl From<ArtifactEnvelopeWireDecoded> for ArtifactEnvelopeWire {
    fn from(wire: ArtifactEnvelopeWireDecoded) -> Self {
        Self {
            ir_version: wire.ir_version,
            artifact_id: wire.artifact_id,
            namespace: wire.namespace,
            name: wire.name,
            version: wire.version,
            lifecycle: wire.lifecycle,
            scope: wire.scope,
            dependencies: wire.dependencies,
            provenance: wire.provenance,
            payload: wire.payload,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ArtifactEnvelopeWidgetProbe {
    kind: ArtifactKind,
    payload: Box<RawValue>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WidgetPayloadUxProbe {
    ux: Option<Box<RawValue>>,
}

impl<'de> Deserialize<'de> for ArtifactEnvelopeWire {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let deserializer_type = std::any::type_name::<D>();
        if deserializer_type.contains("serde_json::value::Value") {
            let value = serde_json::Value::deserialize(deserializer)?;
            if value.get("kind").and_then(serde_json::Value::as_str) == Some("widget")
                && let Some(ux) = value.get("payload").and_then(|payload| payload.get("ux"))
            {
                crate::widget_ux::guard_widget_ux_numeric_value(ux)
                    .map_err(serde::de::Error::custom)?;
            }

            let decoded: ArtifactEnvelopeWireDecoded =
                serde_json::from_value(value).map_err(serde::de::Error::custom)?;
            return Ok(Self::from(decoded));
        }

        let is_json_text = deserializer_type.contains("serde_json::de::Deserializer<")
            && deserializer.is_human_readable();
        if !is_json_text {
            return ArtifactEnvelopeWireDecoded::deserialize(deserializer).map(Self::from);
        }

        // Capture the complete envelope before serde's flattened payload path
        // buffers members through JsonValue's f64 number representation.
        let raw = Box::<RawValue>::deserialize(deserializer)?;
        let probe: ArtifactEnvelopeWidgetProbe =
            serde_json::from_str(raw.get()).map_err(serde::de::Error::custom)?;
        if probe.kind == ArtifactKind::Widget {
            let payload: WidgetPayloadUxProbe =
                serde_json::from_str(probe.payload.get()).map_err(serde::de::Error::custom)?;
            if let Some(ux) = payload.ux {
                crate::widget_ux::guard_widget_ux_numeric_roundtrip(ux.get())
                    .map_err(serde::de::Error::custom)?;
            }
        }

        let decoded: ArtifactEnvelopeWireDecoded =
            serde_json::from_str(raw.get()).map_err(serde::de::Error::custom)?;
        Ok(Self::from(decoded))
    }
}

impl From<ArtifactEnvelopeWire> for ArtifactEnvelope {
    fn from(wire: ArtifactEnvelopeWire) -> Self {
        Self {
            ir_version: wire.ir_version,
            artifact_id: wire.artifact_id,
            namespace: wire.namespace,
            name: wire.name,
            version: wire.version,
            lifecycle: wire.lifecycle,
            scope: wire.scope,
            dependencies: wire.dependencies,
            provenance: wire.provenance,
            payload: wire.payload,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Info,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticRemediation {
    FixDefinition,
    UpgradeIr,
    ResolveDependency,
    CheckScope,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostic {
    pub code: String,
    pub severity: DiagnosticSeverity,
    pub message: String,
    pub artifact_id: Option<String>,
    pub path: Option<String>,
    pub remediation: Option<DiagnosticRemediation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GraphTraversalDirection {
    Outbound,
    Inbound,
    Either,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GraphRelationShape {
    pub relation_kind: String,
    pub direction: GraphTraversalDirection,
    pub target_object_kind: String,
    pub fields: Vec<String>,
    pub include_relation_metadata: Option<bool>,
    pub limit: u32,
    #[serde(default)]
    pub relations: Vec<GraphRelationShape>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GraphReadShape {
    pub root_object_kind: String,
    pub root_ids: Vec<String>,
    pub fields: Vec<String>,
    pub relations: Vec<GraphRelationShape>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GraphQueryBudget {
    pub max_depth: u32,
    pub max_root_objects: u32,
    pub max_relation_shapes: u32,
    pub max_relation_batches: u32,
    pub max_rows: u64,
    pub allow_higher_risk_policy: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GraphLoadRequest {
    pub request_id: String,
    pub correlation_id: String,
    pub scope: ScopeRef,
    pub scope_required: Vec<String>,
    pub shape: GraphReadShape,
    pub budget: GraphQueryBudget,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RelationBatchPlan {
    pub batch_id: String,
    pub relation_kind: String,
    pub direction: GraphTraversalDirection,
    pub source_object_kind: String,
    pub target_object_kind: String,
    pub depth: u32,
    pub limit: u32,
    pub source_identity_keys: Vec<String>,
    pub scope_pushdown: ScopeRef,
    pub include_relation_metadata: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GraphLoadEvidence {
    pub correlation_id: String,
    pub depth: u32,
    pub relation_shape_count: u32,
    pub relation_batch_count: u32,
    pub estimated_rows: u64,
    pub budget: GraphQueryBudget,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GraphLoadPlan {
    pub request_id: String,
    pub correlation_id: String,
    pub root_identity_keys: Vec<String>,
    pub relation_batches: Vec<RelationBatchPlan>,
    pub evidence: GraphLoadEvidence,
    pub diagnostics: Vec<Diagnostic>,
    pub can_execute: bool,
}

/// Emit the envelope schema with `kind` and `payload` paired in each branch.
///
/// Derived from the envelope wire form: the flattened, adjacently tagged
/// [`ArtifactPayload`] yields one `oneOf` branch per kind, so a cross-kind envelope
/// cannot validate. Every declared payload member carries its typed contract (T1-IR-2);
/// the reserved `extensions` slot is the only generic JSON member.
#[must_use]
pub fn artifact_envelope_schema() -> Schema {
    schema_for!(ArtifactEnvelope)
}

/// Emit the JSON Schema for the canonical resource-limit profile reference.
///
/// This small schema is also used by check receipts, so consumers can project
/// it without restating the IR type.
#[must_use]
pub fn limit_profile_ref_schema() -> Schema {
    schema_for!(LimitProfileRef)
}

/// Emit the JSON Schema for the canonical diagnostic severity.
#[must_use]
pub fn diagnostic_severity_schema() -> Schema {
    schema_for!(DiagnosticSeverity)
}

/// Emit the JSON Schema for the canonical diagnostic remediation class.
#[must_use]
pub fn diagnostic_remediation_schema() -> Schema {
    schema_for!(DiagnosticRemediation)
}

/// JSON Schema for the versioned editor-layout sidecar.
#[must_use]
pub fn layout_sidecar_schema() -> Schema {
    schema_for!(LayoutSidecar)
}

/// Emit the JSON Schema for the closed payload root selected by an envelope kind.
///
/// The wire `kind` is the discriminator ([`ArtifactEnvelope::kind`]), so callers that need
/// a per-kind contract select the schema with the same kind value.
#[must_use]
pub fn artifact_payload_schema(kind: &ArtifactKind) -> Schema {
    match kind {
        ArtifactKind::Entity => schema_for!(EntityPayload),
        ArtifactKind::Relation => schema_for!(RelationPayload),
        ArtifactKind::Action => schema_for!(ActionPayload),
        ArtifactKind::Rule => schema_for!(RulePayload),
        ArtifactKind::Connector => schema_for!(ConnectorPayload),
        ArtifactKind::Widget => schema_for!(WidgetPayload),
        ArtifactKind::Customization => schema_for!(CustomizationPayload),
        ArtifactKind::Setting => schema_for!(SettingPayload),
        ArtifactKind::BusinessPack => schema_for!(BusinessPackPayload),
        ArtifactKind::Policy => schema_for!(PolicyPayload),
        ArtifactKind::Derived => schema_for!(DerivedPayload),
        ArtifactKind::SourceSchema => schema_for!(SourceSchemaPayload),
        ArtifactKind::Mapping => schema_for!(MappingPayload),
        ArtifactKind::Law => schema_for!(LawPayload),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// #2440: generic IR JSON decodes to the same value from bytes, from a
    /// `serde_json::Value`, and through the flattened envelope, whether or not
    /// `serde_json/arbitrary_precision` is enabled (CI runs both).
    #[test]
    fn json_value_numbers_decode_independently_of_serde_json_features() {
        let source = r#"{"a":1.50,"b":1e2,"c":-0,"d":9007199254740993,"e":[0.1,-7],"f":{"g":3}}"#;
        let expected = JsonValue::Object(BTreeMap::from([
            ("a".to_owned(), JsonValue::Number(1.5)),
            ("b".to_owned(), JsonValue::Number(100.0)),
            ("c".to_owned(), JsonValue::Number(0.0)),
            ("d".to_owned(), JsonValue::Number(9_007_199_254_740_992.0)),
            (
                "e".to_owned(),
                JsonValue::Array(vec![JsonValue::Number(0.1), JsonValue::Number(-7.0)]),
            ),
            (
                "f".to_owned(),
                JsonValue::Object(BTreeMap::from([("g".to_owned(), JsonValue::Number(3.0))])),
            ),
        ]));
        let from_bytes: JsonValue = serde_json::from_str(source).expect("bytes decode");
        let value: serde_json::Value = serde_json::from_str(source).expect("value decode");
        let from_value: JsonValue = serde_json::from_value(value).expect("from_value decode");
        let in_memory: JsonValue = json_tree_from_value(json!({
            "a": 1.5, "b": 100.0, "c": -0.0, "d": 9_007_199_254_740_992_u64,
            "e": [0.1, -7], "f": { "g": 3 }
        }))
        .expect("in-memory decode");
        assert_eq!(in_memory, expected);
        assert_eq!(from_bytes, expected);
        assert_eq!(from_value, expected);
        let JsonValue::Number(zero) = &from_bytes.as_object().expect("object")["c"] else {
            panic!("c must be a number");
        };
        assert!(zero.is_sign_positive(), "negative zero collapses to 0");
        assert_eq!(
            serde_json::to_string(&from_bytes).expect("serialize"),
            r#"{"a":1.5,"b":100.0,"c":0.0,"d":9007199254740992.0,"e":[0.1,-7.0],"f":{"g":3.0}}"#
        );
        assert!(serde_json::from_str::<JsonValue>("1e400").is_err());
    }

    /// #2440 review: a constructed near-halfway float keeps its bits through
    /// `to_value` -> `JsonValue` and through `JsonValueDeserializer`, under
    /// both `serde_json` profiles (CI runs both).
    #[test]
    fn constructed_floats_keep_their_bits_under_both_serde_json_profiles() {
        use serde::de::IntoDeserializer;

        for value in [
            9.1417e60,
            7_841_115_271_120_439.0,
            -3_618_697_969_859_606.0,
            0.1 + 0.2,
        ] {
            let source = JsonValue::Array(vec![JsonValue::Number(value)]);
            let serialized = serde_json::to_value(&source).expect("serialize");
            let via_from_value: JsonValue =
                serde_json::from_value(serialized.clone()).expect("from_value decode");
            assert_eq!(via_from_value, source, "{value:e}");
            let decoded: JsonValue = json_tree_from_value(serialized).expect("in-memory decode");
            let JsonValue::Array(items) = &decoded else {
                panic!("array expected");
            };
            let [JsonValue::Number(number)] = items.as_slice() else {
                panic!("one number expected");
            };
            assert_eq!(number.to_bits(), value.to_bits(), "{value:e}");

            let typed: Vec<f64> =
                serde::Deserialize::deserialize(
                    IntoDeserializer::<serde::de::value::Error>::into_deserializer(source),
                )
                .expect("typed decode");
            assert_eq!(typed[0].to_bits(), value.to_bits(), "{value:e}");
        }
    }

    fn provenance() -> ArtifactProvenance {
        ArtifactProvenance {
            source: ProvenanceSource::AuthoringSdk,
            source_path: None,
            actor_id: None,
            created_at: None,
        }
    }

    fn entity_payload() -> JsonValue {
        // Minimal valid entity payload: `fields` is required and must be an object.
        let mut fields_map: BTreeMap<String, JsonValue> = BTreeMap::new();
        let mut field_def: BTreeMap<String, JsonValue> = BTreeMap::new();
        field_def.insert("type".to_string(), JsonValue::String("text".to_string()));
        field_def.insert("required".to_string(), JsonValue::Bool(true));
        fields_map.insert("id".to_string(), JsonValue::Object(field_def));
        let mut payload: BTreeMap<String, JsonValue> = BTreeMap::new();
        payload.insert(
            "objectKind".to_string(),
            JsonValue::String("commerce.Product".to_string()),
        );
        payload.insert("fields".to_string(), JsonValue::Object(fields_map));
        JsonValue::Object(payload)
    }

    fn entity_artifact() -> ArtifactEnvelope {
        ArtifactEnvelope {
            ir_version: SUPPORTED_IR_VERSION.to_string(),
            artifact_id: "commerce.entity.product".to_string(),
            namespace: "commerce".to_string(),
            name: "Product".to_string(),
            version: "0.1.0".to_string(),
            lifecycle: ArtifactLifecycle::Draft,
            scope: ScopeRef::default(),
            dependencies: Vec::new(),
            provenance: provenance(),
            payload: ArtifactPayload::from_kind_json(&ArtifactKind::Entity, entity_payload())
                .expect("entity payload is an object"),
        }
    }

    #[test]
    #[allow(
        deprecated,
        reason = "the test explicitly covers the transitional compatibility accessor"
    )]
    fn entity_payload_compatibility_view_tracks_typed_field_mutations() {
        let mut artifact = entity_artifact();
        let ArtifactPayload::Entity(payload) = &mut artifact.payload else {
            panic!("entity artifact should select EntityPayload");
        };
        let mutation_authority = JsonValue::Object(BTreeMap::from([
            (
                "kind".to_string(),
                JsonValue::String("domain_engine".to_string()),
            ),
            (
                "domain".to_string(),
                JsonValue::String("inventory".to_string()),
            ),
        ]));
        payload.mutation_authority = PayloadField::present(mutation_authority.clone());

        let compatibility_view = artifact.payload.as_json_value();
        assert_eq!(
            compatibility_view
                .as_object()
                .and_then(|payload| payload.get("mutationAuthority")),
            Some(&mutation_authority),
        );

        let serialized = serde_json::to_value(&artifact.payload)
            .expect("typed payload should serialize through its current fields");
        assert_eq!(
            serialized["payload"]["mutationAuthority"],
            serde_json::json!({
                "kind": "domain_engine",
                "domain": "inventory"
            })
        );
    }

    #[test]
    fn rejects_non_object_payloads_during_envelope_decode() {
        let mut source =
            serde_json::to_value(entity_artifact()).expect("entity artifact should serialize");
        source["payload"] = serde_json::json!("not-an-object");

        assert!(serde_json::from_value::<ArtifactEnvelope>(source).is_err());
    }

    #[test]
    fn serializes_with_ts_compatible_field_names() {
        let artifact = ArtifactEnvelope {
            ir_version: SUPPORTED_IR_VERSION.to_string(),
            artifact_id: "commerce.entity.product".to_string(),
            namespace: "commerce".to_string(),
            name: "Product".to_string(),
            version: "0.1.0".to_string(),
            lifecycle: ArtifactLifecycle::Draft,
            scope: ScopeRef::default(),
            dependencies: Vec::new(),
            provenance: provenance(),
            payload: ArtifactPayload::from_kind_json(
                &ArtifactKind::Entity,
                JsonValue::Object(BTreeMap::new()),
            )
            .expect("entity payload is an object"),
        };

        let serialized = serde_json::to_value(artifact).expect("artifact should serialize");

        assert_eq!(serialized["irVersion"], "0.1");
        assert_eq!(serialized["artifactId"], "commerce.entity.product");
        assert_eq!(serialized["kind"], "entity");
    }

    #[test]
    fn exposes_artifact_json_schema() {
        let schema = artifact_envelope_schema();
        let schema_json = serde_json::to_value(schema).expect("schema should serialize");

        assert!(schema_json.is_object());
    }

    fn artifact_with_payload(kind: &ArtifactKind, payload: serde_json::Value) -> ArtifactEnvelope {
        let payload: JsonValue =
            json_tree_from_value(payload).expect("payload should decode to JsonValue");
        let payload =
            ArtifactPayload::from_kind_json(kind, payload).expect("test payload is an object");
        ArtifactEnvelope {
            ir_version: SUPPORTED_IR_VERSION.to_string(),
            artifact_id: "commerce.artifact.demo".to_string(),
            namespace: "commerce".to_string(),
            name: "Demo".to_string(),
            version: "0.1.0".to_string(),
            lifecycle: ArtifactLifecycle::Draft,
            scope: ScopeRef::default(),
            dependencies: Vec::new(),
            provenance: provenance(),
            payload,
        }
    }

    fn valid_derived_payload() -> serde_json::Value {
        json!({
            "viewVersion": "0.1",
            "viewId": "commerce.view.open_invoices",
            "inputs": [
                {
                    "artifactId": "commerce.entity.invoice",
                    "kind": "entity",
                    "version": "0.1.0",
                    "alias": "invoice"
                }
            ],
            "calc": { "kind": "field_projection", "fields": ["id"] },
            "outputSchema": { "type": "array" },
            "maintenance": "recompute_on_read"
        })
    }

    #[test]
    fn serializes_derived_kind_as_snake_case() {
        let artifact = artifact_with_payload(&ArtifactKind::Derived, valid_derived_payload());
        let serialized = serde_json::to_value(&artifact).expect("should serialize");
        assert_eq!(serialized["kind"], "derived");
    }

    #[test]
    fn roundtrip_derived_view_ir_payload() {
        // The typed View root decodes the validator-accepted payload and re-encodes it unchanged.
        let source = valid_derived_payload();
        let payload: ViewIrPayload =
            serde_json::from_value(source.clone()).expect("should deserialize");
        assert_eq!(payload.view_id, "commerce.view.open_invoices");
        assert_eq!(payload.maintenance, ViewMaintenance::RecomputeOnRead);
        assert_eq!(payload.inputs[0].kind, ViewInputKind::Entity);
        assert!(matches!(payload.calc, CalcExpr::FieldProjection { .. }));

        let serialized = serde_json::to_value(&payload).expect("should serialize");
        assert_eq!(serialized, source);
    }
}
