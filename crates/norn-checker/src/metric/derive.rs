//! Ontology-derived metric candidates. Admission and execution use the existing
//! metric checker and compute plan; this module has no evaluator.

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::metric::{
    AdditivityAxis, DimensionPath, FilterOperator, MeasureAggregate, MetricAdditivity,
    MetricAggregationParams, MetricNorn, MetricType, MetricTypeParams, NonAdditiveDimension,
    QuestionDimension, Reaggregate, TimeGrain, WindowChoice,
};
use norn_ir::{
    ArtifactEnvelope, ArtifactPayload, LawIR, LawModality, LawResponse, LawRoot, LawSeverity,
    MetricDefinition, PropertyAdditivity, PropertyId, PropertyType, PropertyTypeName, QuestionIR,
};

use crate::entity_instance::{EffectiveCatalog, EffectiveKind, EffectiveObjectType};
use crate::{Diagnostic, Span};

use super::{codes, compute, field_name};

/// Exact source of a generated additivity contract and its ratios.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConservationSource {
    /// Law identity, preserved rather than inferred from its wording.
    pub law: String,
    /// Canonical Norn1 hash of the entire law envelope.
    pub digest: String,
    /// Input term of the checked equality.
    pub input: PropertyId,
    /// Distinct output terms of the checked equality.
    pub outputs: Vec<PropertyId>,
}

/// A factorized property × dimension × time-grain candidate space.
#[derive(Debug, Clone)]
pub struct MetricCandidate {
    /// Existing `MetricIR`, suitable for ordinary admission.
    pub definition: MetricDefinition,
    /// Possible direct and single-link dimensions. Selecting one still requires
    /// ordinary path, fan-out and execution-profile checks.
    pub dimensions: Vec<DimensionPath>,
    /// Calendar grains supported by the existing question vocabulary.
    pub time_grains: [TimeGrain; 5],
    /// Conservation evidence, when the law supplies term additivity.
    pub conservation: Vec<ConservationSource>,
    /// METRIC-UNIT-PARTITION-1: the properties holding the measure's unit.
    /// [`MetricSpace::admit`] groups by each one a question neither groups by
    /// nor fixes with an `eq` filter (`eq null` included); a question admitted
    /// any other way that leaves one free refuses `mixed_unit`.
    pub unit_partition: Vec<DimensionPath>,
}

/// Generated candidates and checker-owned law admission evidence.
/// Callers cannot supply a refinement or replace a generated definition.
#[derive(Debug, Clone)]
pub struct MetricSpace {
    candidates: Vec<MetricCandidate>,
    catalog: EffectiveCatalog,
}

impl MetricSpace {
    /// Deterministically ordered candidates; none is a governed/certified KPI.
    #[must_use]
    pub fn candidates(&self) -> &[MetricCandidate] {
        &self.candidates
    }

    /// The catalog the candidates were derived from, with any conserved
    /// terms recorded: what an ad-hoc question over them is admitted against.
    #[must_use]
    pub const fn catalog(&self) -> &EffectiveCatalog {
        &self.catalog
    }

    /// Lower a selection through the existing checked compute seam.
    ///
    /// # Errors
    /// Returns the existing metric/question and compute-profile diagnostics.
    pub fn admit(
        &self,
        question: &QuestionIR,
        max_items: u32,
    ) -> Result<compute::QuestionPlan, Vec<Diagnostic>> {
        let by_name: BTreeMap<_, _> = self
            .candidates
            .iter()
            .map(|candidate| (candidate.definition.name.as_str(), &candidate.definition))
            .collect();
        let mut pending: Vec<_> = question
            .metrics
            .iter()
            .map(|selection| selection.metric.as_str())
            .collect();
        let mut selected = BTreeSet::new();
        while let Some(name) = pending.pop() {
            if !selected.insert(name) {
                continue;
            }
            if let Some(definition) = by_name.get(name) {
                pending.extend(definition.type_params.numerator.as_deref());
                pending.extend(definition.type_params.denominator.as_deref());
            }
        }
        // METRIC-UNIT-PARTITION-1: group by each unit source the question leaves free.
        let mut question = question.clone();
        for path in self
            .candidates
            .iter()
            .filter(|candidate| selected.contains(candidate.definition.name.as_str()))
            .flat_map(|candidate| &candidate.unit_partition)
        {
            let fixed = question
                .dimensions
                .iter()
                .any(|dimension| &dimension.path == path)
                || question.filters.iter().any(|filter| {
                    &filter.path == path && matches!(filter.operator, FilterOperator::Eq)
                });
            if !fixed && question.dimensions.len() < compute::MAX_GROUP_BY {
                question.dimensions.push(QuestionDimension {
                    path: path.clone(),
                    grain: None,
                });
            }
        }
        let definitions: Vec<_> = selected
            .into_iter()
            .filter_map(|name| by_name.get(name))
            .map(|definition| (**definition).clone())
            .collect();
        compute::admit(&definitions, &question, &self.catalog, max_items)
    }
}

