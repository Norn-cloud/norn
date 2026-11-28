//! STEP-PIN-1 full dependency-closure pinning for parked `norn.question@1`
//! flow steps.
//!
//! Compares the effective question, its resolved [`QuestionPlan`], and its
//! full typed dependency closure (entity schemas, property types, interfaces,
//! extensions, markings, metrics, derived views/transforms, defined concepts,
//! relations, mappings, source schemas, connectors, laws, and policies)
//! between run start (`run_definitions_version`) and dispatch under one
//! `definition_read` epoch, failing closed on any drift or missing/undecodable
//! pin evidence.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Display;

use norn_cell::engine::SqlEngine;
use norn_cell::flow_run::QuestionStepPin;
use norn_cell::retained::{RetainedDefinitionRecord, RetainedLifecycle};
use norn_cell::{InstalledDefinitionSet, MAIN_WORLD};
use norn_checker::ArtifactRef;
use norn_checker::entity_scopes::ScopedView;
use norn_checker::metric::compute::{QuestionPlan, admit as admit_declared_plan};
use norn_checker::metric::derive::{MetricSpace, derive_metric_space};
use norn_ir::access::PolicyRoot;
use norn_ir::connector::{ConnectorMapping, ConnectorRoot};
use norn_ir::definition_catalog::Member;
use norn_ir::deontic::DeonticIR;
use norn_ir::mapping::MappingDocument;
use norn_ir::source_schema::SourceSchemaIr;
use norn_ir::view::ViewIrPayload;
use norn_ir::{
    ArtifactEnvelope, ArtifactKind, DefinedConceptRoot, EntityRoot, LawIR, LawRoot,
    MetricDefinition, QuestionIR, RelationRoot, RuntimeValue, UtcTimestamp,
};

use crate::host::DevHost;
use crate::pull::PullProfile;
use crate::refusal::Refusal;
use crate::scope::CellScope;

use super::{definition_changed, pin_mismatch, pin_unavailable, question_not_found};

/// Internal host-written sync-report mapping; never a domain dependency.
const HOST_REPORT_MAPPING: &str = "norn.connect.mapping";

fn pin_unavailable_cause(error: impl Display) -> Refusal {
    pin_unavailable().with_cause(error)
}

fn check_explicit_pin(
    pin: Option<&QuestionStepPin>,
    active_ver: u64,
    artifact_ids: (&str, Option<&str>),
    version_str: &str,
    active_digest: &str,
) -> Result<(), Refusal> {
    let Some(pin) = pin else {
        return Ok(());
    };
    let id_mismatch = pin.artifact_id.as_deref().is_some_and(|expected| {
        expected != artifact_ids.0 && artifact_ids.1.is_none_or(|alt| expected != alt)
    });
    if pin
        .definitions_version
        .is_some_and(|expected| expected != active_ver)
        || id_mismatch
        || pin
            .version
            .as_deref()
            .is_some_and(|expected| expected != version_str)
        || pin
            .digest
            .as_deref()
            .is_some_and(|expected| expected != active_digest)
    {
        return Err(pin_mismatch());
    }
    Ok(())
}

/// Live `mapping_id -> {connector_id}` bindings read from `source.Connection`
/// records in the cell store (empty when the model declares no connections).
fn connection_connectors<E: SqlEngine>(
    host: &DevHost<E>,
) -> Result<BTreeMap<String, BTreeSet<String>>, Refusal> {
    let cell = host.cell()?;
    let reader = cell.store().read();
    if !reader.model().has_type("source.Connection") {
        return Ok(BTreeMap::new());
    }
    let rows = reader
        .list(MAIN_WORLD, "source.Connection", 1_024)
        .map_err(Refusal::host)?;
    let mut bindings: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for row in rows {
        let Some(mapping) = row.values.get("mapping").and_then(RuntimeValue::as_str) else {
            continue;
        };
        if let Some(connector) = row.values.get("connector").and_then(RuntimeValue::as_str) {
            bindings
                .entry(mapping.to_owned())
                .or_default()
                .insert(connector.to_owned());
        }
    }
    Ok(bindings)
}

