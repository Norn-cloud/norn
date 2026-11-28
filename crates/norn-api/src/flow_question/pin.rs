//! STEP-PIN-1 full dependency-closure pinning for parked `norn.question@1`
//! flow steps.
//!
//! Compares the effective question AND its full dependency closure (referenced
//! entity schemas, property types, interfaces, markings, metrics, derived
//! views/transforms, mappings, source schemas, connectors, laws, and policies)
//! between run start (`run_definitions_version`) and dispatch under one
//! `definition_read` epoch, failing closed on any drift or missing pin
//! evidence.

use std::collections::{BTreeMap, BTreeSet};

use norn_cell::InstalledDefinitionSet;
use norn_cell::engine::SqlEngine;
use norn_cell::flow_run::QuestionStepPin;
use norn_cell::retained::{RetainedDefinitionRecord, RetainedLifecycle};
use norn_checker::ArtifactRef;
use norn_checker::entity_scopes::ScopedView;
use norn_checker::metric::compute::QuestionPlan;
use norn_checker::metric::derive::derive_metric_space;
use norn_ir::mapping::MappingDocument;
use norn_ir::{ArtifactEnvelope, ArtifactKind, EntityRoot, LawRoot, MetricDefinition, QuestionIR};

use crate::host::DevHost;
use crate::refusal::Refusal;
use crate::scope::CellScope;

use super::{definition_changed, pin_mismatch, pin_unavailable, question_not_found};

/// Internal host-written sync-report mapping; never a domain dependency.
const HOST_REPORT_MAPPING: &str = "norn.connect.mapping";

/// Verify both explicit `pin` and run-start dependency-closure equivalence for
/// a retained `"question"` definition under the caller's held `definition_read`
/// epoch.
pub(crate) fn verify_retained_pin<E: SqlEngine>(
    host: &DevHost<E>,
    record: &RetainedDefinitionRecord,
    plan: &QuestionPlan,
    installed_version: Option<u64>,
    active_set: Option<&InstalledDefinitionSet>,
    run_definitions_version: Option<u64>,
    explicit: Option<&QuestionStepPin>,
) -> Result<(), Refusal> {
    let active_ver = installed_version.ok_or_else(pin_unavailable)?;
    if record.definition.lifecycle != RetainedLifecycle::Active {
        if explicit.is_some() {
            return Err(pin_mismatch());
        }
        let started = run_definitions_version.ok_or_else(pin_unavailable)?;
        let cell = host.cell()?;
        let was_active = cell
            .store()
            .read()
            .retained_definition_at_version(&record.definition.id, started)
            .map_err(Refusal::host)?
            .is_some_and(|prior| {
                prior.definition.kind == "question"
                    && prior.definition.lifecycle == RetainedLifecycle::Active
            });
        return Err(if was_active {
            definition_changed()
        } else {
            question_not_found()
        });
    }
    let active_set = active_set.ok_or_else(pin_unavailable)?;
    let active_digest = effective_closure_digest(
        host.scope(),
        &record.definition.id,
        plan,
        Some(record),
        active_set,
    )?;
    if let Some(pin) = explicit {
        let rev_str = record.definition.revision.to_string();
        if pin
            .definitions_version
            .is_some_and(|expected| expected != active_ver)
            || pin
                .artifact_id
                .as_deref()
                .is_some_and(|expected| expected != record.definition.id)
            || pin
                .version
                .as_deref()
                .is_some_and(|expected| expected != rev_str)
            || pin
                .digest
                .as_deref()
                .is_some_and(|expected| expected != active_digest)
        {
            return Err(pin_mismatch());
        }
    }
    let started_version = run_definitions_version.ok_or_else(pin_unavailable)?;
    if started_version == active_ver {
        return Ok(());
    }
    let cell = host.cell()?;
    let reader = cell.store().read();
    let start_set = reader
        .installed_definitions(Some(started_version))
        .map_err(Refusal::host)?
        .ok_or_else(pin_unavailable)?;
    let start_retained = reader
        .retained_definition_at_version(&record.definition.id, started_version)
        .map_err(Refusal::host)?
        .ok_or_else(definition_changed)?;
    if start_retained.definition.kind != "question"
        || start_retained.definition.lifecycle != RetainedLifecycle::Active
        || start_retained.definition.revision != record.definition.revision
        || start_retained.definition.payload_sha256 != record.definition.payload_sha256
    {
        return Err(definition_changed());
    }
    let start_digest = effective_closure_digest(
        host.scope(),
        &record.definition.id,
        plan,
        Some(&start_retained),
        &start_set,
    )?;
    if start_digest != active_digest {
        return Err(definition_changed());
    }
    Ok(())
}

