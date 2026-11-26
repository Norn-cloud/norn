//! Native entrypoint for the same checker ABI used by typed TypeScript authoring.
use std::io::{BufRead as _, Read as _, Write as _};
mod cell;
#[cfg(test)]
mod cli_contract;
mod eval;
mod explain;
mod mcp;
mod remote;
mod source;
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

#[allow(
    clippy::too_many_lines,
    reason = "one dispatch maps every CLI subcommand to its handler"
)]
fn run(command: Command) -> Result<(String, bool), String> {
    match command {
        Command::Explain(args) => explain::execute(&args),
        Command::Mcp => mcp::serve(),
        Command::Capabilities { cursor } => mcp::capabilities(cursor.as_deref()),
        Command::Remote(args) => remote::execute(&args),
        Command::Cell(args) => cell::execute(&args),
        Command::Source(args) => source::execute(&args),
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
            const MAX_EVAL_LINE: usize = 4 * 1024 * 1024;
            const MAX_EVAL_LINE_READ: u64 = 4 * 1024 * 1024 + 1;
            let cell::JsonInput::Dir(raw_docs) =
                cell::read_json_input(&defs, cell::MAX_INPUT_FILE, "8 MiB eval input")?
            else {
                return Err(format!("{}: not a directory", defs.display()));
            };
            let mut artifacts = Vec::with_capacity(raw_docs.len());
            let mut documents = Vec::with_capacity(raw_docs.len());
            let mut exact_payloads = std::collections::BTreeMap::new();
            for text in raw_docs {
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
                &input,
                &exact_payloads,
                receipt,
                digest,
                &rule,
            )?;
            let mut accepted = 0_u64;
            let mut refused = 0_u64;
            let mut stdin = std::io::stdin().lock();
            let mut stdout = std::io::stdout().lock();
            let mut raw_line = Vec::new();
            loop {
                raw_line.clear();
                let bytes = stdin
                    .by_ref()
                    .take(MAX_EVAL_LINE_READ)
                    .read_until(b'\n', &mut raw_line)
                    .map_err(|error| error.to_string())?;
                if bytes == 0 {
                    break;
                }
                if bytes > MAX_EVAL_LINE {
                    return Err("eval input line exceeds 4 MiB".to_owned());
                }
                let line = std::str::from_utf8(&raw_line)
                    .map_err(|error| error.to_string())?
                    .trim_end_matches(['\r', '\n']);
                let digest = decision.closure_digest();
                let response = match decision.run(line, units) {
                    Ok(output) => {
                        accepted = accepted.checked_add(1).ok_or("eval counter overflow")?;
                        format!(
                            "{{\"ok\":true,\"closureDigest\":\"{digest}\",\"output\":{output}}}"
                        )
                    }
                    Err(error) => {
                        refused = refused.checked_add(1).ok_or("eval counter overflow")?;
                        serde_json::json!({"ok":false,"closureDigest":digest,"error":error})
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
        Command::SourceDigest { dir } => source_digest(&dir).map(|document| (document, true)),
        Command::Check { artifacts, context } => {
            let artifacts =
                cell::read_regular_text(&artifacts, cell::MAX_INPUT_FILE, "8 MiB check input")?;
            let context =
                cell::read_regular_text(&context, cell::MAX_INPUT_FILE, "8 MiB check input")?;
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

#[cfg(unix)]
const MAX_TREE_BYTES: u64 = 64 * 1024 * 1024;
#[cfg(unix)]
const MAX_TREE_DEPTH: usize = 128;

#[cfg(unix)]
fn open_tree_entry(
    dir: impl std::os::fd::AsFd,
    name: &std::ffi::CStr,
    shown: &std::path::Path,
) -> Result<std::fs::File, String> {
    use rustix::fs::{AtFlags, Mode, OFlags};
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    match rustix::fs::openat(&dir, name, flags, Mode::empty()) {
        Ok(fd) => Ok(std::fs::File::from(fd)),
        Err(rustix::io::Errno::LOOP) => Err(format!("{}: symlinks are refused", shown.display())),
        Err(rustix::io::Errno::NXIO | rustix::io::Errno::OPNOTSUPP) => {
            // On Unix, opening a domain socket with O_RDONLY fails with ENXIO/EOPNOTSUPP.
            if let Ok(stat) = rustix::fs::statat(&dir, name, AtFlags::SYMLINK_NOFOLLOW) {
                let kind = rustix::fs::FileType::from_raw_mode(stat.st_mode);
                if kind == rustix::fs::FileType::Symlink {
                    return Err(format!("{}: symlinks are refused", shown.display()));
                }
            }
            Err(format!("{}: not a regular file", shown.display()))
        }
        Err(error) => Err(format!("{}: {error}", shown.display())),
    }
}

#[cfg(unix)]
fn walk_source_tree(
    dir_fd: &std::fs::File,
    dir_path: &std::path::Path,
    prefix: &str,
    depth: usize,
    budget: &mut u64,
    files: &mut Vec<(String, Vec<u8>)>,
) -> Result<(), String> {
    if depth > MAX_TREE_DEPTH {
        return Err(format!(
            "{}: source tree exceeds maximum nesting depth",
            dir_path.display()
        ));
    }
    let mut names = Vec::new();
    for entry in rustix::fs::Dir::read_from(dir_fd)
        .map_err(|error| format!("{}: {error}", dir_path.display()))?
    {
        let name = entry
            .map_err(|error| format!("{}: {error}", dir_path.display()))?
            .file_name()
            .to_owned();
        let raw = name.as_bytes();
        if raw == b"." || raw == b".." {
            continue;
        }
        names.push(name);
    }
    names.sort();
    for entry_name in names {
        let name = entry_name
            .to_str()
            .map_err(|_e| format!("non-UTF-8 name under {}", dir_path.display()))?;
        if name.starts_with('.') {
            continue;
        }
        let child_path = dir_path.join(name);
        let opened = open_tree_entry(dir_fd, entry_name.as_c_str(), &child_path)?;
        let kind = opened
            .metadata()
            .map_err(|error| format!("{}: {error}", child_path.display()))?
            .file_type();
        let relative = if prefix.is_empty() {
            name.to_owned()
        } else {
            format!("{prefix}/{name}")
        };
        if kind.is_dir() {
            if name == "target" {
                continue;
            }
            walk_source_tree(
                &opened,
                &child_path,
                &relative,
                depth.saturating_add(1),
                budget,
                files,
            )?;
        } else if kind.is_file() {
            let mut bytes = Vec::new();
            opened
                .take(budget.saturating_add(1))
                .read_to_end(&mut bytes)
                .map_err(|error| format!("{}: {error}", child_path.display()))?;
            let used = u64::try_from(bytes.len()).map_err(|error| error.to_string())?;
            *budget = budget.checked_sub(used).ok_or_else(|| {
                format!("{}: source tree exceeds 64 MiB limit", child_path.display())
            })?;
            files.push((relative, bytes));
        } else {
            return Err(format!("{}: not a regular file", child_path.display()));
        }
    }
    Ok(())
}

/// HOST-TREE-1: read every counted file below `root` and digest the tree.
#[cfg(unix)]
fn source_digest(root: &std::path::Path) -> Result<String, String> {
    use std::os::unix::ffi::OsStrExt as _;
    let raw = root.as_os_str().as_bytes();
    let target: PathBuf = if raw == b"/" {
        PathBuf::from("/")
    } else {
        root.components().collect()
    };
    let root_fd = cell::open_no_follow(rustix::fs::CWD, &target).map_err(|error| {
        if error == "links are refused" {
            format!("{}: symlinks are refused", root.display())
        } else {
            format!("{}: {error}", root.display())
        }
    })?;
    let is_dir = root_fd
        .metadata()
        .map_err(|error| format!("{}: {error}", root.display()))?
        .is_dir();
    if !is_dir {
        return Err(format!("{}: not a directory", root.display()));
    }
    let mut files = Vec::new();
    let mut budget = MAX_TREE_BYTES;
    walk_source_tree(&root_fd, root, "", 0, &mut budget, &mut files)?;
    let digest = norn_ir::source_tree::source_tree_digest(
        files
            .iter()
            .map(|(path, bytes)| (path.as_str(), bytes.as_slice())),
    )
    .map_err(|error| error.to_string())?;
    Ok(serde_json::json!({ "digest": digest.0, "files": files.len() }).to_string())
}

/// HOST-TREE-1: read every counted file below `root` and digest the tree.
#[cfg(not(unix))]
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
    if std::fs::symlink_metadata(root)
        .map_err(|error| format!("{}: {error}", root.display()))?
        .file_type()
        .is_symlink()
    {
        return Err(format!("{}: symlinks are refused", root.display()));
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
        let target = Tree::new("root-link-target");
        let root_link =
            std::env::temp_dir().join(format!("norn-host-tree-root-link-{}", std::process::id()));
        let _stale = std::fs::remove_file(&root_link);
        symlink(target.path(), &root_link).unwrap();
        let refused = super::source_digest(&root_link).unwrap_err();
        let _cleaned = std::fs::remove_file(&root_link);
        assert!(
            refused.contains("symlinks are refused"),
            "root symlink: {refused}"
        );
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
}
