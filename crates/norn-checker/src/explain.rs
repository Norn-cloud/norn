//! Read-only explanations use the same admission, closure and induction generator.
use crate::{Diagnostic, DiagnosticCode, DiagnosticSeverity, EntityArtifactSetContext};
use norn_ir::definition_catalog::{
    ArtifactKey, DefinitionCatalog, DependencyVia, LawBinding, Member, MemberRef,
};
use norn_ir::explain::{ExplainFormat, ExplainKind, ExplainReport, ExplainUnknown, ExplainedLaw};
use norn_ir::{ActionRoot, ArtifactEnvelope, ArtifactKind, LawIR};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// Locale-map diagnostic identities, shared with structured-requirement rendering.
pub mod wording_codes {
    /// Select an available BCP 47 tag from the pinned wording map; no fallback is applied.
    pub const UNKNOWN_LOCALE: &str = "law.wording.unknown_locale";
    /// Use a well-formed BCP 47 tag and declare it in the reviewed wording map.
    pub const INVALID_LOCALE: &str = "law.wording.invalid_locale";
    /// Keep one locale entry per case-insensitive BCP 47 identity.
    pub const DUPLICATE_LOCALE: &str = "law.wording.duplicate_locale";
    /// Supply every required template, semantic label and phrase for the declared locale.
    pub const INCOMPLETE_LOCALE: &str = "law.wording.incomplete_locale";
}

fn refusal(code: &str, message: impl Into<String>) -> Vec<Diagnostic> {
    vec![Diagnostic {
        code: DiagnosticCode::new(code),
        severity: DiagnosticSeverity::Error,
        message: message.into(),
        span: None,
        artifact_id: None,
        remediation: None,
    }]
}

fn closure_error(error: impl std::fmt::Display) -> Vec<Diagnostic> {
    refusal(
        crate::verification::codes::CLOSURE_REFUSED,
        error.to_string(),
    )
}

/// EXPLAIN-CLOSURE-1: admission precedes projection; identities come from the catalog.
/// Callers supply a complete authorized definition set, including the installed base.
/// No object values or authority decisions are loaded by this function.
/// # Errors
/// Returns the existing checker diagnostics for invalid, ambiguous or oversized input.
pub fn explain(
    artifacts: &[ArtifactEnvelope],
    context: &EntityArtifactSetContext<'_>,
    kind: ExplainKind,
    id: &str,
) -> Result<ExplainReport, Vec<Diagnostic>> {
    if artifacts.len().saturating_add(context.base.len()) > 16_384 {
        return Err(closure_error(
            "definition count exceeds the graph node budget",
        ));
    }
    let diagnostics = crate::check_artifact_set(artifacts, context);
    if diagnostics.iter().any(Diagnostic::is_error) {
        return Err(diagnostics);
    }
    let catalog = crate::definition_catalog::project(artifacts, context.base, context.registry)
        .map_err(closure_error)?;
    let target = target(&catalog, artifacts, context.base, kind, id)?;
    let pool: Vec<_> = context.base.iter().chain(artifacts).cloned().collect();
    let artifact = pool
        .iter()
        .find(|a| a.artifact_id == target.artifact_id && a.version == target.version)
        .ok_or_else(|| {
            refusal(
                crate::verification::codes::UNKNOWN_BINDING,
                "target declaration is unavailable",
            )
        })?;
    let closure = crate::verification::conservative_closure(
        artifact,
        &pool,
        context.registry,
        norn_graph::Budget {
            max_nodes: 16_384,
            max_edges: 131_072,
            max_steps: 1_048_576,
        },
    )
    .map_err(closure_error)?;
    let goals = crate::obligations::explain_induction(
        &pool,
        &closure.writers,
        &closure.open_boundaries,
        context.registry,
    )
    .map_err(closure_error)?;
    let starts = subject_starts(&target, &catalog);
    let mut laws = explain_laws(&pool, kind, &target, &catalog, &closure, &goals)?;
    let interpretations = declared_interpretations(&pool)?;
    if laws.len().saturating_mul(interpretations.len()) > 16_384 {
        return Err(closure_error(
            "interpretation footprints exceed the report budget",
        ));
    }
    for law in &mut laws {
        for interpretation in &interpretations {
            law.why_unknown.push(ExplainUnknown {
                writer: None,
                footprint: format!("interpretation:{}@{}", interpretation.declaration.artifact_id, interpretation.declaration.version),
                reason: "Ontology correspondence is declared; a checked law-interpretation path is not established.".into(),
                fix: "Bind the law and correspondence to a generated preservation obligation and supply checked interpretation evidence.".into(),
            });
        }
    }
    let reads = if kind == ExplainKind::Action {
        decode::<ActionRoot>(&pool, &target)?.slice
    } else {
        Vec::new()
    };
    let direct_effects = catalog
        .writes
        .iter()
        .filter(|row| {
            if kind == ExplainKind::Action {
                row.writer == target
            } else {
                starts.iter().any(|at| same_type(at, &row.target))
            }
        })
        .cloned()
        .collect();
    Ok(ExplainReport {
        format: ExplainFormat::V2,
        target,
        scope: artifact.scope.clone(),
        closure: catalog,
        interpretations,
        writers: closure.writers.into_iter().collect(),
        reads,
        direct_effects,
        laws,
    })
}

