//! Bounded core-law preservation checks across candidate actions
//! (`upsert_input_object`, `derive`, `fire_transition` + `setFields`) and
//! `synchronousRule` / decide-graph literal fact writers.

use std::collections::{BTreeMap, BTreeSet};

use norn_expr::Value;
use norn_ir::{
    ActionEffectProgram, ActionRoot, ArtifactEnvelope, ArtifactKind, InterfaceDef, InterfaceName,
    JsonValue, LawIR, PropertyId, UpsertWriteMode,
};

use super::eval::{
    EvalStop, MAX_COMBINATIONS, WORK_BUDGET, apply_derivations, budget_unknown_finding,
    check_rule_literal_writes, derive_input_dependencies, evaluate_core_rule, expand_assignments,
    expr_value_to_string, extract_finite_schema_domain, extract_fixed_schema_value,
    preconditions_admit, rule_single_hop_fields, serde_value_to_expr,
    slice_constrained_input_fields,
};
use super::{GoverningCoreRule, codes, decode_action, finding, governing_core_rules};
use crate::entity_scopes::ScopedViews;
use crate::{
    Diagnostic, DiagnosticWitness, EffectiveCatalog, EffectiveObjectType, WitnessMove, WitnessSide,
    WitnessStep,
};

pub(super) fn check_core_writer_preservation(
    artifacts: &[ArtifactEnvelope],
    interfaces: &BTreeMap<InterfaceName, InterfaceDef>,
    base_laws: &[(&ArtifactEnvelope, LawIR)],
    candidate_laws: &[(&ArtifactEnvelope, LawIR)],
    views: &mut ScopedViews<'_>,
    findings: &mut Vec<Diagnostic>,
) {
    let mut checked_rules: BTreeSet<(String, String)> = BTreeSet::new();

    for artifact in artifacts {
        let Some(root) = decode_action(artifact) else {
            continue;
        };
        let view = views.of(artifact);
        let catalog = view.catalog();
        check_action_effect(
            artifact,
            &root,
            catalog,
            view.pool(),
            interfaces,
            base_laws,
            candidate_laws,
            findings,
        );

        if let Some(rule_ref) = &root.synchronous_rule
            && let Some(rule_artifact) = view.pool().iter().find(|candidate| {
                candidate.kind() == ArtifactKind::Rule
                    && candidate.artifact_id == rule_ref.artifact_id
                    && candidate.version == rule_ref.version
            })
        {
            checked_rules.insert((
                rule_artifact.artifact_id.clone(),
                rule_artifact.version.clone(),
            ));
            check_rule_literal_writes(
                artifact,
                &root.action_id,
                rule_artifact,
                catalog,
                interfaces,
                base_laws,
                candidate_laws,
                findings,
            );
        }
    }

    for artifact in artifacts {
        if artifact.kind() != ArtifactKind::Rule {
            continue;
        }
        if checked_rules.contains(&(artifact.artifact_id.clone(), artifact.version.clone())) {
            continue;
        }
        let view = views.of(artifact);
        check_rule_literal_writes(
            artifact,
            &artifact.artifact_id,
            artifact,
            view.catalog(),
            interfaces,
            base_laws,
            candidate_laws,
            findings,
        );
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "dispatches effect program checks against scoped catalog and governing rules"
)]
fn check_action_effect(
    artifact: &ArtifactEnvelope,
    root: &ActionRoot,
    catalog: &EffectiveCatalog,
    pool: &[ArtifactEnvelope],
    interfaces: &BTreeMap<InterfaceName, InterfaceDef>,
    base_laws: &[(&ArtifactEnvelope, LawIR)],
    candidate_laws: &[(&ArtifactEnvelope, LawIR)],
    findings: &mut Vec<Diagnostic>,
) {
    match &root.effect_program {
        Some(ActionEffectProgram::UpsertInputObject {
            object_kind,
            write_mode,
            ..
        }) => {
            if let Some(Ok(effective)) = catalog.object_type_named(object_kind) {
                let rules =
                    governing_core_rules(effective, catalog, interfaces, base_laws, candidate_laws);
                if !rules.is_empty() {
                    check_upsert_action(
                        artifact, root, effective, write_mode, catalog, pool, &rules, findings,
                    );
                }
            }
        }
        Some(ActionEffectProgram::FireTransition {
            object_kind,
            lifecycle,
            transition,
            set_fields,
            ..
        }) => {
            if let Some(Ok(effective)) = catalog.object_type_named(object_kind) {
                let rules =
                    governing_core_rules(effective, catalog, interfaces, base_laws, candidate_laws);
                if !rules.is_empty() {
                    check_fire_transition_action(
                        artifact,
                        root,
                        effective,
                        lifecycle.as_str(),
                        transition.as_str(),
                        set_fields,
                        &rules,
                        findings,
                    );
                }
            }
        }
        Some(ActionEffectProgram::MutateParentWithRows { .. }) | None => {}
    }
}

