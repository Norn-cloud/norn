//! Flow resource limits reporting (`tools/norn-ts/src/flow-limits.ts`).
//!
//! Computes per-flow durable step counts, retry attempts and delays, decision
//! row counts, command/timer fan-out, and checks them against [`FLOW_MAX_RECURS`],
//! [`FLOW_MAX_TIMER_MILLIS`], [`FLOW_MAX_STEPS`], [`FLOW_MAX_ROWS`], and a
//! `norn.limits/1` envelope [`LimitsProfile`].

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::flow_source_map::{
    FLOW_MAX_RECURS, FLOW_MAX_ROWS, FLOW_MAX_STEPS, FLOW_MAX_TIMER_MILLIS, FlowDef, Step,
    retry_delay,
};

/// Envelope limits section of a `norn.limits/1` profile.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LimitsEnvelope {
    /// Maximum commands emitted by one decision.
    pub max_commands: usize,
    /// Maximum events emitted by one decision.
    pub max_events: usize,
    /// Maximum output bytes emitted by one decision.
    pub max_output_bytes: usize,
    /// Maximum timers emitted by one decision.
    pub max_timers: usize,
}

/// Kernel-owned `norn.limits/1` resource profile.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LimitsProfile {
    /// Profile identifier (for example `"norn.limits"`).
    pub id: String,
    /// Profile schema version (for example `1`).
    pub version: u32,
    /// Per-decision envelope ceilings.
    pub envelope: LimitsEnvelope,
}

/// Kind of durable step reported in [`StepLimits`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StepLimitKind {
    /// `Norn.perform` (with or without `Effect.retry`).
    Perform,
    /// `Effect.sleep`.
    Sleep,
}

/// Resource summary for one durable flow step.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StepLimits {
    /// Step identifier.
    pub step: String,
    /// Durable step kind.
    pub kind: StepLimitKind,
    /// Target function `<id>@<version>` for `perform` steps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function: Option<String>,
    /// Total attempts including the first (`recurs + 1`) for `perform` steps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_attempts: Option<u32>,
    /// Ordered retry timer delays in milliseconds for `perform` steps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_delays_millis: Option<Vec<f64>>,
    /// Timer duration in milliseconds for `sleep` steps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timer_millis: Option<f64>,
}

/// Result of checking one flow metric against its configured ceiling.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LimitCheck {
    /// Limit identifier.
    pub limit: String,
    /// Measured value.
    pub value: f64,
    /// Maximum permitted value.
    pub max: f64,
    /// Whether `value <= max`.
    pub ok: bool,
}

/// Full resource limit summary for one lowered flow.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlowLimits {
    /// Flow `<id>@<version>`.
    pub flow: String,
    /// Per-step resource summaries in traversal order.
    pub steps: Vec<StepLimits>,
    /// Count of durable steps (`perform` + `sleep`).
    pub durable_steps: usize,
    /// Count of generated decision-table rows (excluding the blank catch-all).
    pub rows: usize,
    /// Maximum commands emitted by any single decision row.
    pub max_fan_out: usize,
    /// Maximum timers emitted by any single decision row.
    pub max_timers_per_decision: usize,
    /// Longest single timer delay in milliseconds.
    pub max_timer_millis: f64,
    /// Per-decision output byte ceiling from the limits profile.
    pub max_output_bytes: usize,
    /// Static and envelope limit checks.
    pub checks: Vec<LimitCheck>,
}

fn walk_steps(steps: &[Step], out: &mut Vec<StepLimits>) {
    for step in steps {
        match step {
            Step::Perform(p) => out.push(StepLimits {
                step: p.step.clone(),
                kind: StepLimitKind::Perform,
                function: Some(format!("{}@{}", p.func.id, p.func.version)),
                max_attempts: Some(1),
                retry_delays_millis: Some(Vec::new()),
                timer_millis: None,
            }),
            Step::Retry {
                perform, policy, ..
            } => {
                let recurs = policy.recurs.unwrap_or(0);
                let mut delays = Vec::with_capacity(usize::try_from(recurs).unwrap_or(0));
                for attempt in 1..=recurs {
                    delays.push(retry_delay(policy, attempt));
                }
                out.push(StepLimits {
                    step: perform.step.clone(),
                    kind: StepLimitKind::Perform,
                    function: Some(format!("{}@{}", perform.func.id, perform.func.version)),
                    max_attempts: Some(recurs.saturating_add(1)),
                    retry_delays_millis: Some(delays),
                    timer_millis: None,
                });
            }
            Step::Sleep { step, millis, .. } => out.push(StepLimits {
                step: step.clone(),
                kind: StepLimitKind::Sleep,
                function: None,
                max_attempts: None,
                retry_delays_millis: None,
                timer_millis: Some(*millis),
            }),
            Step::Route {
                branches,
                otherwise,
                ..
            } => {
                for branch in branches {
                    walk_steps(&branch.then, out);
                }
                walk_steps(otherwise, out);
            }
            Step::CatchTag { body, handler, .. } => {
                walk_steps(body, out);
                walk_steps(handler, out);
            }
            Step::Compose { body, .. } => walk_steps(body, out),
            Step::All { steps, .. } => walk_steps(steps, out),
            Step::Fail { .. } | Step::Succeed { .. } | Step::Value { .. } => {}
        }
    }
}

