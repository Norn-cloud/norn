//! Bounded core-law preservation checks for `fire_transition` actions
//! (including `setFields` patches and input derivations).

use std::collections::{BTreeMap, BTreeSet};

use norn_expr::Value;
use norn_ir::{ActionRoot, ArtifactEnvelope, JsonValue, PropertyId};

use super::eval::{
    EvalStop, PreconditionVerdict, WORK_BUDGET, apply_derivations, budget_unknown_finding,
    derive_input_dependencies, evaluate_core_rule, expand_assignments, expr_value_to_string,
    extract_finite_schema_domain, extract_fixed_schema_value, preconditions_admit,
    rule_single_hop_fields, slice_constrained_input_fields,
};
use super::writers::preservation_unknown_finding;
use super::{GoverningCoreRule, codes, finding};
use crate::{
    Diagnostic, DiagnosticWitness, EffectiveObjectType, WitnessMove, WitnessSide, WitnessStep,
};

fn determinable_transition_fields(
    needed: &BTreeSet<String>,
    lifecycle_prop: &str,
    set_fields: &BTreeMap<PropertyId, String>,
    base_input: &BTreeMap<String, Value>,
    finite_domains: &BTreeMap<String, Vec<Value>>,
    derived_fields: &BTreeSet<String>,
    derived_deps: &BTreeMap<String, Option<BTreeSet<String>>>,
) -> Option<Vec<String>> {
    let mut enum_fields: BTreeSet<String> = BTreeSet::new();
    for field in needed {
        if field == lifecycle_prop {
            continue;
        }
        let input_field = set_fields
            .iter()
            .find(|(prop, _)| prop.as_str() == field)
            .map(|(_, input_field)| input_field)?;
        if derived_fields.contains(input_field) {
            let deps = match derived_deps.get(input_field) {
                Some(Some(deps)) => deps,
                Some(None) | None => return None,
            };
            for dep in deps {
                if base_input.contains_key(dep) {
                    continue;
                }
                if finite_domains.contains_key(dep) {
                    enum_fields.insert(dep.clone());
                } else {
                    return None;
                }
            }
        } else if base_input.contains_key(input_field) {
            continue;
        } else if finite_domains.contains_key(input_field) {
            enum_fields.insert(input_field.clone());
        } else {
            return None;
        }
    }
    Some(enum_fields.into_iter().collect())
}

