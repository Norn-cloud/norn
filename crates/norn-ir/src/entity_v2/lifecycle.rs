//! Declared lifecycles of object types (T3-LIFE-1, #2672; ruling 2026-09-25
//! §2: "A type with a status property declares its allowed transitions as a
//! list on the type. The checker verifies that every rule that writes that
//! property only emits declared transitions. This one list renders the
//! lifecycle diagram, replaces the process graph as the thing a business user
//! looks at, and is the single owner of the state machine.").
//!
//! A [`Lifecycle`] is the business state machine of one `enum` property of an
//! object type (its *lifecycle property*, such as `status`). It is unrelated
//! to the definition's [`super::status::ResourceStatus`], which is the
//! maturity of the *definition*, not a state of its objects.
//!
//! Rules that shape the wire:
//!
//! - **The states are the property's enum values.** A lifecycle does not
//!   restate them: every value the property admits is a state, in the enum's
//!   presentation order, so the enum and the state machine cannot disagree.
//!   A subtype that narrows the enum narrows the state set with it.
//! - **One lifecycle per property.** A type may declare several (an order's
//!   `fulfillment_status` and `payment_status` are orthogonal machines), each
//!   over a different property.
//! - **A transition is named and may leave several states.** `cancel` from
//!   `draft` and `submitted` is one transition: its [`TransitionId`] is the
//!   name actions, classifier label spaces (ruling 2026-09-25 §26), and
//!   transition contracts (§25, T3-TC-1) refer to. The lifecycle does not
//!   name the action that fires it: the action refers to the transition, so
//!   the lifecycle stays the single owner of the machine.
//! - **Inheritance only narrows.** A subtype inherits its supertype's
//!   lifecycles. Redeclaring one (same property) may only drop transitions,
//!   drop source states of a transition, drop initial states, and append
//!   guards (every inherited guard is restated; guards conjoin). Immutable
//!   states are inherited monotonically: a subtype may add restrictions but
//!   cannot remove an inherited one.
//!   Tenants extend through subtypes under the same rule and never widen a
//!   base lifecycle; an [`super::extension::ObjectTypeExtension`] cannot
//!   carry a lifecycle at all.
//! - **Omitted vs `null`** follows #2819.
//!
//! Every consistency rule (the property is a non-nullable, stored, local
//! `enum`; states are enum values; reachability from an initial state; no
//! dead ends outside the terminal states; narrowing) is a `norn-checker`
//! `lifecycle.validation.*` diagnostic, not a decode error.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use std::collections::{BTreeMap, BTreeSet};

use super::extension::ObjectTypeExtension;
use super::object::{MEMBER_ID_PATTERN, ObjectTypeDef, PropertyId};
use super::property::PropertyType;
use crate::ZenExpression;
use crate::members::present_non_null;

/// Id of a [`Transition`], unique within its lifecycle. Same grammar as a
/// property id ([`MEMBER_ID_PATTERN`]).
#[derive(
    Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(transparent)]
pub struct TransitionId(#[schemars(regex(pattern = MEMBER_ID_PATTERN))] pub String);

impl TransitionId {
    /// The id as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The declared state machine of one `enum` property of an object type.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Lifecycle {
    /// The lifecycle property: a stored, non-nullable `enum` property of the
    /// type (declared, inherited, or added by an extension). Its values are
    /// the states.
    pub property: PropertyId,
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub description: Option<String>,
    /// The states a new object may be created in; non-empty. The property's
    /// default, if any, must be one of them.
    pub initial: Vec<String>,
    /// Absorbing states: no transition leaves them. Every other state has at
    /// least one outgoing transition. Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub terminal: Vec<String>,
    /// States in which non-lifecycle properties cannot be written. Declared
    /// transitions may still leave these states. Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub immutable: Vec<String>,
    /// The allowed transitions, in presentation order. A write that changes
    /// the property from `a` to `b` is allowed exactly when a transition has
    /// `a` in `from` and `to == b`. Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transitions: Vec<Transition>,
    /// Structural ordering laws, checked over every declared transition path.
    /// Guards can remove paths but cannot introduce a path that avoids `via`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub path_properties: Vec<LifecyclePathProperty>,
}

/// Every path from an initial state to `target` must visit `via`, including
/// the initial and target states. An unreachable target satisfies this law
/// vacuously, but is independently rejected by the reachability check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LifecyclePathProperty {
    /// Destination state.
    pub target: String,
    /// Required state on every path to the destination.
    pub via: String,
}

