//! Interface lifecycle resolution, non-weakening meet, and inheritance
//! checking (`INTERFACE-LIFECYCLE-1`).

use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use norn_ir::{
    InterfaceDef, InterfaceName, Lifecycle, ObjectTypeDef, ObjectTypeName, PropertyId,
    PropertyType, TransitionMode,
};

use super::super::{Checker, Table};
use super::{States, states};
use crate::lifecycles::{Edge, Machine, MachineEvent, Machines, codes};
use crate::property_types::{TypeResolution, error, span_at};

fn edge_is_fact(edge: &Edge<'_>) -> bool {
    edge.trigger.is_some() || edge.mode == Some(TransitionMode::Fact)
}

/// Conjoin inherited machine `wide` into redeclared `machine` (guards,
/// immutable states, path properties, endings, events, triggers, modes,
/// compensations, and guard diagnostics).
pub(super) fn conjoin_machine<'a>(machine: &mut Machine<'a>, wide: &Machine<'a>) {
    machine.terminal.extend(
        wide.terminal
            .iter()
            .copied()
            .filter(|state| machine.states.contains(state)),
    );
    machine.immutable.extend(
        wide.immutable
            .iter()
            .copied()
            .filter(|state| machine.states.contains(state)),
    );
    for property in &wide.path_properties {
        if !machine.path_properties.contains(property) {
            machine.path_properties.push(property);
        }
    }
    for (state, outcome) in &wide.endings {
        if machine.states.contains(state) && !machine.endings.iter().any(|(own, _)| own == state) {
            machine.endings.push((state, *outcome));
        }
    }
    for event in &wide.events {
        if let Some(own) = machine.events.iter_mut().find(|own| own.id == event.id) {
            let wide_ignored: BTreeSet<&str> = event
                .ignored_in
                .iter()
                .flat_map(|(states, _)| states.iter().copied())
                .collect();
            for (states, _) in &mut own.ignored_in {
                states.retain(|state| wide_ignored.contains(state));
            }
            own.ignored_in.retain(|(states, _)| !states.is_empty());
        } else {
            machine.events.push(event.clone());
        }
    }
    for edge in &mut machine.transitions {
        if let Some(parent) = wide.transitions.iter().find(|p| p.id == edge.id) {
            edge.trigger = edge.trigger.or(parent.trigger);
            edge.mode = edge.mode.or(parent.mode);
            edge.compensated_by = edge.compensated_by.or(parent.compensated_by);
            if parent.guard_refusal_code.is_some() {
                edge.guard_refusal_code = parent.guard_refusal_code;
            }
            let own = std::mem::take(&mut edge.guards);
            edge.guards = parent.guards.clone();
            for guard in own {
                if !edge.guards.contains(&guard) {
                    edge.guards.push(guard);
                }
            }
        }
    }
}

/// Meet (greatest lower bound / conjunction) of two inherited machines over
/// the same `states`. Returns `Err(reason)` on incompatible definitions so
/// callers fail closed deterministically.
fn meet_machines<'a>(left: &Machine<'a>, right: &Machine<'a>) -> Result<Machine<'a>, String> {
    let initial: BTreeSet<&'a str> = left.initial.intersection(&right.initial).copied().collect();
    if initial.is_empty() {
        return Err("lifecycles have no common initial state".to_owned());
    }
    let terminal: BTreeSet<&'a str> = left.terminal.union(&right.terminal).copied().collect();
    let immutable: BTreeSet<&'a str> = left.immutable.union(&right.immutable).copied().collect();
    let mut path_properties = left.path_properties.clone();
    for property in &right.path_properties {
        if !path_properties.contains(property) {
            path_properties.push(property);
        }
    }
    let endings = meet_machine_endings(&left.endings, &right.endings, &terminal)?;
    let events = meet_machine_events(&left.events, &right.events);
    let transitions = meet_machine_edges(&left.transitions, &right.transitions, &terminal)?;
    Ok(Machine {
        states: left.states.clone(),
        initial,
        terminal,
        immutable,
        transitions,
        path_properties,
        endings,
        events,
    })
}

