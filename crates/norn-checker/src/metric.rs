//! Pure `MetricIR`/`QuestionIR` validation (#3301 slice 1).
//!
//! Checks use the admitted effective ontology, including inherited and inverse
//! roles. A successful check is a definition check, never evaluation evidence.

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::metric::{
    FilterOperator, FreshnessPolicy, MeasureAggregate, MetricAdditivity, MetricCurrency,
    MetricDefinition, MetricFilter, MetricRollup, MetricType, QuestionComparison, Reaggregate,
};
use norn_ir::{Multiplicity, PropertyType, QuestionIR, TimezonePolicy, UnitRef};

use crate::entity_instance::{EffectiveCatalog, EffectiveKind, EffectiveProperty};
use crate::property_types::error;
use crate::{Diagnostic, Span};
use norn_ir::metric::DimensionPath;

/// Stable diagnostic codes for this vocabulary.
pub mod codes {
    /// Calculation semantics outside the admitted pure checker profile.
    pub const UNSUPPORTED_SEMANTICS: &str = "metric.validation.unsupported_semantics";
    /// Norn policy values must be explicit and valid.
    pub const INVALID_POLICY: &str = "metric.validation.invalid_policy";
    /// Generated datasets and relationships must match the effective ontology.
    pub const ONTOLOGY_PROJECTION: &str = "metric.validation.ontology_projection";
    /// OSI imports need a checked calculation and ontology binding.
    pub const IMPORT_GAP: &str = "metric.validation.import_gap";
    /// Metric currency must match the amount type's authoritative currency source.
    pub const CURRENCY_SOURCE_MISMATCH: &str = "metric.validation.currency_source_mismatch";
    /// Mandatory time, currency and rate inputs require total paths.
    pub const PARTIAL_PATH: &str = "metric.validation.partial_path";
    /// Overlapping fan-out buckets cannot be rolled up as scalars.
    pub const FAN_OUT_ROLLUP: &str = "question.validation.fan_out_rollup";
    /// Snapshot aggregation requires a series grain and ordered reductions; this contract refuses it.
    pub const SNAPSHOT_AGGREGATION: &str = "question.validation.snapshot_aggregation";
    /// Simultaneous time and non-time reductions require an ordered reduction contract.
    pub const MIXED_AXIS_ROLLUP: &str = "question.validation.mixed_axis_rollup";
    /// Metric names must be dotted lower-case identifiers.
    pub const INVALID_NAME: &str = "metric.validation.invalid_name";
    /// Metric names must be unique.
    pub const DUPLICATE_METRIC: &str = "metric.validation.duplicate_metric";
    /// Metrics require an explicit non-blank owner.
    pub const INVALID_OWNER: &str = "metric.validation.invalid_owner";
    /// The referenced metric does not exist.
    pub const UNKNOWN_METRIC: &str = "metric.validation.unknown_metric";
    /// Ratio dependencies must be acyclic.
    pub const METRIC_CYCLE: &str = "metric.validation.cycle";
    /// The aggregate requires a compatible property.
    pub const INVALID_MEASURE: &str = "metric.validation.invalid_measure";
    /// The measure object type is unavailable.
    pub const UNKNOWN_OBJECT: &str = "metric.validation.unknown_object";
    /// Every path role and terminal property must resolve.
    pub const UNKNOWN_PATH: &str = "metric.validation.unknown_path";
    /// To-many dimensions require a symmetric aggregation strategy.
    pub const FAN_OUT: &str = "metric.validation.fan_out";
    /// The time axis must be a single-valued date or timestamp.
    pub const INVALID_TIME: &str = "metric.validation.invalid_time";
    /// Dimensions require a scalar property and compatible grain.
    pub const INVALID_DIMENSION: &str = "metric.validation.invalid_dimension";
    /// The filter literal and operator must match the property contract.
    pub const INVALID_FILTER: &str = "metric.validation.invalid_filter";
    /// Additivity must match the measure algebra.
    pub const INVALID_ADDITIVITY: &str = "metric.validation.invalid_additivity";
    /// Ratio operand units and currencies must agree.
    pub const UNIT_MISMATCH: &str = "metric.validation.unit_mismatch";
    /// Ratio operands must share their object and time grain.
    pub const GRAIN_MISMATCH: &str = "metric.validation.grain_mismatch";
    /// Currency must bind a single-valued currency property.
    pub const INVALID_CURRENCY: &str = "metric.validation.invalid_currency";
    /// Summing multiple currencies requires an explicit conversion.
    pub const MIXED_CURRENCY: &str = "metric.validation.mixed_currency";
    /// Conversion requires a target currency and a dimensionless decimal rate.
    pub const INVALID_CONVERSION: &str = "metric.validation.invalid_conversion";
    /// Metric dependencies and link paths are bounded to 64 levels.
    pub const COMPLEXITY_LIMIT: &str = "metric.validation.complexity_limit";
    /// Question names must be dotted lower-case identifiers.
    pub const QUESTION_NAME: &str = "question.validation.invalid_name";
    /// Question names must be unique.
    pub const DUPLICATE_QUESTION: &str = "question.validation.duplicate_question";
    /// Questions require at least one metric.
    pub const EMPTY_METRICS: &str = "question.validation.empty_metrics";
    /// Metric and dimension selections must be unique.
    pub const DUPLICATE_SELECTION: &str = "question.validation.duplicate_selection";
    /// The time window must be an ordered half-open UTC interval.
    pub const INVALID_WINDOW: &str = "question.validation.invalid_window";
    /// Freshness requires a positive age or a valid UTC instant.
    pub const INVALID_FRESHNESS: &str = "question.validation.invalid_freshness";
    /// Comparison questions must exist and have compatible selections.
    pub const UNKNOWN_COMPARISON: &str = "question.validation.unknown_comparison";
    /// Question comparisons must be acyclic.
    pub const COMPARISON_CYCLE: &str = "question.validation.comparison_cycle";
    /// Roll-ups must name selected axes and use an admitted reduction.
    pub const INVALID_ROLLUP: &str = "question.validation.invalid_rollup";
    /// Semi-additive metrics require their declared time reduction.
    pub const SEMI_ADDITIVE_TIME: &str = "question.validation.semi_additive_time";
    /// Non-additive metrics cannot be re-aggregated.
    pub const NON_ADDITIVE_ROLLUP: &str = "question.validation.non_additive_rollup";
}