#[allow(
    clippy::too_many_arguments,
    reason = "transition preservation needs action, effective type, transition metadata, setFields, and rules"
)]
pub(super) fn check_fire_transition_action(
    artifact: &ArtifactEnvelope,
    root: &ActionRoot,
    effective: &EffectiveObjectType,
    lifecycle_prop: &str,
    transition_id: &str,
    set_fields: &BTreeMap<PropertyId, String>,
    rules: &[GoverningCoreRule],
    findings: &mut Vec<Diagnostic>,
) {
    let Some(lc) = effective.lifecycle(lifecycle_prop) else {
        return;
    };
    let Some(tr) = lc
        .lifecycle
        .transitions
        .iter()
        .find(|t| t.id.as_str() == transition_id)
    else {
        return;
    };

    let mut base_state: BTreeMap<String, Value> = BTreeMap::new();
    base_state.insert(lifecycle_prop.to_owned(), Value::String(tr.to.clone()));
    let mut base_input: BTreeMap<String, Value> = BTreeMap::new();
    let slice_constrained = slice_constrained_input_fields(root);
    let mut finite_domains: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    if let Some(props) = root
        .input_schema
        .get("properties")
        .and_then(JsonValue::as_object)
    {
        for (field, schema) in props {
            if let Some(fixed) = extract_fixed_schema_value(schema) {
                base_input.insert(field.clone(), fixed);
            } else if !slice_constrained.contains(field)
                && let Some(domain) = extract_finite_schema_domain(schema)
            {
                finite_domains.insert(field.clone(), domain);
            }
        }
    }

    let derived_deps = derive_input_dependencies(root);
    let derived_fields: BTreeSet<String> = root.derive.iter().map(|s| s.field.clone()).collect();
    let mut work = WORK_BUDGET;

    for rule in rules {
        let Some(needed) = rule_single_hop_fields(rule) else {
            continue;
        };
        let Some(ordered_fields) = determinable_transition_fields(
            &needed,
            lifecycle_prop,
            set_fields,
            &base_input,
            &finite_domains,
            &derived_fields,
            &derived_deps,
        ) else {
            continue;
        };
        let Some(assignments) = expand_assignments(&ordered_fields, &finite_domains) else {
            findings.push(budget_unknown_finding(artifact, &root.action_id, &rule.id));
            continue;
        };
        evaluate_transition_assignments(
            artifact,
            root,
            effective,
            tr,
            set_fields,
            rule,
            &needed,
            &base_state,
            &base_input,
            &assignments,
            &mut work,
            findings,
        );
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "evaluates expanded finite valuations for one fire_transition action and one core rule"
)]
fn evaluate_transition_assignments(
    artifact: &ArtifactEnvelope,
    root: &ActionRoot,
    effective: &EffectiveObjectType,
    tr: &norn_ir::Transition,
    set_fields: &BTreeMap<PropertyId, String>,
    rule: &GoverningCoreRule,
    needed: &BTreeSet<String>,
    base_state: &BTreeMap<String, Value>,
    base_input: &BTreeMap<String, Value>,
    assignments: &[BTreeMap<String, Value>],
    work: &mut u64,
    findings: &mut Vec<Diagnostic>,
) {
    let empty_props = BTreeSet::new();
    let mut unknown_detail: Option<String> = None;
    for assignment in assignments {
        let mut input_map = base_input.clone();
        for (k, v) in assignment {
            input_map.insert(k.clone(), v.clone());
        }
        let mut dummy_state = BTreeMap::new();
        match apply_derivations(root, &mut input_map, &mut dummy_state, &empty_props, work) {
            Ok(()) => {}
            Err(EvalStop::Budget) => {
                findings.push(budget_unknown_finding(artifact, &root.action_id, &rule.id));
                return;
            }
            Err(EvalStop::Other) => {
                unknown_detail.get_or_insert_with(|| {
                    "action derivation could not be statically evaluated over a candidate input"
                        .to_owned()
                });
                continue;
            }
        }
        let pre_verdict = match preconditions_admit(root, &input_map, work) {
            Ok(PreconditionVerdict::Refused) => continue,
            Ok(verdict) => verdict,
            Err(EvalStop::Budget) => {
                findings.push(budget_unknown_finding(artifact, &root.action_id, &rule.id));
                return;
            }
            Err(EvalStop::Other) => PreconditionVerdict::Unknown(
                "action preconditions could not be evaluated statically".to_owned(),
            ),
        };
        let mut state_map = base_state.clone();
        for (prop_id, input_field) in set_fields {
            if let Some(val) = input_map.get(input_field) {
                state_map.insert(prop_id.as_str().to_owned(), val.clone());
            }
        }
        if !needed.iter().all(|f| state_map.contains_key(f)) {
            continue;
        }
        match evaluate_core_rule(rule, &state_map, work) {
            Ok(true) => {}
            Ok(false) => match pre_verdict {
                PreconditionVerdict::Admitted => {
                    findings.push(transition_violation_finding(
                        artifact, root, effective, tr, set_fields, rule, needed, assignment,
                        &state_map, &input_map,
                    ));
                    return;
                }
                PreconditionVerdict::Unknown(reason) => {
                    unknown_detail.get_or_insert_with(|| {
                        format!(
                            "candidate post-state violates core law {:?}, but {reason} so reachability cannot be confirmed",
                            rule.id
                        )
                    });
                }
                PreconditionVerdict::Refused => {}
            },
            Err(EvalStop::Other) => {
                unknown_detail.get_or_insert_with(|| {
                    format!(
                        "core law {:?} predicate could not be statically evaluated over the candidate post-state",
                        rule.id
                    )
                });
            }
            Err(EvalStop::Budget) => {
                findings.push(budget_unknown_finding(artifact, &root.action_id, &rule.id));
                return;
            }
        }
    }
    if let Some(detail) = unknown_detail {
        findings.push(preservation_unknown_finding(
            artifact,
            &root.action_id,
            &rule.id,
            &detail,
        ));
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "formats a concrete fire_transition preservation violation witness"
)]
fn transition_violation_finding(
    artifact: &ArtifactEnvelope,
    root: &ActionRoot,
    effective: &EffectiveObjectType,
    tr: &norn_ir::Transition,
    set_fields: &BTreeMap<PropertyId, String>,
    rule: &GoverningCoreRule,
    needed: &BTreeSet<String>,
    assignment: &BTreeMap<String, Value>,
    state_map: &BTreeMap<String, Value>,
    input_map: &BTreeMap<String, Value>,
) -> Diagnostic {
    let transition_id = tr.id.as_str();
    let writer_id = if set_fields.is_empty() {
        format!("transition:{transition_id}")
    } else {
        format!("transition:{transition_id}+setFields")
    };
    let state_assignment = state_map
        .iter()
        .filter(|(k, _)| needed.contains(*k))
        .map(|(k, v)| (k.clone(), expr_value_to_string(v)))
        .collect();
    let input_assignment = input_map
        .iter()
        .filter(|(k, _)| assignment.contains_key(*k) || set_fields.values().any(|f| f == *k))
        .map(|(k, v)| (k.clone(), expr_value_to_string(v)))
        .collect();
    let trace = vec![format!(
        "{} --({transition_id})--> {}",
        tr.from.join("|"),
        tr.to
    )];
    let detail = format!(
        "action {:?} firing transition {:?} -> {:?} on {:?} violates core law {:?}",
        root.action_id,
        transition_id,
        tr.to,
        effective.name.as_str(),
        rule.id
    );
    let steps = vec![WitnessStep {
        edge: 0,
        moves: vec![WitnessMove {
            side: WitnessSide::Backend,
            label: transition_id.to_owned(),
        }],
    }];
    finding(
        codes::PRESERVATION_VIOLATED,
        artifact,
        "/payload/effectProgram",
        detail.as_str(),
        Some(DiagnosticWitness::core_law_violation(
            steps,
            root.action_id.as_str(),
            writer_id,
            effective.name.as_str(),
            rule.id.as_str(),
            state_assignment,
            input_assignment,
            trace,
            detail,
        )),
    )
}
