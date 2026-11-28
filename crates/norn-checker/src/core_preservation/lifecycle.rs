//! Bounded finite lifecycle soundness under governing `core.*` laws:
//! detects reachable non-terminal states where inherited `core.*` laws block
//! all exit transitions (or refuse a reachable fact transition) and attaches a
//! shortest-run counterexample witness.

use std::collections::{BTreeMap, BTreeSet};

use norn_expr::{Expression, Value};
use norn_ir::{
    ArtifactEnvelope, EntityRoot, InterfaceDef, InterfaceName, LawIR, LifecycleOutcome,
    PropertyType, TransitionTrigger,
};

use super::eval::{
    EvalStop, MAX_FINITE_ENUM, WORK_BUDGET, eval_bool, evaluate_core_rule, expr_value_to_string,
    rule_single_hop_fields,
};
use super::{GoverningCoreRule, codes, finding, governing_core_rules, is_core_name};
use crate::entity_artifacts::{Decoded, decode_v2};
use crate::entity_instance::{EffectiveKind, EffectiveLifecycle, EffectiveObjectType};
use crate::entity_scopes::ScopedViews;
use crate::soundness::{self, Ending, Net, NetEdge};
use crate::{Diagnostic, DiagnosticWitness, WitnessMove, WitnessSide, WitnessStep};

const MAX_ROWS: usize = 256;

struct LawfulEdge {
    from: usize,
    to: usize,
    transition: String,
    input: bool,
    guarded_unknown: bool,
}

struct BlockedExit {
    from: usize,
    transition: String,
    laws: Vec<String>,
    fact: bool,
}

