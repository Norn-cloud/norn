//! ACTOR-INDEPENDENT-1 (#1091, Astra ruling on #1101): an independent-actor
//! check guards the effective transition, not only the action declaring it.
//!
//! Hosts bind the guards of every admitted action
//! ([`crate::TypeCatalog::with_transition_guards`]). At commit the kernel
//! applies every guard on the fired transition, whichever action fires it,
//! and refuses a synchronous writer that fires a guarded transition. A guard
//! property must be able to hold only a stamped principal: a protected,
//! stored property with no default and no lifecycle.

use std::collections::BTreeSet;

use norn_ir::{ActionEffectProgram, ActionRoot, ObjectTypeName, PropertyId, TransitionId};

use crate::action_commit::{ActionCall, ActionRefusal};
use crate::cell_host::{FactKey, FactState, JsonValue};
use crate::decision_commit::TypeCatalog;

/// One transition an admitted action guards with `independentOf`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransitionGuard {
    /// The guarded type; its subtypes and supertypes share the guard.
    pub object_type: ObjectTypeName,
    /// The lifecycle property.
    pub lifecycle: PropertyId,
    /// The guarded transition.
    pub transition: TransitionId,
    /// The protected actor properties the caller must not be.
    pub independent_of: Vec<PropertyId>,
}

/// The guards of `actions`: each `fire_transition` with `independentOf`.
#[must_use]
pub fn transition_guards<'a>(
    actions: impl IntoIterator<Item = &'a ActionRoot>,
) -> Vec<TransitionGuard> {
    actions
        .into_iter()
        .filter_map(|action| match &action.effect_program {
            Some(ActionEffectProgram::FireTransition {
                object_kind,
                lifecycle,
                transition,
                ..
            }) if !action.independent_of.is_empty() => Some(TransitionGuard {
                object_type: ObjectTypeName(object_kind.clone()),
                lifecycle: lifecycle.clone(),
                transition: transition.clone(),
                independent_of: action.independent_of.clone(),
            }),
            Some(_) | None => None,
        })
        .collect()
}

/// The guards on firing `transition` of `lifecycle` on an object of `object_type`.
fn guards_on<'g>(
    catalog: &TypeCatalog<'g>,
    object_type: &ObjectTypeName,
    lifecycle: &PropertyId,
    transition: &TransitionId,
) -> impl Iterator<Item = &'g TransitionGuard> {
    let lineage = catalog.lineage(object_type);
    let related = move |guard: &TransitionGuard| {
        lineage.contains(&guard.object_type)
            || catalog.lineage(&guard.object_type).contains(object_type)
    };
    catalog
        .transition_guards
        .unwrap_or_default()
        .iter()
        .filter(move |guard| {
            guard.lifecycle == *lifecycle && guard.transition == *transition && related(guard)
        })
}

/// TXN-CLOSED-1 / ACTOR-INDEPENDENT-1: whether a writer's `transition` change
/// is refused. With no bound guards a writer fires no transition (fail
/// closed); otherwise only an unguarded transition.
pub(crate) fn writer_transition_refused(
    catalog: &TypeCatalog<'_>,
    object_type: &ObjectTypeName,
    lifecycle: &PropertyId,
    transition: &TransitionId,
) -> bool {
    catalog.transition_guards.is_none()
        || guards_on(catalog, object_type, lifecycle, transition)
            .next()
            .is_some()
}

/// Exact set of protected actor properties read by `independentOf` and every
/// bound transition guard for `(object_type, lifecycle, transition)`.
/// Returns `None` (fail closed) when transition guards are unbound on a type
/// with related protected properties.
pub(crate) fn independent_actor_properties(
    catalog: &TypeCatalog<'_>,
    action: &ActionRoot,
    object_type: &ObjectTypeName,
    lifecycle: &PropertyId,
    transition: &TransitionId,
) -> Option<BTreeSet<PropertyId>> {
    if catalog.transition_guards.is_none() && catalog.related_protected(object_type) {
        return None;
    }
    let mut properties: BTreeSet<PropertyId> = action.independent_of.iter().cloned().collect();
    properties.extend(
        guards_on(catalog, object_type, lifecycle, transition)
            .flat_map(|guard| guard.independent_of.iter().cloned()),
    );
    Some(properties)
}

/// ACTOR-INDEPENDENT-1: the verified principal differs from every recorded
/// actor the action or any guard of this transition names on its committed
/// target. A property that could hold anything but a stamped principal, or
/// records none, cannot show independence and refuses too. The commit fences
/// the target's read version, so a concurrent change conflicts rather than
/// passing. Refusals never carry the recorded value.
pub(crate) fn check_independent_actor<X>(
    catalog: &TypeCatalog<'_>,
    call: &ActionCall<'_>,
    target: &FactKey,
    lifecycle: &PropertyId,
    transition: &TransitionId,
    state: &FactState,
) -> Result<(), ActionRefusal<X>> {
    let Some(properties) = independent_actor_properties(
        catalog,
        call.action,
        &target.object_type,
        lifecycle,
        transition,
    ) else {
        return Err(ActionRefusal::PermissionDenied {
            reason: "ACTOR-INDEPENDENT-1: transition guards are not bound for a type with protected actors".to_owned(),
        });
    };
    let effective = catalog.effective_type(&target.object_type);
    let lifecycles = catalog.lifecycle_properties(&target.object_type);
    for property in &properties {
        let stampable = effective
            .and_then(|effective| effective.property(property.as_str()))
            .is_some_and(|declared| {
                declared.protected() && !declared.computed && declared.default.is_none()
            })
            && !lifecycles.contains(property);
        let recorded = state
            .values
            .as_ref()
            .and_then(|values| values.get(property.as_str()))
            .and_then(JsonValue::as_str);
        let reason = if stampable {
            match recorded {
                Some(actor) if actor != call.actor => continue,
                Some(_) => "records this principal, who cannot also perform this action",
                None => "records no principal, so independence cannot be shown",
            }
        } else {
            "cannot hold a stamped principal"
        };
        return Err(ActionRefusal::PermissionDenied {
            reason: format!("ACTOR-INDEPENDENT-1: {} {reason}", property.as_str()),
        });
    }
    Ok(())
}
