//! Lifecycle admission of one decision change: declared defaults on create
//! (initial states: [`crate::lifecycle_init`]), and the effective transition
//! and its guards on fire.
//! Split from `decision_commit` to keep that module within its LOC ceiling.

use std::collections::BTreeMap;

use norn_expr::Value as ExprValue;
use norn_ir::{FiringRefusal, PropertyId, Transition, TransitionId};

use super::{DecisionRefusal, TypeCatalog, expr_value, guard_holds};
use crate::cell_host::{FactKey, JsonObject, JsonValue};

/// Fill declared defaults for lifecycle properties without requiring their
/// final values to already be initial states. Callers that prepare a create
/// still use [`crate::lifecycle_init::with_initial_states`] for that full
/// validation.
pub(crate) fn fill_lifecycle_defaults<X>(
    catalog: &TypeCatalog<'_>,
    key: &FactKey,
    values: &JsonObject,
) -> Result<JsonObject, DecisionRefusal<X>> {
    materialize_lifecycle_defaults(catalog, key, values, false)
}

pub(crate) fn materialize_lifecycle_defaults<X>(
    catalog: &TypeCatalog<'_>,
    key: &FactKey,
    values: &JsonObject,
    validate_initial: bool,
) -> Result<JsonObject, DecisionRefusal<X>> {
    let mut values = values.clone();
    let effective = catalog.lifecycles(&key.object_type);
    if let Some(unresolved) = catalog
        .lifecycle_properties(&key.object_type)
        .into_iter()
        .find(|property| !effective.contains_key(property))
    {
        return Err(DecisionRefusal::UnresolvedLifecycle {
            key: key.clone(),
            lifecycle: unresolved.as_str().to_owned(),
        });
    }
    for lifecycle in effective.into_values() {
        let property = lifecycle.property.as_str();
        let default = catalog
            .default_of(&key.object_type, &lifecycle.property)
            .filter(|_| !values.contains_key(property));
        if let Some(default) = default {
            let default =
                JsonValue::from_ir(default).map_err(|error| DecisionRefusal::InvalidOutput {
                    reason: format!("the default of `{property}`: {error}"),
                })?;
            values.insert(property.to_owned(), default);
        }
        if validate_initial {
            let state = match values.get(property) {
                Some(JsonValue::String(state)) => Some(state),
                _ => None,
            };
            if !state.is_some_and(|state| lifecycle.initial.contains(state)) {
                return Err(DecisionRefusal::InvalidInitialState {
                    key: key.clone(),
                    lifecycle: property.to_owned(),
                    state: state.cloned(),
                    initial: lifecycle.initial.clone(),
                });
            }
        }
    }
    Ok(values)
}

/// The effective lifecycle of `lifecycle`, then the firing of `transition`
/// from the object's current state under `Lifecycle::fire` — the per-call
/// semantics `norn-checker` uses for `fire_transition` (same codes).
/// Returns the transition and the state it leaves.
pub(crate) fn resolve_transition<X>(
    catalog: &TypeCatalog<'_>,
    key: &FactKey,
    values: &JsonObject,
    lifecycle: &PropertyId,
    transition: &TransitionId,
) -> Result<(Transition, String), DecisionRefusal<X>> {
    let Some(declared) = catalog.lifecycle(&key.object_type, lifecycle) else {
        let lifecycle_name = lifecycle.as_str().to_owned();
        return Err(
            if catalog
                .lifecycle_properties(&key.object_type)
                .contains(lifecycle)
            {
                DecisionRefusal::UnresolvedLifecycle {
                    key: key.clone(),
                    lifecycle: lifecycle_name,
                }
            } else {
                DecisionRefusal::UnknownLifecycle {
                    key: key.clone(),
                    lifecycle: lifecycle_name,
                }
            },
        );
    };
    let current = match values.get(lifecycle.as_str()) {
        Some(JsonValue::String(state)) => Some(state.clone()),
        _ => None,
    };
    match declared.fire(transition.as_str(), current.as_deref()) {
        Ok(fired) => Ok((fired.clone(), current.unwrap_or_default())),
        Err(FiringRefusal::UnknownTransition) => Err(DecisionRefusal::UnknownTransition {
            key: key.clone(),
            lifecycle: lifecycle.as_str().to_owned(),
            transition: transition.as_str().to_owned(),
        }),
        Err(FiringRefusal::NotFromState) => Err(DecisionRefusal::NotFromState {
            lifecycle: lifecycle.as_str().to_owned(),
            key: Box::new(key.clone()),
            transition: transition.as_str().to_owned(),
            current,
            from: declared
                .transitions
                .iter()
                .find(|candidate| candidate.id == *transition)
                .map_or_default(|candidate| candidate.from.clone()),
        }),
    }
}

/// Evaluates every guard of `declared` against the object's state before
/// the change (the checker does not: guards read the object).
pub(crate) fn check_guards<X>(
    key: &FactKey,
    values: &JsonObject,
    declared: &Transition,
) -> Result<(), DecisionRefusal<X>> {
    if declared.guards.is_empty() {
        return Ok(());
    }
    let diagnostic = declared
        .guard_refusal_code
        .as_deref()
        .and_then(norn_checker::codes::lookup)
        .map(|entry| entry.code);
    let guard_error = |guard: &str, reason: String| DecisionRefusal::GuardError {
        diagnostic,
        key: Box::new(key.clone()),
        transition: declared.id.as_str().to_owned(),
        guard: guard.to_owned(),
        reason,
    };
    let object = values
        .iter()
        .map(|(name, value)| expr_value(value).map(|value| (name.clone(), value)))
        .collect::<Result<BTreeMap<_, _>, _>>();
    let context = match object {
        Ok(object) => ExprValue::object([("self", ExprValue::Object(object))]),
        Err(reason) => {
            let first = declared.guards.first().map_or("", |guard| guard.as_str());
            return Err(guard_error(first, reason));
        }
    };
    for guard in &declared.guards {
        match guard_holds(guard.as_str(), &context) {
            Ok(true) => {}
            Ok(false) => {
                return Err(DecisionRefusal::GuardFalse {
                    diagnostic,
                    key: key.clone(),
                    transition: declared.id.as_str().to_owned(),
                    guard: guard.as_str().to_owned(),
                });
            }
            Err(reason) => return Err(guard_error(guard.as_str(), reason)),
        }
    }
    Ok(())
}
