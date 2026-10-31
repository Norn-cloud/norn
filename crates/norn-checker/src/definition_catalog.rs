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
    ActionEffectProgram, ActionRoot, ArtifactEnvelope, ArtifactKind, ArtifactPayload,
    BusinessPackRoot, EntityRoot, FactChangeOp, LawIR, LawResponse, Lifecycle, ObjectTypeDef,
    Property, PropertyType,
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
    let mut rows = Rows {
        bound: bound_types(set, base),
        ..Rows::default()
    };
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

/// The type an entity artifact declares (an object type or interface), and
/// the type names it refers to: supertypes, interfaces, an extension's
/// target.
fn type_names(artifact: &ArtifactEnvelope) -> (Option<String>, Vec<String>) {
    let Decoded::Root(root) = decode_v2(artifact) else {
        return (None, Vec::new());
    };
    match *root {
        EntityRoot::V2(root) => {
            let def = root.object_type;
            let mut refs: Vec<String> = def.extends.iter().map(|n| n.as_str().to_owned()).collect();
            refs.extend(def.implements.iter().map(|n| n.as_str().to_owned()));
            refs.extend(
                def.owned_collections
                    .iter()
                    .map(|collection| collection.child_type.as_str().to_owned()),
            );
            (Some(def.name.as_str().to_owned()), refs)
        }
        EntityRoot::V2Interface(root) => (
            Some(root.interface.name.as_str().to_owned()),
            root.interface
                .extends
                .iter()
                .map(|n| n.as_str().to_owned())
                .collect(),
        ),
        EntityRoot::V2Extension(root) => (
            None,
            vec![root.object_type_extension.target.as_str().to_owned()],
        ),
        EntityRoot::V2PropertyTypes(_) | EntityRoot::V2Markings(_) | EntityRoot::V1(_) => {
            (None, Vec::new())
        }
    }
}

