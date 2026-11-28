#![forbid(unsafe_code)]
//! Metered O(N) dependency graph projection for [`super::compute_ripple_effects_with_budget`].

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use norn_graph::{Budget, Limit};
use norn_ir::access::PolicyRoot;
use norn_ir::action::ActionRoot;
use norn_ir::business_pack::BusinessPackRoot;
use norn_ir::customization::CustomizationRoot;
use norn_ir::defined_concept::DefinedConceptRoot;
use norn_ir::entity::{EntityRoot, RelationRoot};
use norn_ir::labels::{LabelCatalogRoot, LabelTarget};
use norn_ir::mapping::MappingDocument;
use norn_ir::model::ModelRoot;
use norn_ir::rule::RuleRoot;
use norn_ir::verification::{LawIR, LawResponse};
use norn_ir::view::ViewIrPayload;
use norn_ir::widget::WidgetRoot;
use norn_ir::{ActionEffectProgram, ArtifactEnvelope, ArtifactKind, ArtifactPayload};
use serde_json::Value;

use crate::lifecycle_writes::FactSchema;

use super::{BudgetRefusal, kind_name};

pub(super) enum ProjectionError {
    Budget(BudgetRefusal),
    Invalid(String),
}

fn tick_step(steps: &mut usize, budget: Budget) -> Result<(), ProjectionError> {
    *steps = steps.saturating_add(1);
    if *steps > budget.max_steps {
        return Err(ProjectionError::Budget(BudgetRefusal::from_limit(
            Limit::Steps,
            budget.max_steps,
        )));
    }
    Ok(())
}

fn check_nodes(count: usize, budget: Budget) -> Result<(), ProjectionError> {
    if count > budget.max_nodes {
        return Err(ProjectionError::Budget(BudgetRefusal::from_limit(
            Limit::Nodes,
            budget.max_nodes,
        )));
    }
    Ok(())
}

fn payload_json(artifact: &ArtifactEnvelope) -> Result<Value, ProjectionError> {
    serde_json::to_value(artifact.payload.to_json_value()).map_err(|error| {
        ProjectionError::Invalid(format!(
            "failed to serialize {} artifact `{}` payload: {error}",
            kind_name(&artifact.kind()),
            artifact.artifact_id
        ))
    })
}

fn decode_error(
    artifact_id: &str,
    kind: ArtifactKind,
    error: &serde_json::Error,
) -> ProjectionError {
    ProjectionError::Invalid(format!(
        "failed to decode {} artifact `{artifact_id}`: {error}",
        kind_name(&kind)
    ))
}

macro_rules! decode_payload {
    ($id:expr, $kind:expr, $payload:expr $(,)?) => {
        serde_json::from_value($payload).map_err(|error| decode_error($id, $kind, &error))
    };
}

pub(super) struct Universe {
    pub(super) all_ids: BTreeSet<String>,
    pub(super) removed_ids: BTreeSet<String>,
    pub(super) roots: BTreeSet<String>,
    pub(super) artifact_kinds: BTreeMap<String, ArtifactKind>,
    pub(super) type_owners: BTreeMap<String, String>,
    pub(super) owner_types: BTreeMap<String, BTreeSet<String>>,
    pub(super) owned_children: BTreeMap<String, BTreeSet<String>>,
    pub(super) action_owners: BTreeMap<String, String>,
    pub(super) rule_owners: BTreeMap<String, String>,
    pub(super) law_owners: BTreeMap<String, String>,
    pub(super) widget_owners: BTreeMap<String, String>,
    pub(super) has_lifecycle: BTreeSet<String>,
}