/// METRIC-DERIVE-1: derive sums and conservation ratios from an admitted
/// ontology at an explicitly selected law scope. Units never grant additivity.
/// Conditional, advisory and state-limited laws do not refine a population.
///
/// # Errors
/// Returns ordinary law diagnostics if the law set is not admitted, or an
/// existing complexity diagnostic if candidate construction exceeds its bound.
pub fn derive_metric_space(
    artifacts: &[ArtifactEnvelope],
    registry: &BTreeSet<PropertyTypeName>,
    scope: &LawRoot,
) -> Result<MetricSpace, Vec<Diagnostic>> {
    let findings = crate::verification::check_law_set(artifacts, &[], registry);
    if !findings.is_empty() {
        return Err(findings);
    }
    let mut catalog = EffectiveCatalog::from_artifacts_with_registry(artifacts, registry);
    let laws = conservation_laws(artifacts, scope);
    let mut evidence: BTreeMap<String, Vec<ConservationSource>> = BTreeMap::new();
    for name in catalog.object_type_names() {
        let Some(Ok(object)) = catalog.object_type_named(name) else {
            continue;
        };
        for (law, source) in &laws {
            if applies(law, object) && compatible_terms(object, source) {
                evidence
                    .entry(name.to_owned())
                    .or_default()
                    .push(source.clone());
            }
        }
    }
    for (name, sources) in &evidence {
        let terms = sources
            .iter()
            .flat_map(|source| std::iter::once(&source.input).chain(&source.outputs))
            .cloned()
            .collect();
        catalog.record_conserved_metric_terms(name, &terms);
    }
    let mut candidates = Vec::new();
    for name in catalog.object_type_names() {
        let Some(Ok(object)) = catalog.object_type_named(name) else {
            continue;
        };
        let dimensions = dimensions(object, &catalog);
        for time in object
            .properties
            .iter()
            .filter(|property| super::is_time(property))
        {
            let time_path = direct(&time.id);
            let sources = evidence.get(name).cloned().unwrap_or_default();
            candidates.extend(candidates_at_time(
                object,
                &dimensions,
                &sources,
                &time_path,
            ));
            if candidates.len() > 16_384 {
                return Err(vec![crate::property_types::error(
                    codes::COMPLEXITY_LIMIT,
                    "Derived metric space exceeds 16384 candidates".to_owned(),
                    Span::pointer("/metrics"),
                )]);
            }
        }
    }
    candidates.sort_by(|left, right| left.definition.name.cmp(&right.definition.name));
    candidates.dedup_by(|left, right| left.definition == right.definition);
    Ok(MetricSpace {
        candidates,
        catalog,
    })
}

fn conservation_laws(
    artifacts: &[ArtifactEnvelope],
    scope: &LawRoot,
) -> Vec<(LawIR, ConservationSource)> {
    let mut laws: Vec<_> = artifacts
        .iter()
        .filter(|artifact| matches!(artifact.payload, ArtifactPayload::Law(_)))
        .filter_map(|artifact| {
            let law: LawIR = serde_json::from_value(
                serde_json::to_value(artifact.payload.to_json_value()).ok()?,
            )
            .ok()?;
            if &law.scope.root != scope
                || law.condition.is_some()
                || law.timing.is_some()
                || !law.exceptions.is_empty()
                || law.severity != LawSeverity::Error
                || !matches!(law.modality, LawModality::Always {})
                || !law.scope.lifecycle_states.is_empty()
                || !law.scope.commitments.is_empty()
            {
                return None;
            }
            let LawResponse::Predicate { expression } = &law.response else {
                return None;
            };
            let (input, outputs) = conservation_terms(expression.as_str())?;
            let bytes = norn_canonical::canonical_json_of(artifact).ok()?;
            let digest = norn_canonical::HashProfile::Norn1.hash_canonical_bytes(&bytes);
            Some((
                law,
                ConservationSource {
                    law: String::new(),
                    digest,
                    input,
                    outputs,
                },
            ))
        })
        .map(|(law, mut source)| {
            source.law.clone_from(&law.id);
            (law, source)
        })
        .collect();
    laws.sort_by(|left, right| {
        left.1
            .law
            .cmp(&right.1.law)
            .then(left.1.digest.cmp(&right.1.digest))
    });
    laws.dedup_by(|left, right| left.1 == right.1);
    laws
}

fn candidates_at_time(
    object: &EffectiveObjectType,
    dimensions: &[DimensionPath],
    sources: &[ConservationSource],
    time_path: &DimensionPath,
) -> Vec<MetricCandidate> {
    let mut candidates = Vec::new();
    let mut sums = BTreeMap::new();
    for property in &object.properties {
        let EffectiveKind::Base(kind) = &property.kind else {
            continue;
        };
        if !super::is_numeric(property) {
            continue;
        }
        let proof: Vec<_> = sources
            .iter()
            .filter(|source| source.input == property.id || source.outputs.contains(&property.id))
            .cloned()
            .collect();
        let additive = match kind.additivity() {
            PropertyAdditivity::Additive {} => true,
            PropertyAdditivity::Undeclared {} => !proof.is_empty(),
            PropertyAdditivity::SemiAdditive {}
            | PropertyAdditivity::NonAdditive {}
            | PropertyAdditivity::Ratio { .. } => false,
        };
        if matches!(kind.additivity(), PropertyAdditivity::SemiAdditive {}) {
            let mut generated = candidate(
                snapshot(object, &property.id, time_path),
                dimensions,
                Vec::new(),
            );
            generated.unit_partition = unit_partition(object, &[&property.id]);
            candidates.push(generated);
            continue;
        }
        if !additive {
            continue;
        }
        let definition = sum(object, &property.id, time_path, &proof);
        sums.insert(property.id.clone(), definition.name.clone());
        let mut generated = candidate(definition, dimensions, proof);
        generated.unit_partition = unit_partition(object, &[&property.id]);
        candidates.push(generated);
    }
    declared_ratios(
        object,
        dimensions,
        sources,
        time_path,
        &sums,
        &mut candidates,
    );
    conserved_ratios(
        object,
        dimensions,
        sources,
        time_path,
        &sums,
        &mut candidates,
    );
    candidates
}

