//! Interface lifecycle inheritance, narrowing, and non-weakening meet
//! helpers for [`super::lifecycle`] (`INTERFACE-LIFECYCLE-1`).

use std::collections::{BTreeMap, BTreeSet};

use super::interface::InterfaceDef;
use super::lifecycle::{Lifecycle, LifecycleEnding, LifecycleEvent, Transition};
use super::object::PropertyId;
use super::property::PropertyType;

pub(super) type InterfaceLifecycleCache<'a> =
    BTreeMap<&'a str, (BTreeMap<PropertyId, Lifecycle>, BTreeSet<PropertyId>)>;

pub(super) fn merge_inherited_lifecycles(
    incoming: BTreeMap<PropertyId, Lifecycle>,
    incoming_conflicts: BTreeSet<PropertyId>,
    states: &dyn Fn(&PropertyId) -> Option<Vec<String>>,
    inherited: &mut BTreeMap<PropertyId, Option<Lifecycle>>,
) {
    for prop in incoming_conflicts {
        inherited.insert(prop, None);
    }
    for (property, lifecycle) in incoming {
        if matches!(inherited.get(&property), Some(None)) {
            continue;
        }
        let Some(prop_states) = states(&property) else {
            continue;
        };
        let restricted = lifecycle.restricted(&prop_states);
        match inherited.get(&property) {
            None => {
                inherited.insert(property, Some(restricted));
            }
            Some(Some(existing)) => {
                let merged = meet_lifecycles(existing, &restricted, &prop_states);
                inherited.insert(property, merged);
            }
            Some(None) => {}
        }
    }
}

pub(super) fn apply_declared_lifecycles(
    lifecycles: &[Lifecycle],
    states: &dyn Fn(&PropertyId) -> Option<Vec<String>>,
    inherited: &BTreeMap<PropertyId, Option<Lifecycle>>,
    machines: &mut BTreeMap<PropertyId, Option<Lifecycle>>,
) {
    for lifecycle in lifecycles {
        if matches!(inherited.get(&lifecycle.property), Some(None)) {
            machines.insert(lifecycle.property.clone(), None);
            continue;
        }
        let Some(prop_states) = states(&lifecycle.property) else {
            continue;
        };
        let machine = match inherited.get(&lifecycle.property) {
            Some(Some(wide)) => narrow_lifecycle(lifecycle, wide, &prop_states),
            Some(None) => continue,
            None => lifecycle.restricted(&prop_states),
        };
        machines.insert(lifecycle.property.clone(), Some(machine));
    }
}

pub(super) fn inline_enum_values(property_type: &PropertyType) -> Option<Vec<String>> {
    match property_type {
        PropertyType::Enum { values } => {
            Some(values.iter().map(|value| value.value.clone()).collect())
        }
        PropertyType::Text { .. }
        | PropertyType::Integer { .. }
        | PropertyType::Decimal { .. }
        | PropertyType::Boolean { .. }
        | PropertyType::Timestamp { .. }
        | PropertyType::Date { .. }
        | PropertyType::Duration { .. }
        | PropertyType::Record { .. }
        | PropertyType::JsonExt { .. }
        | PropertyType::DecimalBinding { .. }
        | PropertyType::Ref { .. } => None,
    }
}

/// Compute effective inline enum values per interface across `owners`
/// (ordered parents-first), in linear O(V + E) time without recursion.
fn compute_interface_enum_states<'a>(
    owners: &[&'a InterfaceDef],
) -> BTreeMap<&'a str, BTreeMap<PropertyId, Option<Vec<String>>>> {
    let mut enum_cache: BTreeMap<&'a str, BTreeMap<PropertyId, Option<Vec<String>>>> =
        BTreeMap::new();
    for &owner in owners {
        let mut props: BTreeMap<PropertyId, Option<Vec<String>>> = BTreeMap::new();
        for parent_name in &owner.extends {
            let Some(parent_props) = enum_cache.get(parent_name.as_str()) else {
                continue;
            };
            for (prop, parent_vals) in parent_props {
                match (props.get_mut(prop), parent_vals) {
                    (None, vals) => {
                        props.insert(prop.clone(), vals.clone());
                    }
                    (Some(slot), None) => *slot = None,
                    (Some(Some(existing)), Some(incoming)) => {
                        existing.retain(|state| incoming.contains(state));
                    }
                    (Some(None), Some(_)) => {}
                }
            }
        }
        for member in &owner.properties {
            let own_vals = inline_enum_values(&member.property_type);
            match (props.get(&member.id), own_vals) {
                (_, None) => {
                    props.insert(member.id.clone(), None);
                }
                (Some(None), Some(_)) => {}
                (Some(Some(parent_vals)), Some(own)) => {
                    let narrowed: Vec<String> = own
                        .into_iter()
                        .filter(|state| parent_vals.contains(state))
                        .collect();
                    props.insert(member.id.clone(), Some(narrowed));
                }
                (None, Some(own)) => {
                    props.insert(member.id.clone(), Some(own));
                }
            }
        }
        enum_cache.insert(owner.name.as_str(), props);
    }
    enum_cache
}