fn declared_interpretations(
    pool: &[ArtifactEnvelope],
) -> Result<Vec<norn_ir::explain::ExplainInterpretation>, Vec<Diagnostic>> {
    let mut result = Vec::new();
    for artifact in pool
        .iter()
        .filter(|artifact| artifact.kind() == ArtifactKind::BusinessPack)
    {
        let pack: norn_ir::BusinessPackRoot =
            serde_json::to_value(artifact.payload.to_json_value())
                .and_then(serde_json::from_value)
                .map_err(|error| {
                    refusal(
                        crate::verification::codes::INVALID_DOCUMENT,
                        error.to_string(),
                    )
                })?;
        if let norn_ir::BusinessPackRoot::Link(link) = pack {
            result.push(norn_ir::explain::ExplainInterpretation {
                declaration: ArtifactKey {
                    artifact_id: artifact.artifact_id.clone(),
                    version: artifact.version.clone(),
                },
                link,
                proof: norn_ir::explain::ExplainProof::Unknown,
            });
        }
    }
    result.sort_by(|a, b| a.declaration.cmp(&b.declaration));
    Ok(result)
}

fn explain_laws(
    pool: &[ArtifactEnvelope],
    kind: ExplainKind,
    target: &MemberRef,
    catalog: &DefinitionCatalog,
    closure: &crate::verification::LawClosure,
    goals: &[serde_json::Value],
) -> Result<Vec<ExplainedLaw>, Vec<Diagnostic>> {
    let mut law_ids: Vec<_> = catalog
        .laws
        .iter()
        .map(|law| law.at.artifact_id.clone())
        .collect();
    law_ids.sort();
    let starts = subject_starts(target, catalog);
    let mut laws = Vec::new();
    for law in &catalog.laws {
        if kind == ExplainKind::Law && law.at != *target {
            continue;
        }
        let index = law_ids
            .iter()
            .position(|id| id == &law.at.artifact_id)
            .ok_or_else(|| {
                refusal(
                    crate::verification::codes::CLOSURE_REFUSED,
                    "law index is unavailable",
                )
            })?;
        let owned: LawIR = decode(pool, &law.at)?;
        let obligations: Vec<_> = goals
            .iter()
            .filter(|goal| {
                goal.get("law").and_then(serde_json::Value::as_u64) == u64::try_from(index).ok()
            })
            .cloned()
            .collect();
        let why_unknown = unknowns(&obligations, &closure.writers, &closure.open_boundaries);
        let law_starts = if kind == ExplainKind::Law {
            vec![law.subject.clone()]
        } else {
            starts.clone()
        };
        let inheritance_paths = law_starts
            .iter()
            .filter_map(|start| inheritance_path(start, &law.subject, catalog))
            .collect();
        laws.push(ExplainedLaw {
            declaration: law.at.clone(),
            source_path: owned.provenance.source_path.clone(),
            law: owned,
            wording: None,
            inheritance_paths,
            containment_paths: containment_paths(&law.at, catalog)?,
            proof: norn_ir::explain::ExplainProof::Unknown,
            assumptions: Vec::new(),
            enforcement: law
                .binds
                .clone()
                .filter(|binding| matches!(binding, LawBinding::Invariant { .. })),
            obligations: obligations
                .iter()
                .map(|goal| norn_ir::RuntimeValue::from_json_text(goal.to_string()))
                .collect::<Result<_, _>>()
                .map_err(closure_error)?,
            why_unknown,
        });
    }
    Ok(laws)
}

