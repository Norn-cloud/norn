//! The `source_schema` and `mapping` artifact-set check (T8-SRC-1,
//! T8-MAP-1a): run by [`crate::check_artifact_set`] over the candidates and
//! the installed base. See the [`super`] module docs.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use norn_ir::mapping::{LawInterpretationIr, MappingDocument, MappingIr};
use norn_ir::source_schema::SourceSchemaIr;
use norn_ir::{ArtifactEnvelope, ArtifactKind, ArtifactPayload};

use super::{CompiledMapping, MappingModel, check_mapping_local, codes, compile_mapping};
use crate::lint::mapping::codes as lint_codes;
use crate::lint::{LintInput, lint_artifact_set};
use crate::property_types::error;
use crate::receipt::ArtifactRef;
use crate::source_schema::check_source_schema;
use crate::{Diagnostic, Span};

/// `SemVer` 2.0.0 (<https://semver.org>, the suggested regular expression).
pub(super) static SEMVER: LazyLock<Option<regex_lite::Regex>> = LazyLock::new(|| {
    regex_lite::Regex::new(
        r"^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-(0|[1-9][0-9]*|[0-9]*[a-zA-Z-][0-9a-zA-Z-]*)(\.(0|[1-9][0-9]*|[0-9]*[a-zA-Z-][0-9a-zA-Z-]*))*)?(\+[0-9a-zA-Z-]+(\.[0-9a-zA-Z-]+)*)?$",
    )
    .ok()
});

/// The provenance key of a `mapping` artifact: `<artifactId>@<version>`,
/// the `mappingVersion` of every fact it produces. The set check rejects an
/// id containing `@` and a version that is not `SemVer`
/// ([`codes::INVALID_KEY`]), so the key determines the pair (#2934).
#[must_use]
pub fn mapping_key(artifact: &ArtifactEnvelope) -> String {
    format!("{}@{}", artifact.artifact_id, artifact.version)
}

/// Check every candidate `source_schema` artifact (its document checks)
/// and every candidate `mapping` artifact (its key, its source binding, and
/// [`super::check_mapping`] against the bound schema and the [`MappingModel`] of
/// every `entity` artifact of the base and the set). Payloads that do not
/// decode are the per-artifact typed-root findings and are skipped here.
/// Findings carry the artifact id and point into the envelope.
#[must_use]
pub fn check_mapping_artifacts(
    artifacts: &[ArtifactEnvelope],
    base: &[ArtifactEnvelope],
    registry: &BTreeSet<norn_ir::PropertyTypeName>,
) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    // One deterministic work ceiling across every required path obligation.
    let work = crate::consistency::chase::AdmissionWork::new(100_000);
    for artifact in artifacts {
        if artifact.kind() == ArtifactKind::Mapping
            && let Ok(MappingDocument::LawInterpretation(document)) =
                serde_json::to_value(artifact.payload.to_json_value())
                    .and_then(serde_json::from_value::<MappingDocument>)
        {
            out.extend(interpretation_endpoints(artifact, &document, base));
            if !interpretation_members(&document, base) {
                out.push(in_payload(error(
                    "core.composition.mapping_gap",
                    "interpretation must cover the retained source contract exactly with unique compatible target members".to_owned(),
                    Span::pointer("/members"),
                ), artifact));
            }
            out.extend(key_findings(artifact));
            out.push(in_payload(
                error(
                    "mapping.interpretation.unsupported",
                    "law interpretations require member conformance and preservation obligations"
                        .to_owned(),
                    Span::pointer("/role"),
                ),
                artifact,
            ));
        }
        if let Some(document) = source_payload(artifact) {
            out.extend(
                check_source_schema(&document)
                    .into_iter()
                    .map(|finding| in_payload(finding, artifact)),
            );
        }
    }
    let mappings: Vec<(&ArtifactEnvelope, MappingIr)> = artifacts
        .iter()
        .filter_map(|artifact| mapping_payload(artifact).map(|mapping| (artifact, mapping)))
        .collect();
    if mappings.is_empty() {
        return out;
    }
    let model = MappingModel::from_artifacts(&entity_artifacts(artifacts, base));
    let mut path_refused = false;
    for (artifact, mapping) in &mappings {
        if let Ok(model) = &model {
            out.extend(missing_path_bindings(
                artifact, mapping, model, artifacts, base,
            ));
        }
        if !path_refused {
            let findings = path_findings(artifact, mapping, artifacts, base, registry, &work);
            path_refused = findings
                .iter()
                .any(|d| d.code == crate::consistency::codes::CHASE_REFUSED);
            out.extend(findings);
        }
        out.extend(key_findings(artifact));
        out.extend(dependency_findings(artifact, mapping));
        let findings = match (&model, resolve_source(mapping, artifacts, base)) {
            (Ok(model), Ok(source)) => {
                match compile_checked(artifact, mapping, &source, model, artifacts, base) {
                    Ok(compiled) => coverage_findings(artifact, &compiled, artifacts, base),
                    Err(findings) => findings,
                }
            }
            (Err(model_findings), _) => {
                let mut findings = check_mapping_local(mapping);
                findings.extend(model_findings.iter().cloned());
                findings
            }
            (Ok(_), Err(finding)) => {
                let mut findings = check_mapping_local(mapping);
                findings.push(*finding);
                findings
            }
        };
        out.extend(
            findings
                .into_iter()
                .map(|finding| in_payload(finding, artifact)),
        );
    }
    out
}

