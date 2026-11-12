//! PATH-MAP-1: preservation admission for declared path mappings (#712).
//!
//! A [`PathMapping`] `F: target -> source` lowers, with both catalogs' path
//! equations, to an [`Exchange`]. Preservation is first attempted by chase
//! certificates (`preservation.rs`, #718 U7): the production check is proved
//! (`Checker/Preservation/Proof.lean`, `check_sound`) to accept only
//! certificates that imply preservation on every source instance.
//! Otherwise it is the existing bounded schema-compatibility search, whose
//! clean result is only bounded evidence. The exchange is validated and the
//! certificate search is charged to the admission's work before either
//! answer. No rule is hand-authored by the caller.

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::entity::{LinkImage, PathMapping};
use norn_ir::{ArtifactEnvelope, Multiplicity, PropertyTypeName};

use super::chase::{AdmissionWork, ChaseBudget};
use super::exchange::{Exchange, check_schema_compatibility_with_work};
use super::theory::{
    Atom, Dependency, Egd, RuleOrigin, Term, Tgd, Theory, equation_path, lower_path_equations,
};
use crate::entity_instance::EffectiveCatalog;
use crate::{Diagnostic, DiagnosticSeverity};

/// Refusal code for malformed mappings and compositions.
pub const PATH_MAPPING_INVALID: &str = "consistency.path_mapping.invalid";

/// The kind of evidence behind an admitted mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum PreservationClass {
    /// Every lowered target dependency has a chase certificate the proved
    /// check accepted, so it holds on every source instance satisfying the
    /// source theory.
    Certified,
    /// The exchange search found no counterexample within its bound.
    BoundedNoCounterexample,
    /// The target declares no dependency, so there is nothing to preserve.
    NoTargetDependencies,
}

impl PreservationClass {
    /// The class as it appears at the start of the evidence message.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Certified => "certified",
            Self::BoundedNoCounterexample => "bounded-no-counterexample",
            Self::NoTargetDependencies => "no target dependencies",
        }
    }
}

/// What an admitted mapping establishes.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct PreservationEvidence {
    /// Target dependencies checked (path equations and functional links).
    pub obligations: usize,
    /// Dependencies in the lowered exchange.
    pub dependencies: usize,
    /// Whether the obligations are certified or only searched.
    pub evidence: PreservationClass,
}

fn var(name: &str) -> Term {
    Term::Var(name.to_owned())
}

fn atom(relation: String, terms: Vec<Term>) -> Atom {
    Atom { relation, terms }
}

fn refuse(mapping: &str, message: &str) -> Vec<Diagnostic> {
    vec![super::finding(
        PATH_MAPPING_INVALID,
        DiagnosticSeverity::Error,
        format!("path mapping {mapping:?}: {message}"),
        Some(&origin(mapping)),
    )]
}

fn origin(mapping: &str) -> RuleOrigin {
    RuleOrigin {
        artifact_id: None,
        pointer: format!("/pathMappings/{mapping}"),
    }
}

/// Concepts reachable from `roots` along functional links, sorted.
fn reachable(catalog: &EffectiveCatalog, roots: impl Iterator<Item = String>) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut stack: Vec<String> = roots.collect();
    while let Some(concept) = stack.pop() {
        if seen.insert(concept.clone()) {
            for link in catalog.links_for_object_type(&concept).unwrap_or_default() {
                if link.cardinality.to == Multiplicity::One {
                    stack.push(link.target.as_str().to_owned());
                }
            }
        }
    }
    seen
}

/// Functional EGDs for every to-one link of `concepts`, plus existential
/// totality TGDs for required ones when `total`.
fn link_contracts(
    catalog: &EffectiveCatalog,
    concepts: &BTreeSet<String>,
    prefix: &str,
    total: bool,
) -> Theory {
    let mut theory = Theory::default();
    for concept in concepts {
        for link in catalog.links_for_object_type(concept).unwrap_or_default() {
            if link.cardinality.to != Multiplicity::One {
                continue;
            }
            let relation = format!("link:{concept}:{}", link.id.as_str());
            let id = format!("{prefix}:{concept}.{}", link.id.as_str());
            theory.dependencies.push(Dependency::Egd(Egd {
                id: format!("{id}:functional"),
                origin: origin(prefix),
                body: vec![
                    atom(relation.clone(), vec![var("x"), var("y")]),
                    atom(relation.clone(), vec![var("x"), var("z")]),
                ],
                left: var("y"),
                right: var("z"),
            }));
            if total && link.cardinality.required {
                theory.dependencies.push(Dependency::Tgd(Tgd {
                    id: format!("{id}:total"),
                    origin: origin(prefix),
                    body: vec![atom(format!("concept:{concept}"), vec![var("x")])],
                    head: vec![atom(relation, vec![var("x"), var("y")])],
                }));
            }
        }
    }
    theory
}

