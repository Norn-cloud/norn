//! Read-only catalog projection from admitted definitions.
use crate::action_set::{Admission, AdmittedType};
use crate::entity_artifacts::{Decoded, Layer, decode_v2};
use crate::entity_lowering::{EntityForm, entity_object_type};
use crate::lifecycle_writes::{FactSchema, fact_decisions};
use crate::receipt::ArtifactRef;
/// Failure to compute a projected artifact's semantic identity.
pub use crate::receipt::ReceiptError as CatalogError;
use norn_ir::definition_catalog::{
    ArtifactKey, CatalogArtifact, CatalogFormat, ConceptFacet, ConceptFacetRow, DefinitionCatalog,
    DependencyVia, DependsOnRow, LawBinding, LawRow, LifecycleStates, LinkRow, MapRule, MappingRow,
    Member, MemberRef, PropertyRow, TransitionRow, TypeKind, TypeRow, WriteVia, WritesRow,
};
use norn_ir::mapping::MappingIr;
use norn_ir::{
    ActionEffectProgram, ActionRoot, ArtifactEnvelope, ArtifactKind, ArtifactPayload,
    BusinessPackRoot, EntityRoot, FactChangeOp, LawIR, LawResponse, Lifecycle, ObjectTypeDef,
    Property, PropertyType, PropertyTypeDef, PropertyTypeName,
};
use std::collections::BTreeSet;
/// Project admitted candidates and installed artifacts into sorted catalog rows.
///
/// Call only after `check_artifact_set` admits the set. This slice projects
/// entities and dependency edges, action writes, decide-graph writes, mappings and laws. See verification/definition-catalog-v1/SPEC.md.
///
/// # Errors
/// Returns `CatalogError` if an artifact semantic hash cannot be computed.
pub fn project(
    artifacts: &[ArtifactEnvelope],
    base: &[ArtifactEnvelope],
    registry: &BTreeSet<PropertyTypeName>,
) -> Result<DefinitionCatalog, CatalogError> {
    let all: Vec<&ArtifactEnvelope> = artifacts.iter().chain(base).collect();
    let mut layer = Layer::default();
    for (index, artifact) in base.iter().chain(artifacts).enumerate() {
        if let Decoded::Root(root) = decode_v2(artifact) {
            layer.push(index, *root);
        }
    }
    let mut rows = Rows {
        property_types: layer.catalog.into_iter().map(|(_, def)| def).collect(),
        ..Rows::default()
    };
    for artifact in &all {
        let mut layer = crate::entity_artifacts::Layer::default();
        let context = std::iter::once(*artifact).chain(
            all.iter()
                .copied()
                .filter(|other| other.artifact_id != artifact.artifact_id),
        );
        for (index, artifact) in context.enumerate() {
            if let Decoded::Root(root) = decode_v2(artifact) {
                layer.push(index, *root);
            }
        }
        rows.roles.insert(
            key(artifact),
            crate::concept_roles::resolve(layer.as_model(), crate::EntityModel::EMPTY)
                .into_iter()
                .filter_map(|(name, role)| role.ok().map(|role| (name, role)))
                .collect(),
        );
    }
    for (index, artifact) in all.iter().enumerate() {
        let reference = ArtifactRef::of(artifact)?;
        rows.artifacts.insert(CatalogArtifact {
            artifact_id: reference.artifact_id,
            version: reference.version,
            kind: artifact.kind(),
            sha256: reference.sha256,
            candidate: index < artifacts.len(),
        });
    }
    let combined: Vec<_> = all.iter().map(|artifact| (*artifact).clone()).collect();
    let effective = crate::EffectiveCatalog::from_artifacts_with_registry(&combined, registry);
    for artifact in &all {
        rows.entity(artifact, &effective);
        rows.depends_on(artifact, &all);
    }
    let admission = Admission::new(artifacts, base, registry);
    for artifact in &all {
        if matches!(artifact.payload, ArtifactPayload::Action(_)) {
            rows.action(artifact, &admission);
        }
        if matches!(artifact.payload, ArtifactPayload::Mapping(_)) {
            rows.mapping(artifact, &admission);
        }
        if matches!(artifact.payload, ArtifactPayload::Law(_)) {
            rows.law(artifact, &all, &admission);
        }
    }
    for (artifact, output) in fact_decisions(all.iter().copied()) {
        if let Some(schema) = FactSchema::of(&output) {
            rows.fact_writes(artifact, &schema, &admission);
        }
    }
    Ok(rows.finish())
}
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct PinnedType {
    artifact: ArtifactKey,
    name: String,
}