/// Derive obligations from the mapped types, independent of optional bindings.
/// The model's lineage includes the type itself and every ancestor.
fn missing_path_bindings(
    artifact: &ArtifactEnvelope,
    mapping: &MappingIr,
    model: &MappingModel,
    artifacts: &[ArtifactEnvelope],
    base: &[ArtifactEnvelope],
) -> Vec<Diagnostic> {
    use crate::entity_artifacts::{Decoded, decode_v2};
    let mut out = Vec::new();
    for (index, map) in mapping.maps.iter().enumerate() {
        let Some(name) = model.object_types.get(&map.target.entity) else {
            continue;
        };
        let Some(target) = model.types.get(name.as_str()) else {
            continue;
        };
        let mut equations = Vec::new();
        let mut owners = BTreeSet::new();
        for declaration in artifacts.iter().chain(base) {
            let Decoded::Root(root) = decode_v2(declaration) else {
                continue;
            };
            let norn_ir::EntityRoot::V2(root) = *root else {
                continue;
            };
            if !target.lineage.contains(&root.object_type.name) {
                continue;
            }
            let Some(value) = root
                .extensions
                .as_ref()
                .and_then(|e| e.get(norn_ir::entity::PATH_EQUATIONS_SLOT))
            else {
                continue;
            };
            // Malformed declarations already refuse ordinary entity admission.
            let Ok(declared) = serde_json::to_value(value)
                .and_then(serde_json::from_value::<Vec<norn_ir::entity::PathEquation>>)
            else {
                continue;
            };
            if !declared.is_empty() {
                owners.insert(declaration.artifact_id.as_str());
                equations.extend(declared.into_iter().map(|e| e.id));
            }
        }
        let covered = mapping.path_mappings.iter().any(|binding| {
            binding.target_entities.contains(&map.target.entity)
                && binding.mapping.objects.contains_key(name.as_str())
                && owners
                    .iter()
                    .all(|owner| binding.target_entities.iter().any(|id| id == owner))
        });
        if !equations.is_empty() && !covered {
            out.push(in_payload(
                error(
                    "consistency.path_mapping.missing_binding",
                    format!(
                        "missing path-equation binding for {} (equation {})",
                        map.target.entity,
                        equations.join(", ")
                    ),
                    Span::from_segments(["maps", &index.to_string(), "target", "entity"]),
                ),
                artifact,
            ));
        }
    }
    out
}