/// One TGD per object and link image of `mapping`, refusing ill-typed images
/// and catalogs that share a concept name.
fn lower_mapping(
    source_catalog: &EffectiveCatalog,
    target_catalog: &EffectiveCatalog,
    mapping: &PathMapping,
) -> Result<Theory, Vec<Diagnostic>> {
    let concrete = |catalog: &EffectiveCatalog, name: &str| {
        matches!(catalog.object_type_named(name), Some(Ok(_)))
    };
    for concept in target_catalog.object_type_names() {
        if !mapping.objects.contains_key(concept) {
            return Err(refuse(
                &mapping.id,
                &format!("{concept} has no object image"),
            ));
        }
    }
    let mut lowered = Theory::default();
    for (to, from) in &mapping.objects {
        if !concrete(target_catalog, to) || !concrete(source_catalog, from) {
            return Err(refuse(
                &mapping.id,
                &format!("{to} -> {from} is not between concrete types"),
            ));
        }
        if concrete(source_catalog, to) || concrete(target_catalog, from) {
            return Err(refuse(
                &mapping.id,
                &format!("{to} -> {from}: catalogs must be disjoint"),
            ));
        }
        lowered.dependencies.push(Dependency::Tgd(Tgd {
            id: format!("{}:object:{to}", mapping.id),
            origin: origin(&mapping.id),
            body: vec![atom(format!("concept:{from}"), vec![var("root")])],
            head: vec![atom(format!("concept:{to}"), vec![var("root")])],
        }));
    }
    for (
        index,
        LinkImage {
            concept,
            link,
            path,
        },
    ) in mapping.links.iter().enumerate()
    {
        let image = |message: String| refuse(&mapping.id, &format!("{concept}.{link}: {message}"));
        let declared = target_catalog
            .links_for_object_type(concept)
            .and_then(|links| links.iter().find(|l| l.id.as_str() == link))
            .filter(|l| l.cardinality.to == Multiplicity::One)
            .ok_or_else(|| image("not a functional target link".into()))?;
        let (Some(from), Some(end)) = (
            mapping.objects.get(concept),
            mapping.objects.get(declared.target.as_str()),
        ) else {
            return Err(image("concept or link target is not mapped".into()));
        };
        let mut body = vec![atom(format!("concept:{from}"), vec![var("root")])];
        let (reached, last) =
            equation_path(source_catalog, from, path, "p", &mut body).map_err(image)?;
        if reached != *end {
            return Err(image(format!("source path ends at {reached}, not {end}")));
        }
        lowered.dependencies.push(Dependency::Tgd(Tgd {
            id: format!("{}:link:{concept}.{link}:{index}", mapping.id),
            origin: origin(&mapping.id),
            body,
            head: vec![atom(
                format!("link:{concept}:{link}"),
                vec![var("root"), last],
            )],
        }));
    }
    for concept in mapping.objects.keys() {
        for link in target_catalog
            .links_for_object_type(concept)
            .unwrap_or_default()
        {
            if link.cardinality.to == Multiplicity::One
                && !mapping
                    .links
                    .iter()
                    .any(|image| image.concept == *concept && image.link == link.id.as_str())
            {
                return Err(refuse(
                    &mapping.id,
                    &format!("{concept}.{} has no image", link.id.as_str()),
                ));
            }
        }
    }
    Ok(lowered)
}