/// Verify both explicit `pin` and run-start dependency-closure equivalence for
/// a pre-admitted named question under the caller's held `definition_read`
/// epoch. Fails closed when pin evidence is absent.
pub(crate) fn verify_named_pin<E: SqlEngine>(
    host: &DevHost<E>,
    question_id: &str,
    plan: &QuestionPlan,
    installed_version: Option<u64>,
    active_set: Option<&InstalledDefinitionSet>,
    run_definitions_version: Option<u64>,
    explicit: Option<&QuestionStepPin>,
) -> Result<(), Refusal> {
    let active_ver = installed_version.ok_or_else(pin_unavailable)?;
    let active_set = active_set.ok_or_else(pin_unavailable)?;
    let active_digest =
        effective_closure_digest(host.scope(), question_id, plan, None, active_set)?;
    if let Some(pin) = explicit {
        let ver_str = active_ver.to_string();
        if pin
            .definitions_version
            .is_some_and(|expected| expected != active_ver)
            || pin
                .artifact_id
                .as_deref()
                .is_some_and(|expected| expected != question_id && expected != plan.question().name)
            || pin
                .version
                .as_deref()
                .is_some_and(|expected| expected != ver_str)
            || pin
                .digest
                .as_deref()
                .is_some_and(|expected| expected != active_digest)
        {
            return Err(pin_mismatch());
        }
    }
    let started_version = run_definitions_version.ok_or_else(pin_unavailable)?;
    if started_version == active_ver {
        return Ok(());
    }
    let cell = host.cell()?;
    let start_set = cell
        .store()
        .read()
        .installed_definitions(Some(started_version))
        .map_err(Refusal::host)?
        .ok_or_else(pin_unavailable)?;
    let start_digest = effective_closure_digest(host.scope(), question_id, plan, None, &start_set)?;
    if start_digest != active_digest {
        return Err(definition_changed());
    }
    Ok(())
}

/// Canonical 64-hex digest of the effective question and its full dependency
/// closure in `set` (entity schemas, property types, interfaces, markings,
/// metrics, derived views/transforms, mappings, source schemas, connectors,
/// laws, and policies).
pub(crate) fn effective_closure_digest(
    scope: &CellScope,
    question_id: &str,
    plan: &QuestionPlan,
    retained: Option<&RetainedDefinitionRecord>,
    set: &InstalledDefinitionSet,
) -> Result<String, Refusal> {
    let artifacts =
        crate::kernel_set::retained_artifacts(&set.canonical_set).map_err(|_| pin_unavailable())?;
    let model = crate::kernel_set::serving_model_installed(&artifacts, &set.model_hash)
        .map_err(|_| pin_unavailable())?;
    let touched = collect_touched_types(&model, plan);
    let closure_refs = collect_closure_artifact_refs(&model, &artifacts, &touched)?;
    let metric_names = referenced_metric_names(plan);
    let derived_metrics = derive_referenced_metrics(scope, &artifacts, &metric_names);
    let retained_pin = retained.map(|rec| {
        (
            rec.definition.id.as_str(),
            rec.definition.revision,
            rec.definition.payload_sha256.as_str(),
        )
    });
    let question_ir: &QuestionIR = plan.question();
    let payload = (
        question_id,
        retained_pin,
        question_ir,
        plan.definitions(),
        derived_metrics,
        closure_refs,
    );
    norn_ir::canonical::domain_hash_hex(b"norn.question.step.pin/1\0", &payload)
        .map_err(|_| pin_unavailable())
}

