//! Declared lifecycles of an `EntityIR` v2 model (T3-LIFE-1, #2672).
//!
//! - **The property.** A lifecycle names a stored, non-nullable property of
//!   the type (declared, inherited, or added by an extension) that resolves
//!   to a local `enum`; its values are the states. At most one lifecycle per
//!   property per type.
//! - **States.** `initial` (non-empty), `terminal`, and each transition's
//!   `from` and `to` name states, without repeats. The property's effective
//!   default, if any, is an initial state.
//! - **Transitions.** Ids follow the member grammar and are unique within the
//!   lifecycle; `from` is non-empty and excludes `to`; a `(from, to)` pair is
//!   declared once; no transition leaves a terminal state; a guard is not
//!   blank.
//! - **Structure.** Every state is reachable from an initial state, and
//!   every non-terminal state has an outgoing transition.
//! - **Narrowing.** A subtype inherits its supertype's machines, restricted
//!   to the states its (possibly narrowed) enum keeps; the restricted machine
//!   is re-checked for structure where the subtype narrows the property. A
//!   redeclared lifecycle may only keep inherited transitions (by id, same
//!   target, a subset of the sources, every inherited guard restated
//!   verbatim) and inherited initial states; guards conjoin, and an appended
//!   guard only narrows. Dropping or replacing an inherited guard is
//!   `drops_guard`. Tenant
//!   types are subtypes, so a tenant never widens a base lifecycle.

use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use norn_ir::{
    InterfaceDef, InterfaceName, JsonValue, Lifecycle, ObjectTypeDef, ObjectTypeName, PropertyId,
    PropertyType, Transition, TransitionMode,
};

use super::{Checker, Table};
use crate::lifecycles::{Edge, Machine, MachineEvent, Machines, codes};
use crate::object_types::{MEMBER_ID_GRAMMAR, Resolved, check_zen_syntax};
use crate::property_types::{TypeResolution, error, matches_grammar, span_at};

/// The states of a lifecycle property, or why it cannot hold a lifecycle.
enum States<'a> {
    Enum(Vec<&'a str>),
    /// Not a local enum (reported as `property_not_enum`).
    NotEnum,
    /// The type does not resolve; reported where it is declared.
    Unresolved,
}

fn states<'a>(member: &Resolved<'a>) -> States<'a> {
    match member.ty.resolution {
        TypeResolution::Base(PropertyType::Enum { values }) => {
            States::Enum(values.iter().map(|value| value.value.as_str()).collect())
        }
        TypeResolution::Base(_) | TypeResolution::Registry => States::NotEnum,
        TypeResolution::Unresolved { .. } => States::Unresolved,
    }
}

/// The member's effective default, when it is a string.
fn default_state<'a>(member: &Resolved<'a>) -> Option<&'a str> {
    match member.default.map(|default| &default.0) {
        Some(JsonValue::String(value)) => Some(value.as_str()),
        _ => None,
    }
}

fn edge_is_fact(edge: &Edge<'_>) -> bool {
    edge.trigger.is_some() || edge.mode == Some(TransitionMode::Fact)
}

