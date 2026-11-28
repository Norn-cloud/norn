//! Bounded core-law preservation checks across candidate actions
//! (`upsert_input_object`, `derive`, `fire_transition` + `setFields`) and
//! `synchronousRule` / decide-graph literal fact writers.

use std::collections::{BTreeMap, BTreeSet};

use norn_expr::Value;
use norn_ir::{
    ActionEffectProgram, ActionRoot, ArtifactEnvelope, ArtifactKind, InterfaceDef, InterfaceName,
    JsonValue, LawIR, UpsertWriteMode,
};

use super::eval::{
    EvalStop, MAX_COMBINATIONS, PreconditionVerdict, WORK_BUDGET, apply_derivations,
    budget_unknown_finding, check_rule_literal_writes, derive_input_dependencies,
    evaluate_core_rule, expand_assignments, expr_value_to_string, extract_finite_schema_domain,
    extract_fixed_schema_value, preconditions_admit, rule_single_hop_fields, serde_value_to_expr,
    slice_constrained_input_fields,
};
use super::transitions::check_fire_transition_action;
use super::{GoverningCoreRule, codes, decode_action, finding, governing_core_rules};
use crate::entity_scopes::ScopedViews;
use crate::{Diagnostic, DiagnosticWitness, EffectiveCatalog, EffectiveObjectType};

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
            object_id_field,
            object_kind,
            write_mode,
            ..
        }) => {
            if let Some(Ok(effective)) = catalog.object_type_named(object_kind) {
                let rules =
                    governing_core_rules(effective, catalog, interfaces, base_laws, candidate_laws);
                if !rules.is_empty() {
                    check_upsert_action(
                        artifact,
                        root,
                        effective,
                        object_id_field,
                        write_mode,
                        catalog,
                        pool,
                        &rules,
                        findings,
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
    object_id_field: &str,
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
        bind_unbound_create_links_null(root, effective, object_id_field, catalog, pool, &mut state);
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
    object_id_field: &str,
    catalog: &EffectiveCatalog,
    pool: &[ArtifactEnvelope],
    state: &mut BTreeMap<String, Value>,
) {
    let Some(links) = catalog.links_for_object_type(effective.name.as_str()) else {
        return;
    };
    let bound = super::link_bindings::bound_create_links(
        root,
        pool,
        effective.name.as_str(),
        object_id_field,
    );
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
    object_id_field: &str,
    write_mode: &UpsertWriteMode,
    catalog: &EffectiveCatalog,
    pool: &[ArtifactEnvelope],
    rules: &[GoverningCoreRule],
    findings: &mut Vec<Diagnostic>,
) {
    let base = build_upsert_base(root, effective, object_id_field, write_mode, catalog, pool);
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
    let mut unknown_detail: Option<String> = None;
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
        if !needed.iter().all(|f| state_map.contains_key(f)) {
            continue;
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
        match evaluate_core_rule(rule, &state_map, work) {
            Ok(true) => {}
            Ok(false) => match pre_verdict {
                PreconditionVerdict::Admitted => {
                    findings.push(upsert_violation_finding(
                        artifact, root, effective, rule, needed, base, assignment, &state_map,
                        &input_map,
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
    reason = "formats a concrete upsert preservation violation witness"
)]
fn upsert_violation_finding(
    artifact: &ArtifactEnvelope,
    root: &ActionRoot,
    effective: &EffectiveObjectType,
    rule: &GoverningCoreRule,
    needed: &BTreeSet<String>,
    base: &UpsertBase,
    assignment: &BTreeMap<String, Value>,
    state_map: &BTreeMap<String, Value>,
    input_map: &BTreeMap<String, Value>,
) -> Diagnostic {
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
    finding(
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
    )
}

pub(super) fn preservation_unknown_finding(
    artifact: &ArtifactEnvelope,
    action_id: &str,
    core_law_id: &str,
    detail: &str,
) -> Diagnostic {
    finding(
        codes::PRESERVATION_UNKNOWN,
        artifact,
        "/payload/effectProgram",
        format!(
            "bounded core-law preservation check for action {action_id:?} against core law {core_law_id:?} is unknown: {detail}"
        ),
        None,
    )
}
