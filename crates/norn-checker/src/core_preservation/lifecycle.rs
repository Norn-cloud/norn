//! Bounded finite lifecycle soundness under governing `core.*` laws:
//! detects reachable non-terminal states where inherited `core.*` laws block
//! all exit transitions (or refuse a reachable fact transition) and attaches a
//! shortest-run counterexample witness.

use std::collections::{BTreeMap, BTreeSet};

use norn_expr::{Expression, Value};
use norn_ir::{
    ArtifactEnvelope, EntityRoot, InterfaceDef, InterfaceName, LawIR, LifecycleOutcome,
    PropertyType, Transition, TransitionTrigger,
};

use super::eval::{
    EvalStop, MAX_FINITE_ENUM, WORK_BUDGET, eval_bool, evaluate_core_rule, expr_value_to_string,
    rule_single_hop_fields,
};
use super::{GoverningCoreRule, codes, finding, governing_core_rules, is_core_name};
use crate::entity_artifacts::{Decoded, decode_v2};
use crate::entity_instance::{EffectiveKind, EffectiveLifecycle, EffectiveObjectType};
use crate::entity_scopes::ScopedViews;
use crate::soundness::{self, Completion, Ending, Net, NetEdge};
use crate::{Diagnostic, DiagnosticWitness, WitnessMove, WitnessSide, WitnessStep};

const MAX_ROWS: usize = 256;

#[derive(Default)]
struct RowEvaluation {
    failed: Vec<String>,
    unknown: Vec<String>,
}

struct LawfulEdge {
    from: usize,
    to: usize,
    transition: String,
    input: bool,
    guarded_unknown: bool,
}

struct ExitFailure {
    from: usize,
    transition: String,
    laws: Vec<String>,
    fact: bool,
}

struct LifecycleGraph {
    lawful_edges: Vec<LawfulEdge>,
    blocked_exits: Vec<ExitFailure>,
    unknown_exits: Vec<ExitFailure>,
}

struct LifecycleReportContext<'a> {
    artifact: &'a ArtifactEnvelope,
    object_type: &'a str,
    property: &'a str,
    rows: &'a [BTreeMap<String, Value>],
    lawful_edges: &'a [LawfulEdge],
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

fn check_one_lifecycle(
    artifact: &ArtifactEnvelope,
    effective: &EffectiveObjectType,
    machine: &EffectiveLifecycle,
    all_rules: &[GoverningCoreRule],
    findings: &mut Vec<Diagnostic>,
) {
    let chart = &machine.lifecycle;
    let property = chart.property.as_str();
    let object_type = effective.name.as_str();
    let (relevant_rules, domains) = collect_lifecycle_domains(effective, machine, all_rules);
    if relevant_rules.is_empty() {
        return;
    }

    let ordered_fields: Vec<String> = domains.keys().cloned().collect();
    let Some(rows) = build_rows(&ordered_fields, &domains) else {
        findings.push(finding(
            codes::PRESERVATION_UNKNOWN,
            artifact,
            "/payload/objectType/lifecycles",
            format!(
                "finite lifecycle core-law check for {object_type:?}.{property} exceeded its state budget ({MAX_ROWS})"
            ),
            None,
        ));
        return;
    };

    let mut work = WORK_BUDGET;
    let Some(eval_by_row) = evaluate_rows(&rows, &relevant_rules, &mut work) else {
        findings.push(lifecycle_budget_finding(artifact, object_type, property));
        return;
    };

    if let Some(init_diag) = check_illegal_initial_states(
        artifact,
        object_type,
        property,
        &chart.initial,
        &rows,
        &eval_by_row,
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
                    && eval_by_row.get(idx).is_some_and(|e| e.failed.is_empty()) =>
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
    let Some(graph) = build_lifecycle_graph(chart, &rows, &eval_by_row, &known_fields, &mut work)
    else {
        findings.push(lifecycle_budget_finding(artifact, object_type, property));
        return;
    };

    if graph.blocked_exits.is_empty() && graph.unknown_exits.is_empty() {
        return;
    }

    let ctx = LifecycleReportContext {
        artifact,
        object_type,
        property,
        rows: &rows,
        lawful_edges: &graph.lawful_edges,
    };
    report_lifecycle_exits(&ctx, chart, initial, &graph, findings);
}

fn collect_lifecycle_domains<'a>(
    effective: &EffectiveObjectType,
    machine: &EffectiveLifecycle,
    all_rules: &'a [GoverningCoreRule],
) -> (Vec<&'a GoverningCoreRule>, BTreeMap<String, Vec<Value>>) {
    let property = machine.lifecycle.property.as_str();
    let mut relevant_rules: Vec<&'a GoverningCoreRule> = Vec::new();
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
        return (relevant_rules, BTreeMap::new());
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
    (relevant_rules, domains)
}

