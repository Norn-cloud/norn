//! Declarative BI contracts (#3301, slice 1). No query or evaluation runtime.
//!
//! Paths are relative to each measure's object type. Units are inferred from
//! effective property types; money binds the separate ontology currency property.

use schemars::{JsonSchema, Schema, schema_for};
use serde::{Deserialize, Serialize};

use crate::members::present_non_null;
use crate::{
    IrVersion01, LinkId, ObjectTypeName, PropertyId, RuntimeValue, UtcTimestamp, WholeNumber,
};

/// A property reached by zero or more declared directional link roles.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DimensionPath {
    /// Forward or inverse roles, in traversal order; empty means the root object.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<LinkId>,
    /// The terminal property.
    pub property: PropertyId,
}

/// A MetricFlow definition carried by the OSI DBT extension.
/// Norn-specific policy is isolated in `x-norn`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MetricDefinition {
    /// Stable metric identifier.
    pub name: String,
    /// Human-readable MetricFlow label.
    pub label: String,
    /// MetricFlow metric type.
    #[serde(rename = "type")]
    pub metric_type: MetricType,
    /// Closure-free MetricFlow calculation parameters.
    pub type_params: MetricTypeParams,
    /// Policies which do not belong to OSI or MetricFlow.
    #[serde(rename = "x-norn")]
    pub norn: MetricNorn,
}

/// Standard MetricFlow metric metadata, without Norn policy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MetricFlowMetric {
    /// Metric name.
    pub name: String,
    /// Display label.
    pub label: String,
    /// Calculation type.
    #[serde(rename = "type")]
    pub metric_type: MetricType,
    /// Calculation parameters.
    pub type_params: MetricTypeParams,
    /// Standard MetricFlow filter text, projected from exact bound predicates.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub filter: Option<String>,
}

/// MetricFlow's five metric types; conversion means cohort conversion, not FX.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MetricType {
    /// Aggregate a measure.
    Simple,
    /// Divide two bucket metrics.
    Ratio,
    /// Accumulate a metric over a window.
    Cumulative,
    /// Expression over named input metrics.
    Derived,
    /// Cohort conversion calculation.
    Conversion,
}

/// MetricFlow calculation parameters. The checker enforces the fields for each type.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MetricTypeParams {
    /// Stored property for a simple metric, or derived expression.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub expr: Option<String>,
    /// Modern MetricFlow aggregate specification.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "MetricAggregationParams")]
    pub metric_aggregation_params: Option<MetricAggregationParams>,
    /// Numerator metric.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub numerator: Option<String>,
    /// Denominator metric.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub denominator: Option<String>,
    /// Derived metric inputs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub metrics: Vec<MetricInput>,
    /// Cumulative parameters.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "CumulativeTypeParams")]
    pub cumulative_type_params: Option<CumulativeTypeParams>,
    /// Conversion parameters.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "ConversionTypeParams")]
    pub conversion_type_params: Option<ConversionTypeParams>,
}

/// Aggregate parameters use MetricFlow field names.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MetricAggregationParams {
    /// Ontology-bound semantic model.
    pub semantic_model: ObjectTypeName,
    /// Aggregate operator.
    pub agg: MeasureAggregate,
    /// Generated scalar field name; directional ontology paths bind in `x-norn`.
    pub agg_time_dimension: String,
    /// Snapshot dimension and series groupings.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "NonAdditiveDimension")]
    pub non_additive_dimension: Option<NonAdditiveDimension>,
}

/// MetricFlow snapshot metadata. Admission does not promise snapshot evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NonAdditiveDimension {
    /// Time dimension.
    pub name: String,
    /// Choose the earliest/latest timestamp, rather than a min/max amount.
    pub window_choice: WindowChoice,
    /// Snapshot series keys.
    #[serde(default)]
    pub window_groupings: Vec<String>,
}

/// MetricFlow timestamp selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WindowChoice {
    /// Earliest timestamp.
    Min,
    /// Latest timestamp.
    Max,
}

/// Named metric input in a derived calculation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MetricInput {
    /// Metric name.
    pub name: String,
    /// Expression alias.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub alias: Option<String>,
}

/// MetricFlow cumulative window parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CumulativeTypeParams {
    /// Underlying metric.
    pub metric: String,
    /// Rolling window, e.g. `7 days`.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub window: Option<String>,
    /// Calendar grain to date.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "TimeGrain")]
    pub grain_to_date: Option<TimeGrain>,
}

