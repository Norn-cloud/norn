//! Native entrypoint for the same checker ABI used by typed TypeScript authoring.
use std::io::{BufRead as _, Read as _, Write as _};
mod authoring;
mod cell;
mod cell_render;
mod cell_source;
#[cfg(test)]
mod cli_contract;
mod eval;
mod explain;
mod local;
mod mcp;
mod red_team;
mod remote;
mod source;
use clap::{Args, Parser, Subcommand};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

const MAX_CHECK_BYTES: u64 = 16 * 1024 * 1024;
const MAX_EVAL_DEFS_BYTES: u64 = 16 * 1024 * 1024;
const MAX_EVAL_LINE_BYTES: usize = 4 * 1024 * 1024;
const MAX_TREE_DEPTH: usize = 64;
const MAX_TREE_FILES: usize = 10_000;
const MAX_TREE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Parser)]
#[command(name = "norn", version, about = "Norn core checker")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Args)]
pub(crate) struct CheckArgs {
    /// JSON array emitted by typed authoring.
    #[arg(
        long,
        conflicts_with = "source",
        requires = "context",
        required_unless_present = "source"
    )]
    pub(crate) artifacts: Option<PathBuf>,
    /// Checker context JSON, containing the explicit tenant scope (required with `--artifacts`, optional with `--source`).
    #[arg(long, required_unless_present = "source")]
    pub(crate) context: Option<PathBuf>,
    /// Repeatable base pack path (artifact JSON file, JSON artifact directory, or `.ts`/`.py` authoring source) merged into `context.base`.
    #[arg(long)]
    pub(crate) base: Vec<PathBuf>,
    /// `TypeScript` (`.ts`) or Python (`.py`) authoring file or directory to compile and check in-process.
    #[arg(
        long,
        conflicts_with = "artifacts",
        required_unless_present = "artifacts"
    )]
    pub(crate) source: Option<PathBuf>,
    /// Explicit organization scope for `--source` compilation (`scope.orgId`; pair with `--env`, or omit both to use `norn.json` scope).
    #[arg(long, requires_all = ["source", "env"])]
    pub(crate) org: Option<String>,
    /// Explicit environment scope for `--source` compilation (`scope.environmentId`; pair with `--org`, or omit both to use `norn.json` scope).
    #[arg(long, requires_all = ["source", "org"])]
    pub(crate) env: Option<String>,
    /// Promote `NORN-AUTHORING-UNSAFE-ZEN` warnings to hard errors when compiling `--source`.
    #[arg(long, requires = "source")]
    pub(crate) forbid_unsafe_zen: bool,
    /// Optional path to `tools/norn-ts` containing pinned `node_modules/effect`.
    #[arg(long, requires = "source")]
    pub(crate) tool_root: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Explain an action, law or concept through the shared checker.
    Explain(explain::ExplainArgs),
    /// Serve available human operations over bounded MCP stdio.
    Mcp,
    /// Discover and invoke a remote server's live MCP catalog.
    Remote(remote::RemoteArgs),
    /// Plan and apply a pack on a running hosted cell (HOST-INSTALL-1).
    Cell(cell::CellArgs),
    /// Read a configured source and print sealed raw observations.
    Source(source::SourceArgs),
    /// Compile typed `TypeScript` (`.ts`) or Python (`.py`) authoring sources to canonical JSON IR.
    Compile(authoring::CompileArgs),
    /// Format typed `TypeScript` (`.ts`, `.tsx`) and Python (`.py`) authoring sources in-process.
    Fmt(authoring::FmtArgs),
    /// Check a `TypeScript` authoring file against the `norn-effect` profile.
    Profile(authoring::ProfileArgs),
    /// Compile standalone `Norn.question` declarations from a `TypeScript` module.
    Question(authoring::QuestionArgs),
    /// Compile standalone `Norn.dashboard` declarations from a `TypeScript` module.
    Dashboard(authoring::DashboardArgs),
    /// Run declarative scenario suites against an isolated in-memory local cell.
    Test(local::TestArgs),
    /// Run a loopback-only local HTTP/1.1 development cell.
    Dev(local::DevArgs),
    /// List the available human operations and their input schemas.
    #[command(
        long_about = "List the available human operations and their input schemas. Returns tools and a catalogDigest, with at most 32 tools per page. Follow nextCursor using --cursor until it is absent. A cursor is bound to this working directory and catalog; restart without it after a revision. Only installed backends appear; discovery never installs definitions."
    )]
    Capabilities {
        /// Opaque nextCursor from the preceding catalog page; omit to start discovery.
        #[arg(long)]
        cursor: Option<String>,
    },
    /// Check a JSON artifact array or native authoring source using the shared checker and resource limits.
    #[command(
        long_about = "Check a JSON artifact array or native TypeScript/Python authoring source using the shared checker and resource limits. Pass --artifacts <FILE> --context <FILE> for compiled IR, or --source <PATH> [--org <ID> --env <ID>] to compile and check in-process (supply both --org and --env, or omit both to read scope from norn.json). A refused report retains diagnostic codes, source locations, remediation and receipt/profile pins. Checking never installs definitions."
    )]
    Check(CheckArgs),
    /// Evaluate an admitted decision, with its sub-decision closure, over
    /// JSON-lines stdin.
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