fn collect_touched_types(model: &norn_cell::Model, plan: &QuestionPlan) -> BTreeSet<String> {
    let mut seeds = BTreeSet::from([plan.grain().as_str().to_owned()]);
    for sum in plan.sums() {
        if let Some(link) = &sum.time_link {
            seeds.insert(link.target_type.as_str().to_owned());
        }
    }
    for def in plan.definitions() {
        if let Some(agg) = &def.type_params.metric_aggregation_params {
            seeds.insert(agg.semantic_model.0.clone());
        }
    }
    let mut touched = BTreeSet::new();
    for declared in model.object_types() {
        let name = declared.name.as_str();
        if seeds
            .iter()
            .any(|seed| model.is_a(name, seed) || model.is_a(seed, name))
        {
            touched.insert(name.to_owned());
        }
    }
    touched.extend(seeds);
    touched
}

fn referenced_metric_names(plan: &QuestionPlan) -> BTreeSet<String> {
    let mut names: BTreeSet<String> = plan
        .question()
        .metrics
        .iter()
        .map(|m| m.metric.clone())
        .collect();
    for def in plan.definitions() {
        names.insert(def.name.clone());
        if let Some(num) = &def.type_params.numerator {
            names.insert(num.clone());
        }
        if let Some(den) = &def.type_params.denominator {
            names.insert(den.clone());
        }
    }
    names
}

fn derive_referenced_metrics(
    scope: &CellScope,
    artifacts: &[ArtifactEnvelope],
    metric_names: &BTreeSet<String>,
) -> Vec<MetricDefinition> {
    let view = ScopedView::bound(&scope.as_scope_ref(), artifacts, &[], &BTreeSet::new());
    let Ok(space) = derive_metric_space(&view, &LawRoot::Cell(scope.key())) else {
        return Vec::new();
    };
    let mut matched: Vec<MetricDefinition> = space
        .candidates()
        .iter()
        .filter(|candidate| metric_names.contains(&candidate.definition.name))
        .map(|candidate| candidate.definition.clone())
        .collect();
    matched.sort_by(|left, right| left.name.cmp(&right.name));
    matched
}

fn collect_closure_artifact_refs(
    model: &norn_cell::Model,
    artifacts: &[ArtifactEnvelope],
    touched: &BTreeSet<String>,
) -> Result<Vec<(String, String, String)>, Refusal> {
    let entity_id_to_type = entity_artifact_types(artifacts);
    let mut source_schemas: BTreeSet<String> = BTreeSet::new();
    let mut mapping_ids: BTreeSet<String> = BTreeSet::new();
    let mut included: BTreeSet<usize> = BTreeSet::new();

    for (idx, artifact) in artifacts.iter().enumerate() {
        match artifact.kind() {
            ArtifactKind::Entity if entity_in_closure(artifact, touched) => {
                included.insert(idx);
            }
            ArtifactKind::Mapping if artifact.artifact_id != HOST_REPORT_MAPPING => {
                if let Some(schema_id) =
                    mapping_source_if_relevant(model, artifact, &entity_id_to_type, touched)
                {
                    included.insert(idx);
                    source_schemas.insert(schema_id);
                    mapping_ids.insert(artifact.artifact_id.clone());
                }
            }
            ArtifactKind::Relation
            | ArtifactKind::Derived
            | ArtifactKind::DefinedConcept
            | ArtifactKind::Law
            | ArtifactKind::Policy => {
                included.insert(idx);
            }
            _ => {}
        }
    }
    include_sources_and_connectors(artifacts, &source_schemas, &mapping_ids, &mut included);

    let mut refs = Vec::with_capacity(included.len());
    for idx in included {
        let Some(artifact) = artifacts.get(idx) else {
            continue;
        };
        let pin = ArtifactRef::of(artifact).map_err(|_| pin_unavailable())?;
        refs.push((pin.artifact_id, pin.version, pin.sha256));
    }
    refs.sort();
    Ok(refs)
}