fn decode<T: serde::de::DeserializeOwned>(
    pool: &[ArtifactEnvelope],
    at: &MemberRef,
) -> Result<T, Vec<Diagnostic>> {
    let artifact = pool
        .iter()
        .find(|a| a.artifact_id == at.artifact_id && a.version == at.version)
        .ok_or_else(|| {
            refusal(
                crate::verification::codes::UNKNOWN_BINDING,
                "pinned declaration is missing",
            )
        })?;
    serde_json::to_value(artifact.payload.to_json_value())
        .and_then(serde_json::from_value)
        .map_err(|error| {
            refusal(
                crate::verification::codes::INVALID_DOCUMENT,
                error.to_string(),
            )
        })
}

fn target(
    catalog: &DefinitionCatalog,
    artifacts: &[ArtifactEnvelope],
    base: &[ArtifactEnvelope],
    kind: ExplainKind,
    id: &str,
) -> Result<MemberRef, Vec<Diagnostic>> {
    let mut matches = Vec::new();
    match kind {
        ExplainKind::Concept => matches.extend(
            catalog
                .types
                .iter()
                .filter(|row| row.at.member.id() == format!("type:{id}"))
                .map(|row| row.at.clone()),
        ),
        ExplainKind::Law => matches.extend(
            catalog
                .laws
                .iter()
                .filter(|row| {
                    row.at.artifact_id == id
                        || matches!(&row.at.member, Member::Law { law } if law == id)
                })
                .map(|row| row.at.clone()),
        ),
        ExplainKind::Action => {
            for artifact in artifacts
                .iter()
                .chain(base)
                .filter(|a| a.kind() == ArtifactKind::Action)
            {
                let action: ActionRoot = serde_json::to_value(artifact.payload.to_json_value())
                    .and_then(serde_json::from_value)
                    .map_err(|error| {
                        refusal(
                            crate::verification::codes::INVALID_DOCUMENT,
                            error.to_string(),
                        )
                    })?;
                if artifact.artifact_id == id || action.action_id == id {
                    matches.push(MemberRef {
                        artifact_id: artifact.artifact_id.clone(),
                        version: artifact.version.clone(),
                        member: Member::Artifact {},
                    });
                }
            }
        }
    }
    if matches.len() != 1 {
        return Err(refusal(
            crate::verification::codes::UNKNOWN_BINDING,
            format!(
                "expected one {kind:?} named {id:?}; found {}",
                matches.len()
            ),
        ));
    }
    matches.pop().ok_or_else(|| {
        refusal(
            crate::verification::codes::UNKNOWN_BINDING,
            "target is missing",
        )
    })
}

fn type_name(at: &MemberRef) -> Option<&str> {
    match &at.member {
        Member::Type { type_name }
        | Member::Property { type_name, .. }
        | Member::Link { type_name, .. }
        | Member::State { type_name, .. }
        | Member::Transition { type_name, .. } => Some(type_name),
        Member::Artifact {} | Member::ObjectMap { .. } | Member::Law { .. } => None,
    }
}
fn same_type(a: &MemberRef, b: &MemberRef) -> bool {
    type_name(a).is_some() && type_name(a) == type_name(b)
}
fn subject_starts(target: &MemberRef, catalog: &DefinitionCatalog) -> Vec<MemberRef> {
    if matches!(target.member, Member::Type { .. }) {
        return vec![target.clone()];
    }
    let names: BTreeSet<_> = catalog
        .writes
        .iter()
        .filter(|row| row.writer == *target)
        .filter_map(|row| type_name(&row.target))
        .collect();
    catalog
        .types
        .iter()
        .filter(|row| type_name(&row.at).is_some_and(|name| names.contains(name)))
        .map(|row| row.at.clone())
        .collect()
}

