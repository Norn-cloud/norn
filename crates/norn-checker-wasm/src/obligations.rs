//! Read-only proof obligation summary over admitted artifact sets.
//!
//! Reuses `norn_checker::obligations` to reconstruct the conservative goal
//! inventory (`GENERATOR_PROFILE`) without claiming proof or granting
//! activation authority.

use std::collections::{BTreeMap, BTreeSet};

use norn_checker::obligations::{ApprovedContract, DefinitionSource, GENERATOR_PROFILE};
use norn_ir::{PropertyTypeName, ResourceLimitProfile};
use serde::Deserialize;
use serde_json::value::RawValue;
use serde_json::{Value, json};

/// Format tag of the [`analyze_obligations_json`] summary document.
pub const OBLIGATIONS_SUMMARY_FORMAT: &str = "norn-checker/obligations-summary@1";

/// Maximum number of individual goal entries emitted inline in `goals`.
const MAX_SUMMARY_GOALS: usize = 64;

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ObligationsContextInput {
    #[serde(default)]
    base: Option<Box<RawValue>>,
    #[serde(default)]
    registry: BTreeSet<PropertyTypeName>,
    #[serde(default)]
    tenant: Option<Value>,
    #[serde(default)]
    limits: Option<ResourceLimitProfile>,
    #[serde(default)]
    flow_source_map: Option<Value>,
}

/// Generate a read-only, bounded proof obligation summary for an admitted
/// artifact set and checker context.
///
/// Always reports honest status: `"generated_unproved"` when obligations were
/// reconstructed, or `"unavailable"` with a `reason` when obligation generation
/// is refused or exceeds checker budgets. Never claims a law is proved.
///
/// # Errors
///
/// Returns an error string when `artifacts_json` is not a JSON array or
/// `context_json` is not `null` or a valid checker context object.
pub fn analyze_obligations_json(
    artifacts_json: &str,
    context_json: &str,
) -> Result<String, String> {
    let parsed_artifacts: Value = serde_json::from_str(artifacts_json)
        .map_err(|error| format!("invalid artifacts: {error}"))?;
    if !parsed_artifacts.is_array() {
        return Err("invalid artifacts: expected a JSON array of artifact envelopes".to_owned());
    }
    let context: ObligationsContextInput =
        serde_json::from_str::<Option<ObligationsContextInput>>(context_json)
            .map_err(|error| format!("invalid context: {error}"))?
            .unwrap_or_default();

    let _unused_flow_source_map = context.flow_source_map;
    if context.tenant.is_some() {
        return Ok(unavailable_summary(
            "obligation summary requires a non-tenant base/candidate layer",
        ));
    }
    if context
        .limits
        .as_ref()
        .is_some_and(|limits| *limits != ResourceLimitProfile::norn_limits_v1())
    {
        return Ok(unavailable_summary(
            "obligation summary uses norn.limits/1 qualification limits",
        ));
    }

    let base_json = context.base.as_deref().map_or("[]", RawValue::get);

    let source = match DefinitionSource::parse(artifacts_json, base_json) {
        Ok(source) => source,
        Err(error) => return Ok(unavailable_summary(&error.to_string())),
    };
    let contract = match ApprovedContract::approve(
        &source,
        &context.registry,
        json!({ "exprProfile": norn_checker::receipt::EXPR_PROFILE }),
        json!({ "mode": "read_only_summary" }),
        json!({ "status": "generated_unproved" }),
    ) {
        Ok(contract) => contract,
        Err(error) => return Ok(unavailable_summary(&error.to_string())),
    };
    let challenge = match contract.challenge(&source, &context.registry) {
        Ok(challenge) => challenge,
        Err(error) => return Ok(unavailable_summary(&error.to_string())),
    };

    Ok(summarize_challenge_body(challenge.body()))
}

fn unavailable_summary(reason: &str) -> String {
    json!({
        "format": OBLIGATIONS_SUMMARY_FORMAT,
        "generatorProfile": GENERATOR_PROFILE,
        "status": "unavailable",
        "reason": reason,
        "totalGoals": 0,
        "byKind": {},
        "goals": [],
        "truncated": false,
    })
    .to_string()
}