/// Stable codes of lifecycle refusals shared by `norn-checker` (static
/// findings) and `norn-kernel` (commit-time refusals), so both report the
/// same code for the same violation.
pub mod codes {
    /// A write of the lifecycle property that no declared transition (or, on
    /// create, no initial state) allows; also firing a transition from a
    /// state it does not leave, including its own target state.
    pub const UNDECLARED_WRITE: &str = "lifecycle.validation.undeclared_write";
    /// A direct (non-transition) write of a lifecycle property.
    pub const DIRECT_WRITE: &str = "artifact_set.lifecycle.direct_write";
    /// A transition of a property that has no declared lifecycle.
    pub const NOT_A_LIFECYCLE: &str = "artifact_set.lifecycle.not_a_lifecycle";
    /// A transition id the lifecycle does not declare.
    pub const UNKNOWN_TRANSITION: &str = "artifact_set.lifecycle.unknown_transition";
    /// A non-lifecycle property write while the object is in an immutable
    /// state.
    pub const IMMUTABLE_STATE_WRITE: &str = "lifecycle.validation.immutable_state_write";
}

/// Why firing a transition is refused ([`Lifecycle::fire`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FiringRefusal {
    /// The lifecycle declares no transition with that id
    /// ([`codes::UNKNOWN_TRANSITION`]).
    UnknownTransition,
    /// The current state (or its absence) is not a source state of the
    /// transition ([`codes::UNDECLARED_WRITE`]).
    NotFromState,
}

impl FiringRefusal {
    /// The stable code of the refusal.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::UnknownTransition => codes::UNKNOWN_TRANSITION,
            Self::NotFromState => codes::UNDECLARED_WRITE,
        }
    }
}

impl Lifecycle {
    /// The per-call semantics of firing `transition` on an object whose
    /// lifecycle property holds `current` (T3-LIFE-3): allowed exactly when
    /// the object holds a state, the lifecycle declares the transition, and
    /// `current` is one of its source states. An object with no state is
    /// refused as [`FiringRefusal::NotFromState`] before the transition id is
    /// looked up (the precedence of `norn-checker`'s `check_action_firing`). A transition never leaves its own target, so a repeat
    /// firing once the object is in the target state is refused too. Guards
    /// are not evaluated here (they read the object; the kernel evaluates
    /// them at commit). Pass the effective lifecycle of the object's own type.
    ///
    /// # Errors
    ///
    /// The [`FiringRefusal`].
    pub fn fire(
        &self,
        transition: &str,
        current: Option<&str>,
    ) -> Result<&Transition, FiringRefusal> {
        let current = current.ok_or(FiringRefusal::NotFromState)?;
        let declared = self
            .transitions
            .iter()
            .find(|declared| declared.id.as_str() == transition)
            .ok_or(FiringRefusal::UnknownTransition)?;
        if declared.from.iter().any(|from| from == current) {
            Ok(declared)
        } else {
            Err(FiringRefusal::NotFromState)
        }
    }

    /// This lifecycle restricted to `states` (a subtype's narrowed enum, in
    /// presentation order): initial, terminal, and source states outside it
    /// are dropped, as are transitions into an excluded state or left with no
    /// source. Descriptions are dropped.
    #[must_use]
    pub fn restricted(&self, states: &[String]) -> Self {
        let ordered = |list: &[String]| -> Vec<String> {
            states
                .iter()
                .filter(|state| list.contains(state))
                .cloned()
                .collect()
        };
        Self {
            property: self.property.clone(),
            description: None,
            initial: ordered(&self.initial),
            terminal: ordered(&self.terminal),
            immutable: ordered(&self.immutable),
            path_properties: self.path_properties.clone(),
            transitions: self
                .transitions
                .iter()
                .filter(|transition| states.contains(&transition.to))
                .map(|transition| Transition {
                    id: transition.id.clone(),
                    description: None,
                    from: ordered(&transition.from),
                    to: transition.to.clone(),
                    guards: transition.guards.clone(),
                })
                .filter(|transition| !transition.from.is_empty())
                .collect(),
        }
    }

    /// Whether a declared transition moves the property from `from` to `to`.
    #[must_use]
    pub fn allows(&self, from: &str, to: &str) -> bool {
        self.transitions
            .iter()
            .any(|transition| transition.to == to && transition.from.iter().any(|f| f == from))
    }
}

/// One named, allowed change of the lifecycle property.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Transition {
    /// Unique id within the lifecycle (`submit`, `cancel`).
    pub id: TransitionId,
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub description: Option<String>,
    /// The source states, non-empty and distinct; none is `to`.
    pub from: Vec<String>,
    /// The target state.
    pub to: String,
    /// Boolean ZEN Expressions over the object's properties, evaluated on
    /// the state before the change; the transition may be taken only when
    /// every one holds (their conjunction). A subtype that redeclares the
    /// transition restates every inherited guard verbatim and may append its
    /// own, so the declaration it carries is the full effective guard set;
    /// dropping or replacing an inherited guard is a checker error. A guard
    /// reads the object through `self`; the checker parses it (T1-ONT-2)
    /// and types it (T1-EXPR-5, #2523); enforcement at commit is the
    /// kernel's. Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub guards: Vec<ZenExpression>,
}

