//! Diagnostic code lookup, `--base` context merging, and honest live law rendering for `norn explain`.

use norn_ir::explain::{ExplainProof, ExplainedLaw};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Typed per-law runtime enforcement and proof record from `norn_checker_wasm::law_honesty_json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LawHonestyEntry {
    pub(crate) artifact_id: String,
    pub(crate) law_id: String,
    pub(crate) enforcement: String,
    pub(crate) proof: String,
    pub(crate) evidence_digest: Option<String>,
    pub(crate) reason: Option<String>,
}

impl LawHonestyEntry {
    fn from_json_value(value: &Value) -> Result<Self, String> {
        let field = |key: &str| {
            value
                .get(key)
                .and_then(Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| format!("law honesty entry missing string field `{key}`"))
        };
        let optional_field = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned);
        Ok(Self {
            artifact_id: field("artifactId")?,
            law_id: field("lawId")?,
            enforcement: field("enforcement")?,
            proof: field("proof")?,
            evidence_digest: optional_field("evidenceDigest"),
            reason: optional_field("reason"),
        })
    }
}

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

fn parse_strict_context_object(path: &Path, raw: &str) -> Result<Map<String, Value>, String> {
    if raw.trim().is_empty() {
        return Err(format!(
            "{}: explain --context must be a JSON object",
            path.display()
        ));
    }
    let parsed: Value =
        serde_json::from_str(raw).map_err(|error| format!("{}: {error}", path.display()))?;
    let Value::Object(map) = parsed else {
        return Err("explain --context must be a JSON object".to_owned());
    };
    if let Some(base_val) = map.get("base")
        && !base_val.is_array()
    {
        return Err("explain context.base must be a JSON array".to_owned());
    }
    Ok(map)
}

/// Builds the explain checker context JSON by merging optional `--context` with repeatable `--base` inputs.
pub(crate) fn build_context_json(
    context_path: Option<&Path>,
    base_paths: &[PathBuf],
    read_file: fn(&Path) -> Result<String, String>,
    read_defs: fn(&Path) -> Result<String, String>,
) -> Result<String, String> {
    if base_paths.is_empty() {
        return match context_path {
            Some(path) => {
                let raw = read_file(path)?;
                let _validated = parse_strict_context_object(path, &raw)?;
                Ok(raw)
            }
            None => Ok("{}".to_owned()),
        };
    }
    let mut context_obj = match context_path {
        Some(path) => {
            let raw = read_file(path)?;
            parse_strict_context_object(path, &raw)?
        }
        None => Map::new(),
    };
    let mut base_items = match context_obj.remove("base") {
        None => Vec::new(),
        Some(Value::Array(items)) => items,
        Some(
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Object(_),
        ) => return Err("explain context.base must be a JSON array".to_owned()),
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
    serde_json::to_string(&Value::Object(context_obj)).map_err(|error| error.to_string())
}

fn classify_honesty_into(
    artifacts_json: &str,
    map: &mut BTreeMap<(String, String), LawHonestyEntry>,
) -> Result<(), String> {
    let raw =
        norn_checker_wasm::law_honesty_json(artifacts_json).map_err(|error| error.to_string())?;
    let parsed: Value = serde_json::from_str(&raw).map_err(|error| error.to_string())?;
    let Value::Array(items) = parsed else {
        return Err("law honesty response must be a JSON array".to_owned());
    };
    for item in &items {
        let entry = LawHonestyEntry::from_json_value(item)?;
        map.insert((entry.artifact_id.clone(), entry.law_id.clone()), entry);
    }
    Ok(())
}

/// Computes live `LAW-HONESTY-1` classifications for explained laws from the checker ABI.
pub(crate) fn load_law_honesty(
    artifacts_json: &str,
    context_json: &str,
) -> Result<BTreeMap<(String, String), LawHonestyEntry>, String> {
    let mut by_key = BTreeMap::new();
    if let Ok(Value::Object(ctx)) = serde_json::from_str::<Value>(context_json)
        && let Some(Value::Array(base_items)) = ctx.get("base")
        && !base_items.is_empty()
    {
        let base_json = serde_json::to_string(base_items).map_err(|error| error.to_string())?;
        classify_honesty_into(&base_json, &mut by_key)?;
    }
    classify_honesty_into(artifacts_json, &mut by_key)?;
    Ok(by_key)
}

/// Renders one explained law using the checker's honest live enforcement and proof status.
pub(crate) fn render_law_entry(
    lines: &mut Vec<String>,
    law: &ExplainedLaw,
    honesty: Option<&LawHonestyEntry>,
) {
    let fallback_proof = match law.proof {
        ExplainProof::Unknown => "unknown",
    };
    let proof_status = honesty
        .and_then(|entry| (!entry.proof.is_empty()).then_some(entry.proof.as_str()))
        .unwrap_or(fallback_proof);
    let enforcement = honesty
        .and_then(|entry| (!entry.enforcement.is_empty()).then_some(entry.enforcement.as_str()))
        .unwrap_or("unknown");
    let binding = if law.enforcement.is_some() {
        "declared invariant"
    } else {
        "not established"
    };
    lines.push(format!(
        "Law {}: proof {proof_status}; enforcement {enforcement}; binding {binding}",
        law.law.id
    ));
    if let Some(entry) = honesty {
        if let Some(reason) = entry.reason.as_deref().filter(|text| !text.is_empty()) {
            lines.push(format!("  enforcement reason: {reason}"));
        }
        if let Some(digest) = entry
            .evidence_digest
            .as_deref()
            .filter(|text| !text.is_empty())
        {
            lines.push(format!("  evidence digest: {digest}"));
        }
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
