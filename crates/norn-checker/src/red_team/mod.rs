//! Terminating `norn-datalog` red-team analysis + unconstrained-write report (`RED-TEAM-1`).
//!
//! Runs purely on admitted IR (zero IO, guaranteed termination via stratified
//! function-free Datalog over the finite artifact domain) and surfaces four
//! high-signal structural security/modeling checks:
//!
//! 1. **Scope Confinement** (`scope_confinement`): a role that uses a set-scope
//!    attribute (`in principalAttribute(Attr)`) on at least one rule, yet holds
//!    another rule grant without checking `Attr` (or an explicit `eq` self-scope)
//!    on a resource type that carries the scoped property (catches branch/warehouse
//!    hopping, keyed per `rule_id` so one scoped rule can never mask an unscoped rule).
//! 2. **Unauthenticated External Settlement** (`unauthenticated_settlements`):
//!    an action firing a `mode: "fact"` or event-triggered transition on an entity
//!    (or ancestor) with declared external `events` or outbound `commands` without
//!    `acceptReceipt`.
//! 3. **Self-Ratifiable Lifecycle Transition** (`self_ratifiable_transitions`):
//!    recursive Datalog reachability finding any `fire_transition` action
//!    reaching a `completed` or `immutable` state without crossing an `SoD` gate
//!    where one role can both create the object and fire the transition without
//!    `independentOf` or per-rule `SoD` on a `protected` property.
//! 4. **Unconstrained Caller Writes** (`unconstrained_writes`): stored
//!    non-identity, non-lifecycle, non-`protected` properties (including inherited
//!    properties) written directly from caller input in an `upsert_input_object`
//!    with zero `Enforced` `LawIR`, `Invariant`, `preconditions`, or `derive`
//!    referencing them.
//!
//! Fail-closed completeness: any Datalog evaluation error (budget exhaustion,
//! non-NFC constant) or undecodable artifact envelope is recorded explicitly in
//! [`RedTeamReport::evaluation_errors`] / [`RedTeamReport::decode_errors`] rather
//! than silently returning empty findings.

mod scope;
mod settlement;
mod sod;
mod writes;

use std::collections::{BTreeMap, BTreeSet};

use norn_datalog::{Atom, Facts, Predicate, RelationKind, Term, Value};
use norn_ir::access::{AccessCondition, AccessRule};
use norn_ir::{
    AccessIr, ActionRoot, ArtifactEnvelope, ArtifactPayload, EntityRoot, InterfaceDef, LawIR,
    Lifecycle, ObjectTypeDef, PolicyRoot, Property,
};
use serde_json::{Map, Value as JsonValue, json};

/// Role granted access to an entity/action without its declared scope attribute.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ScopeConfinementFinding {
    /// Policy rule id holding the unconstrained grant.
    pub rule_id: String,
    /// Scoped role holding the unconstrained grant.
    pub role: String,
    /// Action id granted without the scope predicate.
    pub action_id: String,
    /// Target resource type on the unconstrained rule.
    pub resource_type: String,
    /// Principal set attribute checked on other rules for this role.
    pub missing_attribute: String,
}

impl ScopeConfinementFinding {
    fn to_json_value(&self) -> JsonValue {
        json!({
            "ruleId": self.rule_id,
            "role": self.role,
            "actionId": self.action_id,
            "resourceType": self.resource_type,
            "missingAttribute": self.missing_attribute,
        })
    }
}

/// External `mode: "fact"` transition callable without `acceptReceipt`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct UnauthenticatedSettlementFinding {
    /// Action id firing the `mode: "fact"` transition.
    pub action_id: String,
    /// Target object type carrying external events or commands.
    pub object_type: String,
    /// Lifecycle transition id lacking `acceptReceipt`.
    pub transition_id: String,
}

impl UnauthenticatedSettlementFinding {
    fn to_json_value(&self) -> JsonValue {
        json!({
            "actionId": self.action_id,
            "objectType": self.object_type,
            "transitionId": self.transition_id,
        })
    }
}

/// Transition reaching a completed/immutable state where one role can self-ratify.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SelfRatifiableFinding {
    /// Role able to both create the object and fire the ratifying transition.
    pub role: String,
    /// Transition action id lacking `independentOf` or `SoD` resource guard.
    pub action_id: String,
    /// Target object type with a `protected` creator field.
    pub object_type: String,
    /// Transition id that can reach a completed or immutable state.
    pub transition_id: String,
}