/// Effective lifecycles of an interface, computed iteratively in topological
/// order (`owners`, parents-first) with per-call memoization so diamond graphs
/// stay linear. Returns `(resolved, conflicted_properties)`.
pub(super) fn interface_effective_lifecycles<'a>(
    interfaces: &dyn Fn(&str) -> Option<&'a InterfaceDef>,
    owners: &[&'a InterfaceDef],
    target: &'a InterfaceDef,
    cache: &mut InterfaceLifecycleCache<'a>,
) -> (BTreeMap<PropertyId, Lifecycle>, BTreeSet<PropertyId>) {
    if let Some(hit) = cache.get(target.name.as_str()) {
        return hit.clone();
    }
    let enum_cache = compute_interface_enum_states(owners);
    for &owner in owners {
        if cache.contains_key(owner.name.as_str()) {
            continue;
        }
        let owner_enums = enum_cache.get(owner.name.as_str());
        let states = |property: &PropertyId| -> Option<Vec<String>> {
            owner_enums.and_then(|map| map.get(property).and_then(Clone::clone))
        };
        let mut inherited: BTreeMap<PropertyId, Option<Lifecycle>> = BTreeMap::new();
        let mut seen_parents: BTreeSet<&'a str> = BTreeSet::new();
        for parent_name in &owner.extends {
            let Some(parent_def) = interfaces(parent_name.as_str()) else {
                continue;
            };
            if !seen_parents.insert(parent_def.name.as_str()) {
                continue;
            }
            let Some((parent_machines, parent_conflicts)) = cache.get(parent_def.name.as_str())
            else {
                continue;
            };
            merge_inherited_lifecycles(
                parent_machines.clone(),
                parent_conflicts.clone(),
                &states,
                &mut inherited,
            );
        }
        let mut machines = inherited.clone();
        apply_declared_lifecycles(&owner.lifecycles, &states, &inherited, &mut machines);
        let mut resolved = BTreeMap::new();
        let mut conflicts = BTreeSet::new();
        for (property, lifecycle) in machines {
            match lifecycle {
                Some(lc) => {
                    resolved.insert(property, lc);
                }
                None => {
                    conflicts.insert(property);
                }
            }
        }
        cache.insert(owner.name.as_str(), (resolved, conflicts));
    }
    cache.get(target.name.as_str()).cloned().unwrap_or_default()
}

/// Apply `declared` as a narrowing redeclaration over inherited machine `wide`
/// on `states`.
fn narrow_lifecycle(declared: &Lifecycle, wide: &Lifecycle, states: &[String]) -> Lifecycle {
    let mut machine = declared.restricted(states);
    machine.terminal = states
        .iter()
        .filter(|state| machine.terminal.contains(state) || wide.terminal.contains(state))
        .cloned()
        .collect();
    machine.immutable = states
        .iter()
        .filter(|state| machine.immutable.contains(state) || wide.immutable.contains(state))
        .cloned()
        .collect();
    for property in &wide.path_properties {
        if !machine.path_properties.contains(property) {
            machine.path_properties.push(property.clone());
        }
    }
    // Endings and events are inherited; redeclared events cannot widen ignored states.
    for ending in &wide.endings {
        if states.contains(&ending.state)
            && !machine.endings.iter().any(|own| own.state == ending.state)
        {
            machine.endings.push(ending.clone());
        }
    }
    for event in &wide.events {
        if let Some(own) = machine.events.iter_mut().find(|own| own.id == event.id) {
            let wide_ignored: BTreeSet<&str> = event
                .ignored_in
                .iter()
                .flat_map(|ignore| ignore.states.iter().map(String::as_str))
                .collect();
            for ignore in &mut own.ignored_in {
                ignore
                    .states
                    .retain(|state| wide_ignored.contains(state.as_str()));
            }
            own.ignored_in.retain(|ignore| !ignore.states.is_empty());
        } else {
            machine.events.push(event.clone());
        }
    }
    for transition in &mut machine.transitions {
        if let Some(parent) = wide.transitions.iter().find(|p| p.id == transition.id) {
            if transition.trigger.is_none() {
                transition.trigger.clone_from(&parent.trigger);
            }
            if transition.mode.is_none() {
                transition.mode = parent.mode;
            }
            if transition.compensated_by.is_none() {
                transition.compensated_by.clone_from(&parent.compensated_by);
            }
            if parent.guard_refusal_code.is_some() {
                transition
                    .guard_refusal_code
                    .clone_from(&parent.guard_refusal_code);
            }
            let own = std::mem::take(&mut transition.guards);
            transition.guards = parent.guards.clone();
            for guard in own {
                if !transition.guards.contains(&guard) {
                    transition.guards.push(guard);
                }
            }
        }
    }
    machine
}

