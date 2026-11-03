//! Native presentation of the shared checker explanation ABI.
use clap::Args;
use norn_ir::explain::{ExplainOptions, ExplainReport};
use std::path::{Path, PathBuf};

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
    /// JSON array of typed, reviewed `StructuredRequirement` values.
    #[arg(long, requires = "locale")]
    wording: Option<PathBuf>,
}

fn read(path: &Path) -> Result<String, String> {
    if std::fs::metadata(path)
        .map_err(|error| error.to_string())?
        .len()
        > 8_388_608
    {
        return Err(format!(
            "{} exceeds the 8 MiB explain input limit",
            path.display()
        ));
    }
    std::fs::read_to_string(path).map_err(|error| format!("{}: {error}", path.display()))
}

fn definitions(path: &Path) -> Result<String, String> {
    if path.is_file() {
        return read(path);
    }
    let mut paths = std::fs::read_dir(path)
        .map_err(|error| error.to_string())?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    paths.sort();
    let mut documents = Vec::new();
    let mut size = 0_usize;
    for path in paths.iter().filter(|path| {
        path.extension()
            .is_some_and(|extension| extension == "json")
    }) {
        let text = read(path)?;
        size = size.saturating_add(text.len());
        if size > 8_388_608 {
            return Err("definition set exceeds the 8 MiB explain input limit".into());
        }
        documents.push(text);
    }
    Ok(format!("[{}]", documents.join(",")))
}

/// The JSON and text forms share one report; unknown proof status is explicit.
pub(crate) fn execute(args: &ExplainArgs) -> Result<(String, bool), String> {
    let artifacts = definitions(&args.defs)?;
    let context = args
        .context
        .as_deref()
        .map(read)
        .transpose()?
        .unwrap_or_else(|| "{}".into());
    let options = ExplainOptions {
        locale: args.locale.clone(),
        requirements: args
            .wording
            .as_deref()
            .map(read)
            .transpose()?
            .map(|text| serde_json::from_str(&text))
            .transpose()
            .map_err(|error| error.to_string())?
            .unwrap_or_default(),
    };
    let options = serde_json::to_string(&options).map_err(|error| error.to_string())?;
    let output = norn_checker_wasm::explain_artifact_set_json(
        &artifacts, &context, &args.kind, &args.id, &options,
    )
    .map_err(|error| error.to_string())?;
    let value: serde_json::Value =
        serde_json::from_str(&output).map_err(|error| error.to_string())?;
    let admitted = value.get("admitted").and_then(serde_json::Value::as_bool) == Some(true);
    if args.format == "json" {
        return Ok((output, admitted));
    }
    if !admitted {
        return Ok((diagnostic_text(&value), false));
    }
    let report: ExplainReport = serde_json::from_value(
        value
            .get("report")
            .cloned()
            .ok_or("missing explain report")?,
    )
    .map_err(|error| error.to_string())?;
    Ok((render(&report), true))
}