/// Run required obligations even when other mapping checks also refuse.
fn path_findings(
    artifact: &ArtifactEnvelope,
    mapping: &MappingIr,
    artifacts: &[ArtifactEnvelope],
    base: &[ArtifactEnvelope],
    registry: &BTreeSet<norn_ir::PropertyTypeName>,
    work: &crate::consistency::chase::AdmissionWork,
) -> Vec<Diagnostic> {
    use crate::consistency::{ChaseBudget, PATH_MAPPING_INVALID, path_mapping::admit_with_work};
    if mapping.path_mappings.is_empty() {
        return Vec::new();
    }
    let mapping_pin = path_pin(artifact);
    let resolve = |ids: &[String]| -> Result<Vec<ArtifactEnvelope>, Vec<Diagnostic>> {
        ids.iter()
            .map(|id| {
                let entity = artifacts
                    .iter()
                    .chain(base)
                    .find(|a| {
                        a.artifact_id == *id
                            && matches!(a.payload, ArtifactPayload::Entity(_))
                            && artifact
                                .dependencies
                                .iter()
                                .find(|d| d.artifact_id == *id && d.kind == ArtifactKind::Entity)
                                .and_then(|d| d.version.as_deref())
                                .is_none_or(|version| a.version == version)
                    })
                    .ok_or_else(|| {
                        vec![error(
                            PATH_MAPPING_INVALID,
                            format!("unresolved path catalog entity {id:?}"),
                            Span::pointer("/pathMappings"),
                        )]
                    })?;
                // Reserve bytes before cloning, including repeated catalog IDs.
                norn_canonical::meter_json_verbatim(work, entity).map_err(|cause| {
                    vec![error(
                        crate::consistency::codes::CHASE_REFUSED,
                        format!("path catalog metering refused: {cause}; consistency is unknown"),
                        Span::pointer("/pathMappings"),
                    )]
                })?;
                Ok(entity.clone())
            })
            .collect()
    };
    let mut out = Vec::new();
    for (index, binding) in mapping.path_mappings.iter().enumerate() {
        let locate = |mut d: Diagnostic| {
            d.span = Some(Span::from_segments(["pathMappings", &index.to_string()]));
            in_payload(d, artifact)
        };
        let result = (|| {
            let source = resolve(&binding.source_entities)?;
            let target = resolve(&binding.target_entities)?;
            let evidence = admit_with_work(
                &source,
                &target,
                &binding.mapping,
                registry,
                ChaseBudget::default(),
                work,
            )?;
            // The mapping envelope commits the declaration; catalog envelope
            // hashes commit the exact equations and link contracts checked.
            let mut pins: Vec<_> = source
                .iter()
                .chain(&target)
                .map(path_pin)
                .collect::<Result<_, _>>()?;
            pins.push(mapping_pin.clone()?);
            Ok::<_, Vec<Diagnostic>>(crate::consistency::finding(
                "consistency.path_mapping.preserved",
                crate::DiagnosticSeverity::Info,
                format!(
                    "{}; obligations={}; inputs={pins:?}",
                    evidence.evidence.as_str(),
                    evidence.obligations
                ),
                None,
            ))
        })();
        match result {
            Ok(finding) => out.push(locate(finding)),
            Err(findings) => {
                let exhausted = findings
                    .iter()
                    .any(|d| d.code == crate::consistency::codes::CHASE_REFUSED);
                out.extend(findings.into_iter().map(|mut d| {
                    d.severity = crate::DiagnosticSeverity::Error;
                    locate(d)
                }));
                if exhausted {
                    break;
                }
            }
        }
    }
    out
}

fn path_pin(artifact: &ArtifactEnvelope) -> Result<ArtifactRef, Vec<Diagnostic>> {
    ArtifactRef::of(artifact).map_err(|_error| {
        vec![error(
            crate::consistency::PATH_MAPPING_INVALID,
            "cannot hash path inputs".into(),
            Span::pointer("/pathMappings"),
        )]
    })
}

/// MAP-TOTAL-1: totality comes from retained definitions, never a supplied list.
/// This cut checks identity bindings; transformations remain unsupported.
fn interpretation_members(document: &LawInterpretationIr, base: &[ArtifactEnvelope]) -> bool {
    let Some(source) = layer_members(&document.source_layer, base) else {
        return false;
    };
    let Some(target) = layer_members(&document.target_layer, base) else {
        return false;
    };
    let mut seen = BTreeSet::new();
    let mut images = BTreeSet::new();
    document.members.iter().all(|binding| {
        binding.transform.is_none()
            && source.contains(&binding.source)
            && target.contains(&binding.target)
            && std::mem::discriminant(&binding.source.member)
                == std::mem::discriminant(&binding.target.member)
            && seen.insert(binding.source.clone())
            && images.insert(binding.target.clone())
    }) && seen == source
        && interpretation_contracts(document, base)
}

