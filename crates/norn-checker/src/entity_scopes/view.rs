//! The scope-bound view admission resolves every entity name through (#1376).

use std::cell::OnceCell;
use std::collections::{BTreeMap, BTreeSet};

use norn_expr::{EntityDefinitions, EnvError, EnvOptions, ZenTypeEnv};
use norn_ir::{
    ArtifactEnvelope, ArtifactPayload, PropertyTypeDef, PropertyTypeName, ResourceLimitProfile,
    ScopeRef,
};

use super::{ScopeKey, entity_object_type_owners, scope_key, scope_visible};
use crate::action_set::Model;
use crate::entity_artifacts::{Decoded, Layer, decode_v2};
use crate::entity_instance::EffectiveCatalog;
use crate::entity_lowering::entity_object_type;

/// The definitions one consumer may observe: the candidates and installed
/// base whose scope is visible from the consumer's, and the effective
/// catalog, type environments and object-type [`Model`] derived from them
/// alone. It is the only public source of those for a set, so a consumer
/// cannot resolve a name its scope cannot see.
pub struct ScopedView<'a> {
    artifacts: Vec<&'a ArtifactEnvelope>,
    hidden: Vec<&'a ArtifactEnvelope>,
    base: Vec<&'a ArtifactEnvelope>,
    pool: Vec<ArtifactEnvelope>,
    registry: BTreeSet<PropertyTypeName>,
    catalog: OnceCell<EffectiveCatalog>,
    layer: OnceCell<Layer>,
    env: OnceCell<Option<ZenTypeEnv>>,
    model: OnceCell<Model<'a>>,
}

impl<'a> ScopedView<'a> {
    /// The view of the artifact `consumer` over candidates `artifacts` and
    /// the installed `base`.
    #[must_use]
    pub fn of(
        consumer: &ArtifactEnvelope,
        artifacts: &'a [ArtifactEnvelope],
        base: &'a [ArtifactEnvelope],
        registry: &BTreeSet<PropertyTypeName>,
    ) -> Self {
        Self::bound(&consumer.scope, artifacts, base, registry)
    }

    /// The view of a host-bound consumer: the scope a cell or a provisioned
    /// tenant is bound to.
    #[must_use]
    pub fn bound(
        scope: &ScopeRef,
        artifacts: &'a [ArtifactEnvelope],
        base: &'a [ArtifactEnvelope],
        registry: &BTreeSet<PropertyTypeName>,
    ) -> Self {
        let (mut visible, mut hidden) = (Vec::new(), Vec::new());
        for (installed, artifact) in base
            .iter()
            .map(|artifact| (true, artifact))
            .chain(artifacts.iter().map(|artifact| (false, artifact)))
        {
            if scope_visible(scope, &artifact.scope) {
                visible.push((installed, artifact));
            } else {
                hidden.push(artifact);
            }
        }
        let base: Vec<_> = visible
            .iter()
            .filter(|(installed, _)| *installed)
            .map(|(_, a)| *a)
            .collect();
        let artifacts = visible
            .iter()
            .filter(|(installed, _)| !installed)
            .map(|(_, a)| *a)
            .collect();
        let pool = base
            .iter()
            .chain(&artifacts)
            .map(|a| (*a).clone())
            .collect();
        Self {
            artifacts,
            hidden,
            base,
            pool,
            registry: registry.clone(),
            catalog: OnceCell::new(),
            layer: OnceCell::new(),
            env: OnceCell::new(),
            model: OnceCell::new(),
        }
    }

    /// The view of the artifact that declares object type `name`: what a
    /// consumer without an envelope of its own (a question over `name`, an
    /// expression with `self: name`) reads. Without a declarer it is the
    /// view of the set's one shared scope, or of the global scope.
    #[must_use]
    pub fn declaring(
        name: &str,
        artifacts: &'a [ArtifactEnvelope],
        registry: &BTreeSet<PropertyTypeName>,
    ) -> Self {
        let scope = entity_object_type_owners(artifacts, &[])
            .get(name)
            .map(|owner| owner.scope.clone())
            .or_else(|| shared_scope(artifacts))
            .unwrap_or_default();
        Self::bound(&scope, artifacts, &[], registry)
    }