impl Universe {
    pub(super) fn build(
        before_artifacts: &[ArtifactEnvelope],
        after_artifacts: &[ArtifactEnvelope],
        changed_artifact_ids: &[String],
        budget: Budget,
        steps: &mut usize,
    ) -> Result<Self, ProjectionError> {
        let mut universe = Self {
            all_ids: BTreeSet::new(),
            removed_ids: BTreeSet::new(),
            roots: BTreeSet::new(),
            artifact_kinds: BTreeMap::new(),
            type_owners: BTreeMap::new(),
            owner_types: BTreeMap::new(),
            owned_children: BTreeMap::new(),
            action_owners: BTreeMap::new(),
            rule_owners: BTreeMap::new(),
            law_owners: BTreeMap::new(),
            widget_owners: BTreeMap::new(),
            has_lifecycle: BTreeSet::new(),
        };
        let mut after_ids = BTreeSet::new();

        for artifact in before_artifacts {
            tick_step(steps, budget)?;
            universe.all_ids.insert(artifact.artifact_id.clone());
            check_nodes(universe.all_ids.len(), budget)?;
            universe
                .artifact_kinds
                .insert(artifact.artifact_id.clone(), artifact.kind());
            universe.index_symbols(artifact, budget, steps)?;
        }
        for artifact in after_artifacts {
            tick_step(steps, budget)?;
            universe.all_ids.insert(artifact.artifact_id.clone());
            check_nodes(universe.all_ids.len(), budget)?;
            after_ids.insert(artifact.artifact_id.clone());
            universe
                .artifact_kinds
                .insert(artifact.artifact_id.clone(), artifact.kind());
            universe.index_symbols(artifact, budget, steps)?;
        }

        for artifact in before_artifacts {
            tick_step(steps, budget)?;
            if !after_ids.contains(&artifact.artifact_id) {
                universe.removed_ids.insert(artifact.artifact_id.clone());
            }
        }

        universe.roots.clone_from(&universe.removed_ids);
        for changed in changed_artifact_ids {
            tick_step(steps, budget)?;
            if universe.all_ids.contains(changed) {
                universe.roots.insert(changed.clone());
            }
        }
        Ok(universe)
    }

    fn record_type_owner(&mut self, type_name: String, artifact_id: &str) {
        self.owner_types
            .entry(artifact_id.to_owned())
            .or_default()
            .insert(type_name.clone());
        self.type_owners.insert(type_name, artifact_id.to_owned());
    }

    fn index_symbols(
        &mut self,
        artifact: &ArtifactEnvelope,
        budget: Budget,
        steps: &mut usize,
    ) -> Result<(), ProjectionError> {
        let kind = artifact.kind();
        let id = artifact.artifact_id.as_str();
        let payload = payload_json(artifact)?;
        match kind {
            ArtifactKind::Entity => {
                let root: EntityRoot = decode_payload!(id, kind, payload)?;
                match root {
                    EntityRoot::V2(v2) => {
                        let type_name = v2.object_type.name.as_str().to_owned();
                        self.record_type_owner(type_name.clone(), id);
                        if !v2.object_type.lifecycles.is_empty() {
                            self.has_lifecycle.insert(id.to_owned());
                        }
                        for collection in &v2.object_type.owned_collections {
                            tick_step(steps, budget)?;
                            self.owned_children
                                .entry(type_name.clone())
                                .or_default()
                                .insert(collection.child_type.as_str().to_owned());
                        }
                    }
                    EntityRoot::V2Interface(iface) => {
                        let type_name = iface.interface.name.as_str().to_owned();
                        self.record_type_owner(type_name, id);
                        if !iface.interface.lifecycles.is_empty() {
                            self.has_lifecycle.insert(id.to_owned());
                        }
                    }
                    EntityRoot::V1(v1) => {
                        self.record_type_owner(v1.object_kind, id);
                    }
                    EntityRoot::V2Extension(_)
                    | EntityRoot::V2PropertyTypes(_)
                    | EntityRoot::V2Markings(_) => {}
                }
            }
            ArtifactKind::Action => {
                let action: ActionRoot = decode_payload!(id, kind, payload)?;
                self.action_owners.insert(action.action_id, id.to_owned());
            }
            ArtifactKind::Rule => {
                let rule: RuleRoot = decode_payload!(id, kind, payload)?;
                self.rule_owners.insert(rule.rule_id, id.to_owned());
            }
            ArtifactKind::Law => {
                let law: LawIR = decode_payload!(id, kind, payload)?;
                self.law_owners.insert(law.id, id.to_owned());
            }
            ArtifactKind::Widget => {
                let widget: WidgetRoot = decode_payload!(id, kind, payload)?;
                self.widget_owners.insert(widget.widget_id, id.to_owned());
            }
            ArtifactKind::Relation
            | ArtifactKind::Connector
            | ArtifactKind::Customization
            | ArtifactKind::Setting
            | ArtifactKind::BusinessPack
            | ArtifactKind::Policy
            | ArtifactKind::Derived
            | ArtifactKind::SourceSchema
            | ArtifactKind::Mapping
            | ArtifactKind::DefinedConcept
            | ArtifactKind::Labels
            | ArtifactKind::Model => {}
        }
        Ok(())
    }
}