/// MAP-UNITS / EVENT-MULTI: member owners, scalar units and relation roles
/// come from resolved contracts. This identity profile never invents conversions.
fn interpretation_contracts(document: &LawInterpretationIr, base: &[ArtifactEnvelope]) -> bool {
    use norn_ir::definition_catalog::Member;
    let catalog = crate::entity_instance::EffectiveCatalog::from_artifacts(base);
    document.members.iter().all(|binding| {
        let (Some(source_owner), Some(target_owner)) = (member_owner(&binding.source), member_owner(&binding.target)) else { return false; };
        if !document.members.iter().any(|b| b.source == source_owner && b.target == target_owner) { return false; }
        match (&binding.source.member, &binding.target.member) {
            (Member::Type { .. }, Member::Type { .. }) => true,
            (Member::Property { .. }, Member::Property { .. }) => {
                let source = property_signature(&binding.source, &catalog, base);
                source.is_some() && source == property_signature(&binding.target, &catalog, base)
            }
            (Member::Link { .. }, Member::Link { .. }) => {
                let (Some((source, arity)), Some((target, target_arity))) = (link_signature(&binding.source, &catalog, base), link_signature(&binding.target, &catalog, base)) else { return false; };
                let interpreted = document.members.iter().any(|b| matches!((&b.source.member, &b.target.member),
                    (Member::Type { type_name: a }, Member::Type { type_name: b }) if a == &source && b == &target));
                arity == target_arity && (interpreted || (source == target && catalog.target_types(&source).is_some()))
            }
            (Member::State { type_name: source_type, lifecycle: source_lifecycle, state: source_id }, Member::State { type_name: target_type, lifecycle: target_lifecycle, state: target_id })
            | (Member::Transition { type_name: source_type, lifecycle: source_lifecycle, transition: source_id }, Member::Transition { type_name: target_type, lifecycle: target_lifecycle, transition: target_id }) => {
                let source = catalog.object_type_named(source_type).and_then(Result::ok).and_then(|o| o.lifecycle(source_lifecycle));
                let target = catalog.object_type_named(target_type).and_then(Result::ok).and_then(|o| o.lifecycle(target_lifecycle));
                source_lifecycle == target_lifecycle && source_id == target_id && source.is_some() && source == target
            }
            _ => false,
        }
    })
}

fn member_owner(
    at: &norn_ir::definition_catalog::MemberRef,
) -> Option<norn_ir::definition_catalog::MemberRef> {
    use norn_ir::definition_catalog::Member;
    let name = match &at.member {
        Member::Type { type_name }
        | Member::Property { type_name, .. }
        | Member::Link { type_name, .. }
        | Member::State { type_name, .. }
        | Member::Transition { type_name, .. } => type_name,
        Member::Artifact {} | Member::ObjectMap { .. } | Member::Law { .. } => return None,
    };
    Some(norn_ir::definition_catalog::MemberRef {
        member: Member::Type {
            type_name: name.clone(),
        },
        ..at.clone()
    })
}

fn property_signature(
    at: &norn_ir::definition_catalog::MemberRef,
    catalog: &crate::entity_instance::EffectiveCatalog,
    base: &[ArtifactEnvelope],
) -> Option<(norn_ir::PropertyType, bool)> {
    use norn_ir::definition_catalog::Member;
    let Member::Property {
        type_name,
        property,
    } = &at.member
    else {
        return None;
    };
    if let Some(Ok(object)) = catalog.object_type_named(type_name) {
        let p = object
            .properties
            .iter()
            .find(|p| p.id.as_str() == property)?;
        let crate::entity_instance::EffectiveKind::Base(kind) = &p.kind else {
            return None;
        };
        return Some((kind.clone(), p.nullable));
    }
    let definition = base
        .iter()
        .find(|a| a.artifact_id == at.artifact_id && a.version == at.version)?;
    let crate::entity_artifacts::Decoded::Root(root) =
        crate::entity_artifacts::decode_v2(definition)
    else {
        return None;
    };
    let norn_ir::EntityRoot::V2Interface(root) = *root else {
        return None;
    };
    let p = root
        .interface
        .properties
        .iter()
        .find(|p| p.id.as_str() == property)?;
    let kind = crate::property_types::resolved_property_type(&p.property_type, &[])?;
    Some((kind.clone(), p.constraints.nullable))
}

