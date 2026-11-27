//! Effective lifecycle snapshots and inheritance meet/conjoin logic for
//! [`super`] (`INTERFACE-LIFECYCLE-1`).

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use super::{EntityModelSnapshot, enum_values, strings};

impl EntityModelSnapshot {
    /// Owners of an interface contract in topological order (parents first),
    /// iteratively in O(V + E) time without recursion.
    fn interface_owners_topological<'a>(&'a self, root: &'a str) -> Vec<&'a str> {
        let mut pending = vec![(root, false)];
        let mut seen = BTreeSet::new();
        let mut owners = Vec::new();
        while let Some((name, exiting)) = pending.pop() {
            if exiting {
                owners.push(name);
            } else if seen.insert(name)
                && let Some(interface) = self.interfaces.get(name)
            {
                pending.push((name, true));
                for parent in interface.extends.iter().rev() {
                    pending.push((parent.as_str(), false));
                }
            }
        }
        owners
    }

    fn interface_effective_lifecycles(
        &self,
        name: &str,
    ) -> (BTreeMap<String, LifecycleSnapshot>, BTreeSet<String>) {
        let owners = self.interface_owners_topological(name);
        if owners.iter().all(|owner| {
            self.interfaces
                .get(*owner)
                .is_none_or(|iface| iface.lifecycles.is_empty())
        }) {
            return (BTreeMap::new(), BTreeSet::new());
        }
        let mut enum_cache: BTreeMap<&str, BTreeMap<String, BTreeSet<String>>> = BTreeMap::new();
        let mut cache: BTreeMap<&str, (BTreeMap<String, LifecycleSnapshot>, BTreeSet<String>)> =
            BTreeMap::new();
        for &owner in &owners {
            let Some(interface) = self.interfaces.get(owner) else {
                continue;
            };
            let mut states: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
            for parent in &interface.extends {
                if let Some(parent_states) = enum_cache.get(parent.as_str()) {
                    for (property, vals) in parent_states {
                        states
                            .entry(property.clone())
                            .and_modify(|existing| existing.retain(|s| vals.contains(s)))
                            .or_insert_with(|| vals.clone());
                    }
                }
            }
            for (property, value) in &interface.properties {
                let own = enum_values(value);
                if own.is_empty() {
                    states.remove(property);
                } else {
                    states
                        .entry(property.clone())
                        .and_modify(|existing| existing.retain(|s| own.contains(s)))
                        .or_insert(own);
                }
            }
            let mut machines: BTreeMap<String, LifecycleSnapshot> = BTreeMap::new();
            let mut conflicts: BTreeSet<String> = BTreeSet::new();
            let mut seen_parents = BTreeSet::new();
            for parent in &interface.extends {
                if !seen_parents.insert(parent.as_str()) {
                    continue;
                }
                let Some((parent_machines, parent_conflicts)) = cache.get(parent.as_str()) else {
                    continue;
                };
                for prop in parent_conflicts {
                    conflicts.insert(prop.clone());
                    machines.remove(prop);
                }
                for (property, parent_machine) in parent_machines {
                    if conflicts.contains(property) {
                        continue;
                    }
                    let Some(property_states) = states.get(property) else {
                        continue;
                    };
                    let mut restricted = parent_machine.clone();
                    restricted.restrict(property_states);
                    if let Some(existing) = machines.get_mut(property) {
                        if !existing.meet(&restricted) {
                            machines.remove(property);
                            conflicts.insert(property.clone());
                        }
                    } else {
                        machines.insert(property.clone(), restricted);
                    }
                }
            }
            for (property, value) in &interface.lifecycles {
                if conflicts.contains(property) {
                    continue;
                }
                let property_states = states.get(property).cloned().unwrap_or_default();
                let mut declared = LifecycleSnapshot::from_value(value, &property_states);
                if let Some(inherited) = machines.get(property) {
                    declared.conjoin(inherited);
                }
                machines.insert(property.clone(), declared);
            }
            enum_cache.insert(owner, states);
            cache.insert(owner, (machines, conflicts));
        }
        cache.remove(name).unwrap_or_default()
    }

