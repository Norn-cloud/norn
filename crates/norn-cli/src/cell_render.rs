//! Rendering helpers for `norn cell plan|apply` summaries, ripples, refusals and witnesses.

use serde_json::Value;

pub(super) fn text(value: &Value, key: &str) -> String {
    value
        .get(key)
        .map_or_default(|v| v.as_str().map_or_else(|| v.to_string(), str::to_owned))
}

/// Append one line to `out`.
fn line(out: &mut String, text: &str) {
    out.push_str(text);
    out.push('\n');
}

pub(super) fn plan_summary(plan: &Value) -> String {
    let mut out = String::new();
    line(
        &mut out,
        &format!(
            "plan over revision {} -> {}{}",
            text(plan, "baseVersion"),
            text(plan, "nextVersion"),
            if plan.get("unchanged") == Some(&Value::Bool(true)) {
                " (unchanged: the pack is already installed)"
            } else {
                ""
            }
        ),
    );
    for (key, mark) in [("added", "+"), ("changed", "~")] {
        for artifact in plan
            .get(key)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let from = artifact
                .get("fromVersion")
                .and_then(Value::as_str)
                .map_or_default(|from| format!("{from} -> "));
            line(
                &mut out,
                &format!(
                    "  {mark} {} ({} {from}{})",
                    text(artifact, "artifactId"),
                    text(artifact, "kind"),
                    text(artifact, "toVersion")
                ),
            );
        }
    }
    for id in plan
        .get("same")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        line(
            &mut out,
            &format!("  = {}", id.as_str().unwrap_or_default()),
        );
    }
    let types: Vec<&str> = plan
        .get("newObjectTypes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    if !types.is_empty() {
        line(&mut out, &format!("  new types: {}", types.join(", ")));
    }
    line(
        &mut out,
        &format!(
            "  checked {} stored object(s) under the new laws",
            text(plan, "checkedObjects")
        ),
    );
    plan_ripples(&mut out, plan);
    diagnostics(&mut out, plan);
    out.trim_end().to_owned()
}

fn plan_ripples(out: &mut String, plan: &Value) {
    if let Some(report) = plan.get("ripples").and_then(Value::as_object) {
        if report.get("status").and_then(Value::as_str) == Some("unknown") {
            let detail = report
                .get("reason")
                .and_then(Value::as_str)
                .or_else(|| {
                    report
                        .get("budgetExceeded")
                        .and_then(|v| v.get("detail"))
                        .and_then(Value::as_str)
                })
                .unwrap_or("graph budget exceeded");
            line(out, &format!("  ripples: UNKNOWN ({detail})"));
            return;
        }
        if let Some(ripples) = report.get("ripples").and_then(Value::as_array) {
            render_ripple_entries(out, ripples);
        }
        return;
    }
    if plan.get("rippleStatus").and_then(Value::as_str) == Some("unknown") {
        let detail = plan
            .get("rippleReason")
            .and_then(Value::as_str)
            .or_else(|| {
                plan.pointer("/rippleBudgetExceeded/detail")
                    .and_then(Value::as_str)
            })
            .unwrap_or("graph budget exceeded");
        line(out, &format!("  ripples: UNKNOWN ({detail})"));
        return;
    }
    if let Some(ripples) = plan.get("ripples").and_then(Value::as_array) {
        render_ripple_entries(out, ripples);
    }
}

fn render_ripple_entries(out: &mut String, ripples: &[Value]) {
    if ripples.is_empty() {
        return;
    }
    line(out, &format!("  transitive ripples ({}):", ripples.len()));
    for ripple in ripples {
        let via: Vec<&str> = ripple
            .get("via")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        let chain = if via.is_empty() {
            String::new()
        } else {
            format!(" via {}", via.join(" -> "))
        };
        line(
            out,
            &format!(
                "    [{}] {} [{}]{chain}",
                text(ripple, "impactKind"),
                text(ripple, "artifactId"),
                text(ripple, "kind")
            ),
        );
    }
}

pub(crate) fn format_witness(witness: &Value) -> String {
    if let Some(steps) = witness.get("steps").and_then(Value::as_array) {
        let base = if steps.is_empty() {
            "(initial state, 0 steps)".to_owned()
        } else {
            steps
                .iter()
                .map(|step| {
                    let edge = text(step, "edge");
                    let moves: Vec<String> = step
                        .get("moves")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                        .map(|mv| format!("{}:{}", text(mv, "side"), text(mv, "label")))
                        .collect();
                    format!("edge {edge} [{}]", moves.join(", "))
                })
                .collect::<Vec<_>>()
                .join(" -> ")
        };
        if let Some(obj) = witness.as_object() {
            let mut extras: Vec<(&str, &Value)> = obj
                .iter()
                .filter(|(k, _)| k.as_str() != "steps")
                .map(|(k, v)| (k.as_str(), v))
                .collect();
            extras.sort_by_key(|(key, _)| *key);
            if !extras.is_empty() {
                let tail: Vec<String> = extras
                    .into_iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect();
                return format!("{base}; {}", tail.join("; "));
            }
        }
        return base;
    }
    witness.to_string()
}

fn diagnostics(out: &mut String, document: &Value) {
    for finding in document
        .get("diagnostics")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let at = finding
            .pointer("/span/pointer")
            .and_then(Value::as_str)
            .map_or_default(|pointer| format!(" at {pointer}"));
        line(
            out,
            &format!(
                "  {} {}{at}: {}",
                text(finding, "severity"),
                text(finding, "code"),
                text(finding, "message")
            ),
        );
        if let Some(witness) = finding.get("witness").filter(|value| !value.is_null()) {
            line(out, &format!("    witness: {}", format_witness(witness)));
        }
    }
}

fn refusal_summary(status: u16, refusal: &Value) -> String {
    let mut out = format!(
        "refused ({status}) {}: {}\n  {}\n",
        text(refusal, "code"),
        text(refusal, "message"),
        text(refusal, "explanation")
    );
    diagnostics(&mut out, refusal);
    out.trim_end().to_owned()
}

pub(super) fn answer(
    json_out: bool,
    status: u16,
    document: &Value,
    summary: impl Fn(&Value) -> String,
) -> (String, bool) {
    let accepted = (200..300).contains(&status);
    let shown = if json_out {
        document.to_string()
    } else if accepted {
        summary(document)
    } else {
        refusal_summary(status, document)
    };
    (shown, accepted)
}