pub(super) fn check_core_lifecycle_preservation(
    artifacts: &[ArtifactEnvelope],
    interfaces: &BTreeMap<InterfaceName, InterfaceDef>,
    base_laws: &[(&ArtifactEnvelope, LawIR)],
    candidate_laws: &[(&ArtifactEnvelope, LawIR)],
    views: &mut ScopedViews<'_>,
    findings: &mut Vec<Diagnostic>,
) {
    for artifact in artifacts {
        let Decoded::Root(root) = decode_v2(artifact) else {
            continue;
        };
        let EntityRoot::V2(object_root) = *root else {
            continue;
        };
        let type_name = object_root.object_type.name.as_str();
        let view = views.of(artifact);
        let catalog = view.catalog();
        let Some(Ok(effective)) = catalog.object_type_named(type_name) else {
            continue;
        };
        if effective.lifecycles.is_empty() {
            continue;
        }
        let governs_core = is_core_name(type_name)
            || effective
                .interfaces
                .iter()
                .any(|iface| is_core_name(iface.as_str()));
        if !governs_core {
            continue;
        }
        let all_rules =
            governing_core_rules(effective, catalog, interfaces, base_laws, candidate_laws);
        if all_rules.is_empty() {
            continue;
        }

        for machine in &effective.lifecycles {
            check_one_lifecycle(artifact, effective, machine, &all_rules, findings);
        }
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "finite row construction, law filtering, soundness fixpoint, and witness formatting share one bounded graph"
)]
fn check_one_lifecycle(
    artifact: &ArtifactEnvelope,
    effective: &EffectiveObjectType,
    machine: &EffectiveLifecycle,
    all_rules: &[GoverningCoreRule],
    findings: &mut Vec<Diagnostic>,
) {
    let chart = &machine.lifecycle;
    let property = chart.property.as_str();
    let mut relevant_rules: Vec<&GoverningCoreRule> = Vec::new();
    let mut extra_fields: BTreeSet<String> = BTreeSet::new();

    for rule in all_rules {
        let Some(fields) = rule_single_hop_fields(rule) else {
            continue;
        };
        if !fields.contains(property) {
            continue;
        }
        let all_finite = fields
            .iter()
            .all(|field| field == property || finite_property_domain(effective, field).is_some());
        if all_finite {
            for field in &fields {
                if field != property {
                    extra_fields.insert(field.clone());
                }
            }
            relevant_rules.push(rule);
        }
    }
    if relevant_rules.is_empty() {
        return;
    }

    let mut domains: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    domains.insert(
        property.to_owned(),
        machine
            .states
            .iter()
            .map(|state| Value::String(state.clone()))
            .collect(),
    );
    for field in &extra_fields {
        if let Some(domain) = finite_property_domain(effective, field) {
            domains.insert(field.clone(), domain);
        }
    }

    let ordered_fields: Vec<String> = domains.keys().cloned().collect();
    let Some(rows) = build_rows(&ordered_fields, &domains) else {
        findings.push(finding(
            codes::PRESERVATION_UNKNOWN,
            artifact,
            "/payload/objectType/lifecycles",
            format!(
                "finite lifecycle core-law check for {:?}.{property} exceeded its state budget ({MAX_ROWS})",
                effective.name.as_str()
            ),
            None,
        ));
        return;
    };

    let mut row_index: BTreeMap<Vec<(String, String)>, usize> = BTreeMap::new();
    for (idx, row) in rows.iter().enumerate() {
        row_index.insert(row_key(row), idx);
    }

    let mut work = WORK_BUDGET;
    let mut failing_by_row: Vec<Vec<String>> = Vec::with_capacity(rows.len());
    for row in &rows {
        let mut failed = Vec::new();
        for rule in &relevant_rules {
            match evaluate_core_rule(rule, row, &mut work) {
                Ok(true) | Err(EvalStop::Other) => {}
                Ok(false) => failed.push(rule.id.clone()),
                Err(EvalStop::Budget) => {
                    findings.push(lifecycle_budget_finding(
                        artifact,
                        effective.name.as_str(),
                        property,
                    ));
                    return;
                }
            }
        }
        failing_by_row.push(failed);
    }

    if let Some(init_diag) = check_illegal_initial_states(
        artifact,
        effective.name.as_str(),
        property,
        &chart.initial,
        &rows,
        &failing_by_row,
    ) {
        findings.push(init_diag);
        return;
    }

    let initial: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter_map(|(idx, row)| match row.get(property) {
            Some(Value::String(state))
                if chart.initial.contains(state)
                    && failing_by_row.get(idx).is_some_and(Vec::is_empty) =>
            {
                Some(idx)
            }
            Some(
                Value::Null
                | Value::Bool(_)
                | Value::Number(_)
                | Value::String(_)
                | Value::Array(_)
                | Value::Object(_),
            )
            | None => None,
        })
        .collect();

    let known_fields: BTreeSet<&str> = domains.keys().map(String::as_str).collect();
    let mut lawful_edges: Vec<LawfulEdge> = Vec::new();
    let mut blocked_exits: Vec<BlockedExit> = Vec::new();

    for (from, row) in rows.iter().enumerate() {
        let Some(Value::String(state)) = row.get(property) else {
            continue;
        };
        if !failing_by_row.get(from).is_some_and(Vec::is_empty) {
            continue;
        }
        for transition in &chart.transitions {
            if !transition.from.contains(state) {
                continue;
            }
            let guard_status =
                match evaluate_guards(&transition.guards, row, &known_fields, &mut work) {
                    Ok(status) => status,
                    Err(EvalStop::Budget) => {
                        findings.push(lifecycle_budget_finding(
                            artifact,
                            effective.name.as_str(),
                            property,
                        ));
                        return;
                    }
                    Err(EvalStop::Other) => GuardStatus::Unknown,
                };
            if guard_status == GuardStatus::Disabled {
                continue;
            }
            let mut after = row.clone();
            after.insert(property.to_owned(), Value::String(transition.to.clone()));
            let Some(&to) = row_index.get(&row_key(&after)) else {
                continue;
            };
            let post_failed = failing_by_row.get(to).cloned().unwrap_or_default();
            if !post_failed.is_empty() {
                if guard_status == GuardStatus::Enabled {
                    blocked_exits.push(BlockedExit {
                        from,
                        transition: transition.id.as_str().to_owned(),
                        laws: post_failed,
                        fact: transition.is_fact(),
                    });
                }
                continue;
            }
            let is_timer = matches!(transition.trigger, Some(TransitionTrigger::Timer { .. }));
            lawful_edges.push(LawfulEdge {
                from,
                to,
                transition: transition.id.as_str().to_owned(),
                input: !is_timer,
                guarded_unknown: guard_status == GuardStatus::Unknown,
            });
        }
    }

    if blocked_exits.is_empty() {
        return;
    }

    report_blocked_exits(
        artifact,
        effective.name.as_str(),
        property,
        chart,
        &rows,
        initial,
        &lawful_edges,
        &blocked_exits,
        findings,
    );
}