    /// The view every artifact of `artifacts` shares, when they all have one
    /// scope; `None` for a set spanning several, which needs a consumer.
    #[must_use]
    pub fn uniform(
        artifacts: &'a [ArtifactEnvelope],
        registry: &BTreeSet<PropertyTypeName>,
    ) -> Option<Self> {
        shared_scope(artifacts).map(|scope| Self::bound(&scope, artifacts, &[], registry))
    }

    /// The visible installed base, then the visible candidates.
    #[must_use]
    pub fn pool(&self) -> &[ArtifactEnvelope] {
        &self.pool
    }

    pub(crate) fn candidates(&self) -> &[&'a ArtifactEnvelope] {
        &self.artifacts
    }

    pub(crate) fn installed(&self) -> &[&'a ArtifactEnvelope] {
        &self.base
    }

    /// What the consumer cannot see: kept only to name the scope a refused
    /// reference lies in, never to resolve through.
    pub(crate) fn hidden(&self) -> &[&'a ArtifactEnvelope] {
        &self.hidden
    }

    pub(crate) const fn registry(&self) -> &BTreeSet<PropertyTypeName> {
        &self.registry
    }

    /// The effective catalog of the visible definitions.
    #[must_use]
    pub fn catalog(&self) -> &EffectiveCatalog {
        self.catalog
            .get_or_init(|| EffectiveCatalog::build_single_layer(&self.pool, &self.registry))
    }

    /// [`Self::catalog`], owned.
    #[must_use]
    pub fn into_catalog(self) -> EffectiveCatalog {
        let Self {
            pool,
            registry,
            catalog,
            ..
        } = self;
        catalog
            .into_inner()
            .unwrap_or_else(|| EffectiveCatalog::build_single_layer(&pool, &registry))
    }

    /// The object-type model admission checks actions, writes and bindings
    /// against.
    #[must_use]
    pub fn model(&self) -> &Model<'a> {
        self.model.get_or_init(|| Model::scoped(self))
    }

    /// The `EntityIR` v2 layer of [`Self::pool`], indexed into it.
    pub(crate) fn layer(&self) -> &Layer {
        self.layer.get_or_init(|| {
            let mut layer = Layer::default();
            for (index, artifact) in self.pool.iter().enumerate() {
                if let Decoded::Root(root) = decode_v2(artifact) {
                    layer.push(index, *root);
                }
            }
            layer
        })
    }

    /// The registry-aware expression environment under `norn.limits/1`, when
    /// it derives.
    pub(crate) fn env(&self) -> Option<&ZenTypeEnv> {
        self.env
            .get_or_init(|| {
                let limits = ResourceLimitProfile::norn_limits_v1();
                self.type_env(EnvOptions::from_limit_profile(&limits), false)
                    .ok()
            })
            .as_ref()
    }

    /// The expression environment of the visible definitions; `for_checking`
    /// derives it from every object type, v1 included, as action checking does.
    ///
    /// # Errors
    /// Every [`EnvError`] of the derivation.
    pub fn type_env(
        &self,
        options: EnvOptions,
        for_checking: bool,
    ) -> Result<ZenTypeEnv, Vec<EnvError>> {
        let layer = self.layer();
        let property_types = catalog_of(layer);
        let checked: Vec<_> = if for_checking {
            self.pool
                .iter()
                .filter_map(|artifact| {
                    let ArtifactPayload::Entity(payload) = &artifact.payload else {
                        return None;
                    };
                    entity_object_type(payload).ok().flatten()
                })
                .map(|object| object.root.object_type)
                .collect()
        } else {
            Vec::new()
        };
        let definitions = EntityDefinitions {
            property_types: &property_types,
            interfaces: &layer.interfaces,
            object_types: if for_checking {
                &checked
            } else {
                &layer.object_types
            },
            extensions: &layer.extensions,
        };
        if for_checking {
            ZenTypeEnv::from_artifact_set_for_checking(&definitions, &self.registry, options)
        } else {
            ZenTypeEnv::from_artifact_set_with_registry(&definitions, &self.registry, options)
        }
    }

    /// The environment an admission receipt pins for this consumer: the
    /// visible definitions under `limits`, with the exact rule and action
    /// schemas bound.
    ///
    /// # Errors
    /// Every [`EnvError`] of the derivation.
    pub fn receipt_env(&self, limits: &ResourceLimitProfile) -> Result<ZenTypeEnv, Vec<EnvError>> {
        definitions_env(self.layer(), &self.pool, limits)
    }
}

