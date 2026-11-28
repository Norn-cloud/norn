#![forbid(unsafe_code)]
//! Dependency graph projection for [`super::compute_ripple_effects_with_budget`].

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use norn_graph::{Budget, Limit};
use norn_ir::access::PolicyRoot;
use norn_ir::action::ActionRoot;
use norn_ir::business_pack::BusinessPackRoot;
use norn_ir::customization::CustomizationRoot;
use norn_ir::defined_concept::DefinedConceptRoot;
use norn_ir::definition_catalog::{DefinitionCatalog, LawBinding, Member, MemberRef};
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

use super::BudgetRefusal;

pub(super) struct Universe {
    pub(super) all_ids: BTreeSet<String>,
    pub(super) removed_ids: BTreeSet<String>,
    pub(super) roots: BTreeSet<String>,
    pub(super) artifact_kinds: BTreeMap<String, ArtifactKind>,
    pub(super) type_owners: BTreeMap<String, String>,
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
    ) -> Self {
        let mut universe = Self {
            all_ids: BTreeSet::new(),
            removed_ids: BTreeSet::new(),
            roots: BTreeSet::new(),
            artifact_kinds: BTreeMap::new(),
            type_owners: BTreeMap::new(),
            action_owners: BTreeMap::new(),
            rule_owners: BTreeMap::new(),
            law_owners: BTreeMap::new(),
            widget_owners: BTreeMap::new(),
            has_lifecycle: BTreeSet::new(),
        };
        let mut after_ids = BTreeSet::new();

        for artifact in before_artifacts {
            universe.all_ids.insert(artifact.artifact_id.clone());
            universe
                .artifact_kinds
                .insert(artifact.artifact_id.clone(), artifact.kind());
            universe.index_symbols(artifact);
        }
        for artifact in after_artifacts {
            universe.all_ids.insert(artifact.artifact_id.clone());
            after_ids.insert(artifact.artifact_id.clone());
            universe
                .artifact_kinds
                .insert(artifact.artifact_id.clone(), artifact.kind());
            universe.index_symbols(artifact);
        }

        universe.removed_ids = before_artifacts
            .iter()
            .filter(|artifact| !after_ids.contains(&artifact.artifact_id))
            .map(|artifact| artifact.artifact_id.clone())
            .collect();

        universe.roots.clone_from(&universe.removed_ids);
        for changed in changed_artifact_ids {
            if universe.all_ids.contains(changed) {
                universe.roots.insert(changed.clone());
            }
        }
        universe
    }

    fn index_symbols(&mut self, artifact: &ArtifactEnvelope) {
        let payload = payload_json(artifact);
        match artifact.kind() {
            ArtifactKind::Entity => {
                if let Ok(root) = serde_json::from_value::<EntityRoot>(payload) {
                    match root {
                        EntityRoot::V2(v2) => {
                            self.type_owners.insert(
                                v2.object_type.name.as_str().to_owned(),
                                artifact.artifact_id.clone(),
                            );
                            if !v2.object_type.lifecycles.is_empty() {
                                self.has_lifecycle.insert(artifact.artifact_id.clone());
                            }
                        }
                        EntityRoot::V2Interface(iface) => {
                            self.type_owners.insert(
                                iface.interface.name.as_str().to_owned(),
                                artifact.artifact_id.clone(),
                            );
                            if !iface.interface.lifecycles.is_empty() {
                                self.has_lifecycle.insert(artifact.artifact_id.clone());
                            }
                        }
                        EntityRoot::V1(v1) => {
                            self.type_owners
                                .insert(v1.object_kind, artifact.artifact_id.clone());
                        }
                        EntityRoot::V2Extension(_)
                        | EntityRoot::V2PropertyTypes(_)
                        | EntityRoot::V2Markings(_) => {}
                    }
                }
            }
            ArtifactKind::Action => {
                if let Ok(action) = serde_json::from_value::<ActionRoot>(payload) {
                    self.action_owners
                        .insert(action.action_id, artifact.artifact_id.clone());
                }
            }
            ArtifactKind::Rule => {
                if let Ok(rule) = serde_json::from_value::<RuleRoot>(payload) {
                    self.rule_owners
                        .insert(rule.rule_id, artifact.artifact_id.clone());
                }
            }
            ArtifactKind::Law => {
                if let Ok(law) = serde_json::from_value::<LawIR>(payload) {
                    self.law_owners.insert(law.id, artifact.artifact_id.clone());
                }
            }
            ArtifactKind::Widget => {
                if let Ok(widget) = serde_json::from_value::<WidgetRoot>(payload) {
                    self.widget_owners
                        .insert(widget.widget_id, artifact.artifact_id.clone());
                }
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
    }
}

pub(super) struct Projection<'a> {
    universe: &'a Universe,
    budget: Budget,
    pub(super) edges: BTreeSet<(String, String)>,
    pub(super) specialization_edges: BTreeSet<(String, String)>,
    type_parents: BTreeMap<String, BTreeSet<String>>,
    type_writers: BTreeMap<String, BTreeSet<String>>,
    law_subjects: Vec<(String, String)>,
    steps: usize,
}