fn declared_ratios(
    object: &EffectiveObjectType,
    dimensions: &[DimensionPath],
    sources: &[ConservationSource],
    time_path: &DimensionPath,
    sums: &BTreeMap<PropertyId, String>,
    candidates: &mut Vec<MetricCandidate>,
) {
    for property in &object.properties {
        let EffectiveKind::Base(kind) = &property.kind else {
            continue;
        };
        let PropertyAdditivity::Ratio {
            numerator,
            denominator,
        } = kind.additivity()
        else {
            continue;
        };
        let (Some(numerator_name), Some(denominator_name)) =
            (sums.get(numerator), sums.get(denominator))
        else {
            continue;
        };
        let proof: Vec<_> = sources
            .iter()
            .filter(|source| {
                [&source.input]
                    .into_iter()
                    .chain(&source.outputs)
                    .any(|term| term == numerator || term == denominator)
            })
            .cloned()
            .collect();
        let mut definition = sum(object, &property.id, time_path, &proof);
        definition.name.push_str(".ratio_of_sums");
        definition.label = format!(
            "sum({}) / sum({}); {}",
            numerator.as_str(),
            denominator.as_str(),
            definition.label
        );
        definition.metric_type = MetricType::Ratio;
        definition.type_params = MetricTypeParams {
            numerator: Some(numerator_name.clone()),
            denominator: Some(denominator_name.clone()),
            ..MetricTypeParams::default()
        };
        definition.norn.additivity = MetricAdditivity::NonAdditive;
        definition.norn.currency = None;
        let mut generated = candidate(definition, dimensions, proof);
        generated.unit_partition = unit_partition(object, &[numerator, denominator]);
        candidates.push(generated);
    }
}

fn conserved_ratios(
    object: &EffectiveObjectType,
    dimensions: &[DimensionPath],
    sources: &[ConservationSource],
    time_path: &DimensionPath,
    sums: &BTreeMap<PropertyId, String>,
    candidates: &mut Vec<MetricCandidate>,
) {
    for source in sources {
        let Some(denominator) = sums.get(&source.input) else {
            continue;
        };
        for output in &source.outputs {
            let Some(numerator) = sums.get(output) else {
                continue;
            };
            let mut definition = sum(object, output, time_path, std::slice::from_ref(source));
            definition.name = format!(
                "{}.per_{}.law_{}",
                definition.name,
                source.input.as_str(),
                source.digest
            );
            definition.label = format!(
                "sum({}) / sum({}); law {}@{}",
                output.as_str(),
                source.input.as_str(),
                source.law,
                source.digest
            );
            definition.metric_type = MetricType::Ratio;
            definition.type_params = MetricTypeParams {
                numerator: Some(numerator.clone()),
                denominator: Some(denominator.clone()),
                ..MetricTypeParams::default()
            };
            definition.norn.additivity = MetricAdditivity::NonAdditive;
            let mut generated = candidate(definition, dimensions, vec![source.clone()]);
            generated.unit_partition = unit_partition(object, &[output, &source.input]);
            candidates.push(generated);
        }
    }
}

fn applies(law: &LawIR, object: &EffectiveObjectType) -> bool {
    let matches = |name: &str| {
        object
            .lineage
            .iter()
            .any(|ancestor| ancestor.as_str() == name)
            || object
                .interfaces
                .iter()
                .any(|interface| interface.as_str() == name)
    };
    matches(&law.subject) && law.scope.object_types.iter().any(|name| matches(name))
}

/// METRIC-RATE-UNKNOWN: rate terms never supply quantity-additivity evidence.
fn compatible_terms(object: &EffectiveObjectType, source: &ConservationSource) -> bool {
    let Some(input) = object.property(source.input.as_str()) else {
        return false;
    };
    let EffectiveKind::Base(input_kind) = &input.kind else {
        return false;
    };
    std::iter::once(&source.input)
        .chain(&source.outputs)
        .all(|id| {
            let Some(property) = object.property(id.as_str()) else {
                return false;
            };
            if property.nullable || property.computed {
                return false;
            }
            let EffectiveKind::Base(kind) = &property.kind else {
                return false;
            };
            if !matches!(
                kind.additivity(),
                PropertyAdditivity::Undeclared {} | PropertyAdditivity::Additive {}
            ) {
                return false;
            }
            match (input_kind, kind) {
                (PropertyType::Integer { .. }, PropertyType::Integer { .. }) => true,
                (
                    PropertyType::Decimal {
                        scale: a,
                        unit: u,
                        currency_source: None,
                        per_unit: None,
                        ..
                    },
                    PropertyType::Decimal {
                        scale: b,
                        unit: v,
                        currency_source: None,
                        per_unit: None,
                        ..
                    },
                ) => a == b && u == v,
                _ => false,
            }
        })
}