/// The views of a set's consumers, one per distinct consumer scope.
pub struct ScopedViews<'a> {
    artifacts: &'a [ArtifactEnvelope],
    base: &'a [ArtifactEnvelope],
    registry: BTreeSet<PropertyTypeName>,
    views: BTreeMap<ScopeKey, ScopedView<'a>>,
}

impl<'a> ScopedViews<'a> {
    /// The views over candidates `artifacts` and installed `base`.
    #[must_use]
    pub fn new(
        artifacts: &'a [ArtifactEnvelope],
        base: &'a [ArtifactEnvelope],
        registry: &BTreeSet<PropertyTypeName>,
    ) -> Self {
        Self {
            artifacts,
            base,
            registry: registry.clone(),
            views: BTreeMap::new(),
        }
    }

    /// The view of `consumer`.
    pub fn of(&mut self, consumer: &ArtifactEnvelope) -> &ScopedView<'a> {
        let (artifacts, base, registry) = (self.artifacts, self.base, &self.registry);
        self.views
            .entry(scope_key(&consumer.scope))
            .or_insert_with(|| ScopedView::of(consumer, artifacts, base, registry))
    }
}

fn shared_scope(artifacts: &[ArtifactEnvelope]) -> Option<ScopeRef> {
    let scope = artifacts
        .first()
        .map_or_else(ScopeRef::default, |first| first.scope.clone());
    artifacts
        .iter()
        .all(|artifact| artifact.scope == scope)
        .then_some(scope)
}

fn catalog_of(layer: &Layer) -> Vec<PropertyTypeDef> {
    layer.catalog.iter().map(|(_, def)| def.clone()).collect()
}

/// The `EntityIR` v2 environment of `layer` (indexed into `pool`) under
/// `limits`, with every rule and action schema of `pool` bound exactly.
pub(crate) fn definitions_env(
    layer: &Layer,
    pool: &[ArtifactEnvelope],
    limits: &ResourceLimitProfile,
) -> Result<ZenTypeEnv, Vec<EnvError>> {
    let property_types = catalog_of(layer);
    let definitions = EntityDefinitions {
        property_types: &property_types,
        interfaces: &layer.interfaces,
        object_types: &layer.object_types,
        extensions: &layer.extensions,
    };
    let mut env =
        ZenTypeEnv::from_artifact_set(&definitions, EnvOptions::from_limit_profile(limits))?;
    for artifact in pool {
        let action = matches!(artifact.payload, ArtifactPayload::Action(_));
        if !action && !matches!(artifact.payload, ArtifactPayload::Rule(_)) {
            continue;
        }
        let payload = artifact.payload.to_json_value();
        for member in ["inputSchema", "outputSchema"] {
            let Some(schema) = payload.as_object().and_then(|payload| payload.get(member)) else {
                continue;
            };
            let name = format!(
                "exact.{}.{}.{member}",
                artifact.artifact_id, artifact.version
            );
            let bound = if action && member == "inputSchema" {
                env.bind_exact_action_schema(&name, schema)
            } else {
                env.bind_exact_schema(&name, schema)
            };
            env = bound.map_err(|reason| {
                vec![EnvError::Unresolved {
                    site: name,
                    name: reason,
                }]
            })?;
        }
    }
    Ok(env)
}