struct UpsertBase {
    state: BTreeMap<String, Value>,
    input: BTreeMap<String, Value>,
    finite_domains: BTreeMap<String, Vec<Value>>,
    prop_names: BTreeSet<String>,
    derived_fields: BTreeSet<String>,
    derived_deps: BTreeMap<String, Option<BTreeSet<String>>>,
}

fn build_upsert_base(
    root: &ActionRoot,
    effective: &EffectiveObjectType,
    write_mode: &UpsertWriteMode,
    catalog: &EffectiveCatalog,
    pool: &[ArtifactEnvelope],
) -> UpsertBase {
    let mut state: BTreeMap<String, Value> = BTreeMap::new();
    let mut input: BTreeMap<String, Value> = BTreeMap::new();
    let schema_props = root
        .input_schema
        .get("properties")
        .and_then(JsonValue::as_object);
    let prop_names: BTreeSet<String> = effective
        .properties
        .iter()
        .map(|prop| prop.id.as_str().to_owned())
        .collect();

    if matches!(
        write_mode,
        UpsertWriteMode::CreateOnly | UpsertWriteMode::Upsert
    ) {
        for prop in &effective.properties {
            let supplied = schema_props.is_some_and(|props| props.contains_key(prop.id.as_str()));
            let is_lifecycle = effective.lifecycle(prop.id.as_str()).is_some();
            if (!supplied || is_lifecycle)
                && let Ok(Some(default_val)) = prop.default_value()
                && let Some(expr_val) = serde_value_to_expr(default_val)
            {
                state.insert(prop.id.as_str().to_owned(), expr_val);
            }
        }
    }
    if let UpsertWriteMode::DraftOnly {
        lifecycle,
        state: draft,
    } = write_mode
    {
        state.insert(lifecycle.as_str().to_owned(), Value::String(draft.clone()));
    }
    if matches!(write_mode, UpsertWriteMode::CreateOnly) {
        bind_unbound_create_links_null(root, effective, catalog, pool, &mut state);
    }

    let slice_constrained = slice_constrained_input_fields(root);
    let mut finite_domains: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    if let Some(props) = schema_props {
        for (field, schema) in props {
            if let Some(fixed) = extract_fixed_schema_value(schema) {
                input.insert(field.clone(), fixed.clone());
                if prop_names.contains(field.as_str()) {
                    state.insert(field.clone(), fixed);
                }
            } else if !slice_constrained.contains(field)
                && let Some(domain) = extract_finite_schema_domain(schema)
            {
                finite_domains.insert(field.clone(), domain);
            }
        }
    }

    UpsertBase {
        state,
        input,
        finite_domains,
        prop_names,
        derived_fields: root.derive.iter().map(|step| step.field.clone()).collect(),
        derived_deps: derive_input_dependencies(root),
    }
}