fn check_illegal_initial_states(
    artifact: &ArtifactEnvelope,
    object_type: &str,
    property: &str,
    initial_states: &[String],
    rows: &[BTreeMap<String, Value>],
    failing_by_row: &[Vec<String>],
) -> Option<Diagnostic> {
    for init_state in initial_states {
        let any_legal = rows.iter().enumerate().any(|(idx, row)| {
            row.get(property) == Some(&Value::String(init_state.clone()))
                && failing_by_row.get(idx).is_some_and(Vec::is_empty)
        });
        if !any_legal
            && let Some((idx, row)) = rows
                .iter()
                .enumerate()
                .find(|(_, row)| row.get(property) == Some(&Value::String(init_state.clone())))
            && let Some(laws) = failing_by_row.get(idx)
            && !laws.is_empty()
        {
            let core_law_id = laws.join(", ");
            let detail = format!(
                "initial lifecycle state {init_state:?} on {object_type:?}.{property} violates governing core law(s) {core_law_id:?}"
            );
            return Some(finding(
                codes::PRESERVATION_VIOLATED,
                artifact,
                "/payload/objectType/lifecycles",
                detail.as_str(),
                Some(DiagnosticWitness::core_law_violation(
                    Vec::new(),
                    "<lifecycle>",
                    format!("init:{init_state}"),
                    object_type,
                    core_law_id,
                    row_assignment(row),
                    BTreeMap::new(),
                    vec![format!("init({property}={init_state})")],
                    detail,
                )),
            ));
        }
    }
    None
}

