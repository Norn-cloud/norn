//! Native entrypoint for the same checker ABI used by typed TypeScript authoring.
use std::io::{BufRead as _, Write as _};
mod eval;
mod mcp;
mod remote;
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
    /// Serve available human operations over bounded MCP stdio.
    Mcp,
    /// Discover and invoke a remote server's live MCP catalog.
    Remote(remote::RemoteArgs),
    /// List the available human operations and their input schemas.
    #[command(
        long_about = "List the available human operations and their input schemas. Returns tools and a catalogDigest, with at most 32 tools per page. Follow nextCursor using --cursor until it is absent. A cursor is bound to this working directory and catalog; restart without it after a revision. Only installed backends appear; discovery never installs definitions."
    )]
    Capabilities {
        /// Opaque nextCursor from the preceding catalog page; omit to start discovery.
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Check a JSON artifact array using the shared checker and resource limits.
    #[command(
        long_about = "Check a JSON artifact array using the shared checker and resource limits. Read artifacts and context from local files. Author ordinary typed source, compile it with the authoring CLI, then check its output. A refused report retains diagnostic codes, source locations, remediation and receipt/profile pins. Edit the named source and compile/check again; checking never installs definitions."
    )]
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
    /// Print the HOST-TREE-1 source-tree digest of a directory.
    #[command(
        long_about = "Print the HOST-TREE-1 source-tree digest of a directory (FUNCTION-HOST-1's implementation.digest for a built-in crate). Every regular file below the directory counts, except entries whose name starts with '.' and directories named 'target'. Symlinks and non-UTF-8 names refuse. Run it on a clean checkout."
    )]
    SourceDigest {
        /// The source tree's root directory (for a built-in, its crate directory).
        dir: PathBuf,
    },
}

fn run(command: Command) -> Result<(String, bool), String> {
    match command {
        Command::Mcp => mcp::serve(),
        Command::Capabilities { cursor } => mcp::capabilities(cursor.as_deref()),
        Command::Remote(args) => remote::execute(&args).map(|(text, failed)| (text, !failed)),
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
        Command::SourceDigest { dir } => source_digest(&dir).map(|document| (document, true)),
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

/// HOST-TREE-1: read every counted file below `root` and digest the tree.
fn source_digest(root: &std::path::Path) -> Result<String, String> {
    fn walk(
        root: &std::path::Path,
        dir: &std::path::Path,
        files: &mut Vec<(String, Vec<u8>)>,
    ) -> Result<(), String> {
        let mut entries = std::fs::read_dir(dir)
            .map_err(|error| format!("{}: {error}", dir.display()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| error.to_string())?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let name = entry.file_name();
            let name = name
                .to_str()
                .ok_or_else(|| format!("non-UTF-8 name under {}", dir.display()))?;
            let kind = entry.file_type().map_err(|error| error.to_string())?;
            if name.starts_with('.') || (kind.is_dir() && name == "target") {
                continue;
            }
            let path = entry.path();
            if kind.is_symlink() {
                return Err(format!("{}: symlinks are refused", path.display()));
            }
            if kind.is_dir() {
                walk(root, &path, files)?;
            } else if kind.is_file() {
                let relative = path
                    .strip_prefix(root)
                    .map_err(|error| error.to_string())?
                    .components()
                    .map(|part| {
                        part.as_os_str()
                            .to_str()
                            .ok_or_else(|| format!("{}: non-UTF-8 name", path.display()))
                    })
                    .collect::<Result<Vec<_>, _>>()?
                    .join("/");
                let bytes =
                    std::fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))?;
                files.push((relative, bytes));
            } else {
                return Err(format!("{}: not a regular file", path.display()));
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    walk(root, root, &mut files)?;
    let digest = norn_ir::source_tree::source_tree_digest(
        files
            .iter()
            .map(|(path, bytes)| (path.as_str(), bytes.as_slice())),
    )
    .map_err(|error| error.to_string())?;
    Ok(serde_json::json!({ "digest": digest.0, "files": files.len() }).to_string())
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