const MAX_DEPTH: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Signature {
    object_type: String,
    unit: Option<UnitRef>,
    currency: Option<String>,
    depth: usize,
}

struct Path<'a> {
    properties: Vec<&'a EffectiveProperty>,
    fan_out: bool,
    partial: bool,
}

// Ontology-bound calculation plan. This is checker-local, never a wire contract.
#[derive(Debug, Clone)]
struct BoundMetric {
    source_index: usize,
    name: String,
    measure: BoundMeasure,
    additivity: MetricAdditivity,
    time_dimension: DimensionPath,
    default_filters: Vec<MetricFilter>,
    snapshot_groupings: Vec<DimensionPath>,
    currency: Option<MetricCurrency>,
    safe_aggregation: Option<norn_ir::metric::SafeAggregation>,
    owner: String,
}

#[derive(Debug, Clone)]
enum BoundMeasure {
    Property {
        object_type: norn_ir::ObjectTypeName,
        aggregate: MeasureAggregate,
        property: Option<norn_ir::PropertyId>,
    },
    Ratio {
        numerator: String,
        denominator: String,
    },
}

struct Checker<'a> {
    catalog: &'a EffectiveCatalog,
    metrics: BTreeMap<&'a str, (usize, &'a BoundMetric)>,
    signatures: BTreeMap<&'a str, Option<Signature>>,
    active: BTreeSet<&'a str>,
    out: Vec<Diagnostic>,
}

/// Check standalone BI definitions against an already admitted ontology.
///
/// Spans point into `{ metrics: [...], questions: [...] }`. Every ratio is
/// checked transitively; questions resolve dimensions/filter on each underlying
/// measure object. Missing or unavailable ontology contracts fail closed.
#[must_use]
#[allow(
    clippy::too_many_lines,
    reason = "bind question-local inline names and map every source diagnostic in one admission pass"
)]
pub fn check_metrics_and_questions(
    definitions: &[MetricDefinition],
    questions: &[QuestionIR],
    catalog: &EffectiveCatalog,
) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    let mut all = definitions.to_vec();
    let inline_names: BTreeMap<_, _> = questions
        .iter()
        .flat_map(|q| {
            q.metrics
                .iter()
                .filter(|s| s.inline.is_some())
                .map(move |s| {
                    (
                        (q.name.clone(), s.metric.clone()),
                        format!("{}.inline.{}", q.name, s.metric),
                    )
                })
        })
        .collect();
    let mut lowered_questions = questions.to_vec();
    for question in &mut lowered_questions {
        for selection in &mut question.metrics {
            if let Some(inline) = &mut selection.inline {
                if inline.name != selection.metric {
                    out.push(error(
                        codes::INVALID_MEASURE,
                        "Inline output and selection names must agree".into(),
                        Span::pointer("/questions"),
                    ));
                }
                if let Some(name) =
                    inline_names.get(&(question.name.clone(), selection.metric.clone()))
                {
                    selection.metric.clone_from(name);
                    inline.name.clone_from(name);
                }
            }
        }
        if let Some(
            QuestionComparison::Plan {
                question: reference,
                mapping,
            }
            | QuestionComparison::Forecast {
                question: reference,
                mapping,
            },
        ) = &mut question.comparison
        {
            if mapping.is_empty()
                && let Some(original) = questions.iter().find(|q| q.name == question.name)
            {
                *mapping = original
                    .metrics
                    .iter()
                    .map(|s| norn_ir::metric::ComparisonMetricMapping {
                        metric: s.metric.clone(),
                        reference_metric: s.metric.clone(),
                    })
                    .collect();
            }
            for entry in mapping {
                if let Some(name) = inline_names.get(&(question.name.clone(), entry.metric.clone()))
                {
                    entry.metric.clone_from(name);
                }
                if let Some(name) =
                    inline_names.get(&(reference.clone(), entry.reference_metric.clone()))
                {
                    entry.reference_metric.clone_from(name);
                }
            }
        }
    }
    let questions = lowered_questions.as_slice();
    let mut inline_locations = BTreeMap::new();
    for (index, question) in questions.iter().enumerate() {
        for (selection_index, selection) in question.metrics.iter().enumerate() {
            if selection.inline.is_none() && !definitions.iter().any(|d| d.name == selection.metric)
            {
                out.push(error(
                    codes::UNKNOWN_METRIC,
                    "Only declared metrics or local inline calculations are selectable".into(),
                    Span::pointer(format!(
                        "/questions/{index}/aggregate/{selection_index}/metric"
                    )),
                ));
            }
        }
    }
    for (index, question) in questions.iter().enumerate() {
        let roots = question_roots(question, catalog);
        if let Some(root) = roots
            .as_ref()
            .and_then(|r| r.iter().next())
            .filter(|_| roots.as_ref().is_some_and(|r| r.len() == 1))
        {
            for (selection_index, selection) in question.metrics.iter().enumerate() {
                let Some(inline) = selection.inline.as_ref() else {
                    continue;
                };
                inline_locations.insert(all.len(), (index, selection_index));
                let mut norn = inline.norn.clone();
                norn.time_dimension = Some(inline.agg_time_dimension.clone());
                all.push(MetricDefinition {
                    name: inline.name.clone(),
                    label: inline.name.clone(),
                    metric_type: MetricType::Simple,
                    type_params: norn_ir::metric::MetricTypeParams {
                        expr: inline.property.as_ref().map(|p| p.0.clone()),
                        metric_aggregation_params: Some(norn_ir::metric::MetricAggregationParams {
                            semantic_model: norn_ir::ObjectTypeName(root.clone()),
                            agg: inline.agg,
                            agg_time_dimension: field_name(&inline.agg_time_dimension),
                            non_additive_dimension: None,
                        }),
                        ..Default::default()
                    },
                    norn,
                });
            }
        } else if question
            .metrics
            .iter()
            .any(|selection| selection.inline.is_some())
        {
            out.push(error(
                codes::GRAIN_MISMATCH,
                "Inline aggregates require one concrete terminal object type".into(),
                Span::pointer(format!("/questions/{index}/inline")),
            ));
        }
    }
    let mut bound = Vec::new();
    for (index, definition) in all.iter().enumerate() {
        match bind_metric(definition) {
            Ok(mut metric) => {
                metric.source_index = index;
                bound.push(metric);
            }
            Err((code, field)) => out.push(error(
                code,
                format!("Cannot admit {}: {field}", definition.name),
                Span::pointer(format!("/metrics/{index}/{field}")),
            )),
        }
    }
    out.extend(check_bound_metrics(&bound, questions, catalog));
    for diagnostic in &mut out {
        if let Some(span) = &mut diagnostic.span {
            for (metric_index, (question_index, selection_index)) in &inline_locations {
                let prefix = format!("/metrics/{metric_index}/");
                if let Some(suffix) = span.pointer.strip_prefix(&prefix) {
                    let suffix = suffix
                        .replace("type_params/expr", "property")
                        .replace("type_params/metric_aggregation_params/agg", "agg")
                        .replace("x-norn/timeDimension", "aggTimeDimension");
                    span.pointer = format!(
                        "/questions/{question_index}/aggregate/{selection_index}/inline/{suffix}"
                    );
                    break;
                }
            }
        }
    }
    out
}

