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

use std::collections::BTreeSet;
use std::rc::Rc;

use norn_ir::{
    JsonValue, Lifecycle, ObjectTypeDef, ObjectTypeName, PropertyId, PropertyType, Transition,
    TransitionMode,
};

use super::{Checker, Table};
use crate::lifecycles::{Edge, Machine, Machines, codes};
use crate::object_types::{MEMBER_ID_GRAMMAR, Resolved, check_zen_syntax};
use crate::property_types::{TypeResolution, error, matches_grammar, span_at};

mod interfaces;

use interfaces::{InheritedObjectMachines, conjoin_machine};

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

impl<'a> Checker<'a, '_> {
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