/// Verify both explicit `pin` and run-start dependency-closure equivalence for
/// a retained `"question"` definition under the caller's held `definition_read`
/// epoch. Independently resolves the retained question plan against BOTH the
/// run-start definitions set and the active definitions set.
#[allow(
    clippy::too_many_arguments,
    reason = "STEP-PIN-1 verification coordinates"
)]
pub(crate) fn verify_retained_pin<E: SqlEngine>(
    host: &DevHost<E>,
    record: &RetainedDefinitionRecord,
    plan: &QuestionPlan,
    installed_version: Option<u64>,
    active_set: Option<&InstalledDefinitionSet>,
    run_definitions_version: Option<u64>,
    explicit: Option<&QuestionStepPin>,
    as_of_override: Option<&str>,
    max_rows: u32,
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
    let conn_map = connection_connectors(host)?;
    let active_digest = effective_closure_digest(
        host.scope(),
        &record.definition.id,
        plan,
        Some(record),
        active_set,
        &conn_map,
    )?;
    check_explicit_pin(
        explicit,
        active_ver,
        (&record.definition.id, None),
        &record.definition.revision.to_string(),
        &active_digest,
    )?;
    let started_version = run_definitions_version.ok_or_else(pin_unavailable)?;
    if started_version == active_ver {
        return Ok(());
    }
    let (start_set, start_retained) = {
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
        (start_set, start_retained)
    };
    if start_retained.definition.kind != "question"
        || start_retained.definition.lifecycle != RetainedLifecycle::Active
        || start_retained.definition.revision != record.definition.revision
        || start_retained.definition.payload_sha256 != record.definition.payload_sha256
    {
        return Err(definition_changed());
    }
    let mut start_ir: QuestionIR =
        serde_json::from_str(&start_retained.definition.payload).map_err(pin_unavailable_cause)?;
    if let Some(as_of) = as_of_override {
        start_ir.time_window.end = UtcTimestamp(as_of.to_owned());
    }
    let start_plan = resolve_plan_in_set(host.scope(), &start_set, &start_ir, None, max_rows)?;
    let start_digest = effective_closure_digest(
        host.scope(),
        &record.definition.id,
        &start_plan,
        Some(&start_retained),
        &start_set,
        &conn_map,
    )?;
    if start_digest != active_digest {
        return Err(definition_changed());
    }
    Ok(())
}

/// Verify both explicit `pin` and run-start dependency-closure equivalence for
/// a pre-admitted named question under the caller's held `definition_read`
/// epoch. Resolves the named question plan against BOTH the run-start
/// definitions set and the active definitions set rather than reusing the
/// current plan for both. Fails closed when pin evidence is absent.
#[allow(
    clippy::too_many_arguments,
    reason = "STEP-PIN-1 verification coordinates"
)]
pub(crate) fn verify_named_pin<E: SqlEngine>(
    host: &DevHost<E>,
    question_id: &str,
    plan: &QuestionPlan,
    installed_version: Option<u64>,
    active_set: Option<&InstalledDefinitionSet>,
    run_definitions_version: Option<u64>,
    explicit: Option<&QuestionStepPin>,
    max_rows: u32,
) -> Result<(), Refusal> {
    let active_ver = installed_version.ok_or_else(pin_unavailable)?;
    let active_set = active_set.ok_or_else(pin_unavailable)?;
    let conn_map = connection_connectors(host)?;
    let active_plan = resolve_plan_in_set(
        host.scope(),
        active_set,
        plan.question(),
        Some(plan.definitions()),
        max_rows,
    )?;
    let active_digest = effective_closure_digest(
        host.scope(),
        question_id,
        &active_plan,
        None,
        active_set,
        &conn_map,
    )?;
    check_explicit_pin(
        explicit,
        active_ver,
        (question_id, Some(plan.question().name.as_str())),
        &active_ver.to_string(),
        &active_digest,
    )?;
    let started_version = run_definitions_version.ok_or_else(pin_unavailable)?;
    if started_version == active_ver {
        return Ok(());
    }
    let start_set = {
        let cell = host.cell()?;
        cell.store()
            .read()
            .installed_definitions(Some(started_version))
            .map_err(Refusal::host)?
            .ok_or_else(pin_unavailable)?
    };
    let start_plan = resolve_plan_in_set(
        host.scope(),
        &start_set,
        plan.question(),
        Some(plan.definitions()),
        max_rows,
    )?;
    let start_digest = effective_closure_digest(
        host.scope(),
        question_id,
        &start_plan,
        None,
        &start_set,
        &conn_map,
    )?;
    if start_digest != active_digest {
        return Err(definition_changed());
    }
    Ok(())
}