fn summarize_challenge_body(body: &Value) -> String {
    let laws = body
        .get("lawStatements")
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice);
    let writers = body
        .get("writerStatements")
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice);
    let boundaries = body
        .get("openBoundaries")
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice);
    let raw_goals = body
        .get("obligations")
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice);

    let mut by_kind: BTreeMap<String, usize> = BTreeMap::new();
    let mut goals: Vec<Value> = Vec::with_capacity(raw_goals.len().min(MAX_SUMMARY_GOALS));

    for raw in raw_goals {
        let kind = raw
            .pointer("/goal/kind")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let count = by_kind.entry(kind.to_owned()).or_insert(0);
        *count = count.saturating_add(1);

        if goals.len() < MAX_SUMMARY_GOALS {
            goals.push(project_goal(raw, kind, laws, writers, boundaries));
        }
    }

    let total_goals = raw_goals.len();
    let truncated = total_goals > MAX_SUMMARY_GOALS;
    json!({
        "format": OBLIGATIONS_SUMMARY_FORMAT,
        "generatorProfile": GENERATOR_PROFILE,
        "status": "generated_unproved",
        "totalGoals": total_goals,
        "byKind": by_kind,
        "goals": goals,
        "truncated": truncated,
    })
    .to_string()
}

fn project_goal(
    raw: &Value,
    kind: &str,
    laws: &[Value],
    writers: &[Value],
    boundaries: &[Value],
) -> Value {
    let subject = resolve_subject(raw, laws);
    let target = resolve_target(raw, &subject, writers, boundaries);
    let id = if target == subject {
        format!("{kind}:{subject}")
    } else {
        format!("{kind}:{subject}:{target}")
    };
    let description = describe_goal(kind, &subject, &target);
    let assessment = raw
        .get("assessment")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    json!({
        "id": id,
        "kind": kind,
        "subject": subject,
        "target": target,
        "description": description,
        "assessment": assessment,
    })
}

fn resolve_subject(raw: &Value, laws: &[Value]) -> String {
    if let Some(law_idx) = raw
        .get("law")
        .and_then(Value::as_u64)
        .and_then(|idx| usize::try_from(idx).ok())
        && let Some(artifact_id) = laws
            .get(law_idx)
            .and_then(|entry| entry.get("artifactId"))
            .and_then(Value::as_str)
    {
        return artifact_id.to_owned();
    }
    if let Some(object_type) = raw.pointer("/statement/objectType").and_then(Value::as_str) {
        return match raw.pointer("/statement/property").and_then(Value::as_str) {
            Some(property) => format!("{object_type}.{property}"),
            None => object_type.to_owned(),
        };
    }
    "unknown".to_owned()
}

fn resolve_target(raw: &Value, subject: &str, writers: &[Value], boundaries: &[Value]) -> String {
    if let Some(writer_idx) = raw
        .pointer("/goal/writer")
        .and_then(Value::as_u64)
        .and_then(|idx| usize::try_from(idx).ok())
        && let Some(writer) = writers.get(writer_idx)
    {
        return format_writer_identity(writer.get("identity"));
    }
    if let Some(boundary_idx) = raw
        .pointer("/goal/boundary")
        .and_then(Value::as_u64)
        .and_then(|idx| usize::try_from(idx).ok())
        && let Some(boundary) = boundaries.get(boundary_idx).and_then(Value::as_str)
    {
        return boundary.to_owned();
    }
    if let Some(observation) = raw.pointer("/goal/observation").and_then(Value::as_str) {
        return format!("observation:{observation}");
    }
    if let Some(terminal) = raw.pointer("/goal/terminal").and_then(Value::as_str) {
        return format!("terminal:{terminal}");
    }
    if let Some(transition) = raw.pointer("/goal/transition").and_then(Value::as_str) {
        return format!("transition:{transition}");
    }
    subject.to_owned()
}

fn format_writer_identity(identity: Option<&Value>) -> String {
    let Some(obj) = identity.and_then(Value::as_object) else {
        return "writer:unknown".to_owned();
    };
    let Some(kind) = obj.get("kind").and_then(Value::as_str) else {
        return "writer:unknown".to_owned();
    };
    match kind {
        "artifact" => obj
            .get("artifactId")
            .and_then(Value::as_str)
            .map_or_else(|| "artifact:unknown".to_owned(), str::to_owned),
        "transition" => {
            let object_type = obj
                .get("objectType")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let property = obj
                .get("property")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let transition = obj
                .get("transition")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            format!("{object_type}.{property}::{transition}")
        }
        _ => "writer:unknown".to_owned(),
    }
}