fn decide_table_rows(rule: Option<&Value>) -> Vec<&serde_json::Map<String, Value>> {
    let Some(nodes) = rule
        .and_then(|r| r.get("payload"))
        .and_then(|p| p.get("graph"))
        .and_then(|g| g.get("nodes"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    for node in nodes {
        if node.get("type").and_then(Value::as_str) == Some("decisionTableNode")
            && let Some(rules) = node
                .get("content")
                .and_then(|c| c.get("rules"))
                .and_then(Value::as_array)
        {
            return rules.iter().filter_map(Value::as_object).collect();
        }
    }
    Vec::new()
}

fn usize_to_f64(value: usize) -> f64 {
    u32::try_from(value).map_or(f64::from(u32::MAX), f64::from)
}

fn u64_to_f64(value: u64) -> f64 {
    let hi = u32::try_from(value >> 32).unwrap_or(0);
    let lo = u32::try_from(value & 0xFFFF_FFFF).unwrap_or(0);
    f64::from(hi).mul_add(4_294_967_296.0, f64::from(lo))
}

/// Computes the resource limits summary for one lowered flow.
#[must_use]
pub fn flow_limits(def: &FlowDef, artifacts: &[Value], profile: &LimitsProfile) -> FlowLimits {
    let mut steps = Vec::new();
    walk_steps(&def.body, &mut steps);
    let rule = artifacts
        .iter()
        .find(|a| a.get("kind").and_then(Value::as_str) == Some("rule"));
    let table: Vec<&serde_json::Map<String, Value>> = decide_table_rows(rule)
        .into_iter()
        .filter(|row| {
            row.get("c_when")
                .and_then(Value::as_str)
                .is_some_and(|w| !w.trim().is_empty())
        })
        .collect();

    let max_fan_out = table
        .iter()
        .map(|r| list_length(r.get("o_commands").and_then(Value::as_str).unwrap_or("")))
        .max()
        .unwrap_or(0);
    let max_timers_per_decision = table
        .iter()
        .map(|r| list_length(r.get("o_timers").and_then(Value::as_str).unwrap_or("")))
        .max()
        .unwrap_or(0);

    let mut max_timer_millis = 0.0_f64;
    let mut max_recurs: u32 = 0;
    for s in &steps {
        if let Some(delays) = &s.retry_delays_millis {
            for &d in delays {
                if d > max_timer_millis {
                    max_timer_millis = d;
                }
            }
        }
        if let Some(t) = s.timer_millis
            && t > max_timer_millis
        {
            max_timer_millis = t;
        }
        if let Some(attempts) = s.max_attempts {
            max_recurs = max_recurs.max(attempts.saturating_sub(1));
        }
    }

    let checks = build_flow_limit_checks(
        max_recurs,
        max_timer_millis,
        steps.len(),
        table.len(),
        max_fan_out,
        max_timers_per_decision,
        profile,
    );

    FlowLimits {
        flow: format!("{}@{}", def.meta.id, def.meta.version),
        durable_steps: steps.len(),
        steps,
        rows: table.len(),
        max_fan_out,
        max_timers_per_decision,
        max_timer_millis,
        max_output_bytes: profile.envelope.max_output_bytes,
        checks,
    }
}

fn build_flow_limit_checks(
    max_recurs: u32,
    max_timer_millis: f64,
    step_count: usize,
    row_count: usize,
    max_fan_out: usize,
    max_timers_per_decision: usize,
    profile: &LimitsProfile,
) -> Vec<LimitCheck> {
    let check = |limit: String, value: f64, max: f64| LimitCheck {
        limit,
        value,
        max,
        ok: value <= max,
    };
    vec![
        check(
            "flow.maxRecurs".to_owned(),
            f64::from(max_recurs),
            f64::from(FLOW_MAX_RECURS),
        ),
        check(
            "flow.maxTimerMillis".to_owned(),
            max_timer_millis,
            u64_to_f64(FLOW_MAX_TIMER_MILLIS),
        ),
        check(
            "flow.maxSteps".to_owned(),
            usize_to_f64(step_count),
            usize_to_f64(FLOW_MAX_STEPS),
        ),
        check(
            "flow.maxRows".to_owned(),
            usize_to_f64(row_count),
            usize_to_f64(FLOW_MAX_ROWS),
        ),
        check(
            format!("{}/{}.envelope.max_commands", profile.id, profile.version),
            usize_to_f64(max_fan_out),
            usize_to_f64(profile.envelope.max_commands),
        ),
        check(
            format!("{}/{}.envelope.max_timers", profile.id, profile.version),
            usize_to_f64(max_timers_per_decision),
            usize_to_f64(profile.envelope.max_timers),
        ),
    ]
}

/// Formats human-readable limit summaries (`renderFlowLimits` in `flow-limits.ts`).
#[must_use]
pub fn render_flow_limits(limits: &[FlowLimits]) -> String {
    let mut lines = Vec::new();
    for f in limits {
        lines.push(format!(
            "flow {}: {} durable steps, {} rows, fan-out {}, {} timer(s)/decision, longest timer {} ms, output cap {} B/decision",
            f.flow,
            f.durable_steps,
            f.rows,
            f.max_fan_out,
            f.max_timers_per_decision,
            f.max_timer_millis,
            f.max_output_bytes
        ));
        for s in &f.steps {
            match s.kind {
                StepLimitKind::Perform => {
                    let func = s.function.as_deref().unwrap_or("");
                    let attempts = s.max_attempts.unwrap_or(1);
                    let retry_note = match &s.retry_delays_millis {
                        Some(delays) if !delays.is_empty() => {
                            let joined = delays
                                .iter()
                                .map(ToString::to_string)
                                .collect::<Vec<_>>()
                                .join("/");
                            format!(", retry timers {joined} ms")
                        }
                        Some(_) | None => String::new(),
                    };
                    lines.push(format!(
                        "  perform {} -> {}: max {} attempt(s){}",
                        s.step, func, attempts, retry_note
                    ));
                }
                StepLimitKind::Sleep => {
                    lines.push(format!(
                        "  sleep {}: {} ms",
                        s.step,
                        s.timer_millis.unwrap_or(0.0)
                    ));
                }
            }
        }
        for c in f.checks.iter().filter(|c| !c.ok) {
            lines.push(format!("  OVER LIMIT {}: {} > {}", c.limit, c.value, c.max));
        }
    }
    lines.join("\n")
}

/// Counts top-level elements of a rendered ZEN list literal (`[{...}, {...}]`),
/// skipping strings and nested brackets (`listLength` in `flow-limits.ts`).
#[must_use]
pub(crate) fn list_length(text: &str) -> usize {
    let source = text.trim();
    if !source.starts_with('[') {
        return 0;
    }
    let mut depth: usize = 0;
    let mut elements: usize = 0;
    let mut quote: Option<char> = None;
    let mut saw_value = false;
    let mut chars = source.chars();
    while let Some(ch) = chars.next() {
        if let Some(q) = quote {
            if ch == '\\' {
                let _skipped = chars.next();
            } else if ch == q {
                quote = None;
            }
            continue;
        }
        if ch == '\'' || ch == '"' {
            quote = Some(ch);
            if depth == 1 {
                saw_value = true;
            }
            continue;
        }
        if matches!(ch, '[' | '{' | '(') {
            if depth == 1 {
                saw_value = true;
            }
            depth = depth.saturating_add(1);
            continue;
        }
        if matches!(ch, ']' | '}' | ')') {
            depth = depth.saturating_sub(1);
            if depth == 0 {
                return elements.saturating_add(usize::from(saw_value));
            }
            continue;
        }
        if depth == 1 {
            if ch == ',' {
                elements = elements.saturating_add(1);
                saw_value = false;
            } else if !ch.is_whitespace() {
                saw_value = true;
            }
        }
    }
    elements.saturating_add(usize::from(saw_value))
}
