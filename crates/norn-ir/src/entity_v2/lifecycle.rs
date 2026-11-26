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
use super::interface::{InterfaceDef, InterfaceName};
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
    /// LIFE-SOUND-1: the declared endings and their outcomes. A run is sound
    /// when it can always reach one. A non-`resting` ending is a terminal
    /// state; a `resting` ending is a non-terminal state a perpetual object
    /// may stay in (an account, an item). A terminal state with no entry here
    /// is an unclassified ending. Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub endings: Vec<LifecycleEnding>,
    /// LIFE-SOUND-6: the external event kinds an object of this lifecycle can
    /// receive (connector observations, provider results, counterparty
    /// cancellations). Every reachable non-terminal state handles each one by
    /// an event-triggered transition or an explicit `ignoredIn` entry.
    /// Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<LifecycleEvent>,
}

/// How a lifecycle run ended (LIFE-SOUND-1).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleOutcome {
    /// The run did what it was for.
    Completed,
    /// The run was undone and every dispatched effect was compensated.
    Compensated,
    /// The run was abandoned before any uncompensated effect.
    Cancelled,
    /// An obligation was breached and the breach is visible (a remedy owed).
    BreachedVisible,
    /// A non-terminal state a perpetual object may rest in indefinitely.
    Resting,
}

impl LifecycleOutcome {
    /// Whether an ending with this outcome must be a terminal state.
    #[must_use]
    pub const fn is_final(self) -> bool {
        !matches!(self, Self::Resting)
    }
}

/// One declared ending of a lifecycle.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LifecycleEnding {
    /// The state; listed once.
    pub state: String,
    /// What ending in it means.
    pub outcome: LifecycleOutcome,
}

/// An external event kind a lifecycle receives (LIFE-SOUND-6).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LifecycleEvent {
    /// The event id, same grammar as a transition id; unique per lifecycle.
    pub id: String,
    /// States in which the event is deliberately ignored, each with a reason.
    /// Omitted when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignored_in: Vec<LifecycleIgnore>,
}

/// States in which an external event is ignored on purpose.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LifecycleIgnore {
    /// The states; non-empty.
    pub states: Vec<String>,
    /// Why ignoring it there is correct; non-blank.
    pub reason: String,
}

/// What fires a transition (LIFE-SOUND-3). Omitted: an action fires it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TransitionTrigger {
    /// The cell clock: the transition fires once the object has been in a
    /// source state for `millis`. A deadline; always a fact.
    Timer {
        /// Positive canonical unsigned milliseconds, bounded by u64.
        millis: String,
    },
    /// A declared external event of the lifecycle. Always a fact.
    Event {
        /// The [`LifecycleEvent`] id.
        event: String,
    },
}

/// Whether a transition records a fact or asks for a change (LIFE-SOUND-5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TransitionMode {
    /// A request: laws and guards may refuse it. The default.
    Request,
    /// A fact (an observation, an offline sale, a count, a provider result):
    /// never refused. It commits, commits with a variance or shortfall, or is
    /// quarantined. A fact transition carries no guard.
    Fact,
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
            endings: self
                .endings
                .iter()
                .filter(|ending| states.contains(&ending.state))
                .cloned()
                .collect(),
            events: self
                .events
                .iter()
                .map(|event| LifecycleEvent {
                    id: event.id.clone(),
                    ignored_in: event
                        .ignored_in
                        .iter()
                        .map(|ignore| LifecycleIgnore {
                            states: ordered(&ignore.states),
                            reason: ignore.reason.clone(),
                        })
                        .filter(|ignore| !ignore.states.is_empty())
                        .collect(),
                })
                .collect(),
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
                    guard_refusal_code: transition.guard_refusal_code.clone(),
                    trigger: transition.trigger.clone(),
                    mode: transition.mode,
                    compensated_by: transition.compensated_by.clone(),
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
    /// GUARD-DIAGNOSTIC-1: registered T6 error code for guard refusals.
    /// Omission inherits a parent's diagnostic; guard evidence is retained.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub guard_refusal_code: Option<String>,
    /// LIFE-SOUND-3: a timer or external event that fires this transition.
    /// Omitted: an action fires it.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "TransitionTrigger")]
    pub trigger: Option<TransitionTrigger>,
    /// LIFE-SOUND-5: fact or request. Omitted: a request, unless a trigger
    /// makes it a fact.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "TransitionMode")]
    pub mode: Option<TransitionMode>,
    /// LIFE-SOUND-8: this transition dispatches an irreversible external
    /// effect, and the named transition compensates it. A run that dispatched
    /// it may end `cancelled`, `compensated` or `breached_visible` only after
    /// the compensating transition. Omitted when the transition dispatches none.
    #[serde(
        default,
        deserialize_with = "present_non_null",
        skip_serializing_if = "Option::is_none"
    )]
    #[schemars(with = "String")]
    pub compensated_by: Option<TransitionId>,
}