impl PinnedType {
    fn of(admitted: AdmittedType<'_, '_>) -> Self {
        Self {
            artifact: key(admitted.artifact()),
            name: admitted.name().to_owned(),
        }
    }

    fn lineage(admitted: AdmittedType<'_, '_>, admission: &Admission<'_>) -> Vec<Self> {
        std::iter::once(Self::of(admitted))
            .chain(
                admitted
                    .supertypes()
                    .into_iter()
                    .filter_map(|name| admission.named(name).map(Self::of)),
            )
            .collect()
    }

    fn member(&self, member: Member) -> MemberRef {
        member_of(&self.artifact, member)
    }

    fn type_ref(&self) -> MemberRef {
        self.member(Member::Type {
            type_name: self.name.clone(),
        })
    }
}

#[derive(Default)]
struct Rows {
    property_types: Vec<PropertyTypeDef>,
    artifacts: BTreeSet<CatalogArtifact>,
    types: BTreeSet<TypeRow>,
    facets: BTreeSet<ConceptFacetRow>,
    roles: std::collections::BTreeMap<
        ArtifactKey,
        std::collections::BTreeMap<String, norn_ir::ConceptRole>,
    >,
    properties: BTreeSet<PropertyRow>,
    links: BTreeSet<LinkRow>,
    transitions: BTreeSet<TransitionRow>,
    states: BTreeSet<MemberRef>,
    writes: BTreeSet<WritesRow>,
    depends_on: BTreeSet<DependsOnRow>,
    mappings: BTreeSet<MappingRow>,
    laws: BTreeSet<LawRow>,
}

fn at(artifact: &ArtifactEnvelope, member: Member) -> MemberRef {
    MemberRef {
        artifact_id: artifact.artifact_id.clone(),
        version: artifact.version.clone(),
        member,
    }
}

fn key(artifact: &ArtifactEnvelope) -> ArtifactKey {
    ArtifactKey {
        artifact_id: artifact.artifact_id.clone(),
        version: artifact.version.clone(),
    }
}

fn payload_json(artifact: &ArtifactEnvelope) -> serde_json::Value {
    serde_json::to_value(artifact.payload.to_json_value()).unwrap_or_default()
}

fn type_member(type_name: String) -> Member {
    Member::Type { type_name }
}

fn property_member(type_name: &str, property: &str) -> Member {
    Member::Property {
        type_name: type_name.to_owned(),
        property: property.to_owned(),
    }
}

fn with_type(member: &Member, type_name: String) -> Member {
    match member.clone() {
        Member::Property { property, .. } => Member::Property {
            type_name,
            property,
        },
        Member::Link { link, .. } => Member::Link { type_name, link },
        Member::Transition {
            lifecycle,
            transition,
            ..
        } => Member::Transition {
            type_name,
            lifecycle,
            transition,
        },
        other @ (Member::Artifact {}
        | Member::Type { .. }
        | Member::State { .. }
        | Member::ObjectMap { .. }
        | Member::Law { .. }) => other,
    }
}

fn member_of(key: &ArtifactKey, member: Member) -> MemberRef {
    MemberRef {
        artifact_id: key.artifact_id.clone(),
        version: key.version.clone(),
        member,
    }
}

impl Rows {
    /// GRAPH-META-1: lifecycle metadata follows admission's local enum resolution.
    fn lifecycle(&self, property: &Property, lifecycles: &[Lifecycle]) -> Option<LifecycleStates> {
        let lifecycle = lifecycles
            .iter()
            .find(|lifecycle| lifecycle.property == property.id)?;
        let PropertyType::Enum { values } = crate::property_types::resolved_property_type(
            &property.property_type,
            &self.property_types,
        )?
        else {
            return None;
        };
        Some(LifecycleStates {
            states: values.iter().map(|value| value.value.clone()).collect(),
            initial: lifecycle.initial.clone(),
            terminal: lifecycle.terminal.clone(),
        })
    }