fn check_bound_metrics(
    metrics: &[BoundMetric],
    questions: &[QuestionIR],
    catalog: &EffectiveCatalog,
) -> Vec<Diagnostic> {
    let mut checker = Checker {
        catalog,
        metrics: BTreeMap::new(),
        signatures: BTreeMap::new(),
        active: BTreeSet::new(),
        out: Vec::new(),
    };
    for metric in metrics {
        let index = metric.source_index;
        let at = format!("/metrics/{index}");
        if !valid_name(&metric.name) {
            checker.reject(codes::INVALID_NAME, &format!("{at}/name"));
        }
        if checker
            .metrics
            .insert(&metric.name, (index, metric))
            .is_some()
        {
            checker.reject(codes::DUPLICATE_METRIC, &format!("{at}/name"));
        }
        if metric.owner.trim().is_empty() || metric.owner.trim() != metric.owner {
            checker.reject(codes::INVALID_OWNER, &format!("{at}/owner"));
        }
    }
    for metric in metrics {
        drop(checker.metric(&metric.name));
    }
    checker.questions(questions);
    checker.out
}

fn valid_name(name: &str) -> bool {
    name.split('.').all(|part| {
        part.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
            && part
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
    })
}

fn currency_code(code: &str) -> bool {
    code.len() == 3 && code.bytes().all(|c| c.is_ascii_uppercase())
}

fn is_time(property: &EffectiveProperty) -> bool {
    matches!(
        property.kind,
        EffectiveKind::Base(PropertyType::Date {} | PropertyType::Timestamp { .. })
    )
}

fn is_numeric(property: &EffectiveProperty) -> bool {
    matches!(
        property.kind,
        EffectiveKind::Base(PropertyType::Integer {} | PropertyType::Decimal { .. })
    )
}

fn is_scalar(property: &EffectiveProperty) -> bool {
    matches!(
        property.kind,
        EffectiveKind::Base(
            PropertyType::Text { .. }
                | PropertyType::Integer {}
                | PropertyType::Decimal { .. }
                | PropertyType::Boolean {}
                | PropertyType::Timestamp { .. }
                | PropertyType::Date {}
                | PropertyType::Duration {}
                | PropertyType::Enum { .. }
        )
    )
}

fn valid_utc(text: &str) -> bool {
    text.ends_with('Z') && crate::object_types::valid_timestamp(text, TimezonePolicy::Utc)
}

impl<'a> Checker<'a> {
    fn reject(&mut self, code: &str, at: &str) {
        let message = crate::codes::lookup(code).map_or(code, |entry| entry.message);
        let at = if at.starts_with("/metrics/") {
            at.replace(
                "/measure/objectType",
                "/type_params/metric_aggregation_params/semantic_model",
            )
            .replace("/measure/property", "/type_params/expr")
            .replace("/measure/", "/type_params/")
            .replace("/measure", "/type_params")
            .replace("/additivity", "/x-norn/additivity")
            .replace("/owner", "/x-norn/owner")
            .replace("/currency", "/x-norn/currency")
            .replace("/timeDimension", "/x-norn/timeDimension")
            .replace("/defaultFilters", "/x-norn/defaultFilters")
        } else {
            at.to_owned()
        };
        self.out
            .push(error(code, message.to_owned(), Span::pointer(at)));
    }

