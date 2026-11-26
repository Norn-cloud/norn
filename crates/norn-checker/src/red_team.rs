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
//!    an action firing a `mode: "fact"` transition on an entity (or ancestor) with
//!    declared external `events` or outbound `commands` without `acceptReceipt`.
//! 3. **Self-Ratifiable Lifecycle Transition** (`self_ratifiable_transitions`):
//!    recursive Datalog reachability finding any `fire_transition` action
//!    reaching a `completed` or `immutable` state without crossing an `SoD` gate
//!    where one role can both create the object and fire the transition without
//!    `independentOf` or per-rule `SoD`.
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

use std::collections::{BTreeMap, BTreeSet};

use norn_datalog::{
    Atom, Budget, ExecBudget, Facts, Literal, Mode, Predicate, Program, RelationKind, Rule, Term,
    Value, evaluate,
};
use norn_ir::access::{AccessCondition, AccessRule, ResourceCompareOp, ResourceOperand};
use norn_ir::{
    AccessIr, ActionEffectProgram, ActionRoot, ArtifactEnvelope, ArtifactKind, ArtifactPayload,
    EntityRoot, InterfaceDef, LawIR, LawResponse, Lifecycle, LifecycleOutcome, ObjectTypeDef,
    PolicyRoot, Property, UpsertWriteMode,
};
use serde::{Deserialize, Serialize};

use crate::law_honesty::{LawEnforcementStatus, classify_law};

/// Role granted access to an entity/action without its declared scope attribute.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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

/// External `mode: "fact"` transition callable without `acceptReceipt`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnauthenticatedSettlementFinding {
    /// Action id firing the `mode: "fact"` transition.
    pub action_id: String,
    /// Target object type carrying external events or commands.
    pub object_type: String,
    /// Lifecycle transition id lacking `acceptReceipt`.
    pub transition_id: String,
}

/// Transition reaching a completed/immutable state where one role can self-ratify.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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

/// Stored property written directly from caller input with zero enforced law,
/// invariant, precondition, or derivation constraint.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnconstrainedWriteFinding {
    /// Upsert action accepting the unconstrained property.
    pub action_id: String,
    /// Target object type owning the property.
    pub object_type: String,
    /// Stored property id written directly from caller input.
    pub property: String,
}

/// Complete deterministic red-team & unconstrained-write report.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evaluation_errors: Vec<String>,
    /// Artifact envelopes that failed decoding during extraction.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decode_errors: Vec<String>,
}

impl RedTeamReport {
    /// Returns `true` only when every Datalog program evaluated cleanly and no
    /// artifact failed decoding.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.evaluation_errors.is_empty() && self.decode_errors.is_empty()
    }
}

fn s(text: &str) -> Value {
    Value::Text(text.to_owned())
}

fn v(name: &str) -> Term {
    Term::Variable(name.to_owned())
}

fn atom(predicate: &str, vars: &[&str]) -> Atom {
    Atom {
        predicate: predicate.to_owned(),
        terms: vars.iter().map(|name| v(name)).collect(),
    }
}

fn pred(name: &str, arity: usize, kind: RelationKind) -> Predicate {
    Predicate {
        name: name.to_owned(),
        arity,
        kind,
    }
}

fn push_fact(facts: &mut Facts, pred_name: &str, tuple: Vec<Value>) {
    facts.entry(pred_name.to_owned()).or_default().push(tuple);
}

fn as_str(val: &Value) -> &str {
    match val {
        Value::Text(text) => text.as_str(),
        Value::Bool(_) | Value::Scope(_) => "",
    }
}

struct ExtractedSet {
    object_types: BTreeMap<String, ObjectTypeDef>,
    interfaces: BTreeMap<String, InterfaceDef>,
    actions: Vec<ActionRoot>,
    policies: Vec<AccessIr>,
    laws: Vec<(String, LawIR)>,
    decode_errors: Vec<String>,
}

