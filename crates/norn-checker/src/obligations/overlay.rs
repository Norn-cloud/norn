//! OVERLAY-MONOTONE-1: retained authority and conjunctive goal generation.
use super::{DefinitionSource, RetainedLayers, admit_with_authority};
use crate::entity_artifacts::{Decoded, Layer, decode_v2};
use crate::{EffectiveCatalog, EntityArtifactSetContext};
use norn_expr::{EntityDefinitions, EnvOptions, LinearGuard, ZenTypeEnv, linear_guard};
use norn_graph::Budget;
use norn_ir::{
    ArtifactEnvelope, ArtifactKind, CustomizationOverlayOp, CustomizationRoot, ScopeRef,
};
use std::collections::{BTreeMap, BTreeSet};

/// The single Core 06 refusal namespace, shared with application integrations.
pub const OVERLAY_CONFLICT: &str = "core.composition.overlay_conflict";
/// Why a source-bound overlay cannot enter this analysis profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OverlayRefusal {
    reason: &'static str,
}
impl OverlayRefusal {
    /// Stable composition refusal code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        OVERLAY_CONFLICT
    }
    /// Stable reason; no solver failure is interpreted as compatibility.
    #[must_use]
    pub const fn reason(self) -> &'static str {
        self.reason
    }
}
fn refuse(reason: &'static str) -> OverlayRefusal {
    OverlayRefusal { reason }
}

type OverlayGuardGoal = (norn_ir::mapping::InterpretationEndpoint, Vec<LinearGuard>);
/// Generated goals and unchanged B2 law instances; grants no admission or proof.
#[derive(Debug)]
pub struct OverlayPlan {
    source_sha256: String,
    retained_sha256: String,
    goals: Vec<OverlayGuardGoal>,
    laws: Vec<norn_ir::verification::EffectiveLawInstance>,
}
impl OverlayPlan {
    /// Exact candidate/base source commitment.
    #[must_use]
    pub fn source_sha256(&self) -> &str {
        &self.source_sha256
    }
    /// Independently verified B3 retained-layer identity.
    #[must_use]
    pub fn retained_sha256(&self) -> &str {
        &self.retained_sha256
    }
    /// Source-derived conjunctions; satisfiability has not been established.
    pub fn goals(
        &self,
    ) -> impl Iterator<Item = (&norn_ir::mapping::InterpretationEndpoint, &[LinearGuard])> {
        self.goals
            .iter()
            .map(|(pin, guards)| (pin, guards.as_slice()))
    }
    /// Existing B2 law projection, including inherited origins and instance paths.
    #[must_use]
    pub fn laws(&self) -> &[norn_ir::verification::EffectiveLawInstance] {
        &self.laws
    }
}

/// OVERLAY-MONOTONE-1 forbids replacement, including `update_validation`.
/// The result is pending analysis: SAT is necessary, and progress stays unknown.
/// # Errors
/// Refuses stale scope/pins/authority, non-conjunctive edits and unsupported guards.
pub fn overlay_obligations(
    source: &DefinitionSource,
    retained: &RetainedLayers,
    scope: &ScopeRef,
) -> Result<OverlayPlan, OverlayRefusal> {
    let context = EntityArtifactSetContext {
        registry: retained.registry(),
        base: &source.base,
        tenant: None,
    };
    admit_with_authority(source, &context, Some(retained))
        .map_err(|_error| refuse("invalid_retained_input"))?;
    let mut layer = Layer::default();
    for (index, artifact) in retained.artifacts().iter().enumerate() {
        if let Decoded::Root(root) = decode_v2(artifact) {
            layer.push(index, *root);
        }
    }
    let properties: Vec<_> = layer.catalog.iter().map(|(_, p)| p.clone()).collect();
    let env = ZenTypeEnv::from_artifact_set_with_registry(
        &EntityDefinitions {
            property_types: &properties,
            interfaces: &layer.interfaces,
            object_types: &layer.object_types,
            extensions: &layer.extensions,
        },
        retained.registry(),
        EnvOptions::from_limit_profile(&norn_ir::ResourceLimitProfile::norn_limits_v1()),
    )
    .map_err(|_error| refuse("unsupported_predicate"))?;
    let catalog =
        EffectiveCatalog::from_artifacts_with_registry(retained.artifacts(), retained.registry());
    let mut goals = BTreeMap::<(String, String, String), OverlayGuardGoal>::new();
    let mut ids = BTreeSet::new();
    for artifact in &source.artifacts {
        if artifact.kind() != ArtifactKind::Customization {
            return Err(refuse("unsupported_predicate"));
        }
        let overlay: CustomizationRoot = serde_json::from_value(
            serde_json::to_value(artifact.payload.to_json_value())
                .map_err(|_error| refuse("unsupported_predicate"))?,
        )
        .map_err(|_error| refuse("unsupported_predicate"))?;
        let dimensions = serde_json::to_value(scope).map_err(|_error| refuse("scope_mismatch"))?;
        if &artifact.scope != scope
            || overlay.scope_required.iter().any(|name| {
                dimensions
                    .get(name)
                    .and_then(serde_json::Value::as_str)
                    .is_none_or(|s| s.trim().is_empty())
            })
        {
            return Err(refuse("scope_mismatch"));
        }
        let (target, pin) = pinned_target(retained, &overlay, scope)?;
        let (bound, base) = entity_guards(target, &layer, &catalog, &env)?;
        let key = (
            pin.artifact_id.clone(),
            pin.version.clone(),
            pin.sha256.clone(),
        );
        let goal = goals.entry(key).or_insert((
            norn_ir::mapping::InterpretationEndpoint {
                artifact_id: pin.artifact_id,
                version: pin.version,
                sha256: pin.sha256,
            },
            base,
        ));
        append_guards(goal, overlay.overlay.operations, &bound, &mut ids)?;
    }

    let laws = crate::verification::effective_law_instances(
        &source.artifacts,
        &context,
        Budget {
            max_nodes: 16_384,
            max_edges: 131_072,
            max_steps: 1_048_576,
        },
    )
    .map_err(|_error| refuse("invalid_retained_input"))?;
    Ok(OverlayPlan {
        source_sha256: source.exact_digest().to_owned(),
        retained_sha256: retained.exact_digest().to_owned(),
        goals: goals.into_values().collect(),
        laws,
    })
}