    fn entity(&mut self, artifact: &ArtifactEnvelope, effective: &crate::EffectiveCatalog) {
        let decoded = decode_v2(artifact);
        if let (Decoded::NotV2, ArtifactPayload::Entity(payload)) = (&decoded, &artifact.payload)
            && let Ok(Some(object)) = entity_object_type(payload)
            && object.form == EntityForm::V1
        {
            self.object_type(artifact, &object.root.object_type, effective);
            return;
        }
        let Decoded::Root(root) = decoded else {
            return;
        };
        match *root {
            EntityRoot::V2(root) => self.object_type(artifact, &root.object_type, effective),
            EntityRoot::V2Interface(root) => {
                let def = &root.interface;
                let name = def.name.as_str().to_owned();
                self.types.insert(TypeRow {
                    at: at(artifact, type_member(name.clone())),
                    kind: TypeKind::Interface,
                    role: self
                        .roles
                        .get(&key(artifact))
                        .and_then(|roles| roles.get(&name))
                        .copied()
                        .unwrap_or(norn_ir::ConceptRole::Thing),
                    extends: root
                        .interface
                        .extends
                        .iter()
                        .map(|n| n.as_str().to_owned())
                        .collect(),
                    implements: Vec::new(),
                });
                for property in &def.properties {
                    self.properties.insert(PropertyRow {
                        at: at(artifact, property_member(&name, property.id.as_str())),
                        computed: false,
                        lifecycle: None,
                    });
                }
                for link in &def.links {
                    self.link(artifact, &name, link.id.as_str(), link.target.as_str());
                }
            }
            EntityRoot::V2Extension(root) => {
                let extension = root.object_type_extension;
                let name = extension.target.as_str().to_owned();
                for property in &extension.properties {
                    self.property(artifact, &name, property, &[]);
                }
                for link in &extension.links {
                    self.link(artifact, &name, link.id.as_str(), link.target.as_str());
                }
            }
            EntityRoot::V2PropertyTypes(_) | EntityRoot::V2Markings(_) | EntityRoot::V1(_) => {}
        }
    }

    fn object_type(
        &mut self,
        artifact: &ArtifactEnvelope,
        def: &ObjectTypeDef,
        effective: &crate::EffectiveCatalog,
    ) {
        let name = def.name.as_str().to_owned();
        self.types.insert(TypeRow {
            at: at(artifact, type_member(name.clone())),
            kind: TypeKind::ObjectType,
            role: self
                .roles
                .get(&key(artifact))
                .and_then(|roles| roles.get(&name))
                .copied()
                .unwrap_or(norn_ir::ConceptRole::Thing),
            extends: def.extends.iter().map(|n| n.as_str().to_owned()).collect(),
            implements: def
                .implements
                .iter()
                .map(|n| n.as_str().to_owned())
                .collect(),
        });
        for property in &def.properties {
            self.property(artifact, &name, property, &def.lifecycles);
        }
        for link in &def.links {
            self.link(artifact, &name, link.id.as_str(), link.target.as_str());
        }
        for lifecycle in &def.lifecycles {
            let states = def
                .properties
                .iter()
                .find(|p| p.id == lifecycle.property)
                .and_then(|p| self.lifecycle(p, &def.lifecycles))
                .map(|s| s.states)
                .or_else(|| {
                    effective
                        .object_type(&artifact.artifact_id)?
                        .ok()?
                        .lifecycle(lifecycle.property.as_str())
                        .map(|m| m.states.clone())
                });
            if let Some(states) = states {
                self.states.extend(states.iter().map(|state| {
                    at(
                        artifact,
                        Member::State {
                            type_name: name.clone(),
                            lifecycle: lifecycle.property.as_str().to_owned(),
                            state: state.clone(),
                        },
                    )
                }));
            }
            for transition in &lifecycle.transitions {
                self.transitions.insert(TransitionRow {
                    guards: transition.guards.clone(),
                    at: at(
                        artifact,
                        Member::Transition {
                            type_name: name.clone(),
                            lifecycle: lifecycle.property.as_str().to_owned(),
                            transition: transition.id.as_str().to_owned(),
                        },
                    ),
                    from: transition.from.clone(),
                    to: transition.to.clone(),
                });
            }
        }
    }