pub(super) struct ProjectedGraph {
    pub(super) edges: BTreeSet<(String, String)>,
    pub(super) specialization_edges: BTreeSet<(String, String)>,
    pub(super) broken_dependents: BTreeSet<String>,
}

pub(super) struct Projection<'a> {
    universe: &'a Universe,
    budget: Budget,
    steps: &'a mut usize,
    pub(super) edges: BTreeSet<(String, String)>,
    pub(super) specialization_edges: BTreeSet<(String, String)>,
    pub(super) broken_dependents: BTreeSet<String>,
    type_parents: BTreeMap<String, BTreeSet<String>>,
    type_writers: BTreeMap<String, BTreeSet<String>>,
    law_subjects: Vec<(String, String)>,
}

impl<'a> Projection<'a> {
    pub(super) fn new(universe: &'a Universe, budget: Budget, steps: &'a mut usize) -> Self {
        Self {
            universe,
            budget,
            steps,
            edges: BTreeSet::new(),
            specialization_edges: BTreeSet::new(),
            broken_dependents: BTreeSet::new(),
            type_parents: BTreeMap::new(),
            type_writers: BTreeMap::new(),
            law_subjects: Vec::new(),
        }
    }

    pub(super) fn into_graph(self) -> ProjectedGraph {
        ProjectedGraph {
            edges: self.edges,
            specialization_edges: self.specialization_edges,
            broken_dependents: self.broken_dependents,
        }
    }

    fn tick(&mut self) -> Result<(), ProjectionError> {
        tick_step(self.steps, self.budget)
    }

    fn add_edge(&mut self, dependent: &str, dependency: &str) -> Result<(), ProjectionError> {
        self.tick()?;
        if dependent == dependency
            || !self.universe.all_ids.contains(dependent)
            || !self.universe.all_ids.contains(dependency)
        {
            return Ok(());
        }
        self.edges
            .insert((dependent.to_owned(), dependency.to_owned()));
        if self.universe.removed_ids.contains(dependency) {
            self.broken_dependents.insert(dependent.to_owned());
        }
        if self.edges.len() > self.budget.max_edges {
            return Err(ProjectionError::Budget(BudgetRefusal::from_limit(
                Limit::Edges,
                self.budget.max_edges,
            )));
        }
        Ok(())
    }

    fn add_specialization(
        &mut self,
        dependent: &str,
        dependency: &str,
    ) -> Result<(), ProjectionError> {
        if dependent != dependency
            && self.universe.all_ids.contains(dependent)
            && self.universe.all_ids.contains(dependency)
        {
            self.specialization_edges
                .insert((dependent.to_owned(), dependency.to_owned()));
        }
        self.add_edge(dependent, dependency)
    }

    fn add_type_edge(&mut self, dependent: &str, type_name: &str) -> Result<(), ProjectionError> {
        self.tick()?;
        if let Some(owner) = self.universe.type_owners.get(type_name) {
            self.add_edge(dependent, owner)?;
        }
        Ok(())
    }

    fn add_action_edge(&mut self, dependent: &str, action_id: &str) -> Result<(), ProjectionError> {
        self.tick()?;
        if let Some(owner) = self.universe.action_owners.get(action_id) {
            self.add_edge(dependent, owner)?;
        } else {
            self.add_edge(dependent, action_id)?;
        }
        Ok(())
    }

    pub(super) fn collect(
        &mut self,
        before_artifacts: &[ArtifactEnvelope],
        after_artifacts: &[ArtifactEnvelope],
    ) -> Result<(), ProjectionError> {
        for artifact in after_artifacts {
            self.tick()?;
            self.ingest_artifact(artifact)?;
        }
        for artifact in before_artifacts {
            self.tick()?;
            if self.universe.removed_ids.contains(&artifact.artifact_id) {
                self.ingest_removed_type_hierarchy(artifact)?;
            }
        }
        self.add_law_writer_edges()
    }