/// Independently admit `question` against `set`'s derived metric space (or,
/// for static named questions whose metrics are host-declared outside the
/// derived space, against `set`'s effective catalog). Fails closed with
/// `definition_changed` if the question cannot be admitted in `set`.
fn resolve_plan_in_set(
    scope: &CellScope,
    set: &InstalledDefinitionSet,
    question: &QuestionIR,
    declared_fallback: Option<&[MetricDefinition]>,
    max_rows: u32,
) -> Result<QuestionPlan, Refusal> {
    let artifacts =
        crate::kernel_set::retained_artifacts(&set.canonical_set).map_err(pin_unavailable_cause)?;
    let space = derive_space(scope, &artifacts)?;
    if let Ok(resolved) = space.admit(question, max_rows) {
        return Ok(resolved);
    }
    if let Some(declared) = declared_fallback
        && let Ok(resolved) = admit_declared_plan(declared, question, space.catalog(), max_rows)
    {
        return Ok(resolved);
    }
    Err(definition_changed())
}

fn derive_space(scope: &CellScope, artifacts: &[ArtifactEnvelope]) -> Result<MetricSpace, Refusal> {
    let view = ScopedView::bound(&scope.as_scope_ref(), artifacts, &[], &BTreeSet::new());
    derive_metric_space(&view, &LawRoot::Cell(scope.key())).map_err(|findings| {
        pin_unavailable_cause(format!(
            "metric space derivation failed with {} finding(s)",
            findings.len()
        ))
    })
}

/// Canonical 64-hex digest of the effective question, its resolved plan, and
/// its full typed dependency closure in `set`.
pub(crate) fn effective_closure_digest(
    scope: &CellScope,
    question_id: &str,
    plan: &QuestionPlan,
    retained: Option<&RetainedDefinitionRecord>,
    set: &InstalledDefinitionSet,
    conn_map: &BTreeMap<String, BTreeSet<String>>,
) -> Result<String, Refusal> {
    let artifacts =
        crate::kernel_set::retained_artifacts(&set.canonical_set).map_err(pin_unavailable_cause)?;
    let model = crate::kernel_set::serving_model_installed(&artifacts, &set.model_hash)
        .map_err(pin_unavailable_cause)?;
    let space = derive_space(scope, &artifacts)?;
    let touched = collect_seed_types(&model, plan);
    let closure_refs = collect_closure_artifact_refs(&model, &artifacts, touched, conn_map)?;
    let metric_names = referenced_metric_names(plan);
    let mut derived_metrics: Vec<MetricDefinition> = space
        .candidates()
        .iter()
        .filter(|candidate| metric_names.contains(&candidate.definition.name))
        .map(|candidate| candidate.definition.clone())
        .collect();
    derived_metrics.sort_by(|left, right| left.name.cmp(&right.name));
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
        .map_err(pin_unavailable_cause)
}

fn collect_seed_types(model: &norn_cell::Model, plan: &QuestionPlan) -> BTreeSet<String> {
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
    expand_subtypes(model, &mut seeds);
    seeds
}