fn extract_set(artifacts: &[ArtifactEnvelope]) -> ExtractedSet {
    let mut object_types = BTreeMap::new();
    let mut interfaces = BTreeMap::new();
    let mut actions = Vec::new();
    let mut policies = Vec::new();
    let mut laws = Vec::new();
    let mut decode_errors = Vec::new();
    for artifact in artifacts {
        match (&artifact.payload, artifact.kind()) {
            (ArtifactPayload::Entity(entity), _) if entity.is_entity_ir_v2() => {
                match entity.to_json_value().decode::<EntityRoot>() {
                    Ok(EntityRoot::V2(root)) => {
                        let ty = root.object_type;
                        object_types.insert(ty.name.as_str().to_owned(), ty);
                    }
                    Ok(EntityRoot::V2Interface(iface)) => {
                        interfaces
                            .insert(iface.interface.name.as_str().to_owned(), iface.interface);
                    }
                    Ok(_) => {}
                    Err(err) => decode_errors.push(format!("{}: {err}", artifact.artifact_id)),
                }
            }
            (ArtifactPayload::Action(action), _) => {
                match action.to_json_value().decode::<ActionRoot>() {
                    Ok(root) => actions.push(root),
                    Err(err) => decode_errors.push(format!("{}: {err}", artifact.artifact_id)),
                }
            }
            (ArtifactPayload::Policy(policy), _) => {
                match policy.to_json_value().decode::<PolicyRoot>() {
                    Ok(PolicyRoot::Access(access)) => policies.push(access),
                    Ok(_) => {}
                    Err(err) => decode_errors.push(format!("{}: {err}", artifact.artifact_id)),
                }
            }
            (_, ArtifactKind::Law) => {
                match serde_json::to_value(artifact.payload.to_json_value())
                    .and_then(serde_json::from_value::<LawIR>)
                {
                    Ok(law) => laws.push((artifact.artifact_id.clone(), law)),
                    Err(err) => decode_errors.push(format!("{}: {err}", artifact.artifact_id)),
                }
            }
            _ => {}
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

fn effective_object_properties<'a>(
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

fn effective_object_lifecycles<'a>(
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

fn type_governed_subjects(set: &ExtractedSet, ty: &ObjectTypeDef) -> BTreeSet<String> {
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

fn collect_rule_roles(rule: &AccessRule) -> Vec<String> {
    let mut roles = Vec::new();
    for cond in &rule.all_of {
        if let AccessCondition::Role(role) = cond {
            roles.extend(role.any_of.iter().cloned());
        }
    }
    roles
}

fn collect_rule_scope_pairs(rule: &AccessRule) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    for cond in &rule.all_of {
        if let AccessCondition::Resource(res) = cond
            && matches!(res.op, ResourceCompareOp::In)
            && let ResourceOperand::PrincipalAttribute(pa) = &res.value
        {
            pairs.push((pa.principal_attribute.clone(), res.name.clone()));
        }
    }
    pairs
}

fn is_actor_resource_field(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [
        "requester",
        "requestor",
        "creator",
        "created_by",
        "author",
        "authored_by",
        "submitter",
        "submitted_by",
        "initiator",
        "initiated_by",
        "owner",
        "actor",
        "preparer",
        "prepared_by",
    ]
    .iter()
    .any(|kw| lower.contains(kw))
}

fn effective_object_key_props<'a>(
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

fn effective_protected_props<'a>(
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

fn is_principal_identity_attr(name: &str) -> bool {
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

fn is_self_scope_principal_attr(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    is_principal_identity_attr(name)
        || matches!(
            lower.as_str(),
            "employee_id" | "employeeid" | "person_id" | "personid" | "member_id" | "memberid"
        )
}

fn rule_has_sod(rule: &AccessRule) -> bool {
    rule.all_of.iter().any(|cond| {
        matches!(
            cond,
            AccessCondition::Resource(res)
                if matches!(res.op, ResourceCompareOp::Ne)
                    && is_actor_resource_field(&res.name)
                    && matches!(
                        &res.value,
                        ResourceOperand::PrincipalAttribute(pa)
                            if is_principal_identity_attr(&pa.principal_attribute)
                    )
        )
    })
}

fn rule_has_self_scope(rule: &AccessRule) -> bool {
    rule.all_of.iter().any(|cond| {
        matches!(
            cond,
            AccessCondition::Resource(res)
                if matches!(res.op, ResourceCompareOp::Eq)
                    && matches!(
                        &res.value,
                        ResourceOperand::PrincipalAttribute(pa)
                            if is_self_scope_principal_attr(&pa.principal_attribute)
                    )
        )
    })
}

fn collect_scope_facts(set: &ExtractedSet) -> Facts {
    let mut facts = Facts::new();
    let mut scoped_props = BTreeSet::<String>::new();
    let mut seen_role_scoped = BTreeSet::<(String, String, String)>::new();
    for policy in &set.policies {
        for (rule_idx, rule) in policy.rules.iter().enumerate() {
            if rule.resource_type.as_str() == "norn.Marking" {
                continue;
            }
            let rule_key = format!("{}#{}#{}", policy.policy_id, rule.rule_id, rule_idx);
            let roles = collect_rule_roles(rule);
            let scope_pairs = collect_rule_scope_pairs(rule);
            let self_scoped = rule_has_self_scope(rule);
            for (_, prop) in &scope_pairs {
                scoped_props.insert(prop.clone());
            }
            if !roles.is_empty() && !rule.actions.is_empty() {
                if self_scoped {
                    push_fact(&mut facts, "self_scoped_grant", vec![s(&rule_key)]);
                }
                for (attr, prop) in &scope_pairs {
                    push_fact(
                        &mut facts,
                        "grant_checks_attr",
                        vec![s(&rule_key), s(attr), s(prop)],
                    );
                }
            }
            for role in &roles {
                for (attr, prop) in &scope_pairs {
                    if seen_role_scoped.insert((role.clone(), attr.clone(), prop.clone())) {
                        push_fact(&mut facts, "role_scoped", vec![s(role), s(attr), s(prop)]);
                    }
                }
                for action_id in &rule.actions {
                    push_fact(
                        &mut facts,
                        "role_grants",
                        vec![
                            s(&rule_key),
                            s(&rule.rule_id),
                            s(role),
                            s(action_id),
                            s(rule.resource_type.as_str()),
                        ],
                    );
                }
            }
        }
    }
    let mut seen_types = BTreeSet::<&str>::new();
    for policy in &set.policies {
        for rule in &policy.rules {
            let ty_name = rule.resource_type.as_str();
            if ty_name == "norn.Marking" || !seen_types.insert(ty_name) {
                continue;
            }
            if let Some(ty) = set.object_types.get(ty_name) {
                let props: BTreeSet<&str> = effective_object_properties(set, ty)
                    .into_iter()
                    .map(|p| p.id.as_str())
                    .collect();
                for prop in &scoped_props {
                    if props.contains(prop.as_str()) {
                        push_fact(
                            &mut facts,
                            "type_applicable_prop",
                            vec![s(ty_name), s(prop)],
                        );
                    }
                }
            } else {
                // Unknown resource type in partial artifact set: fail closed for all scoped props.
                for prop in &scoped_props {
                    push_fact(
                        &mut facts,
                        "type_applicable_prop",
                        vec![s(ty_name), s(prop)],
                    );
                }
            }
        }
    }
    facts
}

fn run_scope_confinement(
    set: &ExtractedSet,
    eval_errors: &mut Vec<String>,
) -> Vec<ScopeConfinementFinding> {
    let program = Program {
        version: norn_datalog::Version::V1,
        predicates: vec![
            pred("role_scoped", 3, RelationKind::Input),
            pred("role_grants", 5, RelationKind::Input),
            pred("grant_checks_attr", 3, RelationKind::Input),
            pred("self_scoped_grant", 1, RelationKind::Input),
            pred("type_applicable_prop", 2, RelationKind::Input),
            pred("unscoped_grant", 5, RelationKind::Derived),
        ],
        rules: vec![Rule {
            id: "scope_confinement".to_owned(),
            mode: Mode::Validation,
            head: atom("unscoped_grant", &["RuleLabel", "R", "A", "T", "Attr"]),
            body: vec![
                Literal::Positive(atom("role_scoped", &["R", "Attr", "Prop"])),
                Literal::Positive(atom(
                    "role_grants",
                    &["RuleKey", "RuleLabel", "R", "A", "T"],
                )),
                Literal::Positive(atom("type_applicable_prop", &["T", "Prop"])),
                Literal::Negative(atom("grant_checks_attr", &["RuleKey", "Attr", "Prop"])),
                Literal::Negative(atom("self_scoped_grant", &["RuleKey"])),
            ],
        }],
    };
    let facts = collect_scope_facts(set);
    let eval = match evaluate(&program, &facts, Budget::default(), ExecBudget::default()) {
        Ok(eval) => eval,
        Err(err) => {
            eval_errors.push(format!("scope_confinement: {err:?}"));
            return Vec::new();
        }
    };
    let mut out: Vec<ScopeConfinementFinding> = eval
        .relations
        .into_iter()
        .find(|rel| rel.predicate == "unscoped_grant")
        .into_iter()
        .flat_map(|rel| rel.tuples)
        .filter_map(|tuple| match tuple.as_slice() {
            [rule_id, r, a, t, attr] => Some(ScopeConfinementFinding {
                rule_id: as_str(rule_id).to_owned(),
                role: as_str(r).to_owned(),
                action_id: as_str(a).to_owned(),
                resource_type: as_str(t).to_owned(),
                missing_attribute: as_str(attr).to_owned(),
            }),
            _ => None,
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

fn run_external_settlement_check(
    set: &ExtractedSet,
    eval_errors: &mut Vec<String>,
) -> Vec<UnauthenticatedSettlementFinding> {
    let program = Program {
        version: norn_datalog::Version::V1,
        predicates: vec![
            pred("has_external_event", 1, RelationKind::Input),
            pred("fact_transition", 3, RelationKind::Input),
            pred("uses_receipt", 1, RelationKind::Input),
            pred("unauth_settlement", 3, RelationKind::Derived),
        ],
        rules: vec![Rule {
            id: "receipt_bound_external_fact".to_owned(),
            mode: Mode::Validation,
            head: atom("unauth_settlement", &["A", "T", "Tr"]),
            body: vec![
                Literal::Positive(atom("has_external_event", &["T"])),
                Literal::Positive(atom("fact_transition", &["A", "T", "Tr"])),
                Literal::Negative(atom("uses_receipt", &["A"])),
            ],
        }],
    };
    let mut facts = Facts::new();
    let mut seen_external = BTreeSet::<String>::new();
    for (type_name, ty) in &set.object_types {
        if effective_object_lifecycles(set, ty)
            .iter()
            .any(|lc| !lc.events.is_empty())
            && seen_external.insert(type_name.clone())
        {
            push_fact(&mut facts, "has_external_event", vec![s(type_name)]);
        }
    }
    for action in &set.actions {
        if !action.commands.is_empty() {
            if let Some(program) = &action.effect_program {
                let kind = program.target().object_kind.to_owned();
                if seen_external.insert(kind.clone()) {
                    push_fact(&mut facts, "has_external_event", vec![s(&kind)]);
                }
            }
            for write in &action.writes {
                if seen_external.insert(write.entity_kind.clone()) {
                    push_fact(
                        &mut facts,
                        "has_external_event",
                        vec![s(&write.entity_kind)],
                    );
                }
            }
        }
        if let Some(ActionEffectProgram::FireTransition {
            object_kind,
            lifecycle,
            transition,
            accept_receipt,
            ..
        }) = &action.effect_program
        {
            if accept_receipt.is_some() {
                push_fact(&mut facts, "uses_receipt", vec![s(&action.action_id)]);
            }
            let is_fact = set
                .object_types
                .get(object_kind)
                .and_then(|ty| {
                    effective_object_lifecycles(set, ty)
                        .into_iter()
                        .find(|lc| lc.property == *lifecycle)
                })
                .and_then(|lc| lc.transitions.iter().find(|tr| tr.id == *transition))
                .is_some_and(norn_ir::Transition::is_fact);
            if is_fact {
                push_fact(
                    &mut facts,
                    "fact_transition",
                    vec![s(&action.action_id), s(object_kind), s(transition.as_str())],
                );
            }
        }
    }
    let eval = match evaluate(&program, &facts, Budget::default(), ExecBudget::default()) {
        Ok(eval) => eval,
        Err(err) => {
            eval_errors.push(format!("external_settlement: {err:?}"));
            return Vec::new();
        }
    };
    let mut out: Vec<UnauthenticatedSettlementFinding> = eval
        .relations
        .into_iter()
        .find(|rel| rel.predicate == "unauth_settlement")
        .into_iter()
        .flat_map(|rel| rel.tuples)
        .filter_map(|tuple| match tuple.as_slice() {
            [a, t, tr] => Some(UnauthenticatedSettlementFinding {
                action_id: as_str(a).to_owned(),
                object_type: as_str(t).to_owned(),
                transition_id: as_str(tr).to_owned(),
            }),
            _ => None,
        })
        .collect();
    out.sort();
    out
}

fn collect_sod_entity_facts(set: &ExtractedSet, facts: &mut Facts) {
    for (type_name, ty) in &set.object_types {
        if !effective_protected_props(set, ty).is_empty() {
            push_fact(facts, "protected_creator", vec![s(type_name)]);
        }
        for lc in effective_object_lifecycles(set, ty) {
            let lc_prop = lc.property.as_str();
            for state in &lc.immutable {
                push_fact(
                    facts,
                    "sensitive_state",
                    vec![s(type_name), s(lc_prop), s(state)],
                );
            }
            for ending in &lc.endings {
                if matches!(ending.outcome, LifecycleOutcome::Completed) {
                    push_fact(
                        facts,
                        "sensitive_state",
                        vec![s(type_name), s(lc_prop), s(&ending.state)],
                    );
                }
            }
            for tr in &lc.transitions {
                push_fact(
                    facts,
                    "transition_target",
                    vec![s(type_name), s(lc_prop), s(tr.id.as_str()), s(&tr.to)],
                );
                for from in &tr.from {
                    push_fact(
                        facts,
                        "transition_edge",
                        vec![
                            s(type_name),
                            s(lc_prop),
                            s(tr.id.as_str()),
                            s(from),
                            s(&tr.to),
                        ],
                    );
                }
            }
        }
    }
}

fn collect_sod_facts(set: &ExtractedSet) -> Facts {
    let mut facts = Facts::new();
    collect_sod_entity_facts(set, &mut facts);
    // Track per-action AccessIR SoD coverage: an action has universal policy SoD
    // when at least one rule grants it and EVERY rule granting it enforces SoD.
    let mut action_policy_grants = BTreeMap::<&str, (usize, usize)>::new();
    let mut all_policy_roles = BTreeSet::<String>::new();
    all_policy_roles.insert("*".to_owned());
    for policy in &set.policies {
        for rule in &policy.rules {
            all_policy_roles.extend(collect_rule_roles(rule));
            let sod = rule_has_sod(rule);
            for action_id in &rule.actions {
                let entry = action_policy_grants.entry(action_id.as_str()).or_default();
                entry.0 = entry.0.saturating_add(1);
                if sod {
                    entry.1 = entry.1.saturating_add(1);
                }
            }
        }
    }
    let mut create_actions = BTreeMap::<String, String>::new();
    let mut transition_actions = BTreeMap::<String, (String, String, String)>::new();
    for action in &set.actions {
        let universal_policy_sod = action_policy_grants
            .get(action.action_id.as_str())
            .is_some_and(|&(total, with_sod)| total > 0 && total == with_sod);
        if !action.independent_of.is_empty() {
            push_fact(&mut facts, "action_independent", vec![s(&action.action_id)]);
        }
        if !action.independent_of.is_empty() || universal_policy_sod {
            push_fact(&mut facts, "action_has_sod", vec![s(&action.action_id)]);
        }
        match &action.effect_program {
            Some(ActionEffectProgram::UpsertInputObject {
                object_kind,
                write_mode: UpsertWriteMode::CreateOnly | UpsertWriteMode::Upsert,
                ..
            }) => {
                create_actions.insert(action.action_id.clone(), object_kind.clone());
            }
            Some(ActionEffectProgram::FireTransition {
                object_kind,
                lifecycle,
                transition,
                accept_receipt,
                ..
            }) => {
                if accept_receipt.is_some() {
                    push_fact(&mut facts, "uses_receipt", vec![s(&action.action_id)]);
                }
                push_fact(
                    &mut facts,
                    "transition_action",
                    vec![
                        s(&action.action_id),
                        s(object_kind),
                        s(lifecycle.as_str()),
                        s(transition.as_str()),
                    ],
                );
                transition_actions.insert(
                    action.action_id.clone(),
                    (
                        object_kind.clone(),
                        lifecycle.as_str().to_owned(),
                        transition.as_str().to_owned(),
                    ),
                );
            }
            _ => {}
        }
    }
    collect_sod_policy_facts(
        set,
        &all_policy_roles,
        &create_actions,
        &transition_actions,
        &mut facts,
    );
    facts
}

fn collect_sod_policy_facts(
    set: &ExtractedSet,
    all_policy_roles: &BTreeSet<String>,
    create_actions: &BTreeMap<String, String>,
    transition_actions: &BTreeMap<String, (String, String, String)>,
    facts: &mut Facts,
) {
    let mut seen_create_grant = BTreeSet::<(String, String)>::new();
    for policy in &set.policies {
        for (idx, rule) in policy.rules.iter().enumerate() {
            let rule_key = format!("{}#{}#{}", policy.policy_id, rule.rule_id, idx);
            let explicit_roles = collect_rule_roles(rule);
            let effective_roles: Vec<&str> = if explicit_roles.is_empty() {
                all_policy_roles.iter().map(String::as_str).collect()
            } else {
                explicit_roles.iter().map(String::as_str).collect()
            };
            let sod = rule_has_sod(rule);
            let mut rule_has_transition = false;
            for action_id in &rule.actions {
                if let Some(ty) = create_actions.get(action_id) {
                    for role in &effective_roles {
                        if seen_create_grant.insert(((*role).to_owned(), ty.clone())) {
                            push_fact(facts, "create_grant", vec![s(role), s(ty)]);
                        }
                    }
                }
                if let Some((ty, lc, tr)) = transition_actions.get(action_id) {
                    rule_has_transition = true;
                    for role in &effective_roles {
                        push_fact(
                            facts,
                            "transition_grant",
                            vec![s(&rule_key), s(role), s(action_id), s(ty), s(lc), s(tr)],
                        );
                    }
                }
            }
            if sod && rule_has_transition {
                push_fact(facts, "rule_has_sod", vec![s(&rule_key)]);
            }
        }
    }
}

fn sod_reachability_program() -> Program {
    Program {
        version: norn_datalog::Version::V1,
        predicates: vec![
            pred("protected_creator", 1, RelationKind::Input),
            pred("sensitive_state", 3, RelationKind::Input),
            pred("transition_edge", 5, RelationKind::Input),
            pred("transition_action", 4, RelationKind::Input),
            pred("action_independent", 1, RelationKind::Input),
            pred("action_has_sod", 1, RelationKind::Input),
            pred("rule_has_sod", 1, RelationKind::Input),
            pred("create_grant", 2, RelationKind::Input),
            pred("transition_grant", 6, RelationKind::Input),
            pred("transition_target", 4, RelationKind::Input),
            pred("uses_receipt", 1, RelationKind::Input),
            pred("transition_has_action", 3, RelationKind::Derived),
            pred("edge_without_sod", 4, RelationKind::Derived),
            pred("reaches_sensitive_without_sod", 3, RelationKind::Derived),
            pred("self_ratifiable", 4, RelationKind::Derived),
        ],
        rules: vec![
            Rule {
                id: "transition_has_action_rule".to_owned(),
                mode: Mode::Entailment,
                head: atom("transition_has_action", &["T", "L", "Tr"]),
                body: vec![Literal::Positive(atom(
                    "transition_action",
                    &["A", "T", "L", "Tr"],
                ))],
            },
            // An edge is open if ANY action firing that transition lacks SoD
            // (`independentOf` or universal AccessIR SoD), or if it is a
            // timer/automatic transition with no action at all.
            Rule {
                id: "edge_without_sod_by_action".to_owned(),
                mode: Mode::Entailment,
                head: atom("edge_without_sod", &["T", "L", "S1", "S2"]),
                body: vec![
                    Literal::Positive(atom("transition_edge", &["T", "L", "Tr", "S1", "S2"])),
                    Literal::Positive(atom("transition_action", &["A", "T", "L", "Tr"])),
                    Literal::Negative(atom("action_has_sod", &["A"])),
                ],
            },
            Rule {
                id: "edge_without_sod_auto".to_owned(),
                mode: Mode::Entailment,
                head: atom("edge_without_sod", &["T", "L", "S1", "S2"]),
                body: vec![
                    Literal::Positive(atom("transition_edge", &["T", "L", "Tr", "S1", "S2"])),
                    Literal::Negative(atom("transition_has_action", &["T", "L", "Tr"])),
                ],
            },
            Rule {
                id: "reach_base".to_owned(),
                mode: Mode::Entailment,
                head: atom("reaches_sensitive_without_sod", &["T", "L", "S"]),
                body: vec![Literal::Positive(atom("sensitive_state", &["T", "L", "S"]))],
            },
            Rule {
                id: "reach_step".to_owned(),
                mode: Mode::Entailment,
                head: atom("reaches_sensitive_without_sod", &["T", "L", "S1"]),
                body: vec![
                    Literal::Positive(atom("edge_without_sod", &["T", "L", "S1", "S2"])),
                    Literal::Positive(atom("reaches_sensitive_without_sod", &["T", "L", "S2"])),
                ],
            },
            Rule {
                id: "self_ratifiable_rule".to_owned(),
                mode: Mode::Validation,
                head: atom("self_ratifiable", &["R", "A", "T", "Tr"]),
                body: vec![
                    Literal::Positive(atom("protected_creator", &["T"])),
                    Literal::Positive(atom("create_grant", &["R", "T"])),
                    Literal::Positive(atom(
                        "transition_grant",
                        &["RuleKey", "R", "A", "T", "L", "Tr"],
                    )),
                    Literal::Positive(atom("transition_target", &["T", "L", "Tr", "S"])),
                    Literal::Positive(atom("reaches_sensitive_without_sod", &["T", "L", "S"])),
                    Literal::Negative(atom("action_independent", &["A"])),
                    Literal::Negative(atom("rule_has_sod", &["RuleKey"])),
                    Literal::Negative(atom("uses_receipt", &["A"])),
                ],
            },
        ],
    }
}

fn run_sod_reachability(
    set: &ExtractedSet,
    eval_errors: &mut Vec<String>,
) -> Vec<SelfRatifiableFinding> {
    let program = sod_reachability_program();
    let facts = collect_sod_facts(set);
    let eval = match evaluate(&program, &facts, Budget::default(), ExecBudget::default()) {
        Ok(eval) => eval,
        Err(err) => {
            eval_errors.push(format!("sod_reachability: {err:?}"));
            return Vec::new();
        }
    };
    let mut out: Vec<SelfRatifiableFinding> = eval
        .relations
        .into_iter()
        .find(|rel| rel.predicate == "self_ratifiable")
        .into_iter()
        .flat_map(|rel| rel.tuples)
        .filter_map(|tuple| match tuple.as_slice() {
            [r, a, t, tr] => Some(SelfRatifiableFinding {
                role: as_str(r).to_owned(),
                action_id: as_str(a).to_owned(),
                object_type: as_str(t).to_owned(),
                transition_id: as_str(tr).to_owned(),
            }),
            _ => None,
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

fn collect_governed_properties(set: &ExtractedSet) -> BTreeMap<String, BTreeSet<String>> {
    let mut by_subject = BTreeMap::<String, BTreeSet<String>>::new();
    for (type_name, ty) in &set.object_types {
        let entry = by_subject.entry(type_name.clone()).or_default();
        for inv in &ty.invariants {
            if let Ok(reads) = norn_expr::expression_reads(inv.expression.as_str(), "self") {
                entry.extend(reads.fields);
            }
        }
    }
    for (iface_name, iface) in &set.interfaces {
        let entry = by_subject.entry(iface_name.clone()).or_default();
        for inv in &iface.invariants {
            if let Ok(reads) = norn_expr::expression_reads(inv.expression.as_str(), "self") {
                entry.extend(reads.fields);
            }
        }
    }
    for (artifact_id, law) in &set.laws {
        // Only credit laws actually enforced on commit, never declared-only laws.
        if classify_law(artifact_id, law).enforcement != LawEnforcementStatus::Enforced {
            continue;
        }
        let entry = by_subject.entry(law.subject.clone()).or_default();
        if let Some(cond) = &law.condition
            && let Ok(reads) = norn_expr::expression_reads(cond.as_str(), "self")
        {
            entry.extend(reads.fields);
        }
        if let LawResponse::Predicate { expression } = &law.response
            && let Ok(reads) = norn_expr::expression_reads(expression.as_str(), "self")
        {
            entry.extend(reads.fields);
        }
    }
    let mut resolved = BTreeMap::new();
    for (type_name, ty) in &set.object_types {
        let mut combined = BTreeSet::new();
        for subj in type_governed_subjects(set, ty) {
            if let Some(fields) = by_subject.get(&subj) {
                combined.extend(fields.iter().cloned());
            }
        }
        resolved.insert(type_name.clone(), combined);
    }
    resolved
}

fn collect_unconstrained_writes(set: &ExtractedSet) -> Vec<UnconstrainedWriteFinding> {
    let governed_props = collect_governed_properties(set);
    let mut out = Vec::new();
    for action in &set.actions {
        let Some(ActionEffectProgram::UpsertInputObject { object_kind, .. }) =
            &action.effect_program
        else {
            continue;
        };
        let Some(ty) = set.object_types.get(object_kind) else {
            continue;
        };
        let Some(props) = action
            .input_schema
            .get("properties")
            .and_then(norn_ir::JsonValue::as_object)
        else {
            continue;
        };
        let derived: BTreeSet<&str> = action.derive.iter().map(|d| d.field.as_str()).collect();
        let mut checked_inputs = BTreeSet::new();
        for pre in &action.preconditions {
            if let Ok(reads) = norn_expr::expression_reads(pre.as_str(), "input") {
                checked_inputs.extend(reads.fields);
            }
        }
        let effective_props = effective_object_properties(set, ty);
        let key_props = effective_object_key_props(set, ty);
        let protected_props = effective_protected_props(set, ty);
        let lc_props: BTreeSet<&str> = effective_object_lifecycles(set, ty)
            .into_iter()
            .map(|lc| lc.property.as_str())
            .collect();
        let link_fields: BTreeSet<&str> = action
            .create_links
            .iter()
            .map(|l| l.target_id_field.as_str())
            .collect();
        let governed = governed_props.get(object_kind);

        for field in props.keys() {
            let Some(prop) = effective_props.iter().find(|p| p.id.as_str() == field) else {
                continue;
            };
            if prop.protected
                || protected_props.contains(field.as_str())
                || prop.computed.is_some()
                || key_props.contains(field.as_str())
                || lc_props.contains(field.as_str())
                || link_fields.contains(field.as_str())
                || derived.contains(field.as_str())
                || checked_inputs.contains(field.as_str())
                || governed.is_some_and(|set| set.contains(field.as_str()))
            {
                continue;
            }
            out.push(UnconstrainedWriteFinding {
                action_id: action.action_id.clone(),
                object_type: object_kind.clone(),
                property: field.clone(),
            });
        }
    }
    out.sort();
    out
}

/// Executes all three terminating `norn-datalog` red-team checks and the
/// unconstrained-write report over an artifact set.
#[must_use]
pub fn analyze_red_team(artifacts: &[ArtifactEnvelope]) -> RedTeamReport {
    let set = extract_set(artifacts);
    let mut evaluation_errors = Vec::new();
    let scope_confinement = run_scope_confinement(&set, &mut evaluation_errors);
    let unauthenticated_settlements = run_external_settlement_check(&set, &mut evaluation_errors);
    let self_ratifiable_transitions = run_sod_reachability(&set, &mut evaluation_errors);
    let unconstrained_writes = collect_unconstrained_writes(&set);
    RedTeamReport {
        scope_confinement,
        unauthenticated_settlements,
        self_ratifiable_transitions,
        unconstrained_writes,
        evaluation_errors,
        decode_errors: set.decode_errors,
    }
}