fn link_signature(
    at: &norn_ir::definition_catalog::MemberRef,
    catalog: &crate::entity_instance::EffectiveCatalog,
    base: &[ArtifactEnvelope],
) -> Option<(String, norn_ir::LinkCardinality)> {
    use norn_ir::definition_catalog::Member;
    let Member::Link { type_name, link } = &at.member else {
        return None;
    };
    if let Some(links) = catalog.links_for_object_type(type_name) {
        let l = links.iter().find(|l| l.id.as_str() == link)?;
        if l.acyclic || l.inverse.is_some() || !l.properties.is_empty() {
            return None;
        }
        return Some((l.target.as_str().to_owned(), l.cardinality));
    }
    let definition = base
        .iter()
        .find(|a| a.artifact_id == at.artifact_id && a.version == at.version)?;
    let crate::entity_artifacts::Decoded::Root(root) =
        crate::entity_artifacts::decode_v2(definition)
    else {
        return None;
    };
    let norn_ir::EntityRoot::V2Interface(root) = *root else {
        return None;
    };
    let l = root
        .interface
        .links
        .iter()
        .find(|l| l.id.as_str() == link)?;
    Some((l.target.as_str().to_owned(), l.cardinality))
}

fn layer_members(
    pin: &norn_ir::mapping::InterpretationEndpoint,
    base: &[ArtifactEnvelope],
) -> Option<BTreeSet<norn_ir::definition_catalog::MemberRef>> {
    use norn_ir::{BusinessPackRoot, PackArtifactKind};
    let layers: Vec<_> = base
        .iter()
        .filter(|a| a.artifact_id == pin.artifact_id && a.version == pin.version)
        .collect();
    let [layer] = layers.as_slice() else {
        return None;
    };
    let root: BusinessPackRoot = serde_json::to_value(layer.payload.to_json_value())
        .ok()
        .and_then(|v| serde_json::from_value(v).ok())?;
    let BusinessPackRoot::Composition(pack) = root else {
        return None;
    };
    let catalog = crate::entity_instance::EffectiveCatalog::from_artifacts(base);
    let mut members = BTreeSet::new();
    for reference in pack.artifacts {
        // Nested pack/extension contracts need retained closure, not guessed scope.
        if reference.kind == PackArtifactKind::BusinessPack {
            return None;
        }
        if reference.kind != PackArtifactKind::Entity {
            continue;
        }
        let definitions: Vec<_> = base
            .iter()
            .filter(|a| {
                a.artifact_id == reference.artifact_id
                    && a.version == reference.version
                    && a.kind() == ArtifactKind::Entity
            })
            .collect();
        let [definition] = definitions.as_slice() else {
            return None;
        };
        let crate::entity_artifacts::Decoded::Root(root) =
            crate::entity_artifacts::decode_v2(definition)
        else {
            return None;
        };
        members.extend(definition_members(&reference, &root, &catalog)?);
    }
    (!members.is_empty()).then_some(members)
}

fn definition_members(
    reference: &norn_ir::PackArtifactRef,
    root: &norn_ir::EntityRoot,
    catalog: &crate::entity_instance::EffectiveCatalog,
) -> Option<BTreeSet<norn_ir::definition_catalog::MemberRef>> {
    use norn_ir::EntityRoot;
    use norn_ir::definition_catalog::{Member, MemberRef};
    let mut members = BTreeSet::new();
    let mut add = |member| {
        members.insert(MemberRef {
            artifact_id: reference.artifact_id.clone(),
            version: reference.version.clone(),
            member,
        });
    };
    let (name, properties, links) = match root {
        EntityRoot::V2(_) => {
            let effective = catalog.object_type(&reference.artifact_id)?.ok()?;
            let name = effective.name.as_str().to_owned();
            for machine in &effective.lifecycles {
                let lifecycle = machine.lifecycle.property.as_str().to_owned();
                for state in &machine.states {
                    add(Member::State {
                        type_name: name.clone(),
                        lifecycle: lifecycle.clone(),
                        state: state.clone(),
                    });
                }
                for transition in &machine.lifecycle.transitions {
                    add(Member::Transition {
                        type_name: name.clone(),
                        lifecycle: lifecycle.clone(),
                        transition: transition.id.as_str().to_owned(),
                    });
                }
            }
            (
                name,
                effective
                    .properties
                    .iter()
                    .map(|p| p.id.as_str().to_owned())
                    .collect::<Vec<_>>(),
                catalog
                    .links_for_object_type(effective.name.as_str())?
                    .iter()
                    .map(|l| l.id.as_str().to_owned())
                    .collect::<Vec<_>>(),
            )
        }
        EntityRoot::V2Interface(root) if root.interface.extends.is_empty() => (
            root.interface.name.as_str().to_owned(),
            root.interface
                .properties
                .iter()
                .map(|p| p.id.as_str().to_owned())
                .collect(),
            root.interface
                .links
                .iter()
                .map(|l| l.id.as_str().to_owned())
                .collect(),
        ),
        EntityRoot::V2PropertyTypes(_) | EntityRoot::V2Markings(_) => return Some(members),
        EntityRoot::V1(_) | EntityRoot::V2Extension(_) | EntityRoot::V2Interface(_) => return None,
    };
    add(Member::Type {
        type_name: name.clone(),
    });
    for property in properties {
        add(Member::Property {
            type_name: name.clone(),
            property,
        });
    }
    for link in links {
        add(Member::Link {
            type_name: name.clone(),
            link,
        });
    }
    Some(members)
}