/// MetricFlow cohort conversion parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConversionTypeParams {
    /// Base population metric.
    pub base_metric: String,
    /// Converted population metric.
    pub conversion_metric: String,
    /// Join entity.
    pub entity: String,
    /// Allowed conversion interval.
    pub window: String,
    /// Conversion count or rate.
    pub calculation: ConversionCalculation,
}

/// Conversion outputs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConversionCalculation {
    /// Count converted entities.
    Conversions,
    /// Converted entities divided by base entities.
    ConversionRate,
}

/// Norn policy extension. Units are derived from typed properties, never asserted here.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MetricNorn {
    /// Norn extension version.
    pub ir_version: IrVersion01,
    /// Conservative scalar roll-up policy.
    pub additivity: MetricAdditivity,
    /// Time binding for composite metrics; simple metrics use agg_time_dimension.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "DimensionPath")]
    pub time_dimension: Option<DimensionPath>,
    /// Exact predicates applied before aggregation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub default_filters: Vec<MetricFilter>,
    /// Optional FX conversion; authoritative currency comes from the amount type.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "MetricCurrency")]
    pub currency: Option<MetricCurrency>,
    /// Identity deduplication within each bucket.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "SafeAggregation")]
    pub safe_aggregation: Option<SafeAggregation>,
    /// Accountable owner.
    pub owner: String,
    /// Certification does not grant access.
    pub certified: bool,
    /// Explicit authorization and aggregate disclosure requirements.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "MetricAuthorization")]
    pub authz: Option<MetricAuthorization>,
    /// Definition freshness requirement.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "FreshnessPolicy")]
    pub freshness: Option<FreshnessPolicy>,
    /// Generated inspection metadata, never an author assertion.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "MetricPropertySemantics")]
    pub derived: Option<MetricPropertySemantics>,
    /// Maintenance hint only; no incremental engine is selected.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "IncrementalHint")]
    pub incremental: Option<IncrementalHint>,
}

/// Generated type semantics for inspecting a standalone metric file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MetricPropertySemantics {
    /// Quantity unit of the underlying property; counts and ratios are dimensionless.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "crate::UnitRef")]
    pub unit: Option<crate::UnitRef>,
    /// Authoritative source attached to a monetary decimal type.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "DimensionPath")]
    pub currency_source: Option<DimensionPath>,
}

/// Authorization remains a runtime boundary; checker admission never grants access.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MetricAuthorization {
    /// Named authorization action evaluated for the object population.
    pub action: String,
    /// Minimum distinct objects allowed in disclosed buckets.
    pub min_group_size: WholeNumber,
}

/// Declarative incremental hint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum IncrementalHint {
    /// Recompute from the authorized object population.
    Recompute,
    /// Request maintenance from cell-ledger changes.
    LedgerDelta,
}

/// Base aggregate operators. Count without a property counts objects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MeasureAggregate {
    /// Exact total.
    Sum,
    /// Count objects or non-null property values.
    Count,
    /// Minimum value.
    Min,
    /// Maximum value.
    Max,
    /// Arithmetic mean.
    #[serde(rename = "average")]
    Avg,
    /// Count distinct property values.
    CountDistinct,
}

/// Operators over already aggregated results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Reaggregate {
    /// Total across admitted axes.
    Sum,
    /// First value in time order.
    First,
    /// Last value in time order.
    Last,
    /// Minimum.
    Min,
    /// Maximum.
    Max,
}

/// The axis on which a metric is semi-additive.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AdditivityAxis {
    /// The declared time dimension.
    Time,
}

/// Algebraic contract for re-aggregation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MetricAdditivity {
    /// Sum across any dimension, including time.
    Additive,
    /// Sum across non-time axes; use the declared reduction along time.
    SemiAdditive {
        /// Restricted axis.
        over: AdditivityAxis,
        /// One of first/last/min/max; sum is rejected by the checker.
        aggregate: Reaggregate,
    },
    /// Must be computed from underlying objects for each requested bucket.
    NonAdditive,
}

/// A safe strategy for a to-many dimension path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SafeAggregation {
    /// Deduplicate measure object identities within each output bucket, including
    /// combinations of multiple fan-out paths. Never deduplicate by measure value.
    Symmetric,
}

/// Currency binding; the amount itself remains an ontology decimal property.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MetricCurrency {
    /// ISO 4217 registry property or a closed currency-code enum.
    pub property: DimensionPath,
    /// Required when summing a source whose currency is not a singleton enum.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "CurrencyConversion")]
    pub conversion: Option<CurrencyConversion>,
}

