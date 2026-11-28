//! Native presentation of the shared checker explanation ABI.
use clap::Args;
use norn_ir::explain::{ExplainOptions, ExplainReport};
use std::path::{Path, PathBuf};

#[derive(Debug, Args)]
pub(crate) struct ExplainArgs {
    /// Subject kind (`action`, `law`, `concept`, `diagnostic`) or a dotted diagnostic code.
    pub(crate) kind: String,
    /// Subject identifier or diagnostic code (optional when `kind` is a dotted diagnostic code).
    pub(crate) id: Option<String>,
    /// A JSON artifact array or directory of artifact envelopes.
    #[arg(long)]
    pub(crate) defs: Option<PathBuf>,
    /// Installed base layer artifact JSON file or directory (repeatable).
    #[arg(long)]
    pub(crate) base: Vec<PathBuf>,
    /// Installed base, registry and tenant checker context.
    #[arg(long)]
    pub(crate) context: Option<PathBuf>,
    #[arg(long, value_parser = ["text", "json"], default_value = "text")]
    pub(crate) format: String,
    /// BCP 47 tag resolved by the supplied reviewed wording map.
    #[arg(long, requires = "wording")]
    pub(crate) locale: Option<String>,
    /// JSON array of typed, reviewed `StructuredRequirement` values.
    #[arg(long, requires = "locale")]
    pub(crate) wording: Option<PathBuf>,
}

const EXPLAIN_LIMIT: u64 = 8_388_608;

fn read(path: &Path) -> Result<String, String> {
    crate::cell::read_regular_file_bounded(path, EXPLAIN_LIMIT, "8 MiB explain input limit")
}

fn definitions(path: &Path) -> Result<String, String> {
    crate::cell::read_file_or_json_dir_bounded(path, EXPLAIN_LIMIT, "8 MiB explain input limit")
}