    fn property(
        &mut self,
        artifact: &ArtifactEnvelope,
        type_name: &str,
        property: &Property,
        lifecycles: &[Lifecycle],
    ) {
        self.properties.insert(PropertyRow {
            at: at(artifact, property_member(type_name, property.id.as_str())),
            computed: property.computed.is_some(),
            lifecycle: self.lifecycle(property, lifecycles),
        });
    }

    fn link(&mut self, artifact: &ArtifactEnvelope, type_name: &str, link: &str, target: &str) {
        self.links.insert(LinkRow {
            at: at(
                artifact,
                Member::Link {
                    type_name: type_name.to_owned(),
                    link: link.to_owned(),
                },
            ),
            target: target.to_owned(),
        });
    }

    fn concept_for(&self, member: &MemberRef, type_name: &str) -> Option<MemberRef> {
        let exact = MemberRef {
            member: Member::Type {
                type_name: type_name.to_owned(),
            },
            ..member.clone()
        };
        if self.types.iter().any(|row| row.at == exact) {
            return Some(exact);
        }
        let mut owners = self
            .types
            .iter()
            .filter(|row| row.at.member == exact.member);
        let first = owners.next()?;
        owners.next().is_none().then(|| first.at.clone())
    }

    fn finish(mut self) -> DefinitionCatalog {
        let members = self
            .properties
            .iter()
            .map(|row| (ConceptFacet::Props, &row.at))
            .chain(self.links.iter().map(|row| (ConceptFacet::Links, &row.at)))
            .chain(
                self.transitions
                    .iter()
                    .map(|row| (ConceptFacet::States, &row.at)),
            )
            .chain(self.states.iter().map(|row| (ConceptFacet::States, row)));
        for (facet, member) in members {
            let (Member::Property { type_name, .. }
            | Member::Link { type_name, .. }
            | Member::Transition { type_name, .. }
            | Member::State { type_name, .. }) = &member.member
            else {
                continue;
            };
            let Some(concept) = self.concept_for(member, type_name) else {
                continue;
            };
            self.facets.insert(ConceptFacetRow {
                concept,
                facet,
                member: member.clone(),
            });
        }
        self.facets
            .extend(self.laws.iter().map(|row| ConceptFacetRow {
                concept: row.subject.clone(),
                facet: ConceptFacet::Laws,
                member: row.at.clone(),
            }));
        DefinitionCatalog {
            format: CatalogFormat::V1,
            artifacts: self.artifacts.into_iter().collect(),
            types: self.types.into_iter().collect(),
            facets: self.facets.into_iter().collect(),
            properties: self.properties.into_iter().collect(),
            links: self.links.into_iter().collect(),
            transitions: self.transitions.into_iter().collect(),
            states: self.states.into_iter().collect(),
            writes: self.writes.into_iter().collect(),
            depends_on: self.depends_on.into_iter().collect(),
            mappings: self.mappings.into_iter().collect(),
            laws: self.laws.into_iter().collect(),
        }
    }

    fn depends_on(&mut self, artifact: &ArtifactEnvelope, all: &[&ArtifactEnvelope]) {
        let present = |id: &str, version: &str| {
            all.iter()
                .any(|other| other.artifact_id == id && other.version == version)
        };
        let mut edge = |id: &str, version: &str, via: DependencyVia| {
            if present(id, version) {
                self.depends_on.insert(DependsOnRow {
                    from: key(artifact),
                    to: ArtifactKey {
                        artifact_id: id.to_owned(),
                        version: version.to_owned(),
                    },
                    via,
                });
            }
        };
        for dependency in &artifact.dependencies {
            if let Some(version) = &dependency.version {
                edge(&dependency.artifact_id, version, DependencyVia::Dependency);
            }
        }
        if let ArtifactPayload::BusinessPack(_) = &artifact.payload
            && let Ok(BusinessPackRoot::Composition(pack)) =
                serde_json::from_value(payload_json(artifact))
        {
            for member in &pack.artifacts {
                edge(
                    &member.artifact_id,
                    &member.version,
                    DependencyVia::PackMember,
                );
            }
        }
    }