impl SelfRatifiableFinding {
    fn to_json_value(&self) -> JsonValue {
        json!({
            "role": self.role,
            "actionId": self.action_id,
            "objectType": self.object_type,
            "transitionId": self.transition_id,
        })
    }
}

/// Stored property written directly from caller input with zero enforced law,
/// invariant, precondition, or derivation constraint.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct UnconstrainedWriteFinding {
    /// Upsert action accepting the unconstrained property.
    pub action_id: String,
    /// Target object type owning the property.
    pub object_type: String,
    /// Stored property id written directly from caller input.
    pub property: String,
}

impl UnconstrainedWriteFinding {
    fn to_json_value(&self) -> JsonValue {
        json!({
            "actionId": self.action_id,
            "objectType": self.object_type,
            "property": self.property,
        })
    }
}

/// Complete deterministic red-team & unconstrained-write report.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RedTeamReport {
    /// Roles that appear scoped on some rules yet hold unscoped grants elsewhere.
    pub scope_confinement: Vec<ScopeConfinementFinding>,
    /// External `mode: "fact"` transitions callable without `acceptReceipt`.
    pub unauthenticated_settlements: Vec<UnauthenticatedSettlementFinding>,
    /// Sensitive transitions reachable by a creator role without `SoD`.
    pub self_ratifiable_transitions: Vec<SelfRatifiableFinding>,
    /// Caller-written stored properties lacking any enforced law, invariant, precondition, or derive.
    pub unconstrained_writes: Vec<UnconstrainedWriteFinding>,
    /// Datalog evaluation refusals (e.g. budget exhaustion or non-NFC constants).
    pub evaluation_errors: Vec<String>,
    /// Artifact envelopes that failed decoding during extraction.
    pub decode_errors: Vec<String>,
}

impl RedTeamReport {
    /// Returns `true` only when every Datalog program evaluated cleanly and no
    /// artifact failed decoding.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.evaluation_errors.is_empty() && self.decode_errors.is_empty()
    }

    /// Canonical `camelCase` JSON representation for CLI/WASM projections.
    #[must_use]
    pub fn to_json_value(&self) -> JsonValue {
        let mut map = Map::new();
        map.insert(
            "scopeConfinement".to_owned(),
            JsonValue::Array(
                self.scope_confinement
                    .iter()
                    .map(ScopeConfinementFinding::to_json_value)
                    .collect(),
            ),
        );
        map.insert(
            "unauthenticatedSettlements".to_owned(),
            JsonValue::Array(
                self.unauthenticated_settlements
                    .iter()
                    .map(UnauthenticatedSettlementFinding::to_json_value)
                    .collect(),
            ),
        );
        map.insert(
            "selfRatifiableTransitions".to_owned(),
            JsonValue::Array(
                self.self_ratifiable_transitions
                    .iter()
                    .map(SelfRatifiableFinding::to_json_value)
                    .collect(),
            ),
        );
        map.insert(
            "unconstrainedWrites".to_owned(),
            JsonValue::Array(
                self.unconstrained_writes
                    .iter()
                    .map(UnconstrainedWriteFinding::to_json_value)
                    .collect(),
            ),
        );
        if !self.evaluation_errors.is_empty() {
            map.insert("evaluationErrors".to_owned(), json!(self.evaluation_errors));
        }
        if !self.decode_errors.is_empty() {
            map.insert("decodeErrors".to_owned(), json!(self.decode_errors));
        }
        JsonValue::Object(map)
    }
}

pub(super) fn s(text: &str) -> Value {
    Value::Text(text.to_owned())
}

pub(super) fn atom(predicate: &str, vars: &[&str]) -> Atom {
    Atom {
        predicate: predicate.to_owned(),
        terms: vars
            .iter()
            .map(|name| Term::Variable((*name).to_owned()))
            .collect(),
    }
}

pub(super) fn pred(name: &str, arity: usize, kind: RelationKind) -> Predicate {
    Predicate {
        name: name.to_owned(),
        arity,
        kind,
    }
}

pub(super) fn push_fact(facts: &mut Facts, pred_name: &str, tuple: Vec<Value>) {
    facts.entry(pred_name.to_owned()).or_default().push(tuple);
}

