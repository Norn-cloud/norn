//! Structural conservation evidence from admitted laws; no aggregation authority.
use std::collections::BTreeSet;

use norn_ir::{
    ArtifactEnvelope, ArtifactPayload, LawIR, LawModality, LawResponse, LawRoot, LawSeverity,
    PropertyId, PropertyTypeName,
};

use crate::{Diagnostic, EffectiveObjectType};

/// An immutable law pin and its structurally extracted conservation terms.
/// Consumers must still check quantity contracts before admitting aggregation.
#[derive(Debug, Clone, PartialEq)]
pub struct ConservationEvidence {
    pub(crate) law: LawIR,
    pub(crate) digest: String,
    pub(crate) total: PropertyId,
    pub(crate) terms: Vec<PropertyId>,
}

impl ConservationEvidence {
    /// Admitted law, including its exact applicability and provenance.
    #[must_use]
    pub const fn law(&self) -> &LawIR {
        &self.law
    }

    /// Norn1 digest of the complete source artifact, not only its predicate.
    #[must_use]
    pub fn digest(&self) -> &str {
        &self.digest
    }

    /// Left-hand total in the structural equality.
    #[must_use]
    pub const fn total(&self) -> &PropertyId {
        &self.total
    }

    /// Distinct right-hand properties, in expression order.
    #[must_use]
    pub fn terms(&self) -> &[PropertyId] {
        &self.terms
    }

    /// Both the law subject and its object scope must cover the effective type.
    #[must_use]
    pub fn applies_to(&self, object: &EffectiveObjectType) -> bool {
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
        matches(&self.law.subject) && self.law.scope.object_types.iter().any(|name| matches(name))
    }
}

/// METRIC-LAW-EVIDENCE-1: extract only unconditional, always/error structural
/// equalities at an explicit law root. Preserve the complete law artifact pin.
/// This is structural evidence, never a rewrite of a native property contract.
///
/// # Errors
/// Returns existing law-set diagnostics before extracting any evidence.
pub fn conservation_evidence(
    artifacts: &[ArtifactEnvelope],
    registry: &BTreeSet<PropertyTypeName>,
    scope: &LawRoot,
) -> Result<Vec<ConservationEvidence>, Vec<Diagnostic>> {
    let findings = crate::verification::check_law_set(artifacts, &[], registry);
    if !findings.is_empty() {
        return Err(findings);
    }
    let mut sources: Vec<_> = artifacts
        .iter()
        .filter_map(|artifact| extract(artifact, scope))
        .collect();
    sources.sort_by(|a, b| a.law.id.cmp(&b.law.id).then(a.digest.cmp(&b.digest)));
    sources.dedup();
    Ok(sources)
}

fn extract(artifact: &ArtifactEnvelope, scope: &LawRoot) -> Option<ConservationEvidence> {
    if !matches!(artifact.payload, ArtifactPayload::Law(_)) {
        return None;
    }
    let law: LawIR =
        serde_json::from_value(serde_json::to_value(artifact.payload.to_json_value()).ok()?)
            .ok()?;
    if &law.scope.root != scope
        || law.condition.is_some()
        || law.timing.is_some()
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
    let equality = norn_expr::structural_sum_equality(expression.as_str()).ok()??;
    let bytes = norn_canonical::canonical_json_of(artifact).ok()?;
    let digest = norn_canonical::HashProfile::Norn1.hash_canonical_bytes(&bytes);
    Some(ConservationEvidence {
        law,
        digest,
        total: equality.total,
        terms: equality.terms,
    })
}

