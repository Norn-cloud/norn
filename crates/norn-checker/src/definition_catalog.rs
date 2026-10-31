//! Projection of the definition catalog ([`norn_ir::definition_catalog`],
//! norn#62) from an admitted artifact set.
//!
//! [`project`] reads the typed roots the checker already decodes (`EntityIR`
//! v2, `ActionIR`, decide graphs' fact-change schemas, `MappingIR`, `LawIR`,
//! and composition packs) and emits one row per declared member. It adds no
//! semantics: a payload that does not decode contributes no rows, because
//! its T2 finding already refuses the set. Run it over a set the checker
//! admits ([`crate::check_artifact_set`] with no error findings); rules over
//! the catalog are checker code, never pack data.
//!
//! Which definition a write, mapping target or law binds is never derived
//! here: it is the checker's own admission binding
//! ([`crate::action_set::Admission`], the model the action and
//! lifecycle-write checks resolve with). An action binds the definition its
//! effect pins, a name (a decide graph's `objectType`, a supertype, an
//! owned-collection child type, a law subject) binds what admission binds
//! it to, and inherited members and owned collections are the effective
//! ones. The catalog projects every candidate and every installed artifact.
//!
//! What is not projected yet, so rules over the catalog cannot rely on it:
//! the states of a lifecycle whose property the declaring type inherits,
//! and rule writes other than decide-graph fact changes.

use std::collections::BTreeSet;

use norn_ir::definition_catalog::{
    ArtifactKey, CatalogArtifact, CatalogFormat, DefinitionCatalog, DependencyVia, DependsOnRow,
    LawBinding, LawRow, LifecycleStates, LinkRow, MapRule, MappingRow, Member, MemberRef,
    PropertyRow, TransitionRow, TypeKind, TypeRow, WriteVia, WritesRow,
};
use norn_ir::mapping::MappingIr;
use norn_ir::{
    ActionEffectProgram, ActionRoot, ArtifactEnvelope, ArtifactKind, ArtifactPayload,
    BusinessPackRoot, EntityRoot, FactChangeOp, LawIR, LawResponse, Lifecycle, ObjectTypeDef,
    Property, PropertyType, PropertyTypeName,
};

use crate::action_set::{Admission, AdmittedType};
use crate::entity_artifacts::{Decoded, decode_v2};
use crate::lifecycle_writes::{FactSchema, fact_decisions};
use crate::receipt::{ArtifactRef, ReceiptError};

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

/// Project the catalog of the candidate `artifacts` and the installed
/// `base` they are checked against (with the `registry` value types the
/// entity model resolves). See the module docs.
///
/// # Errors
///
/// [`CatalogError::Hash`] if an artifact's semantic hash cannot be computed.
pub fn project(
    artifacts: &[ArtifactEnvelope],
    base: &[ArtifactEnvelope],
    registry: &BTreeSet<PropertyTypeName>,
) -> Result<DefinitionCatalog, CatalogError> {
    let all: Vec<(&ArtifactEnvelope, bool)> = artifacts
        .iter()
        .map(|artifact| (artifact, true))
        .chain(base.iter().map(|artifact| (artifact, false)))
        .collect();
    let admission = Admission::new(artifacts, base, registry);
    let mut rows = Rows::default();
    for (artifact, candidate) in &all {
        let reference = ArtifactRef::of(artifact).map_err(CatalogError::Hash)?;
        rows.artifacts.insert(CatalogArtifact {
            artifact_id: reference.artifact_id,
            version: reference.version,
            kind: artifact.kind(),
            sha256: reference.sha256,
            candidate: *candidate,
        });
    }
    let envelopes: Vec<&ArtifactEnvelope> = all.iter().map(|(artifact, _)| *artifact).collect();
    for artifact in &envelopes {
        rows.entity(artifact);
    }
    for artifact in &envelopes {
        rows.depends_on(artifact, &envelopes);
        match &artifact.payload {
            ArtifactPayload::Action(_) => rows.action(artifact, &admission),
            ArtifactPayload::Mapping(_) => rows.mapping(artifact, &admission),
            ArtifactPayload::Law(_) => rows.law(artifact, &envelopes, &admission),
            // Entities were projected above; decide graphs below. The other
            // kinds declare no catalog member yet.
            ArtifactPayload::Entity(_)
            | ArtifactPayload::Relation(_)
            | ArtifactPayload::Rule(_)
            | ArtifactPayload::Connector(_)
            | ArtifactPayload::Widget(_)
            | ArtifactPayload::Customization(_)
            | ArtifactPayload::Setting(_)
            | ArtifactPayload::BusinessPack(_)
            | ArtifactPayload::Policy(_)
            | ArtifactPayload::Derived(_)
            | ArtifactPayload::SourceSchema(_) => {}
        }
    }
    for (artifact, output) in fact_decisions(envelopes.iter().copied()) {
        if let Some(schema) = FactSchema::of(&output) {
            rows.fact_writes(artifact, &schema, &admission);
        }
    }
    Ok(rows.finish())
}

