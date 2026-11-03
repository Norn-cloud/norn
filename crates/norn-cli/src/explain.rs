//! Native presentation of the shared checker explanation ABI.
use clap::Args;
use std::path::{Path, PathBuf};
use norn_ir::explain::{ExplainOptions, ExplainReport};

#[derive(Debug, Args)]
pub(crate) struct ExplainArgs {
    #[arg(value_parser = ["action", "law", "concept"])]
    kind: String,
    id: String,
    /// A JSON artifact array or directory of artifact envelopes.
    #[arg(long)]
    defs: PathBuf,
    /// Installed base, registry and tenant checker context.
    #[arg(long)]
    context: Option<PathBuf>,
    #[arg(long, value_parser = ["text", "json"], default_value = "text")]
    format: String,
    /// BCP 47 tag resolved by the supplied reviewed wording map.
    #[arg(long, requires = "wording")]
    locale: Option<String>,
    /// JSON array of typed, reviewed StructuredRequirement values.
    #[arg(long, requires = "locale")]
    wording: Option<PathBuf>,
}

fn read(path: &Path) -> Result<String, String> {
    if std::fs::metadata(path).map_err(|error| error.to_string())?.len() > 8_388_608 {
        return Err(format!("{} exceeds the 8 MiB explain input limit", path.display()));
    }
    std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))
}

fn definitions(path: &Path) -> Result<String, String> {
    if path.is_file() { return read(path); }
    let mut paths = std::fs::read_dir(path).map_err(|error| error.to_string())?
        .map(|entry| entry.map(|entry| entry.path())).collect::<Result<Vec<_>, _>>().map_err(|error| error.to_string())?;
    paths.sort();
    let mut documents = Vec::new();
    let mut size = 0_usize;
    for path in paths.iter().filter(|path| path.extension().is_some_and(|extension| extension == "json")) {
        let text = read(path)?;
        size = size.saturating_add(text.len());
        if size > 8_388_608 { return Err("definition set exceeds the 8 MiB explain input limit".into()); }
        documents.push(text);
    }
    Ok(format!("[{}]", documents.join(",")))
}

/// The JSON and text forms share one report; unknown proof status is explicit.
pub(crate) fn execute(args: &ExplainArgs) -> Result<(String, bool), String> {
    let artifacts = definitions(&args.defs)?;
    let context = args.context.as_deref().map(read).transpose()?.unwrap_or_else(|| "{}".into());
    let options = ExplainOptions {
        locale: args.locale.clone(),
        requirements: args.wording.as_deref().map(read).transpose()?.map(|text| serde_json::from_str(&text)).transpose().map_err(|error| error.to_string())?.unwrap_or_default(),
    };
    let options = serde_json::to_string(&options).map_err(|error| error.to_string())?;
    let output = norn_checker_wasm::explain_artifact_set_json(&artifacts, &context, &args.kind, &args.id, &options).map_err(|error| error.to_string())?;
    let value: serde_json::Value = serde_json::from_str(&output).map_err(|error| error.to_string())?;
    let admitted = value.get("admitted").and_then(serde_json::Value::as_bool) == Some(true);
    if args.format == "json" { return Ok((output, admitted)); }
    if !admitted { return Ok((diagnostic_text(&value), false)); }
    let report: ExplainReport = serde_json::from_value(value.get("report").cloned().ok_or("missing explain report")?).map_err(|error| error.to_string())?;
    Ok((render(&report), true))
}

fn diagnostic_text(value: &serde_json::Value) -> String {
    let Some(diagnostics) = value.get("diagnostics").and_then(serde_json::Value::as_array) else { return "explanation refused".into(); };
    diagnostics.iter().map(|diagnostic| {
        let text = |key| diagnostic.get(key).and_then(serde_json::Value::as_str).unwrap_or("");
        let location = diagnostic.pointer("/span/pointer").and_then(serde_json::Value::as_str).unwrap_or("");
        format!("{}: {}\n  at {location}\n  {}{}", text("code"), text("message"), text("fix"), text("no_fix"))
    }).collect::<Vec<_>>().join("\n")
}

fn render(report: &ExplainReport) -> String {
    let mut lines = vec![format!("{}@{} {}", report.target.artifact_id, report.target.version, report.target.member.id()),
        "Closure: conservative whole-set invalidation inventory".into()];
    for declaration in &report.closure.artifacts { lines.push(format!("  {}@{} {}", declaration.artifact_id, declaration.version, declaration.sha256)); }
    lines.push(format!("Reads: {} declared slice entries; direct effects: {}", report.reads.len(), report.direct_effects.len()));
    for effect in &report.direct_effects { lines.push(format!("  {} -> {}", effect.writer.member.id(), effect.target.member.id())); }
    for law in &report.laws {
        lines.push(format!("Law {}: proof unknown; enforcement {}", law.law.id, if law.enforcement.is_some() { "declared invariant at commit" } else { "not established" }));
        if let Some(source) = &law.source_path { lines.push(format!("  authored source: {source}")); }
        for path in &law.inheritance_paths { lines.push(format!("  inheritance: {}", path.iter().map(|at| at.member.id()).collect::<Vec<_>>().join(" -> "))); }
        for path in &law.containment_paths { lines.push(format!("  containment: {}", path.iter().map(|at| format!("{}@{}", at.artifact_id, at.version)).collect::<Vec<_>>().join(" -> "))); }
        if let Some(wording) = &law.wording { for slot in &wording.slots { lines.push(format!("  {}: {}", slot.label, slot.text)); } }
        for unknown in &law.why_unknown { lines.push(format!("  unknown {} {}: {} Fix: {}", unknown.writer.as_ref().map_or_else(String::new, |writer| writer.to_json().to_string()), unknown.footprint, unknown.reason, unknown.fix)); }
    }
    lines.join("\n")
}