#[cfg(test)]
mod ontology_derived_measures {
    #![allow(
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing,
        reason = "typed law-evidence fixture assertions"
    )]
    use super::*;
    use crate::EffectiveCatalog;
    use norn_ir::{
        ArtifactDependency, ArtifactKind, EntityRootV2, ObjectTypeName, PropertyConstraints,
        PropertyType, ZenExpression,
    };

    fn payload<T: serde::Serialize>(kind: &ArtifactKind, value: &T) -> ArtifactPayload {
        ArtifactPayload::from_kind_json(
            kind,
            serde_json::from_value(serde_json::to_value(value).expect("typed payload"))
                .expect("IR JSON"),
        )
        .expect("payload")
    }

    fn fixture() -> Vec<ArtifactEnvelope> {
        let mut entities: Vec<ArtifactEnvelope> = serde_json::from_str(include_str!(
            "../../../../fixtures/ir/metrics/ontology.json"
        ))
        .expect("ontology");
        let mut entity = entities.remove(0);
        let mut root: EntityRootV2 = serde_json::from_value(
            serde_json::to_value(entity.payload.to_json_value()).expect("payload"),
        )
        .expect("entity");
        let quantity = root
            .object_type
            .properties
            .iter()
            .find(|property| matches!(property.property_type, PropertyType::Integer { .. }))
            .expect("integer property")
            .clone();
        root.object_type.name = ObjectTypeName("demo.Conversion".to_owned());
        root.object_type.links.clear();
        root.object_type.identity.clear();
        root.object_type.properties = ["input", "recovered", "loss"]
            .map(|id| {
                let mut property = quantity.clone();
                property.id = PropertyId(id.to_owned());
                property.constraints = PropertyConstraints::default();
                property
            })
            .to_vec();
        entity.artifact_id = "demo.Conversion.entity".to_owned();
        entity.payload = payload(&ArtifactKind::Entity, &root);
        let mut laws: Vec<ArtifactEnvelope> =
            serde_json::from_str(include_str!("../../../../fixtures/law-ir/laws.json"))
                .expect("laws");
        let mut artifact = laws.remove(0);
        let mut law: LawIR = serde_json::from_value(
            serde_json::to_value(artifact.payload.to_json_value()).expect("payload"),
        )
        .expect("law");
        law.id = "demo.mass_conservation".to_owned();
        law.subject = root.object_type.name.0.clone();
        law.modality = LawModality::Always {};
        law.scope.root = LawRoot::Entity(law.subject.clone());
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

    fn edit(artifacts: &mut [ArtifactEnvelope], change: impl FnOnce(&mut LawIR)) {
        let mut law: LawIR = serde_json::from_value(
            serde_json::to_value(artifacts[1].payload.to_json_value()).expect("payload"),
        )
        .expect("law");
        change(&mut law);
        artifacts[1].payload = payload(&ArtifactKind::Law, &law);
    }

    fn sources(artifacts: &[ArtifactEnvelope]) -> Vec<ConservationEvidence> {
        conservation_evidence(
            artifacts,
            &BTreeSet::new(),
            &LawRoot::Entity("demo.Conversion".to_owned()),
        )
        .expect("admitted laws")
    }

    #[test]
    fn admitted_conservation_retains_terms_scope_and_entire_envelope_pin() {
        let artifacts = fixture();
        let evidence = sources(&artifacts);
        assert_eq!(evidence.len(), 1);
        let source = &evidence[0];
        assert_eq!(source.total().as_str(), "input");
        assert_eq!(
            source
                .terms()
                .iter()
                .map(PropertyId::as_str)
                .collect::<Vec<_>>(),
            ["recovered", "loss"]
        );
        let catalog = EffectiveCatalog::from_artifacts(&artifacts);
        assert!(
            source.applies_to(
                catalog
                    .object_type_named("demo.Conversion")
                    .expect("type")
                    .expect("available")
            )
        );
        let mut changed = artifacts;
        changed[1].name.push_str(" reviewed");
        assert_ne!(source.digest(), sources(&changed)[0].digest());
        assert_eq!(source.law().id, "demo.mass_conservation");
    }

    #[test]
    fn conditional_advisory_non_always_and_other_root_supply_no_evidence() {
        for variant in 0..4 {
            let mut artifacts = fixture();
            edit(&mut artifacts, |law| match variant {
                0 => law.condition = Some(ZenExpression("self.input > 0".to_owned())),
                1 => law.severity = LawSeverity::Warning,
                2 => law.modality = LawModality::Never {},
                _ => law.scope.root = LawRoot::App("other".to_owned()),
            });
            assert!(sources(&artifacts).is_empty(), "variant {variant}");
        }
    }

    #[test]
    fn nonlinear_or_repeated_terms_supply_no_evidence() {
        for expression in [
            "self.input == self.recovered * self.loss",
            "self.input == self.loss + self.loss",
            "self.input == self.input + self.loss",
        ] {
            let mut artifacts = fixture();
            edit(&mut artifacts, |law| {
                law.response = LawResponse::Predicate {
                    expression: ZenExpression(expression.to_owned()),
                }
            });
            assert!(sources(&artifacts).is_empty(), "{expression}");
        }
    }

    #[test]
    fn invalid_law_refuses_before_evidence_is_returned() {
        let mut artifacts = fixture();
        edit(&mut artifacts, |law| law.owner.clear());
        assert!(
            conservation_evidence(
                &artifacts,
                &BTreeSet::new(),
                &LawRoot::Entity("demo.Conversion".to_owned())
            )
            .is_err()
        );
    }

    proptest::proptest! {
        #[test]
        fn law_evidence_pins_are_independent_of_artifact_order(reverse in proptest::bool::ANY) {
            let mut artifacts = fixture();
            let original = sources(&artifacts);
            if reverse { artifacts.reverse(); }
            proptest::prop_assert_eq!(original, sources(&artifacts));
        }
    }
}
