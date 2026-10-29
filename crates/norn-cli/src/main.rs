//! Native entrypoint for the same checker ABI used by typed TypeScript authoring.
use std::io::{BufRead as _, Write as _};
mod eval;
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Debug, Parser)]
#[command(name = "norn", version, about = "Norn core checker")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Check a JSON artifact array using the shared checker and resource limits.
    Check {
        /// JSON array emitted by typed authoring.
        #[arg(long)]
        artifacts: PathBuf,
        /// Checker context JSON, containing the explicit tenant scope.
        #[arg(long)]
        context: PathBuf,
    },
    /// Evaluate an admitted standalone decision over JSON-lines stdin.
    Eval {
        /// Directory of definition envelopes.
        #[arg(long)]
        defs: PathBuf,
        /// Organization (the definitions must carry their own matching scope).
        #[arg(long)]
        org: String,
        /// Environment.
        #[arg(long)]
        env: String,
        /// Exact rule artifact id.
        #[arg(long)]
        rule: String,
        /// Deterministic operation budget per input.
        #[arg(long, default_value_t = 1_000_000)]
        units: u64,
    },
    /// Print generated core IR and checker `OpenAPI` components.
    Openapi,
}

fn run(command: Command) -> Result<(String, bool), String> {
    match command {
        Command::Openapi => norn_api::openapi::openapi_json()
            .map(|document| (document, true))
            .map_err(|error| error.to_string()),
        Command::Eval {
            defs,
            org,
            env,
            rule,
            units,
        } => {
            let mut paths = std::fs::read_dir(defs)
                .map_err(|error| error.to_string())?
                .map(|entry| entry.map(|entry| entry.path()))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| error.to_string())?;
            paths.sort();
            let mut artifacts = Vec::new();
            let mut documents = Vec::new();
            let mut exact_payloads = std::collections::BTreeMap::new();
            for path in paths.into_iter().filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "json")
            }) {
                let text = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
                let artifact: norn_ir::ArtifactEnvelope =
                    serde_json::from_str(&text).map_err(|error| error.to_string())?;
                if artifact.scope.org_id.as_deref() != Some(org.as_str())
                    || artifact.scope.environment_id.as_deref() != Some(env.as_str())
                {
                    return Err("definition scope does not match --org and --env".to_owned());
                }
                let exact = norn_ir::RuntimeValue::from_json_text(&text)
                    .map_err(|error| error.to_string())?;
                if let Some(payload) = exact.as_object().and_then(|object| object.get("payload")) {
                    exact_payloads.insert(artifact.artifact_id.clone(), payload.clone());
                }
                artifacts.push(artifact);
                documents.push(text);
            }
            let input = format!("[{}]", documents.join(","));
            let report = norn_checker_wasm::check_artifact_set_json(&input, "{}")
                .map_err(|error| error.to_string())?;
            let report: serde_json::Value =
                serde_json::from_str(&report).map_err(|error| error.to_string())?;
            if report.get("admitted").and_then(serde_json::Value::as_bool) != Some(true) {
                return Err(report.to_string());
            }
            let receipt = report.get("receipt").ok_or("missing checker receipt")?;
            let digest = report
                .get("receiptSha256")
                .and_then(serde_json::Value::as_str)
                .ok_or("missing checker digest")?;
            let decision = eval::OfflineDecision::prepare(
                &artifacts,
                &exact_payloads,
                receipt,
                digest,
                &rule,
            )?;
            for line in std::io::stdin().lock().lines() {
                let line = line.map_err(|error| error.to_string())?;
                let response = match decision.run(&line, units) {
                    Ok(output) => format!("{{\"ok\":true,\"output\":{output}}}"),
                    Err(error) => serde_json::json!({"ok":false,"error":error}).to_string(),
                };
                writeln!(std::io::stdout().lock(), "{response}")
                    .map_err(|error| error.to_string())?;
            }
            Ok((String::new(), true))
        }
        Command::Check { artifacts, context } => {
            let artifacts =
                std::fs::read_to_string(artifacts).map_err(|error| error.to_string())?;
            let context = std::fs::read_to_string(context).map_err(|error| error.to_string())?;
            let report = norn_checker_wasm::check_artifact_set_json(&artifacts, &context)
                .map_err(|error| error.to_string())?;
            let value: serde_json::Value =
                serde_json::from_str(&report).map_err(|error| error.to_string())?;
            Ok((
                report,
                value.get("admitted").and_then(serde_json::Value::as_bool) == Some(true),
            ))
        }
    }
}

fn main() -> ExitCode {
    match run(Cli::parse().command) {
        Ok((document, accepted)) => {
            if (!document.is_empty() && writeln!(std::io::stdout().lock(), "{document}").is_err())
                || !accepted
            {
                ExitCode::FAILURE
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(error) => {
            let _written = writeln!(std::io::stderr().lock(), "norn: {error}");
            ExitCode::FAILURE
        }
    }
}