pub(super) fn as_str(val: &Value) -> &str {
    match val {
        Value::Text(text) => text.as_str(),
        Value::Bool(_) | Value::Scope(_) => "",
    }
}

pub(super) struct ExtractedSet {
    pub(super) object_types: BTreeMap<String, ObjectTypeDef>,
    pub(super) interfaces: BTreeMap<String, InterfaceDef>,
    pub(super) actions: Vec<ActionRoot>,
    pub(super) policies: Vec<AccessIr>,
    pub(super) laws: Vec<(String, LawIR)>,
    pub(super) decode_errors: Vec<String>,
}

fn extract_set(artifacts: &[ArtifactEnvelope]) -> ExtractedSet {
    let mut object_types = BTreeMap::new();
    let mut interfaces = BTreeMap::new();
    let mut actions = Vec::new();
    let mut policies = Vec::new();
    let mut laws = Vec::new();
    let mut decode_errors = Vec::new();
    for artifact in artifacts {
        match &artifact.payload {
            ArtifactPayload::Entity(entity) if entity.is_entity_ir_v2() => {
                match entity.to_json_value().decode::<EntityRoot>() {
                    Ok(EntityRoot::V2(root)) => {
                        let ty = root.object_type;
                        object_types.insert(ty.name.as_str().to_owned(), ty);
                    }
                    Ok(EntityRoot::V2Interface(iface)) => {
                        interfaces
                            .insert(iface.interface.name.as_str().to_owned(), iface.interface);
                    }
                    Ok(
                        EntityRoot::V2Extension(_)
                        | EntityRoot::V2PropertyTypes(_)
                        | EntityRoot::V2Markings(_)
                        | EntityRoot::V1(_),
                    ) => {}
                    Err(err) => decode_errors.push(format!("{}: {err}", artifact.artifact_id)),
                }
            }
            ArtifactPayload::Entity(_) => {}
            ArtifactPayload::Action(action) => {
                match action.to_json_value().decode::<ActionRoot>() {
                    Ok(root) => actions.push(root),
                    Err(err) => decode_errors.push(format!("{}: {err}", artifact.artifact_id)),
                }
            }
            ArtifactPayload::Policy(policy) => {
                match policy.to_json_value().decode::<PolicyRoot>() {
                    Ok(PolicyRoot::Access(access)) => policies.push(access),
                    Ok(PolicyRoot::Obligations(_) | PolicyRoot::Approval(_)) => {}
                    Err(err) => decode_errors.push(format!("{}: {err}", artifact.artifact_id)),
                }
            }
            ArtifactPayload::Law(law_payload) => {
                match serde_json::to_value(law_payload.to_json_value())
                    .and_then(serde_json::from_value::<LawIR>)
                {
                    Ok(law) => laws.push((artifact.artifact_id.clone(), law)),
                    Err(err) => decode_errors.push(format!("{}: {err}", artifact.artifact_id)),
                }
            }
            ArtifactPayload::Relation(_)
            | ArtifactPayload::Rule(_)
            | ArtifactPayload::Connector(_)
            | ArtifactPayload::Widget(_)
            | ArtifactPayload::Customization(_)
            | ArtifactPayload::Setting(_)
            | ArtifactPayload::BusinessPack(_)
            | ArtifactPayload::Derived(_)
            | ArtifactPayload::SourceSchema(_)
            | ArtifactPayload::Mapping(_)
            | ArtifactPayload::DefinedConcept(_)
            | ArtifactPayload::Labels(_)
            | ArtifactPayload::Model(_) => {}
        }
    }
    decode_errors.sort();
    ExtractedSet {
        object_types,
        interfaces,
        actions,
        policies,
        laws,
        decode_errors,
    }
}

pub(super) fn effective_object_properties<'a>(
    set: &'a ExtractedSet,
    ty: &'a ObjectTypeDef,
) -> Vec<&'a Property> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    let mut visited = BTreeSet::new();
    let mut current = Some(ty);
    while let Some(def) = current {
        if !visited.insert(def.name.as_str()) {
            break;
        }
        for prop in &def.properties {
            if seen.insert(prop.id.as_str()) {
                out.push(prop);
            }
        }
        current = def
            .extends
            .as_ref()
            .and_then(|parent| set.object_types.get(parent.as_str()));
    }
    out
}