    fn ingest_removed_type_hierarchy(
        &mut self,
        artifact: &ArtifactEnvelope,
    ) -> Result<(), ProjectionError> {
        if artifact.kind() != ArtifactKind::Entity {
            return Ok(());
        }
        let root: EntityRoot = decode_payload!(
            &artifact.artifact_id,
            artifact.kind(),
            payload_json(artifact)?,
        )?;
        match root {
            EntityRoot::V2(v2) => {
                for parent in v2
                    .object_type
                    .extends
                    .iter()
                    .map(norn_ir::ObjectTypeName::as_str)
                    .chain(
                        v2.object_type
                            .implements
                            .iter()
                            .map(norn_ir::InterfaceName::as_str),
                    )
                {
                    self.tick()?;
                    self.type_parents
                        .entry(v2.object_type.name.as_str().to_owned())
                        .or_default()
                        .insert(parent.to_owned());
                }
            }
            EntityRoot::V2Interface(iface) => {
                for parent in &iface.interface.extends {
                    self.tick()?;
                    self.type_parents
                        .entry(iface.interface.name.as_str().to_owned())
                        .or_default()
                        .insert(parent.as_str().to_owned());
                }
            }
            EntityRoot::V1(_)
            | EntityRoot::V2Extension(_)
            | EntityRoot::V2PropertyTypes(_)
            | EntityRoot::V2Markings(_) => {}
        }
        Ok(())
    }

    fn ingest_artifact(&mut self, artifact: &ArtifactEnvelope) -> Result<(), ProjectionError> {
        let id = artifact.artifact_id.as_str();
        let kind = artifact.kind();
        for dep in &artifact.dependencies {
            self.add_edge(id, &dep.artifact_id)?;
        }
        let payload = payload_json(artifact)?;
        match &artifact.payload {
            ArtifactPayload::Entity(_) => self.ingest_entity(id, payload)?,
            ArtifactPayload::Relation(_) => {
                let rel: RelationRoot = decode_payload!(id, kind, payload)?;
                self.add_type_edge(id, &rel.from)?;
                self.add_type_edge(id, &rel.to)?;
            }
            ArtifactPayload::Action(_) => self.ingest_action(id, payload)?,
            ArtifactPayload::Rule(_) => self.ingest_rule(id, artifact, payload)?,
            ArtifactPayload::Policy(_) => self.ingest_policy(id, payload)?,
            ArtifactPayload::Law(_) => self.ingest_law(id, payload)?,
            ArtifactPayload::Derived(_) => {
                let view: ViewIrPayload = decode_payload!(id, kind, payload)?;
                for input in &view.inputs {
                    self.add_edge(id, &input.artifact_id)?;
                }
            }
            ArtifactPayload::Mapping(_) => self.ingest_mapping(id, payload)?,
            ArtifactPayload::DefinedConcept(_) => {
                let concept: DefinedConceptRoot = decode_payload!(id, kind, payload)?;
                self.add_specialization(id, &concept.base.artifact_id)?;
                for dep in &concept.dependencies {
                    self.add_edge(id, &dep.artifact_id)?;
                }
            }
            ArtifactPayload::Widget(_) => self.ingest_widget(id, payload)?,
            ArtifactPayload::Model(_) => {
                let model: ModelRoot = decode_payload!(id, kind, payload)?;
                self.add_type_edge(id, model.input.object_type().as_str())?;
                if let Some(predictions) = &model.predictions {
                    self.add_type_edge(id, predictions.object_type.as_str())?;
                }
            }
            ArtifactPayload::Customization(_) => {
                let custom: CustomizationRoot = decode_payload!(id, kind, payload)?;
                self.add_edge(id, &custom.target_artifact.artifact_id)?;
            }
            ArtifactPayload::Labels(_) => self.ingest_labels(id, payload)?,
            ArtifactPayload::BusinessPack(_) => {
                let pack_root: BusinessPackRoot = decode_payload!(id, kind, payload)?;
                if let BusinessPackRoot::Composition(pack) = pack_root {
                    for member in &pack.artifacts {
                        self.add_edge(id, &member.artifact_id)?;
                    }
                }
            }
            ArtifactPayload::Connector(_)
            | ArtifactPayload::Setting(_)
            | ArtifactPayload::SourceSchema(_) => {}
        }
        Ok(())
    }

