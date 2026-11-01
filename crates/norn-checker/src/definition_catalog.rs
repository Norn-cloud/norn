//! Read-only catalog projection from admitted definitions.
use crate::action_set::{Admission, AdmittedType};
use crate::entity_artifacts::{Decoded, decode_v2};
use crate::entity_lowering::{EntityForm, entity_object_type};
use crate::receipt::{ArtifactRef, ReceiptError};
use norn_ir::definition_catalog::{
    ArtifactKey, CatalogArtifact, CatalogFormat, DefinitionCatalog, DependencyVia, DependsOnRow,
    LawRow, LifecycleStates, LinkRow, MappingRow, Member, MemberRef, PropertyRow, TransitionRow,
    TypeKind, TypeRow, WriteVia, WritesRow,
};
use norn_ir::{
    ActionEffectProgram, ActionRoot, ArtifactEnvelope, ArtifactPayload, BusinessPackRoot,
    EntityRoot, Lifecycle, ObjectTypeDef, Property, PropertyType, PropertyTypeName,
};
use std::collections::BTreeSet;
/// Why a catalog could not be projected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogError {
    /// An artifact's semantic hash could not be computed.
    Hash(ReceiptError),
}

impl std::fmt::Display for CatalogError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Hash(error) => write!(formatter, "artifact hash: {error}"),
        }
    }
}

impl std::error::Error for CatalogError {}

/// Project admitted candidates and installed artifacts into sorted catalog rows.
///
/// Call only after `check_artifact_set` admits the set. This slice projects
/// entities and dependency edges, action writes. See verification/definition-catalog-v1/SPEC.md.
///
/// # Errors
/// Returns `CatalogError::Hash` if an artifact semantic hash cannot be computed.
pub fn project(
    artifacts: &[ArtifactEnvelope],
    base: &[ArtifactEnvelope],
    registry: &BTreeSet<PropertyTypeName>,
) -> Result<DefinitionCatalog, CatalogError> {
    let all: Vec<&ArtifactEnvelope> = artifacts.iter().chain(base).collect();
    let mut rows = Rows::default();
    for (artifact, candidate) in artifacts
        .iter()
        .map(|a| (a, true))
        .chain(base.iter().map(|a| (a, false)))
    {
        let reference = ArtifactRef::of(artifact).map_err(CatalogError::Hash)?;
        rows.artifacts.insert(CatalogArtifact {
            artifact_id: reference.artifact_id,
            version: reference.version,
            kind: artifact.kind(),
            sha256: reference.sha256,
            candidate,
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
    artifacts: BTreeSet<CatalogArtifact>,
    types: BTreeSet<TypeRow>,
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

fn lifecycle_states(property: &Property, lifecycles: &[Lifecycle]) -> Option<LifecycleStates> {
    let lifecycle = lifecycles
        .iter()
        .find(|lifecycle| lifecycle.property == property.id)?;
    let PropertyType::Enum { values } = &property.property_type else {
        return None;
    };
    Some(LifecycleStates {
        states: values.iter().map(|value| value.value.clone()).collect(),
        initial: lifecycle.initial.clone(),
        terminal: lifecycle.terminal.clone(),
    })
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
                let name = root.interface.name.as_str().to_owned();
                self.types.insert(TypeRow {
                    at: at(
                        artifact,
                        Member::Type {
                            type_name: name.clone(),
                        },
                    ),
                    kind: TypeKind::Interface,
                    extends: root
                        .interface
                        .extends
                        .iter()
                        .map(|n| n.as_str().to_owned())
                        .collect(),
                    implements: Vec::new(),
                });
                for property in &root.interface.properties {
                    self.properties.insert(PropertyRow {
                        at: at(artifact, property_member(&name, property.id.as_str())),
                        computed: false,
                        lifecycle: None,
                    });
                }
                for link in &root.interface.links {
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
            at: at(
                artifact,
                Member::Type {
                    type_name: name.clone(),
                },
            ),
            kind: TypeKind::ObjectType,
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
                .and_then(|p| lifecycle_states(p, &def.lifecycles))
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
            lifecycle: lifecycle_states(property, lifecycles),
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

    fn finish(self) -> DefinitionCatalog {
        DefinitionCatalog {
            format: CatalogFormat::V1,
            artifacts: self.artifacts.into_iter().collect(),
            types: self.types.into_iter().collect(),
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

    fn finish(self) -> DefinitionCatalog {
        DefinitionCatalog {
            format: CatalogFormat::V1,
            artifacts: self.artifacts.into_iter().collect(),
            types: self.types.into_iter().collect(),
            properties: self.properties.into_iter().collect(),
            links: self.links.into_iter().collect(),
            transitions: self.transitions.into_iter().collect(),
            writes: self.writes.into_iter().collect(),
            depends_on: self.depends_on.into_iter().collect(),
            mappings: self.mappings.into_iter().collect(),
            laws: self.laws.into_iter().collect(),
        }
    }

    fn action(&mut self, artifact: &ArtifactEnvelope, admission: &Admission<'_>) {
        let Ok(action) = serde_json::from_value::<ActionRoot>(payload_json(artifact)) else {
            return;
        };
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
}
