//! The `source_schema` and `mapping` artifact-set check (T8-SRC-1,
//! T8-MAP-1a): run by [`crate::check_artifact_set`] over the candidates and
//! the installed base. See the [`super`] module docs.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use norn_ir::mapping::MappingIr;
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
) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    for artifact in artifacts {
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
    for (artifact, mapping) in &mappings {
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
    let actual = ArtifactRef::of(artifact)
        .map(|reference| reference.sha256)
        .unwrap_or_default();
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