impl Transition {
    /// Whether the transition records a fact: a timer or event trigger, or
    /// `mode: fact`.
    #[must_use]
    pub fn is_fact(&self) -> bool {
        self.trigger.is_some() || self.mode == Some(TransitionMode::Fact)
    }
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
#[must_use]
pub fn effective_lifecycles<'a>(
    lookup: &dyn Fn(&str) -> Option<&'a ObjectTypeDef>,
    extensions: &[&ObjectTypeExtension],
    name: &str,
) -> BTreeMap<PropertyId, Lifecycle> {
    let no_interfaces = |_name: &str| -> Option<&'a InterfaceDef> { None };
    effective_lifecycles_with_interfaces(lookup, &no_interfaces, extensions, name)
}

/// Like [`effective_lifecycles`], also inheriting lifecycles declared on
/// implemented interfaces (directly, through supertype `implements`, and
/// transitively through interface `extends`, `INTERFACE-LIFECYCLE-1`).
///
/// When multiple inherited interfaces or a supertype and an implemented
/// interface govern the same property, their machines combine by
/// non-weakening meet (intersection of initial states and transitions, union
/// of terminal and immutable states, conjunction of guards and path
/// properties). Conflicting definitions (different targets, fact/request
/// modes, triggers, compensations, guard diagnostics, ending outcomes, or
/// empty initial state intersection) omit the property so enforcers fail
/// closed via [`declared_lifecycle_properties_with_interfaces`].
#[must_use]
pub fn effective_lifecycles_with_interfaces<'a>(
    lookup: &dyn Fn(&str) -> Option<&'a ObjectTypeDef>,
    interfaces: &dyn Fn(&str) -> Option<&'a InterfaceDef>,
    extensions: &[&ObjectTypeExtension],
    name: &str,
) -> BTreeMap<PropertyId, Lifecycle> {
    let chain = lineage(lookup, name);
    let mut machines: BTreeMap<PropertyId, Option<Lifecycle>> = BTreeMap::new();
    let mut seen_interfaces: BTreeSet<&'a str> = BTreeSet::new();
    let mut interface_cache: InterfaceLifecycleCache<'a> = BTreeMap::new();
    // Root first; `visible` is the type and its supertypes, nearest first.
    for depth in (0..chain.len()).rev() {
        let visible = chain.get(depth..).unwrap_or(&[]);
        let Some(def) = visible.first() else {
            continue;
        };
        let states = |property: &PropertyId| -> Option<Vec<String>> {
            visible_enum_states(visible, extensions, property)
        };
        let previous = std::mem::take(&mut machines);
        let mut inherited = restrict_inherited(previous, &states);
        fold_level_interfaces(
            interfaces,
            def,
            &states,
            &mut seen_interfaces,
            &mut interface_cache,
            &mut inherited,
        );
        machines.clone_from(&inherited);
        apply_declared_lifecycles(&def.lifecycles, &states, &inherited, &mut machines);
    }
    machines
        .into_iter()
        .filter_map(|(property, lifecycle)| lifecycle.map(|lc| (property, lc)))
        .collect()
}

fn visible_enum_states(
    visible: &[&ObjectTypeDef],
    extensions: &[&ObjectTypeExtension],
    property: &PropertyId,
) -> Option<Vec<String>> {
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
        .filter(|member| !member.is_read_only())
        .and_then(|member| inline_enum_values(&member.property_type))
}