fn evaluate_rows(
    rows: &[BTreeMap<String, Value>],
    relevant_rules: &[&GoverningCoreRule],
    work: &mut u64,
) -> Option<Vec<RowEvaluation>> {
    let mut eval_by_row: Vec<RowEvaluation> = Vec::with_capacity(rows.len());
    for row in rows {
        let mut eval = RowEvaluation::default();
        for rule in relevant_rules {
            match evaluate_core_rule(rule, row, work) {
                Ok(true) => {}
                Ok(false) => eval.failed.push(rule.id.clone()),
                Err(EvalStop::Other) => eval.unknown.push(rule.id.clone()),
                Err(EvalStop::Budget) => return None,
            }
        }
        eval_by_row.push(eval);
    }
    Some(eval_by_row)
}

fn build_lifecycle_graph(
    chart: &norn_ir::Lifecycle,
    rows: &[BTreeMap<String, Value>],
    eval_by_row: &[RowEvaluation],
    known_fields: &BTreeSet<&str>,
    work: &mut u64,
) -> Option<LifecycleGraph> {
    let property = chart.property.as_str();
    let row_index: BTreeMap<Vec<(String, String)>, usize> = rows
        .iter()
        .enumerate()
        .map(|(idx, row)| (row_key(row), idx))
        .collect();
    let mut graph = LifecycleGraph {
        lawful_edges: Vec::new(),
        blocked_exits: Vec::new(),
        unknown_exits: Vec::new(),
    };

    for (from, row) in rows.iter().enumerate() {
        let Some(Value::String(state)) = row.get(property) else {
            continue;
        };
        if !eval_by_row.get(from).is_some_and(|e| e.failed.is_empty()) {
            continue;
        }
        for transition in &chart.transitions {
            if !transition.from.contains(state) {
                continue;
            }
            let guard_status = match evaluate_guards(&transition.guards, row, known_fields, work) {
                Ok(status) => status,
                Err(EvalStop::Budget) => return None,
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
            record_transition_outcome(
                from,
                to,
                transition,
                guard_status,
                eval_by_row.get(to),
                &mut graph,
            );
        }
    }

    Some(graph)
}

fn record_transition_outcome(
    from: usize,
    to: usize,
    transition: &Transition,
    guard_status: GuardStatus,
    post_eval: Option<&RowEvaluation>,
    graph: &mut LifecycleGraph,
) {
    let transition_id = transition.id.as_str().to_owned();
    let fact = transition.is_fact();
    let post_failed = post_eval.map_or_else(Vec::new, |e| e.failed.clone());
    if !post_failed.is_empty() {
        let exit = ExitFailure {
            from,
            transition: transition_id,
            laws: post_failed,
            fact,
        };
        if guard_status == GuardStatus::Enabled {
            graph.blocked_exits.push(exit);
        } else {
            graph.unknown_exits.push(exit);
        }
        return;
    }
    let post_unknown = post_eval.map_or_else(Vec::new, |e| e.unknown.clone());
    let is_timer = matches!(transition.trigger, Some(TransitionTrigger::Timer { .. }));
    let guarded_unknown = !post_unknown.is_empty() || guard_status == GuardStatus::Unknown;
    if !post_unknown.is_empty() {
        graph.unknown_exits.push(ExitFailure {
            from,
            transition: transition_id.clone(),
            laws: post_unknown,
            fact,
        });
    }
    graph.lawful_edges.push(LawfulEdge {
        from,
        to,
        transition: transition_id,
        input: !is_timer,
        guarded_unknown,
    });
}

fn check_illegal_initial_states(
    artifact: &ArtifactEnvelope,
    object_type: &str,
    property: &str,
    initial_states: &[String],
    rows: &[BTreeMap<String, Value>],
    eval_by_row: &[RowEvaluation],
) -> Option<Diagnostic> {
    for init_state in initial_states {
        let any_proved_legal = rows.iter().enumerate().any(|(idx, row)| {
            row.get(property) == Some(&Value::String(init_state.clone()))
                && eval_by_row
                    .get(idx)
                    .is_some_and(|e| e.failed.is_empty() && e.unknown.is_empty())
        });
        if any_proved_legal {
            continue;
        }
        if let Some((idx, _)) = rows.iter().enumerate().find(|(idx, row)| {
            row.get(property) == Some(&Value::String(init_state.clone()))
                && eval_by_row
                    .get(*idx)
                    .is_some_and(|e| e.failed.is_empty() && !e.unknown.is_empty())
        }) && let Some(eval) = eval_by_row.get(idx)
        {
            let core_law_id = eval.unknown.join(", ");
            return Some(finding(
                codes::PRESERVATION_UNKNOWN,
                artifact,
                "/payload/objectType/lifecycles",
                format!(
                    "initial lifecycle state {init_state:?} on {object_type:?}.{property} could not be statically evaluated against governing core law(s) {core_law_id:?}"
                ),
                None,
            ));
        }
        if let Some((idx, row)) = rows
            .iter()
            .enumerate()
            .find(|(_, row)| row.get(property) == Some(&Value::String(init_state.clone())))
            && let Some(eval) = eval_by_row.get(idx)
            && !eval.failed.is_empty()
        {
            let core_law_id = eval.failed.join(", ");
            let detail = format!(
                "initial lifecycle state {init_state:?} on {object_type:?}.{property} violates governing core law(s) {core_law_id:?}"
            );
            return Some(finding(
                codes::PRESERVATION_VIOLATED,
                artifact,
                "/payload/objectType/lifecycles",
                detail.clone(),
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

fn report_lifecycle_exits(
    ctx: &LifecycleReportContext<'_>,
    chart: &norn_ir::Lifecycle,
    initial: Vec<usize>,
    graph: &LifecycleGraph,
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
    let endings: Vec<Option<Ending>> = ctx
        .rows
        .iter()
        .map(|row| match row.get(ctx.property) {
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
        states: ctx.rows.len(),
        initial,
        endings,
        edges: ctx
            .lawful_edges
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
    let mut reported = BTreeSet::new();
    report_blocked_exits(
        ctx,
        &net,
        &verdict,
        &graph.blocked_exits,
        &mut reported,
        findings,
    );
    report_unknown_exits(ctx, &verdict, &graph.unknown_exits, &mut reported, findings);
}

fn report_blocked_exits<'a>(
    ctx: &LifecycleReportContext<'_>,
    net: &Net,
    verdict: &soundness::Verdict,
    blocked_exits: &'a [ExitFailure],
    reported_rows: &mut BTreeSet<(usize, &'a str)>,
    findings: &mut Vec<Diagnostic>,
) {
    let (property, object_type) = (ctx.property, ctx.object_type);
    for blocked in blocked_exits {
        let Some(outcome) = verdict.states.get(blocked.from) else {
            continue;
        };
        if !outcome.reachable {
            continue;
        }
        let law_blocked_stuck = outcome.stuck();
        if !law_blocked_stuck && !blocked.fact {
            if outcome.completion == Completion::Unknown
                && reported_rows.insert((blocked.from, blocked.transition.as_str()))
            {
                let state_label = state_label_at(ctx.rows, blocked.from, property);
                let core_law_id = blocked.laws.join(", ");
                findings.push(finding(
                    codes::PRESERVATION_UNKNOWN,
                    ctx.artifact,
                    "/payload/objectType/lifecycles",
                    format!(
                        "reachable state {property}={state_label} on {object_type:?} has transition {:?} blocked by core law(s) {core_law_id:?} and no proved lawful exit (remaining exits are unevaluated or conditionally guarded)",
                        blocked.transition
                    ),
                    None,
                ));
            }
            continue;
        }
        if !reported_rows.insert((blocked.from, blocked.transition.as_str())) {
            continue;
        }
        findings.push(build_blocked_exit_finding(
            ctx,
            net,
            blocked,
            law_blocked_stuck,
        ));
    }
}

fn build_blocked_exit_finding(
    ctx: &LifecycleReportContext<'_>,
    net: &Net,
    blocked: &ExitFailure,
    law_blocked_stuck: bool,
) -> Diagnostic {
    let (property, object_type) = (ctx.property, ctx.object_type);
    let run_edges = soundness::witness(net, blocked.from).unwrap_or_default();
    let mut steps = Vec::with_capacity(run_edges.len());
    let mut trace = Vec::with_capacity(run_edges.len().saturating_add(1));
    for &edge_idx in &run_edges {
        if let Some(edge) = ctx.lawful_edges.get(edge_idx) {
            steps.push(WitnessStep {
                edge: edge_idx,
                moves: vec![WitnessMove {
                    side: WitnessSide::Backend,
                    label: edge.transition.clone(),
                }],
            });
            trace.push(edge.transition.clone());
        }
    }
    trace.push(format!("blocked:{}", blocked.transition));

    let state_assignment = ctx
        .rows
        .get(blocked.from)
        .map_or_else(BTreeMap::new, row_assignment);
    let state_label = state_label_at(ctx.rows, blocked.from, property);
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
    finding(
        codes::PRESERVATION_VIOLATED,
        ctx.artifact,
        "/payload/objectType/lifecycles",
        detail.clone(),
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
    )
}

fn report_unknown_exits<'a>(
    ctx: &LifecycleReportContext<'_>,
    verdict: &soundness::Verdict,
    unknown_exits: &'a [ExitFailure],
    reported_rows: &mut BTreeSet<(usize, &'a str)>,
    findings: &mut Vec<Diagnostic>,
) {
    let (property, object_type) = (ctx.property, ctx.object_type);
    for unknown in unknown_exits {
        let Some(outcome) = verdict.states.get(unknown.from) else {
            continue;
        };
        if !outcome.reachable || (outcome.completion == Completion::Must && !unknown.fact) {
            continue;
        }
        if !reported_rows.insert((unknown.from, unknown.transition.as_str())) {
            continue;
        }
        let state_label = state_label_at(ctx.rows, unknown.from, property);
        let core_law_id = unknown.laws.join(", ");
        findings.push(finding(
            codes::PRESERVATION_UNKNOWN,
            ctx.artifact,
            "/payload/objectType/lifecycles",
            format!(
                "reachable state {property}={state_label} on {object_type:?} has no proved lawful exit: destination of transition {:?} could not be statically evaluated against governing core law(s) {core_law_id:?}",
                unknown.transition
            ),
            None,
        ));
    }
}

fn state_label_at(rows: &[BTreeMap<String, Value>], idx: usize, property: &str) -> String {
    rows.get(idx)
        .and_then(|r| r.get(property))
        .map_or_else(String::new, expr_value_to_string)
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
            values
                .iter()
                .map(|entry| Value::String(entry.value.clone()))
                .collect()
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