impl<'a> Projection<'a> {
    pub(super) fn new(universe: &'a Universe, budget: Budget) -> Self {
        Self {
            universe,
            budget,
            edges: BTreeSet::new(),
            specialization_edges: BTreeSet::new(),
            type_parents: BTreeMap::new(),
            type_writers: BTreeMap::new(),
            law_subjects: Vec::new(),
            steps: 0,
        }
    }

    fn tick(&mut self) -> Result<(), BudgetRefusal> {
        self.steps = self.steps.saturating_add(1);
        if self.steps > self.budget.max_steps {
            return Err(BudgetRefusal::from_limit(
                Limit::Steps,
                self.budget.max_steps,
            ));
        }
        Ok(())
    }

    fn add_edge(&mut self, dependent: &str, dependency: &str) -> Result<(), BudgetRefusal> {
        self.tick()?;
        if dependent == dependency
            || !self.universe.all_ids.contains(dependent)
            || !self.universe.all_ids.contains(dependency)
        {
            return Ok(());
        }
        self.edges
            .insert((dependent.to_owned(), dependency.to_owned()));
        if self.edges.len() > self.budget.max_edges {
            return Err(BudgetRefusal::from_limit(
                Limit::Edges,
                self.budget.max_edges,
            ));
        }
        Ok(())
    }

    fn add_specialization(
        &mut self,
        dependent: &str,
        dependency: &str,
    ) -> Result<(), BudgetRefusal> {
        if dependent != dependency
            && self.universe.all_ids.contains(dependent)
            && self.universe.all_ids.contains(dependency)
        {
            self.specialization_edges
                .insert((dependent.to_owned(), dependency.to_owned()));
        }
        self.add_edge(dependent, dependency)
    }

    fn add_type_edge(&mut self, dependent: &str, type_name: &str) -> Result<(), BudgetRefusal> {
        if let Some(owner) = self.universe.type_owners.get(type_name) {
            self.add_edge(dependent, owner)?;
        }
        Ok(())
    }

    fn add_action_edge(&mut self, dependent: &str, action_id: &str) -> Result<(), BudgetRefusal> {
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
    ) -> Result<(), BudgetRefusal> {
        let empty_registry = BTreeSet::new();
        if let Ok(catalog) =
            crate::definition_catalog::project(after_artifacts, &[], &empty_registry)
        {
            self.ingest_catalog(&catalog)?;
        }
        for artifact in after_artifacts {
            self.ingest_artifact(artifact)?;
        }
        for artifact in before_artifacts {
            if self.universe.removed_ids.contains(&artifact.artifact_id) {
                self.ingest_removed_type_hierarchy(artifact)?;
            }
        }
        self.add_law_writer_edges()
    }

    fn ingest_catalog(&mut self, catalog: &DefinitionCatalog) -> Result<(), BudgetRefusal> {
        for edge in &catalog.depends_on {
            self.add_edge(&edge.from.artifact_id, &edge.to.artifact_id)?;
        }
        for row in &catalog.types {
            let child_name = member_type_name(&row.at);
            for parent in row.extends.iter().chain(&row.implements) {
                if let Some(child) = child_name {
                    self.type_parents
                        .entry(child.to_owned())
                        .or_default()
                        .insert(parent.clone());
                }
                if let Some(owner) = self.universe.type_owners.get(parent) {
                    self.add_specialization(&row.at.artifact_id, owner)?;
                }
            }
        }
        for link in &catalog.links {
            self.add_type_edge(&link.at.artifact_id, &link.target)?;
        }
        for write in &catalog.writes {
            self.add_edge(&write.writer.artifact_id, &write.target.artifact_id)?;
            if let Some(type_name) = member_type_name(&write.target) {
                self.type_writers
                    .entry(type_name.to_owned())
                    .or_default()
                    .insert(write.writer.artifact_id.clone());
            }
        }
        for mapping in &catalog.mappings {
            self.add_edge(&mapping.map.artifact_id, &mapping.target.artifact_id)?;
            if let Some(type_name) = member_type_name(&mapping.target) {
                self.type_writers
                    .entry(type_name.to_owned())
                    .or_default()
                    .insert(mapping.map.artifact_id.clone());
            }
        }
        for law in &catalog.laws {
            self.add_edge(&law.at.artifact_id, &law.subject.artifact_id)?;
            if let Some(subject_name) = member_type_name(&law.subject) {
                self.law_subjects
                    .push((law.at.artifact_id.clone(), subject_name.to_owned()));
            }
            if let Some(binding) = &law.binds {
                match binding {
                    LawBinding::Transition { target } | LawBinding::Action { target } => {
                        self.add_edge(&law.at.artifact_id, &target.artifact_id)?;
                    }
                    LawBinding::Invariant { owner, .. } => {
                        self.add_edge(&law.at.artifact_id, &owner.artifact_id)?;
                    }
                }
            }
        }
        for facet in &catalog.facets {
            self.add_edge(&facet.member.artifact_id, &facet.concept.artifact_id)?;
        }
        Ok(())
    }