/// Meet (greatest lower bound / conjunction) of two inherited lifecycles over
/// `states`. Returns `None` when the two machines conflict.
fn meet_lifecycles(left: &Lifecycle, right: &Lifecycle, states: &[String]) -> Option<Lifecycle> {
    let left = left.restricted(states);
    let right = right.restricted(states);
    let initial: Vec<String> = states
        .iter()
        .filter(|state| left.initial.contains(state) && right.initial.contains(state))
        .cloned()
        .collect();
    if initial.is_empty() {
        return None;
    }
    let terminal: Vec<String> = states
        .iter()
        .filter(|state| left.terminal.contains(state) || right.terminal.contains(state))
        .cloned()
        .collect();
    let immutable: Vec<String> = states
        .iter()
        .filter(|state| left.immutable.contains(state) || right.immutable.contains(state))
        .cloned()
        .collect();
    let mut path_properties = left.path_properties;
    for property in right.path_properties {
        if !path_properties.contains(&property) {
            path_properties.push(property);
        }
    }
    let endings = meet_endings(left.endings, right.endings, &terminal)?;
    let events = meet_events(left.events, right.events);
    let transitions = meet_transitions(&left.transitions, &right.transitions, states, &terminal)?;
    Some(Lifecycle {
        property: left.property,
        description: None,
        initial,
        terminal,
        immutable,
        transitions,
        path_properties,
        endings,
        events,
    })
}

fn meet_endings(
    mut endings: Vec<LifecycleEnding>,
    right: Vec<LifecycleEnding>,
    terminal: &[String],
) -> Option<Vec<LifecycleEnding>> {
    for ending in right {
        if let Some(existing) = endings.iter().find(|own| own.state == ending.state) {
            if existing.outcome != ending.outcome {
                return None;
            }
        } else {
            endings.push(ending);
        }
    }
    for ending in &endings {
        let is_terminal = terminal.contains(&ending.state);
        if ending.outcome.is_final() != is_terminal {
            return None;
        }
    }
    Some(endings)
}

fn meet_events(mut events: Vec<LifecycleEvent>, right: Vec<LifecycleEvent>) -> Vec<LifecycleEvent> {
    for event in right {
        if let Some(existing) = events.iter_mut().find(|own| own.id == event.id) {
            let right_ignored: BTreeSet<&str> = event
                .ignored_in
                .iter()
                .flat_map(|ignore| ignore.states.iter().map(String::as_str))
                .collect();
            for ignore in &mut existing.ignored_in {
                ignore
                    .states
                    .retain(|state| right_ignored.contains(state.as_str()));
            }
            existing
                .ignored_in
                .retain(|ignore| !ignore.states.is_empty());
        } else {
            events.push(event);
        }
    }
    events
}

fn meet_transitions(
    left: &[Transition],
    right: &[Transition],
    states: &[String],
    terminal: &[String],
) -> Option<Vec<Transition>> {
    let mut transitions = Vec::new();
    for lt in left {
        let Some(rt) = right.iter().find(|r| r.id == lt.id) else {
            continue;
        };
        if lt.to != rt.to || lt.is_fact() != rt.is_fact() {
            return None;
        }
        if lt.trigger.is_some() && rt.trigger.is_some() && lt.trigger != rt.trigger {
            return None;
        }
        if lt.compensated_by.is_some()
            && rt.compensated_by.is_some()
            && lt.compensated_by != rt.compensated_by
        {
            return None;
        }
        if lt.guard_refusal_code.is_some()
            && rt.guard_refusal_code.is_some()
            && lt.guard_refusal_code != rt.guard_refusal_code
        {
            return None;
        }
        let from: Vec<String> = states
            .iter()
            .filter(|state| {
                lt.from.contains(state) && rt.from.contains(state) && !terminal.contains(state)
            })
            .cloned()
            .collect();
        if from.is_empty() {
            continue;
        }
        let mut guards = lt.guards.clone();
        for guard in &rt.guards {
            if !guards.contains(guard) {
                guards.push(guard.clone());
            }
        }
        transitions.push(Transition {
            id: lt.id.clone(),
            description: None,
            from,
            to: lt.to.clone(),
            guards,
            guard_refusal_code: lt
                .guard_refusal_code
                .clone()
                .or_else(|| rt.guard_refusal_code.clone()),
            trigger: lt.trigger.clone().or_else(|| rt.trigger.clone()),
            mode: lt.mode.or(rt.mode),
            compensated_by: lt
                .compensated_by
                .clone()
                .or_else(|| rt.compensated_by.clone()),
        });
    }
    if transitions.iter().any(|transition| {
        transition.compensated_by.as_ref().is_some_and(|comp| {
            comp == &transition.id || !transitions.iter().any(|other| &other.id == comp)
        })
    }) {
        return None;
    }
    Some(transitions)
}