fn describe_goal(kind: &str, subject: &str, target: &str) -> String {
    match kind {
        "initialization" => format!("admitted initial states of {subject} satisfy the law"),
        "preservation" => format!("writer {target} preserves {subject}"),
        "non_vacuity" => format!("{subject} has at least one reachable satisfying state"),
        "law_semantics" => format!("qualified interpretation of {subject} under semantics profile"),
        "open_boundary" => format!("runtime entrance {target} preserves {subject}"),
        "translation_preserves" => {
            format!("{subject} preserves {target} across translation")
        }
        "lifecycle_terminal_reachability" => {
            format!("lifecycle {subject} can reach {target}")
        }
        "lifecycle_no_stuck_non_terminal" => {
            format!("non-terminal states of lifecycle {subject} retain a valid exit")
        }
        "lifecycle_transition_non_vacuity" => {
            format!("lifecycle {subject} can fire {target}")
        }
        _ => format!("{kind} obligation for {subject} ({target})"),
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use serde_json::Value;

    use super::{OBLIGATIONS_SUMMARY_FORMAT, analyze_obligations_json};

    type TestResult = Result<(), Box<dyn Error>>;

    fn law_ir_bundle_json() -> Result<String, Box<dyn Error>> {
        let mut items: Vec<Value> =
            serde_json::from_str(include_str!("../../../fixtures/law-ir/entities.json"))?;
        let laws: Vec<Value> =
            serde_json::from_str(include_str!("../../../fixtures/law-ir/laws.json"))?;
        items.extend(laws);
        Ok(serde_json::to_string(&items)?)
    }

    #[test]
    fn generates_honest_unproved_summary_for_admitted_laws_and_lifecycles() -> TestResult {
        let bundle = law_ir_bundle_json()?;
        let out = analyze_obligations_json(&bundle, "{}")?;
        let summary: Value = serde_json::from_str(&out)?;

        assert_eq!(
            summary.get("format").and_then(Value::as_str),
            Some(OBLIGATIONS_SUMMARY_FORMAT)
        );
        assert_eq!(
            summary.get("generatorProfile").and_then(Value::as_str),
            Some(norn_checker::obligations::GENERATOR_PROFILE)
        );
        assert_eq!(
            summary.get("status").and_then(Value::as_str),
            Some("generated_unproved")
        );
        let total = summary
            .get("totalGoals")
            .and_then(Value::as_u64)
            .ok_or("missing totalGoals")?;
        assert!(total > 0, "expected generated obligations, got: {summary}");
        let by_kind = summary
            .get("byKind")
            .and_then(Value::as_object)
            .ok_or("missing byKind map")?;
        assert!(
            by_kind
                .get("initialization")
                .and_then(Value::as_u64)
                .is_some_and(|n| n > 0),
            "expected initialization obligations in {by_kind:?}"
        );
        assert!(
            by_kind
                .get("preservation")
                .and_then(Value::as_u64)
                .is_some_and(|n| n > 0),
            "expected preservation obligations in {by_kind:?}"
        );
        let goals = summary
            .get("goals")
            .and_then(Value::as_array)
            .ok_or("missing goals array")?;
        let first = goals.first().ok_or("expected first goal")?;
        assert!(first.get("id").and_then(Value::as_str).is_some());
        assert!(first.get("kind").and_then(Value::as_str).is_some());
        assert!(first.get("subject").and_then(Value::as_str).is_some());
        assert!(first.get("target").and_then(Value::as_str).is_some());
        assert!(first.get("description").and_then(Value::as_str).is_some());
        assert_eq!(
            summary.get("truncated").and_then(Value::as_bool),
            Some(total > 64)
        );
        Ok(())
    }

    #[test]
    fn reports_unavailable_for_unadmitted_set_without_claiming_proved() -> TestResult {
        let out = analyze_obligations_json(r#"[{"invalid":true}]"#, "{}")?;
        let summary: Value = serde_json::from_str(&out)?;
        assert_eq!(
            summary.get("status").and_then(Value::as_str),
            Some("unavailable")
        );
        assert!(
            summary
                .get("reason")
                .and_then(Value::as_str)
                .is_some_and(|reason| !reason.is_empty())
        );
        assert_eq!(summary.get("totalGoals").and_then(Value::as_u64), Some(0));
        assert_eq!(
            summary.get("goals").and_then(Value::as_array),
            Some(&Vec::new())
        );
        Ok(())
    }

    #[test]
    fn refuses_malformed_json_inputs_as_errors() {
        assert!(analyze_obligations_json("{not json", "{}").is_err());
        assert!(analyze_obligations_json("{}", "{}").is_err());
        assert!(analyze_obligations_json("[]", "{not json").is_err());
    }
}
