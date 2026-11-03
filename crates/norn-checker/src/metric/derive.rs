//! Ontology-derived metric candidates, checked through the existing compute path.

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::metric::{
    DimensionPath, MeasureAggregate, MetricAdditivity, MetricAggregationParams, MetricNorn,
    MetricType, MetricTypeParams, TimeGrain,
};
use norn_ir::{
    ArtifactEnvelope, MetricDefinition, PropertyAdditivity, PropertyId, PropertyType,
    PropertyTypeName, QuestionIR,
};

use crate::entity_instance::{EffectiveCatalog, EffectiveKind, EffectiveObjectType};
use crate::{Diagnostic, Span};

use super::{codes, compute, field_name};

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
}

/// Generated candidates and their unchanged native catalog.
/// Selections pass through the existing compute admission path.
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
        let definitions: Vec<_> = selected
            .into_iter()
            .filter_map(|name| by_name.get(name))
            .map(|definition| (**definition).clone())
            .collect();
        compute::admit(&definitions, question, &self.catalog, max_items)
    }
}

/// METRIC-DECLARED-SPACE-1: enumerate declared additive sums, dimensions and grains.
/// Units and missing declarations never grant aggregation authority.
///
/// # Errors
/// Returns the existing complexity diagnostic above 16,384 candidates.
pub fn derive_declared_metric_space(
    artifacts: &[ArtifactEnvelope],
    registry: &BTreeSet<PropertyTypeName>,
) -> Result<MetricSpace, Vec<Diagnostic>> {
    let catalog = EffectiveCatalog::from_artifacts_with_registry(artifacts, registry);
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
            let time = direct(&time.id);
            for property in object
                .properties
                .iter()
                .filter(|property| super::is_numeric(property))
            {
                let EffectiveKind::Base(kind) = &property.kind else {
                    continue;
                };
                if !matches!(kind.additivity(), PropertyAdditivity::Additive {}) {
                    continue;
                }
                candidates.push(candidate(sum(object, &property.id, &time), &dimensions));
                if candidates.len() > 16_384 {
                    return Err(vec![crate::property_types::error(
                        codes::COMPLEXITY_LIMIT,
                        "Derived metric space exceeds 16384 candidates".to_owned(),
                        Span::pointer("/metrics"),
                    )]);
                }
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
    let forward = catalog
        .links_for_object_type(object.name.as_str())
        .unwrap_or_default();
    let mut roles: BTreeMap<_, BTreeSet<_>> = forward
        .iter()
        .map(|link| {
            (
                link.id.clone(),
                catalog
                    .target_types(link.target.as_str())
                    .unwrap_or_default(),
            )
        })
        .collect();
    for source in catalog.object_type_names() {
        for link in catalog.links_for_object_type(source).into_iter().flatten() {
            if let Some(role) = &link.inverse
                && !forward.iter().any(|link| &link.id == role)
                && catalog
                    .target_types(link.target.as_str())
                    .is_some_and(|targets| targets.contains(object.name.as_str()))
            {
                roles
                    .entry(role.clone())
                    .or_default()
                    .insert(source.to_owned());
            }
        }
    }
    for (role, targets) in roles {
        for target in targets {
            let Some(Ok(target)) = catalog.object_type_named(&target) else {
                continue;
            };
            for property in target.properties.iter().filter(|p| super::is_scalar(p)) {
                paths.insert(DimensionPath {
                    links: vec![role.clone()],
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
) -> MetricDefinition {
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
        label: format!("sum({})", property.as_str()),
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

fn encoded_name(name: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    name.bytes()
        .flat_map(|byte| [byte >> 4, byte & 15])
        .filter_map(|digit| HEX.get(usize::from(digit)).map(|byte| char::from(*byte)))
        .collect()
}

fn candidate(definition: MetricDefinition, dimensions: &[DimensionPath]) -> MetricCandidate {
    MetricCandidate {
        definition,
        dimensions: dimensions.to_vec(),
        time_grains: [
            TimeGrain::Day,
            TimeGrain::Week,
            TimeGrain::Month,
            TimeGrain::Quarter,
            TimeGrain::Year,
        ],
    }
}

#[cfg(test)]
mod ontology_derived_measures {
    #![allow(
        clippy::expect_used,
        clippy::indexing_slicing,
        reason = "typed fixture assertions"
    )]
    use super::*;
    use norn_ir::{ArtifactKind, ArtifactPayload, EntityRootV2};

    fn fixture() -> Vec<ArtifactEnvelope> {
        serde_json::from_str(include_str!(
            "../../../../fixtures/ir/metrics/ontology.json"
        ))
        .expect("typed ontology")
    }
    fn derive(artifacts: &[ArtifactEnvelope]) -> MetricSpace {
        derive_declared_metric_space(
            artifacts,
            &BTreeSet::from([PropertyTypeName("iso4217.currency_code".into())]),
        )
        .expect("bounded catalog")
    }

    proptest::proptest! {
    #[test]
    fn declared_space_is_deterministic_and_exposes_dimensions_and_grains(rotations in 0usize..5, reverse in proptest::bool::ANY) {
        let mut artifacts = fixture();
        let original = derive(&artifacts);
        artifacts.rotate_left(rotations);
        if reverse { artifacts.reverse(); }
        let reordered = derive(&artifacts);
        assert_eq!(original.candidates.len(), 1);
        let candidate = &original.candidates[0];
        assert_eq!(
            candidate.definition.type_params.expr.as_deref(),
            Some("grand_total")
        );
        assert_eq!(candidate.definition, reordered.candidates[0].definition);
        assert_eq!(candidate.time_grains.len(), 5);
        assert!(
            candidate
                .dimensions
                .iter()
                .any(|path| path.links.len() == 1)
        );
        assert!(!candidate.definition.norn.certified);
        let question: QuestionIR = serde_json::from_str(include_str!(
            "../../../../fixtures/ir/metrics/sales-by-store-day.question.json"
        ))
        .expect("question");
        assert!(
            original.admit(&question, 100).is_err(),
            "an unknown selection cannot execute"
        );
    }

    }

    #[test]
    fn inverse_dimensions_preserve_forward_role_precedence() {
        for (role, expected) in [("invoice_lines", true), ("store", false)] {
            let mut artifacts = fixture();
            let mut root: EntityRootV2 = serde_json::from_value(
                serde_json::to_value(artifacts[0].payload.to_json_value()).expect("payload"),
            )
            .expect("entity");
            root.object_type.links[0].inverse = Some(norn_ir::LinkId(role.to_owned()));
            artifacts[0].payload = ArtifactPayload::from_kind_json(
                &ArtifactKind::Entity,
                serde_json::from_value(serde_json::to_value(root).expect("typed entity"))
                    .expect("IR JSON"),
            )
            .expect("entity payload");
            let generated = derive(&artifacts);
            let has = |property: &str| {
                generated
                    .candidates
                    .iter()
                    .flat_map(|candidate| &candidate.dimensions)
                    .any(|path| {
                        path.links == [norn_ir::LinkId(role.to_owned())]
                            && path.property.as_str() == property
                    })
            };
            assert!(has("name"));
            assert_eq!(has("idx"), expected, "{role}");
        }
    }

    #[test]
    fn declared_space_never_infers_additivity_from_units_or_other_contracts() {
        for class in [
            PropertyAdditivity::Undeclared {},
            PropertyAdditivity::SemiAdditive {},
            PropertyAdditivity::NonAdditive {},
            PropertyAdditivity::Ratio {
                numerator: PropertyId("grand_total".into()),
                denominator: PropertyId("grand_total".into()),
            },
        ] {
            let mut artifacts = fixture();
            let mut root: EntityRootV2 = serde_json::from_value(
                serde_json::to_value(artifacts[1].payload.to_json_value()).expect("payload"),
            )
            .expect("typed entity");
            let property = root
                .object_type
                .properties
                .iter_mut()
                .find(|property| property.id.as_str() == "grand_total")
                .expect("amount");
            let PropertyType::Decimal { additivity, .. } = &mut property.property_type else {
                unreachable!("fixture amount")
            };
            *additivity = class;
            artifacts[1].payload = ArtifactPayload::from_kind_json(
                &ArtifactKind::Entity,
                serde_json::from_value(serde_json::to_value(root).expect("typed payload"))
                    .expect("JSON object"),
            )
            .expect("entity payload");
            assert!(derive(&artifacts).candidates.is_empty());
        }
    }
}
