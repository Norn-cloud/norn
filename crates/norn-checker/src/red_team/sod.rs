//! Recursive lifecycle separation-of-duties (SoD) Datalog check for `RED-TEAM-1`.

use std::collections::{BTreeMap, BTreeSet};

use norn_datalog::{Budget, ExecBudget, Facts, Literal, Mode, Program, RelationKind, Rule, evaluate};
use norn_ir::access::{AccessCondition, AccessRule, ResourceCompareOp, ResourceOperand};
use norn_ir::{ActionEffectProgram, LifecycleOutcome, UpsertWriteMode};

use super::{
    ExtractedSet, SelfRatifiableFinding, as_str, atom, collect_rule_roles,
    effective_object_lifecycles, effective_protected_props, is_principal_identity_attr, pred,
    push_fact, s,
};

/// Only credit a policy `ne principal.<identity>` check as SoD when the
/// compared resource field is a declared `protected` property on the rule's
/// resource type (so callers cannot forge it to self-approve).
fn rule_has_sod(set: &ExtractedSet, rule: &AccessRule) -> bool {
    let Some(ty) = set.object_types.get(rule.resource_type.as_str()) else {
        return false;
    };
    let protected_props = effective_protected_props(set, ty);
    if protected_props.is_empty() {
        return false;
    }
    rule.all_of.iter().any(|cond| {
        matches!(
            cond,
            AccessCondition::Resource(res)
                if matches!(res.op, ResourceCompareOp::Ne)
                    && protected_props.contains(res.name.as_str())
                    && matches!(
                        &res.value,
                        ResourceOperand::PrincipalAttribute(pa)
                            if is_principal_identity_attr(&pa.principal_attribute)
                    )
        )
    })
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
    let mut action_policy_grants = BTreeMap::<&str, (usize, usize)>::new();
    let mut all_policy_roles = BTreeSet::<String>::new();
    all_policy_roles.insert("*".to_owned());
    for policy in &set.policies {
        for rule in &policy.rules {
            all_policy_roles.extend(collect_rule_roles(rule));
            let sod = rule_has_sod(set, rule);
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
            Some(
                ActionEffectProgram::UpsertInputObject {
                    write_mode: UpsertWriteMode::UpdateOnly,
                    ..
                }
                | ActionEffectProgram::MutateParentWithRows { .. },
            )
            | None => {}
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
            let sod = rule_has_sod(set, rule);
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

pub(super) fn run_sod_reachability(
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
