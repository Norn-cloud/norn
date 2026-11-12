//! Checked, identity-grained lowering for the bounded exact query profile.

pub mod arithmetic;
pub mod certificate;
pub mod contribution;

use norn_ir::metric::{FilterOperator, FreshnessPolicy, MetricFilter, TimeGrain};
use norn_ir::{MetricDefinition, ObjectTypeName, PropertyId, PropertyType, QuestionIR};

use super::{BoundMeasure, MeasureAggregate, bind_metric, check_metrics_and_questions};
use crate::entity_instance::{EffectiveCatalog, EffectiveKind, EffectiveProperty};
use crate::property_types::error;
use crate::{Diagnostic, Span};

/// The selected query cannot determine an unambiguous contribution grain.
pub const AMBIGUOUS_GRAIN: &str = "core.compute.ambiguous_grain";
/// Calendar grains this profile buckets a time property by; a group-by
/// without a grain groups by the property's values.
pub const GRAINS: &[TimeGrain] = &[
    TimeGrain::Day,
    TimeGrain::Week,
    TimeGrain::Month,
    TimeGrain::Quarter,
    TimeGrain::Year,
];
/// Filter operators this profile admits.
pub const OPERATORS: &[FilterOperator] = &[FilterOperator::Eq, FilterOperator::Ne];
/// Most group-bys one question may have in this profile.
pub const MAX_GROUP_BY: usize = 4;
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
    /// METRIC-SNAPSHOT-ASOF-1: sum each subject's last version at or before
    /// each bucket's end, ordered by `time_property` (subject = the root
    /// object's identity), instead of summing every row in its bucket.
    pub as_of: bool,
    /// METRIC-GRAIN-LINK-1: `time_property` belongs to the object this to-one
    /// link reaches, not to the measured row.
    pub time_link: Option<LinkedTime>,
}

/// METRIC-GRAIN-LINK-1: a sum's time read through one to-one link.
#[derive(Debug, Clone)]
pub struct LinkedTime {
    /// The to-one link from the measured object.
    pub link: norn_ir::LinkId,
    /// The one concrete object type the link reaches.
    pub target_type: ObjectTypeName,
    /// The linked object's stored, non-null time property.
    pub property: EffectiveProperty,
}

/// QUERY-GRAIN-1: a checked sum or ratio of sums. Construction is through [`admit`].
/// This plan is not execution evidence or an authorization grant.
#[derive(Debug, Clone)]
pub struct QuestionPlan {
    question: QuestionIR,
    sums: Vec<KeyedSum>,
    max_items: u32,
    properties: Vec<EffectiveProperty>,
    definitions: Vec<MetricDefinition>,
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

    /// Checked effective properties used for projection and store-model binding.
    #[must_use]
    pub fn properties(&self) -> &[EffectiveProperty] {
        &self.properties
    }

    /// Canonically ordered declarations, retained for content-addressing the plan.
    #[must_use]
    pub fn definitions(&self) -> &[MetricDefinition] {
        &self.definitions
    }

    /// METRIC-GRAIN-LINK-1: the to-one link this plan's time is read through.
    #[must_use]
    pub fn time_link(&self) -> Option<&LinkedTime> {
        self.sums.first().and_then(|sum| sum.time_link.as_ref())
    }

    /// METRIC-SNAPSHOT-ASOF-1: whether this plan is a snapshot (as-of) sum,
    /// executed over each subject's versions by [`contribution::as_of`].
    #[must_use]
    pub fn as_of(&self) -> bool {
        self.sums.iter().any(|sum| sum.as_of)
    }

    /// Full-population bound; exceeding it must discard the result.
    #[must_use]
    pub const fn max_items(&self) -> u32 {
        self.max_items
    }

    /// ASOF-KNOWLEDGE-1: the declared `AsOf` instant as exact UTC `(seconds, nanoseconds)`,
    /// parsed by the same temporal rules that admitted it. `None` for live/delayed freshness.
    #[must_use]
    pub fn as_of_instant(&self) -> Option<(i64, u32)> {
        match &self.question.freshness {
            FreshnessPolicy::AsOf { at } => {
                norn_expr::temporal::timestamp_ordering_key(at.as_str()).ok()
            }
            FreshnessPolicy::Live { .. } | FreshnessPolicy::Delayed { .. } => None,
        }
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
        return Err(refusal(UNSUPPORTED_ENGINE));
    };
    if max_items == 0
        || max_items
            > norn_ir::ResourceLimitProfile::norn_limits_v1()
                .question_limits()
                .max_scanned_records
        || !question.search_around.is_empty()
        || question.comparison.is_some()
        || selection.inline.is_some()
        || selection.reaggregate.is_some()
        || question.dimensions.len() > MAX_GROUP_BY
        || question.dimensions.iter().any(|dimension| {
            // A linked path is only a time grouping, checked against the sum's linked time below.
            (!dimension.path.links.is_empty() && dimension.grain.is_none())
                || !dimension.grain.is_none_or(|grain| GRAINS.contains(&grain))
        })
        || question.filters.iter().any(unsupported_filter)
    {
        return Err(refuse());
    }
    let definition = definitions
        .iter()
        .find(|m| m.name == selection.metric)
        .ok_or_else(refuse)?;
    let bound = bind_metric(definition).map_err(|_checked_binding| refuse())?;
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
    if unsupported_time_shape(question, &sums) {
        return Err(refuse());
    }
    let properties = catalog
        .object_type_named(question.object_set.as_str())
        .and_then(Result::ok)
        .ok_or_else(refuse)?
        .properties
        .clone();
    let mut definitions = definitions.to_vec();
    definitions.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(QuestionPlan {
        properties,
        definitions,
        question: question.clone(),
        sums,
        max_items,
    })
}