fn interpretation_endpoints(
    artifact: &ArtifactEnvelope,
    document: &LawInterpretationIr,
    base: &[ArtifactEnvelope],
) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for (at, pin) in [
        ("/sourceLayer", &document.source_layer),
        ("/targetLayer", &document.target_layer),
    ] {
        let retained: Vec<_> = base
            .iter()
            .filter(|a| a.artifact_id == pin.artifact_id && a.version == pin.version)
            .collect();
        let dependencies: Vec<_> = artifact
            .dependencies
            .iter()
            .filter(|d| d.artifact_id == pin.artifact_id)
            .collect();
        let exact_dependency = matches!(dependencies.as_slice(), [dependency]
            if dependency.kind == ArtifactKind::BusinessPack && dependency.required
                && dependency.version.as_deref() == Some(pin.version.as_str()));
        let exact_retained = matches!(retained.as_slice(), [layer]
            if layer.kind() == ArtifactKind::BusinessPack
                && ArtifactRef::of(layer).is_ok_and(|r| r.sha256 == pin.sha256));
        let exact_version = SEMVER.as_ref().is_some_and(|g| g.is_match(&pin.version));
        if !exact_dependency || !exact_retained || !exact_version {
            out.push(in_payload(error(
                "mapping.interpretation.invalid_endpoint",
                format!("interpretation endpoint {:?} requires one retained business pack, an exact required dependency and matching version/content identity", pin.artifact_id),
                Span::pointer(at),
            ), artifact));
        }
    }
    out
}

/// Compile the candidate `mapping` artifact `mapping` against the set: its
/// bound source schema and the [`MappingModel`] of every `entity` artifact
/// of `artifacts` and `base` (T8-MAP-1b).
///
/// # Errors
///
/// The model, source-binding, and mapping findings (as
/// [`check_mapping_artifacts`] reports them for this mapping, without the
/// coverage rules).
pub fn compile_mapping_artifact(
    mapping: &ArtifactEnvelope,
    artifacts: &[ArtifactEnvelope],
    base: &[ArtifactEnvelope],
) -> Result<CompiledMapping, Vec<Diagnostic>> {
    let locate = |findings: Vec<Diagnostic>| -> Vec<Diagnostic> {
        findings
            .into_iter()
            .map(|finding| in_payload(finding, mapping))
            .collect()
    };
    let Some(document) = mapping_payload(mapping) else {
        return Err(locate(vec![error(
            codes::INVALID_DOCUMENT,
            "the payload is not a MappingIR document".to_owned(),
            Span::pointer(""),
        )]));
    };
    let model = MappingModel::from_artifacts(&entity_artifacts(artifacts, base)).map_err(locate)?;
    let source =
        resolve_source(&document, artifacts, base).map_err(|finding| locate(vec![*finding]))?;
    compile_checked(mapping, &document, &source, &model, artifacts, base).map_err(locate)
}