    fn ingest_removed_type_hierarchy(
        &mut self,
        artifact: &ArtifactEnvelope,
    ) -> Result<(), BudgetRefusal> {
        if artifact.kind() != ArtifactKind::Entity {
            return Ok(());
        }
        let Ok(root) = serde_json::from_value::<EntityRoot>(payload_json(artifact)) else {
            return Ok(());
        };
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

    fn ingest_artifact(&mut self, artifact: &ArtifactEnvelope) -> Result<(), BudgetRefusal> {
        let id = artifact.artifact_id.as_str();
        for dep in &artifact.dependencies {
            self.add_edge(id, &dep.artifact_id)?;
        }
        let payload = payload_json(artifact);
        match &artifact.payload {
            ArtifactPayload::Entity(_) => self.ingest_entity(id, payload)?,
            ArtifactPayload::Relation(_) => {
                if let Ok(rel) = serde_json::from_value::<RelationRoot>(payload) {
                    self.add_type_edge(id, &rel.from)?;
                    self.add_type_edge(id, &rel.to)?;
                }
            }
            ArtifactPayload::Action(_) => self.ingest_action(id, payload)?,
            ArtifactPayload::Rule(_) => self.ingest_rule(id, payload)?,
            ArtifactPayload::Policy(_) => self.ingest_policy(id, payload)?,
            ArtifactPayload::Law(_) => self.ingest_law(id, payload)?,
            ArtifactPayload::Derived(_) => {
                if let Ok(view) = serde_json::from_value::<ViewIrPayload>(payload) {
                    for input in &view.inputs {
                        self.add_edge(id, &input.artifact_id)?;
                    }
                }
            }
            ArtifactPayload::Mapping(_) => self.ingest_mapping(id, payload)?,
            ArtifactPayload::DefinedConcept(_) => {
                if let Ok(concept) = serde_json::from_value::<DefinedConceptRoot>(payload) {
                    self.add_specialization(id, &concept.base.artifact_id)?;
                    for dep in &concept.dependencies {
                        self.add_edge(id, &dep.artifact_id)?;
                    }
                }
            }
            ArtifactPayload::Widget(_) => self.ingest_widget(id, payload)?,
            ArtifactPayload::Model(_) => {
                if let Ok(model) = serde_json::from_value::<ModelRoot>(payload) {
                    self.add_type_edge(id, model.input.object_type().as_str())?;
                    if let Some(predictions) = &model.predictions {
                        self.add_type_edge(id, predictions.object_type.as_str())?;
                    }
                }
            }
            ArtifactPayload::Customization(_) => {
                if let Ok(custom) = serde_json::from_value::<CustomizationRoot>(payload) {
                    self.add_edge(id, &custom.target_artifact.artifact_id)?;
                }
            }
            ArtifactPayload::Labels(_) => self.ingest_labels(id, payload)?,
            ArtifactPayload::BusinessPack(_) => {
                if let Ok(BusinessPackRoot::Composition(pack)) = serde_json::from_value(payload) {
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

    fn ingest_entity(&mut self, id: &str, payload: Value) -> Result<(), BudgetRefusal> {
        let Ok(root) = serde_json::from_value::<EntityRoot>(payload) else {
            return Ok(());
        };
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

    fn ingest_action(&mut self, id: &str, payload: Value) -> Result<(), BudgetRefusal> {
        let Ok(action) = serde_json::from_value::<ActionRoot>(payload) else {
            return Ok(());
        };
        for write in &action.writes {
            self.add_type_edge(id, &write.entity_kind)?;
            self.type_writers
                .entry(write.entity_kind.clone())
                .or_default()
                .insert(id.to_owned());
        }
        if let Some(program) = &action.effect_program {
            let (entity_artifact_id, object_kind) = match program {
                ActionEffectProgram::UpsertInputObject {
                    entity_artifact_id,
                    object_kind,
                    ..
                }
                | ActionEffectProgram::FireTransition {
                    entity_artifact_id,
                    object_kind,
                    ..
                }
                | ActionEffectProgram::MutateParentWithRows {
                    entity_artifact_id,
                    object_kind,
                    ..
                } => (entity_artifact_id.as_str(), object_kind.as_str()),
            };
            self.add_edge(id, entity_artifact_id)?;
            self.add_type_edge(id, object_kind)?;
            self.type_writers
                .entry(object_kind.to_owned())
                .or_default()
                .insert(id.to_owned());
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
        }
        if let Some(comp) = &action.compensation {
            self.add_action_edge(id, &comp.action_id)?;
        }
        Ok(())
    }

    fn ingest_rule(&mut self, id: &str, payload: Value) -> Result<(), BudgetRefusal> {
        let Ok(rule) = serde_json::from_value::<RuleRoot>(payload) else {
            return Ok(());
        };
        for slice in &rule.slice {
            self.add_type_edge(id, slice.object_type.as_str())?;
        }
        for cmd in &rule.source_commands {
            self.add_edge(id, &cmd.connector)?;
            self.add_edge(id, &cmd.mapping)?;
        }
        Ok(())
    }

    fn ingest_policy(&mut self, id: &str, payload: Value) -> Result<(), BudgetRefusal> {
        let Ok(policy) = serde_json::from_value::<PolicyRoot>(payload) else {
            return Ok(());
        };
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

    fn ingest_law(&mut self, id: &str, payload: Value) -> Result<(), BudgetRefusal> {
        let Ok(law) = serde_json::from_value::<LawIR>(payload) else {
            return Ok(());
        };
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

    fn ingest_mapping(&mut self, id: &str, payload: Value) -> Result<(), BudgetRefusal> {
        let Ok(doc) = serde_json::from_value::<MappingDocument>(payload) else {
            return Ok(());
        };
        match doc {
            MappingDocument::SourceRecord(mapping) => {
                self.add_edge(id, &mapping.source.schema_id)?;
                for iface in &mapping.interfaces {
                    self.add_edge(id, iface)?;
                }
                for map in &mapping.maps {
                    self.add_edge(id, &map.target.entity)?;
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

    fn ingest_widget(&mut self, id: &str, payload: Value) -> Result<(), BudgetRefusal> {
        let Ok(widget) = serde_json::from_value::<WidgetRoot>(payload) else {
            return Ok(());
        };
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

    fn ingest_labels(&mut self, id: &str, payload: Value) -> Result<(), BudgetRefusal> {
        let Ok(labels) = serde_json::from_value::<LabelCatalogRoot>(payload) else {
            return Ok(());
        };
        for entry in &labels.entries {
            match &entry.target {
                LabelTarget::ObjectType { object_type }
                | LabelTarget::Property { object_type, .. }
                | LabelTarget::EnumValue { object_type, .. } => {
                    self.add_type_edge(id, object_type)?;
                }
                LabelTarget::Action { action_id } => self.add_action_edge(id, action_id)?,
                LabelTarget::Rule { rule_id } => {
                    if let Some(owner) = self.universe.rule_owners.get(rule_id) {
                        self.add_edge(id, owner)?;
                    }
                }
                LabelTarget::Law { law_id } => {
                    if let Some(owner) = self.universe.law_owners.get(law_id) {
                        self.add_edge(id, owner)?;
                    }
                }
                LabelTarget::Page { widget_id, .. } => {
                    if let Some(owner) = self.universe.widget_owners.get(widget_id) {
                        self.add_edge(id, owner)?;
                    }
                }
            }
        }
        Ok(())
    }

    fn add_law_writer_edges(&mut self) -> Result<(), BudgetRefusal> {
        let mut children_by_parent: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (child, parents) in &self.type_parents {
            for parent in parents {
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

fn member_type_name(member_ref: &MemberRef) -> Option<&str> {
    match &member_ref.member {
        Member::Type { type_name }
        | Member::Property { type_name, .. }
        | Member::Link { type_name, .. }
        | Member::State { type_name, .. }
        | Member::Transition { type_name, .. } => Some(type_name.as_str()),
        Member::Artifact {} | Member::ObjectMap { .. } | Member::Law { .. } => None,
    }
}

fn payload_json(artifact: &ArtifactEnvelope) -> Value {
    serde_json::to_value(artifact.payload.to_json_value()).unwrap_or_default()
}