pub(crate) fn run_check(artifacts: &Path, context: &Path) -> Result<(String, bool), String> {
    let artifacts =
        cell::read_regular_file_bounded(artifacts, MAX_CHECK_BYTES, "16 MiB check input limit")?;
    let context =
        cell::read_regular_file_bounded(context, MAX_CHECK_BYTES, "16 MiB check input limit")?;
    red_team::check_with_red_team(&artifacts, &context)
}

fn run_check_with_base(
    artifacts: &Path,
    context: &Path,
    base: &[PathBuf],
) -> authoring::CommandOutput {
    if base.is_empty() {
        return legacy_to_output(run_check(artifacts, context));
    }
    let artifacts_text = match cell::read_regular_file_bounded(
        artifacts,
        MAX_CHECK_BYTES,
        "16 MiB check input limit",
    ) {
        Ok(text) => text,
        Err(err) => return authoring::CommandOutput::usage_or_io_error(&err),
    };
    let context_text =
        match cell::read_regular_file_bounded(context, MAX_CHECK_BYTES, "16 MiB check input limit")
        {
            Ok(text) => text,
            Err(err) => return authoring::CommandOutput::usage_or_io_error(&err),
        };
    let mut budget = authoring::InvocationBudget::new();
    let scope = authoring::base_packs::BasePackScope {
        org: None,
        env: None,
        forbid_unsafe_zen: false,
        tool_root: None,
    };
    let merged_context = match authoring::base_packs::merge_base_packs_into_context(
        &context_text,
        None,
        &[],
        base,
        &scope,
        &mut budget,
    ) {
        Ok(merged) => merged,
        Err(failure) => return failure.into_output(),
    };
    legacy_to_output(red_team::check_with_red_team(
        &artifacts_text,
        &merged_context,
    ))
}

