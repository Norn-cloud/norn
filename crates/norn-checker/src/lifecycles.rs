//! Declared lifecycles (T3-LIFE-1, #2672; ruling 2026-09-25 §2): the codes,
//! the effective state machine of a lifecycle property, and the rule-write
//! conformance hook.
//!
//! The model-level checks (the lifecycle property, state names, structure,
//! and narrowing along inheritance) run inside
//! [`crate::check_entity_model`]; see `entity_model/lifecycles.rs` and
//! `docs/platform/entity-ir-v2.md`. [`check_lifecycle_writes`] is the write
//! hook; [`crate::lifecycle_writes`] (T3-LIFE-2, #2673) calls it over an
//! artifact set for the states a decision creates, and rejects every other
//! lifecycle-property write that is not a declared transition.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use norn_ir::{
    ArtifactEnvelope, EntityRoot, Lifecycle, ObjectTypeName, PropertyId, PropertyType, Transition,
    TransitionId, ZenExpression,
};

use crate::entity_instance::{EffectiveCatalog, EffectiveLifecycle};

use crate::entity_artifacts::{Decoded, decode_v2};
use crate::property_types::error;
use crate::{Diagnostic, Span};

/// Stable `lifecycle.validation.*` codes.
pub mod codes {
    /// A declared path reaches its target without the required state.
    pub const PATH_PROPERTY_VIOLATED: &str = "lifecycle.validation.path_property_violated";

    /// The lifecycle property is not a property of the type.
    pub const UNKNOWN_PROPERTY: &str = "lifecycle.validation.unknown_property";
    /// The lifecycle property does not resolve to a local `enum`.
    pub const PROPERTY_NOT_ENUM: &str = "lifecycle.validation.property_not_enum";
    /// The lifecycle property is nullable.
    pub const PROPERTY_NULLABLE: &str = "lifecycle.validation.property_nullable";
    /// The lifecycle property is computed (read-only).
    pub const PROPERTY_COMPUTED: &str = "lifecycle.validation.property_computed";
    /// Two lifecycles of one type name the same property.
    pub const DUPLICATE_LIFECYCLE: &str = "lifecycle.validation.duplicate_lifecycle";
    /// A state is not a value of the lifecycle property's enum.
    pub const UNKNOWN_STATE: &str = "lifecycle.validation.unknown_state";
    /// A state is listed twice in one list.
    pub const DUPLICATE_STATE: &str = "lifecycle.validation.duplicate_state";
    /// No initial state.
    pub const EMPTY_INITIAL: &str = "lifecycle.validation.empty_initial";
    /// A transition id does not follow `[a-z][a-z0-9_]*`.
    pub const INVALID_TRANSITION_ID: &str = "lifecycle.validation.invalid_transition_id";
    /// Two transitions of one lifecycle share an id.
    pub const DUPLICATE_TRANSITION_ID: &str = "lifecycle.validation.duplicate_transition_id";
    /// A transition has no source state.
    pub const EMPTY_TRANSITION_SOURCE: &str = "lifecycle.validation.empty_transition_source";
    /// A transition leaves its own target state.
    pub const SELF_TRANSITION: &str = "lifecycle.validation.self_transition";
    /// A `(from, to)` pair is declared by two transitions.
    pub const DUPLICATE_TRANSITION: &str = "lifecycle.validation.duplicate_transition";
    /// A transition leaves a terminal state.
    pub const TERMINAL_HAS_TRANSITION: &str = "lifecycle.validation.terminal_has_transition";
    /// A state cannot be reached from any initial state.
    pub const UNREACHABLE_STATE: &str = "lifecycle.validation.unreachable_state";
    /// A non-terminal state has no outgoing transition.
    pub const DEAD_END_STATE: &str = "lifecycle.validation.dead_end_state";
    /// The lifecycle property's default is not an initial state.
    pub const DEFAULT_NOT_INITIAL: &str = "lifecycle.validation.default_not_initial";
    /// A transition guard is blank.
    pub const EMPTY_GUARD: &str = "lifecycle.validation.empty_guard";
    /// A transition guard does not parse as a ZEN Expression (T1-ONT-2,
    /// #2515; syntax only, typing is T1-EXPR-5).
    pub const INVALID_GUARD: &str = "lifecycle.validation.invalid_guard";
    /// A subtype's redeclared transition drops (or replaces) an inherited
    /// guard.
    pub const DROPS_GUARD: &str = "lifecycle.validation.drops_guard";
    /// A subtype's lifecycle allows a transition its supertype's does not.
    pub const WIDENS_TRANSITION: &str = "lifecycle.validation.widens_transition";
    /// A subtype's lifecycle adds an initial state.
    pub const WIDENS_INITIAL: &str = "lifecycle.validation.widens_initial";
    /// A rule writes a lifecycle property with an undeclared transition.
    pub const UNDECLARED_WRITE: &str = "lifecycle.validation.undeclared_write";
}

