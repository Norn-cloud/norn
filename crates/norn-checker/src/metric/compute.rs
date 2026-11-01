//! Checked, identity-grained lowering for the bounded exact query profile.

use norn_ir::metric::{MetricFilter, TimeGrain};
use norn_ir::{MetricDefinition, ObjectTypeName, PropertyId, PropertyType, QuestionIR};

use super::{BoundMeasure, MeasureAggregate, bind_metric, check_metrics_and_questions};
use crate::entity_instance::{EffectiveCatalog, EffectiveKind, EffectiveProperty};
use crate::property_types::error;
use crate::{Diagnostic, Span};

/// The selected query cannot determine an unambiguous contribution grain.
pub const AMBIGUOUS_GRAIN: &str = "core.compute.ambiguous_grain";
/// The requested semantics have no qualified execution in this profile.
pub const UNSUPPORTED_ENGINE: &str = "core.compute.unsupported_engine";

/// One exact sum, counted once per root object key in each output bucket.
#[derive(Debug, Clone)]
pub struct KeyedSum {
    /// Metric identity retained for contribution lineage.
    pub name: String,
    /// Stored numeric property and its effective unit, scale and constraints.
    pub property: EffectiveProperty,
    /// Direct effective-time property used by the question window.
    pub time_property: PropertyId,
    /// Predicates applied to this measure before summation.
    pub filters: Vec<MetricFilter>,
}

/// QUERY-GRAIN-1: a checked sum or ratio of sums. Construction is through [`admit`].
/// This plan is not execution evidence or an authorization grant.
#[derive(Debug, Clone)]
pub struct QuestionPlan {
    question: QuestionIR,
    sums: Vec<KeyedSum>,
    max_items: u32,
}

impl QuestionPlan {
    /// Original admitted question; grouping and freshness retain their IR semantics.
    #[must_use]
    pub const fn question(&self) -> &QuestionIR {
        &self.question
    }

    /// Contribution identity is `(object type, object key)`, never numeric value.
    #[must_use]
    pub const fn grain(&self) -> &ObjectTypeName {
        &self.question.object_set
    }

    /// One sum, or numerator then denominator for a ratio. No mean of ratios.
    #[must_use]
    pub fn sums(&self) -> &[KeyedSum] {
        &self.sums
    }

    /// Full-population bound; exceeding it must discard the result.
    #[must_use]
    pub const fn max_items(&self) -> u32 {
        self.max_items
    }
}

/// COMPUTE-DETERMINISTIC-1: lower the existing metric/question algebra to direct keyed sums or a ratio.
/// Link traversal, FX, roll-ups, comparisons, inline and nested ratios remain
/// refused until their evaluator semantics are qualified. No engine is selected.
///
/// # Errors
/// Returns ordinary ontology/metric diagnostics first, then stable Core 03
/// refusals for semantics outside this bounded profile.
pub fn admit(
    definitions: &[MetricDefinition],
    question: &QuestionIR,
    catalog: &EffectiveCatalog,
    max_items: u32,
) -> Result<QuestionPlan, Vec<Diagnostic>> {
    let mut findings =
        check_metrics_and_questions(definitions, std::slice::from_ref(question), catalog);
    if findings.iter().any(|finding| {
        [
            super::codes::FAN_OUT,
            super::codes::GRAIN_MISMATCH,
            super::codes::FAN_OUT_ROLLUP,
        ]
        .contains(&finding.code.as_str())
    }) {
        findings.extend(refusal(AMBIGUOUS_GRAIN));
    }
    if !findings.is_empty() {
        return Err(findings);
    }
    let refuse = || refusal(UNSUPPORTED_ENGINE);
    let [selection] = question.metrics.as_slice() else {
        return Err(refusal(AMBIGUOUS_GRAIN));
    };
    if max_items == 0
        || max_items
            > norn_ir::ResourceLimitProfile::norn_limits_v1()
                .slice
                .max_records
        || !question.search_around.is_empty()
        || question.comparison.is_some()
        || selection.inline.is_some()
        || selection.reaggregate.is_some()
        || question.dimensions.len() > 1
        || question.dimensions.iter().any(|dimension| {
            !dimension.path.links.is_empty()
                || !matches!(dimension.grain, None | Some(TimeGrain::Week))
        })
        || question
            .filters
            .iter()
            .any(|filter| !filter.path.links.is_empty())
    {
        return Err(refuse());
    }
    let definition = definitions
        .iter()
        .find(|m| m.name == selection.metric)
        .ok_or_else(refuse)?;
    let bound = bind_metric(definition).map_err(|_| refuse())?;
    let names = match &bound.measure {
        BoundMeasure::Property { .. } => vec![bound.name.as_str()],
        BoundMeasure::Ratio {
            numerator,
            denominator,
        } => {
            if !bound.default_filters.is_empty()
                || bound.currency.is_some()
                || definition.norn.freshness.is_some()
                || definition.norn.authz.is_some()
            {
                return Err(refuse());
            }
            vec![numerator.as_str(), denominator.as_str()]
        }
    };
    let sums = names
        .into_iter()
        .map(|name| {
            let definition = definitions
                .iter()
                .find(|m| m.name == name)
                .ok_or_else(refuse)?;
            lower_sum(definition, question, catalog)
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(QuestionPlan {
        question: question.clone(),
        sums,
        max_items,
    })
}

fn lower_sum(
    definition: &MetricDefinition,
    question: &QuestionIR,
    catalog: &EffectiveCatalog,
) -> Result<KeyedSum, Vec<Diagnostic>> {
    let refuse = || refusal(UNSUPPORTED_ENGINE);
    let metric = bind_metric(definition).map_err(|_| refuse())?;
    let BoundMeasure::Property {
        object_type,
        aggregate: MeasureAggregate::Sum,
        property: Some(property),
    } = &metric.measure
    else {
        return Err(refuse());
    };
    if object_type != &question.object_set
        || !metric.time_dimension.links.is_empty()
        || metric.currency.is_some()
        || definition.norn.freshness.is_some()
        || definition.norn.authz.is_some()
        || metric
            .default_filters
            .iter()
            .any(|f| !f.path.links.is_empty())
    {
        return Err(refuse());
    }
    let object = catalog
        .object_type_named(object_type.as_str())
        .and_then(Result::ok)
        .ok_or_else(refuse)?;
    let property = object.property(property.as_str()).ok_or_else(refuse)?;
    if property.computed
        || property.nullable
        || !matches!(
            property.kind,
            EffectiveKind::Base(PropertyType::Decimal { .. } | PropertyType::Integer { .. })
        )
    {
        return Err(refuse());
    }
    Ok(KeyedSum {
        name: metric.name,
        property: property.clone(),
        time_property: metric.time_dimension.property,
        filters: metric.default_filters,
    })
}

fn refusal(code: &str) -> Vec<Diagnostic> {
    vec![error(
        code,
        "Question is outside the bounded keyed sum/ratio profile".into(),
        Span::pointer("/questions/0"),
    )]
}