fn diagnostic_text(value: &serde_json::Value) -> String {
    let Some(diagnostics) = value
        .get("diagnostics")
        .and_then(serde_json::Value::as_array)
    else {
        return "explanation refused".into();
    };
    diagnostics
        .iter()
        .map(|diagnostic| {
            let text = |key| {
                diagnostic
                    .get(key)
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
            };
            let location = diagnostic
                .pointer("/span/pointer")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            format!(
                "{}: {}\n  at {location}\n  {}{}",
                text("code"),
                text("message"),
                text("fix"),
                text("no_fix")
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn render(report: &ExplainReport) -> String {
    let mut lines = vec![
        format!(
            "{}@{} {}",
            report.target.artifact_id,
            report.target.version,
            report.target.member.id()
        ),
        "Closure: conservative whole-set invalidation inventory".into(),
    ];
    for declaration in &report.closure.artifacts {
        lines.push(format!(
            "  {}@{} {}",
            declaration.artifact_id, declaration.version, declaration.sha256
        ));
    }
    lines.push(format!(
        "Scope: {}",
        serde_json::to_value(&report.scope)
            .map_or_else(|error| error.to_string(), |value| value.to_string())
    ));
    for writer in &report.writers {
        lines.push(format!("Writer: {}", writer.to_json()));
    }
    for read in &report.reads {
        lines.push(format!(
            "Read: {}",
            serde_json::to_value(read)
                .map_or_else(|error| error.to_string(), |value| value.to_string())
        ));
    }
    lines.push(format!(
        "Reads: {} declared slice entries; direct effects: {}",
        report.reads.len(),
        report.direct_effects.len()
    ));
    for effect in &report.direct_effects {
        lines.push(format!(
            "  {} -> {}",
            effect.writer.member.id(),
            effect.target.member.id()
        ));
    }
    for interpretation in &report.interpretations {
        lines.push(format!("Interpretation {}@{}: {}@{} -> {}@{}; law transport unknown", interpretation.declaration.artifact_id, interpretation.declaration.version, interpretation.link.from_pack.pack_id, interpretation.link.from_pack.version, interpretation.link.to_pack.pack_id, interpretation.link.to_pack.version));
        for edge in &interpretation.link.correspondences { lines.push(format!("  {} -> {}", edge.from_term_id, edge.to_term_ids.join(", "))); }
    }
    for law in &report.laws { render_law(&mut lines, law); }
    lines.join("\n")
}

fn render_law(lines: &mut Vec<String>, law: &norn_ir::explain::ExplainedLaw) {
    lines.push(format!(
        "Law {}: proof unknown; enforcement {}",
        law.law.id,
        if law.enforcement.is_some() {
            "declared invariant at commit"
        } else {
            "not established"
        }
    ));
    if let Some(source) = &law.source_path {
        lines.push(format!("  authored source: {source}"));
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

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "CLI conformance fixtures"
)]
mod tests {
    use super::*;
    use clap::Parser;
    #[test]
    fn real_command_parses_and_projects_the_shared_abi_in_both_formats() {
        let mut pool: Vec<serde_json::Value> =
            serde_json::from_str(include_str!("../../../fixtures/law-ir/entities.json")).unwrap();
        pool.extend(
            serde_json::from_str::<Vec<serde_json::Value>>(include_str!(
                "../../../fixtures/law-ir/laws.json"
            ))
            .unwrap(),
        );
        let temp =
            std::env::temp_dir().join(format!("norn-explain-cli-{}.json", std::process::id()));
        std::fs::write(&temp, serde_json::to_string(&pool).unwrap()).unwrap();
        for format in ["json", "text"] {
            let cli = crate::Cli::try_parse_from([
                "norn",
                "explain",
                "law",
                "erpnext.no_unpaid_ship",
                "--defs",
                temp.to_str().unwrap(),
                "--format",
                format,
            ])
            .unwrap();
            let crate::Command::Explain(args) = cli.command else {
                panic!("wrong command");
            };
            let (text, admitted) = execute(&args).unwrap();
            assert!(admitted);
            if format == "json" {
                let response: serde_json::Value = serde_json::from_str(&text).unwrap();
                assert_eq!(response["format"], "norn/explain@2");
                assert_eq!(response["report"]["laws"][0]["proof"], "unknown");
            } else {
                assert!(text.contains("proof unknown"));
                assert!(text.contains("preservation"));
                assert!(text.contains("fixtures/law-ir/laws.source.ts"));
            }
        }
        std::fs::remove_file(temp).unwrap();
    }
    #[test]
    fn locale_argument_accepts_tags_outside_en_ar() {
        let cli = crate::Cli::try_parse_from([
            "norn",
            "explain",
            "law",
            "demo.law",
            "--defs",
            "defs.json",
            "--wording",
            "wording.json",
            "--locale",
            "fr-CA",
        ])
        .unwrap();
        let crate::Command::Explain(args) = cli.command else {
            panic!("wrong command");
        };
        assert_eq!(args.locale.as_deref(), Some("fr-CA"));
    }
}