    /// Resolve all concrete targets; polymorphic paths must have the same
    /// terminal contract. Inverse cardinality comes from the declared `from`.
    fn path(&mut self, root: &str, path: &DimensionPath, at: &str) -> Option<Path<'a>> {
        if path.links.len() > MAX_DEPTH {
            self.reject(codes::COMPLEXITY_LIMIT, at);
            return None;
        }
        let mut current = BTreeSet::from([root.to_owned()]);
        let mut fan_out = false;
        let mut partial = false;
        for (index, role) in path.links.iter().enumerate() {
            let mut next = BTreeSet::new();
            for source in &current {
                let mut found = false;
                if let Some(links) = self.catalog.links_for_object_type(source) {
                    for link in links.iter().filter(|link| &link.id == role) {
                        found = true;
                        fan_out |= link.cardinality.to == Multiplicity::Many;
                        partial |= !link.cardinality.required;
                        if let Some(targets) = self.catalog.target_types(link.target.as_str()) {
                            next.extend(targets);
                        } else {
                            self.reject(codes::UNKNOWN_PATH, &format!("{at}/links/{index}"));
                            return None;
                        }
                    }
                }
                // The effective catalog exposes forward declarations. Inverse
                // roles are derived from those declarations, never guessed.
                if !found {
                    for candidate in self.catalog.object_type_names() {
                        for link in self
                            .catalog
                            .links_for_object_type(candidate)
                            .into_iter()
                            .flatten()
                        {
                            if link.inverse.as_ref() == Some(role)
                                && self
                                    .catalog
                                    .target_types(link.target.as_str())
                                    .is_some_and(|targets| targets.contains(source))
                            {
                                found = true;
                                fan_out |= link.cardinality.from == Multiplicity::Many;
                                // Forward requiredness proves nothing about inverse existence.
                                partial = true;
                                next.insert(candidate.to_owned());
                            }
                        }
                    }
                }
                if !found {
                    self.reject(codes::UNKNOWN_PATH, &format!("{at}/links/{index}"));
                    return None;
                }
            }
            if next.is_empty() {
                self.reject(codes::UNKNOWN_PATH, &format!("{at}/links/{index}"));
                return None;
            }
            current = next;
        }
        let mut properties = Vec::new();
        for name in &current {
            let property = self
                .catalog
                .object_type_named(name)
                .and_then(Result::ok)
                .and_then(|object| object.property(path.property.as_str()));
            let Some(property) = property else {
                self.reject(codes::UNKNOWN_PATH, &format!("{at}/property"));
                return None;
            };
            if properties
                .first()
                .is_some_and(|first: &&EffectiveProperty| !first.same_contract(property))
            {
                self.reject(codes::UNKNOWN_PATH, at);
                return None;
            }
            properties.push(property);
        }
        Some(Path {
            properties,
            fan_out,
            partial,
        })
    }

    fn metric(&mut self, name: &'a str) -> Option<Signature> {
        if let Some(signature) = self.signatures.get(name) {
            return signature.clone();
        }
        let &(index, metric) = self.metrics.get(name)?;
        let at = format!("/metrics/{index}");
        if self.active.contains(name) {
            self.reject(codes::METRIC_CYCLE, &format!("{at}/measure"));
            return None;
        }
        if self.active.len() >= MAX_DEPTH {
            self.reject(codes::COMPLEXITY_LIMIT, &format!("{at}/measure"));
            return None;
        }
        self.active.insert(name);
        let before = self.out.len();
        let signature = match &metric.measure {
            BoundMeasure::Property {
                object_type,
                aggregate,
                property,
            } => self.property_metric(
                metric,
                object_type.as_str(),
                *aggregate,
                property.as_ref().map(norn_ir::PropertyId::as_str),
                &at,
            ),
            BoundMeasure::Ratio {
                numerator,
                denominator,
            } => {
                if metric.additivity != MetricAdditivity::NonAdditive || metric.currency.is_some() {
                    self.reject(codes::INVALID_ADDITIVITY, &format!("{at}/additivity"));
                }
                let mut operands = Vec::new();
                for (field, operand) in [("numerator", numerator), ("denominator", denominator)] {
                    if let Some(&(_, referenced)) = self.metrics.get(operand.as_str()) {
                        if referenced.time_dimension != metric.time_dimension {
                            self.reject(codes::GRAIN_MISMATCH, &format!("{at}/measure/{field}"));
                        }
                        operands.push(self.metric(referenced.name.as_str()));
                    } else {
                        self.reject(codes::UNKNOWN_METRIC, &format!("{at}/measure/{field}"));
                        operands.push(None);
                    }
                }
                match operands.as_slice() {
                    [Some(left), Some(right)] => {
                        if left.unit != right.unit || left.currency != right.currency {
                            self.reject(codes::UNIT_MISMATCH, &format!("{at}/measure"));
                        }
                        if left.object_type != right.object_type {
                            self.reject(codes::GRAIN_MISMATCH, &format!("{at}/measure"));
                        }
                        self.time_and_filters(metric, &left.object_type, &at);
                        Some(Signature {
                            object_type: left.object_type.clone(),
                            unit: None,
                            currency: None,
                            depth: left.depth.max(right.depth).saturating_add(1),
                        })
                    }
                    _ => None,
                }
            }
        };
        if signature.as_ref().is_some_and(|s| s.depth > MAX_DEPTH) {
            self.reject(codes::COMPLEXITY_LIMIT, &format!("{at}/measure"));
        }
        self.active.remove(name);
        let signature = if self.out.len() == before {
            signature
        } else {
            None
        };
        self.signatures.insert(name, signature.clone());
        signature
    }

    fn property_metric(
        &mut self,
        metric: &BoundMetric,
        root: &str,
        aggregate: MeasureAggregate,
        property: Option<&str>,
        at: &str,
    ) -> Option<Signature> {
        let Some(Ok(object)) = self.catalog.object_type_named(root) else {
            self.reject(codes::UNKNOWN_OBJECT, &format!("{at}/measure/objectType"));
            return None;
        };
        let resolved = property.and_then(|id| object.property(id));
        if property.is_some() && resolved.is_none() {
            self.reject(codes::UNKNOWN_PATH, &format!("{at}/measure/property"));
        }
        let valid = match aggregate {
            MeasureAggregate::Count => property.is_none() || resolved.is_some_and(is_scalar),
            MeasureAggregate::CountDistinct => resolved.is_some_and(is_scalar),
            MeasureAggregate::Sum
            | MeasureAggregate::Min
            | MeasureAggregate::Max
            | MeasureAggregate::Avg => resolved.is_some_and(is_numeric),
        };
        if !valid {
            self.reject(codes::INVALID_MEASURE, &format!("{at}/measure"));
        }
        match aggregate {
            MeasureAggregate::Avg
            | MeasureAggregate::CountDistinct
            | MeasureAggregate::Min
            | MeasureAggregate::Max => {
                if metric.additivity != MetricAdditivity::NonAdditive {
                    self.reject(codes::INVALID_ADDITIVITY, &format!("{at}/additivity"));
                }
            }
            MeasureAggregate::Sum | MeasureAggregate::Count => {}
        }
        if matches!(
            metric.additivity,
            MetricAdditivity::SemiAdditive {
                aggregate: Reaggregate::Sum,
                ..
            }
        ) {
            self.reject(
                codes::INVALID_ADDITIVITY,
                &format!("{at}/additivity/aggregate"),
            );
        }
        self.time_and_filters(metric, root, at);
        let count = matches!(
            aggregate,
            MeasureAggregate::Count | MeasureAggregate::CountDistinct
        );
        let unit = if count {
            None
        } else {
            resolved.and_then(|property| {
                if let EffectiveKind::Base(PropertyType::Decimal { unit, .. }) = &property.kind {
                    unit.clone()
                } else {
                    None
                }
            })
        };
        let currency = self.measure_currency(metric, root, aggregate, resolved, at);
        Some(Signature {
            object_type: root.to_owned(),
            unit,
            currency,
            depth: 1,
        })
    }

    fn measure_currency(
        &mut self,
        metric: &BoundMetric,
        root: &str,
        aggregate: MeasureAggregate,
        property: Option<&EffectiveProperty>,
        at: &str,
    ) -> Option<String> {
        let source = property.and_then(|property| {
            if let EffectiveKind::Base(PropertyType::Decimal {
                currency_source, ..
            }) = &property.kind
            {
                currency_source.as_ref()
            } else {
                None
            }
        });
        if matches!(
            aggregate,
            MeasureAggregate::Count | MeasureAggregate::CountDistinct
        ) {
            if metric.currency.is_some() {
                self.reject(codes::INVALID_CURRENCY, &format!("{at}/currency"));
            }
            None
        } else if let Some(source) = source {
            if property.is_some_and(|p| {
                matches!(
                    p.kind,
                    EffectiveKind::Base(PropertyType::Decimal { unit: Some(_), .. })
                )
            }) {
                self.reject(codes::INVALID_CURRENCY, &format!("{at}/measure/property"));
            }
            let inferred = MetricCurrency {
                property: source.clone(),
                conversion: None,
            };
            let binding = metric.currency.as_ref().unwrap_or(&inferred);
            if &binding.property != source {
                self.reject(
                    codes::CURRENCY_SOURCE_MISMATCH,
                    &format!("{at}/currency/property"),
                );
                drop(self.currency(root, binding, aggregate, &format!("{at}/currency")));
            }
            // Validate the authoritative source even when a metric tries to rebind it.
            let verified = MetricCurrency {
                property: source.clone(),
                conversion: binding.conversion.clone(),
            };
            self.currency(root, &verified, aggregate, &format!("{at}/currency"))
        } else {
            if metric.currency.is_some() {
                self.reject(codes::CURRENCY_SOURCE_MISMATCH, &format!("{at}/currency"));
            }
            None
        }
    }

    fn time_and_filters(&mut self, metric: &BoundMetric, root: &str, at: &str) {
        if let Some(path) = self.path(root, &metric.time_dimension, &format!("{at}/timeDimension"))
        {
            if path.fan_out || path.properties.iter().any(|p| !is_time(p) || p.nullable) {
                self.reject(codes::INVALID_TIME, &format!("{at}/timeDimension"));
            }
            if path.partial {
                self.reject(codes::PARTIAL_PATH, &format!("{at}/timeDimension"));
            }
        }
        for (index, grouping) in metric.snapshot_groupings.iter().enumerate() {
            let grouping_at = format!(
                "{at}/type_params/metric_aggregation_params/non_additive_dimension/window_groupings/{index}"
            );
            if let Some(path) = self.path(root, grouping, &grouping_at)
                && (path.fan_out
                    || path.partial
                    || path.properties.iter().any(|p| !is_scalar(p) || p.nullable))
            {
                self.reject(codes::INVALID_DIMENSION, &grouping_at);
            }
        }
        for (index, filter) in metric.default_filters.iter().enumerate() {
            self.filter(root, filter, &format!("{at}/defaultFilters/{index}"));
        }
    }

    fn currency(
        &mut self,
        root: &str,
        binding: &MetricCurrency,
        aggregate: MeasureAggregate,
        at: &str,
    ) -> Option<String> {
        let path = self.path(root, &binding.property, &format!("{at}/property"))?;
        if path.partial {
            self.reject(codes::PARTIAL_PATH, &format!("{at}/property"));
        }
        let mut singleton = None;
        for property in &path.properties {
            let valid = match &property.kind {
                EffectiveKind::Registry(name) => name.as_str() == "iso4217.currency_code",
                EffectiveKind::Base(PropertyType::Enum { values }) => {
                    if let [value] = values.as_slice() {
                        singleton = Some(value.value.clone());
                    }
                    !values.is_empty() && values.iter().all(|v| currency_code(&v.value))
                }
                EffectiveKind::Base(
                    PropertyType::Text { .. }
                    | PropertyType::Integer {}
                    | PropertyType::Decimal { .. }
                    | PropertyType::Boolean {}
                    | PropertyType::Timestamp { .. }
                    | PropertyType::Date {}
                    | PropertyType::Duration {}
                    | PropertyType::Record { .. }
                    | PropertyType::JsonExt { .. }
                    | PropertyType::Ref { .. },
                )
                | EffectiveKind::Unresolved => false,
            };
            if !valid || path.fan_out || property.nullable {
                self.reject(codes::INVALID_CURRENCY, at);
            }
        }
        if let Some(conversion) = &binding.conversion {
            if !currency_code(&conversion.target) {
                self.reject(
                    codes::INVALID_CONVERSION,
                    &format!("{at}/conversion/target"),
                );
            }
            if let Some(rate) = self.path(root, &conversion.rate, &format!("{at}/conversion/rate"))
            {
                if rate.fan_out
                    || rate.properties.iter().any(|p| {
                        p.nullable
                            || !matches!(
                                p.kind,
                                EffectiveKind::Base(PropertyType::Decimal {
                                    unit: None,
                                    currency_source: None,
                                    ..
                                })
                            )
                    })
                {
                    self.reject(codes::INVALID_CONVERSION, &format!("{at}/conversion/rate"));
                }
                if rate.partial {
                    self.reject(codes::PARTIAL_PATH, &format!("{at}/conversion/rate"));
                }
            }
            Some(conversion.target.clone())
        } else if let Some(singleton) = singleton {
            Some(singleton)
        } else {
            if aggregate == MeasureAggregate::Sum {
                self.reject(codes::MIXED_CURRENCY, at);
            }
            // Without conversion, mixed-currency min/max/avg are meaningless too.
            if matches!(
                aggregate,
                MeasureAggregate::Min | MeasureAggregate::Max | MeasureAggregate::Avg
            ) {
                self.reject(codes::MIXED_CURRENCY, at);
            }
            None
        }
    }

    fn filter(&mut self, root: &str, filter: &MetricFilter, at: &str) {
        let Some(path) = self.path(root, &filter.path, &format!("{at}/path")) else {
            return;
        };
        for property in path.properties {
            let ordered = is_numeric(property)
                || is_time(property)
                || matches!(
                    property.kind,
                    EffectiveKind::Base(PropertyType::Text { .. })
                );
            if !is_scalar(property)
                || property
                    .check_predicate_literal(&filter.value, path.partial || property.nullable)
                    .is_err()
                || (!matches!(filter.operator, FilterOperator::Eq | FilterOperator::Ne)
                    && (!ordered || filter.value == norn_ir::RuntimeValue::Null))
            {
                self.reject(codes::INVALID_FILTER, at);
            }
        }
    }

    fn questions(&mut self, questions: &[QuestionIR]) {
        let mut by_name = BTreeMap::new();
        for (index, question) in questions.iter().enumerate() {
            let at = format!("/questions/{index}");
            if !valid_name(&question.name) {
                self.reject(codes::QUESTION_NAME, &format!("{at}/name"));
            }
            if by_name.insert(question.name.as_str(), question).is_some() {
                self.reject(codes::DUPLICATE_QUESTION, &format!("{at}/name"));
            }
            self.question_shape(question, &at);
            if question_roots(question, self.catalog).is_none() {
                self.reject(codes::UNKNOWN_PATH, &format!("{at}/searchAround"));
            }
            for (filter_index, filter) in question.filters.iter().enumerate() {
                self.filter(
                    question.object_set.as_str(),
                    filter,
                    &format!("{at}/filter/{filter_index}"),
                );
            }
            let mut dimensions = BTreeSet::new();
            for (dimension_index, dimension) in question.dimensions.iter().enumerate() {
                if !dimensions.insert(&dimension.path) {
                    self.reject(
                        codes::DUPLICATE_SELECTION,
                        &format!("{at}/groupBy/{dimension_index}"),
                    );
                }
            }
            let mut selected = BTreeSet::new();
            for (metric_index, selection) in question.metrics.iter().enumerate() {
                let selection_at = format!("{at}/aggregate/{metric_index}");
                if selection
                    .inline
                    .as_ref()
                    .is_some_and(|inline| inline.name != selection.metric)
                {
                    self.reject(
                        codes::INVALID_MEASURE,
                        &format!("{selection_at}/inline/name"),
                    );
                }
                if !selected.insert(&selection.metric) {
                    self.reject(codes::DUPLICATE_SELECTION, &selection_at);
                }
                let Some(&(_, metric)) = self.metrics.get(selection.metric.as_str()) else {
                    self.reject(codes::UNKNOWN_METRIC, &format!("{selection_at}/metric"));
                    continue;
                };
                if self
                    .signatures
                    .get(metric.name.as_str())
                    .is_none_or(Option::is_none)
                {
                    continue; // The metric's own diagnostics already refuse the set.
                }
                self.measure_paths(question, metric, &at);
                if let Some(rollup) = &selection.reaggregate {
                    self.rollup(
                        question,
                        metric,
                        rollup,
                        &format!("{selection_at}/reaggregate"),
                    );
                }
            }
        }
        for (index, question) in questions.iter().enumerate() {
            let at = format!("/questions/{index}/compare");
            if let Some(target) = comparison_target(question) {
                match by_name.get(target) {
                    Some(other) if self.comparison_compatible(question, other) => {}
                    Some(_) | None => self.reject(codes::UNKNOWN_COMPARISON, &at),
                }
            }
            let mut current = Some(question);
            let mut visited = BTreeSet::new();
            while let Some(node) = current {
                if !visited.insert(node.name.as_str()) {
                    self.reject(codes::COMPARISON_CYCLE, &at);
                    break;
                }
                current = comparison_target(node).and_then(|target| by_name.get(target).copied());
            }
        }
    }

    fn comparison_compatible(&self, question: &QuestionIR, other: &QuestionIR) -> bool {
        let Some(
            QuestionComparison::Plan { mapping, .. } | QuestionComparison::Forecast { mapping, .. },
        ) = &question.comparison
        else {
            return false;
        };
        if question.dimensions != other.dimensions
            || question.calendar != other.calendar
            || question.time_window != other.time_window
            || question.metrics.len() != other.metrics.len()
            || (!mapping.is_empty() && mapping.len() != question.metrics.len())
        {
            return false;
        }
        let mut used = BTreeSet::new();
        question.metrics.iter().all(|selection| {
            let target = if mapping.is_empty() {
                selection.metric.as_str()
            } else {
                let mut matches = mapping.iter().filter(|m| m.metric == selection.metric);
                let Some(entry) = matches.next() else {
                    return false;
                };
                if matches.next().is_some() {
                    return false;
                }
                entry.reference_metric.as_str()
            };
            if !used.insert(target) {
                return false;
            }
            let Some(reference) = other.metrics.iter().find(|m| m.metric == target) else {
                return false;
            };
            let left = self
                .signatures
                .get(selection.metric.as_str())
                .and_then(Option::as_ref);
            let right = self.signatures.get(target).and_then(Option::as_ref);
            let time_grains_match = matches!(
                (self.metrics.get(selection.metric.as_str()), self.metrics.get(target)),
                (Some((_, a)), Some((_, b))) if a.time_dimension == b.time_dimension
            );
            matches!((left, right), (Some(a), Some(b)) if a.unit == b.unit
                && a.currency == b.currency && a.object_type == b.object_type)
                && time_grains_match
                && selection.reaggregate == reference.reaggregate
        })
    }

    fn question_shape(&mut self, question: &QuestionIR, at: &str) {
        if question.search_around.len() > MAX_DEPTH {
            self.reject(codes::COMPLEXITY_LIMIT, &format!("{at}/searchAround"));
        }
        if question.metrics.is_empty() {
            self.reject(codes::EMPTY_METRICS, &format!("{at}/aggregate"));
        }
        let start =
            norn_expr::temporal::timestamp_ordering_key(question.time_window.start.as_str());
        let end = norn_expr::temporal::timestamp_ordering_key(question.time_window.end.as_str());
        if !valid_utc(question.time_window.start.as_str())
            || !valid_utc(question.time_window.end.as_str())
            || !matches!((start, end), (Ok(start), Ok(end)) if start < end)
        {
            self.reject(codes::INVALID_WINDOW, &format!("{at}/window"));
        }
        let freshness_valid = match &question.freshness {
            FreshnessPolicy::Live { max_age_ms } | FreshnessPolicy::Delayed { max_age_ms } => {
                max_age_ms.0 > 0
            }
            FreshnessPolicy::AsOf { at } => valid_utc(at.as_str()),
        };
        if !freshness_valid {
            self.reject(codes::INVALID_FRESHNESS, &format!("{at}/freshness"));
        }
    }

    fn measure_paths(&mut self, question: &QuestionIR, metric: &'a BoundMetric, at: &str) {
        let mut pending = vec![(metric, metric.safe_aggregation.is_some())];
        let mut visited = BTreeSet::new();
        while let Some((source, symmetric)) = pending.pop() {
            if !visited.insert((source.name.as_str(), symmetric)) {
                continue;
            }
            if matches!(source.additivity, MetricAdditivity::SemiAdditive { .. }) {
                self.reject(codes::SNAPSHOT_AGGREGATION, &format!("{at}/aggregate"));
            }
            match &source.measure {
                BoundMeasure::Ratio {
                    numerator,
                    denominator,
                } => {
                    for operand in [numerator, denominator] {
                        if let Some(&(_, referenced)) = self.metrics.get(operand.as_str()) {
                            pending.push((
                                referenced,
                                symmetric || referenced.safe_aggregation.is_some(),
                            ));
                        }
                    }
                }
                BoundMeasure::Property { object_type, .. } => {
                    if question_roots(question, self.catalog).is_none_or(|roots| {
                        roots != BTreeSet::from([object_type.0.clone()])
                            && self.catalog.target_types(object_type.as_str()).as_ref()
                                != Some(&roots)
                    }) {
                        self.reject(codes::GRAIN_MISMATCH, &format!("{at}/objectSet"));
                    }
                    self.question_paths(question, symmetric, object_type.as_str(), at);
                }
            }
        }
    }

    fn question_paths(&mut self, question: &QuestionIR, symmetric: bool, root: &str, at: &str) {
        for (index, dimension) in question.dimensions.iter().enumerate() {
            let dimension_at = format!("{at}/groupBy/{index}");
            if let Some(path) = self.path(root, &dimension.path, &format!("{dimension_at}/path")) {
                if path.fan_out && !symmetric {
                    self.reject(codes::FAN_OUT, &dimension_at);
                }
                if path
                    .properties
                    .iter()
                    .any(|p| !is_scalar(p) || (dimension.grain.is_some() && !is_time(p)))
                {
                    self.reject(codes::INVALID_DIMENSION, &dimension_at);
                }
            }
        }
    }

    fn rollup(
        &mut self,
        question: &QuestionIR,
        metric: &BoundMetric,
        rollup: &MetricRollup,
        at: &str,
    ) {
        let mut seen = BTreeSet::new();
        for (index, axis) in rollup.across.iter().enumerate() {
            if !seen.insert(axis) || !question.dimensions.iter().any(|d| &d.path == axis) {
                self.reject(codes::INVALID_ROLLUP, &format!("{at}/across/{index}"));
            }
        }
        if rollup.across.is_empty() && !rollup.over_time {
            self.reject(codes::INVALID_ROLLUP, at);
        }
        let over_time = rollup.over_time || rollup.across.contains(&metric.time_dimension);
        if over_time
            && rollup
                .across
                .iter()
                .any(|axis| axis != &metric.time_dimension)
        {
            self.reject(codes::MIXED_AXIS_ROLLUP, at);
        }
        if let Some(Some(signature)) = self.signatures.get(metric.name.as_str()) {
            let root = signature.object_type.clone();
            for (index, axis) in rollup.across.iter().enumerate() {
                let axis_at = format!("{at}/across/{index}");
                if self
                    .path(&root, axis, &axis_at)
                    .is_some_and(|path| path.fan_out)
                {
                    self.reject(codes::FAN_OUT_ROLLUP, &axis_at);
                }
            }
        }
        match &metric.additivity {
            MetricAdditivity::NonAdditive => self.reject(codes::NON_ADDITIVE_ROLLUP, at),
            MetricAdditivity::SemiAdditive { aggregate, .. } if over_time => {
                if &rollup.aggregate != aggregate {
                    self.reject(codes::SEMI_ADDITIVE_TIME, at);
                }
            }
            MetricAdditivity::Additive | MetricAdditivity::SemiAdditive { .. } => {
                if rollup.aggregate != Reaggregate::Sum {
                    self.reject(codes::INVALID_ROLLUP, at);
                }
            }
        }
    }
}