fn entity_artifact_types(artifacts: &[ArtifactEnvelope]) -> BTreeMap<&str, String> {
    let mut map = BTreeMap::new();
    for artifact in artifacts
        .iter()
        .filter(|a| a.kind() == ArtifactKind::Entity)
    {
        if let Ok(wire) = serde_json::to_value(artifact.payload.to_json_value())
            && let Ok(root) = serde_json::from_value::<EntityRoot>(wire)
            && let Some(kind) = root.object_kind()
        {
            map.insert(artifact.artifact_id.as_str(), kind.to_owned());
        }
    }
    map
}

fn entity_in_closure(artifact: &ArtifactEnvelope, touched: &BTreeSet<String>) -> bool {
    let Ok(wire) = serde_json::to_value(artifact.payload.to_json_value()) else {
        return true;
    };
    let Ok(root) = serde_json::from_value::<EntityRoot>(wire) else {
        return true;
    };
    match &root {
        EntityRoot::V2(v2) => touched.contains(v2.object_type.name.as_str()),
        EntityRoot::V1(v1) => touched.contains(v1.object_kind.as_str()),
        EntityRoot::V2Interface(_)
        | EntityRoot::V2Extension(_)
        | EntityRoot::V2PropertyTypes(_)
        | EntityRoot::V2Markings(_) => true,
    }
}

fn mapping_source_if_relevant(
    model: &norn_cell::Model,
    artifact: &ArtifactEnvelope,
    entity_id_to_type: &BTreeMap<&str, String>,
    touched: &BTreeSet<String>,
) -> Option<String> {
    let wire = serde_json::to_value(artifact.payload.to_json_value()).ok()?;
    let MappingDocument::SourceRecord(mapping) =
        serde_json::from_value::<MappingDocument>(wire).ok()?
    else {
        return None;
    };
    let hits = mapping.maps.iter().any(|map| {
        entity_id_to_type
            .get(map.target.entity.as_str())
            .is_some_and(|target| {
                touched
                    .iter()
                    .any(|t| model.is_a(target, t) || model.is_a(t, target))
            })
    });
    hits.then_some(mapping.source.schema_id)
}

fn include_sources_and_connectors(
    artifacts: &[ArtifactEnvelope],
    source_schemas: &BTreeSet<String>,
    mapping_ids: &BTreeSet<String>,
    included: &mut BTreeSet<usize>,
) {
    for (idx, artifact) in artifacts.iter().enumerate() {
        match artifact.kind() {
            ArtifactKind::SourceSchema if source_schemas.contains(&artifact.artifact_id) => {
                included.insert(idx);
            }
            ArtifactKind::Connector => {
                let Ok(wire) = serde_json::to_value(artifact.payload.to_json_value()) else {
                    continue;
                };
                let text = wire.to_string();
                let hits = !mapping_ids.is_empty()
                    && (source_schemas.iter().any(|id| text.contains(id.as_str()))
                        || mapping_ids.iter().any(|id| text.contains(id.as_str()))
                        || connector_shares_prefix(&artifact.artifact_id, mapping_ids));
                if hits {
                    included.insert(idx);
                }
            }
            _ => {}
        }
    }
}

fn connector_shares_prefix(connector_id: &str, mapping_ids: &BTreeSet<String>) -> bool {
    let prefix = connector_id
        .strip_suffix(".connector")
        .unwrap_or(connector_id);
    mapping_ids
        .iter()
        .any(|id| id.strip_suffix(".mapping").unwrap_or(id) == prefix)
}