fn inheritance_path(
    start: &MemberRef,
    end: &MemberRef,
    catalog: &DefinitionCatalog,
) -> Option<Vec<MemberRef>> {
    let by_name: BTreeMap<_, _> = catalog
        .types
        .iter()
        .filter_map(|row| type_name(&row.at).map(|name| (name, row)))
        .collect();
    let mut queue = VecDeque::from([start.clone()]);
    let mut parents: BTreeMap<MemberRef, Option<MemberRef>> =
        BTreeMap::from([(start.clone(), None)]);
    while let Some(at) = queue.pop_front() {
        if at == *end {
            let mut path = vec![at.clone()];
            let mut cursor = &at;
            while let Some(Some(parent)) = parents.get(cursor) {
                path.push(parent.clone());
                cursor = parent;
            }
            path.reverse();
            return Some(path);
        }
        let Some(row) = type_name(&at).and_then(|name| by_name.get(name)) else {
            continue;
        };
        for name in row.extends.iter().chain(&row.implements) {
            if let Some(parent) = by_name.get(name.as_str())
                && !parents.contains_key(&parent.at)
            {
                parents.insert(parent.at.clone(), Some(at.clone()));
                queue.push_back(parent.at.clone());
            }
        }
    }
    None
}

fn containment_paths(
    law: &MemberRef,
    catalog: &DefinitionCatalog,
) -> Result<Vec<Vec<ArtifactKey>>, Vec<Diagnostic>> {
    let root = ArtifactKey {
        artifact_id: law.artifact_id.clone(),
        version: law.version.clone(),
    };
    let mut parents: BTreeMap<ArtifactKey, Vec<ArtifactKey>> = BTreeMap::new();
    for edge in catalog
        .depends_on
        .iter()
        .filter(|edge| edge.via == DependencyVia::PackMember)
    {
        parents
            .entry(edge.to.clone())
            .or_default()
            .push(edge.from.clone());
    }
    let mut visited = BTreeSet::from([root.clone()]);
    let mut queue = VecDeque::from([vec![root]]);
    let mut result = Vec::new();
    let mut work = 0_usize;
    while let Some(path) = queue.pop_front() {
        let Some(at) = path.first() else {
            continue;
        };
        for parent in parents.get(at).into_iter().flatten() {
            if !visited.insert(parent.clone()) {
                continue;
            }
            work = work.saturating_add(path.len());
            if work > 1_048_576 {
                return Err(refusal(
                    crate::verification::codes::CLOSURE_REFUSED,
                    "law origin paths exceed the graph budget",
                ));
            }
            let mut next = vec![parent.clone()];
            next.extend(path.clone());
            result.push(next.clone());
            queue.push_back(next);
        }
    }
    Ok(result)
}

fn unknowns(
    goals: &[serde_json::Value],
    writers: &BTreeSet<crate::verification::LawWriter>,
    boundaries: &BTreeSet<String>,
) -> Vec<ExplainUnknown> {
    goals.iter().filter(|goal| goal.get("assessment").and_then(serde_json::Value::as_str) == Some("unknown")).map(|goal| {
        let descriptor = goal.get("goal");
        let kind = descriptor.and_then(|value| value.get("kind")).and_then(serde_json::Value::as_str).unwrap_or("law_semantics");
        let writer = descriptor.and_then(|value| value.get("writer")).and_then(serde_json::Value::as_u64).and_then(|i| usize::try_from(i).ok()).and_then(|i| writers.iter().nth(i)).cloned();
        let footprint = descriptor.and_then(|value| value.get("boundary")).and_then(serde_json::Value::as_u64).and_then(|i| usize::try_from(i).ok()).and_then(|i| boundaries.iter().nth(i)).cloned().unwrap_or_else(|| kind.to_owned());
        let reason = if matches!(writer, Some(crate::verification::LawWriter::Artifact { .. })) {
            "The induction profile has no executable relation for this writer; preservation is uncovered."
        } else { "The generated obligation has no checked evidence in this explanation input." };
        ExplainUnknown { writer, footprint, reason: reason.into(), fix: "Supply checked evidence bound to this law, writer closure, semantics, assumptions and bounds.".into() }
    }).collect()
}