    fn ingest_entity(&mut self, id: &str, payload: Value) -> Result<(), ProjectionError> {
        let root: EntityRoot = decode_payload!(id, ArtifactKind::Entity, payload)?;
        match root {
            EntityRoot::V2(v2) => {
                let name = v2.object_type.name.as_str();
                for parent in v2
                    .object_type
                    .extends
                    .iter()
                    .map(norn_ir::ObjectTypeName::as_str)
                    .chain(
                        v2.object_type
                            .implements
                            .iter()
                            .map(norn_ir::InterfaceName::as_str),
                    )
                {
                    self.tick()?;
                    self.type_parents
                        .entry(name.to_owned())
                        .or_default()
                        .insert(parent.to_owned());
                    if let Some(owner) = self.universe.type_owners.get(parent) {
                        self.add_specialization(id, owner)?;
                    }
                }
                for link in &v2.object_type.links {
                    self.add_type_edge(id, link.target.as_str())?;
                }
            }
            EntityRoot::V2Interface(iface) => {
                let name = iface.interface.name.as_str();
                for parent in &iface.interface.extends {
                    self.tick()?;
                    self.type_parents
                        .entry(name.to_owned())
                        .or_default()
                        .insert(parent.as_str().to_owned());
                    if let Some(owner) = self.universe.type_owners.get(parent.as_str()) {
                        self.add_specialization(id, owner)?;
                    }
                }
                for link in &iface.interface.links {
                    self.add_type_edge(id, link.target.as_str())?;
                }
            }
            EntityRoot::V2Extension(ext) => {
                if let Some(owner) = self
                    .universe
                    .type_owners
                    .get(ext.object_type_extension.target.as_str())
                {
                    self.add_specialization(id, owner)?;
                }
                for link in &ext.object_type_extension.links {
                    self.add_type_edge(id, link.target.as_str())?;
                }
            }
            EntityRoot::V1(_) | EntityRoot::V2PropertyTypes(_) | EntityRoot::V2Markings(_) => {}
        }
        Ok(())
    }

    fn ingest_action(&mut self, id: &str, payload: Value) -> Result<(), ProjectionError> {
        let action: ActionRoot = decode_payload!(id, ArtifactKind::Action, payload)?;
        let mut target_types = BTreeSet::new();
        for write in &action.writes {
            self.add_type_edge(id, &write.entity_kind)?;
            self.type_writers
                .entry(write.entity_kind.clone())
                .or_default()
                .insert(id.to_owned());
            target_types.insert(write.entity_kind.clone());
        }
        if let Some(program) = &action.effect_program {
            let (entity_artifact_id, object_kind, is_mutate_rows) = match program {
                ActionEffectProgram::UpsertInputObject {
                    entity_artifact_id,
                    object_kind,
                    ..
                }
                | ActionEffectProgram::FireTransition {
                    entity_artifact_id,
                    object_kind,
                    ..
                } => (entity_artifact_id.as_str(), object_kind.as_str(), false),
                ActionEffectProgram::MutateParentWithRows {
                    entity_artifact_id,
                    object_kind,
                    ..
                } => (entity_artifact_id.as_str(), object_kind.as_str(), true),
            };
            self.add_edge(id, entity_artifact_id)?;
            self.add_type_edge(id, object_kind)?;
            self.type_writers
                .entry(object_kind.to_owned())
                .or_default()
                .insert(id.to_owned());
            target_types.insert(object_kind.to_owned());
            if is_mutate_rows && let Some(children) = self.universe.owned_children.get(object_kind)
            {
                for child_type in children {
                    self.add_type_edge(id, child_type)?;
                    self.type_writers
                        .entry(child_type.clone())
                        .or_default()
                        .insert(id.to_owned());
                }
            }
        }
        for slice in &action.slice {
            self.add_type_edge(id, slice.object_type.as_str())?;
        }
        for agg in &action.aggregates {
            self.add_type_edge(id, agg.object_type.as_str())?;
        }
        if let Some(rule) = &action.synchronous_rule {
            self.add_edge(id, &rule.artifact_id)?;
        }
        if let Some(access) = &action.access {
            self.add_edge(id, &access.artifact_id)?;
            for target_type in &target_types {
                self.add_type_edge(&access.artifact_id, target_type)?;
            }
        }
        if let Some(comp) = &action.compensation {
            self.add_action_edge(id, &comp.action_id)?;
        }
        Ok(())
    }