fn direct(property: &PropertyId) -> DimensionPath {
    DimensionPath {
        links: Vec::new(),
        property: property.clone(),
    }
}

fn dimensions(object: &EffectiveObjectType, catalog: &EffectiveCatalog) -> Vec<DimensionPath> {
    let mut paths: BTreeSet<_> = object
        .properties
        .iter()
        .filter(|p| super::is_scalar(p))
        .map(|p| direct(&p.id))
        .collect();
    for link in catalog
        .links_for_object_type(object.name.as_str())
        .into_iter()
        .flatten()
    {
        for target in catalog
            .target_types(link.target.as_str())
            .into_iter()
            .flatten()
        {
            let Some(Ok(target)) = catalog.object_type_named(&target) else {
                continue;
            };
            for property in target.properties.iter().filter(|p| super::is_scalar(p)) {
                paths.insert(DimensionPath {
                    links: vec![link.id.clone()],
                    property: property.id.clone(),
                });
            }
        }
    }
    paths.into_iter().collect()
}

fn sum(
    object: &EffectiveObjectType,
    property: &PropertyId,
    time: &DimensionPath,
    sources: &[ConservationSource],
) -> MetricDefinition {
    let proof = sources
        .iter()
        .map(|source| format!("{}@{}", source.law, source.digest))
        .collect::<Vec<_>>()
        .join(",");
    let currency = object
        .property(property.as_str())
        .and_then(|property| match &property.kind {
            EffectiveKind::Base(PropertyType::Decimal {
                currency_source: Some(path),
                ..
            }) => Some(norn_ir::metric::MetricCurrency {
                property: path.clone(),
                conversion: None,
            }),
            EffectiveKind::Base(_) | EffectiveKind::Registry(_) | EffectiveKind::Unresolved => None,
        });
    MetricDefinition {
        name: format!(
            "analysis.object_{}.{}.by_{}",
            encoded_name(object.name.as_str()),
            property.as_str(),
            time.property.as_str()
        ),
        label: format!("sum({}); conservation [{proof}]", property.as_str()),
        metric_type: MetricType::Simple,
        type_params: MetricTypeParams {
            expr: Some(property.as_str().to_owned()),
            metric_aggregation_params: Some(MetricAggregationParams {
                semantic_model: object.name.clone(),
                agg: MeasureAggregate::Sum,
                agg_time_dimension: field_name(time),
                non_additive_dimension: None,
            }),
            ..MetricTypeParams::default()
        },
        norn: MetricNorn {
            ir_version: norn_ir::IrVersion01::V0_1,
            additivity: MetricAdditivity::Additive,
            time_dimension: Some(time.clone()),
            default_filters: Vec::new(),
            currency,
            safe_aggregation: None,
            owner: "ontology-derived".to_owned(),
            certified: false,
            authz: None,
            freshness: None,
            derived: None,
            incremental: None,
        },
    }
}

/// METRIC-SNAPSHOT-ASOF-1: a balance's value as of each period's end, summed
/// across subjects and never over time.
fn snapshot(
    object: &EffectiveObjectType,
    property: &PropertyId,
    time: &DimensionPath,
) -> MetricDefinition {
    let mut definition = sum(object, property, time, &[]);
    definition.name = format!(
        "analysis.object_{}.{}.as_of_{}",
        encoded_name(object.name.as_str()),
        property.as_str(),
        time.property.as_str()
    );
    definition.label = format!(
        "{}, as of {}; last value per {} at each period's end",
        property.as_str(),
        time.property.as_str(),
        object.name.as_str()
    );
    if let Some(params) = definition.type_params.metric_aggregation_params.as_mut() {
        params.non_additive_dimension = Some(NonAdditiveDimension {
            name: params.agg_time_dimension.clone(),
            window_choice: WindowChoice::Max,
            window_groupings: Vec::new(),
        });
    }
    definition.norn.additivity = MetricAdditivity::SemiAdditive {
        over: AdditivityAxis::Time,
        aggregate: Reaggregate::Last,
    };
    definition
}

fn encoded_name(name: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    name.bytes()
        .flat_map(|byte| [byte >> 4, byte & 15])
        .filter_map(|digit| HEX.get(usize::from(digit)).map(|byte| char::from(*byte)))
        .collect()
}

/// The direct unit-source paths of `measures`, deduplicated in declaration order.
fn unit_partition(object: &EffectiveObjectType, measures: &[&PropertyId]) -> Vec<DimensionPath> {
    let mut paths: Vec<DimensionPath> = Vec::new();
    for id in measures {
        if let Some(EffectiveKind::Base(PropertyType::Decimal { unit_source, .. })) =
            object.property(id.as_str()).map(|property| &property.kind)
        {
            for source in unit_source {
                let path = direct(source);
                if !paths.contains(&path) {
                    paths.push(path);
                }
            }
        }
    }
    paths
}