/// The effective state machine of one lifecycle property of one type: the
/// declared (or inherited) lifecycle restricted to the property's effective
/// enum values.
#[derive(Debug, Clone)]
pub(crate) struct Machine<'a> {
    /// The property's enum values, in presentation order.
    pub(crate) states: Vec<&'a str>,
    pub(crate) initial: BTreeSet<&'a str>,
    pub(crate) terminal: BTreeSet<&'a str>,
    pub(crate) immutable: BTreeSet<&'a str>,
    pub(crate) transitions: Vec<Edge<'a>>,
    pub(crate) path_properties: Vec<&'a norn_ir::LifecyclePathProperty>,
}

/// One transition of a [`Machine`].
#[derive(Debug, Clone)]
pub(crate) struct Edge<'a> {
    pub(crate) id: &'a TransitionId,
    pub(crate) from: BTreeSet<&'a str>,
    pub(crate) to: &'a str,
    /// Every guard that applies (their conjunction), inherited ones first.
    pub(crate) guards: Vec<&'a str>,
}

/// Effective machines of a type, by lifecycle property.
pub(crate) type Machines<'a> = BTreeMap<&'a PropertyId, Machine<'a>>;

impl<'a> Machine<'a> {
    /// The machine a declaration describes over `states`, keeping only names
    /// that are states.
    pub(crate) fn declared(lifecycle: &'a Lifecycle, states: Vec<&'a str>) -> Self {
        let machine = Self {
            path_properties: lifecycle.path_properties.iter().collect(),
            initial: lifecycle.initial.iter().map(String::as_str).collect(),
            terminal: lifecycle.terminal.iter().map(String::as_str).collect(),
            immutable: lifecycle.immutable.iter().map(String::as_str).collect(),
            transitions: lifecycle
                .transitions
                .iter()
                .map(|transition| Edge {
                    id: &transition.id,
                    from: transition.from.iter().map(String::as_str).collect(),
                    to: transition.to.as_str(),
                    guards: transition
                        .guards
                        .iter()
                        .map(norn_ir::ZenExpression::as_str)
                        .collect(),
                })
                .collect(),
            states: Vec::new(),
        };
        machine.restricted(states)
    }

    /// The machine over `states` (a subset of its own, after a subtype
    /// narrows the enum): states outside it, and transitions into them or
    /// left with no source, are dropped.
    pub(crate) fn restricted(&self, states: Vec<&'a str>) -> Self {
        let keep: BTreeSet<&str> = states.iter().copied().collect();
        Self {
            path_properties: self.path_properties.clone(),
            initial: self.initial.intersection(&keep).copied().collect(),
            terminal: self.terminal.intersection(&keep).copied().collect(),
            immutable: self.immutable.intersection(&keep).copied().collect(),
            transitions: self
                .transitions
                .iter()
                .filter(|edge| keep.contains(edge.to))
                .map(|edge| Edge {
                    from: edge.from.intersection(&keep).copied().collect(),
                    ..edge.clone()
                })
                .filter(|edge| !edge.from.is_empty())
                .collect(),
            states,
        }
    }

    /// The machine as an owned effective lifecycle of `property`: states,
    /// initial and terminal states, and each transition's sources in
    /// presentation order; guards inherited first.
    pub(crate) fn effective(&self, property: &PropertyId) -> EffectiveLifecycle {
        let ordered = |set: &BTreeSet<&str>| -> Vec<String> {
            self.states
                .iter()
                .filter(|state| set.contains(*state))
                .map(|state| (*state).to_owned())
                .collect()
        };
        EffectiveLifecycle {
            states: self
                .states
                .iter()
                .map(|state| (*state).to_owned())
                .collect(),
            lifecycle: Lifecycle {
                path_properties: self.path_properties.iter().map(|p| (*p).clone()).collect(),
                property: property.clone(),
                description: None,
                initial: ordered(&self.initial),
                terminal: ordered(&self.terminal),
                immutable: ordered(&self.immutable),
                transitions: self
                    .transitions
                    .iter()
                    .map(|edge| Transition {
                        id: edge.id.clone(),
                        description: None,
                        from: ordered(&edge.from),
                        to: edge.to.to_owned(),
                        guards: edge
                            .guards
                            .iter()
                            .map(|guard| ZenExpression((*guard).to_owned()))
                            .collect(),
                    })
                    .collect(),
            },
        }
    }