fn expand_subtypes(model: &norn_cell::Model, types: &mut BTreeSet<String>) {
    let mut extra = BTreeSet::new();
    for declared in model.object_types() {
        let name = declared.name.as_str();
        if types
            .iter()
            .any(|seed| model.is_a(name, seed) || model.is_a(seed, name))
        {
            extra.insert(name.to_owned());
        }
    }
    types.extend(extra);
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

/// Typed dependency-closure graph state updated to fixed point.
struct ClosureGraph {
    touched_types: BTreeSet<String>,
    touched_interfaces: BTreeSet<String>,
    included_indices: BTreeSet<usize>,
    included_ids: BTreeSet<String>,
    source_schemas: BTreeSet<String>,
    source_systems: BTreeSet<String>,
    required_datasets: BTreeSet<String>,
    mapping_ids: BTreeSet<String>,
    bound_connectors: BTreeSet<String>,
}

impl ClosureGraph {
    fn new(touched_types: BTreeSet<String>) -> Self {
        Self {
            touched_types,
            touched_interfaces: BTreeSet::new(),
            included_indices: BTreeSet::new(),
            included_ids: BTreeSet::new(),
            source_schemas: BTreeSet::new(),
            source_systems: BTreeSet::new(),
            required_datasets: BTreeSet::new(),
            mapping_ids: BTreeSet::new(),
            bound_connectors: BTreeSet::new(),
        }
    }

    fn include(&mut self, idx: usize, artifact: &ArtifactEnvelope) -> bool {
        let added = self.included_indices.insert(idx);
        self.included_ids.insert(artifact.artifact_id.clone());
        added
    }

    fn matches_type(&self, model: &norn_cell::Model, candidate: &str) -> bool {
        self.touched_types.contains(candidate)
            || self
                .touched_types
                .iter()
                .any(|t| model.is_a(candidate, t) || model.is_a(t, candidate))
    }
}

fn decode_payload<T: serde::de::DeserializeOwned>(
    artifact: &ArtifactEnvelope,
) -> Result<T, Refusal> {
    let wire =
        serde_json::to_value(artifact.payload.to_json_value()).map_err(pin_unavailable_cause)?;
    serde_json::from_value::<T>(wire).map_err(pin_unavailable_cause)
}

/// Build the full typed dependency closure over `artifacts` to fixed point:
/// follows explicit `ArtifactEnvelope.dependencies` AND every typed IR
/// reference across entities, interfaces, extensions, property types,
/// markings, relations, derived views, defined concepts, mappings, source
/// schemas, connectors, laws, and policies.
fn collect_closure_artifact_refs(
    model: &norn_cell::Model,
    artifacts: &[ArtifactEnvelope],
    seed_types: BTreeSet<String>,
    conn_map: &BTreeMap<String, BTreeSet<String>>,
) -> Result<Vec<(String, String, String)>, Refusal> {
    let id_to_idx: BTreeMap<&str, usize> = artifacts
        .iter()
        .enumerate()
        .map(|(idx, a)| (a.artifact_id.as_str(), idx))
        .collect();
    let entity_id_to_type = entity_artifact_types(artifacts)?;
    let mut graph = ClosureGraph::new(seed_types);

    // Fixed-point graph traversal bounded by artifact count + 2 passes.
    let max_passes = artifacts.len().saturating_add(2);
    for _ in 0..max_passes {
        let mut changed = false;

        // 1. Follow explicit `ArtifactEnvelope.dependencies` from every
        //    already-included artifact.
        let current_indices: Vec<usize> = graph.included_indices.iter().copied().collect();
        for idx in current_indices {
            if let Some(artifact) = artifacts.get(idx) {
                for dep in &artifact.dependencies {
                    if let Some(&dep_idx) = id_to_idx.get(dep.artifact_id.as_str())
                        && let Some(dep_art) = artifacts.get(dep_idx)
                        && graph.include(dep_idx, dep_art)
                    {
                        changed = true;
                    }
                }
            }
        }

        // 2. Inspect every artifact via its typed IR root to discover edges
        //    into/out of the current closure.
        for (idx, artifact) in artifacts.iter().enumerate() {
            if visit_artifact(
                model,
                idx,
                artifact,
                &entity_id_to_type,
                conn_map,
                &mut graph,
            )? {
                changed = true;
            }
        }

        if !changed {
            break;
        }
        expand_subtypes(model, &mut graph.touched_types);
    }

    let mut refs = Vec::with_capacity(graph.included_indices.len());
    for idx in graph.included_indices {
        let Some(artifact) = artifacts.get(idx) else {
            continue;
        };
        let pin = ArtifactRef::of(artifact).map_err(pin_unavailable_cause)?;
        refs.push((pin.artifact_id, pin.version, pin.sha256));
    }
    refs.sort();
    Ok(refs)
}

fn entity_artifact_types(
    artifacts: &[ArtifactEnvelope],
) -> Result<BTreeMap<&str, String>, Refusal> {
    let mut map = BTreeMap::new();
    for artifact in artifacts
        .iter()
        .filter(|a| a.kind() == ArtifactKind::Entity)
    {
        let root: EntityRoot = decode_payload(artifact)?;
        if let Some(kind) = root.object_kind() {
            map.insert(artifact.artifact_id.as_str(), kind.to_owned());
        }
    }
    Ok(map)
}

fn member_type_name(member: &Member) -> Option<&str> {
    match member {
        Member::Type { type_name }
        | Member::Property { type_name, .. }
        | Member::Link { type_name, .. }
        | Member::State { type_name, .. }
        | Member::Transition { type_name, .. } => Some(type_name.as_str()),
        Member::Artifact {} | Member::ObjectMap { .. } | Member::Law { .. } => None,
    }
}

fn visit_artifact(
    model: &norn_cell::Model,
    idx: usize,
    artifact: &ArtifactEnvelope,
    entity_id_to_type: &BTreeMap<&str, String>,
    conn_map: &BTreeMap<String, BTreeSet<String>>,
    graph: &mut ClosureGraph,
) -> Result<bool, Refusal> {
    let mut changed = false;
    match artifact.kind() {
        ArtifactKind::Entity => {
            changed |= visit_entity(model, idx, artifact, graph)?;
        }
        ArtifactKind::Relation => {
            let rel: RelationRoot = decode_payload(artifact)?;
            if graph.matches_type(model, &rel.from) || graph.matches_type(model, &rel.to) {
                changed |= graph.include(idx, artifact);
            }
        }
        ArtifactKind::Derived => {
            let view: ViewIrPayload = decode_payload(artifact)?;
            let touches = graph.included_indices.contains(&idx)
                || view
                    .inputs
                    .iter()
                    .any(|input| graph.included_ids.contains(&input.artifact_id));
            if touches {
                changed |= graph.include(idx, artifact);
            }
        }
        ArtifactKind::DefinedConcept => {
            let concept: DefinedConceptRoot = decode_payload(artifact)?;
            let touches = graph.included_indices.contains(&idx)
                || graph.included_ids.contains(&concept.base.artifact_id)
                || member_type_name(&concept.base.member)
                    .is_some_and(|ty| graph.matches_type(model, ty))
                || concept.dependencies.iter().any(|dep| {
                    graph.included_ids.contains(&dep.artifact_id)
                        || member_type_name(&dep.member)
                            .is_some_and(|ty| graph.matches_type(model, ty))
                });
            if touches {
                changed |= graph.include(idx, artifact);
            }
        }
        ArtifactKind::Mapping if artifact.artifact_id != HOST_REPORT_MAPPING => {
            changed |= visit_mapping(model, idx, artifact, entity_id_to_type, conn_map, graph)?;
        }
        ArtifactKind::Mapping => {}
        ArtifactKind::SourceSchema => {
            if graph.source_schemas.contains(&artifact.artifact_id)
                || graph.included_indices.contains(&idx)
            {
                let schema: SourceSchemaIr = decode_payload(artifact)?;
                changed |= graph.include(idx, artifact);
                changed |= graph.source_systems.insert(schema.source.system);
            }
        }
        ArtifactKind::Connector => {
            changed |= visit_connector(model, idx, artifact, graph)?;
        }
        ArtifactKind::Law => {
            changed |= visit_law(model, idx, artifact, graph)?;
        }
        ArtifactKind::Policy => {
            changed |= visit_policy(model, idx, artifact, graph)?;
        }
        ArtifactKind::Action
        | ArtifactKind::Rule
        | ArtifactKind::Widget
        | ArtifactKind::Customization
        | ArtifactKind::Setting
        | ArtifactKind::BusinessPack
        | ArtifactKind::Labels
        | ArtifactKind::Model => {}
    }
    Ok(changed)
}

fn visit_entity(
    model: &norn_cell::Model,
    idx: usize,
    artifact: &ArtifactEnvelope,
    graph: &mut ClosureGraph,
) -> Result<bool, Refusal> {
    let root: EntityRoot = decode_payload(artifact)?;
    let mut changed = false;
    match root {
        EntityRoot::V2(v2) => {
            if graph.matches_type(model, v2.object_type.name.as_str())
                || graph.included_indices.contains(&idx)
            {
                changed |= graph.include(idx, artifact);
                changed |= graph
                    .touched_types
                    .insert(v2.object_type.name.as_str().to_owned());
                for iface in v2.object_type.implements {
                    changed |= graph.touched_interfaces.insert(iface.0);
                }
            }
        }
        EntityRoot::V1(v1) => {
            if graph.matches_type(model, v1.object_kind.as_str())
                || graph.included_indices.contains(&idx)
            {
                changed |= graph.include(idx, artifact);
                changed |= graph.touched_types.insert(v1.object_kind);
            }
        }
        EntityRoot::V2Interface(iface) => {
            if graph
                .touched_interfaces
                .contains(iface.interface.name.as_str())
                || graph.included_indices.contains(&idx)
            {
                changed |= graph.include(idx, artifact);
            }
        }
        EntityRoot::V2Extension(ext) => {
            if graph.matches_type(model, ext.object_type_extension.target.as_str())
                || graph.included_indices.contains(&idx)
            {
                changed |= graph.include(idx, artifact);
            }
        }
        EntityRoot::V2Markings(_) => {
            // Marking catalogs govern cell-wide disclosure on every read.
            changed |= graph.include(idx, artifact);
        }
        EntityRoot::V2PropertyTypes(_) => {
            // Included via explicit `ArtifactEnvelope.dependencies` edges from
            // referencing entities.
        }
    }
    Ok(changed)
}

fn visit_mapping(
    model: &norn_cell::Model,
    idx: usize,
    artifact: &ArtifactEnvelope,
    entity_id_to_type: &BTreeMap<&str, String>,
    conn_map: &BTreeMap<String, BTreeSet<String>>,
    graph: &mut ClosureGraph,
) -> Result<bool, Refusal> {
    let doc: MappingDocument = decode_payload(artifact)?;
    let mut changed = false;
    match doc {
        MappingDocument::SourceRecord(mapping) => {
            let matching_datasets: Vec<String> = mapping
                .maps
                .iter()
                .filter(|map| {
                    entity_id_to_type
                        .get(map.target.entity.as_str())
                        .is_some_and(|target| graph.matches_type(model, target))
                })
                .map(|map| map.source.dataset.clone())
                .collect();
            if !matching_datasets.is_empty() || graph.included_indices.contains(&idx) {
                changed |= graph.include(idx, artifact);
                changed |= graph.source_schemas.insert(mapping.source.schema_id);
                changed |= graph.mapping_ids.insert(artifact.artifact_id.clone());
                for dataset in matching_datasets {
                    changed |= graph.required_datasets.insert(dataset);
                }
                if let Some(connectors) = conn_map.get(&artifact.artifact_id) {
                    for conn_id in connectors {
                        changed |= graph.bound_connectors.insert(conn_id.clone());
                    }
                }
            }
        }
        MappingDocument::LawInterpretation(interp) => {
            let touches = graph.included_indices.contains(&idx)
                || graph
                    .included_ids
                    .contains(&interp.source_layer.artifact_id)
                || graph
                    .included_ids
                    .contains(&interp.target_layer.artifact_id)
                || interp.members.iter().any(|m| {
                    graph.included_ids.contains(&m.source.artifact_id)
                        || graph.included_ids.contains(&m.target.artifact_id)
                        || member_type_name(&m.source.member)
                            .is_some_and(|ty| graph.matches_type(model, ty))
                        || member_type_name(&m.target.member)
                            .is_some_and(|ty| graph.matches_type(model, ty))
                });
            if touches {
                changed |= graph.include(idx, artifact);
            }
        }
    }
    Ok(changed)
}

fn visit_connector(
    model: &norn_cell::Model,
    idx: usize,
    artifact: &ArtifactEnvelope,
    graph: &mut ClosureGraph,
) -> Result<bool, Refusal> {
    let wire =
        serde_json::to_value(artifact.payload.to_json_value()).map_err(pin_unavailable_cause)?;
    let connector: ConnectorRoot =
        serde_json::from_value(wire.clone()).map_err(pin_unavailable_cause)?;
    let dep_hit = artifact.dependencies.iter().any(|dep| {
        graph.included_ids.contains(&dep.artifact_id)
            || graph.source_schemas.contains(&dep.artifact_id)
            || graph.mapping_ids.contains(&dep.artifact_id)
    });
    let mapping_hit = connector.mappings.iter().any(|map| match map {
        ConnectorMapping::Field {
            target_object_kind, ..
        }
        | ConnectorMapping::ExternalId {
            target_object_kind, ..
        } => graph.matches_type(model, target_object_kind),
    });
    let pull_hit = match PullProfile::from_connector(&wire) {
        Ok(Some(profile)) => {
            graph.source_systems.contains(&profile.source)
                && profile
                    .feeds
                    .iter()
                    .any(|feed| graph.required_datasets.contains(&feed.dataset))
        }
        Ok(None) => false,
        Err(error) => return Err(pin_unavailable_cause(error)),
    };
    let conn_hit = graph.bound_connectors.contains(&artifact.artifact_id);
    if dep_hit || mapping_hit || pull_hit || conn_hit || graph.included_indices.contains(&idx) {
        return Ok(graph.include(idx, artifact));
    }
    Ok(false)
}

fn visit_law(
    model: &norn_cell::Model,
    idx: usize,
    artifact: &ArtifactEnvelope,
    graph: &mut ClosureGraph,
) -> Result<bool, Refusal> {
    let wire =
        serde_json::to_value(artifact.payload.to_json_value()).map_err(pin_unavailable_cause)?;
    let touches = if let Ok(law) = serde_json::from_value::<LawIR>(wire.clone()) {
        graph.matches_type(model, &law.subject)
    } else if let Ok(deontic) = serde_json::from_value::<DeonticIR>(wire) {
        deontic
            .rules
            .iter()
            .any(|rule| graph.matches_type(model, &rule.law.subject))
    } else {
        return Err(pin_unavailable_cause(format!(
            "law artifact {} failed to decode as LawIR or DeonticIR",
            artifact.artifact_id
        )));
    };
    if touches || graph.included_indices.contains(&idx) {
        return Ok(graph.include(idx, artifact));
    }
    Ok(false)
}

fn visit_policy(
    model: &norn_cell::Model,
    idx: usize,
    artifact: &ArtifactEnvelope,
    graph: &mut ClosureGraph,
) -> Result<bool, Refusal> {
    let policy: PolicyRoot = decode_payload(artifact)?;
    let touches = match policy {
        PolicyRoot::Access(access) => {
            !access.generic_rules.is_empty()
                || access
                    .rules
                    .iter()
                    .any(|rule| graph.matches_type(model, rule.resource_type.as_str()))
        }
        PolicyRoot::Obligations(ob) => {
            ob.applies_to
                .as_deref()
                .is_some_and(|target| graph.matches_type(model, target))
                || ob
                    .obligations
                    .iter()
                    .any(|item| graph.matches_type(model, &item.applies_to))
        }
        PolicyRoot::Approval(app) => graph.matches_type(model, &app.applies_to),
    };
    if touches || graph.included_indices.contains(&idx) {
        return Ok(graph.include(idx, artifact));
    }
    Ok(false)
}
