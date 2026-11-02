//! Read-only catalog projection from admitted definitions.
use crate::entity_artifacts::{Decoded, Layer, decode_v2};
use crate::entity_lowering::{EntityForm, entity_object_type};
use crate::receipt::ArtifactRef;
/// Failure to compute a projected artifact's semantic identity.
pub use crate::receipt::ReceiptError as CatalogError;
use norn_ir::definition_catalog::{
    CatalogArtifact, CatalogFormat, DefinitionCatalog, LifecycleStates, LinkRow, Member, MemberRef,
    PropertyRow, TransitionRow, TypeKind, TypeRow,
};
use norn_ir::{
    ArtifactEnvelope, ArtifactPayload, EntityRoot, Lifecycle, ObjectTypeDef, Property,
    PropertyType, PropertyTypeDef, PropertyTypeName,
};
use std::collections::BTreeSet;
/// Project admitted candidates and installed artifacts into sorted catalog rows.
///
/// Call only after `check_artifact_set` admits the set. This slice projects
/// entities. Dependency edges follow in the action/binding slice.
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
    }

    Ok(rows.finish())
}
#[derive(Default)]
struct Rows {
    property_types: Vec<PropertyTypeDef>,
    artifacts: BTreeSet<CatalogArtifact>,
    types: BTreeSet<TypeRow>,
    properties: BTreeSet<PropertyRow>,
    links: BTreeSet<LinkRow>,
    transitions: BTreeSet<TransitionRow>,
    states: BTreeSet<MemberRef>,
}

fn at(artifact: &ArtifactEnvelope, member: Member) -> MemberRef {
    MemberRef {
        artifact_id: artifact.artifact_id.clone(),
        version: artifact.version.clone(),
        member,
    }
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
                let def = root.interface;
                let name = def.name.as_str().to_owned();
                self.types.insert(TypeRow {
                    at: at(artifact, type_member(name.clone())),
                    kind: TypeKind::Interface,
                    extends: def.extends.iter().map(|n| n.0.clone()).collect(),
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
            extends: def.extends.iter().map(|n| n.0.clone()).collect(),
            implements: def.implements.iter().map(|n| n.0.clone()).collect(),
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

    fn finish(self) -> DefinitionCatalog {
        DefinitionCatalog {
            format: CatalogFormat::V1,
            artifacts: self.artifacts.into_iter().collect(),
            types: self.types.into_iter().collect(),
            properties: self.properties.into_iter().collect(),
            links: self.links.into_iter().collect(),
            transitions: self.transitions.into_iter().collect(),
            states: self.states.into_iter().collect(),
            writes: Vec::new(),
            depends_on: Vec::new(),
            mappings: Vec::new(),
            laws: Vec::new(),
        }
    }
}