/// Conjoin inherited machine `wide` into redeclared `machine` (guards,
/// immutable states, path properties, endings, events, triggers, modes,
/// compensations, and guard diagnostics).
fn conjoin_machine<'a>(machine: &mut Machine<'a>, wide: &Machine<'a>) {
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
struct InheritedObjectMachines<'a> {
    machines: Machines<'a>,
    conflicts: BTreeSet<&'a PropertyId>,
    /// Newly introduced conflicts on `def`, as `(implements_position, property, reason)`.
    new_conflicts: Vec<(usize, &'a PropertyId, String)>,
    /// Properties inherited from `parent`.
    from_parent: BTreeSet<&'a PropertyId>,
    /// Properties inherited from a newly implemented interface:
    /// `(first_implements_pos, wide_states_before_restriction)`.
    from_iface: BTreeMap<&'a PropertyId, (usize, BTreeSet<&'a str>)>,
    /// Number of distinct sources (supertype / direct interfaces) governing each property.
    source_counts: BTreeMap<&'a PropertyId, usize>,
    /// Directly implemented interfaces not already implemented by an ancestor object type.
    direct_new_interfaces: Vec<(usize, &'a InterfaceDef)>,
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
    pub(super) fn interface_machines(
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
    pub(super) fn check_interface_lifecycles(&mut self, base: &[&str], def: &'a InterfaceDef) {
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

    /// The effective machines of a registered type (memoized).
    pub(super) fn machines(&mut self, name: &'a ObjectTypeName) -> Rc<Machines<'a>> {
        if let Some(machines) = self.machines.get(name) {
            return Rc::clone(machines);
        }
        let Some(entry) = self.types.get(name) else {
            return Rc::default();
        };
        let def = entry.def;
        let parent = self.parent(name);
        let ancestors = self.ancestors(name);
        let table = self.table(name);
        let (machines, conflicts) = self.machines_for(def, parent, &ancestors, &table);
        let machines = Rc::new(machines);
        self.machines.insert(name, Rc::clone(&machines));
        self.machine_conflicts.insert(name, conflicts);
        machines
    }

    /// Gather all machines inherited by `def` from `parent` and newly
    /// implemented interfaces, restricted to `table`'s enum states.
    fn inherited_object_machines(
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

    /// A type's machines: the supertype's and implemented interfaces',
    /// restricted to the states this type's table keeps, replaced by the
    /// type's own declarations (conjoining inherited constraints).
    fn machines_for(
        &mut self,
        def: &'a ObjectTypeDef,
        parent: Option<&'a ObjectTypeName>,
        ancestors: &[&'a ObjectTypeName],
        table: &Table<'a>,
    ) -> (Machines<'a>, BTreeSet<&'a PropertyId>) {
        let inherited = self.inherited_object_machines(def, parent, ancestors, table);
        let mut machines = inherited.machines.clone();
        for lifecycle in &def.lifecycles {
            if inherited.conflicts.contains(&lifecycle.property) {
                continue;
            }
            if let Some((property, member)) = table.get_key_value(&lifecycle.property)
                && !member.property.is_read_only()
                && let States::Enum(states) = states(member)
            {
                let mut machine = Machine::declared(lifecycle, states);
                // Guards conjoin: an inherited guard a (rejected) redeclaration
                // drops still applies.
                if let Some(wide) = inherited.machines.get(property) {
                    conjoin_machine(&mut machine, wide);
                }
                machines.insert(*property, machine);
            }
        }
        (machines, inherited.conflicts)
    }

    /// Every guard of every transition is non-blank and parses as a ZEN
    /// Expression (T1-ONT-2), independent of the lifecycle's other checks.
    fn check_guards(&mut self, at: &[&str], lifecycle: &Lifecycle) {
        for (position, transition) in lifecycle.transitions.iter().enumerate() {
            let position = position.to_string();
            let id = transition.id.as_str();
            if let Some(code) = &transition.guard_refusal_code
                && crate::codes::lookup(code).is_none_or(|entry| {
                    entry.tier != crate::codes::Tier::T6
                        || entry.severity != norn_ir::DiagnosticSeverity::Error
                })
            {
                self.out.push(error(
                    codes::INVALID_GUARD_DIAGNOSTIC,
                    "guardRefusalCode must name a registered T6 error".to_owned(),
                    span_at(at, &["transitions", position.as_str(), "guardRefusalCode"]),
                ));
            }
            for (index, guard) in transition.guards.iter().enumerate() {
                let index = index.to_string();
                let guard_at = span_at(
                    at,
                    &["transitions", position.as_str(), "guards", index.as_str()],
                );
                if guard.as_str().trim().is_empty() {
                    self.out.push(error(
                        codes::EMPTY_GUARD,
                        format!("a guard of transition {id:?} is blank"),
                        guard_at,
                    ));
                } else {
                    check_zen_syntax(
                        guard.as_str(),
                        codes::INVALID_GUARD,
                        &format!("guard {index} of transition {id:?}"),
                        guard_at,
                        &mut self.out,
                    );
                }
            }
        }
    }

    /// Check a type's own lifecycles and the machines it inherits (from its
    /// supertype and implemented interfaces) and narrows.
    pub(super) fn check_lifecycles(
        &mut self,
        base: &[&str],
        def: &'a ObjectTypeDef,
        parent: Option<&'a ObjectTypeName>,
        ancestors: &[&'a ObjectTypeName],
        table: &Table<'a>,
    ) {
        let inherited = self.inherited_object_machines(def, parent, ancestors, table);
        self.check_inherited_lifecycle_conflicts(base, table, &inherited);
        let mut declared = BTreeSet::new();
        for (position, lifecycle) in def.lifecycles.iter().enumerate() {
            let position = position.to_string();
            let at = [base, &["lifecycles", position.as_str()]].concat();
            // Guards are expression sites: checked even when the rest of the
            // lifecycle is skipped (a duplicate or invalid property).
            self.check_guards(&at, lifecycle);
            if !declared.insert(&lifecycle.property) {
                self.out.push(error(
                    codes::DUPLICATE_LIFECYCLE,
                    format!(
                        "property {:?} already has a lifecycle on this type",
                        lifecycle.property.as_str()
                    ),
                    span_at(&at, &["property"]),
                ));
                continue;
            }
            let Some(states) = self.lifecycle_states(&at, lifecycle, table) else {
                continue;
            };
            let clean = self.check_declaration(&at, lifecycle, &states);
            if clean {
                // The declaration's own shape; malformed members are local.
                let findings = crate::lifecycle_soundness::check_declaration(lifecycle, &states);
                self.soundness(&at, findings);
            }
            let machine = Machine::declared(lifecycle, states);
            if let Some(member) = table.get(&lifecycle.property) {
                self.check_default(&at, &["initial"], lifecycle, member, &machine);
            }
            if clean {
                // Soundness and structure hold of the effective machine: the
                // inherited triggers, modes, guards, endings, events and
                // compensation apply with the redeclaration's own.
                let effective = self.machines(&def.name);
                let machine = effective.get(&lifecycle.property).unwrap_or(&machine);
                let findings =
                    crate::lifecycle_soundness::check_effective_transitions(lifecycle, machine);
                self.soundness(&at, findings);
                self.check_structure(&at, machine);
                if effective.contains_key(&lifecycle.property) {
                    self.check_path_properties(&at, machine);
                }
            }
            if !inherited.conflicts.contains(&lifecycle.property)
                && let Some(wide) = inherited.machines.get(&lifecycle.property)
            {
                self.check_narrowing(&at, lifecycle, wide);
            }
        }
        self.check_inherited_unredeclared(base, def, table, &inherited, &declared);
    }

    fn check_inherited_lifecycle_conflicts(
        &mut self,
        base: &[&str],
        table: &Table<'a>,
        inherited: &InheritedObjectMachines<'a>,
    ) {
        for (pos, property, reason) in &inherited.new_conflicts {
            let pos = pos.to_string();
            self.out.push(error(
                crate::object_types::codes::INTERFACE_REQUIREMENT_CONFLICT,
                format!(
                    "inherited lifecycles for property {:?} conflict: {reason}",
                    property.as_str()
                ),
                span_at(base, &["implements", pos.as_str()]),
            ));
        }
        // An interface lifecycle governs a stored property: reject an
        // implementing type that satisfies the property with a computed one.
        let mut reported_computed: BTreeSet<&'a PropertyId> = BTreeSet::new();
        for &(pos, iface_def) in &inherited.direct_new_interfaces {
            let (iface_machines, _) = self.interface_machines(&iface_def.name);
            for &property in iface_machines.keys() {
                if let Some(member) = table.get(property)
                    && member.property.is_read_only()
                    && reported_computed.insert(property)
                {
                    let pos = pos.to_string();
                    self.out.push(error(
                        codes::PROPERTY_COMPUTED,
                        format!(
                            "lifecycle property {:?} inherited from interface {:?} is computed; a lifecycle governs a stored property that rules write",
                            property.as_str(),
                            iface_def.name.as_str()
                        ),
                        span_at(base, &["implements", pos.as_str()]),
                    ));
                }
            }
        }
    }

    /// Check inherited machines (from supertype or implemented interfaces)
    /// that `def` does not redeclare in `def.lifecycles`.
    fn check_inherited_unredeclared(
        &mut self,
        base: &[&str],
        def: &'a ObjectTypeDef,
        table: &Table<'a>,
        inherited: &InheritedObjectMachines<'a>,
        declared: &BTreeSet<&'a PropertyId>,
    ) {
        for (&property, machine) in &inherited.machines {
            if declared.contains(property) {
                continue;
            }
            let own_pos = def.properties.iter().position(|p| p.id == *property);
            let iface_info = inherited.from_iface.get(property);
            if own_pos.is_none() && iface_info.is_none() {
                continue;
            }
            let Some(member) = table.get(property) else {
                continue;
            };
            let pos_str;
            let at = if let Some(pos) = own_pos {
                pos_str = pos.to_string();
                [base, &["properties", pos_str.as_str()]].concat()
            } else if let Some((pos, _)) = iface_info {
                pos_str = pos.to_string();
                [base, &["implements", pos_str.as_str()]].concat()
            } else {
                continue;
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
            if let Some(default) = default_state(member)
                && machine.states.contains(&default)
                && !machine.initial.contains(default)
            {
                self.out.push(error(
                    codes::DEFAULT_NOT_INITIAL,
                    format!(
                        "the default {default:?} of {:?} is not an initial state of its inherited lifecycle",
                        property.as_str()
                    ),
                    span_at(&at, &[]),
                ));
            }
            let parent_overridden = own_pos.is_some() && inherited.from_parent.contains(property);
            let combined = inherited.source_counts.get(property).copied().unwrap_or(0) >= 2;
            let narrowed_from_iface = iface_info.is_some_and(|(_, wide_states)| {
                let effective_states: BTreeSet<&str> = machine.states.iter().copied().collect();
                effective_states != *wide_states
            });
            if parent_overridden || combined || narrowed_from_iface {
                let findings = crate::lifecycle_soundness::check_inherited_compensation(machine);
                self.soundness(&at, findings);
                self.check_structure(&at, machine);
                self.check_path_properties(&at, machine);
            }
        }
    }

    /// The states of a lifecycle's property, or `None` (after reporting why,
    /// unless it is reported elsewhere) when the lifecycle cannot be checked
    /// further.
    fn lifecycle_states(
        &mut self,
        at: &[&str],
        lifecycle: &Lifecycle,
        table: &Table<'a>,
    ) -> Option<Vec<&'a str>> {
        let id = lifecycle.property.as_str();
        let Some(member) = table.get(&lifecycle.property) else {
            self.out.push(error(
                codes::UNKNOWN_PROPERTY,
                format!("lifecycle property {id:?} is not a property of the type"),
                span_at(at, &["property"]),
            ));
            return None;
        };
        if member.property.is_read_only() {
            self.out.push(error(
                codes::PROPERTY_COMPUTED,
                format!(
                    "lifecycle property {id:?} is computed; a lifecycle governs a stored property that rules write"
                ),
                span_at(at, &["property"]),
            ));
            return None;
        }
        let states = match states(member) {
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
        if member.nullable() {
            self.out.push(error(
                codes::PROPERTY_NULLABLE,
                format!("lifecycle property {id:?} is nullable; every object is in a state"),
                span_at(at, &["property"]),
            ));
        }
        Some(states)
    }

    /// State names and transition shape of one declaration. Returns whether
    /// every name is a state (so the structure can be checked).
    fn check_declaration(&mut self, at: &[&str], lifecycle: &Lifecycle, states: &[&str]) -> bool {
        let mut clean = true;
        if lifecycle.initial.is_empty() {
            self.out.push(error(
                codes::EMPTY_INITIAL,
                "a lifecycle declares at least one initial state".to_owned(),
                span_at(at, &["initial"]),
            ));
        }
        clean &= self.check_state_list(at, "initial", &lifecycle.initial, states);
        clean &= self.check_state_list(at, "terminal", &lifecycle.terminal, states);
        clean &= self.check_state_list(at, "immutable", &lifecycle.immutable, states);
        for (index, property) in lifecycle.path_properties.iter().enumerate() {
            let index = index.to_string();
            for (member, state) in [("target", &property.target), ("via", &property.via)] {
                if !states.contains(&state.as_str()) {
                    clean = false;
                    self.out.push(unknown_state(
                        state,
                        span_at(at, &["pathProperties", index.as_str(), member]),
                    ));
                }
            }
        }
        let terminal: BTreeSet<&str> = lifecycle.terminal.iter().map(String::as_str).collect();
        let mut ids = BTreeSet::new();
        let mut pairs = BTreeSet::new();
        for (position, transition) in lifecycle.transitions.iter().enumerate() {
            let position = position.to_string();
            let at = [at, &["transitions", position.as_str()]].concat();
            let id = transition.id.as_str();
            if !matches_grammar(&MEMBER_ID_GRAMMAR, id) {
                self.out.push(error(
                    codes::INVALID_TRANSITION_ID,
                    format!("transition id {id:?} must match [a-z][a-z0-9_]*"),
                    span_at(&at, &["id"]),
                ));
            } else if !ids.insert(id) {
                self.out.push(error(
                    codes::DUPLICATE_TRANSITION_ID,
                    format!("transition id {id:?} is declared twice in this lifecycle"),
                    span_at(&at, &["id"]),
                ));
            }
            if transition.from.is_empty() {
                self.out.push(error(
                    codes::EMPTY_TRANSITION_SOURCE,
                    format!("transition {id:?} has no source state"),
                    span_at(&at, &["from"]),
                ));
            }
            clean &= self.check_state_list(&at, "from", &transition.from, states);
            if !states.contains(&transition.to.as_str()) {
                clean = false;
                self.out
                    .push(unknown_state(&transition.to, span_at(&at, &["to"])));
            }
            for (index, from) in transition.from.iter().enumerate() {
                let index = index.to_string();
                let span = span_at(&at, &["from", index.as_str()]);
                if *from == transition.to {
                    self.out.push(error(
                        codes::SELF_TRANSITION,
                        format!("transition {id:?} leaves and enters {from:?}; a transition changes the state"),
                        span,
                    ));
                } else if terminal.contains(from.as_str()) {
                    self.out.push(error(
                        codes::TERMINAL_HAS_TRANSITION,
                        format!("transition {id:?} leaves the terminal state {from:?}"),
                        span,
                    ));
                } else if !pairs.insert((from.as_str(), transition.to.as_str())) {
                    self.out.push(error(
                        codes::DUPLICATE_TRANSITION,
                        format!(
                            "{from:?} -> {:?} is already declared by another transition",
                            transition.to
                        ),
                        span,
                    ));
                }
            }
        }
        clean
    }

    /// Every name in `list` is a state, once. Returns whether all are states.
    fn check_state_list(
        &mut self,
        at: &[&str],
        member: &str,
        list: &[String],
        states: &[&str],
    ) -> bool {
        let mut clean = true;
        let mut seen = BTreeSet::new();
        for (index, state) in list.iter().enumerate() {
            let index = index.to_string();
            let span = span_at(at, &[member, index.as_str()]);
            if !states.contains(&state.as_str()) {
                clean = false;
                self.out.push(unknown_state(state, span));
            } else if !seen.insert(state) {
                self.out.push(error(
                    codes::DUPLICATE_STATE,
                    format!("state {state:?} is listed twice in {member}"),
                    span,
                ));
            }
        }
        clean
    }

    /// The property's effective default is an initial state.
    fn check_default(
        &mut self,
        at: &[&str],
        member_path: &[&str],
        lifecycle: &Lifecycle,
        member: &Resolved<'a>,
        machine: &Machine<'a>,
    ) {
        if let Some(default) = default_state(member)
            && machine.states.contains(&default)
            && !lifecycle.initial.is_empty()
            && !machine.initial.contains(default)
        {
            self.out.push(error(
                codes::DEFAULT_NOT_INITIAL,
                format!(
                    "the default {default:?} of {:?} is not an initial state",
                    lifecycle.property.as_str()
                ),
                span_at(at, member_path),
            ));
        }
    }

    /// LIFE-SOUND findings as diagnostics at `at` plus each finding's member path.
    fn soundness(&mut self, at: &[&str], findings: Vec<crate::lifecycle_soundness::Finding>) {
        for finding in findings {
            let path: Vec<&str> = finding.path.iter().map(String::as_str).collect();
            let mut diagnostic = error(finding.code, finding.message, span_at(at, &path));
            if finding.info {
                diagnostic.severity = norn_ir::DiagnosticSeverity::Info;
            }
            self.out.push(diagnostic);
        }
    }

    /// Reachability from the initial states, no dead ends, and soundness
    /// (LIFE-SOUND-1..8): option to complete, events, deadlines, compensation.
    fn check_structure(&mut self, at: &[&str], machine: &Machine<'a>) {
        if machine.initial.is_empty() {
            return;
        }
        let findings = crate::lifecycle_soundness::check_machine(machine);
        self.soundness(at, findings);
        for state in machine.unreachable() {
            self.out.push(error(
                codes::UNREACHABLE_STATE,
                format!("state {state:?} is not reachable from an initial state"),
                span_at(at, &[]),
            ));
        }
        for state in machine.dead_ends() {
            let trace = machine
                .trace(state, None)
                .and_then(|trace| serde_json::to_string(&trace).ok())
                .map_or_else(String::new, |trace| {
                    format!("; structural trace [transition, state]: {trace}")
                });
            self.out.push(error(
                codes::DEAD_END_STATE,
                format!(
                    "state {state:?} has no outgoing transition; declare one or list it as terminal{trace}"
                ),
                span_at(at, &[]),
            ));
        }
    }

    /// A graph counterexample is conservative when guards are present: it
    /// is a declared transition path, not a claim that all guards are jointly
    /// satisfiable on a runtime object.
    fn check_path_properties(&mut self, at: &[&str], machine: &Machine<'a>) {
        for property in &machine.path_properties {
            // A subtype may remove the target from its enum. The inherited
            // law then holds vacuously. Locally declared names were checked
            // against the enum by check_declaration.
            if !machine.states.contains(&property.target.as_str()) {
                continue;
            }
            if let Some(trace) = machine.trace(&property.target, Some(&property.via)) {
                let trace = serde_json::to_string(&trace).unwrap_or_default();
                self.out.push(error(
                    codes::PATH_PROPERTY_VIOLATED,
                    format!(
                        "a declared path to {:?} avoids {:?}; structural trace [transition, state]: {trace}",
                        property.target, property.via
                    ),
                    span_at(at, &[]),
                ));
            }
        }
    }

    /// A redeclared lifecycle only narrows the inherited machine `wide`.
    fn check_narrowing(&mut self, at: &[&str], lifecycle: &Lifecycle, wide: &Machine<'a>) {
        for (index, state) in lifecycle.initial.iter().enumerate() {
            if wide.states.contains(&state.as_str()) && !wide.initial.contains(state.as_str()) {
                let index = index.to_string();
                self.out.push(error(
                    codes::WIDENS_INITIAL,
                    format!(
                        "initial state {state:?} is not an initial state of the inherited lifecycle"
                    ),
                    span_at(at, &["initial", index.as_str()]),
                ));
            }
        }
        self.check_narrowed_endings(at, lifecycle, wide);
        self.check_narrowed_events(at, lifecycle, wide);
        for (position, transition) in lifecycle.transitions.iter().enumerate() {
            let position = position.to_string();
            let at = [at, &["transitions", position.as_str()]].concat();
            self.check_narrowed_transition(&at, transition, wide);
        }
    }

    fn check_narrowed_transition(
        &mut self,
        at: &[&str],
        transition: &Transition,
        wide: &Machine<'a>,
    ) {
        let id = transition.id.as_str();
        let Some(inherited) = wide.transitions.iter().find(|edge| edge.id.as_str() == id) else {
            self.out.push(error(
                codes::WIDENS_TRANSITION,
                format!(
                    "transition {id:?} is not a transition of the inherited lifecycle; a subtype may only drop transitions"
                ),
                span_at(at, &["id"]),
            ));
            return;
        };
        let to = inherited.to;
        if transition.to != to {
            self.out.push(error(
                codes::WIDENS_TRANSITION,
                format!(
                    "transition {id:?} enters {:?}, but the inherited transition enters {to:?}",
                    transition.to
                ),
                span_at(at, &["to"]),
            ));
            return;
        }
        for (index, state) in transition.from.iter().enumerate() {
            if !inherited.from.contains(state.as_str()) {
                let index = index.to_string();
                self.out.push(error(
                    codes::WIDENS_TRANSITION,
                    format!(
                        "transition {id:?} leaves {state:?}, which the inherited transition does not"
                    ),
                    span_at(at, &["from", index.as_str()]),
                ));
            }
        }
        self.check_narrowed_transition_contract(at, transition, inherited);
    }

    fn check_narrowed_transition_contract(
        &mut self,
        at: &[&str],
        transition: &Transition,
        inherited: &Edge<'a>,
    ) {
        let id = transition.id.as_str();
        // A fact stays a fact and a request stays a request (LIFE-SOUND-5);
        // a trigger makes the inherited transition a fact.
        if let Some(mode) = transition.mode {
            let fact = inherited.trigger.is_some() || inherited.mode == Some(TransitionMode::Fact);
            if (mode == TransitionMode::Fact) != fact {
                self.out.push(error(
                    codes::WIDENS_TRANSITION,
                    format!(
                        "transition {id:?} is declared {mode:?}, but the inherited transition is {}; a subtype keeps every inherited transition's mode",
                        if fact { "a fact" } else { "a request" }
                    ),
                    span_at(at, &["mode"]),
                ));
            }
        }
        if transition
            .trigger
            .as_ref()
            .is_some_and(|trigger| inherited.trigger.is_some_and(|parent| trigger != parent))
        {
            self.out.push(error(
                codes::WIDENS_TRANSITION,
                format!(
                    "transition {id:?} replaces an inherited trigger; a subtype keeps every inherited trigger"
                ),
                span_at(at, &["trigger"]),
            ));
        }
        if transition.compensated_by.as_ref().is_some_and(|comp| {
            inherited
                .compensated_by
                .is_some_and(|parent| comp != parent)
        }) {
            self.out.push(error(
                codes::WIDENS_TRANSITION,
                format!(
                    "transition {id:?} replaces an inherited compensation; a subtype keeps every inherited compensation"
                ),
                span_at(at, &["compensatedBy"]),
            ));
        }
        if transition
            .guard_refusal_code
            .as_deref()
            .is_some_and(|code| {
                inherited
                    .guard_refusal_code
                    .is_some_and(|parent| code != parent)
            })
        {
            self.out.push(error(
                codes::INVALID_GUARD_DIAGNOSTIC,
                "a subtype cannot replace an inherited guard diagnostic".to_owned(),
                span_at(at, &["guardRefusalCode"]),
            ));
        }
        for guard in &inherited.guards {
            if !transition.guards.iter().any(|own| own.as_str() == *guard) {
                self.out.push(error(
                    codes::DROPS_GUARD,
                    format!(
                        "transition {id:?} drops the inherited guard {guard:?}; a subtype restates every inherited guard and may only append its own"
                    ),
                    span_at(at, &["id"]),
                ));
            }
        }
    }

    fn check_narrowed_events(&mut self, at: &[&str], lifecycle: &Lifecycle, wide: &Machine<'a>) {
        for (index, event) in lifecycle.events.iter().enumerate() {
            let Some(inherited) = wide.events.iter().find(|own| own.id == event.id.as_str()) else {
                continue;
            };
            let wide_ignored: BTreeSet<&str> = inherited
                .ignored_in
                .iter()
                .flat_map(|(states, _)| states.iter().copied())
                .collect();
            let index = index.to_string();
            for (ignore_pos, ignore) in event.ignored_in.iter().enumerate() {
                let ignore_pos = ignore_pos.to_string();
                for (state_pos, state) in ignore.states.iter().enumerate() {
                    if !wide_ignored.contains(state.as_str()) {
                        let state_pos = state_pos.to_string();
                        self.out.push(error(
                            codes::WIDENS_TRANSITION,
                            format!(
                                "event {:?} ignores state {state:?}, which the inherited lifecycle does not ignore",
                                event.id
                            ),
                            span_at(
                                at,
                                &[
                                    "events",
                                    index.as_str(),
                                    "ignoredIn",
                                    ignore_pos.as_str(),
                                    "states",
                                    state_pos.as_str(),
                                ],
                            ),
                        ));
                    }
                }
            }
        }
    }

    /// An inherited ending keeps its outcome: re-labelling it would let a
    /// subtype escape the soundness findings the supertype's outcome owes.
    fn check_narrowed_endings(&mut self, at: &[&str], lifecycle: &Lifecycle, wide: &Machine<'a>) {
        for (index, ending) in lifecycle.endings.iter().enumerate() {
            let index = index.to_string();
            if let Some((_, outcome)) = wide
                .endings
                .iter()
                .find(|(state, _)| *state == ending.state.as_str())
                && *outcome != ending.outcome
            {
                self.out.push(error(
                    codes::WIDENS_TRANSITION,
                    format!(
                        "ending {:?} is {:?}, but the inherited lifecycle declares it {outcome:?}; a subtype keeps every inherited ending's outcome",
                        ending.state, ending.outcome
                    ),
                    span_at(at, &["endings", index.as_str(), "outcome"]),
                ));
            } else if !ending.outcome.is_final() && wide.terminal.contains(ending.state.as_str()) {
                self.out.push(error(
                    codes::WIDENS_TRANSITION,
                    format!(
                        "ending {:?} is {:?}, but the inherited lifecycle declares it terminal; a subtype keeps inherited terminal states final",
                        ending.state, ending.outcome
                    ),
                    span_at(at, &["endings", index.as_str(), "outcome"]),
                ));
            }
        }
        for (index, state) in lifecycle.terminal.iter().enumerate() {
            if let Some((_, outcome)) = wide
                .endings
                .iter()
                .find(|(wide_state, _)| *wide_state == state.as_str())
                && !outcome.is_final()
            {
                let index = index.to_string();
                self.out.push(error(
                    codes::WIDENS_TRANSITION,
                    format!(
                        "terminal state {state:?} is declared {outcome:?} in the inherited lifecycle; a subtype cannot turn an inherited resting ending into a terminal state"
                    ),
                    span_at(at, &["terminal", index.as_str()]),
                ));
            }
        }
    }
}

fn unknown_state(state: &str, at: crate::Span) -> crate::Diagnostic {
    error(
        codes::UNKNOWN_STATE,
        format!("{state:?} is not a value of the lifecycle property's enum"),
        at,
    )
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use norn_ir::{ObjectTypeDef, ObjectTypeName};

    use super::super::{Checker, EntityContext, EntityModel, Roots};

    fn def(value: serde_json::Value) -> ObjectTypeDef {
        serde_json::from_value(value).expect("decodes")
    }

    /// The effective guards of `submit` on `test.Sub`, whose redeclared
    /// `submit` carries `guards`.
    fn effective_guards(guards: &[&str]) -> Vec<String> {
        let types = [
            def(serde_json::json!({
                "name": "test.Base",
                "properties": [{ "id": "stage", "type": { "kind": "enum",
                    "values": [{ "value": "open" }, { "value": "closed" }] } }],
                "lifecycles": [{ "property": "stage", "initial": ["open"], "terminal": ["closed"],
                    "transitions": [{ "id": "close", "from": ["open"], "to": "closed", "guards": ["a"] }] }]
            })),
            def(serde_json::json!({
                "name": "test.Sub",
                "extends": "test.Base",
                "lifecycles": [{ "property": "stage", "initial": ["open"], "terminal": ["closed"],
                    "transitions": [{ "id": "close", "from": ["open"], "to": "closed", "guards": guards }] }]
            })),
        ];
        let registry = BTreeSet::new();
        let context = EntityContext {
            catalog: &[],
            registry: &registry,
            base: EntityModel::EMPTY,
            tenant: None,
        };
        let model = EntityModel {
            object_types: &types,
            ..EntityModel::EMPTY
        };
        let mut checker = Checker::new(&model, &context, &Roots::DOCUMENT, false);
        checker.register();
        checker.find_cycles();
        let name = ObjectTypeName("test.Sub".to_owned());
        let machines = checker.machines(&types[1].name);
        assert_eq!(types[1].name, name);
        let machine = machines.values().next().expect("one machine");
        machine.transitions[0]
            .guards
            .iter()
            .map(|guard| (*guard).to_owned())
            .collect()
    }

    #[test]
    fn effective_lifecycle_guards_conjoin_along_inheritance() {
        assert_eq!(effective_guards(&["a", "b"]), ["a", "b"]);
        // A redeclaration that drops the inherited guard is rejected, and the
        // effective machine still keeps it.
        assert_eq!(effective_guards(&[]), ["a"]);
        assert_eq!(effective_guards(&["b"]), ["a", "b"]);
    }
}
