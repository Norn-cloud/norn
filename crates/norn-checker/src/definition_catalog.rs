//! Projection of the definition catalog ([`norn_ir::definition_catalog`],
//! norn#62) from a resolved artifact set.
//!
//! [`project`] reads the typed roots the checker already decodes (`EntityIR`
//! v2, `ActionIR`, decide graphs' fact-change schemas, `MappingIR`, `LawIR`,
//! and composition packs) and emits one row per declared member. It adds no
//! semantics: a payload that does not decode contributes no rows, because
//! its T2 finding already refuses the set. Run it over a set the checker
//! admits ([`crate::check_artifact_set`] with no error findings); rules over
//! the catalog are checker code, never pack data.
//!
//! What is not projected yet, so rules over the catalog cannot rely on it:
//! inherited members (a row belongs to the artifact that declares it), the
//! states of a lifecycle whose property the declaring type inherits, and
//! rule writes other than decide-graph fact changes.

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::definition_catalog::{
    ArtifactKey, CatalogArtifact, CatalogFormat, DefinitionCatalog, DependencyVia, DependsOnRow,
    LawBinding, LawRow, LifecycleStates, LinkRow, MapRule, MappingRow, Member, MemberRef,
    PropertyRow, TransitionRow, TypeKind, TypeRow, WriteVia, WritesRow,
};
use norn_ir::mapping::MappingIr;
use norn_ir::{
    ActionEffectProgram, ActionRoot, ArtifactEnvelope, ArtifactPayload, BusinessPackRoot,
    EntityRoot, FactChangeOp, LawIR, LawResponse, Lifecycle, ObjectTypeDef, Property, PropertyType,
};

use crate::artifact_set::ResolvedArtifactSet;
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

/// Project the catalog of `set`: its candidates and every installed `base`
/// artifact they reach through references (envelope dependencies, pack
/// members, and action effect pins), transitively. See the module docs.
///
/// # Errors
///
/// [`CatalogError::Hash`] if an artifact's semantic hash cannot be computed.
pub fn project(
    set: &ResolvedArtifactSet<'_>,
    base: &[ArtifactEnvelope],
) -> Result<DefinitionCatalog, CatalogError> {
    let all: Vec<(&ArtifactEnvelope, bool)> = set
        .artifacts
        .iter()
        .map(|artifact| (*artifact, true))
        .chain(
            base_closure(set, base)
                .into_iter()
                .map(|artifact| (artifact, false)),
        )
        .collect();
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
    let types = rows.type_index();
    for artifact in &envelopes {
        rows.depends_on(artifact, &envelopes);
        match &artifact.payload {
            ArtifactPayload::Action(_) => rows.action(artifact, &types),
            ArtifactPayload::Mapping(_) => rows.mapping(artifact, &types),
            ArtifactPayload::Law(_) => rows.law(artifact, &types, &envelopes),
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
            rows.fact_writes(artifact, &schema, &types);
        }
    }
    Ok(rows.finish())
}

/// The exact `(artifactId, version)` references of `artifact`: resolved
/// envelope dependencies, composition pack members, and its action effect's
/// pinned entity.
fn references(artifact: &ArtifactEnvelope) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = artifact
        .dependencies
        .iter()
        .filter_map(|dependency| {
            Some((dependency.artifact_id.clone(), dependency.version.clone()?))
        })
        .collect();
    match &artifact.payload {
        ArtifactPayload::BusinessPack(_) => {
            if let Ok(BusinessPackRoot::Composition(pack)) =
                serde_json::from_value(payload_json(artifact))
            {
                out.extend(
                    pack.artifacts
                        .iter()
                        .map(|member| (member.artifact_id.clone(), member.version.clone())),
                );
            }
        }
        ArtifactPayload::Action(_) => {
            if let Ok(action) = serde_json::from_value::<ActionRoot>(payload_json(artifact))
                && let Some(program) = &action.effect_program
            {
                let target = program.target();
                out.push((
                    target.entity_artifact_id.to_owned(),
                    target.entity_artifact_version.to_owned(),
                ));
            }
        }
        ArtifactPayload::Entity(_)
        | ArtifactPayload::Relation(_)
        | ArtifactPayload::Rule(_)
        | ArtifactPayload::Connector(_)
        | ArtifactPayload::Widget(_)
        | ArtifactPayload::Customization(_)
        | ArtifactPayload::Setting(_)
        | ArtifactPayload::Policy(_)
        | ArtifactPayload::Derived(_)
        | ArtifactPayload::SourceSchema(_)
        | ArtifactPayload::Mapping(_)
        | ArtifactPayload::Law(_) => {}
    }
    out
}

