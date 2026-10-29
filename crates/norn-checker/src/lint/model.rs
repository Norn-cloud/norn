//! The lint view of an artifact set: every object type, interface,
//! extension, and shared property type, decoded once, with the artifact that
//! owns it and a JSON pointer to the definition in that artifact's envelope,
//! plus the effective object types (inherited, overridden, `ref`-resolved,
//! and extension members) the entity model check derives.

use std::collections::{BTreeMap, BTreeSet};

use norn_ir::{
    ArtifactEnvelope, ArtifactPayload, EntityRoot, InterfaceDef, ObjectTypeDef,
    ObjectTypeExtension, ObjectTypeName, PropertyTypeDef,
};

use crate::entity_artifacts::{Decoded, Layer, decode_v2};
use crate::entity_instance::{EffectiveObjectType, LAYER_DEPENDENT};
use crate::entity_lowering::entity_object_type;
use crate::entity_model::{EntityContext, EntityModel, check_entity_model, effective_object_types};
use crate::lint::coverage::MappingCoverage;

/// What a lint run reads: the candidate artifacts (the ones findings are
/// reported on), the installed read-only base they build on (used only to
/// resolve names, never reported), and the mapping coverage computed for
/// the mappings of the set.
#[derive(Debug, Clone, Copy)]
pub struct LintInput<'a> {
    /// The candidate artifacts; every finding belongs to one of them (or to
    /// a mapping of `coverage`).
    pub artifacts: &'a [ArtifactEnvelope],
    /// The installed base layer: resolves supertypes, link targets, and
    /// duplicates, but is never linted.
    pub base: &'a [ArtifactEnvelope],
    /// The coverage of every mapping in the set (empty when the set has no
    /// mappings).
    pub coverage: &'a [MappingCoverage],
}

/// Whether a definition is a candidate (linted) or comes from the base.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Origin {
    Candidate,
    Base,
}

/// One decoded definition with its owner.
#[derive(Debug, Clone)]
pub(crate) struct Owned<'a, T> {
    pub(crate) artifact: &'a ArtifactEnvelope,
    pub(crate) origin: Origin,
    /// JSON pointer to the definition in the owner's envelope; `None` when
    /// the definition was lowered from `EntityIR` v1 and has no v2 location.
    pub(crate) root: Option<String>,
    pub(crate) def: T,
}

impl<T> Owned<'_, T> {
    /// The pointer of `suffix` below the definition (`/name`,
    /// `/properties/0/id`), or `/payload` for a lowered v1 definition.
    pub(crate) fn at(&self, suffix: &str) -> String {
        self.root
            .as_ref()
            .map_or_else(|| "/payload".to_owned(), |root| format!("{root}{suffix}"))
    }

    pub(crate) fn is_candidate(&self) -> bool {
        self.origin == Origin::Candidate
    }
}

/// The decoded definitions of a lint input. Candidates come first, then the
/// base, each in artifact order.
#[derive(Default)]
pub(crate) struct LintSet<'a> {
    pub(crate) object_types: Vec<Owned<'a, ObjectTypeDef>>,
    pub(crate) interfaces: Vec<Owned<'a, InterfaceDef>>,
    pub(crate) extensions: Vec<Owned<'a, ObjectTypeExtension>>,
    pub(crate) property_types: Vec<Owned<'a, PropertyTypeDef>>,
    /// The `EntityIR` v2 definitions of candidates and base as one layer.
    layer: Layer,
    /// Effective object types by name (`EntityIR` v2 only).
    effective: BTreeMap<String, EffectiveObjectType>,
    /// The next artifact index of `layer`.
    layer_index: usize,
    pub(crate) coverage: &'a [MappingCoverage],
    pub(crate) candidates: &'a [ArtifactEnvelope],
}

impl<'a> LintSet<'a> {
    pub(crate) fn new(input: &LintInput<'a>) -> Self {
        let mut set = Self {
            coverage: input.coverage,
            candidates: input.artifacts,
            ..Self::default()
        };
        for artifact in input.artifacts {
            set.push(artifact, Origin::Candidate);
        }
        for artifact in input.base {
            set.push(artifact, Origin::Base);
        }
        set.effective = set.effective_types();
        set
    }