fn restrict_inherited(
    previous: BTreeMap<PropertyId, Option<Lifecycle>>,
    states: &dyn Fn(&PropertyId) -> Option<Vec<String>>,
) -> BTreeMap<PropertyId, Option<Lifecycle>> {
    let mut inherited = BTreeMap::new();
    for (property, lifecycle) in previous {
        match lifecycle {
            None => {
                inherited.insert(property, None);
            }
            Some(lifecycle) => {
                if let Some(prop_states) = states(&property) {
                    inherited.insert(property, Some(lifecycle.restricted(&prop_states)));
                }
            }
        }
    }
    inherited
}

type InterfaceLifecycleCache<'a> =
    BTreeMap<&'a str, (BTreeMap<PropertyId, Lifecycle>, BTreeSet<PropertyId>)>;

fn fold_level_interfaces<'a>(
    interfaces: &dyn Fn(&str) -> Option<&'a InterfaceDef>,
    def: &'a ObjectTypeDef,
    states: &dyn Fn(&PropertyId) -> Option<Vec<String>>,
    seen_interfaces: &mut BTreeSet<&'a str>,
    cache: &mut InterfaceLifecycleCache<'a>,
    inherited: &mut BTreeMap<PropertyId, Option<Lifecycle>>,
) {
    let mut level_interfaces: BTreeSet<&'a str> = BTreeSet::new();
    let mut level_owners: Vec<&'a str> = Vec::new();
    for iface_name in &def.implements {
        if seen_interfaces.contains(iface_name.as_str()) {
            continue;
        }
        let Some(iface_def) = interfaces(iface_name.as_str()) else {
            continue;
        };
        if !level_interfaces.insert(iface_def.name.as_str()) {
            continue;
        }
        let owners = iface_def.contract_owners(|n: &InterfaceName| interfaces(n.as_str()));
        if owners.iter().any(|owner| !owner.lifecycles.is_empty()) {
            let (iface_machines, iface_conflicts) =
                interface_effective_lifecycles(interfaces, &owners, iface_def, cache);
            merge_inherited_lifecycles(iface_machines, iface_conflicts, states, inherited);
        }
        for owner in owners {
            level_owners.push(owner.name.as_str());
        }
    }
    seen_interfaces.extend(level_owners);
}

fn merge_inherited_lifecycles(
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

fn apply_declared_lifecycles(
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

fn inline_enum_values(property_type: &PropertyType) -> Option<Vec<String>> {
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
fn interface_effective_lifecycles<'a>(
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
#[must_use]
pub fn declared_lifecycle_properties<'a>(
    lookup: &dyn Fn(&str) -> Option<&'a ObjectTypeDef>,
    name: &str,
) -> BTreeSet<PropertyId> {
    let no_interfaces = |_name: &str| -> Option<&'a InterfaceDef> { None };
    declared_lifecycle_properties_with_interfaces(lookup, &no_interfaces, name)
}

/// Like [`declared_lifecycle_properties`], also including lifecycle properties
/// declared on implemented interfaces (directly, through supertypes, and
/// transitively through interface `extends`, `INTERFACE-LIFECYCLE-1`).
#[must_use]
pub fn declared_lifecycle_properties_with_interfaces<'a>(
    lookup: &dyn Fn(&str) -> Option<&'a ObjectTypeDef>,
    interfaces: &dyn Fn(&str) -> Option<&'a InterfaceDef>,
    name: &str,
) -> BTreeSet<PropertyId> {
    let chain = lineage(lookup, name);
    let mut properties: BTreeSet<PropertyId> = chain
        .iter()
        .flat_map(|def| {
            def.lifecycles
                .iter()
                .map(|lifecycle| lifecycle.property.clone())
        })
        .collect();
    let mut seen_interfaces: BTreeSet<&'a str> = BTreeSet::new();
    for def in &chain {
        for iface_name in &def.implements {
            let Some(iface_def) = interfaces(iface_name.as_str()) else {
                continue;
            };
            for owner in iface_def.contract_owners(|n: &InterfaceName| interfaces(n.as_str())) {
                if seen_interfaces.insert(owner.name.as_str()) {
                    properties.extend(
                        owner
                            .lifecycles
                            .iter()
                            .map(|lifecycle| lifecycle.property.clone()),
                    );
                }
            }
        }
    }
    properties
}