    fn action(&mut self, artifact: &ArtifactEnvelope, admission: &Admission<'_>) {
        let Ok(action) = serde_json::from_value::<ActionRoot>(payload_json(artifact)) else {
            return;
        };
        self.action_facets(artifact, &action, admission);
        let Some(program) = &action.effect_program else {
            return;
        };
        let Some(target) = admission.effect_target(program) else {
            return;
        };
        let lineage = PinnedType::lineage(target, admission);
        let pinned = PinnedType::of(target);
        let mut writes: Vec<MemberRef> = Vec::new();
        let via = match program {
            ActionEffectProgram::FireTransition {
                lifecycle,
                transition,
                ..
            } => {
                writes.extend(self.resolve(
                    &Member::Transition {
                        type_name: pinned.name,
                        lifecycle: lifecycle.as_str().to_owned(),
                        transition: transition.as_str().to_owned(),
                    },
                    &lineage,
                ));
                WriteVia::FireTransition
            }
            ActionEffectProgram::UpsertInputObject { .. } => {
                writes.push(pinned.type_ref());
                WriteVia::UpsertObject
            }
            ActionEffectProgram::MutateParentWithRows { .. } => {
                writes.push(pinned.type_ref());
                let children: BTreeSet<&str> = target.owned_child_types().into_iter().collect();
                writes.extend(
                    children
                        .into_iter()
                        .filter_map(|child| admission.named(child))
                        .map(|child| PinnedType::of(child).type_ref()),
                );
                WriteVia::MutateRows
            }
        };
        let declared: Vec<MemberRef> = writes
            .into_iter()
            .filter(|target| self.declares(target))
            .collect();
        for target in declared {
            self.writes.insert(WritesRow {
                writer: at(artifact, Member::Artifact {}),
                target,
                via,
            });
        }
    }

    fn action_facets(
        &mut self,
        artifact: &ArtifactEnvelope,
        action: &ActionRoot,
        admission: &Admission<'_>,
    ) {
        let mut concepts: BTreeSet<_> = action
            .writes
            .iter()
            .filter_map(|write| admission.named(&write.entity_kind))
            .map(|target| PinnedType::of(target).type_ref())
            .collect();
        if let Some(target) = action
            .effect_program
            .as_ref()
            .and_then(|program| admission.effect_target(program))
        {
            concepts.insert(PinnedType::of(target).type_ref());
        }
        for concept in concepts {
            self.facets.insert(ConceptFacetRow {
                concept: concept.clone(),
                facet: ConceptFacet::Actions,
                member: at(artifact, Member::Artifact {}),
            });
            if let Some(access) = &action.access {
                let version = access.version.as_deref().or_else(|| {
                    artifact
                        .dependencies
                        .iter()
                        .find(|d| d.artifact_id == access.artifact_id && d.required)
                        .and_then(|d| d.version.as_deref())
                });
                for policy in self.artifacts.iter().filter(|a| {
                    a.artifact_id == access.artifact_id
                        && Some(a.version.as_str()) == version
                        && a.kind == ArtifactKind::Policy
                }) {
                    for facet in [ConceptFacet::Policies, ConceptFacet::Access] {
                        self.facets.insert(ConceptFacetRow {
                            concept: concept.clone(),
                            facet,
                            member: MemberRef {
                                artifact_id: policy.artifact_id.clone(),
                                version: policy.version.clone(),
                                member: Member::Artifact {},
                            },
                        });
                    }
                }
            }
        }
    }

    fn resolve(&self, member: &Member, lineage: &[PinnedType]) -> Option<MemberRef> {
        let extension = |at: &MemberRef| {
            !self
                .types
                .iter()
                .any(|row| row.at.artifact_id == at.artifact_id && row.at.version == at.version)
        };
        lineage.iter().find_map(|owner| {
            let wanted = with_type(member, owner.name.clone());
            let admitted = |at: &&MemberRef| {
                at.member == wanted
                    && ((at.artifact_id == owner.artifact.artifact_id
                        && at.version == owner.artifact.version)
                        || extension(at))
            };
            let found = match &wanted {
                Member::Property { .. } => self.properties.iter().map(|row| &row.at).find(admitted),
                Member::Link { .. } => self.links.iter().map(|row| &row.at).find(admitted),
                Member::Transition { .. } => {
                    self.transitions.iter().map(|row| &row.at).find(admitted)
                }
                Member::Artifact {}
                | Member::Type { .. }
                | Member::State { .. }
                | Member::ObjectMap { .. }
                | Member::Law { .. } => None,
            };
            found.cloned()
        })
    }