/// The base artifacts the candidates reach, transitively, starting from
/// the ones they reference directly.
fn base_closure<'a>(
    set: &ResolvedArtifactSet<'a>,
    base: &'a [ArtifactEnvelope],
) -> Vec<&'a ArtifactEnvelope> {
    let candidates: BTreeSet<(&str, &str)> = set
        .artifacts
        .iter()
        .map(|artifact| (artifact.artifact_id.as_str(), artifact.version.as_str()))
        .collect();
    let mut included: Vec<&'a ArtifactEnvelope> = Vec::new();
    let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
    let mut frontier: Vec<&'a ArtifactEnvelope> = set
        .artifacts
        .iter()
        .chain(&set.base_dependencies)
        .copied()
        .collect();
    while let Some(artifact) = frontier.pop() {
        let mut next = references(artifact);
        if !candidates.contains(&(artifact.artifact_id.as_str(), artifact.version.as_str())) {
            next.push((artifact.artifact_id.clone(), artifact.version.clone()));
        }
        for (id, version) in next {
            if candidates.contains(&(id.as_str(), version.as_str()))
                || seen.contains(&(id.clone(), version.clone()))
            {
                continue;
            }
            if let Some(found) = base
                .iter()
                .find(|other| other.artifact_id == id && other.version == version)
            {
                seen.insert((id, version));
                included.push(found);
                frontier.push(found);
            }
        }
    }
    included.sort_by(|left, right| {
        (&left.artifact_id, &left.version).cmp(&(&right.artifact_id, &right.version))
    });
    included
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
    /// Object type name → the child types of its owned collections.
    children: BTreeMap<String, Vec<String>>,
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
        self.children.insert(
            name.clone(),
            def.owned_collections
                .iter()
                .map(|collection| collection.child_type.as_str().to_owned())
                .collect(),
        );
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

    /// Object type name → the artifact declaring it, over every type row.
    fn type_index(&self) -> BTreeMap<String, ArtifactKey> {
        self.types
            .iter()
            .filter_map(|row| {
                let Member::Type { type_name } = &row.at.member else {
                    return None;
                };
                Some((
                    type_name.clone(),
                    ArtifactKey {
                        artifact_id: row.at.artifact_id.clone(),
                        version: row.at.version.clone(),
                    },
                ))
            })
            .collect()
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

    fn action(&mut self, artifact: &ArtifactEnvelope, types: &BTreeMap<String, ArtifactKey>) {
        let Ok(action) = serde_json::from_value::<ActionRoot>(payload_json(artifact)) else {
            return;
        };
        let Some(program) = &action.effect_program else {
            return;
        };
        let target = program.target();
        let type_name = target.object_kind.to_owned();
        let mut targets = Vec::new();
        let via = match program {
            ActionEffectProgram::FireTransition {
                lifecycle,
                transition,
                ..
            } => {
                targets.push(Member::Transition {
                    type_name,
                    lifecycle: lifecycle.as_str().to_owned(),
                    transition: transition.as_str().to_owned(),
                });
                WriteVia::FireTransition
            }
            ActionEffectProgram::UpsertInputObject { .. } => {
                targets.push(Member::Type { type_name });
                WriteVia::UpsertObject
            }
            ActionEffectProgram::MutateParentWithRows { .. } => {
                targets.push(Member::Type { type_name });
                WriteVia::MutateRows
            }
        };
        let pinned = ArtifactKey {
            artifact_id: target.entity_artifact_id.to_owned(),
            version: target.entity_artifact_version.to_owned(),
        };
        let mut writes: Vec<MemberRef> = targets
            .into_iter()
            .map(|member| member_of(&pinned, member))
            .collect();
        // The rows a parent mutation creates, changes, and deletes are
        // objects of its owned collections' child types.
        if via == WriteVia::MutateRows {
            for child in self.children.get(target.object_kind).into_iter().flatten() {
                if let Some(owner) = types.get(child) {
                    writes.push(member_of(
                        owner,
                        Member::Type {
                            type_name: child.clone(),
                        },
                    ));
                }
            }
        }
        for target in writes {
            self.writes.insert(WritesRow {
                writer: at(artifact, Member::Artifact {}),
                target,
                via,
            });
        }
    }

    /// The row declaring a property or link of a type, in whichever artifact
    /// declares it (the type's, or an extension's); otherwise `member` of
    /// `owner`, which no row declares.
    fn pin(&self, owner: &ArtifactKey, member: Member) -> MemberRef {
        let declared = match &member {
            Member::Property { .. } => self
                .properties
                .iter()
                .map(|row| &row.at)
                .find(|at| at.member == member),
            Member::Link { .. } => self
                .links
                .iter()
                .map(|row| &row.at)
                .find(|at| at.member == member),
            Member::Artifact {}
            | Member::Type { .. }
            | Member::State { .. }
            | Member::Transition { .. }
            | Member::ObjectMap { .. }
            | Member::Law { .. } => None,
        };
        declared
            .cloned()
            .unwrap_or_else(|| member_of(owner, member))
    }

    /// Every write a decide graph's closed fact-change schema admits.
    fn fact_writes(
        &mut self,
        artifact: &ArtifactEnvelope,
        schema: &FactSchema<'_>,
        types: &BTreeMap<String, ArtifactKey>,
    ) {
        let ops = schema.ops();
        let values = |name: &str| schema.values(name).unwrap_or_default();
        for type_name in values("objectType") {
            let Some(owner) = types.get(type_name) else {
                continue;
            };
            let target = |member: Member| MemberRef {
                artifact_id: owner.artifact_id.clone(),
                version: owner.version.clone(),
                member,
            };
            let mut candidates = Vec::new();
            if ops.contains(&FactChangeOp::Transition) {
                for lifecycle in values("lifecycle") {
                    for transition in values("transition") {
                        candidates.push((
                            target(Member::Transition {
                                type_name: type_name.to_owned(),
                                lifecycle: lifecycle.to_owned(),
                                transition: transition.to_owned(),
                            }),
                            WriteVia::FactTransition,
                        ));
                    }
                }
            }
            if ops.contains(&FactChangeOp::Set) {
                for property in values("property") {
                    candidates.push((
                        self.pin(owner, property_member(type_name, property)),
                        WriteVia::FactSet,
                    ));
                }
            }
            if ops.iter().any(|op| {
                matches!(
                    op,
                    FactChangeOp::Create | FactChangeOp::Patch | FactChangeOp::Delete
                )
            }) {
                candidates.push((
                    target(Member::Type {
                        type_name: type_name.to_owned(),
                    }),
                    WriteVia::FactObject,
                ));
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

    fn mapping(&mut self, artifact: &ArtifactEnvelope, types: &BTreeMap<String, ArtifactKey>) {
        let Ok(mapping) = serde_json::from_value::<MappingIr>(payload_json(artifact)) else {
            return;
        };
        // Map targets name entity artifacts by id; a resolved set holds one
        // version of each.
        let by_artifact: BTreeMap<&str, (&str, &ArtifactKey)> = types
            .iter()
            .map(|(name, key)| (key.artifact_id.as_str(), (name.as_str(), key)))
            .collect();
        for map in &mapping.maps {
            let Some((type_name, owner)) = by_artifact.get(map.target.entity.as_str()) else {
                continue;
            };
            let link = |id: &str| Member::Link {
                type_name: (*type_name).to_owned(),
                link: id.to_owned(),
            };
            let mut observed: Vec<(Member, MapRule)> = vec![(
                Member::Type {
                    type_name: (*type_name).to_owned(),
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
                let member = if self.declares(&self.pin(owner, link(&gap.member))) {
                    link(&gap.member)
                } else {
                    property_member(type_name, &gap.member)
                };
                observed.push((member, MapRule::Gap { reason: gap.reason }));
            }
            for (member, rule) in observed {
                self.mappings.insert(MappingRow {
                    map: at(
                        artifact,
                        Member::ObjectMap {
                            map: map.id.clone(),
                        },
                    ),
                    dataset: map.source.dataset.clone(),
                    target: self.pin(owner, member),
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
        types: &BTreeMap<String, ArtifactKey>,
        all: &[&ArtifactEnvelope],
    ) {
        let Ok(law) = serde_json::from_value::<LawIR>(payload_json(artifact)) else {
            return;
        };
        let type_of = |name: &str| {
            types.get(name).map(|owner| {
                member_of(
                    owner,
                    Member::Type {
                        type_name: name.to_owned(),
                    },
                )
            })
        };
        let Some(subject) = type_of(&law.subject) else {
            return;
        };
        let binds = match law.response {
            LawResponse::Predicate { .. } => None,
            LawResponse::Transition {
                object_type,
                property,
                transition_id,
            } => {
                let Some(owner) = types.get(&object_type) else {
                    return;
                };
                Some(LawBinding::Transition {
                    target: member_of(
                        owner,
                        Member::Transition {
                            type_name: object_type,
                            lifecycle: property,
                            transition: transition_id,
                        },
                    ),
                })
            }
            LawResponse::Invariant {
                object_type,
                invariant_id,
            } => {
                let Some(owner) = type_of(&object_type) else {
                    return;
                };
                Some(LawBinding::Invariant {
                    owner,
                    invariant: invariant_id,
                })
            }
            LawResponse::Action { artifact_id } => {
                let Some(action) = all.iter().find(|other| other.artifact_id == artifact_id) else {
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