fn candidate(
    definition: MetricDefinition,
    dimensions: &[DimensionPath],
    conservation: Vec<ConservationSource>,
) -> MetricCandidate {
    MetricCandidate {
        definition,
        dimensions: dimensions.to_vec(),
        conservation,
        unit_partition: Vec::new(),
        time_grains: [
            TimeGrain::Day,
            TimeGrain::Week,
            TimeGrain::Month,
            TimeGrain::Quarter,
            TimeGrain::Year,
        ],
    }
}

// This recognizes a bounded structural equality, not an algebraic solver. Any
// subtraction, coefficient, conditional or non-property term is left unknown.
fn conservation_terms(expression: &str) -> Option<(PropertyId, Vec<PropertyId>)> {
    if expression.len() > 16_384 {
        return None;
    }
    let expression = unwrap(expression)?;
    let (left, right) = expression.split_once("==")?;
    let left = sum_terms(left, 0)?;
    let right = sum_terms(right, 0)?;
    let [input] = left.as_slice() else {
        return None;
    };
    if right.len() < 2
        || right.len() > 64
        || right.contains(input)
        || right.iter().collect::<BTreeSet<_>>().len() != right.len()
    {
        return None;
    }
    Some((input.clone(), right))
}

fn unwrap(mut text: &str) -> Option<&str> {
    text = text.trim();
    let mut wrappers = 0u8;
    while text.starts_with('(') {
        wrappers = wrappers.checked_add(1)?;
        if wrappers > 16 {
            return None;
        }
        let mut depth = 0u32;
        let mut end = None;
        for (index, byte) in text.bytes().enumerate() {
            match byte {
                b'(' => depth = depth.checked_add(1)?,
                b')' => {
                    depth = depth.checked_sub(1)?;
                    if depth == 0 {
                        end = Some(index);
                        break;
                    }
                }
                _ => {}
            }
        }
        if end?.checked_add(1)? != text.len() {
            break;
        }
        text = text.get(1..text.len().checked_sub(1)?)?.trim();
    }
    Some(text)
}