pub(super) fn effective_object_lifecycles<'a>(
    set: &'a ExtractedSet,
    ty: &'a ObjectTypeDef,
) -> Vec<&'a Lifecycle> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    let mut visited = BTreeSet::new();
    let mut current = Some(ty);
    while let Some(def) = current {
        if !visited.insert(def.name.as_str()) {
            break;
        }
        for lc in &def.lifecycles {
            if seen.insert(lc.property.as_str()) {
                out.push(lc);
            }
        }
        current = def
            .extends
            .as_ref()
            .and_then(|parent| set.object_types.get(parent.as_str()));
    }
    out
}

pub(super) fn type_governed_subjects(set: &ExtractedSet, ty: &ObjectTypeDef) -> BTreeSet<String> {
    let mut subjects = BTreeSet::new();
    let mut visited = BTreeSet::new();
    let mut current = Some(ty);
    while let Some(def) = current {
        if !visited.insert(def.name.as_str()) {
            break;
        }
        subjects.insert(def.name.as_str().to_owned());
        for iface_name in &def.implements {
            subjects.insert(iface_name.as_str().to_owned());
            if let Some(iface) = set.interfaces.get(iface_name.as_str()) {
                for owner in iface.contract_owners(|name| set.interfaces.get(name.as_str())) {
                    subjects.insert(owner.name.as_str().to_owned());
                }
            }
        }
        current = def
            .extends
            .as_ref()
            .and_then(|parent| set.object_types.get(parent.as_str()));
    }
    subjects
}

pub(super) fn effective_object_key_props<'a>(
    set: &'a ExtractedSet,
    ty: &'a ObjectTypeDef,
) -> BTreeSet<&'a str> {
    let mut keys = BTreeSet::new();
    let mut visited = BTreeSet::new();
    let mut current = Some(ty);
    while let Some(def) = current {
        if !visited.insert(def.name.as_str()) {
            break;
        }
        for id in &def.identity {
            for prop in &id.properties {
                keys.insert(prop.as_str());
            }
        }
        current = def
            .extends
            .as_ref()
            .and_then(|parent| set.object_types.get(parent.as_str()));
    }
    keys
}

pub(super) fn effective_protected_props<'a>(
    set: &'a ExtractedSet,
    ty: &'a ObjectTypeDef,
) -> BTreeSet<&'a str> {
    let mut protected = BTreeSet::new();
    let mut visited = BTreeSet::new();
    let mut current = Some(ty);
    while let Some(def) = current {
        if !visited.insert(def.name.as_str()) {
            break;
        }
        for prop in &def.properties {
            if prop.protected {
                protected.insert(prop.id.as_str());
            }
        }
        current = def
            .extends
            .as_ref()
            .and_then(|parent| set.object_types.get(parent.as_str()));
    }
    protected
}

pub(super) fn collect_rule_roles(rule: &AccessRule) -> Vec<String> {
    let mut roles = Vec::new();
    for cond in &rule.all_of {
        if let AccessCondition::Role(role) = cond {
            roles.extend(role.any_of.iter().cloned());
        }
    }
    roles
}

pub(super) fn is_principal_identity_attr(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    matches!(
        lower.as_str(),
        "id" | "subject_id"
            | "subjectid"
            | "actor_id"
            | "actorid"
            | "user_id"
            | "userid"
            | "principal_id"
            | "principalid"
    )
}

/// Executes all three terminating `norn-datalog` red-team checks and the
/// unconstrained-write report over an artifact set.
#[must_use]
pub fn analyze_red_team(artifacts: &[ArtifactEnvelope]) -> RedTeamReport {
    let set = extract_set(artifacts);
    let mut evaluation_errors = Vec::new();
    let scope_confinement = scope::run_scope_confinement(&set, &mut evaluation_errors);
    let unauthenticated_settlements =
        settlement::run_external_settlement_check(&set, &mut evaluation_errors);
    let self_ratifiable_transitions = sod::run_sod_reachability(&set, &mut evaluation_errors);
    let unconstrained_writes = writes::collect_unconstrained_writes(&set);
    RedTeamReport {
        scope_confinement,
        unauthenticated_settlements,
        self_ratifiable_transitions,
        unconstrained_writes,
        evaluation_errors,
        decode_errors: set.decode_errors,
    }
}