/// Explicit source-to-target conversion, before any aggregation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CurrencyConversion {
    /// Target ISO 4217 code.
    pub target: String,
    /// Dimensionless decimal rate (target units per source unit) at measure grain.
    pub rate: DimensionPath,
}

/// Comparison operators for exact typed literals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FilterOperator {
    /// Equality.
    Eq,
    /// Inequality.
    Ne,
    /// Less than.
    Lt,
    /// Less than or equal.
    Le,
    /// Greater than.
    Gt,
    /// Greater than or equal.
    Ge,
}

/// A predicate over a resolved property; numeric values retain exact decimals.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MetricFilter {
    /// Relative property path. To-many filters are existential predicates.
    pub path: DimensionPath,
    /// Operator compatible with the property's type.
    pub operator: FilterOperator,
    /// Exact runtime literal checked against the effective property contract.
    pub value: RuntimeValue,
}

/// Calendar semantics, independent of the query window. Future calendar
/// systems require new explicit variants; existing boundaries never change.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CalendarPolicy {
    /// Gregorian UTC buckets; ISO weeks start Monday. Dates mean UTC midnight.
    #[default]
    IsoUtc,
}

/// Calendar bucket grain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TimeGrain {
    /// Day.
    Day,
    /// Week.
    Week,
    /// Month.
    Month,
    /// Quarter.
    Quarter,
    /// Year.
    Year,
}

/// One requested group-by dimension.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuestionDimension {
    /// Link path ending in a scalar property.
    pub path: DimensionPath,
    /// Calendar bucket; admitted only for date/timestamp properties.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "TimeGrain")]
    pub grain: Option<TimeGrain>,
}

/// Explicit roll-up of previously aggregated results.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MetricRollup {
    /// Reduction operator.
    pub aggregate: Reaggregate,
    /// Requested dimensions being collapsed.
    pub across: Vec<DimensionPath>,
    /// Also collapse the metric's time axis (even if not in dimensions).
    pub over_time: bool,
}

/// A selected metric, optionally with an explicit re-aggregation request.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuestionMetric {
    /// Metric name.
    pub metric: String,
    /// Inline calculation at the terminal population; metric names its output.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "InlineAggregation")]
    pub inline: Option<InlineAggregation>,
    /// Omitted means aggregate underlying objects at requested grain. Snapshot
    /// metrics are refused until series and ordered reduction semantics exist.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "MetricRollup")]
    pub reaggregate: Option<MetricRollup>,
}

/// Half-open UTC time window. Calendar bucketing is a separate concern.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuestionTimeWindow {
    /// Inclusive UTC instant.
    pub start: UtcTimestamp,
    /// Exclusive UTC instant.
    pub end: UtcTimestamp,
}

/// One explicit actual-to-reference metric alignment. A mapping is complete
/// and bijective; dimensions use identical paths and windows must agree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComparisonMetricMapping {
    /// Metric selected in the actual question.
    pub metric: String,
    /// Compatible metric selected in the reference question.
    pub reference_metric: String,
}

/// A comparison series, without routing to any execution engine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum QuestionComparison {
    /// Immediately preceding window of the same duration.
    PreviousPeriod,
    /// Another checked question providing planned values.
    Plan {
        /// Question name.
        question: String,
        /// Empty means identity mapping; otherwise map every selected metric.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        mapping: Vec<ComparisonMetricMapping>,
    },
    /// Another checked question providing forecast values.
    Forecast {
        /// Question name.
        question: String,
        /// Empty means identity mapping; otherwise map every selected metric.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        mapping: Vec<ComparisonMetricMapping>,
    },
}

/// Declarative freshness requirement; it chooses no runtime in this slice.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum FreshnessPolicy {
    /// Live series with a bounded age in milliseconds.
    Live {
        /// Positive age bound.
        max_age_ms: WholeNumber,
    },
    /// Delayed series with a bounded age in milliseconds.
    Delayed {
        /// Positive age bound.
        max_age_ms: WholeNumber,
    },
    /// Reproducible historical view.
    AsOf {
        /// UTC instant.
        at: UtcTimestamp,
    },
}