/// One type as one exact artifact declares it: the key every per-type
/// lookup takes. It is only ever built from an admitted definition.
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

    /// `admitted`, then each supertype admission binds, nearest first.
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

    /// `member` of the declaring artifact.
    fn member(&self, member: Member) -> MemberRef {
        member_of(&self.artifact, member)
    }

    /// The type's own row.
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

/// The payload as `serde_json`, to decode its strict typed root.
fn payload_json(artifact: &ArtifactEnvelope) -> serde_json::Value {
    serde_json::to_value(artifact.payload.to_json_value()).unwrap_or_default()
}

/// The states of `property` when `lifecycles` declares its machine.
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

impl Rows {
    fn entity(&mut self, artifact: &ArtifactEnvelope) {
        let Decoded::Root(root) = decode_v2(artifact) else {
            return;
        };
        match *root {
            EntityRoot::V2(root) => self.object_type(artifact, &root.object_type),
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

    fn object_type(&mut self, artifact: &ArtifactEnvelope, def: &ObjectTypeDef) {
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
        let Some(program) = &action.effect_program else {
            return;
        };
        // The definition admission binds the effect to; none when the
        // checker refuses the pin.
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
                // A transition may be inherited: it binds the declaring row.
                writes.extend(self.resolve(
                    &Member::Transition {
                        type_name: pinned.name.clone(),
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
                // The rows it creates, changes, and deletes are objects of the
                // child types of its effective owned collections, inherited
                // ones included.
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
        for target in writes.into_iter().filter(|target| self.declares(target)) {
            self.writes.insert(WritesRow {
                writer: at(artifact, Member::Artifact {}),
                target,
                via,
            });
        }
    }

    /// The row declaring a property, link, or transition along an admitted
    /// `lineage` (the type, then its supertypes): on the nearest type that
    /// declares it. A row binds when that type's own artifact declares it,
    /// or an extension (an artifact that declares no type). `None` when no
    /// row does.
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

    /// Every write a decide graph's closed fact-change schema admits.
    fn fact_writes(
        &mut self,
        artifact: &ArtifactEnvelope,
        schema: &FactSchema<'_>,
        admission: &Admission<'_>,
    ) {
        let ops = schema.ops();
        let values = |name: &str| schema.values(name).unwrap_or_default();
        for type_name in values("objectType") {
            // The definition admission binds the `objectType` value to, as
            // the lifecycle-write check resolves it.
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
                // Only members the target declares: the closed schema
                // admits every combination, not every one is a write.
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

    /// Whether a type, property, link, or transition row declares `member`.
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

    fn mapping(&mut self, artifact: &ArtifactEnvelope, admission: &Admission<'_>) {
        let Ok(mapping) = serde_json::from_value::<MappingIr>(payload_json(artifact)) else {
            return;
        };
        for map in &mapping.maps {
            let Some(admitted) = admission.declared_by(&map.target.entity) else {
                continue;
            };
            let lineage = PinnedType::lineage(admitted, admission);
            let owner = PinnedType::of(admitted);
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
                // The object type row is the map's own target; members bind
                // their declaring row, and an undeclared one has no row.
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

    /// A law whose subject or binding the set does not define has no row:
    /// the law check (`law.model.unknown_binding`) already refuses it.
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
                // An inherited transition binds the row of the supertype
                // that declares it.
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
                // The exact version the law's required dependency pins.
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
}

/// `member` with its type replaced by `type_name`.
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

fn property_member(type_name: &str, property: &str) -> Member {
    Member::Property {
        type_name: type_name.to_owned(),
        property: property.to_owned(),
    }
}