/// Check that `mapping` preserves every target path equation and functional
/// link, given the source catalog's own equations, functional links and
/// required links. Refuses malformed mappings; returns every verified
/// counterexample as `consistency.schema.unpreserved_constraint`.
///
/// # Errors
/// Malformed declarations, refused searches, or counterexample witnesses.
pub fn admit_path_mapping(
    source: &[ArtifactEnvelope],
    target: &[ArtifactEnvelope],
    mapping: &PathMapping,
    registry: &BTreeSet<PropertyTypeName>,
    budget: ChaseBudget,
) -> Result<PreservationEvidence, Vec<Diagnostic>> {
    admit_with_work(
        source,
        target,
        mapping,
        registry,
        budget,
        &AdmissionWork::new(10_000_000),
    )
}

pub(crate) fn admit_with_work(
    source: &[ArtifactEnvelope],
    target: &[ArtifactEnvelope],
    mapping: &PathMapping,
    registry: &BTreeSet<PropertyTypeName>,
    budget: ChaseBudget,
    work: &AdmissionWork,
) -> Result<PreservationEvidence, Vec<Diagnostic>> {
    // Charge input bytes before lowering; large theories and long paths
    // consume the same admission allowance as search and unfolding work.
    norn_canonical::meter_json_verbatim(work, &(source, target, mapping)).map_err(|error| {
        vec![super::refusal(
            &Theory::default(),
            &super::chase::ChaseError::Internal(error.to_string()),
        )]
    })?;
    let source_catalog = EffectiveCatalog::from_artifacts_with_registry(source, registry);
    let target_catalog = EffectiveCatalog::from_artifacts_with_registry(target, registry);
    let mut source_theory = lower_path_equations(source, registry)?;
    let mut target_theory = lower_path_equations(target, registry)?;
    let lowered = lower_mapping(&source_catalog, &target_catalog, mapping)?;
    let sources = reachable(&source_catalog, mapping.objects.values().cloned());
    let targets = reachable(&target_catalog, mapping.objects.keys().cloned());
    if let Some(concept) = sources.intersection(&targets).next() {
        return Err(refuse(
            &mapping.id,
            &format!("{concept}: catalogs must be disjoint"),
        ));
    }
    source_theory
        .dependencies
        .extend(link_contracts(&source_catalog, &sources, "source", true).dependencies);
    target_theory
        .dependencies
        .extend(link_contracts(&target_catalog, &targets, "target", false).dependencies);
    let mut schema = BTreeMap::new();
    for concept in &sources {
        schema.insert(format!("concept:{concept}"), vec!["id".to_owned()]);
        for link in source_catalog
            .links_for_object_type(concept)
            .unwrap_or_default()
        {
            if link.cardinality.to == Multiplicity::One {
                schema.insert(
                    format!("link:{concept}:{}", link.id.as_str()),
                    vec!["from".into(), "to".into()],
                );
            }
        }
    }
    let dependencies = [&source_theory, &lowered, &target_theory]
        .iter()
        .map(|theory| theory.dependencies.len())
        .sum();
    let exchange = Exchange {
        source_schema: &schema,
        source: &source_theory,
        mapping: &lowered,
        target: &target_theory,
    };
    let obligations = target_theory.dependencies.len();
    if obligations > 0 {
        // Validate before certifying, so a malformed exchange refuses as it
        // does under the search. A certified mapping preserves every target
        // dependency on every source instance, so the search is skipped.
        exchange
            .validate()
            .map_err(|error| vec![super::refusal(&lowered, &error)])?;
        if super::preservation::certified(&source_theory, &lowered, &target_theory, work)
            .map_err(|error| vec![super::refusal(&lowered, &error)])?
        {
            return Ok(PreservationEvidence {
                obligations,
                dependencies,
                evidence: PreservationClass::Certified,
            });
        }
    }
    let findings = check_schema_compatibility_with_work(&exchange, budget, work);
    if findings.is_empty() {
        Ok(PreservationEvidence {
            obligations,
            dependencies,
            evidence: if obligations == 0 {
                PreservationClass::NoTargetDependencies
            } else {
                PreservationClass::BoundedNoCounterexample
            },
        })
    } else {
        Err(findings)
    }
}