/// A serializable analytical question over checked metrics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct QuestionIR {
    /// Contract version.
    pub ir_version: IrVersion01,
    /// Unique dotted lower-case name.
    pub name: String,
    /// Starting object population.
    pub object_set: ObjectTypeName,
    /// Link traversal producing a deduplicated terminal object population.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub search_around: Vec<LinkId>,
    /// Non-empty selected metrics.
    #[serde(rename = "aggregate")]
    pub metrics: Vec<QuestionMetric>,
    /// Relative paths resolved for every underlying measure.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[serde(rename = "groupBy")]
    pub dimensions: Vec<QuestionDimension>,
    /// Additional predicates.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[serde(rename = "filter")]
    pub filters: Vec<MetricFilter>,
    /// Half-open UTC interval.
    #[serde(rename = "window")]
    pub time_window: QuestionTimeWindow,
    /// Explicit bucket calendar; omitted means immutable ISO/Gregorian UTC.
    #[serde(default)]
    pub calendar: CalendarPolicy,
    /// Optional comparison; every named question is resolved and cycle-checked.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "QuestionComparison")]
    #[serde(rename = "compare")]
    pub comparison: Option<QuestionComparison>,
    /// Freshness requirement.
    pub freshness: FreshnessPolicy,
}

/// JSON Schema for the standalone metric vocabulary.
#[must_use]
pub fn metric_ir_schema() -> Schema {
    schema_for!(crate::osi::MetricIR)
}

/// JSON Schema for the standalone question vocabulary.
#[must_use]
pub fn question_ir_schema() -> Schema {
    schema_for!(QuestionIR)
}

/// Inline aggregate; lowered through the same definition checker as named metrics.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InlineAggregation {
    /// Unique selection name, usable by aggregate/compare.
    pub name: String,
    /// Aggregate operator.
    pub agg: MeasureAggregate,
    /// Stored property, omitted only for object count.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "PropertyId")]
    pub property: Option<PropertyId>,
    /// Time dimension at the terminal population.
    pub agg_time_dimension: DimensionPath,
    /// Conservative governance; the same rules apply as named metrics.
    #[serde(rename = "x-norn")]
    pub norn: MetricNorn,
}

/// MetricFlow's measure-based semantic model import surface.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MetricFlowSemanticModel {
    /// Semantic model name.
    pub name: String,
    /// Physical model reference, mapped to an ontology binding rather than executed.
    pub model: String,
    /// Default aggregation time dimension.
    #[serde(default)]
    pub defaults: MetricFlowDefaults,
    /// Primary, unique and foreign join entities.
    #[serde(default)]
    pub entities: Vec<MetricFlowEntity>,
    /// Grouping and temporal dimensions.
    #[serde(default)]
    pub dimensions: Vec<MetricFlowDimension>,
    /// Stored-property measures.
    #[serde(default)]
    pub measures: Vec<MetricFlowMeasure>,
}

/// MetricFlow semantic model defaults.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MetricFlowDefaults {
    /// Time dimension used unless a measure overrides it.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub agg_time_dimension: Option<String>,
}

/// A MetricFlow measure imported with an ontology field binding.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MetricFlowMeasure {
    /// Measure name.
    pub name: String,
    /// Aggregate.
    pub agg: MeasureAggregate,
    /// Scalar expression, admitted only with a matching ontology binding.
    pub expr: String,
    /// Time dimension override.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub agg_time_dimension: Option<String>,
    /// Snapshot time and series keys.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "NonAdditiveDimension")]
    pub non_additive_dimension: Option<NonAdditiveDimension>,
}

/// Join entity, never treated as an unchecked uniqueness assertion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MetricFlowEntity {
    /// Join entity name.
    pub name: String,
    /// Join cardinality role.
    #[serde(rename = "type")]
    pub entity_type: MetricFlowEntityType,
    /// Bound key or link role expression.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub expr: Option<String>,
}

/// MetricFlow entity types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MetricFlowEntityType {
    /// Preferred row identity.
    Primary,
    /// Unique key.
    Unique,
    /// Non-unique foreign key.
    Foreign,
    /// Natural business identity.
    Natural,
}

/// MetricFlow grouping dimension.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MetricFlowDimension {
    /// Dimension name.
    pub name: String,
    /// Categorical or time.
    #[serde(rename = "type")]
    pub dimension_type: MetricFlowDimensionType,
    /// Bound ontology field expression.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub expr: Option<String>,
    /// Time granularity for temporal dimensions.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "MetricFlowDimensionParams")]
    pub type_params: Option<MetricFlowDimensionParams>,
}

/// MetricFlow dimension categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MetricFlowDimensionType {
    /// Scalar grouping dimension.
    Categorical,
    /// Calendar grouping dimension.
    Time,
}

/// MetricFlow time dimension parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MetricFlowDimensionParams {
    /// Declared time granularity.
    pub time_granularity: TimeGrain,
}