/// Reuse T2/T3 admission for the generated actions. Only entity artifacts
/// become their installed base, avoiding recursive mapping compilation.
fn compile_checked(
    artifact: &ArtifactEnvelope,
    mapping: &MappingIr,
    source: &SourceSchemaIr,
    model: &MappingModel,
    artifacts: &[ArtifactEnvelope],
    base: &[ArtifactEnvelope],
) -> Result<CompiledMapping, Vec<Diagnostic>> {
    if artifact.provenance.source_path.as_deref() == Some(super::evidence::CORE_MAPPING_GENERATOR)
        && !matches!(super::evidence::read_evidence(mapping), Ok(Some(_)))
    {
        return Err(vec![error(
            codes::INVALID_DOCUMENT,
            "generated mapping requires retained reviewed identity evidence".to_owned(),
            Span::pointer("/extensions"),
        )]);
    }
    let complete: Vec<_> = artifacts.iter().chain(base).cloned().collect();
    match super::evidence::evidence_impacts(mapping, &complete) {
        Ok(impacted) if impacted.is_empty() => {}
        result => {
            return Err(vec![error(
                codes::INVALID_DOCUMENT,
                format!("mapping evidence requires fresh review: {result:?}"),
                Span::pointer("/extensions"),
            )]);
        }
    }
    let compiled = compile_mapping(artifact, mapping, source, model)?;
    let entities = entity_artifacts(artifacts, base);
    let findings = crate::check_artifact_set(
        compiled.action_artifacts(),
        &crate::EntityArtifactSetContext {
            registry: &BTreeSet::new(),
            base: &entities,
            tenant: None,
        },
    );
    let errors: Vec<_> = findings
        .into_iter()
        .filter(Diagnostic::is_error)
        .map(|finding| {
            error(
                codes::UNENCODABLE_MEMBER,
                format!(
                    "compiled action fails {}: {}",
                    finding.code, finding.message
                ),
                Span::pointer(""),
            )
        })
        .collect();
    if errors.is_empty() {
        Ok(compiled)
    } else {
        Err(errors)
    }
}

/// The coverage rules of the ontology lint (T8-LINT-1) over a compiled
/// mapping: an unknown target member, or a required member neither mapped
/// nor declared a gap, refuses the mapping.
fn coverage_findings(
    artifact: &ArtifactEnvelope,
    compiled: &CompiledMapping,
    artifacts: &[ArtifactEnvelope],
    base: &[ArtifactEnvelope],
) -> Vec<Diagnostic> {
    lint_artifact_set(&LintInput {
        artifacts,
        base,
        coverage: compiled.coverage(),
    })
    .into_iter()
    .filter(|finding| {
        finding.is_error()
            && finding.artifact_id.as_deref() == Some(artifact.artifact_id.as_str())
            && [
                lint_codes::UNMAPPED_REQUIRED_MEMBER,
                lint_codes::UNKNOWN_MAPPING_TARGET,
            ]
            .contains(&finding.code.as_str())
    })
    .collect()
}

/// Every `entity` artifact of the base and the set.
fn entity_artifacts(
    artifacts: &[ArtifactEnvelope],
    base: &[ArtifactEnvelope],
) -> Vec<ArtifactEnvelope> {
    base.iter()
        .chain(artifacts)
        .filter(|artifact| matches!(artifact.payload, ArtifactPayload::Entity(_)))
        .cloned()
        .collect()
}

/// The strict root of a `source_schema` artifact's payload.
fn source_payload(artifact: &ArtifactEnvelope) -> Option<SourceSchemaIr> {
    let ArtifactPayload::SourceSchema(payload) = &artifact.payload else {
        return None;
    };
    serde_json::to_value(payload.to_json_value())
        .and_then(serde_json::from_value)
        .ok()
}

/// The strict root of a `mapping` artifact's payload.
fn mapping_payload(artifact: &ArtifactEnvelope) -> Option<MappingIr> {
    let ArtifactPayload::Mapping(payload) = &artifact.payload else {
        return None;
    };
    serde_json::to_value(payload.to_json_value())
        .and_then(serde_json::from_value)
        .ok()
}