fn bind_unbound_create_links_null(
    root: &ActionRoot,
    effective: &EffectiveObjectType,
    catalog: &EffectiveCatalog,
    pool: &[ArtifactEnvelope],
    state: &mut BTreeMap<String, Value>,
) {
    let rule_emits_links = root.synchronous_rule.as_ref().is_some_and(|rule_ref| {
        super::synchronous_rule_declares_links(pool, &rule_ref.artifact_id, &rule_ref.version)
    });
    if rule_emits_links {
        return;
    }
    let Some(links) = catalog.links_for_object_type(effective.name.as_str()) else {
        return;
    };
    let bound: BTreeSet<&str> = root
        .create_links
        .iter()
        .filter(|binding| {
            binding
                .source
                .as_ref()
                .is_none_or(|src| src.object_type == effective.name.as_str())
        })
        .map(|binding| binding.link.as_str())
        .collect();
    for link in links {
        if !bound.contains(link.id.as_str()) {
            state.insert(link.id.as_str().to_owned(), Value::Null);
        }
    }
}

fn determinable_upsert_fields(needed: &BTreeSet<String>, base: &UpsertBase) -> Option<Vec<String>> {
    let mut enum_fields: BTreeSet<String> = BTreeSet::new();
    for field in needed {
        if base.derived_fields.contains(field) {
            let deps = match base.derived_deps.get(field) {
                Some(Some(deps)) => deps,
                Some(None) | None => return None,
            };
            for dep in deps {
                if base.input.contains_key(dep) {
                    continue;
                }
                if base.finite_domains.contains_key(dep) {
                    enum_fields.insert(dep.clone());
                } else {
                    return None;
                }
            }
        } else if base.state.contains_key(field) {
            continue;
        } else if base.finite_domains.contains_key(field) {
            enum_fields.insert(field.clone());
        } else {
            return None;
        }
    }
    Some(enum_fields.into_iter().collect())
}