    fn declares(&self, member: &MemberRef) -> bool {
        match &member.member {
            Member::Type { .. } => self.types.iter().any(|row| &row.at == member),
            Member::Property { .. } => self.properties.iter().any(|row| &row.at == member),
            Member::Link { .. } => self.links.iter().any(|row| &row.at == member),
            Member::Transition { .. } => self.transitions.iter().any(|row| &row.at == member),
            Member::Artifact {}
            | Member::State { .. }
            | Member::ObjectMap { .. }
            | Member::Law { .. } => false,
        }
    }

    fn fact_writes(
        &mut self,
        artifact: &ArtifactEnvelope,
        schema: &FactSchema<'_>,
        admission: &Admission<'_>,
    ) {
        let ops = schema.ops();
        let values = |name: &str| schema.values(name).unwrap_or_default();
        for type_name in values("objectType") {
            let Some(admitted) = admission.named(type_name) else {
                continue;
            };
            let lineage = PinnedType::lineage(admitted, admission);
            let owner = PinnedType::of(admitted);
            let mut candidates = Vec::new();
            if ops.contains(&FactChangeOp::Transition) {
                for lifecycle in values("lifecycle") {
                    for transition in values("transition") {
                        if let Some(declared) = self.resolve(
                            &Member::Transition {
                                type_name: type_name.to_owned(),
                                lifecycle: lifecycle.to_owned(),
                                transition: transition.to_owned(),
                            },
                            &lineage,
                        ) {
                            candidates.push((declared, WriteVia::FactTransition));
                        }
                    }
                }
            }
            if ops.contains(&FactChangeOp::Set) {
                for property in values("property") {
                    if let Some(declared) =
                        self.resolve(&property_member(type_name, property), &lineage)
                    {
                        candidates.push((declared, WriteVia::FactSet));
                    }
                }
            }
            if ops.iter().any(|op| {
                matches!(
                    op,
                    FactChangeOp::Create | FactChangeOp::Patch | FactChangeOp::Delete
                )
            }) {
                candidates.push((owner.type_ref(), WriteVia::FactObject));
            }
            for (target, via) in candidates {
                if self.declares(&target) {
                    self.writes.insert(WritesRow {
                        writer: at(artifact, Member::Artifact {}),
                        target,
                        via,
                    });
                }
            }
        }
    }

    fn mapping(&mut self, artifact: &ArtifactEnvelope, admission: &Admission<'_>) {
        let Ok(mapping) = serde_json::from_value::<MappingIr>(payload_json(artifact)) else {
            return;
        };
        for map in &mapping.maps {
            let version = artifact
                .dependencies
                .iter()
                .find(|dependency| {
                    dependency.required && dependency.artifact_id == map.target.entity
                })
                .and_then(|dependency| dependency.version.as_deref());
            let Some(admitted) = admission.declared_by(&map.target.entity, version) else {
                continue;
            };
            let lineage = PinnedType::lineage(admitted, admission);
            let owner = PinnedType::of(admitted);
            self.facets.insert(ConceptFacetRow {
                concept: owner.type_ref(),
                facet: ConceptFacet::Sources,
                member: at(
                    artifact,
                    Member::ObjectMap {
                        map: map.id.clone(),
                    },
                ),
            });
            let type_name = owner.name.as_str();
            let link = |id: &str| Member::Link {
                type_name: type_name.to_owned(),
                link: id.to_owned(),
            };
            let mut observed: Vec<(Member, MapRule)> = vec![(
                Member::Type {
                    type_name: type_name.to_owned(),
                },
                MapRule::Object {},
            )];
            for property in &map.properties {
                observed.push((
                    property_member(type_name, property.property.as_str()),
                    MapRule::Property {},
                ));
            }
            for link_map in &map.links {
                observed.push((link(link_map.link.as_str()), MapRule::Link {}));
            }
            for lifecycle in &map.lifecycles {
                observed.push((
                    property_member(type_name, lifecycle.property.as_str()),
                    MapRule::Lifecycle {},
                ));
            }
            for gap in &map.gaps {
                let member = if self.resolve(&link(&gap.member), &lineage).is_some() {
                    link(&gap.member)
                } else {
                    property_member(type_name, &gap.member)
                };
                observed.push((member, MapRule::Gap { reason: gap.reason }));
            }
            for (member, rule) in observed {
                let target = if matches!(member, Member::Type { .. }) {
                    Some(owner.type_ref())
                } else {
                    self.resolve(&member, &lineage)
                };
                let Some(target) = target else {
                    continue;
                };
                self.mappings.insert(MappingRow {
                    map: at(
                        artifact,
                        Member::ObjectMap {
                            map: map.id.clone(),
                        },
                    ),
                    dataset: map.source.dataset.clone(),
                    target,
                    rule,
                });
            }
        }
    }