fn sum_terms(text: &str, depth: u8) -> Option<Vec<PropertyId>> {
    if depth > 64 {
        return None;
    }
    let text = unwrap(text)?;
    let mut nesting = 0u32;
    for (index, byte) in text.bytes().enumerate() {
        match byte {
            b'(' => nesting = nesting.checked_add(1)?,
            b')' => nesting = nesting.checked_sub(1)?,
            b'+' if nesting == 0 => {
                let mut terms = sum_terms(text.get(..index)?, depth.checked_add(1)?)?;
                terms.extend(sum_terms(
                    text.get(index.checked_add(1)?..)?,
                    depth.checked_add(1)?,
                )?);
                return (terms.len() <= 64).then_some(terms);
            }
            _ => {}
        }
    }
    let id = text.strip_prefix("self.")?;
    (id.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'))
    .then(|| vec![PropertyId(id.to_owned())])
}

#[cfg(test)]
mod ontology_derived_measures {
    #![allow(
        clippy::expect_used,
        clippy::indexing_slicing,
        reason = "typed fixture construction and assertions"
    )]

    use super::*;
    use norn_ir::metric::{
        CalendarPolicy, FilterOperator, FreshnessPolicy, MetricFilter, QuestionDimension,
        QuestionMetric, QuestionTimeWindow,
    };
    use norn_ir::{
        ArtifactDependency, ArtifactKind, EntityRootV2, ObjectTypeName, Property,
        PropertyConstraints, RuntimeObject, RuntimeValue, TimezonePolicy, UtcTimestamp,
        WholeNumber, ZenExpression,
    };

    fn payload<T: serde::Serialize>(kind: &ArtifactKind, value: &T) -> ArtifactPayload {
        ArtifactPayload::from_kind_json(
            kind,
            serde_json::from_value(serde_json::to_value(value).expect("typed JSON"))
                .expect("IR JSON"),
        )
        .expect("typed payload")
    }

    fn property(id: &str, kind: PropertyType) -> Property {
        Property {
            protected: false,
            id: PropertyId(id.to_owned()),
            description: None,
            property_type: kind,
            constraints: PropertyConstraints::default(),
            default: None,
            markings: Vec::new(),
            computed: None,
        }
    }

    fn fixture() -> Vec<ArtifactEnvelope> {
        let mut entities: Vec<ArtifactEnvelope> = serde_json::from_str(include_str!(
            "../../../../fixtures/ir/metrics/ontology.json"
        ))
        .expect("fixture");
        let mut entity = entities.remove(0);
        let mut root: EntityRootV2 = serde_json::from_value(
            serde_json::to_value(entity.payload.to_json_value()).expect("payload JSON"),
        )
        .expect("typed entity");
        root.object_type.name = ObjectTypeName("demo.Conversion".to_owned());
        root.object_type.links.clear();
        root.object_type.identity.clear();
        root.object_type.properties = ["input", "recovered", "loss"]
            .into_iter()
            .map(|id| {
                property(
                    id,
                    PropertyType::Decimal {
                        scale: WholeNumber(3),
                        unit: Some(norn_ir::UnitRef {
                            system: norn_ir::UnitSystem::Ucum,
                            code: "g".to_owned(),
                        }),
                        currency_source: None,
                        rounding: None,
                        per_unit: None,
                        additivity: PropertyAdditivity::Undeclared {},
                        unit_source: Vec::new(),
                    },
                )
            })
            .collect();
        root.object_type.properties.extend([
            property("name", PropertyType::Text { format: None }),
            property("metal", PropertyType::Text { format: None }),
            property(
                "effective_at",
                PropertyType::Timestamp {
                    timezone: TimezonePolicy::Utc,
                },
            ),
            property(
                "known_at",
                PropertyType::Timestamp {
                    timezone: TimezonePolicy::Utc,
                },
            ),
        ]);
        entity.artifact_id = "demo.Conversion.entity".to_owned();
        entity.namespace = "demo".to_owned();
        entity.name = "Conversion".to_owned();
        entity.payload = payload(&ArtifactKind::Entity, &root);
        let mut laws: Vec<ArtifactEnvelope> =
            serde_json::from_str(include_str!("../../../../fixtures/law-ir/laws.json"))
                .expect("law fixture");
        let mut artifact = laws.remove(0);
        let mut law: LawIR = serde_json::from_value(
            serde_json::to_value(artifact.payload.to_json_value()).expect("payload JSON"),
        )
        .expect("typed law");
        law.id = "demo.mass_conservation".to_owned();
        law.subject = "demo.Conversion".to_owned();
        law.modality = LawModality::Always {};
        law.scope.root = LawRoot::Entity("demo.Conversion".to_owned());
        law.scope.object_types = vec![law.subject.clone()];
        law.response = LawResponse::Predicate {
            expression: ZenExpression("self.input == self.recovered + self.loss".to_owned()),
        };
        artifact.artifact_id = "demo.mass_conservation.law".to_owned();
        artifact.scope = entity.scope.clone();
        artifact.dependencies = vec![ArtifactDependency {
            artifact_id: entity.artifact_id.clone(),
            kind: ArtifactKind::Entity,
            version: Some(entity.version.clone()),
            required: true,
        }];
        artifact.payload = payload(&ArtifactKind::Law, &law);
        vec![entity, artifact]
    }

    fn scope() -> LawRoot {
        LawRoot::Entity("demo.Conversion".to_owned())
    }

    fn space(artifacts: &[ArtifactEnvelope]) -> MetricSpace {
        derive_metric_space(artifacts, &BTreeSet::new(), &scope()).expect("derived space")
    }

    fn yield_question(space: &MetricSpace) -> QuestionIR {
        let candidate = space
            .candidates()
            .iter()
            .find(|candidate| {
                candidate
                    .definition
                    .label
                    .starts_with("sum(recovered) / sum(input)")
                    && candidate
                        .definition
                        .norn
                        .time_dimension
                        .as_ref()
                        .is_some_and(|path| path.property.as_str() == "effective_at")
            })
            .expect("yield candidate");
        QuestionIR {
            ir_version: norn_ir::IrVersion01::V0_1,
            name: "demo.scrap_yield".to_owned(),
            object_set: ObjectTypeName("demo.Conversion".to_owned()),
            search_around: Vec::new(),
            metrics: vec![QuestionMetric {
                metric: candidate.definition.name.clone(),
                inline: None,
                reaggregate: None,
            }],
            dimensions: vec![QuestionDimension {
                path: direct(&PropertyId("effective_at".to_owned())),
                grain: Some(TimeGrain::Week),
            }],
            filters: vec![MetricFilter {
                path: direct(&PropertyId("metal".to_owned())),
                operator: FilterOperator::Eq,
                value: RuntimeValue::from("gold"),
            }],
            time_window: QuestionTimeWindow {
                start: UtcTimestamp("2026-07-06T00:00:00Z".to_owned()),
                end: UtcTimestamp("2026-10-26T00:00:00Z".to_owned()),
            },
            calendar: CalendarPolicy::IsoUtc,
            comparison: None,
            freshness: FreshnessPolicy::Live {
                max_age_ms: WholeNumber(5000),
            },
        }
    }

    #[test]
    fn conservation_generates_yield_and_loss_without_metric_or_additivity_declarations() {
        let artifacts = fixture();
        let generated = space(&artifacts);
        assert_eq!(generated.candidates().len(), 10);
        assert!(generated.candidates().iter().any(|candidate| {
            candidate
                .definition
                .label
                .starts_with("sum(loss) / sum(input)")
        }));
        let question = yield_question(&generated);
        let plan = generated
            .admit(&question, 100)
            .expect("ordinary compute plan");
        assert_eq!(plan.grain().as_str(), "demo.Conversion");
        assert_eq!(
            plan.sums()
                .iter()
                .map(|sum| sum.property.id.as_str())
                .collect::<Vec<_>>(),
            ["recovered", "input"]
        );
        let raw = EffectiveCatalog::from_artifacts(&artifacts);
        let model = raw
            .object_type_named("demo.Conversion")
            .expect("model")
            .expect("available");
        assert!(
            plan.properties().iter().all(|property| model
                .property(property.id.as_str())
                .is_some_and(|actual| property.same_contract(actual))),
            "native model contracts must remain unchanged by conservation evidence"
        );
        let findings = compute::admit(plan.definitions(), &question, &raw, 100)
            .expect_err("raw undeclared terms remain closed");
        assert!(
            findings
                .iter()
                .any(|finding| finding.code.as_str() == codes::UNDECLARED_ADDITIVITY)
        );
    }

    #[test]
    fn conditional_advisory_non_always_or_other_scope_laws_do_not_supply_additivity() {
        for variant in 0..4 {
            let mut artifacts = fixture();
            let mut law: LawIR = serde_json::from_value(
                serde_json::to_value(artifacts[1].payload.to_json_value()).expect("payload JSON"),
            )
            .expect("law");
            match variant {
                0 => law.condition = Some(ZenExpression("self.metal == 'gold'".to_owned())),
                1 => law.severity = LawSeverity::Warning,
                2 => law.scope.root = LawRoot::App("another".to_owned()),
                _ => law.modality = LawModality::Never {},
            }
            artifacts[1].payload = payload(&ArtifactKind::Law, &law);
            assert!(space(&artifacts).candidates().is_empty());
        }
    }

    #[test]
    fn canonical_derivation_and_pins_are_independent_of_artifact_order() {
        let mut artifacts = fixture();
        let original = space(&artifacts);
        artifacts.reverse();
        let reordered = space(&artifacts);
        assert_eq!(
            original
                .candidates()
                .iter()
                .map(|candidate| &candidate.definition)
                .collect::<Vec<_>>(),
            reordered
                .candidates()
                .iter()
                .map(|candidate| &candidate.definition)
                .collect::<Vec<_>>()
        );
        let mut law: LawIR = serde_json::from_value(
            serde_json::to_value(artifacts[0].payload.to_json_value()).expect("payload JSON"),
        )
        .expect("law");
        law.rationale.push_str(" (reviewed)");
        artifacts[0].payload = payload(&ArtifactKind::Law, &law);
        assert_ne!(
            yield_question(&original).metrics,
            yield_question(&space(&artifacts)).metrics
        );
    }

    #[test]
    fn conservation_rates_do_not_supply_additivity() {
        let generated = space(&fixture());
        let Some(Ok(object)) = generated.catalog.object_type_named("demo.Conversion") else {
            panic!("conversion catalog")
        };
        let source = &generated.candidates()[0].conservation[0];
        assert!(compatible_terms(object, source));
        for id in [&source.input].into_iter().chain(&source.outputs) {
            let mut rate = object.clone();
            let property = rate
                .properties
                .iter_mut()
                .find(|property| &property.id == id)
                .expect("conservation term");
            let EffectiveKind::Base(PropertyType::Decimal { per_unit, .. }) = &mut property.kind
            else {
                panic!("decimal term")
            };
            *per_unit = Some(norn_ir::UnitRef {
                system: norn_ir::UnitSystem::Ucum,
                code: "kg".to_owned(),
            });
            assert!(!compatible_terms(&rate, source), "{id:?}");
        }
    }

    #[test]
    fn scrap_yield_contributions_equal_the_existing_compute_path_on_all_80_rows() {
        let artifacts = fixture();
        let generated = space(&artifacts);
        let question = yield_question(&generated);
        let plan = generated.admit(&question, 100).expect("derived plan");
        let mut root: EntityRootV2 = serde_json::from_value(
            serde_json::to_value(artifacts[0].payload.to_json_value()).expect("payload JSON"),
        )
        .expect("entity");
        for property in &mut root.object_type.properties {
            if let PropertyType::Decimal { additivity, .. } = &mut property.property_type {
                *additivity = PropertyAdditivity::Additive {};
            }
        }
        let mut declared = artifacts;
        declared[0].payload = payload(&ArtifactKind::Entity, &root);
        let existing = compute::admit(
            plan.definitions(),
            &question,
            &EffectiveCatalog::from_artifacts(&declared),
            100,
        )
        .expect("existing plan");
        let mut showcase = [0i128; 2];
        let mut count = 0;
        for line in include_str!("../../../../fixtures/demo/scrap-yield-k1.csv")
            .lines()
            .skip(1)
        {
            let columns: Vec<_> = line.split(',').collect();
            let mass = |column: usize| {
                let value: i64 = columns[column].parse().expect("integer milligrams");
                let decimal: norn_ir::Decimal = format!("{}.{:03}", value / 1000, value % 1000)
                    .parse()
                    .expect("exact grams");
                RuntimeValue::Number(decimal)
            };
            let row = RuntimeObject::from([
                ("name".to_owned(), RuntimeValue::from(columns[2])),
                ("metal".to_owned(), RuntimeValue::from(columns[3])),
                ("input".to_owned(), mass(4)),
                ("recovered".to_owned(), mass(5)),
                ("loss".to_owned(), mass(6)),
                ("effective_at".to_owned(), RuntimeValue::from(columns[8])),
            ]);
            let actual = compute::contribution::project(&plan, &row).expect("derived contribution");
            assert_eq!(
                actual,
                compute::contribution::project(&existing, &row).expect("existing contribution")
            );
            if columns[0] == "15"
                && let Some(contribution) = actual
            {
                for (total, amount) in showcase.iter_mut().zip(contribution.amounts) {
                    *total = compute::arithmetic::add(*total, amount.expect("sum term"))
                        .expect("exact addition");
                }
            }
            count += 1;
        }
        assert_eq!(count, 80);
        assert_eq!(showcase, [294_000_000, 300_000_000]);
        assert_eq!(
            compute::arithmetic::ratio(showcase[0], showcase[1]),
            Some(980_000)
        );
    }

    #[test]
    fn explicit_ratio_properties_derive_from_their_declared_operands() {
        let mut artifacts = fixture();
        artifacts.truncate(1);
        let mut root: EntityRootV2 = serde_json::from_value(
            serde_json::to_value(artifacts[0].payload.to_json_value()).expect("payload JSON"),
        )
        .expect("entity");
        for property in &mut root.object_type.properties {
            if let PropertyType::Decimal { additivity, .. } = &mut property.property_type {
                *additivity = PropertyAdditivity::Additive {};
            }
        }
        root.object_type.properties.push(property(
            "yield_ratio",
            PropertyType::Decimal {
                scale: WholeNumber(6),
                rounding: None,
                unit: None,
                currency_source: None,
                per_unit: None,
                additivity: PropertyAdditivity::Ratio {
                    numerator: PropertyId("recovered".to_owned()),
                    denominator: PropertyId("input".to_owned()),
                },
                unit_source: Vec::new(),
            },
        ));
        artifacts[0].payload = payload(&ArtifactKind::Entity, &root);
        let generated = space(&artifacts);
        let plan = generated
            .admit(&yield_question(&generated), 100)
            .expect("declared ratio terms");
        assert_eq!(
            plan.sums()
                .iter()
                .map(|sum| sum.property.id.as_str())
                .collect::<Vec<_>>(),
            ["recovered", "input"]
        );
        assert!(
            generated
                .candidates()
                .iter()
                .all(|candidate| candidate.conservation.is_empty()
                    && !candidate.definition.norn.certified)
        );
    }

    #[test]
    fn conservation_cannot_replace_an_explicit_non_additive_or_balance_contract() {
        for class in [
            PropertyAdditivity::NonAdditive {},
            PropertyAdditivity::SemiAdditive {},
            PropertyAdditivity::Ratio {
                numerator: PropertyId("recovered".to_owned()),
                denominator: PropertyId("loss".to_owned()),
            },
        ] {
            let mut artifacts = fixture();
            let mut root: EntityRootV2 = serde_json::from_value(
                serde_json::to_value(artifacts[0].payload.to_json_value()).expect("payload JSON"),
            )
            .expect("entity");
            let PropertyType::Decimal { additivity, .. } =
                &mut root.object_type.properties[0].property_type
            else {
                panic!("input quantity")
            };
            let balance = matches!(class, PropertyAdditivity::SemiAdditive {});
            *additivity = class;
            artifacts[0].payload = payload(&ArtifactKind::Entity, &root);
            let space = space(&artifacts);
            // A balance yields only its as-of snapshots (METRIC-SNAPSHOT-ASOF-1), never a sum or ratio.
            assert_eq!(!space.candidates().is_empty(), balance);
            assert!(space.candidates().iter().all(|candidate| {
                matches!(candidate.definition.norn.additivity, MetricAdditivity::SemiAdditive { .. })
                    && candidate.definition.type_params.expr.as_deref() == Some("input")
            }));
        }
    }

    #[test]
    fn conservation_term_count_accepts_18_and_64_but_refuses_65() {
        for count in [18usize, 64, 65] {
            let terms = (0..count)
                .map(|index| format!("self.output{index}"))
                .collect::<Vec<_>>()
                .join(" + ");
            let derived = conservation_terms(&format!("self.input == {terms}"));
            if count <= 64 {
                assert_eq!(derived.expect("bounded terms").1.len(), count);
            } else {
                assert!(derived.is_none());
            }
        }
    }

    #[test]
    fn nonlinear_duplicate_or_conditional_equalities_do_not_derive() {
        for expression in [
            "self.input == self.recovered * self.loss",
            "self.input == self.loss + self.loss",
            "self.input == self.input + self.loss",
            "self.input >= self.recovered + self.loss",
            "self.input == self.recovered + self.loss or true",
            "self.input == self.recovered + self.loss + 0",
        ] {
            assert!(conservation_terms(expression).is_none(), "{expression}");
        }
    }

    proptest::proptest! {
        #[test]
        fn structural_conservation_preserves_terms(a in "[a-z]{1,12}", b in "[a-z]{1,12}", c in "[a-z]{1,12}", wrappers in 0usize..8) {
            proptest::prop_assume!(a != b && a != c && b != c);
            let expression = format!("{}self.{a} == (self.{b} + self.{c}){}", "(".repeat(wrappers), ")".repeat(wrappers));
            proptest::prop_assert_eq!(conservation_terms(&expression), Some((PropertyId(a), vec![PropertyId(b), PropertyId(c)])));
        }
    }
}