/// Resolve BCP 47 tags through the authored locale map; never infer a fallback.
/// EXPLAIN-WORDING-1: wording binds the complete law and its own content digest.
/// # Errors
/// Uses the locale-map diagnostics or refuses a stale/missing law binding.
pub fn render_wording(
    report: &mut ExplainReport,
    options: &norn_ir::explain::ExplainOptions,
) -> Result<(), Vec<Diagnostic>> {
    let Some(locale) = &options.locale else {
        return Ok(());
    };
    if options.requirements.is_empty() || report.laws.is_empty() {
        return Err(refusal(
            wording_codes::UNKNOWN_LOCALE,
            "no pinned wording map is available for the requested law and locale",
        ));
    }
    for law in &mut report.laws {
        let matches: Vec<_> = options
            .requirements
            .iter()
            .filter(|requirement| requirement.norm.law == law.law)
            .collect();
        let [requirement] = matches.as_slice() else {
            return Err(refusal(
                crate::verification::codes::UNKNOWN_BINDING,
                format!(
                    "no unique pinned wording map for law {} and locale {locale}",
                    law.law.id
                ),
            ));
        };
        let view = norn_ir::requirement_display::RequirementView {
            requirement: (*requirement).clone(),
            locale: locale.clone(),
        };
        let slots = view.slots().map_err(|code| {
            refusal(
                code,
                format!("cannot render law {} in locale {locale}", law.law.id),
            )
        })?;
        let direction = view
            .direction()
            .map_err(|code| refusal(code, format!("cannot resolve locale {locale}")))?;
        let canonical = norn_ir::canonical::canonical_json_of(*requirement).map_err(|error| {
            refusal(
                crate::verification::codes::INVALID_DOCUMENT,
                error.to_string(),
            )
        })?;
        law.wording = Some(norn_ir::explain::ExplainWording {
            locale: locale.clone(),
            direction: direction.to_owned(),
            source: requirement.norm.source.clone(),
            slots,
            sha256: norn_ir::canonical::HashProfile::Norn1.hash_canonical_bytes(&canonical),
        });
    }
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "ontology-link projection correspondence fixture"
)]
mod interpretation_tests {
    #[test]
    fn declared_correspondence_never_becomes_a_checked_law_path() {
        use serde_json::json;
        let artifact: norn_ir::ArtifactEnvelope = serde_json::from_value(json!({
        "irVersion": "0.1",
        "artifactId": "norn.link.fixture-shop.fixture-canon",
        "kind": "business_pack",
        "namespace": "norn.ontology",
        "name": "Fixture shop to canon",
        "version": "0.1.0",
        "lifecycle": "draft",
        "scope": {},
        "dependencies": [],
        "provenance": {"source": "authoring_sdk", "sourcePath": "fixture.ts"},
        "payload": {
            "packId": "norn.link.fixture-shop.fixture-canon",
            "packVersion": "0.1",
            "packKind": "link",
            "fromPack": {"packId": "norn.ontology-pack.fixture-shop", "version": "0.1.0"},
            "toPack": {"packId": "norn.ontology-pack.fixture-canon", "version": "0.1.0"},
            "correspondences": [
                {
                    "correspondenceId": "corr.order",
                    "fromTermId": "norn.shop.order",
                    "relation": "subsumed_by",
                    "toTermIds": ["norn.canon.commitment"],
                    "justification": "A shop order is a commitment to transfer goods for payment.",
                    "evidenceRefs": ["source.shop-api", "https://example.com/docs/order"],
                    "confidenceBasisPoints": 8000,
                    "reviewedBy": "fixture",
                    "reviewedAt": "2026-10-30T00:00:00Z"
                },
                {
                    "correspondenceId": "corr.order.outstanding",
                    "fromTermId": "norn.shop.order.outstanding",
                    "relation": "derived",
                    "toTermIds": ["norn.canon.claim.amount", "norn.canon.settlement.amount"],
                    "justification": "Outstanding is the claim less its settlements.",
                    "evidenceRefs": ["source.shop-api"],
                    "confidenceBasisPoints": 6000,
                    "reviewedBy": "fixture",
                    "reviewedAt": "2026-10-30T00:00:00Z"
                }
            ],
            "residuals": [
                {"termId": "norn.shop.order.theme", "reason": "system_specific", "note": "Storefront presentation."}
            ]
        }
    })).expect("existing link fixture");
        let report = super::declared_interpretations(&[artifact]).expect("project correspondence");
        assert_eq!(report[0].proof, norn_ir::explain::ExplainProof::Unknown);
        assert_eq!(
            report[0].declaration.artifact_id,
            "norn.link.fixture-shop.fixture-canon"
        );
        assert_eq!(
            report[0].link.correspondences[0].from_term_id,
            "norn.shop.order"
        );
    }
}