    fn law(
        &mut self,
        artifact: &ArtifactEnvelope,
        all: &[&ArtifactEnvelope],
        admission: &Admission<'_>,
    ) {
        let Ok(law) = serde_json::from_value::<LawIR>(payload_json(artifact)) else {
            return;
        };
        let pin = |name: &str| {
            admission
                .named(name)
                .map(|admitted| PinnedType::of(admitted).type_ref())
        };
        let Some(subject) = pin(&law.subject) else {
            return;
        };
        let binds = match law.response {
            LawResponse::Predicate { .. } => None,
            LawResponse::Transition {
                object_type,
                property,
                transition_id,
            } => {
                let Some(admitted) = admission.named(&object_type) else {
                    return;
                };
                let Some(target) = self.resolve(
                    &Member::Transition {
                        type_name: object_type,
                        lifecycle: property,
                        transition: transition_id,
                    },
                    &PinnedType::lineage(admitted, admission),
                ) else {
                    return;
                };
                Some(LawBinding::Transition { target })
            }
            LawResponse::Invariant {
                object_type,
                invariant_id,
            } => {
                let Some(owner) = pin(&object_type) else {
                    return;
                };
                Some(LawBinding::Invariant {
                    owner,
                    invariant: invariant_id,
                })
            }
            LawResponse::Action { artifact_id } => {
                let Some(action) = artifact
                    .dependencies
                    .iter()
                    .filter(|dependency| {
                        dependency.required
                            && dependency.kind == ArtifactKind::Action
                            && dependency.artifact_id == artifact_id
                    })
                    .find_map(|dependency| {
                        let version = dependency.version.as_deref()?;
                        all.iter().find(|other| {
                            other.artifact_id == artifact_id && other.version == version
                        })
                    })
                else {
                    return;
                };
                Some(LawBinding::Action {
                    target: at(action, Member::Artifact {}),
                })
            }
        };
        self.laws.insert(LawRow {
            at: at(artifact, Member::Law { law: law.id }),
            subject,
            binds,
        });
    }
}

/// GRAPH-META-1: a catalog or catalog row is never executable definition input.
pub(crate) fn catalog_write(artifact: &ArtifactEnvelope) -> Option<crate::Diagnostic> {
    catalog_refusal(&payload_json(artifact), Some(&artifact.artifact_id))
}

pub(crate) fn catalog_write_json(value: &serde_json::Value) -> Option<crate::Diagnostic> {
    catalog_refusal(
        value.get("payload")?,
        value.get("artifactId").and_then(serde_json::Value::as_str),
    )
}

fn catalog_refusal(
    value: &serde_json::Value,
    artifact_id: Option<&str>,
) -> Option<crate::Diagnostic> {
    let projection = value.get("format").and_then(serde_json::Value::as_str)
        == Some("norn.catalog/1")
        || (value.get("at").is_some()
            && (serde_json::from_value::<TransitionRow>(value.clone()).is_ok()
                || serde_json::from_value::<LawRow>(value.clone()).is_ok()));
    if !projection {
        return None;
    }
    let mut finding = crate::property_types::error(
        "core.graph.catalog_write",
        "catalog projections cannot activate definitions".to_owned(),
        crate::Span::pointer("/payload"),
    );
    finding.artifact_id = artifact_id.map(str::to_owned);
    Some(finding)
}