fn run_eval(
    defs: &Path,
    org: &str,
    env: &str,
    rule: &str,
    units: u64,
) -> Result<(String, bool), String> {
    let documents =
        cell::read_json_dir_bounded(defs, MAX_EVAL_DEFS_BYTES, "16 MiB eval definitions limit")?;
    let mut artifacts = Vec::with_capacity(documents.len());
    let mut exact_payloads = std::collections::BTreeMap::new();
    for text in &documents {
        let artifact: norn_ir::ArtifactEnvelope =
            serde_json::from_str(text).map_err(|error| error.to_string())?;
        if artifact.scope.org_id.as_deref() != Some(org)
            || artifact.scope.environment_id.as_deref() != Some(env)
        {
            return Err("definition scope does not match --org and --env".to_owned());
        }
        let exact =
            norn_ir::RuntimeValue::from_json_text(text).map_err(|error| error.to_string())?;
        if let Some(payload) = exact.as_object().and_then(|object| object.get("payload")) {
            exact_payloads.insert(artifact.artifact_id.clone(), payload.clone());
        }
        artifacts.push(artifact);
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
    let decision =
        eval::OfflineDecision::prepare(&artifacts, &input, &exact_payloads, receipt, digest, rule)?;
    let closure_digest = decision.closure_digest();
    let closure_digest_json =
        serde_json::to_string(closure_digest).map_err(|error| error.to_string())?;
    let mut accepted = 0_u64;
    let mut refused = 0_u64;
    let mut stdin = std::io::stdin().lock();
    let mut stdout = std::io::stdout().lock();
    let mut line_buf = Vec::new();
    let max_line_read = u64::try_from(MAX_EVAL_LINE_BYTES)
        .map_err(|error| error.to_string())?
        .saturating_add(1);
    loop {
        line_buf.clear();
        let read = stdin
            .by_ref()
            .take(max_line_read)
            .read_until(b'\n', &mut line_buf)
            .map_err(|error| error.to_string())?;
        if read == 0 {
            break;
        }
        if read > MAX_EVAL_LINE_BYTES {
            return Err("eval input line exceeds 4 MiB limit".to_owned());
        }
        let line = std::str::from_utf8(&line_buf).map_err(|error| error.to_string())?;
        let line = line.trim_end_matches(['\r', '\n']);
        let response = match decision.run(line, units) {
            Ok(output) => {
                accepted = accepted.checked_add(1).ok_or("eval counter overflow")?;
                format!(
                    "{{\"ok\":true,\"closureDigest\":{closure_digest_json},\"output\":{output}}}"
                )
            }
            Err(error) => {
                refused = refused.checked_add(1).ok_or("eval counter overflow")?;
                serde_json::json!({"ok":false,"closureDigest":closure_digest,"error":error})
                    .to_string()
            }
        };
        writeln!(stdout, "{response}").map_err(|error| error.to_string())?;
    }
    writeln!(
        std::io::stderr().lock(),
        "norn eval: {accepted} accepted, {refused} refused"
    )
    .map_err(|error| error.to_string())?;
    Ok((String::new(), refused == 0))
}

pub(crate) fn run_check_args(args: &CheckArgs) -> authoring::CommandOutput {
    if let Some(source_path) = &args.source {
        if args.artifacts.is_some() {
            return authoring::CommandOutput::usage_or_io_error(
                "--artifacts and --source cannot be combined",
            );
        }
        if args.org.is_some() != args.env.is_some() {
            return authoring::CommandOutput::usage_or_io_error(
                "--org and --env must be supplied together with --source (or omit both when checking a norn.json project)",
            );
        }
        return authoring::execute_check_source(&authoring::CheckSourceRequest {
            source: source_path,
            context: args.context.as_deref(),
            base: &args.base,
            org: args.org.as_deref(),
            env: args.env.as_deref(),
            forbid_unsafe_zen: args.forbid_unsafe_zen,
            tool_root: args.tool_root.as_deref(),
        });
    }
    let Some(artifacts_path) = &args.artifacts else {
        return authoring::CommandOutput::usage_or_io_error(
            "specify either --artifacts <FILE> --context <FILE> or --source <PATH> [--org <ID> --env <ID>]",
        );
    };
    if args.org.is_some()
        || args.env.is_some()
        || args.forbid_unsafe_zen
        || args.tool_root.is_some()
    {
        return authoring::CommandOutput::usage_or_io_error(
            "--org, --env, --forbid-unsafe-zen, and --tool-root require --source",
        );
    }
    let Some(context_path) = &args.context else {
        return authoring::CommandOutput::usage_or_io_error(
            "--context is required with --artifacts",
        );
    };
    run_check_with_base(artifacts_path, context_path, &args.base)
}

fn legacy_to_output(result: Result<(String, bool), String>) -> authoring::CommandOutput {
    match result {
        Ok((document, true)) => authoring::CommandOutput::ok(document),
        Ok((document, false)) => authoring::CommandOutput::refused(document),
        Err(error) => authoring::CommandOutput {
            stdout: String::new(),
            stderr: format!("norn: {error}"),
            exit_code: 1,
        },
    }
}

fn run(command: Command) -> authoring::CommandOutput {
    match command {
        Command::Explain(args) => legacy_to_output(explain::execute(&args)),
        Command::Mcp => legacy_to_output(mcp::serve()),
        Command::Capabilities { cursor } => legacy_to_output(mcp::capabilities(cursor.as_deref())),
        Command::Remote(args) => legacy_to_output(remote::execute(&args)),
        Command::Cell(args) => legacy_to_output(cell::execute(&args)),
        Command::Source(args) => legacy_to_output(source::execute(&args)),
        Command::Compile(args) => authoring::execute_compile(&args),
        Command::Fmt(args) => authoring::execute_fmt(&args),
        Command::Profile(args) => authoring::execute_profile(&args),
        Command::Question(args) => authoring::execute_question(&args),
        Command::Dashboard(args) => authoring::execute_dashboard(&args),
        Command::Test(args) => local::execute_test(&args),
        Command::Dev(args) => local::execute_dev(&args),
        Command::Openapi => legacy_to_output(
            norn_api::openapi::openapi_json()
                .map(|document| (document, true))
                .map_err(|error| error.to_string()),
        ),
        Command::Eval {
            defs,
            org,
            env,
            rule,
            units,
        } => legacy_to_output(run_eval(&defs, &org, &env, &rule, units)),
        Command::SourceDigest { dir } => {
            legacy_to_output(source_digest(&dir).map(|document| (document, true)))
        }
        Command::Check(args) => run_check_args(&args),
    }
}

#[cfg(unix)]
fn open_tree_entry<P: rustix::path::Arg>(
    dir: impl std::os::fd::AsFd,
    name: P,
    display_path: &Path,
) -> Result<std::fs::File, String> {
    use rustix::fs::{Mode, OFlags};
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    rustix::fs::openat(dir, name, flags, Mode::empty())
        .map(std::fs::File::from)
        .map_err(|error| {
            if error == rustix::io::Errno::LOOP {
                format!("{}: symlinks are refused", display_path.display())
            } else if error == rustix::io::Errno::NXIO || error == rustix::io::Errno::OPNOTSUPP {
                format!("{}: not a regular file", display_path.display())
            } else {
                format!("{}: {error}", display_path.display())
            }
        })
}

#[cfg(unix)]
fn walk_source_tree(
    dir_fd: &std::fs::File,
    dir_display: &Path,
    prefix: &str,
    depth: usize,
    budget: &mut u64,
    files: &mut Vec<(String, Vec<u8>)>,
) -> Result<(), String> {
    if depth > MAX_TREE_DEPTH {
        return Err(format!(
            "{}: directory nesting exceeds {MAX_TREE_DEPTH}",
            dir_display.display()
        ));
    }
    let mut entries = Vec::new();
    for entry in rustix::fs::Dir::read_from(dir_fd)
        .map_err(|error| format!("{}: {error}", dir_display.display()))?
    {
        let name = entry
            .map_err(|error| format!("{}: {error}", dir_display.display()))?
            .file_name()
            .to_owned();
        if name.as_bytes() == b"." || name.as_bytes() == b".." {
            continue;
        }
        entries.push(name);
    }
    entries.sort();
    for entry in entries {
        let name = entry
            .to_str()
            .map_err(|_error| format!("non-UTF-8 name under {}", dir_display.display()))?;
        if name.starts_with('.') {
            continue;
        }
        let child_display = dir_display.join(name);
        let opened = open_tree_entry(dir_fd, entry.as_c_str(), &child_display)?;
        let kind = opened
            .metadata()
            .map_err(|error| format!("{}: {error}", child_display.display()))?
            .file_type();
        if kind.is_dir() {
            if name == "target" {
                continue;
            }
            let next_prefix = if prefix.is_empty() {
                name.to_owned()
            } else {
                format!("{prefix}/{name}")
            };
            walk_source_tree(
                &opened,
                &child_display,
                &next_prefix,
                depth.saturating_add(1),
                budget,
                files,
            )?;
        } else if kind.is_file() {
            if files.len() >= MAX_TREE_FILES {
                return Err(format!(
                    "{}: source tree exceeds {MAX_TREE_FILES} files",
                    child_display.display()
                ));
            }
            let relative = if prefix.is_empty() {
                name.to_owned()
            } else {
                format!("{prefix}/{name}")
            };
            let mut bytes = Vec::new();
            opened
                .take(budget.saturating_add(1))
                .read_to_end(&mut bytes)
                .map_err(|error| format!("{}: {error}", child_display.display()))?;
            let used = u64::try_from(bytes.len())
                .map_err(|error| format!("{}: {error}", child_display.display()))?;
            *budget = budget.checked_sub(used).ok_or_else(|| {
                format!(
                    "{}: source tree exceeds 64 MiB limit",
                    child_display.display()
                )
            })?;
            files.push((relative, bytes));
        } else {
            return Err(format!("{}: not a regular file", child_display.display()));
        }
    }
    Ok(())
}

/// HOST-TREE-1: read every counted file below `root` and digest the tree.
#[cfg(unix)]
fn source_digest(root: &Path) -> Result<String, String> {
    let target: PathBuf = root.components().collect();
    let root_fd = open_tree_entry(rustix::fs::CWD, &target, root)?;
    if !root_fd
        .metadata()
        .map_err(|error| format!("{}: {error}", root.display()))?
        .is_dir()
    {
        return Err(format!("{}: not a directory", root.display()));
    }
    let mut budget = MAX_TREE_BYTES;
    let mut files = Vec::new();
    walk_source_tree(&root_fd, root, "", 0, &mut budget, &mut files)?;
    let digest = norn_ir::source_tree::source_tree_digest(
        files
            .iter()
            .map(|(path, bytes)| (path.as_str(), bytes.as_slice())),
    )
    .map_err(|error| error.to_string())?;
    Ok(serde_json::json!({ "digest": digest.0, "files": files.len() }).to_string())
}

#[cfg(not(unix))]
fn walk_source_tree(
    root: &Path,
    dir: &Path,
    depth: usize,
    budget: &mut u64,
    files: &mut Vec<(String, Vec<u8>)>,
) -> Result<(), String> {
    if depth > MAX_TREE_DEPTH {
        return Err(format!(
            "{}: directory nesting exceeds {MAX_TREE_DEPTH}",
            dir.display()
        ));
    }
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
            walk_source_tree(root, &path, depth.saturating_add(1), budget, files)?;
        } else if kind.is_file() {
            if files.len() >= MAX_TREE_FILES {
                return Err(format!(
                    "{}: source tree exceeds {MAX_TREE_FILES} files",
                    path.display()
                ));
            }
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
            let file = std::fs::File::open(&path)
                .map_err(|error| format!("{}: {error}", path.display()))?;
            let mut bytes = Vec::new();
            file.take(budget.saturating_add(1))
                .read_to_end(&mut bytes)
                .map_err(|error| format!("{}: {error}", path.display()))?;
            let used = u64::try_from(bytes.len())
                .map_err(|error| format!("{}: {error}", path.display()))?;
            *budget = budget
                .checked_sub(used)
                .ok_or_else(|| format!("{}: source tree exceeds 64 MiB limit", path.display()))?;
            files.push((relative, bytes));
        } else {
            return Err(format!("{}: not a regular file", path.display()));
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn source_digest(root: &Path) -> Result<String, String> {
    let meta =
        std::fs::symlink_metadata(root).map_err(|error| format!("{}: {error}", root.display()))?;
    if meta.file_type().is_symlink() {
        return Err(format!("{}: symlinks are refused", root.display()));
    }
    if !meta.is_dir() {
        return Err(format!("{}: not a directory", root.display()));
    }
    let mut budget = MAX_TREE_BYTES;
    let mut files = Vec::new();
    walk_source_tree(root, root, 0, &mut budget, &mut files)?;
    let digest = norn_ir::source_tree::source_tree_digest(
        files
            .iter()
            .map(|(path, bytes)| (path.as_str(), bytes.as_slice())),
    )
    .map_err(|error| error.to_string())?;
    Ok(serde_json::json!({ "digest": digest.0, "files": files.len() }).to_string())
}

fn main() -> ExitCode {
    let output = run(Cli::parse().command);
    if !output.stderr.is_empty() {
        let _written = writeln!(std::io::stderr().lock(), "{}", output.stderr);
    }
    if !output.stdout.is_empty() {
        let write_result = if output.stdout.ends_with('\n') {
            write!(std::io::stdout().lock(), "{}", output.stdout)
        } else {
            writeln!(std::io::stdout().lock(), "{}", output.stdout)
        };
        if write_result.is_err() {
            return ExitCode::FAILURE;
        }
    }
    ExitCode::from(output.exit_code)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "HOST-TREE-EXCLUDE fixture assertions"
)]
mod tests {
    //! HOST-TREE-EXCLUDE: the walk's exclusion order (Core 07, HOST-TREE-1).
    use std::path::{Path, PathBuf};

    /// A fresh directory holding `src/lib.rs`, removed on drop.
    struct Tree(PathBuf);

    impl Tree {
        fn new(name: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("norn-host-tree-{name}-{}", std::process::id()));
            let _stale = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join("src")).unwrap();
            std::fs::write(root.join("src/lib.rs"), b"//! crate\n").unwrap();
            Self(root)
        }

        fn write(&self, path: &str, bytes: &[u8]) {
            let path = self.0.join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, bytes).unwrap();
        }

        fn digest(&self) -> Result<String, String> {
            super::source_digest(&self.0)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _cleaned = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn hidden_entries_and_target_directories_are_skipped_at_any_depth() {
        let base = Tree::new("base").digest().unwrap();
        let tree = Tree::new("skipped");
        tree.write(".hidden", b"x");
        tree.write(".git/config", b"x");
        tree.write("src/.cache/blob", b"x");
        tree.write("target/debug/out", b"x");
        tree.write("src/target/nested", b"x");
        assert_eq!(tree.digest().unwrap(), base);
    }

    #[test]
    fn a_file_named_target_counts() {
        let base = Tree::new("file-base").digest().unwrap();
        let tree = Tree::new("file");
        tree.write("src/target", b"x");
        assert_ne!(tree.digest().unwrap(), base);
    }

    #[cfg(unix)]
    #[test]
    fn a_hidden_symlink_is_skipped_and_any_other_symlink_refuses() {
        use std::os::unix::fs::symlink;
        let base = Tree::new("link-base").digest().unwrap();
        let hidden = Tree::new("hidden-link");
        symlink(
            hidden.path().join("src/lib.rs"),
            hidden.path().join(".link"),
        )
        .unwrap();
        symlink(hidden.path().join("src"), hidden.path().join("src/.dir")).unwrap();
        assert_eq!(
            hidden.digest().unwrap(),
            base,
            "a hidden symlink is never followed"
        );
        // A hidden directory is never visited: what it holds would refuse.
        let unvisited = Tree::new("hidden-dir");
        std::fs::create_dir_all(unvisited.path().join(".git")).unwrap();
        symlink(
            unvisited.path().join("src"),
            unvisited.path().join(".git/link"),
        )
        .unwrap();
        {
            use std::os::unix::ffi::OsStrExt;
            let name = std::ffi::OsStr::from_bytes(b"\xff");
            // Some filesystems refuse non-UTF-8 names themselves.
            let _optional = std::fs::write(unvisited.path().join(".git").join(name), b"x");
        }
        assert_eq!(
            unvisited.digest().unwrap(),
            base,
            "nothing below a hidden directory is read or refused"
        );
        for name in ["link", "target"] {
            let tree = Tree::new(&format!("visible-{name}"));
            symlink(tree.path().join("src"), tree.path().join(name)).unwrap();
            let refused = tree.digest().unwrap_err();
            assert!(
                refused.contains("symlinks are refused"),
                "{name}: {refused}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn an_entry_that_is_neither_a_directory_nor_a_file_refuses() {
        let tree = Tree::new("socket");
        let _socket = std::os::unix::net::UnixListener::bind(tree.path().join("src/sock")).unwrap();
        let refused = tree.digest().unwrap_err();
        assert!(refused.contains("not a regular file"), "{refused}");
    }

    #[cfg(unix)]
    #[test]
    fn a_non_utf8_name_refuses_even_when_hidden() {
        use std::os::unix::ffi::OsStrExt;
        let tree = Tree::new("non-utf8");
        let name = std::ffi::OsStr::from_bytes(b".\xff");
        if std::fs::write(tree.path().join(name), b"x").is_err() {
            return; // the filesystem refuses non-UTF-8 names itself
        }
        assert!(tree.digest().unwrap_err().contains("non-UTF-8"));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_root_is_refused_not_followed() {
        use std::os::unix::fs::symlink;
        let tree = Tree::new("symlink-root-target");
        let link = std::env::temp_dir().join(format!("norn-host-tree-link-{}", std::process::id()));
        let _stale = std::fs::remove_file(&link);
        symlink(tree.path(), &link).unwrap();
        let refused = super::source_digest(&link).unwrap_err();
        let _cleaned = std::fs::remove_file(&link);
        assert!(refused.contains("symlinks are refused"), "{refused}");
    }
}