/// The one artifact each type name binds to, as the entity model binds it:
/// a candidate's own declaration, else the first installed artifact (in
/// base order) that declares it.
fn bound_types(
    set: &ResolvedArtifactSet<'_>,
    base: &[ArtifactEnvelope],
) -> BTreeMap<String, ArtifactKey> {
    let mut bound = BTreeMap::new();
    for artifact in set.artifacts.iter().copied().chain(base) {
        if let Some(name) = type_names(artifact).0 {
            bound.entry(name).or_insert_with(|| ArtifactKey {
                artifact_id: artifact.artifact_id.clone(),
                version: artifact.version.clone(),
            });
        }
    }
    bound
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
    // A subtype, implementor, or extension reaches the types it names even
    // without an envelope dependency on them: the one artifact each name is
    // bound to.
    let bound = bound_types(set, base);
    while let Some(artifact) = frontier.pop() {
        let mut next = references(artifact);
        for name in type_names(artifact).1 {
            next.extend(
                bound
                    .get(&name)
                    .map(|key| (key.artifact_id.clone(), key.version.clone())),
            );
        }
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
    /// Type name → the artifact the name binds to ([`bound_types`]).
    bound: BTreeMap<String, ArtifactKey>,
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

    /// Type name → the artifact it binds to, for names with a projected type
    /// row.
    fn type_index(&self) -> BTreeMap<String, ArtifactKey> {
        // Only names whose bound artifact has a projected type row.
        self.bound
            .iter()
            .filter(|(name, key)| {
                let wanted = Member::Type {
                    type_name: (*name).clone(),
                };
                self.types.iter().any(|row| {
                    row.at.artifact_id == key.artifact_id
                        && row.at.version == key.version
                        && row.at.member == wanted
                })
            })
            .map(|(name, key)| (name.clone(), key.clone()))
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
        // A transition may be inherited: it binds the declaring row. Writes
        // to members the catalog does not hold are not projected.
        let mut writes: Vec<MemberRef> = targets
            .into_iter()
            .filter_map(|member| {
                if matches!(member, Member::Transition { .. }) {
                    self.resolve(&member, Some(&pinned))
                } else {
                    Some(member_of(&pinned, member)).filter(|target| self.declares(target))
                }
            })
            .collect();
        // The rows a parent mutation creates, changes, and deletes are
        // objects of its owned collections' child types, declared on the
        // type or inherited from a supertype.
        if via == WriteVia::MutateRows {
            let children: BTreeSet<&String> = self
                .lineage(target.object_kind, Some(&pinned))
                .iter()
                .filter_map(|name| self.children.get(name))
                .flatten()
                .collect();
            for child in children {
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

    /// `type_name`, then its supertypes, nearest first. A cycle (refused by
    /// the entity check) ends after at most one pass over the types.
    fn lineage(&self, type_name: &str, owner: Option<&ArtifactKey>) -> Vec<String> {
        let mut out = vec![type_name.to_owned()];
        while out.len() <= self.types.len() {
            let current = Member::Type {
                type_name: out.last().cloned().unwrap_or_default(),
            };
            // The type itself is the version the reference pins; each
            // supertype, the artifact its name is bound to.
            let bound = if out.len() == 1 {
                owner.or_else(|| self.bound.get(type_name))
            } else {
                self.bound.get(&*out.last().cloned().unwrap_or_default())
            };
            let Some(parent) = self
                .types
                .iter()
                .find(|row| {
                    row.at.member == current
                        && row.kind == TypeKind::ObjectType
                        && bound.is_none_or(|key| {
                            row.at.artifact_id == key.artifact_id && row.at.version == key.version
                        })
                })
                .and_then(|row| row.extends.first())
            else {
                break;
            };
            out.push(parent.clone());
        }
        out
    }

    /// The row declaring a property, link, or transition of a type: on the
    /// type or its nearest supertype that declares it, in whichever artifact
    /// declares it (the type's or an extension's). `None` when no row does.
    /// On the type itself, `owner` (the exact artifact a reference pins)
    /// excludes rows of another version of the type's own artifact.
    fn resolve(&self, member: &Member, owner: Option<&ArtifactKey>) -> Option<MemberRef> {
        let (Member::Property { type_name, .. }
        | Member::Link { type_name, .. }
        | Member::Transition { type_name, .. }) = member
        else {
            return None;
        };
        let declares_type = |at: &MemberRef| {
            self.types
                .iter()
                .any(|row| row.at.artifact_id == at.artifact_id && row.at.version == at.version)
        };
        // The type itself binds the version the reference pins; each
        // supertype, the artifact its name is bound to. Extensions (artifacts
        // that declare no type) bind their own rows.
        let admitted = |at: &MemberRef, step: usize, name: &str| {
            let pinned = if step == 0 {
                owner
            } else {
                self.bound.get(name)
            };
            pinned.is_none_or(|key| {
                (at.artifact_id == key.artifact_id && at.version == key.version)
                    || !declares_type(at)
            })
        };
        self.lineage(type_name, owner)
            .into_iter()
            .enumerate()
            .find_map(|(step, name)| {
                let wanted = with_type(member, name.clone());
                let found = match &wanted {
                    Member::Property { .. } => self
                        .properties
                        .iter()
                        .map(|row| &row.at)
                        .find(|at| at.member == wanted && admitted(at, step, &name)),
                    Member::Link { .. } => self
                        .links
                        .iter()
                        .map(|row| &row.at)
                        .find(|at| at.member == wanted && admitted(at, step, &name)),
                    Member::Transition { .. } => self
                        .transitions
                        .iter()
                        .map(|row| &row.at)
                        .find(|at| at.member == wanted && admitted(at, step, &name)),
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
                        if let Some(declared) = self.resolve(
                            &Member::Transition {
                                type_name: type_name.to_owned(),
                                lifecycle: lifecycle.to_owned(),
                                transition: transition.to_owned(),
                            },
                            Some(owner),
                        ) {
                            candidates.push((declared, WriteVia::FactTransition));
                        }
                    }
                }
            }
            if ops.contains(&FactChangeOp::Set) {
                for property in values("property") {
                    if let Some(declared) =
                        self.resolve(&property_member(type_name, property), Some(owner))
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
                let member = if self.resolve(&link(&gap.member), Some(owner)).is_some() {
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
                    Some(member_of(owner, member))
                } else {
                    self.resolve(&member, Some(owner))
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
                // An inherited transition binds the row of the supertype
                // that declares it.
                let owner = types.get(&object_type);
                let Some(target) = self.resolve(
                    &Member::Transition {
                        type_name: object_type.clone(),
                        lifecycle: property,
                        transition: transition_id,
                    },
                    owner,
                ) else {
                    return;
                };
                Some(LawBinding::Transition { target })
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