    /// A shortest structural trace to `target`, excluding `avoid`. The first
    /// pair has no transition; each later pair names the transition entering
    /// the state. Declaration order breaks ties deterministically.
    pub(crate) fn trace(
        &self,
        target: &str,
        avoid: Option<&str>,
    ) -> Option<Vec<(Option<&'a str>, &'a str)>> {
        let mut previous = BTreeMap::new();
        let mut queue = VecDeque::new();
        for state in &self.states {
            if self.initial.contains(state) && avoid != Some(*state) {
                previous.insert(*state, None);
                queue.push_back(*state);
            }
        }
        while let Some(state) = queue.pop_front() {
            if state == target {
                let mut trace = Vec::new();
                let mut current = state;
                loop {
                    if let Some((from, transition)) = previous.get(current).copied().flatten() {
                        trace.push((Some(transition), current));
                        current = from;
                    } else {
                        trace.push((None, current));
                        trace.reverse();
                        return Some(trace);
                    }
                }
            }
            for edge in &self.transitions {
                if edge.from.contains(state)
                    && avoid != Some(edge.to)
                    && !previous.contains_key(edge.to)
                {
                    previous.insert(edge.to, Some((state, edge.id.as_str())));
                    queue.push_back(edge.to);
                }
            }
        }
        None
    }

    /// States no initial state reaches, in presentation order.
    pub(crate) fn unreachable(&self) -> Vec<&'a str> {
        let mut reached: BTreeSet<&str> = self.initial.clone();
        let mut frontier: Vec<&str> = reached.iter().copied().collect();
        while let Some(state) = frontier.pop() {
            for edge in &self.transitions {
                if edge.from.contains(state) && reached.insert(edge.to) {
                    frontier.push(edge.to);
                }
            }
        }
        self.states
            .iter()
            .copied()
            .filter(|state| !reached.contains(state))
            .collect()
    }

    /// Non-terminal states with no outgoing transition, in presentation
    /// order.
    pub(crate) fn dead_ends(&self) -> Vec<&'a str> {
        self.states
            .iter()
            .copied()
            .filter(|state| {
                !self.terminal.contains(state)
                    && !self
                        .transitions
                        .iter()
                        .any(|edge| edge.from.contains(state))
            })
            .collect()
    }
}

/// One statically known write of a lifecycle property by a rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LifecycleWrite<'a> {
    /// The state the object is in when the write applies; `None` when the
    /// write creates the object.
    pub from: Option<&'a str>,
    /// The state written.
    pub to: &'a str,
    /// Where the write is, in the rule document.
    pub at: Span,
}