    fn ingest_rule(
        &mut self,
        id: &str,
        artifact: &ArtifactEnvelope,
        payload: Value,
    ) -> Result<(), ProjectionError> {
        let rule: RuleRoot = decode_payload!(id, ArtifactKind::Rule, payload)?;
        for slice in &rule.slice {
            self.add_type_edge(id, slice.object_type.as_str())?;
        }
        for cmd in &rule.source_commands {
            self.add_edge(id, &cmd.connector)?;
            self.add_edge(id, &cmd.mapping)?;
        }
        // Project decide-graph fact-write targets in O(1) per rule (matching definition_catalog::fact_writes).
        let raw_payload = artifact.payload.to_json_value();
        if let Some(object) = raw_payload.as_object()
            && object
                .get("contract")
                .and_then(norn_ir::JsonValue::as_string)
                == Some("decide")
            && object.contains_key("graph")
            && let Some(output) = object.get("outputSchema")
            && let Some(schema) = FactSchema::of(output)
        {
            for type_name in schema.values("objectType").unwrap_or_default() {
                self.add_type_edge(id, type_name)?;
                self.type_writers
                    .entry(type_name.to_owned())
                    .or_default()
                    .insert(id.to_owned());
            }
        }
        Ok(())
    }

    fn ingest_policy(&mut self, id: &str, payload: Value) -> Result<(), ProjectionError> {
        let policy: PolicyRoot = decode_payload!(id, ArtifactKind::Policy, payload)?;
        match policy {
            PolicyRoot::Access(access) => {
                for rule in &access.rules {
                    self.add_type_edge(id, rule.resource_type.as_str())?;
                    for action_id in &rule.actions {
                        self.add_action_edge(id, action_id)?;
                    }
                }
            }
            PolicyRoot::Obligations(ob) => {
                if let Some(target) = &ob.applies_to {
                    self.add_type_edge(id, target)?;
                    self.add_action_edge(id, target)?;
                }
                for item in &ob.obligations {
                    self.add_type_edge(id, &item.applies_to)?;
                    self.add_action_edge(id, &item.applies_to)?;
                }
            }
            PolicyRoot::Approval(approval) => {
                self.add_type_edge(id, &approval.applies_to)?;
                self.add_action_edge(id, &approval.applies_to)?;
            }
        }
        Ok(())
    }

    fn ingest_law(&mut self, id: &str, payload: Value) -> Result<(), ProjectionError> {
        let law: LawIR = decode_payload!(id, ArtifactKind::Law, payload)?;
        self.add_type_edge(id, &law.subject)?;
        self.law_subjects.push((id.to_owned(), law.subject.clone()));
        for ty in law.scope.object_types.iter().chain(&law.scope.commitments) {
            self.add_type_edge(id, ty)?;
        }
        match &law.response {
            LawResponse::Action { artifact_id } => self.add_edge(id, artifact_id)?,
            LawResponse::Invariant { object_type, .. }
            | LawResponse::Transition { object_type, .. } => {
                self.add_type_edge(id, object_type)?;
            }
            LawResponse::TranslationPreserves { target_type, .. } => {
                self.add_type_edge(id, target_type)?;
            }
            LawResponse::Predicate { .. } => {}
        }
        Ok(())
    }