fn comparison_target(question: &QuestionIR) -> Option<&str> {
    match &question.comparison {
        Some(
            QuestionComparison::Plan { question, .. }
            | QuestionComparison::Forecast { question, .. },
        ) => Some(question),
        Some(QuestionComparison::PreviousPeriod) | None => None,
    }
}

/// Canonical field identifier for a directional ontology path.
#[must_use]
pub fn field_name(path: &DimensionPath) -> String {
    if path.links.is_empty()
        && path.property.0.len() >= 2
        && !path.property.0.ends_with('_')
        && !path.property.0.contains("__")
    {
        return path.property.0.clone();
    }
    // Length-prefixed components avoid collisions between underscored role names.
    // Escaping underscores keeps the generated name within MetricFlow's grammar.
    let components = path
        .links
        .iter()
        .map(norn_ir::LinkId::as_str)
        .chain(std::iter::once(path.property.as_str()));
    let encoded = components
        .map(|component| format!("{}_{}", component.len(), component.replace('_', "_u")))
        .collect::<Vec<_>>()
        .join("_");
    format!("norn_path_{encoded}_value")
}

pub(crate) fn dimension_path(name: &str) -> DimensionPath {
    DimensionPath {
        links: Vec::new(),
        property: norn_ir::PropertyId(name.into()),
    }
}