fn meet_machine_endings<'a>(
    left: &[(&'a str, norn_ir::LifecycleOutcome)],
    right: &[(&'a str, norn_ir::LifecycleOutcome)],
    terminal: &BTreeSet<&'a str>,
) -> Result<Vec<(&'a str, norn_ir::LifecycleOutcome)>, String> {
    let mut endings = left.to_vec();
    for &(state, outcome) in right {
        if let Some((_, existing)) = endings.iter().find(|(own, _)| *own == state) {
            if *existing != outcome {
                return Err(format!(
                    "ending {state:?} has conflicting outcomes ({existing:?} vs {outcome:?})"
                ));
            }
        } else {
            endings.push((state, outcome));
        }
    }
    for &(state, outcome) in &endings {
        let is_terminal = terminal.contains(state);
        if outcome.is_final() != is_terminal {
            return Err(format!(
                "state {state:?} conflicts between terminal and resting outcomes"
            ));
        }
    }
    Ok(endings)
}

fn meet_machine_events<'a>(
    left: &[MachineEvent<'a>],
    right: &[MachineEvent<'a>],
) -> Vec<MachineEvent<'a>> {
    let mut events = left.to_vec();
    for event in right {
        if let Some(existing) = events.iter_mut().find(|own| own.id == event.id) {
            let right_ignored: BTreeSet<&str> = event
                .ignored_in
                .iter()
                .flat_map(|(states, _)| states.iter().copied())
                .collect();
            for (states, _) in &mut existing.ignored_in {
                states.retain(|state| right_ignored.contains(state));
            }
            existing.ignored_in.retain(|(states, _)| !states.is_empty());
        } else {
            events.push(event.clone());
        }
    }
    events
}

fn meet_machine_edges<'a>(
    left: &[Edge<'a>],
    right: &[Edge<'a>],
    terminal: &BTreeSet<&'a str>,
) -> Result<Vec<Edge<'a>>, String> {
    let mut transitions = Vec::new();
    for lt in left {
        let Some(rt) = right.iter().find(|r| r.id == lt.id) else {
            continue;
        };
        let id = lt.id.as_str();
        if lt.to != rt.to {
            return Err(format!(
                "transition {id:?} targets {:?} in one lifecycle and {:?} in another",
                lt.to, rt.to
            ));
        }
        if edge_is_fact(lt) != edge_is_fact(rt) {
            return Err(format!(
                "transition {id:?} conflicts between fact and request modes"
            ));
        }
        if lt.trigger.is_some() && rt.trigger.is_some() && lt.trigger != rt.trigger {
            return Err(format!("transition {id:?} declares conflicting triggers"));
        }
        if lt.compensated_by.is_some()
            && rt.compensated_by.is_some()
            && lt.compensated_by != rt.compensated_by
        {
            return Err(format!(
                "transition {id:?} declares conflicting compensations"
            ));
        }
        if lt.guard_refusal_code.is_some()
            && rt.guard_refusal_code.is_some()
            && lt.guard_refusal_code != rt.guard_refusal_code
        {
            return Err(format!(
                "transition {id:?} declares conflicting guard diagnostics"
            ));
        }
        let from: BTreeSet<&'a str> = lt
            .from
            .intersection(&rt.from)
            .copied()
            .filter(|state| !terminal.contains(state))
            .collect();
        if from.is_empty() {
            continue;
        }
        let mut guards = lt.guards.clone();
        for &guard in &rt.guards {
            if !guards.contains(&guard) {
                guards.push(guard);
            }
        }
        transitions.push(Edge {
            id: lt.id,
            from,
            to: lt.to,
            guards,
            guard_refusal_code: lt.guard_refusal_code.or(rt.guard_refusal_code),
            trigger: lt.trigger.or(rt.trigger),
            mode: lt.mode.or(rt.mode),
            compensated_by: lt.compensated_by.or(rt.compensated_by),
        });
    }
    for edge in &transitions {
        if let Some(comp) = edge.compensated_by
            && (comp == edge.id || !transitions.iter().any(|other| other.id == comp))
        {
            return Err(format!(
                "transition {:?} references compensating transition {:?} which does not survive the meet",
                edge.id.as_str(),
                comp.as_str()
            ));
        }
    }
    Ok(transitions)
}

/// Summary of machines inherited by an interface from its parent interfaces.
struct InheritedInterfaceMachines<'a> {
    machines: Machines<'a>,
    conflicts: BTreeSet<&'a PropertyId>,
    new_conflicts: Vec<(&'a PropertyId, String)>,
    source_counts: BTreeMap<&'a PropertyId, usize>,
    wide_states: BTreeMap<&'a PropertyId, BTreeSet<&'a str>>,
}

/// Summary of machines inherited by an object type from its supertype and
/// newly implemented interfaces.
pub(super) struct InheritedObjectMachines<'a> {
    pub(super) machines: Machines<'a>,
    pub(super) conflicts: BTreeSet<&'a PropertyId>,
    /// Newly introduced conflicts on `def`, as `(implements_position, property, reason)`.
    pub(super) new_conflicts: Vec<(usize, &'a PropertyId, String)>,
    /// Properties inherited from `parent`.
    pub(super) from_parent: BTreeSet<&'a PropertyId>,
    /// Properties inherited from a newly implemented interface:
    /// `(first_implements_pos, wide_states_before_restriction)`.
    pub(super) from_iface: BTreeMap<&'a PropertyId, (usize, BTreeSet<&'a str>)>,
    /// Number of distinct sources (supertype / direct interfaces) governing each property.
    pub(super) source_counts: BTreeMap<&'a PropertyId, usize>,
    /// Directly implemented interfaces not already implemented by an ancestor object type.
    pub(super) direct_new_interfaces: Vec<(usize, &'a InterfaceDef)>,
}

impl<'a> Checker<'a, '_> {
    /// Effective enum states and nullability of `property` on interface `def`
    /// (across `def` and its parent interfaces). Returns `None` if `property`
    /// is not required by `def`.
    fn interface_property_states(
        &self,
        def: &'a InterfaceDef,
        property: &PropertyId,
    ) -> Option<(States<'a>, bool)> {
        let reqs: Vec<_> = self
            .requirements(def)
            .into_iter()
            .filter(|req| req.property.id == *property)
            .collect();
        if reqs.is_empty() {
            return None;
        }
        let all_nullable = reqs.iter().all(|req| req.shape().nullable);
        let mut enum_lists: Vec<Vec<&'a str>> = Vec::new();
        let mut own_list: Option<Vec<&'a str>> = None;
        for req in &reqs {
            match req.resolved.resolution {
                TypeResolution::Unresolved { .. } => {
                    return Some((States::Unresolved, all_nullable));
                }
                TypeResolution::Base(PropertyType::Enum { values }) => {
                    let list: Vec<&'a str> =
                        values.iter().map(|value| value.value.as_str()).collect();
                    if req.interface == &def.name {
                        own_list = Some(list.clone());
                    }
                    enum_lists.push(list);
                }
                TypeResolution::Base(_) | TypeResolution::Registry => {
                    return Some((States::NotEnum, all_nullable));
                }
            }
        }
        let base = own_list.or_else(|| enum_lists.first().cloned())?;
        let intersected: Vec<&'a str> = base
            .into_iter()
            .filter(|state| enum_lists.iter().all(|list| list.contains(state)))
            .collect();
        Some((States::Enum(intersected), all_nullable))
    }

    /// Effective machines of a registered interface (memoized), plus the set
    /// of properties whose inherited parent lifecycles conflicted.
    pub(in crate::entity_model) fn interface_machines(
        &mut self,
        name: &'a InterfaceName,
    ) -> (Rc<Machines<'a>>, BTreeSet<&'a PropertyId>) {
        self.interface_machines_inner(name, &mut BTreeSet::new())
    }

    fn interface_machines_inner(
        &mut self,
        name: &'a InterfaceName,
        visiting: &mut BTreeSet<&'a InterfaceName>,
    ) -> (Rc<Machines<'a>>, BTreeSet<&'a PropertyId>) {
        if let Some(machines) = self.interface_machines.get(name) {
            let conflicts = self
                .interface_machine_conflicts
                .get(name)
                .cloned()
                .unwrap_or_default();
            return (Rc::clone(machines), conflicts);
        }
        let Some(entry) = self.interfaces.get(name) else {
            return (Rc::default(), BTreeSet::new());
        };
        let def = entry.def;
        if !visiting.insert(name) {
            return (Rc::default(), BTreeSet::new());
        }
        let inherited = self.inherited_interface_machines(def, visiting);
        let mut machines = inherited.machines.clone();
        for lifecycle in &def.lifecycles {
            if inherited.conflicts.contains(&lifecycle.property) {
                continue;
            }
            if let Some((States::Enum(states), _)) =
                self.interface_property_states(def, &lifecycle.property)
            {
                let mut machine = Machine::declared(lifecycle, states);
                if let Some(wide) = inherited.machines.get(&lifecycle.property) {
                    conjoin_machine(&mut machine, wide);
                }
                machines.insert(&lifecycle.property, machine);
            }
        }
        visiting.remove(name);
        let machines = Rc::new(machines);
        // Always memoize finished frames (even when a descendant hit a cycle,
        // which is already rejected by cycle detection) so diamond lattices
        // stacked above a cycle remain O(V + E) linear.
        self.interface_machines.insert(name, Rc::clone(&machines));
        self.interface_machine_conflicts
            .insert(name, inherited.conflicts.clone());
        (machines, inherited.conflicts)
    }

    /// Fold parent interface machines for `def`.
    fn inherited_interface_machines(
        &mut self,
        def: &'a InterfaceDef,
        visiting: &mut BTreeSet<&'a InterfaceName>,
    ) -> InheritedInterfaceMachines<'a> {
        let mut out = InheritedInterfaceMachines {
            machines: Machines::new(),
            conflicts: BTreeSet::new(),
            new_conflicts: Vec::new(),
            source_counts: BTreeMap::new(),
            wide_states: BTreeMap::new(),
        };
        let mut seen_parents: BTreeSet<&'a InterfaceName> = BTreeSet::new();
        for parent_name in &def.extends {
            let Some((parent_key, _)) = self.interfaces.get_key_value(parent_name) else {
                continue;
            };
            let parent_key = *parent_key;
            if !seen_parents.insert(parent_key) {
                continue;
            }
            let (parent_machines, parent_conflicts) =
                self.interface_machines_inner(parent_key, visiting);
            for prop in parent_conflicts {
                out.conflicts.insert(prop);
                out.machines.remove(prop);
            }
            for (&property, parent_machine) in parent_machines.iter() {
                if out.conflicts.contains(property) {
                    continue;
                }
                let Some((States::Enum(states), _)) = self.interface_property_states(def, property)
                else {
                    continue;
                };
                let count = out.source_counts.entry(property).or_default();
                *count = count.saturating_add(1);
                out.wide_states
                    .entry(property)
                    .or_insert_with(|| parent_machine.states.iter().copied().collect());
                let restricted = parent_machine.restricted(states);
                if let Some(existing) = out.machines.get(property) {
                    match meet_machines(existing, &restricted) {
                        Ok(met) => {
                            out.machines.insert(property, met);
                        }
                        Err(reason) => {
                            out.machines.remove(property);
                            out.conflicts.insert(property);
                            out.new_conflicts.push((property, reason));
                        }
                    }
                } else {
                    out.machines.insert(property, restricted);
                }
            }
        }
        out
    }

    /// Check lifecycles declared on an interface and those inherited from its
    /// parent interfaces (`INTERFACE-LIFECYCLE-1`).
    pub(in crate::entity_model) fn check_interface_lifecycles(
        &mut self,
        base: &[&str],
        def: &'a InterfaceDef,
    ) {
        let inherited = self.inherited_interface_machines(def, &mut BTreeSet::new());
        for (property, reason) in &inherited.new_conflicts {
            self.out.push(error(
                crate::interfaces::codes::INVALID_INHERITANCE,
                format!(
                    "parent interfaces declare conflicting lifecycles for property {:?}: {reason}",
                    property.as_str()
                ),
                span_at(base, &["extends"]),
            ));
        }
        let mut declared = BTreeSet::new();
        for (position, lifecycle) in def.lifecycles.iter().enumerate() {
            let position = position.to_string();
            let at = [base, &["lifecycles", position.as_str()]].concat();
            self.check_guards(&at, lifecycle);
            if !declared.insert(&lifecycle.property) {
                self.out.push(error(
                    codes::DUPLICATE_LIFECYCLE,
                    format!(
                        "property {:?} already has a lifecycle on this interface",
                        lifecycle.property.as_str()
                    ),
                    span_at(&at, &["property"]),
                ));
                continue;
            }
            let Some(states) = self.interface_lifecycle_states(&at, def, lifecycle) else {
                continue;
            };
            let clean = self.check_declaration(&at, lifecycle, &states);
            if clean {
                let findings = crate::lifecycle_soundness::check_declaration(lifecycle, &states);
                self.soundness(&at, findings);
            }
            let machine = Machine::declared(lifecycle, states);
            if clean {
                let (effective, _) = self.interface_machines(&def.name);
                let effective_machine = effective.get(&lifecycle.property).unwrap_or(&machine);
                let findings = crate::lifecycle_soundness::check_effective_transitions(
                    lifecycle,
                    effective_machine,
                );
                self.soundness(&at, findings);
                self.check_structure(&at, effective_machine);
                if effective.contains_key(&lifecycle.property) {
                    self.check_path_properties(&at, effective_machine);
                }
            }
            if !inherited.conflicts.contains(&lifecycle.property)
                && let Some(wide) = inherited.machines.get(&lifecycle.property)
            {
                self.check_narrowing(&at, lifecycle, wide);
            }
        }
        self.check_interface_unredeclared(base, def, &inherited, &declared);
    }

    fn check_interface_unredeclared(
        &mut self,
        base: &[&str],
        def: &'a InterfaceDef,
        inherited: &InheritedInterfaceMachines<'a>,
        declared: &BTreeSet<&'a PropertyId>,
    ) {
        for (&property, machine) in &inherited.machines {
            if declared.contains(property) {
                continue;
            }
            let own_pos = def.properties.iter().position(|p| p.id == *property);
            let combined = inherited.source_counts.get(property).copied().unwrap_or(0) >= 2;
            let narrowed = inherited.wide_states.get(property).is_some_and(|wide| {
                let effective_states: BTreeSet<&str> = machine.states.iter().copied().collect();
                effective_states != *wide
            });
            if own_pos.is_none() && !combined && !narrowed {
                continue;
            }
            let pos_str;
            let at = if let Some(pos) = own_pos {
                pos_str = pos.to_string();
                [base, &["properties", pos_str.as_str()]].concat()
            } else {
                [base, &["extends"]].concat()
            };
            if machine.initial.is_empty() {
                self.out.push(error(
                    codes::EMPTY_INITIAL,
                    format!(
                        "narrowing {:?} removes every initial state of the inherited lifecycle",
                        property.as_str()
                    ),
                    span_at(&at, &[]),
                ));
                continue;
            }
            let findings = crate::lifecycle_soundness::check_inherited_compensation(machine);
            self.soundness(&at, findings);
            self.check_structure(&at, machine);
            self.check_path_properties(&at, machine);
        }
    }

    fn interface_lifecycle_states(
        &mut self,
        at: &[&str],
        def: &'a InterfaceDef,
        lifecycle: &Lifecycle,
    ) -> Option<Vec<&'a str>> {
        let id = lifecycle.property.as_str();
        let Some((resolved_states, nullable)) =
            self.interface_property_states(def, &lifecycle.property)
        else {
            self.out.push(error(
                codes::UNKNOWN_PROPERTY,
                format!("lifecycle property {id:?} is not a property of the interface"),
                span_at(at, &["property"]),
            ));
            return None;
        };
        let states = match resolved_states {
            States::Enum(states) => states,
            States::NotEnum => {
                self.out.push(error(
                    codes::PROPERTY_NOT_ENUM,
                    format!(
                        "lifecycle property {id:?} must be a local enum; its values are the states"
                    ),
                    span_at(at, &["property"]),
                ));
                return None;
            }
            States::Unresolved => return None,
        };
        if nullable {
            self.out.push(error(
                codes::PROPERTY_NULLABLE,
                format!("lifecycle property {id:?} is nullable; every object is in a state"),
                span_at(at, &["property"]),
            ));
        }
        Some(states)
    }

    /// Gather all machines inherited by `def` from `parent` and newly
    /// implemented interfaces, restricted to `table`'s enum states.
    pub(super) fn inherited_object_machines(
        &mut self,
        def: &'a ObjectTypeDef,
        parent: Option<&'a ObjectTypeName>,
        ancestors: &[&'a ObjectTypeName],
        table: &Table<'a>,
    ) -> InheritedObjectMachines<'a> {
        let mut out = InheritedObjectMachines {
            machines: Machines::new(),
            conflicts: BTreeSet::new(),
            new_conflicts: Vec::new(),
            from_parent: BTreeSet::new(),
            from_iface: BTreeMap::new(),
            source_counts: BTreeMap::new(),
            direct_new_interfaces: self.direct_new_interfaces(def, ancestors),
        };
        if let Some(parent_name) = parent {
            self.fold_parent_machines(parent_name, table, &mut out);
        }
        let direct = out.direct_new_interfaces.clone();
        for (pos, iface_def) in direct {
            self.fold_interface_machines(pos, iface_def, table, &mut out);
        }
        out
    }

    fn direct_new_interfaces(
        &self,
        def: &'a ObjectTypeDef,
        ancestors: &[&'a ObjectTypeName],
    ) -> Vec<(usize, &'a InterfaceDef)> {
        let ancestor_interfaces: BTreeSet<&'a InterfaceName> = ancestors
            .iter()
            .filter_map(|ancestor| self.types.get(ancestor))
            .flat_map(|entry| entry.def.implements.iter())
            .filter_map(|name| self.interfaces.get(name).map(|entry| entry.def))
            .flat_map(|iface| self.interface_lineage(iface))
            .map(|iface| &iface.name)
            .collect();
        let mut seen_direct: BTreeSet<&'a InterfaceName> = BTreeSet::new();
        let mut direct_new_interfaces = Vec::new();
        for (pos, iface_name) in def.implements.iter().enumerate() {
            if ancestor_interfaces.contains(iface_name) {
                continue;
            }
            let Some(entry) = self.interfaces.get(iface_name) else {
                continue;
            };
            if seen_direct.insert(&entry.def.name) {
                direct_new_interfaces.push((pos, entry.def));
            }
        }
        direct_new_interfaces
    }

    fn fold_parent_machines(
        &mut self,
        parent_name: &'a ObjectTypeName,
        table: &Table<'a>,
        out: &mut InheritedObjectMachines<'a>,
    ) {
        let parent_machines = self.machines(parent_name);
        if let Some(parent_conflicts) = self.machine_conflicts.get(parent_name) {
            out.conflicts.extend(parent_conflicts.iter().copied());
        }
        for (&property, machine) in parent_machines.iter() {
            if out.conflicts.contains(property) {
                continue;
            }
            if let Some(member) = table.get(property)
                && !member.property.is_read_only()
                && let States::Enum(states) = states(member)
            {
                out.machines.insert(property, machine.restricted(states));
                out.from_parent.insert(property);
                let count = out.source_counts.entry(property).or_default();
                *count = count.saturating_add(1);
            }
        }
    }

    fn fold_interface_machines(
        &mut self,
        pos: usize,
        iface_def: &'a InterfaceDef,
        table: &Table<'a>,
        out: &mut InheritedObjectMachines<'a>,
    ) {
        let (iface_machines, iface_conflicts) = self.interface_machines(&iface_def.name);
        for prop in iface_conflicts {
            out.conflicts.insert(prop);
            out.machines.remove(prop);
        }
        for (&property, iface_machine) in iface_machines.iter() {
            if out.conflicts.contains(property) {
                continue;
            }
            let Some(member) = table.get(property) else {
                continue;
            };
            if member.property.is_read_only() {
                continue;
            }
            let States::Enum(states) = states(member) else {
                continue;
            };
            let count = out.source_counts.entry(property).or_default();
            *count = count.saturating_add(1);
            out.from_iface
                .entry(property)
                .or_insert_with(|| (pos, iface_machine.states.iter().copied().collect()));
            let restricted = iface_machine.restricted(states);
            if let Some(existing) = out.machines.get(property) {
                match meet_machines(existing, &restricted) {
                    Ok(met) => {
                        out.machines.insert(property, met);
                    }
                    Err(reason) => {
                        out.machines.remove(property);
                        out.conflicts.insert(property);
                        out.new_conflicts.push((pos, property, reason));
                    }
                }
            } else {
                out.machines.insert(property, restricted);
            }
        }
    }
}