    fn ingest_mapping(&mut self, id: &str, payload: Value) -> Result<(), ProjectionError> {
        let doc: MappingDocument = decode_payload!(id, ArtifactKind::Mapping, payload)?;
        match doc {
            MappingDocument::SourceRecord(mapping) => {
                self.add_edge(id, &mapping.source.schema_id)?;
                for iface in &mapping.interfaces {
                    self.add_edge(id, iface)?;
                }
                for map in &mapping.maps {
                    self.add_edge(id, &map.target.entity)?;
                    if let Some(types) = self.universe.owner_types.get(&map.target.entity) {
                        for type_name in types {
                            self.tick()?;
                            self.type_writers
                                .entry(type_name.clone())
                                .or_default()
                                .insert(id.to_owned());
                        }
                    }
                }
                for path_map in &mapping.path_mappings {
                    for entity_id in path_map
                        .source_entities
                        .iter()
                        .chain(&path_map.target_entities)
                    {
                        self.add_edge(id, entity_id)?;
                    }
                }
                for ent in &mapping.entitlements {
                    self.add_type_edge(id, ent.target_type.as_str())?;
                }
            }
            MappingDocument::LawInterpretation(interp) => {
                self.add_edge(id, &interp.source_layer.artifact_id)?;
                self.add_edge(id, &interp.target_layer.artifact_id)?;
            }
        }
        Ok(())
    }

    fn ingest_widget(&mut self, id: &str, payload: Value) -> Result<(), ProjectionError> {
        let widget: WidgetRoot = decode_payload!(id, ArtifactKind::Widget, payload)?;
        for data in &widget.data_dependencies {
            self.add_type_edge(id, &data.object_kind)?;
        }
        for binding in &widget.action_bindings {
            self.add_action_edge(id, &binding.action_id)?;
        }
        if let Some(ux) = &widget.ux {
            for data in &ux.data_bindings {
                self.add_edge(id, &data.object.id)?;
            }
            for binding in &ux.action_bindings {
                self.add_edge(id, &binding.action.id)?;
            }
        }
        Ok(())
    }

    fn ingest_labels(&mut self, id: &str, payload: Value) -> Result<(), ProjectionError> {
        let labels: LabelCatalogRoot = decode_payload!(id, ArtifactKind::Labels, payload)?;
        for entry in &labels.entries {
            match &entry.target {
                LabelTarget::ObjectType { object_type }
                | LabelTarget::Property { object_type, .. }
                | LabelTarget::EnumValue { object_type, .. } => {
                    self.add_type_edge(id, object_type)?;
                }
                LabelTarget::Action { action_id } => self.add_action_edge(id, action_id)?,
                LabelTarget::Rule { rule_id } => {
                    self.tick()?;
                    if let Some(owner) = self.universe.rule_owners.get(rule_id) {
                        self.add_edge(id, owner)?;
                    }
                }
                LabelTarget::Law { law_id } => {
                    self.tick()?;
                    if let Some(owner) = self.universe.law_owners.get(law_id) {
                        self.add_edge(id, owner)?;
                    }
                }
                LabelTarget::Page { widget_id, .. } => {
                    self.tick()?;
                    if let Some(owner) = self.universe.widget_owners.get(widget_id) {
                        self.add_edge(id, owner)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn add_law_writer_edges(&mut self) -> Result<(), ProjectionError> {
        let mut children_by_parent: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (child, parents) in &self.type_parents {
            for parent in parents {
                tick_step(self.steps, self.budget)?;
                children_by_parent
                    .entry(parent.clone())
                    .or_default()
                    .push(child.clone());
            }
        }
        let subjects = self.law_subjects.clone();
        for (law_id, subject_type) in subjects {
            let mut covered_types = BTreeSet::from([subject_type.clone()]);
            let mut queue = VecDeque::from([subject_type]);
            while let Some(current) = queue.pop_front() {
                self.tick()?;
                if let Some(children) = children_by_parent.get(&current) {
                    for child in children {
                        self.tick()?;
                        if covered_types.insert(child.clone()) {
                            queue.push_back(child.clone());
                        }
                    }
                }
            }
            for covered in covered_types {
                if let Some(writers) = self.type_writers.get(&covered).cloned() {
                    for writer_id in writers {
                        self.add_edge(&law_id, &writer_id)?;
                    }
                }
            }
        }
        Ok(())
    }
}
