//! GRAPH-DAG-1: classification dependencies retain exact artifact versions.
use crate::graph::{cycle_groups, topological_order};
use crate::{Diagnostic, DiagnosticCode, Span};
use norn_ir::{
    ArtifactEnvelope, ArtifactKind, ArtifactPayload, DefinedConceptRoot, DiagnosticSeverity,
};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn decode(payload: &ArtifactPayload) -> Result<DefinedConceptRoot, String> {
    let bytes = norn_canonical::canonical_json_of(&payload.to_json_value())
        .map_err(|error| error.to_string())?;
    let root: DefinedConceptRoot =
        serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    if norn_canonical::canonical_json_of(&root).map_err(|error| error.to_string())? != bytes {
        return Err("payload must use the declared object representation".into());
    }
    Ok(root)
}

/// GRAPH-DAG-1 refuses actual classification cycles, including retained dependencies.
/// Ambiguous identities remain resolver errors; they never select a graph vertex.
pub(crate) fn check_cycles(
    artifacts: &[ArtifactEnvelope],
    base: &[ArtifactEnvelope],
) -> Vec<Diagnostic> {
    let nodes: Vec<_> = artifacts
        .iter()
        .chain(base)
        .enumerate()
        .filter(|(_, artifact)| artifact.kind() == ArtifactKind::DefinedConcept)
        .filter_map(|(index, artifact)| {
            decode(&artifact.payload)
                .ok()
                .map(|root| (artifact, root, index < artifacts.len()))
        })
        .collect();
    let mut identities = BTreeMap::new();
    for (index, (artifact, _, _)) in nodes.iter().enumerate() {
        identities
            .entry((&artifact.artifact_id, &artifact.version))
            .and_modify(|target| *target = None)
            .or_insert(Some(index));
    }
    let edges: BTreeMap<usize, BTreeSet<usize>> = nodes
        .iter()
        .enumerate()
        .map(|(index, (_, root, _))| {
            (
                index,
                root.dependencies
                    .iter()
                    .filter_map(|dependency| {
                        identities
                            .get(&(&dependency.artifact_id, &dependency.version))
                            .copied()
                            .flatten()
                    })
                    .collect(),
            )
        })
        .collect();
    let (_, blocked) = topological_order(&edges, |index| index);
    cycle_groups(&edges, &blocked)
        .into_iter()
        .filter_map(|group| {
            let (artifact, _, _) = group
                .iter()
                .filter_map(|index| nodes.get(*index))
                .filter(|(_, _, candidate)| *candidate)
                .min_by_key(|(artifact, _, _)| (&artifact.artifact_id, &artifact.version))?;
            Some(Diagnostic {
                witness: None,
                code: DiagnosticCode::new("core.graph.cycle"),
                severity: DiagnosticSeverity::Error,
                message: "classification dependencies must be acyclic".into(),
                span: Some(Span::pointer("/payload/dependencies")),
                artifact_id: Some(artifact.artifact_id.clone()),
                remediation: None,
            })
        })
        .collect()
}