/// The effective lifecycles of object type `name`, by lifecycle property:
/// the supertype's, restricted to the states (local `enum` values) the type
/// keeps, replaced by the type's own declarations restricted the same way,
/// with guards conjoined (inherited first, then the redeclaration's own).
///
/// This is the effective state machine `norn-checker` validates against
/// (its `EffectiveLifecycle`; `norn-checker/tests/shared_lifecycle_semantics.rs`
/// pins the two equal), exposed here so `norn-kernel` enforces the same
/// machine at commit. A property's states come from its nearest declaration
/// along the lineage, or from an `extensions` member added to the type or a
/// supertype. A lifecycle whose property does not resolve to an inline
/// `enum` that way has no machine (the checker rejects or resolves it
/// through a shared property type); callers that enforce must fail closed on
/// such a lifecycle (see [`declared_lifecycle_properties`]).
/// `lookup` resolves an object type by name; a cyclic `extends` chain (also
/// a checker error) is cut.
pub fn effective_lifecycles<'a>(
    lookup: &dyn Fn(&str) -> Option<&'a ObjectTypeDef>,
    extensions: &[&ObjectTypeExtension],
    name: &str,
) -> BTreeMap<PropertyId, Lifecycle> {
    let chain = lineage(lookup, name);
    let mut machines: BTreeMap<PropertyId, Lifecycle> = BTreeMap::new();
    // Root first; `visible` is the type and its supertypes, nearest first.
    for depth in (0..chain.len()).rev() {
        let visible = chain.get(depth..).unwrap_or(&[]);
        let Some(def) = visible.first() else {
            continue;
        };
        let states = |property: &PropertyId| -> Option<Vec<String>> {
            visible
                .iter()
                .find_map(|def| def.properties.iter().find(|member| member.id == *property))
                .or_else(|| {
                    extensions
                        .iter()
                        .filter(|extension| visible.iter().any(|def| def.name == extension.target))
                        .find_map(|extension| {
                            extension
                                .properties
                                .iter()
                                .find(|member| member.id == *property)
                        })
                })
                .and_then(|member| match &member.property_type {
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
                })
        };
        let inherited = std::mem::take(&mut machines);
        for (property, lifecycle) in &inherited {
            if let Some(states) = states(property) {
                machines.insert(property.clone(), lifecycle.restricted(&states));
            }
        }
        for lifecycle in &def.lifecycles {
            let Some(states) = states(&lifecycle.property) else {
                continue;
            };
            let mut machine = lifecycle.restricted(&states);
            if let Some(wide) = inherited.get(&lifecycle.property) {
                machine.immutable = states
                    .iter()
                    .filter(|state| {
                        machine.immutable.contains(state) || wide.immutable.contains(state)
                    })
                    .cloned()
                    .collect();
                for property in &wide.path_properties {
                    if !machine.path_properties.contains(property) {
                        machine.path_properties.push(property.clone());
                    }
                }
                for transition in &mut machine.transitions {
                    if let Some(parent) = wide.transitions.iter().find(|p| p.id == transition.id) {
                        let own = std::mem::take(&mut transition.guards);
                        transition.guards = parent.guards.clone();
                        for guard in own {
                            if !transition.guards.contains(&guard) {
                                transition.guards.push(guard);
                            }
                        }
                    }
                }
            }
            machines.insert(lifecycle.property.clone(), machine);
        }
    }
    machines
}

/// `name` and then its supertypes, nearest first; a cycle is cut.
fn lineage<'a>(
    lookup: &dyn Fn(&str) -> Option<&'a ObjectTypeDef>,
    name: &str,
) -> Vec<&'a ObjectTypeDef> {
    let mut chain: Vec<&'a ObjectTypeDef> = Vec::new();
    let mut next = lookup(name);
    while let Some(def) = next {
        if chain.iter().any(|seen| seen.name == def.name) {
            break;
        }
        chain.push(def);
        next = def
            .extends
            .as_ref()
            .and_then(|parent| lookup(parent.as_str()));
    }
    chain
}

/// Every property a lifecycle is declared over anywhere along `name`'s
/// lineage, whether or not it resolves to an effective machine. An enforcer
/// treats each as a lifecycle property: never written directly, and a
/// transition or create over one without an effective machine is refused
/// (fail closed).
pub fn declared_lifecycle_properties<'a>(
    lookup: &dyn Fn(&str) -> Option<&'a ObjectTypeDef>,
    name: &str,
) -> BTreeSet<PropertyId> {
    lineage(lookup, name)
        .iter()
        .flat_map(|def| {
            def.lifecycles
                .iter()
                .map(|lifecycle| lifecycle.property.clone())
        })
        .collect()
}