fn pinned_target<'a>(
    retained: &'a RetainedLayers,
    overlay: &CustomizationRoot,
    scope: &ScopeRef,
) -> Result<(&'a ArtifactEnvelope, crate::ArtifactRef), OverlayRefusal> {
    let target = retained
        .artifacts()
        .iter()
        .find(|a| {
            a.artifact_id == overlay.target_artifact.artifact_id
                && a.version == overlay.target_artifact.version
        })
        .ok_or_else(|| refuse("unresolved_pin"))?;
    let pin = crate::receipt::ArtifactRef::of(target).map_err(|_error| refuse("unresolved_pin"))?;
    let expected = overlay
        .target_artifact
        .additional
        .get("sha256")
        .and_then(norn_ir::JsonValue::as_string);
    if serde_json::to_value(target.kind()).ok()
        != serde_json::to_value(overlay.target_artifact.kind).ok()
        || expected != Some(pin.sha256.as_str())
        || crate::artifact_set::scope_conflict(scope, &target.scope).is_some()
    {
        return Err(refuse("unresolved_pin"));
    }
    Ok((target, pin))
}

fn append_guards(
    goal: &mut OverlayGuardGoal,
    operations: Vec<CustomizationOverlayOp>,
    env: &ZenTypeEnv,
    ids: &mut BTreeSet<(String, String)>,
) -> Result<(), OverlayRefusal> {
    for operation in operations {
        let CustomizationOverlayOp::AddValidation { additional } = operation else {
            return Err(refuse("weakens_inherited"));
        };
        let id = additional
            .get("id")
            .and_then(norn_ir::JsonValue::as_string)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| refuse("unsupported_predicate"))?;
        let expression = additional
            .get("expression")
            .and_then(norn_ir::JsonValue::as_string)
            .ok_or_else(|| refuse("unsupported_predicate"))?;
        if additional.len() != 2 || !ids.insert((goal.0.artifact_id.clone(), id.to_owned())) {
            return Err(refuse("unsupported_predicate"));
        }
        goal.1
            .push(linear_guard(expression, env).map_err(|_error| refuse("unsupported_predicate"))?);
    }
    Ok(())
}

fn entity_guards(
    target: &ArtifactEnvelope,
    layer: &Layer,
    catalog: &EffectiveCatalog,
    env: &ZenTypeEnv,
) -> Result<(ZenTypeEnv, Vec<LinearGuard>), OverlayRefusal> {
    let Decoded::Root(root) = decode_v2(target) else {
        return Err(refuse("unsupported_predicate"));
    };
    let (bound, invariants): (_, Vec<_>) = match *root {
        norn_ir::EntityRoot::V2(root) => {
            let effective = catalog
                .object_type_named(root.object_type.name.as_str())
                .and_then(Result::ok)
                .ok_or_else(|| refuse("unsupported_predicate"))?;
            let invariants = layer
                .object_types
                .iter()
                .filter(|t| effective.lineage.contains(&t.name))
                .flat_map(|t| &t.invariants)
                .chain(
                    layer
                        .interfaces
                        .iter()
                        .filter(|i| effective.interfaces.contains(&i.name))
                        .flat_map(|i| &i.invariants),
                )
                .collect();
            (env.clone().bind_self(&root.object_type.name), invariants)
        }
        norn_ir::EntityRoot::V2Interface(root) => {
            if !root.interface.extends.is_empty() {
                return Err(refuse("unsupported_predicate"));
            }
            (
                env.clone().bind_interface_self(&root.interface.name),
                layer
                    .interfaces
                    .iter()
                    .filter(|i| i.name == root.interface.name)
                    .flat_map(|i| &i.invariants)
                    .collect(),
            )
        }
        norn_ir::EntityRoot::V2Extension(_)
        | norn_ir::EntityRoot::V2PropertyTypes(_)
        | norn_ir::EntityRoot::V2Markings(_)
        | norn_ir::EntityRoot::V1(_) => return Err(refuse("unsupported_predicate")),
    };
    let bound = bound.map_err(|_error| refuse("unsupported_predicate"))?;
    let guards = invariants
        .into_iter()
        .filter(|invariant| invariant.classification.is_integrity())
        .map(|invariant| {
            linear_guard(invariant.expression.as_str(), &bound)
                .map_err(|_error| refuse("unsupported_predicate"))
        })
        .collect::<Result<_, _>>()?;
    Ok((bound, guards))
}