/// The composite `first ; then` of `first: target -> middle` and
/// `then: middle -> source`, substituting `then`'s images into `first`'s
/// paths. Non-functional components refuse rather than composing.
///
/// # Errors
/// A component with two images of one link, or an unmapped middle concept/link.
pub fn compose_path_mappings(
    first: &PathMapping,
    then: &PathMapping,
    middle: &[ArtifactEnvelope],
    registry: &BTreeSet<PropertyTypeName>,
) -> Result<PathMapping, Vec<Diagnostic>> {
    let id = format!("{};{}", first.id, then.id);
    let catalog = EffectiveCatalog::from_artifacts_with_registry(middle, registry);
    let mut images = BTreeMap::new();
    for component in [first, then] {
        let mut seen = BTreeSet::new();
        for image in &component.links {
            if !seen.insert((&image.concept, &image.link)) {
                return Err(refuse(
                    &id,
                    &format!(
                        "{} maps {}.{} twice",
                        component.id, image.concept, image.link
                    ),
                ));
            }
        }
    }
    for image in &then.links {
        images.insert(
            (image.concept.as_str(), image.link.as_str()),
            image.path.as_slice(),
        );
    }
    let mut objects = BTreeMap::new();
    for (to, via) in &first.objects {
        let from = then
            .objects
            .get(via)
            .ok_or_else(|| refuse(&id, &format!("{via} is not mapped")))?;
        objects.insert(to.clone(), from.clone());
    }
    let mut links = Vec::new();
    for image in &first.links {
        let mut current = first
            .objects
            .get(&image.concept)
            .cloned()
            .ok_or_else(|| refuse(&id, &format!("{} has no object image", image.concept)))?;
        let mut path = Vec::new();
        for step in &image.path {
            let next = catalog
                .links_for_object_type(&current)
                .and_then(|links| links.iter().find(|l| l.id.as_str() == step))
                .filter(|link| link.cardinality.to == Multiplicity::One)
                .map(|l| l.target.as_str().to_owned());
            let (Some(next), Some(sub)) = (next, images.get(&(current.as_str(), step.as_str())))
            else {
                return Err(refuse(&id, &format!("{current}.{step} has no image")));
            };
            if !then.objects.contains_key(&next) {
                return Err(refuse(&id, &format!("{next} is not mapped")));
            }
            path.extend_from_slice(sub);
            current = next;
        }
        links.push(LinkImage {
            path,
            ..image.clone()
        });
    }
    Ok(PathMapping { id, objects, links })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "fixture construction")]

    use super::{AdmissionWork, ChaseBudget, PathMapping, PreservationClass, admit_with_work};
    use crate::consistency::codes;
    use norn_ir::ArtifactEnvelope;
    use serde_json::json;
    use std::collections::BTreeSet;

    /// `D` with two required functional links to `P`, equated by a path
    /// equation in both catalogs, so the identity mapping is certifiable.
    fn catalog(ns: &str) -> Vec<ArtifactEnvelope> {
        let link = |id: &str| {
            json!({"id": id, "target": format!("{ns}.P"),
                "cardinality": {"to": "one", "from": "many", "required": true}})
        };
        [("D", json!([link("a"), link("b")])), ("P", json!([]))]
            .into_iter()
            .map(|(name, links)| {
                let mut payload = json!({"entityVersion": "2.0",
                    "objectType": {"name": format!("{ns}.{name}"), "links": links}});
                if name == "D" {
                    payload["extensions"] = json!({"norn.path-equations/1.0.0":
                        [{"id": "same", "left": ["a"], "right": ["b"]}]});
                }
                serde_json::from_value(json!({
                    "irVersion": "0.1", "artifactId": format!("{ns}.{name}.entity"),
                    "kind": "entity", "namespace": ns, "name": name, "version": "0.1.0",
                    "lifecycle": "draft", "scope": {"orgId": "org_demo", "environmentId": "dev"},
                    "dependencies": [],
                    "provenance": {"source": "authoring_sdk", "sourcePath": "paths.ts"},
                    "payload": payload
                }))
                .unwrap()
            })
            .collect()
    }

    /// Exhausted admission work refuses a certifiable mapping: the certificate
    /// search is charged to the same allowance as the bounded search.
    #[test]
    fn a_certifiable_mapping_refuses_once_admission_work_is_exhausted() {
        let (source, target) = (catalog("s"), catalog("t"));
        let mapping: PathMapping = serde_json::from_value(json!({
            "id": "identity", "objects": {"t.D": "s.D", "t.P": "s.P"},
            "links": [{"concept": "t.D", "link": "a", "path": ["a"]},
                      {"concept": "t.D", "link": "b", "path": ["b"]}]
        }))
        .unwrap();
        let admit = |steps: usize| {
            admit_with_work(
                &source,
                &target,
                &mapping,
                &BTreeSet::new(),
                ChaseBudget::default(),
                &AdmissionWork::new(steps),
            )
        };
        assert_eq!(
            admit(10_000_000).unwrap().evidence,
            PreservationClass::Certified
        );
        // Ample for reading the input, with a few units left for the search:
        // the refusal comes from the certificate path, not input metering.
        let input = input_len(&source, &target, &mapping);
        work_refusal(&admit(input.saturating_add(16)).unwrap_err());
    }

    /// The bytes admission meters for this input before any search: the same
    /// verbatim encoding `admit_with_work` charges to the work allowance.
    fn input_len(
        source: &[ArtifactEnvelope],
        target: &[ArtifactEnvelope],
        mapping: &PathMapping,
    ) -> usize {
        norn_canonical::serialize_json_verbatim(&(source, target, mapping))
            .unwrap()
            .len()
    }

    /// Exactly one refusal, from the exhausted admission work allowance.
    fn work_refusal(findings: &[crate::Diagnostic]) {
        assert_eq!(findings.len(), 1, "{findings:?}");
        let refusal = findings.first().unwrap();
        assert_eq!(refusal.code.as_str(), codes::CHASE_REFUSED);
        assert!(
            refusal
                .message
                .starts_with("the chase exceeded its whole-admission work budget"),
            "{refusal:?}"
        );
    }

    /// One 128-atom target equation over a self-link whose image is the empty
    /// path, beside a 64-link image that widens every rule: the single case
    /// starts with both ends already merged, so the chase is free and nearly
    /// all the work is setting the case up (256 merges over 8,449 classes,
    /// done by the search and again by the check). That setup is charged, so
    /// an allowance well under its cost refuses instead of doing it unmetered.
    #[test]
    fn case_setup_is_charged_to_admission_work() {
        let node = |ns: &str, equations: serde_json::Value| -> Vec<ArtifactEnvelope> {
            let link = |id: &str| {
                json!({"id": id, "target": format!("{ns}.Node"),
                    "cardinality": {"to": "one", "from": "many", "required": true}})
            };
            let mut payload = json!({"entityVersion": "2.0", "objectType": {
                "name": format!("{ns}.Node"), "links": [link("next"), link("far")]}});
            if !equations.is_null() {
                payload["extensions"] = json!({"norn.path-equations/1.0.0": equations});
            }
            vec![
                serde_json::from_value(json!({
                    "irVersion": "0.1", "artifactId": format!("{ns}.Node.entity"),
                    "kind": "entity", "namespace": ns, "name": "Node", "version": "0.1.0",
                    "lifecycle": "draft", "scope": {"orgId": "org_demo", "environmentId": "dev"},
                    "dependencies": [],
                    "provenance": {"source": "authoring_sdk", "sourcePath": "paths.ts"},
                    "payload": payload
                }))
                .unwrap(),
            ]
        };
        let long = vec!["next"; 64];
        let source = node("s", serde_json::Value::Null);
        let target = node("t", json!([{"id": "loop", "left": long, "right": long}]));
        let mapping: PathMapping = serde_json::from_value(json!({
            "id": "collapse", "objects": {"t.Node": "s.Node"},
            "links": [{"concept": "t.Node", "link": "next", "path": []},
                      {"concept": "t.Node", "link": "far", "path": long}]
        }))
        .unwrap();
        let admit = |steps: usize| {
            admit_with_work(
                &source,
                &target,
                &mapping,
                &BTreeSet::new(),
                ChaseBudget::default(),
                &AdmissionWork::new(steps),
            )
        };
        let input = input_len(&source, &target, &mapping);
        // The setup alone costs over four million iterations.
        work_refusal(&admit(input.saturating_add(1_000_000)).unwrap_err());
        // With the work to do it, the same mapping is certified.
        assert_eq!(
            admit(1_000_000_000).unwrap().evidence,
            PreservationClass::Certified
        );
    }
}