/// Check statically known writes of a lifecycle property against its
/// (effective) lifecycle: a creating write must write an initial state, and
/// any other write must leave the state unchanged or follow a declared
/// transition. Every other write is `lifecycle.validation.undeclared_write`.
///
/// This is the hook of the ruling's "the checker verifies that every rule
/// that writes that property only emits declared transitions";
/// [`crate::lifecycle_writes::check_lifecycle_write_set`] calls it.
#[must_use]
pub fn check_lifecycle_writes(
    lifecycle: &Lifecycle,
    writes: &[LifecycleWrite<'_>],
) -> Vec<Diagnostic> {
    writes
        .iter()
        .filter_map(|write| {
            let property = lifecycle.property.as_str();
            let message = match write.from {
                None if lifecycle.initial.iter().any(|state| state == write.to) => return None,
                None => format!(
                    "a rule creates an object with {property} {:?}, which is not an initial state of its lifecycle",
                    write.to
                ),
                Some(from) if from == write.to || lifecycle.allows(from, write.to) => return None,
                Some(from) => format!(
                    "a rule writes {property} from {from:?} to {:?}, which is not a declared transition of its lifecycle",
                    write.to
                ),
            };
            Some(error(codes::UNDECLARED_WRITE, message, write.at.clone()))
        })
        .collect()
}

/// Check one firing of the declared transition `transition` of `lifecycle`
/// on an object whose lifecycle property holds `current` (T3-LIFE-3, #2867).
///
/// The firing is allowed exactly when `current` is one of the transition's
/// source states. A transition id the lifecycle does not declare is
/// [`crate::lifecycle_writes::codes::UNKNOWN_TRANSITION`]; any other firing
/// would write the property from `current` to the transition's target state,
/// which is not a declared transition, so it is
/// `lifecycle.validation.undeclared_write` (the code of
/// [`check_lifecycle_writes`]). Unlike a value write, a firing from the
/// target state itself is not "unchanged": a transition never leaves its own
/// target, so it is refused too.
///
/// Pass the **effective** lifecycle of the object's own type (inherited and
/// narrowed, [`EffectiveLifecycle`]). Guards are not evaluated here: they
/// read the object, and enforcing them at commit is the kernel's.
#[must_use]
pub fn check_transition_firing(
    lifecycle: &Lifecycle,
    transition: &str,
    current: &str,
    at: Span,
) -> Vec<Diagnostic> {
    let property = lifecycle.property.as_str();
    // The semantics are `Lifecycle::fire`, shared with the kernel's commit.
    // (`tests/shared_lifecycle_semantics.rs` pins its codes equal to these.)
    if lifecycle.fire(transition, Some(current)).is_ok() {
        return Vec::new();
    }
    let Some(declared) = lifecycle
        .transitions
        .iter()
        .find(|declared| declared.id.as_str() == transition)
    else {
        return vec![error(
            crate::lifecycle_writes::codes::UNKNOWN_TRANSITION,
            format!("{transition:?} is not a declared transition of the {property} lifecycle"),
            at,
        )];
    };
    vec![error(
        codes::UNDECLARED_WRITE,
        format!(
            "firing {transition:?} writes {property} from {current:?} to {:?}, which is not a declared transition of its lifecycle: {transition:?} leaves only {:?}",
            declared.to, declared.from
        ),
        at,
    )]
}

/// One lifecycle an `EntityIR` v2 object type declares, with the states it
/// ranges over: the input of a lifecycle diagram (T1-CHK-4, #2527).
#[derive(Debug, Clone, PartialEq)]
pub struct DeclaredLifecycle {
    /// The artifact that declares the object type.
    pub artifact_id: String,
    /// The object type.
    pub object_type: ObjectTypeName,
    /// The lifecycle property's enum values in presentation order: the
    /// effective states when the type resolves in the set, else its local
    /// `enum` values, else empty.
    pub states: Vec<String>,
    /// The effective lifecycle when the type resolves in the set (inherited
    /// guards first, narrowed states), else the declaration.
    pub lifecycle: Lifecycle,
}

/// Every lifecycle the `EntityIR` v2 object types in `artifacts` declare, in
/// artifact then declaration order. Artifacts that are not `EntityIR` v2
/// object types, or do not decode, are skipped: this is a projection input,
/// not a check; run [`crate::check_artifact_set`] for the findings.
#[must_use]
pub fn declared_lifecycles(artifacts: &[ArtifactEnvelope]) -> Vec<DeclaredLifecycle> {
    let catalog = EffectiveCatalog::from_artifacts(artifacts);
    let mut out = Vec::new();
    for artifact in artifacts {
        let Decoded::Root(root) = decode_v2(artifact) else {
            continue;
        };
        let EntityRoot::V2(root) = *root else {
            continue;
        };
        let def = root.object_type;
        let effective = catalog
            .object_type(&artifact.artifact_id)
            .and_then(Result::ok);
        for declared in &def.lifecycles {
            let property = declared.property.as_str();
            let (states, lifecycle) =
                match effective.and_then(|object_type| object_type.lifecycle(property)) {
                    // The machine the checker enforces (inherited guards,
                    // narrowed states): what the diagram renders.
                    Some(EffectiveLifecycle { states, lifecycle }) => {
                        (states.clone(), lifecycle.clone())
                    }
                    None => (
                        def.properties
                            .iter()
                            .find(|local| local.id.as_str() == property)
                            .and_then(|local| enum_values(&local.property_type))
                            .unwrap_or_default(),
                        declared.clone(),
                    ),
                };
            out.push(DeclaredLifecycle {
                artifact_id: artifact.artifact_id.clone(),
                object_type: def.name.clone(),
                states,
                lifecycle,
            });
        }
    }
    out
}

fn enum_values(property_type: &PropertyType) -> Option<Vec<String>> {
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
        | PropertyType::Ref { .. } => None,
    }
}