/// A snapshot or a linked time this profile cannot execute: more than one
/// sum, or a time grouping off the sum's own (linked) time path.
fn unsupported_time_shape(question: &QuestionIR, sums: &[KeyedSum]) -> bool {
    // METRIC-GRAIN-LINK-1: one sum, grouped in time only along its own linked path.
    let linked = sums.first().and_then(|sum| {
        sum.time_link
            .as_ref()
            .map(|link| (link.link.clone(), sum.time_property.clone()))
    });
    let linked_off_path = question.dimensions.iter().any(|dimension| {
        !dimension.path.links.is_empty()
            && linked.as_ref().is_none_or(|(link, property)| {
                dimension.path.links.as_slice() != std::slice::from_ref(link)
                    || &dimension.path.property != property
            })
    });
    // METRIC-SNAPSHOT-ASOF-1: one snapshot sum; a time grouping buckets its own time.
    let snapshot_off_path = sums.iter().any(|sum| sum.as_of)
        && question.dimensions.iter().any(|dimension| {
            dimension.grain.is_some()
                && sums
                    .first()
                    .is_none_or(|sum| dimension.path.property != sum.time_property)
        });
    let several = sums.len() != 1 && (linked.is_some() || sums.iter().any(|sum| sum.as_of));
    several || linked_off_path || snapshot_off_path
}

fn lower_sum(
    definition: &MetricDefinition,
    question: &QuestionIR,
    catalog: &EffectiveCatalog,
) -> Result<KeyedSum, Vec<Diagnostic>> {
    let refuse = || refusal(UNSUPPORTED_ENGINE);
    let metric = bind_metric(definition).map_err(|_checked_binding| refuse())?;
    let BoundMeasure::Property {
        object_type,
        aggregate: MeasureAggregate::Sum,
        property: Some(property),
    } = &metric.measure
    else {
        return Err(refuse());
    };
    if object_type != &question.object_set
        || metric.time_dimension.links.len() > 1
        || metric.currency.is_some()
        || definition.norn.freshness.is_some()
        || definition.norn.authz.is_some()
        || metric.default_filters.iter().any(unsupported_filter)
    {
        return Err(refuse());
    }
    let object = catalog
        .object_type_named(object_type.as_str())
        .and_then(Result::ok)
        .ok_or_else(refuse)?;
    let property = object.property(property.as_str()).ok_or_else(refuse)?;
    // A nullable property is summed over its present values; nulls are counted, never read as 0.
    if property.computed
        || !matches!(
            property.kind,
            EffectiveKind::Base(PropertyType::Decimal { .. } | PropertyType::Integer { .. })
        )
    {
        return Err(refuse());
    }
    // METRIC-SNAPSHOT-ASOF-1: the exact engine supports identity subjects only.
    let as_of = matches!(
        metric.additivity,
        norn_ir::metric::MetricAdditivity::SemiAdditive { .. }
    );
    if as_of
        && (!metric.snapshot
            || !metric.snapshot_groupings.is_empty()
            || !metric.time_dimension.links.is_empty())
    {
        return Err(refuse());
    }
    let time_link = match metric.time_dimension.links.as_slice() {
        [] => None,
        [link] => Some(
            linked_time(
                catalog,
                object_type.as_str(),
                link,
                &metric.time_dimension.property,
            )
            .ok_or_else(refuse)?,
        ),
        _ => return Err(refuse()),
    };
    Ok(KeyedSum {
        name: metric.name,
        property: property.clone(),
        time_property: metric.time_dimension.property,
        filters: metric.default_filters,
        as_of,
        time_link,
    })
}

/// METRIC-GRAIN-LINK-1: a to-one `link` of `source` reaching exactly one
/// concrete object type whose `property` is a stored, non-null time.
fn linked_time(
    catalog: &EffectiveCatalog,
    source: &str,
    link: &norn_ir::LinkId,
    property: &PropertyId,
) -> Option<LinkedTime> {
    let declared = catalog
        .links_for_object_type(source)?
        .iter()
        .find(|candidate| &candidate.id == link)?;
    if declared.cardinality.to != norn_ir::Multiplicity::One {
        return None;
    }
    let targets = catalog.target_types(declared.target.as_str())?;
    let [target] = targets.into_iter().collect::<Vec<_>>().try_into().ok()?;
    let object = catalog.object_type_named(&target)?.ok()?;
    let time = object.property(property.as_str())?;
    let is_time = matches!(
        time.kind,
        EffectiveKind::Base(PropertyType::Timestamp { .. } | PropertyType::Date {})
    );
    (is_time && !time.nullable && !time.computed).then(|| LinkedTime {
        link: link.clone(),
        target_type: object.name.clone(),
        property: time.clone(),
    })
}

fn unsupported_filter(filter: &MetricFilter) -> bool {
    !filter.path.links.is_empty() || !OPERATORS.contains(&filter.operator)
}

fn refusal(code: &str) -> Vec<Diagnostic> {
    vec![error(
        code,
        "Question is outside the bounded keyed sum/ratio profile".into(),
        Span::pointer("/questions/0"),
    )]
}