#[allow(
    clippy::too_many_arguments,
    reason = "maps soundness verdict and shortest witnesses to preservation diagnostics"
)]
fn report_blocked_exits(
    artifact: &ArtifactEnvelope,
    object_type: &str,
    property: &str,
    chart: &norn_ir::Lifecycle,
    rows: &[BTreeMap<String, Value>],
    initial: Vec<usize>,
    lawful_edges: &[LawfulEdge],
    blocked_exits: &[BlockedExit],
    findings: &mut Vec<Diagnostic>,
) {
    let ending = |state: &str| match chart
        .endings
        .iter()
        .find(|e| e.state == state)
        .map(|e| e.outcome)
    {
        Some(LifecycleOutcome::Resting) => Some(Ending::Resting),
        Some(LifecycleOutcome::Compensated) => Some(Ending::Compensated),
        Some(LifecycleOutcome::Cancelled) => Some(Ending::Cancelled),
        Some(LifecycleOutcome::BreachedVisible) => Some(Ending::BreachedVisible),
        Some(LifecycleOutcome::Completed) => Some(Ending::Good),
        None => chart
            .terminal
            .iter()
            .any(|t| t == state)
            .then_some(Ending::Good),
    };

    let endings: Vec<Option<Ending>> = rows
        .iter()
        .map(|row| match row.get(property) {
            Some(Value::String(state)) => ending(state),
            Some(
                Value::Null
                | Value::Bool(_)
                | Value::Number(_)
                | Value::Array(_)
                | Value::Object(_),
            )
            | None => None,
        })
        .collect();

    let net = Net {
        states: rows.len(),
        initial,
        endings,
        edges: lawful_edges
            .iter()
            .enumerate()
            .map(|(label, edge)| {
                if edge.guarded_unknown {
                    NetEdge::guarded(edge.from, edge.to, label, edge.input)
                } else {
                    NetEdge::evaluated(edge.from, edge.to, label, edge.input)
                }
            })
            .collect(),
    };
    let verdict = soundness::analyse(&net);

    let mut reported_rows = BTreeSet::new();
    for blocked in blocked_exits {
        let Some(outcome) = verdict.states.get(blocked.from) else {
            continue;
        };
        if !outcome.reachable {
            continue;
        }
        let law_blocked_stuck = outcome.stuck();
        if !law_blocked_stuck && !blocked.fact {
            continue;
        }
        if !reported_rows.insert((blocked.from, blocked.transition.as_str())) {
            continue;
        }
        let run_edges = soundness::witness(&net, blocked.from).unwrap_or_default();
        let steps: Vec<WitnessStep> = run_edges
            .iter()
            .filter_map(|&edge_idx| {
                lawful_edges.get(edge_idx).map(|edge| WitnessStep {
                    edge: edge_idx,
                    moves: vec![WitnessMove {
                        side: WitnessSide::Backend,
                        label: edge.transition.clone(),
                    }],
                })
            })
            .collect();
        let mut trace: Vec<String> = run_edges
            .iter()
            .filter_map(|&edge_idx| lawful_edges.get(edge_idx))
            .map(|edge| edge.transition.clone())
            .collect();
        trace.push(format!("blocked:{}", blocked.transition));

        let row = rows.get(blocked.from);
        let state_assignment = row.map(row_assignment).unwrap_or_default();
        let state_label = row
            .and_then(|r| r.get(property))
            .map_or_else(String::new, expr_value_to_string);
        let core_law_id = blocked.laws.join(", ");
        let detail = if blocked.fact && !law_blocked_stuck {
            format!(
                "reachable fact transition {:?} from {property}={state_label} on {object_type:?} is refused by governing core law(s) {core_law_id:?}",
                blocked.transition
            )
        } else {
            format!(
                "reachable state {property}={state_label} on {object_type:?} has no law-satisfying exit: transition {:?} is blocked by governing core law(s) {core_law_id:?}",
                blocked.transition
            )
        };
        findings.push(finding(
            codes::PRESERVATION_VIOLATED,
            artifact,
            "/payload/objectType/lifecycles",
            detail.as_str(),
            Some(DiagnosticWitness::core_law_violation(
                steps,
                "<lifecycle>",
                format!("transition:{}", blocked.transition),
                object_type,
                core_law_id,
                state_assignment,
                BTreeMap::new(),
                trace,
                detail,
            )),
        ));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GuardStatus {
    Enabled,
    Disabled,
    Unknown,
}

fn evaluate_guards(
    guards: &[norn_ir::ZenExpression],
    row: &BTreeMap<String, Value>,
    known_fields: &BTreeSet<&str>,
    work: &mut u64,
) -> Result<GuardStatus, EvalStop> {
    if guards.is_empty() {
        return Ok(GuardStatus::Enabled);
    }
    let context = Value::object([("self", Value::Object(row.clone()))]);
    let mut unknown = false;
    for guard in guards {
        let src = guard.as_str();
        let Ok(self_reads) = norn_expr::expression_read_paths(src, "self") else {
            unknown = true;
            continue;
        };
        let reads_unknown_self = self_reads.iter().any(|path| {
            path.len() != 1
                || !path
                    .first()
                    .is_some_and(|head| known_fields.contains(head.as_str()))
        });
        let reads_external = ["input", "actor", "event", "slice", "aggregates", "clock"]
            .iter()
            .any(|root| {
                norn_expr::expression_read_paths(src, root).map_or(true, |paths| !paths.is_empty())
            });
        if reads_unknown_self || reads_external {
            unknown = true;
            continue;
        }
        let Ok(expr) = Expression::compile(src) else {
            unknown = true;
            continue;
        };
        if !eval_bool(&expr, &context, work)? {
            return Ok(GuardStatus::Disabled);
        }
    }
    Ok(if unknown {
        GuardStatus::Unknown
    } else {
        GuardStatus::Enabled
    })
}

fn finite_property_domain(effective: &EffectiveObjectType, field: &str) -> Option<Vec<Value>> {
    let prop = effective.property(field)?;
    let mut domain = match &prop.kind {
        EffectiveKind::Base(PropertyType::Boolean {}) => {
            vec![Value::Bool(false), Value::Bool(true)]
        }
        EffectiveKind::Base(PropertyType::Enum { values })
            if !values.is_empty() && values.len() <= MAX_FINITE_ENUM =>
        {
            values.iter().cloned().map(Value::String).collect()
        }
        EffectiveKind::Base(
            PropertyType::Text { .. }
            | PropertyType::Integer { .. }
            | PropertyType::Decimal { .. }
            | PropertyType::DecimalBinding { .. }
            | PropertyType::Timestamp { .. }
            | PropertyType::Date { .. }
            | PropertyType::Duration { .. }
            | PropertyType::Enum { .. }
            | PropertyType::Record { .. }
            | PropertyType::JsonExt { .. }
            | PropertyType::Ref { .. },
        )
        | EffectiveKind::Registry(_)
        | EffectiveKind::Unresolved => return None,
    };
    if prop.nullable {
        domain.push(Value::Null);
    }
    Some(domain)
}

fn build_rows(
    fields: &[String],
    domains: &BTreeMap<String, Vec<Value>>,
) -> Option<Vec<BTreeMap<String, Value>>> {
    let mut rows = vec![BTreeMap::new()];
    for field in fields {
        let values = domains.get(field)?;
        let next_len = rows.len().checked_mul(values.len())?;
        if next_len > MAX_ROWS {
            return None;
        }
        let mut next = Vec::with_capacity(next_len);
        for row in &rows {
            for val in values {
                let mut cloned = row.clone();
                cloned.insert(field.clone(), val.clone());
                next.push(cloned);
            }
        }
        rows = next;
    }
    Some(rows)
}

fn row_key(row: &BTreeMap<String, Value>) -> Vec<(String, String)> {
    row.iter()
        .map(|(k, v)| (k.clone(), expr_value_to_string(v)))
        .collect()
}

fn row_assignment(row: &BTreeMap<String, Value>) -> BTreeMap<String, String> {
    row.iter()
        .map(|(k, v)| (k.clone(), expr_value_to_string(v)))
        .collect()
}

fn lifecycle_budget_finding(
    artifact: &ArtifactEnvelope,
    object_type: &str,
    property: &str,
) -> Diagnostic {
    finding(
        codes::PRESERVATION_UNKNOWN,
        artifact,
        "/payload/objectType/lifecycles",
        format!(
            "bounded finite lifecycle core-law check for {object_type:?}.{property} exhausted its work budget"
        ),
        None,
    )
}