/// GRAPH-DAG-1 / GRAPH-CLASS-1 admit only exact, pure, bounded declarations.
pub(crate) fn check_admission(
    artifacts: &[ArtifactEnvelope],
    context: &crate::EntityArtifactSetContext<'_>,
) -> Vec<Diagnostic> {
    let pool: Vec<_> = artifacts.iter().chain(context.base).collect();
    artifacts
        .iter()
        .filter(|a| a.kind() == ArtifactKind::DefinedConcept)
        .filter_map(|artifact| {
            let root = decode(&artifact.payload).ok()?;
            qualify(artifact, &root, &pool, context.registry)
                .err()
                .map(|message| Diagnostic {
                    witness: None,
                    code: DiagnosticCode::new("core.graph.incomplete_classification"),
                    severity: DiagnosticSeverity::Error,
                    message: message.into(),
                    span: Some(Span::pointer("/payload")),
                    artifact_id: Some(artifact.artifact_id.clone()),
                    remediation: None,
                })
        })
        .collect()
}
fn required_target<'a>(
    owner: &ArtifactEnvelope,
    reference: &norn_ir::definition_catalog::MemberRef,
    pool: &'a [&'a ArtifactEnvelope],
) -> Result<&'a ArtifactEnvelope, &'static str> {
    let mut targets = pool
        .iter()
        .filter(|a| a.artifact_id == reference.artifact_id && a.version == reference.version);
    let target = targets.next().ok_or("a dependency target is unavailable")?;
    if targets.next().is_some()
        || !owner.dependencies.iter().any(|pin| {
            pin.required
                && pin.artifact_id == target.artifact_id
                && pin.version.as_ref() == Some(&target.version)
                && pin.kind == target.kind()
        })
    {
        return Err("every definition reference requires a unique exact required dependency");
    }
    Ok(target)
}
fn has_member(
    target: &ArtifactEnvelope,
    reference: &norn_ir::definition_catalog::MemberRef,
    pool: &[&ArtifactEnvelope],
    registry: &BTreeSet<norn_ir::PropertyTypeName>,
) -> bool {
    use norn_ir::definition_catalog::Member;
    if target.kind() == ArtifactKind::DefinedConcept {
        return matches!(reference.member, Member::Artifact {})
            || matches!(&reference.member, Member::Type { type_name }
                if decode(&target.payload).is_ok_and(|root| root.id == *type_name));
    }
    let Ok(closure) = entity_closure(target, pool) else {
        return false;
    };
    let Ok(catalog) = crate::definition_catalog::project(&[], &closure, registry) else {
        return false;
    };
    target.kind() == ArtifactKind::Entity
        && (matches!(reference.member, Member::Artifact {})
            || catalog.types.iter().any(|r| r.at == *reference)
            || catalog.properties.iter().any(|r| r.at == *reference)
            || catalog.links.iter().any(|r| r.at == *reference)
            || catalog.transitions.iter().any(|r| r.at == *reference)
            || catalog.states.contains(reference))
}
fn entity_closure(
    target: &ArtifactEnvelope,
    pool: &[&ArtifactEnvelope],
) -> Result<Vec<ArtifactEnvelope>, &'static str> {
    let mut closure = vec![target.clone()];
    let mut cursor = 0;
    while let Some(current) = closure.get(cursor).cloned() {
        let others: Vec<_> = pool
            .iter()
            .filter(|a| a.artifact_id != current.artifact_id)
            .map(|a| ArtifactEnvelope::clone(a))
            .collect();
        let resolved = crate::resolve_artifact_set(std::slice::from_ref(&current), &others)
            .map_err(|_error| "the pinned base type closure must resolve")?;
        for dependency in resolved.base_dependencies {
            if pool
                .iter()
                .filter(|a| {
                    a.artifact_id == dependency.artifact_id && a.version == dependency.version
                })
                .count()
                != 1
            {
                return Err("the pinned base closure contains an ambiguous identity");
            }
            if !closure
                .iter()
                .any(|a| a.artifact_id == dependency.artifact_id && a.version == dependency.version)
            {
                closure.push(dependency.clone());
            }
        }
        cursor = cursor.saturating_add(1);
    }
    Ok(closure)
}
fn bounded(limits: &norn_ir::ResourceLimitProfile) -> bool {
    let admitted = norn_ir::ResourceLimitProfile::norn_limits_v1();
    macro_rules! caps { ($($section:ident.$field:ident),* $(,)?) => {
        true $(&& limits.$section.$field <= admitted.$section.$field)*
    }; }
    limits.id == admitted.id
        && limits.version == admitted.version
        && caps!(
            decide.cpu_p99_micros,
            decide.fuel_envelope_micros,
            decide.fuel_units,
            turn.cpu_p99_micros,
            turn.fuel_units,
            slice.max_canonical_bytes,
            slice.max_records,
            slice.max_relation_depth,
            slice.qualification_ceiling_bytes,
            memory.max_turn_working_bytes,
            memory.live_footprint_target_bytes,
            envelope.max_output_bytes,
            envelope.max_events,
            envelope.max_commands,
            envelope.max_timers,
            helper.cpu_p99_micros,
            helper.aggregate_micros_per_decision,
            helper.max_guest_memory_bytes,
        )
}
pub(crate) fn qualify(
    artifact: &ArtifactEnvelope,
    root: &DefinedConceptRoot,
    pool: &[&ArtifactEnvelope],
    registry: &BTreeSet<norn_ir::PropertyTypeName>,
) -> Result<norn_expr::Expression, &'static str> {
    use crate::entity_artifacts::{Decoded, decode_v2};
    use norn_ir::definition_catalog::Member;
    if root.id.is_empty() || root.version != artifact.version || !bounded(&root.bounds) {
        return Err("classification identity/version and resource-profile caps must be valid");
    }
    let target = required_target(artifact, &root.base, pool)?;
    let Member::Type { type_name } = &root.base.member else {
        return Err("base must name a type member");
    };
    let Decoded::Root(base) = decode_v2(target) else {
        return Err("base must pin an EntityIR v2 definition");
    };
    let (object, declared) = match base.as_ref() {
        norn_ir::EntityRoot::V2(root) => (true, root.object_type.name.as_str()),
        norn_ir::EntityRoot::V2Interface(root) => (false, root.interface.name.as_str()),
        norn_ir::EntityRoot::V1(_)
        | norn_ir::EntityRoot::V2Extension(_)
        | norn_ir::EntityRoot::V2PropertyTypes(_)
        | norn_ir::EntityRoot::V2Markings(_) => {
            return Err("base must declare an object type or interface");
        }
    };
    if target.kind() != ArtifactKind::Entity || declared != type_name {
        return Err("base member must be declared by its exact pinned entity artifact");
    }
    for reference in &root.dependencies {
        let dependency = required_target(artifact, reference, pool)?;
        if !has_member(dependency, reference, pool, registry) {
            return Err("a declared dependency member is unavailable");
        }
    }
    let closure = entity_closure(target, pool)?;
    let env = crate::derive_type_env(&[], &closure, &root.bounds)
        .map_err(|_error| "the exact base closure must have a supported type environment")?;
    let env = if object {
        env.bind_self(&norn_ir::ObjectTypeName(type_name.clone()))
    } else {
        env.bind_interface_self(&norn_ir::InterfaceName(type_name.clone()))
    }
    .map_err(|_error| "the pinned base must bind self")?;
    let expression = norn_expr::Expression::compile(root.predicate.as_str())
        .map_err(|_error| "classification predicate must be a supported pure expression")?;
    norn_expr::check_expression_with(&expression, &env, &norn_expr::Type::Bool, &root.bounds)
        .map_err(
            |_error| "classification predicate must be Boolean within its declared resource bounds",
        )?;
    Ok(expression)
}