    /// The effective object types of the combined layer. Effective views are
    /// only derived from a model the entity model check accepts (as
    /// [`EffectiveCatalog`](crate::EffectiveCatalog) requires); for a model
    /// with errors the map is empty and callers fall back to the declared
    /// definitions.
    fn effective_types(&self) -> BTreeMap<String, EffectiveObjectType> {
        let catalog: Vec<PropertyTypeDef> = self
            .layer
            .catalog
            .iter()
            .map(|(_, def)| def.clone())
            .collect();
        let registry = BTreeSet::new();
        let context = EntityContext {
            catalog: &catalog,
            registry: &registry,
            base: EntityModel::EMPTY,
            tenant: None,
        };
        let model = self.layer.as_model();
        // Findings that depend on the layer an artifact is installed in are
        // ignored, as in the effective catalog: candidates and base are one
        // layer here, so an extension sits beside its target.
        if check_entity_model(&model, &context)
            .iter()
            .any(|finding| finding.is_error() && !LAYER_DEPENDENT.contains(&finding.code.as_str()))
        {
            return BTreeMap::new();
        }
        effective_object_types(&model, &context)
            .into_values()
            .map(|(effective, _links)| (effective.name.as_str().to_owned(), effective))
            .collect()
    }

    fn push(&mut self, artifact: &'a ArtifactEnvelope, origin: Origin) {
        let ArtifactPayload::Entity(payload) = &artifact.payload else {
            return;
        };
        match decode_v2(artifact) {
            Decoded::Root(root) => {
                self.layer.push(self.layer_index, (*root).clone());
                self.layer_index = self.layer_index.saturating_add(1);
                self.push_root(artifact, origin, *root);
            }
            // v1 entities take part through their lowering (T1-ONT-4).
            Decoded::NotV2 => {
                if let Ok(Some(lowered)) = entity_object_type(payload) {
                    self.object_types.push(Owned {
                        artifact,
                        origin,
                        root: None,
                        def: lowered.root.object_type,
                    });
                }
            }
            // The per-artifact check reports an undecodable payload.
            Decoded::Invalid => {}
        }
    }

    fn push_root(&mut self, artifact: &'a ArtifactEnvelope, origin: Origin, root: EntityRoot) {
        match root {
            EntityRoot::V2(root) => self.object_types.push(Owned {
                artifact,
                origin,
                root: Some("/payload/objectType".to_owned()),
                def: root.object_type,
            }),
            EntityRoot::V2Interface(root) => self.interfaces.push(Owned {
                artifact,
                origin,
                root: Some("/payload/interface".to_owned()),
                def: root.interface,
            }),
            EntityRoot::V2PropertyTypes(root) => {
                self.property_types
                    .extend(
                        root.property_types
                            .into_iter()
                            .enumerate()
                            .map(|(position, def)| Owned {
                                artifact,
                                origin,
                                root: Some(format!("/payload/propertyTypes/{position}")),
                                def,
                            }),
                    );
            }
            EntityRoot::V2Extension(root) => self.extensions.push(Owned {
                artifact,
                origin,
                root: Some("/payload/objectTypeExtension".to_owned()),
                def: root.object_type_extension,
            }),
            EntityRoot::V2Markings(_) | EntityRoot::V1(_) => {}
        }
    }

    /// The candidate object types.
    pub(crate) fn candidate_object_types(&self) -> impl Iterator<Item = &Owned<'a, ObjectTypeDef>> {
        self.object_types
            .iter()
            .filter(|owned| owned.is_candidate())
    }

    /// The first object type named `name` (candidates before the base).
    pub(crate) fn object_type(&self, name: &ObjectTypeName) -> Option<&Owned<'a, ObjectTypeDef>> {
        self.object_types
            .iter()
            .find(|owned| &owned.def.name == name)
    }

    /// The effective view of object type `name`, when the entity model can
    /// derive one.
    pub(crate) fn effective(&self, name: &ObjectTypeName) -> Option<&EffectiveObjectType> {
        self.effective.get(name.as_str())
    }

    /// The extensions of object type `name`.
    pub(crate) fn extensions_of<'s>(
        &'s self,
        name: &'s ObjectTypeName,
    ) -> impl Iterator<Item = &'s Owned<'a, ObjectTypeExtension>> {
        self.extensions
            .iter()
            .filter(move |owned| &owned.def.target == name)
    }

    /// `name` and its supertypes, nearest first, stopping at the first
    /// unknown or repeated name (an inheritance cycle is the entity model
    /// check's finding, not the lint's).
    pub(crate) fn ancestry(&self, name: &ObjectTypeName) -> Vec<&Owned<'a, ObjectTypeDef>> {
        let mut chain = Vec::new();
        let mut seen = BTreeSet::new();
        let mut next = Some(name);
        while let Some(current) = next {
            if !seen.insert(current.as_str()) {
                break;
            }
            let Some(owned) = self.object_type(current) else {
                break;
            };
            chain.push(owned);
            next = owned.def.extends.as_ref();
        }
        chain
    }
}