    fn ancestor_interfaces(&self, name: &str, seen: &mut BTreeSet<String>) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        let Some(definition) = self.object_types.get(name) else {
            return out;
        };
        let Some(parent) = definition.extends.as_deref() else {
            return out;
        };
        let mut current = Some(parent);
        while let Some(ancestor) = current {
            if !seen.insert(ancestor.to_owned()) {
                break;
            }
            let Some(ancestor_def) = self.object_types.get(ancestor) else {
                break;
            };
            let mut pending: Vec<String> = ancestor_def.implements.clone();
            while let Some(iface_name) = pending.pop() {
                if out.insert(iface_name.clone())
                    && let Some(iface) = self.interfaces.get(&iface_name)
                {
                    pending.extend(iface.extends.iter().cloned());
                }
            }
            current = ancestor_def.extends.as_deref();
        }
        out
    }

    pub(super) fn effective_lifecycles(
        &self,
        name: &str,
        seen: &mut BTreeSet<String>,
    ) -> (BTreeMap<String, LifecycleSnapshot>, BTreeSet<String>) {
        if !seen.insert(name.to_owned()) {
            return (BTreeMap::new(), BTreeSet::new());
        }
        let Some(definition) = self.object_types.get(name) else {
            seen.remove(name);
            return (BTreeMap::new(), BTreeSet::new());
        };
        let properties = self.effective_properties(name, &mut BTreeSet::new());
        let states = properties
            .iter()
            .filter(|(_, value)| value.get("computed").is_none())
            .map(|(property, value)| (property.clone(), enum_values(value)))
            .collect::<BTreeMap<_, _>>();
        let (mut machines, mut conflicts) = definition
            .extends
            .as_deref()
            .map_or_default(|parent| self.effective_lifecycles(parent, seen));
        machines.retain(|property, _machine| states.contains_key(property));
        for (property, machine) in &mut machines {
            if let Some(property_states) = states.get(property) {
                machine.restrict(property_states);
            }
        }
        let inherited_interfaces = self.ancestor_interfaces(name, &mut BTreeSet::new());
        let mut seen_direct = BTreeSet::new();
        for interface_name in &definition.implements {
            if inherited_interfaces.contains(interface_name)
                || !seen_direct.insert(interface_name.as_str())
            {
                continue;
            }
            let (iface_machines, iface_conflicts) =
                self.interface_effective_lifecycles(interface_name);
            for prop in iface_conflicts {
                conflicts.insert(prop.clone());
                machines.remove(&prop);
            }
            for (property, mut iface_machine) in iface_machines {
                if conflicts.contains(&property) {
                    continue;
                }
                let Some(property_states) = states.get(&property) else {
                    continue;
                };
                iface_machine.restrict(property_states);
                if let Some(existing) = machines.get_mut(&property) {
                    if !existing.meet(&iface_machine) {
                        machines.remove(&property);
                        conflicts.insert(property);
                    }
                } else {
                    machines.insert(property, iface_machine);
                }
            }
        }
        for (property, value) in &definition.lifecycles {
            if conflicts.contains(property) || !states.contains_key(property) {
                continue;
            }
            let property_states = states.get(property).cloned().unwrap_or_default();
            let mut declared = LifecycleSnapshot::from_value(value, &property_states);
            if let Some(inherited) = machines.get(property) {
                declared.conjoin(inherited);
            }
            machines.insert(property.clone(), declared);
        }
        seen.remove(name);
        (machines, conflicts)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct TransitionSnapshot {
    pub(super) from: BTreeSet<String>,
    pub(super) to: String,
    pub(super) guards: Vec<String>,
    pub(super) mode: Option<String>,
    pub(super) trigger: Option<Value>,
    pub(super) compensated_by: Option<String>,
    pub(super) guard_refusal_code: Option<String>,
}

impl TransitionSnapshot {
    fn is_fact(&self) -> bool {
        self.trigger.is_some() || self.mode.as_deref() == Some("fact")
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct LifecycleSnapshot {
    pub(super) states: BTreeSet<String>,
    pub(super) initial: BTreeSet<String>,
    pub(super) terminal: BTreeSet<String>,
    pub(super) immutable: BTreeSet<String>,
    pub(super) endings: BTreeMap<String, String>,
    pub(super) transitions: BTreeMap<String, TransitionSnapshot>,
}

impl LifecycleSnapshot {
    fn from_value(value: &Value, states: &BTreeSet<String>) -> Self {
        let transitions = value
            .get("transitions")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|transition| {
                let id = transition.get("id")?.as_str()?.to_owned();
                Some((
                    id,
                    TransitionSnapshot {
                        from: strings(transition.get("from")).into_iter().collect(),
                        to: transition
                            .get("to")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned(),
                        guards: strings(transition.get("guards")),
                        mode: transition
                            .get("mode")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        trigger: transition.get("trigger").cloned(),
                        compensated_by: transition
                            .get("compensatedBy")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                        guard_refusal_code: transition
                            .get("guardRefusalCode")
                            .and_then(Value::as_str)
                            .map(str::to_owned),
                    },
                ))
            })
            .collect();
        let endings = value
            .get("endings")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|ending| {
                let state = ending.get("state")?.as_str()?.to_owned();
                let outcome = ending.get("outcome")?.as_str()?.to_owned();
                Some((state, outcome))
            })
            .collect();
        let mut machine = Self {
            states: states.clone(),
            initial: strings(value.get("initial")).into_iter().collect(),
            terminal: strings(value.get("terminal")).into_iter().collect(),
            immutable: strings(value.get("immutable")).into_iter().collect(),
            endings,
            transitions,
        };
        machine.restrict(states);
        machine
    }

    fn restrict(&mut self, states: &BTreeSet<String>) {
        self.states.clone_from(states);
        self.initial.retain(|state| states.contains(state));
        self.terminal.retain(|state| states.contains(state));
        self.immutable.retain(|state| states.contains(state));
        self.endings.retain(|state, _| states.contains(state));
        self.transitions.retain(|_, transition| {
            transition.from.retain(|state| states.contains(state));
            states.contains(&transition.to) && !transition.from.is_empty()
        });
    }

    fn conjoin(&mut self, inherited: &Self) {
        self.terminal.extend(
            inherited
                .terminal
                .iter()
                .filter(|state| self.states.contains(*state))
                .cloned(),
        );
        self.immutable.extend(
            inherited
                .immutable
                .iter()
                .filter(|state| self.states.contains(*state))
                .cloned(),
        );
        for (state, outcome) in &inherited.endings {
            if self.states.contains(state) {
                self.endings
                    .entry(state.clone())
                    .or_insert_with(|| outcome.clone());
            }
        }
        for (id, transition) in &mut self.transitions {
            if let Some(parent) = inherited.transitions.get(id) {
                if transition.mode.is_none() {
                    transition.mode.clone_from(&parent.mode);
                }
                if transition.trigger.is_none() {
                    transition.trigger.clone_from(&parent.trigger);
                }
                if transition.compensated_by.is_none() {
                    transition.compensated_by.clone_from(&parent.compensated_by);
                }
                if parent.guard_refusal_code.is_some() {
                    transition
                        .guard_refusal_code
                        .clone_from(&parent.guard_refusal_code);
                }
                transition.guards.extend(parent.guards.iter().cloned());
                transition.guards.sort();
                transition.guards.dedup();
            }
        }
    }

    fn meet(&mut self, other: &Self) -> bool {
        self.initial.retain(|state| other.initial.contains(state));
        if self.initial.is_empty() {
            return false;
        }
        self.terminal.extend(other.terminal.iter().cloned());
        self.immutable.extend(other.immutable.iter().cloned());
        for (state, outcome) in &other.endings {
            if let Some(existing) = self.endings.get(state) {
                if existing != outcome {
                    return false;
                }
            } else {
                self.endings.insert(state.clone(), outcome.clone());
            }
        }
        for (state, outcome) in &self.endings {
            let is_final = outcome != "resting";
            if is_final != self.terminal.contains(state) {
                return false;
            }
        }
        let mut conflict = false;
        let terminal = &self.terminal;
        self.transitions.retain(|id, transition| {
            let Some(other_transition) = other.transitions.get(id) else {
                return false;
            };
            if transition.to != other_transition.to
                || transition.is_fact() != other_transition.is_fact()
                || (transition.trigger.is_some()
                    && other_transition.trigger.is_some()
                    && transition.trigger != other_transition.trigger)
                || (transition.compensated_by.is_some()
                    && other_transition.compensated_by.is_some()
                    && transition.compensated_by != other_transition.compensated_by)
                || (transition.guard_refusal_code.is_some()
                    && other_transition.guard_refusal_code.is_some()
                    && transition.guard_refusal_code != other_transition.guard_refusal_code)
            {
                conflict = true;
                return false;
            }
            transition
                .from
                .retain(|state| other_transition.from.contains(state) && !terminal.contains(state));
            if transition.mode.is_none() {
                transition.mode.clone_from(&other_transition.mode);
            }
            if transition.trigger.is_none() {
                transition.trigger.clone_from(&other_transition.trigger);
            }
            if transition.compensated_by.is_none() {
                transition
                    .compensated_by
                    .clone_from(&other_transition.compensated_by);
            }
            if transition.guard_refusal_code.is_none() {
                transition
                    .guard_refusal_code
                    .clone_from(&other_transition.guard_refusal_code);
            }
            transition
                .guards
                .extend(other_transition.guards.iter().cloned());
            transition.guards.sort();
            transition.guards.dedup();
            !transition.from.is_empty()
        });
        if conflict {
            return false;
        }
        !self.transitions.iter().any(|(id, transition)| {
            transition
                .compensated_by
                .as_ref()
                .is_some_and(|comp| comp == id || !self.transitions.contains_key(comp))
        })
    }
}
