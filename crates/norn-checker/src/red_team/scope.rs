//! Scope-confinement Datalog check for `RED-TEAM-1`.

use std::collections::BTreeSet;

use norn_datalog::{Budget, ExecBudget, Facts, Literal, Mode, Program, RelationKind, Rule, evaluate};
use norn_ir::access::{AccessCondition, AccessRule, ResourceCompareOp, ResourceOperand};

use super::{
    ExtractedSet, ScopeConfinementFinding, as_str, atom, collect_rule_roles,
    effective_object_properties, is_principal_identity_attr, pred, push_fact, s,
};

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

fn is_self_scope_principal_attr(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    is_principal_identity_attr(name)
        || matches!(
            lower.as_str(),
            "employee_id" | "employeeid" | "person_id" | "personid" | "member_id" | "memberid"
        )
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
    collect_applicable_type_props(set, &scoped_props, &mut facts);
    facts
}

fn collect_applicable_type_props(
    set: &ExtractedSet,
    scoped_props: &BTreeSet<String>,
    facts: &mut Facts,
) {
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
                for prop in scoped_props {
                    if props.contains(prop.as_str()) {
                        push_fact(facts, "type_applicable_prop", vec![s(ty_name), s(prop)]);
                    }
                }
            } else {
                // Unknown resource type in partial artifact set: fail closed for all scoped props.
                for prop in scoped_props {
                    push_fact(facts, "type_applicable_prop", vec![s(ty_name), s(prop)]);
                }
            }
        }
    }
}

pub(super) fn run_scope_confinement(
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