fn question_roots(question: &QuestionIR, catalog: &EffectiveCatalog) -> Option<BTreeSet<String>> {
    if question.search_around.len() > MAX_DEPTH
        || !matches!(
            catalog.object_type_named(question.object_set.as_str()),
            Some(Ok(_))
        )
    {
        return None;
    }
    let mut roots = BTreeSet::from([question.object_set.0.clone()]);
    for role in &question.search_around {
        let mut targets = BTreeSet::new();
        for root in &roots {
            if let Some(link) = catalog
                .links_for_object_type(root)?
                .iter()
                .find(|link| &link.id == role)
            {
                targets.extend(catalog.target_types(link.target.as_str())?);
            } else {
                let mut found = false;
                for candidate in catalog.object_type_names() {
                    for link in catalog.links_for_object_type(candidate)? {
                        if link.inverse.as_ref() == Some(role)
                            && catalog.target_types(link.target.as_str())?.contains(root)
                        {
                            found = true;
                            targets.insert(candidate.to_owned());
                        }
                    }
                }
                if !found {
                    return None;
                }
            }
        }
        if targets.is_empty() {
            return None;
        }
        roots = targets;
    }
    Some(roots)
}

#[allow(
    clippy::too_many_lines,
    reason = "one closed MetricFlow-to-ontology binding pass with fail-closed parameter checks"
)]
fn bind_metric(definition: &MetricDefinition) -> Result<BoundMetric, (&'static str, &'static str)> {
    let p = &definition.type_params;
    let n = &definition.norn;
    if n.derived.is_some() {
        return Err((codes::INVALID_POLICY, "x-norn/derived"));
    }
    if n.authz.as_ref().is_some_and(|a| {
        a.action.trim().is_empty() || a.action.trim() != a.action || a.min_group_size.0 == 0
    }) {
        return Err((codes::INVALID_POLICY, "x-norn/authz"));
    }
    if n.freshness.as_ref().is_some_and(|f| match f {
        FreshnessPolicy::Live { max_age_ms } | FreshnessPolicy::Delayed { max_age_ms } => {
            max_age_ms.0 == 0
        }
        FreshnessPolicy::AsOf { at } => !valid_utc(at.as_str()),
    }) {
        return Err((codes::INVALID_POLICY, "x-norn/freshness"));
    }
    if definition.label.trim().is_empty() {
        return Err((codes::INVALID_MEASURE, "label"));
    }
    let mut additivity = n.additivity.clone();
    let (measure, time_dimension) = match definition.metric_type {
        MetricType::Simple => {
            if p.numerator.is_some()
                || p.denominator.is_some()
                || !p.metrics.is_empty()
                || p.cumulative_type_params.is_some()
                || p.conversion_type_params.is_some()
            {
                return Err((codes::INVALID_MEASURE, "type_params"));
            }
            let agg = p.metric_aggregation_params.as_ref().ok_or((
                codes::INVALID_MEASURE,
                "type_params/metric_aggregation_params",
            ))?;
            let time = n
                .time_dimension
                .clone()
                .unwrap_or_else(|| dimension_path(&agg.agg_time_dimension));
            if field_name(&time) != agg.agg_time_dimension {
                return Err((
                    codes::GRAIN_MISMATCH,
                    "type_params/metric_aggregation_params/agg_time_dimension",
                ));
            }
            if let Some(snapshot) = &agg.non_additive_dimension {
                if snapshot.name != agg.agg_time_dimension {
                    return Err((
                        codes::GRAIN_MISMATCH,
                        "type_params/metric_aggregation_params/non_additive_dimension/name",
                    ));
                }
                if snapshot
                    .window_groupings
                    .iter()
                    .collect::<BTreeSet<_>>()
                    .len()
                    != snapshot.window_groupings.len()
                {
                    return Err((
                        codes::INVALID_DIMENSION,
                        "type_params/metric_aggregation_params/non_additive_dimension/window_groupings",
                    ));
                }
                let expected = MetricAdditivity::SemiAdditive {
                    over: norn_ir::metric::AdditivityAxis::Time,
                    aggregate: match snapshot.window_choice {
                        norn_ir::metric::WindowChoice::Min => Reaggregate::First,
                        norn_ir::metric::WindowChoice::Max => Reaggregate::Last,
                    },
                };
                if additivity != expected {
                    return Err((codes::INVALID_ADDITIVITY, "x-norn/additivity"));
                }
                additivity = expected;
            }
            (
                BoundMeasure::Property {
                    object_type: agg.semantic_model.clone(),
                    aggregate: agg.agg,
                    property: p
                        .expr
                        .as_ref()
                        .filter(|p| p.as_str() != "*")
                        .map(|id| norn_ir::PropertyId(id.clone())),
                },
                time,
            )
        }
        MetricType::Ratio => {
            if p.expr.is_some()
                || p.metric_aggregation_params.is_some()
                || !p.metrics.is_empty()
                || p.cumulative_type_params.is_some()
                || p.conversion_type_params.is_some()
            {
                return Err((codes::INVALID_MEASURE, "type_params"));
            }
            (
                BoundMeasure::Ratio {
                    numerator: p
                        .numerator
                        .clone()
                        .ok_or((codes::INVALID_MEASURE, "type_params/numerator"))?,
                    denominator: p
                        .denominator
                        .clone()
                        .ok_or((codes::INVALID_MEASURE, "type_params/denominator"))?,
                },
                n.time_dimension
                    .clone()
                    .ok_or((codes::INVALID_TIME, "x-norn/timeDimension"))?,
            )
        }
        // Full wire vocabulary is present; unsafe unspecified calculation semantics are refused.
        MetricType::Cumulative => {
            return Err((
                codes::UNSUPPORTED_SEMANTICS,
                "type_params/cumulative_type_params",
            ));
        }
        MetricType::Derived => return Err((codes::UNSUPPORTED_SEMANTICS, "type_params/expr")),
        MetricType::Conversion => {
            return Err((
                codes::UNSUPPORTED_SEMANTICS,
                "type_params/conversion_type_params",
            ));
        }
    };
    Ok(BoundMetric {
        source_index: 0,
        name: definition.name.clone(),
        measure,
        additivity,
        time_dimension,
        default_filters: n.default_filters.clone(),
        snapshot_groupings: p
            .metric_aggregation_params
            .as_ref()
            .and_then(|p| p.non_additive_dimension.as_ref())
            .map_or_else(Vec::new, |snapshot| {
                snapshot
                    .window_groupings
                    .iter()
                    .map(|name| dimension_path(name))
                    .collect()
            }),
        currency: n.currency.clone(),
        safe_aggregation: n.safe_aggregation,
        owner: n.owner.clone(),
    })
}

// Interchange uses the same effective path resolver as metric/query admission.
pub(crate) fn projection_path(
    root: &str,
    path: &DimensionPath,
    catalog: &EffectiveCatalog,
) -> Result<(bool, bool), Vec<Diagnostic>> {
    let mut checker = Checker {
        catalog,
        metrics: BTreeMap::new(),
        signatures: BTreeMap::new(),
        active: BTreeSet::new(),
        out: Vec::new(),
    };
    let resolved = checker.path(root, path, "/path");
    let result = resolved.map(|p| (p.fan_out, p.properties.iter().all(|p| is_time(p))));
    result.ok_or(checker.out)
}