/// The bound source schema: the `source_schema` artifact of the set (or
/// else the base) at the bound id and version, whose envelope hash is the
/// bound hash.
fn resolve_source(
    mapping: &MappingIr,
    artifacts: &[ArtifactEnvelope],
    base: &[ArtifactEnvelope],
) -> Result<SourceSchemaIr, Box<Diagnostic>> {
    let binding = &mapping.source;
    let Some(artifact) = artifacts.iter().chain(base).find(|artifact| {
        matches!(artifact.payload, ArtifactPayload::SourceSchema(_))
            && artifact.artifact_id == binding.schema_id
            && artifact.version == binding.version
    }) else {
        return Err(Box::new(error(
            codes::UNRESOLVED_SOURCE,
            format!(
                "source schema {:?} version {:?} is not a source_schema artifact of the set or the base",
                binding.schema_id, binding.version
            ),
            Span::pointer("/source"),
        )));
    };
    let actual = ArtifactRef::of(artifact).map_or_default(|reference| reference.sha256);
    if actual != binding.sha256 {
        return Err(Box::new(error(
            codes::SOURCE_HASH_MISMATCH,
            format!(
                "source schema {:?} version {:?} hashes to {actual:?}, not the bound {:?}",
                binding.schema_id, binding.version, binding.sha256
            ),
            Span::pointer("/source/sha256"),
        )));
    }
    source_payload(artifact).ok_or_else(|| {
        Box::new(error(
            codes::UNRESOLVED_SOURCE,
            format!(
                "source schema {:?} version {:?} does not decode",
                binding.schema_id, binding.version
            ),
            Span::pointer("/source"),
        ))
    })
}

fn key_findings(artifact: &ArtifactEnvelope) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    if artifact.artifact_id.contains('@') {
        out.push(error(
            codes::INVALID_KEY,
            format!(
                "mapping id {:?} contains '@', so its key {:?} is ambiguous",
                artifact.artifact_id,
                mapping_key(artifact)
            ),
            Span::pointer("/artifactId"),
        ));
    }
    let semver = SEMVER
        .as_ref()
        .is_some_and(|grammar| grammar.is_match(&artifact.version));
    if !semver {
        out.push(error(
            codes::INVALID_KEY,
            format!(
                "mapping version {:?} is not a SemVer version",
                artifact.version
            ),
            Span::pointer("/version"),
        ));
    }
    out.into_iter()
        .map(|mut finding| {
            finding.artifact_id = Some(artifact.artifact_id.clone());
            finding
        })
        .collect()
}

/// Locate a document finding in its artifact: set the artifact id and move
/// the pointer under `/payload`.
fn in_payload(mut finding: Diagnostic, artifact: &ArtifactEnvelope) -> Diagnostic {
    if let Some(span) = finding.span.as_mut() {
        span.pointer = format!("/payload{}", span.pointer);
    }
    finding.artifact_id = Some(artifact.artifact_id.clone());
    finding
}

/// Keep semantic references on the artifact-set dependency graph. The set
/// resolver checks exact versions, kinds, scopes, and availability.
fn dependency_findings(artifact: &ArtifactEnvelope, mapping: &MappingIr) -> Vec<Diagnostic> {
    let declares = |id: &str, kind: ArtifactKind, version: Option<&str>| {
        artifact.dependencies.iter().any(|dependency| {
            dependency.artifact_id == id
                && dependency.kind == kind
                && dependency.required
                && dependency
                    .version
                    .as_deref()
                    .is_some_and(|pinned| version.is_none_or(|expected| pinned == expected))
        })
    };
    let mut out = Vec::new();
    let mut require = |id: &str, kind: ArtifactKind, version: Option<&str>, at: Span| {
        if !declares(id, kind, version) {
            out.push(in_payload(
                error(
                    codes::UNDECLARED_DEPENDENCY,
                    format!(
                        "artifact reference {id:?} is not a required, exact-version dependency"
                    ),
                    at,
                ),
                artifact,
            ));
        }
    };
    require(
        &mapping.source.schema_id,
        ArtifactKind::SourceSchema,
        Some(&mapping.source.version),
        Span::pointer("/source"),
    );
    for binding in &mapping.path_mappings {
        for id in binding
            .source_entities
            .iter()
            .chain(&binding.target_entities)
        {
            require(
                id,
                ArtifactKind::Entity,
                None,
                Span::pointer("/pathMappings"),
            );
        }
    }
    for (index, interface) in mapping.interfaces.iter().enumerate() {
        require(
            interface,
            ArtifactKind::Entity,
            None,
            Span::from_segments(["interfaces", index.to_string().as_str()]),
        );
    }
    for (index, map) in mapping.maps.iter().enumerate() {
        for (member, id) in [
            ("entity", &map.target.entity),
            ("interface", &map.target.interface),
        ] {
            require(
                id,
                ArtifactKind::Entity,
                None,
                Span::from_segments(["maps", index.to_string().as_str(), "target", member]),
            );
        }
    }
    out
}
