//! Diagnostic code lookup, `--base` context merging, and rich law rendering for `norn explain`.

use norn_ir::explain::{ExplainProof, ExplainedLaw};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};

/// Returns `true` when `token` looks like a diagnostic code (dotted checker code or `NORN-*`).
pub(crate) fn looks_like_diagnostic_code(token: &str) -> bool {
    !token.is_empty() && (token.contains('.') || token.starts_with("NORN-"))
}

fn authoring_profile_code(code: &str) -> Option<Value> {
    if !code.starts_with("NORN-") {
        return None;
    }
    let profile = norn_authoring::Profile::load().ok()?;
    let doc = profile.diagnostics.get(code)?;
    Some(json!({
        "format": "norn/explain-diagnostic@1",
        "found": true,
        "code": code,
        "tier": "T0",
        "severity": "error",
        "message": doc.summary,
        "since": profile.profile,
        "fix": doc.fix,
        "no_fix": Value::Null,
    }))
}

fn render_diagnostic_text(value: &Value) -> String {
    let text = |key: &str| value.get(key).and_then(Value::as_str).unwrap_or_default();
    let found = value.get("found").and_then(Value::as_bool) == Some(true);
    let code = text("code");
    if !found {
        return format!("Unknown diagnostic code: {code}");
    }
    let mut lines = vec![
        format!("Diagnostic {code}"),
        format!("  severity: {}", text("severity")),
        format!("  tier: {}", text("tier")),
        format!("  since: {}", text("since")),
        format!("  message: {}", text("message")),
    ];
    let fix = text("fix");
    if !fix.is_empty() {
        lines.push(format!("  fix: {fix}"));
    }
    let no_fix = text("no_fix");
    if !no_fix.is_empty() {
        lines.push(format!("  no_fix: {no_fix}"));
    }
    lines.join("\n")
}

/// Looks up `code` in the checker registry (and `NORN-*` authoring profile) and renders text or JSON.
pub(crate) fn execute_diagnostic(code: &str, format: &str) -> Result<(String, bool), String> {
    let raw = norn_checker_wasm::explain_code_json(code);
    let mut value: Value = serde_json::from_str(&raw).map_err(|error| error.to_string())?;
    let mut found = value.get("found").and_then(Value::as_bool) == Some(true);
    if !found && let Some(profile_doc) = authoring_profile_code(code) {
        value = profile_doc;
        found = true;
    }
    if format == "json" {
        let rendered = serde_json::to_string(&value).map_err(|error| error.to_string())?;
        return Ok((rendered, found));
    }
    Ok((render_diagnostic_text(&value), found))
}

/// Builds the explain checker context JSON by merging optional `--context` with repeatable `--base` inputs.
pub(crate) fn build_context_json(
    context_path: Option<&Path>,
    base_paths: &[PathBuf],
    read_file: fn(&Path) -> Result<String, String>,
    read_defs: fn(&Path) -> Result<String, String>,
) -> Result<String, String> {
    if base_paths.is_empty() {
        return Ok(context_path
            .map(read_file)
            .transpose()?
            .unwrap_or_else(|| "{}".to_owned()));
    }
    let raw_context = context_path
        .map(read_file)
        .transpose()?
        .unwrap_or_else(|| "{}".to_owned());
    let mut context_value: Value =
        serde_json::from_str(&raw_context).map_err(|error| error.to_string())?;
    if context_value.is_null() {
        context_value = Value::Object(Map::new());
    }
    let Some(context_obj) = context_value.as_object_mut() else {
        return Err("explain --context must be a JSON object".to_owned());
    };
    let mut base_items = match context_obj.remove("base") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items,
        Some(_) => return Err("explain context.base must be a JSON array".to_owned()),
    };
    for base_path in base_paths {
        let raw_base = read_defs(base_path)?;
        let parsed: Value = serde_json::from_str(&raw_base)
            .map_err(|error| format!("{}: {error}", base_path.display()))?;
        match parsed {
            Value::Array(items) => base_items.extend(items),
            Value::Object(map) => base_items.push(Value::Object(map)),
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {
                return Err(format!(
                    "{}: --base input must be a JSON artifact object, array, or directory of JSON files",
                    base_path.display()
                ));
            }
        }
    }
    context_obj.insert("base".to_owned(), Value::Array(base_items));
    serde_json::to_string(&context_value).map_err(|error| error.to_string())
}

fn proof_status_from_json(law_proof: ExplainProof, raw_law: Option<&Value>) -> String {
    if let Some(status) = raw_law
        .and_then(|value| value.get("status"))
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
    {
        return status.to_owned();
    }
    if let Some(proof) = raw_law.and_then(|value| value.get("proof")) {
        if let Some(text) = proof.as_str().filter(|text| !text.is_empty()) {
            return text.to_owned();
        }
        if let Some(text) = proof
            .get("status")
            .and_then(Value::as_str)
            .filter(|text| !text.is_empty())
        {
            return text.to_owned();
        }
    }
    match law_proof {
        ExplainProof::Unknown => "unknown".to_owned(),
    }
}

fn format_json_detail(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::Array(_) | Value::Object(_) => {
            value.to_string()
        }
    }
}

fn render_raw_assurance_fields(lines: &mut Vec<String>, raw_law: &Value) {
    if let Some(tier) = raw_law
        .get("tier")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
    {
        lines.push(format!("  tier: {tier}"));
    }
    if let Some(evidence) = raw_law.get("evidence").filter(|value| !value.is_null()) {
        lines.push(format!("  evidence: {}", format_json_detail(evidence)));
    }
    if let Some(fix) = raw_law
        .get("fix")
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
    {
        lines.push(format!("  fix: {fix}"));
    }
}

/// Renders one explained law using the checker's actual proof/assurance status and metadata.
pub(crate) fn render_law_entry(
    lines: &mut Vec<String>,
    law: &ExplainedLaw,
    raw_law: Option<&Value>,
) {
    let proof_status = proof_status_from_json(law.proof, raw_law);
    let enforcement = if law.enforcement.is_some() {
        "declared invariant at commit"
    } else {
        "not established"
    };
    lines.push(format!(
        "Law {}: proof {proof_status}; enforcement {enforcement}",
        law.law.id
    ));
    if let Some(raw) = raw_law {
        render_raw_assurance_fields(lines, raw);
    }
    if let Some(source) = &law.source_path {
        lines.push(format!("  authored source: {source}"));
    }
    for assumption in &law.assumptions {
        lines.push(format!("  assumption: {assumption}"));
    }
    for path in &law.inheritance_paths {
        lines.push(format!(
            "  inheritance: {}",
            path.iter()
                .map(|at| at.member.id())
                .collect::<Vec<_>>()
                .join(" -> ")
        ));
    }
    for path in &law.containment_paths {
        lines.push(format!(
            "  containment: {}",
            path.iter()
                .map(|at| format!("{}@{}", at.artifact_id, at.version))
                .collect::<Vec<_>>()
                .join(" -> ")
        ));
    }
    if let Some(wording) = &law.wording {
        for slot in &wording.slots {
            lines.push(format!("  {}: {}", slot.label, slot.text));
        }
    }
    for unknown in &law.why_unknown {
        lines.push(format!(
            "  unknown {} {}: {} Fix: {}",
            unknown
                .writer
                .as_ref()
                .map_or_else(String::new, |writer| writer.to_json().to_string()),
            unknown.footprint,
            unknown.reason,
            unknown.fix
        ));
    }
}