/// The JSON and text forms share one report; unknown proof status is explicit.
pub(crate) fn execute(args: &ExplainArgs) -> Result<(String, bool), String> {
    if args.kind == "diagnostic" {
        let code = args.id.as_deref().ok_or_else(|| {
            "missing diagnostic code: usage `norn explain diagnostic <CODE>`".to_owned()
        })?;
        return crate::explain_support::execute_diagnostic(code, &args.format);
    }
    if args.id.is_none() && crate::explain_support::looks_like_diagnostic_code(&args.kind) {
        return crate::explain_support::execute_diagnostic(&args.kind, &args.format);
    }
    if !matches!(args.kind.as_str(), "action" | "law" | "concept") {
        return Err(format!(
            "invalid explain kind `{}`: expected action, law, concept, diagnostic, or a dotted diagnostic code",
            args.kind
        ));
    }
    let id = args.id.as_deref().ok_or_else(|| {
        format!(
            "missing target id: usage `norn explain {} <ID> --defs <PATH>`",
            args.kind
        )
    })?;
    let defs_path = args
        .defs
        .as_deref()
        .ok_or_else(|| format!("--defs <PATH> is required for `norn explain {}`", args.kind))?;
    let artifacts = definitions(defs_path)?;
    let context = crate::explain_support::build_context_json(
        args.context.as_deref(),
        &args.base,
        read,
        definitions,
    )?;
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
        &artifacts, &context, &args.kind, id, &options,
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
    let raw_report = value.get("report").ok_or("missing explain report")?;
    let report: ExplainReport =
        serde_json::from_value(raw_report.clone()).map_err(|error| error.to_string())?;
    Ok((render(&report, raw_report), true))
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
            // A finding has a location only when it has a span, and exactly
            // one of a fix or the reason there is none.
            let location = diagnostic
                .pointer("/span/pointer")
                .and_then(serde_json::Value::as_str)
                .map_or_else(String::new, |pointer| format!("\n  at {pointer}"));
            let witness = diagnostic
                .get("witness")
                .filter(|value| !value.is_null())
                .map_or_else(String::new, |value| {
                    format!("\n  witness: {}", crate::cell_render::format_witness(value))
                });
            let advice = if text("fix").is_empty() {
                format!("no automatic fix: {}", text("no_fix"))
            } else {
                format!("fix: {}", text("fix"))
            };
            format!(
                "{}: {}{location}{witness}\n  {advice}",
                text("code"),
                text("message")
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn render(report: &ExplainReport, raw_report: &serde_json::Value) -> String {
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
        serde_json::to_string(&report.scope).unwrap_or_else(|error| error.to_string())
    ));
    for writer in &report.writers {
        lines.push(format!("Writer: {}", writer.to_json()));
    }
    for read in &report.reads {
        lines.push(format!(
            "Read: {}",
            serde_json::to_string(read).unwrap_or_else(|error| error.to_string())
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
        lines.push(format!(
            "Interpretation {}@{}: {}@{} -> {}@{}; law transport unknown",
            interpretation.declaration.artifact_id,
            interpretation.declaration.version,
            interpretation.link.from_pack.pack_id,
            interpretation.link.from_pack.version,
            interpretation.link.to_pack.pack_id,
            interpretation.link.to_pack.version
        ));
        for edge in &interpretation.link.correspondences {
            lines.push(format!(
                "  {} -> {}",
                edge.from_term_id,
                edge.to_term_ids.join(", ")
            ));
        }
    }
    let raw_laws = raw_report.get("laws").and_then(serde_json::Value::as_array);
    for (idx, law) in report.laws.iter().enumerate() {
        let raw_law = raw_laws.and_then(|items| items.get(idx));
        crate::explain_support::render_law_entry(&mut lines, law, raw_law);
    }
    lines.join("\n")
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
                assert_eq!(
                    text.as_bytes(),
                    norn_ir::canonical::canonical_json_of(&response).unwrap()
                );
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

    fn explain_widget_case(tag: &str, widget: serde_json::Value) -> (String, bool) {
        let mut pool: Vec<serde_json::Value> =
            serde_json::from_str(include_str!("../../../fixtures/law-ir/entities.json")).unwrap();
        pool.extend(
            serde_json::from_str::<Vec<serde_json::Value>>(include_str!(
                "../../../fixtures/law-ir/laws.json"
            ))
            .unwrap(),
        );
        pool.push(widget);
        let path = std::env::temp_dir().join(format!(
            "norn-explain-cli-{tag}-widget-{}.json",
            std::process::id()
        ));
        std::fs::write(&path, serde_json::to_string(&pool).unwrap()).unwrap();
        let cli = crate::Cli::try_parse_from([
            "norn",
            "explain",
            "law",
            "erpnext.no_unpaid_ship",
            "--defs",
            path.to_str().unwrap(),
            "--format",
            "json",
        ])
        .unwrap();
        let crate::Command::Explain(args) = cli.command else {
            panic!("wrong command");
        };
        let result = execute(&args).unwrap();
        std::fs::remove_file(&path).unwrap();
        result
    }

    #[test]
    fn explain_command_admits_valid_widget_and_refuses_invalid_widget() {
        let valid_widget = serde_json::json!({
            "irVersion": "0.1",
            "artifactId": "erpnext.widget.valid",
            "kind": "widget",
            "namespace": "erpnext",
            "name": "Valid Widget",
            "version": "0.1.0",
            "lifecycle": "draft",
            "scope": { "orgId": "org_demo", "environmentId": "dev" },
            "dependencies": [],
            "provenance": { "source": "authoring_sdk" },
            "payload": {
                "widgetVersion": "0.1",
                "widgetId": "erpnext.widget.valid",
                "dataDependencies": [],
                "viewStates": [{ "stateId": "ready", "kind": "ready" }],
                "actionBindings": [],
                "permissions": []
            }
        });
        let (valid_out, valid_admitted) = explain_widget_case("valid", valid_widget);
        assert!(valid_admitted, "{valid_out}");

        let invalid_widget = serde_json::json!({
            "irVersion": "0.1",
            "artifactId": "erpnext.widget.invalid",
            "kind": "widget",
            "namespace": "erpnext",
            "name": "Invalid Widget",
            "version": "0.1.0",
            "lifecycle": "draft",
            "scope": { "orgId": "org_demo", "environmentId": "dev" },
            "dependencies": [],
            "provenance": { "source": "authoring_sdk" },
            "payload": {
                "widgetVersion": "0.1",
                "widgetId": "erpnext.widget.invalid",
                "dataDependencies": [{
                    "dependencyId": "edge",
                    "objectKind": "erpnext.SalesOrder",
                    "query": { "kind": "relation_traversal" }
                }],
                "viewStates": [{ "stateId": "ready", "kind": "ready" }],
                "actionBindings": [],
                "permissions": []
            }
        });
        let (invalid_out, invalid_admitted) = explain_widget_case("invalid", invalid_widget);
        assert!(!invalid_admitted, "{invalid_out}");
        let response: serde_json::Value = serde_json::from_str(&invalid_out).unwrap();
        assert!(
            response["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .any(|d| d["code"] == "widget.validation.invalid_payload"),
            "{invalid_out}"
        );
    }

    #[test]
    fn explain_diagnostic_explicit_and_auto_detected_dotted_codes() {
        for argv in [
            vec![
                "norn",
                "explain",
                "diagnostic",
                "core.laws.preservation_violated",
                "--format",
                "text",
            ],
            vec![
                "norn",
                "explain",
                "core.laws.preservation_violated",
                "--format",
                "text",
            ],
        ] {
            let cli = crate::Cli::try_parse_from(argv).unwrap();
            let crate::Command::Explain(args) = cli.command else {
                panic!("wrong command");
            };
            let (text, found) = execute(&args).unwrap();
            assert!(found, "{text}");
            assert!(text.contains("Diagnostic core.laws.preservation_violated"));
            assert!(text.contains("severity: error"));
            assert!(text.contains("tier:"));
            assert!(text.contains("message:"));
            assert!(text.contains("fix:") || text.contains("no_fix:"));
        }

        let cli_json = crate::Cli::try_parse_from([
            "norn",
            "explain",
            "core.laws.preservation_violated",
            "--format",
            "json",
        ])
        .unwrap();
        let crate::Command::Explain(args_json) = cli_json.command else {
            panic!("wrong command");
        };
        let (json_out, found_json) = execute(&args_json).unwrap();
        assert!(found_json);
        let parsed: serde_json::Value = serde_json::from_str(&json_out).unwrap();
        assert_eq!(parsed["found"], true);
        assert_eq!(parsed["code"], "core.laws.preservation_violated");
        assert_eq!(parsed["severity"], "error");

        let cli_missing = crate::Cli::try_parse_from([
            "norn",
            "explain",
            "diagnostic",
            "core.laws.definitely_not_registered",
            "--format",
            "json",
        ])
        .unwrap();
        let crate::Command::Explain(args_missing) = cli_missing.command else {
            panic!("wrong command");
        };
        let (missing_out, found_missing) = execute(&args_missing).unwrap();
        assert!(!found_missing);
        let missing_json: serde_json::Value = serde_json::from_str(&missing_out).unwrap();
        assert_eq!(missing_json["found"], false);
    }

    #[test]
    fn explain_repeatable_base_merges_base_artifacts_without_context_file() {
        let base_path =
            std::env::temp_dir().join(format!("norn-explain-base-{}.json", std::process::id()));
        let defs_path =
            std::env::temp_dir().join(format!("norn-explain-defs-{}.json", std::process::id()));
        std::fs::write(
            &base_path,
            include_str!("../../../fixtures/law-ir/entities.json"),
        )
        .unwrap();
        std::fs::write(
            &defs_path,
            include_str!("../../../fixtures/law-ir/laws.json"),
        )
        .unwrap();

        let cli = crate::Cli::try_parse_from([
            "norn",
            "explain",
            "law",
            "erpnext.no_unpaid_ship",
            "--defs",
            defs_path.to_str().unwrap(),
            "--base",
            base_path.to_str().unwrap(),
            "--format",
            "json",
        ])
        .unwrap();
        let crate::Command::Explain(args) = cli.command else {
            panic!("wrong command");
        };
        let (out, admitted) = execute(&args).unwrap();
        let _rm_base = std::fs::remove_file(&base_path);
        let _rm_defs = std::fs::remove_file(&defs_path);
        assert!(admitted, "expected --base merge to admit: {out}");
        let parsed: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(parsed["admitted"], true);
        assert_eq!(
            parsed["report"]["laws"][0]["law"]["id"],
            "erpnext.no_unpaid_ship"
        );
    }

    #[test]
    fn render_law_uses_actual_proof_status_and_assurance_metadata_from_json() {
        let mut pool: Vec<serde_json::Value> =
            serde_json::from_str(include_str!("../../../fixtures/law-ir/entities.json")).unwrap();
        pool.extend(
            serde_json::from_str::<Vec<serde_json::Value>>(include_str!(
                "../../../fixtures/law-ir/laws.json"
            ))
            .unwrap(),
        );
        let output = norn_checker_wasm::explain_artifact_set_json(
            &serde_json::to_string(&pool).unwrap(),
            "{}",
            "law",
            "erpnext.no_unpaid_ship",
            "{}",
        )
        .unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&output).unwrap();
        let report: ExplainReport = serde_json::from_value(parsed["report"].clone()).unwrap();
        let law = report.laws.first().unwrap();

        let mut enriched_raw = parsed["report"]["laws"][0].clone();
        if let Some(obj) = enriched_raw.as_object_mut() {
            obj.insert("status".to_owned(), serde_json::json!("checked-proof"));
            obj.insert("tier".to_owned(), serde_json::json!("T3"));
            obj.insert(
                "evidence".to_owned(),
                serde_json::json!("sha256:abcdef0123456789"),
            );
            obj.insert(
                "fix".to_owned(),
                serde_json::json!("Maintain preservation certificate on edits."),
            );
        }

        let mut lines = Vec::new();
        crate::explain_support::render_law_entry(&mut lines, law, Some(&enriched_raw));
        let rendered = lines.join("\n");
        assert!(
            rendered.contains("Law erpnext.no_unpaid_ship: proof checked-proof;"),
            "expected actual proof status, got:\n{rendered}"
        );
        assert!(
            !rendered.contains("proof unknown"),
            "must not hardcode proof unknown when status is checked-proof:\n{rendered}"
        );
        assert!(rendered.contains("tier: T3"));
        assert!(rendered.contains("evidence: sha256:abcdef0123456789"));
        assert!(rendered.contains("fix: Maintain preservation certificate on edits."));
    }
}