#[allow(
    clippy::too_many_arguments,
    reason = "action preservation requires action root, effective target, catalog, and governing rules"
)]
fn check_upsert_action(
    artifact: &ArtifactEnvelope,
    root: &ActionRoot,
    effective: &EffectiveObjectType,
    write_mode: &UpsertWriteMode,
    catalog: &EffectiveCatalog,
    pool: &[ArtifactEnvelope],
    rules: &[GoverningCoreRule],
    findings: &mut Vec<Diagnostic>,
) {
    let base = build_upsert_base(root, effective, write_mode, catalog, pool);
    let prop_refs: BTreeSet<&str> = base.prop_names.iter().map(String::as_str).collect();
    let mut work = WORK_BUDGET;

    for rule in rules {
        let Some(needed) = rule_single_hop_fields(rule) else {
            continue;
        };
        let Some(ordered_fields) = determinable_upsert_fields(&needed, &base) else {
            continue;
        };
        let Some(assignments) = expand_assignments(&ordered_fields, &base.finite_domains) else {
            findings.push(finding(
                codes::PRESERVATION_UNKNOWN,
                artifact,
                "/payload/effectProgram",
                format!(
                    "finite core-law preservation check for action {:?} against core law {:?} exceeded its combination budget ({MAX_COMBINATIONS})",
                    root.action_id, rule.id
                ),
                None,
            ));
            continue;
        };
        evaluate_upsert_assignments(
            artifact,
            root,
            effective,
            rule,
            &needed,
            &base,
            &prop_refs,
            &assignments,
            &mut work,
            findings,
        );
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "evaluates expanded finite valuations for one upsert action and one governing rule"
)]
fn evaluate_upsert_assignments(
    artifact: &ArtifactEnvelope,
    root: &ActionRoot,
    effective: &EffectiveObjectType,
    rule: &GoverningCoreRule,
    needed: &BTreeSet<String>,
    base: &UpsertBase,
    prop_refs: &BTreeSet<&str>,
    assignments: &[BTreeMap<String, Value>],
    work: &mut u64,
    findings: &mut Vec<Diagnostic>,
) {
    for assignment in assignments {
        let mut input_map = base.input.clone();
        let mut state_map = base.state.clone();
        for (k, v) in assignment {
            input_map.insert(k.clone(), v.clone());
            if prop_refs.contains(k.as_str()) {
                state_map.insert(k.clone(), v.clone());
            }
        }
        match apply_derivations(root, &mut input_map, &mut state_map, prop_refs, work) {
            Ok(()) => {}
            Err(EvalStop::Budget) => {
                findings.push(budget_unknown_finding(artifact, &root.action_id, &rule.id));
                break;
            }
            Err(EvalStop::Other) => continue,
        }
        if !needed.iter().all(|f| state_map.contains_key(f)) {
            continue;
        }
        match preconditions_admit(root, &input_map, work) {
            Ok(true) => {}
            Ok(false) | Err(EvalStop::Other) => continue,
            Err(EvalStop::Budget) => {
                findings.push(budget_unknown_finding(artifact, &root.action_id, &rule.id));
                break;
            }
        }
        match evaluate_core_rule(rule, &state_map, work) {
            Ok(true) | Err(EvalStop::Other) => {}
            Ok(false) => {
                let writer_id = needed
                    .iter()
                    .find(|f| base.derived_fields.contains(*f))
                    .map_or_else(
                        || "upsert_input_object".to_owned(),
                        |f| format!("derive:{f}"),
                    );
                let state_assignment = state_map
                    .iter()
                    .filter(|(k, _)| needed.contains(*k))
                    .map(|(k, v)| (k.clone(), expr_value_to_string(v)))
                    .collect();
                let input_assignment = input_map
                    .iter()
                    .filter(|(k, _)| needed.contains(*k) || assignment.contains_key(*k))
                    .map(|(k, v)| (k.clone(), expr_value_to_string(v)))
                    .collect();
                let detail = format!(
                    "action {:?} ({writer_id}) produces a post-state on {:?} violating core law {:?}",
                    root.action_id,
                    effective.name.as_str(),
                    rule.id
                );
                let trace = vec![format!(
                    "action {} -> {} on {}",
                    root.action_id,
                    writer_id,
                    effective.name.as_str()
                )];
                findings.push(finding(
                    codes::PRESERVATION_VIOLATED,
                    artifact,
                    "/payload/effectProgram",
                    detail.as_str(),
                    Some(DiagnosticWitness::core_law_violation(
                        Vec::new(),
                        root.action_id.as_str(),
                        writer_id,
                        effective.name.as_str(),
                        rule.id.as_str(),
                        state_assignment,
                        input_assignment,
                        trace,
                        detail,
                    )),
                ));
                break;
            }
            Err(EvalStop::Budget) => {
                findings.push(budget_unknown_finding(artifact, &root.action_id, &rule.id));
                break;
            }
        }
    }
}

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
fn check_fire_transition_action(
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
    let transition_id = tr.id.as_str();
    let empty_props = BTreeSet::new();
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
                break;
            }
            Err(EvalStop::Other) => continue,
        }
        match preconditions_admit(root, &input_map, work) {
            Ok(true) => {}
            Ok(false) | Err(EvalStop::Other) => continue,
            Err(EvalStop::Budget) => {
                findings.push(budget_unknown_finding(artifact, &root.action_id, &rule.id));
                break;
            }
        }
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
            Ok(true) | Err(EvalStop::Other) => {}
            Ok(false) => {
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
                    .filter(|(k, _)| {
                        assignment.contains_key(*k) || set_fields.values().any(|f| f == *k)
                    })
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
                findings.push(finding(
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
                ));
                break;
            }
            Err(EvalStop::Budget) => {
                findings.push(budget_unknown_finding(artifact, &root.action_id, &rule.id));
                break;
            }
        }
    }
}
