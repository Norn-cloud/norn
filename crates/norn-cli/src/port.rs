//! `norn port`: forward to the porting tools shipped with this binary.
//!
//! The CLI links none of that code. It finds the tool directory from an
//! explicit absolute path, the installed layout, or the checkout it was built
//! from, never from the working directory or the tree being analysed; refuses
//! a directory, script or interpreter that someone else could have written;
//! runs the tool with an isolated interpreter and a short explicit
//! environment; and exits with the tool's own status.
//!
//! `code-ontology` reads source as data and never imports or executes it.
//! `parity` is different: it EXECUTES the original function it is given, in a
//! subprocess. That is process isolation for source the caller already
//! trusts. It is not a sandbox.
use clap::{Args, ValueEnum};
use std::ffi::{OsStr, OsString};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, ExitStatus};

/// Absolute path of the tool directory, for a binary that is not installed
/// next to it.
const HOME: &str = "NORN_PORT_HOME";
/// Absolute path of the Python interpreter, instead of `python3` on `PATH`.
const PYTHON: &str = "NORN_PORT_PYTHON";
/// The only variables a tool receives. `CI` lets a tool refuse what it only
/// allows interactively; the rest are where temporary files and text go.
const PASSED: &[&str] = &["TMPDIR", "LANG", "LC_ALL", "CI"];

#[derive(Debug, Args)]
pub(super) struct PortArgs {
    /// The tool to run.
    #[arg(value_enum)]
    tool: Tool,
    /// Arguments for the tool, passed unchanged.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    args: Vec<OsString>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Tool {
    /// EXECUTES the named original function and compares it with its Norn
    /// candidate on generated inputs. Not a sandbox: only for code you would
    /// run yourself.
    Parity,
    /// Count inventoried, translated and measured code units.
    Scoreboard,
    /// Emit `norn.code-ontology/1` facts for source directories, which are
    /// read as data and never imported or executed.
    CodeOntology,
    /// Check the parity harness against this binary.
    Selftest,
}

impl Tool {
    /// The script, and the arguments that select this tool inside it.
    const fn script(self) -> (&'static str, &'static [&'static str]) {
        match self {
            Self::Parity => ("port.py", &["parity"]),
            Self::Scoreboard => ("port.py", &["scoreboard"]),
            Self::CodeOntology => ("code_ontology.py", &[]),
            Self::Selftest => ("selftest.py", &[]),
        }
    }

    /// Whether the tool drives a `norn` binary, which is then this one.
    const fn drives_norn(self) -> bool {
        matches!(self, Self::Parity | Self::Selftest)
    }
}

/// The checkout this binary was built from, fixed at build time. Runtime
/// location of the binary plays no part in it.
const CHECKOUT_TOOLS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tools/norn-port");

/// The tool directory, checked by [`trusted_tools`] before it is returned.
///
/// In order: `explicit` (`NORN_PORT_HOME`, absolute); for a binary installed
/// as `<prefix>/bin/norn`, exactly `<prefix>/share/norn/port` when that
/// directory exists; otherwise the checkout the binary was built from. The
/// working directory and any other place near the binary are never searched.
fn home(explicit: Option<OsString>, binary: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(explicit) = explicit {
        let explicit = PathBuf::from(explicit);
        if !explicit.is_absolute() {
            return Err(format!(
                "{HOME} must be an absolute path, not {}",
                explicit.display()
            ));
        }
        return trusted_tools(&explicit);
    }
    let installed = binary
        .and_then(Path::parent)
        .filter(|directory| directory.file_name().is_some_and(|name| name == "bin"))
        .and_then(Path::parent)
        .map(|prefix| prefix.join("share/norn/port"));
    if let Some(installed) = installed
        && installed.exists()
    {
        return trusted_tools(&installed);
    }
    let checkout = Path::new(CHECKOUT_TOOLS);
    if checkout.join("port.py").is_file() {
        return trusted_tools(checkout);
    }
    Err(format!(
        "the porting tools are not installed with this binary and its build checkout is gone; install them at <prefix>/share/norn/port beside <prefix>/bin/norn, or set {HOME} to the absolute path of tools/norn-port in a Norn checkout"
    ))
}

/// The canonical tool directory, once it and its ancestors pass the checks in
/// [`trust`].
fn trusted_tools(directory: &Path) -> Result<PathBuf, String> {
    let canonical = std::fs::canonicalize(directory)
        .map_err(|error| format!("{}: {error}", directory.display()))?;
    trust::directory(&canonical)?;
    Ok(canonical)
}

/// Whether a file or directory may supply code that runs as this user.
///
/// An object passes when it is owned by this user or root and is not writable
/// by group or others; every directory above it must be owned by this user or
/// root too, and may be writable by group or others only when it is owned by
/// root and has the sticky bit (as `/tmp` is): there, nobody else can remove or
/// replace the entry below it, which itself must pass. The tool directory
/// itself and each file are held to the strict rule, sticky or not. A symlink
/// is checked where it sits (owner only, since its own mode means nothing) and
/// again at its canonical target. Off Unix there is no check.
mod trust {
    use std::path::Path;

    #[cfg(unix)]
    fn owner_ok(uid: u32) -> bool {
        uid == 0 || uid == rustix::process::geteuid().as_raw()
    }

    #[cfg(unix)]
    fn refuse(path: &Path, why: &str) -> String {
        format!(
            "{} {why}; code from there could have been planted by another user, so norn port refuses to run it",
            path.display()
        )
    }

    /// `path` itself under the strict rule.
    #[cfg(unix)]
    fn strict(path: &Path) -> Result<(), String> {
        use std::os::unix::fs::MetadataExt as _;
        let metadata =
            std::fs::metadata(path).map_err(|error| format!("{}: {error}", path.display()))?;
        if !owner_ok(metadata.uid()) {
            return Err(refuse(path, "is owned by another user"));
        }
        if metadata.mode() & 0o022 != 0 {
            return Err(refuse(path, "is writable by group or others"));
        }
        Ok(())
    }

    /// Every directory above the canonical `path`.
    #[cfg(unix)]
    fn ancestors(path: &Path) -> Result<(), String> {
        use std::os::unix::fs::MetadataExt as _;
        for ancestor in path.ancestors().skip(1) {
            let metadata = std::fs::metadata(ancestor)
                .map_err(|error| format!("{}: {error}", ancestor.display()))?;
            if !owner_ok(metadata.uid()) {
                return Err(refuse(ancestor, "is owned by another user"));
            }
            let sticky_root = metadata.uid() == 0 && metadata.mode() & 0o1000 != 0;
            if metadata.mode() & 0o022 != 0 && !sticky_root {
                return Err(refuse(ancestor, "is writable by group or others"));
            }
        }
        Ok(())
    }

    /// A canonical directory: itself strictly, its ancestors as above.
    #[cfg(unix)]
    pub(super) fn directory(canonical: &Path) -> Result<(), String> {
        strict(canonical)?;
        ancestors(canonical)
    }

    /// A file that will be executed or read as code, possibly through links.
    #[cfg(unix)]
    pub(super) fn file(path: &Path) -> Result<(), String> {
        use std::os::unix::fs::MetadataExt as _;
        let parent = path
            .parent()
            .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
        let parent = std::fs::canonicalize(parent)
            .map_err(|error| format!("{}: {error}", parent.display()))?;
        // The directory that holds the entry decides who can replace it.
        if let Err(error) = strict(&parent) {
            let sticky_root = std::fs::metadata(&parent)
                .is_ok_and(|metadata| metadata.uid() == 0 && metadata.mode() & 0o1000 != 0);
            if !sticky_root {
                return Err(error);
            }
        }
        ancestors(&parent)?;
        let link = path
            .file_name()
            .map(|name| parent.join(name))
            .ok_or_else(|| format!("{} names no file", path.display()))?;
        let metadata = std::fs::symlink_metadata(&link)
            .map_err(|error| format!("{}: {error}", link.display()))?;
        if !owner_ok(metadata.uid()) {
            return Err(refuse(&link, "is owned by another user"));
        }
        if metadata.file_type().is_symlink() {
            let target = std::fs::canonicalize(&link)
                .map_err(|error| format!("{}: {error}", link.display()))?;
            return file(&target);
        }
        strict(&link)
    }

    #[cfg(not(unix))]
    pub(super) fn directory(_canonical: &Path) -> Result<(), String> {
        Ok(())
    }

    #[cfg(not(unix))]
    pub(super) fn file(_path: &Path) -> Result<(), String> {
        Ok(())
    }
}

/// The interpreter: `explicit` when given, otherwise the first `python3` in
/// an absolute `PATH` entry. A relative entry would resolve against the
/// working directory, so it is skipped. Either way it must pass the same
/// ownership check as the tools, at its own location and at its target.
fn interpreter(explicit: Option<OsString>, path: Option<&OsStr>) -> Result<PathBuf, String> {
    let chosen = if let Some(explicit) = explicit {
        let explicit = PathBuf::from(explicit);
        if !explicit.is_absolute() {
            return Err(format!(
                "{PYTHON} must be an absolute path, not {}",
                explicit.display()
            ));
        }
        explicit
    } else {
        path.into_iter()
            .flat_map(|path| std::env::split_paths(path))
            .filter(|directory| directory.is_absolute())
            .map(|directory| directory.join("python3"))
            .find(|candidate| candidate.is_file())
            .ok_or_else(|| {
                format!(
                    "no python3 in an absolute PATH entry; set {PYTHON} to an interpreter's absolute path"
                )
            })?
    };
    trust::file(&chosen)?;
    // Run it by the name it was found under: a virtual environment's python
    // is a link, and Python finds the environment from that name.
    Ok(chosen)
}

/// Every argument after the interpreter, in order.
fn arguments(
    tool: Tool,
    home: &Path,
    binary: Option<&Path>,
    passed: &[OsString],
) -> Result<Vec<OsString>, String> {
    let (script, selector) = tool.script();
    let script = home.join(script);
    if !script.is_file() {
        return Err(format!("{} is not a file", script.display()));
    }
    trust::file(&script)?;
    // Isolated mode: no PYTHON* variables, no user site directory, and neither
    // the working directory nor the script directory on the module path.
    let mut command = vec![OsString::from("-I"), script.into_os_string()];
    command.extend(selector.iter().map(OsString::from));
    if tool.drives_norn() {
        let binary = binary.ok_or("the running binary's location is unknown")?;
        // First, so a `--norn` the caller passes still wins.
        command.push("--norn".into());
        command.push(binary.as_os_str().to_owned());
    }
    command.extend(passed.iter().cloned());
    Ok(command)
}

/// The tool's own exit status; a signal becomes 128 plus its number.
fn exit_code(status: ExitStatus) -> u8 {
    if let Some(code) = status.code() {
        return u8::try_from(code).unwrap_or(1);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt as _;
        if let Some(signal) = status.signal() {
            return u8::try_from(signal.saturating_add(128)).unwrap_or(u8::MAX);
        }
    }
    1
}

fn run(args: &PortArgs) -> Result<u8, String> {
    // Resolving links first keeps a symlinked `norn` from moving the lookup.
    let binary = std::env::current_exe().and_then(std::fs::canonicalize).ok();
    let home = home(std::env::var_os(HOME), binary.as_deref())?;
    let python = interpreter(
        std::env::var_os(PYTHON),
        std::env::var_os("PATH").as_deref(),
    )?;
    let arguments = arguments(args.tool, &home, binary.as_deref(), &args.args)?;
    #[allow(
        clippy::disallowed_methods,
        reason = "this command's one capability: running the named porting tool"
    )]
    let mut command = Command::new(&python);
    command.args(&arguments).env_clear();
    for name in PASSED {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    let status = command
        .status()
        .map_err(|error| format!("{}: {error}", python.display()))?;
    Ok(exit_code(status))
}

/// Run the tool with inherited standard streams and return its exit status.
pub(super) fn execute(args: &PortArgs) -> ExitCode {
    match run(args) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            let _written = writeln!(std::io::stderr().lock(), "norn port: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "location contract assertions"
)]
mod tests {
    //! Where the tool and the interpreter may come from. What actually runs
    //! is checked against real processes in `tests/port.rs`.
    use super::{Tool, arguments, home, interpreter};
    use std::ffi::OsString;
    use std::path::{Path, PathBuf};

    /// This repository's root.
    fn root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .unwrap()
            .to_path_buf()
    }

    #[test]
    fn a_relative_home_or_interpreter_is_refused() {
        for relative in ["tools/norn-port", ".", "../x"] {
            let error = home(Some(relative.into()), None).expect_err("relative");
            assert!(error.contains("NORN_PORT_HOME"), "{error}");
            let error = interpreter(Some(relative.into()), None).expect_err("relative");
            assert!(error.contains("NORN_PORT_PYTHON"), "{error}");
        }
    }

    #[test]
    fn without_an_explicit_home_the_build_checkout_is_used_wherever_the_binary_is() {
        let tools = root().join("tools/norn-port").canonicalize().unwrap();
        for binary in [
            root().join("target/debug/norn"),
            PathBuf::from("/tmp/x/norn"),
            PathBuf::from("/nonexistent/prefix/bin/norn"),
        ] {
            assert_eq!(
                home(None, Some(&binary)).unwrap(),
                tools,
                "{}",
                binary.display()
            );
        }
        assert_eq!(home(None, None).unwrap(), tools);
    }

    #[test]
    fn relative_path_entries_never_supply_the_interpreter() {
        let relative: OsString = std::env::join_paths([Path::new("."), Path::new("bin")]).unwrap();
        assert!(interpreter(None, Some(&relative)).is_err());
        assert!(interpreter(None, None).is_err());
    }

    #[test]
    fn arguments_are_isolated_ordered_and_name_this_binary_where_needed() {
        let passed: Vec<OsString> = vec!["--manifest".into(), "-x".into(), "a b".into()];
        let binary = Path::new("/opt/norn");
        let home = root().join("tools/norn-port");
        let text = |tool| -> Vec<String> {
            arguments(tool, &home, Some(binary), &passed)
                .unwrap()
                .iter()
                .map(|value| value.to_string_lossy().into_owned())
                .collect()
        };
        for (tool, script, rest) in [
            (
                Tool::Parity,
                "port.py",
                vec!["parity", "--norn", "/opt/norn", "--manifest", "-x", "a b"],
            ),
            (
                Tool::Scoreboard,
                "port.py",
                vec!["scoreboard", "--manifest", "-x", "a b"],
            ),
            (
                Tool::CodeOntology,
                "code_ontology.py",
                vec!["--manifest", "-x", "a b"],
            ),
            (
                Tool::Selftest,
                "selftest.py",
                vec!["--norn", "/opt/norn", "--manifest", "-x", "a b"],
            ),
        ] {
            let command = text(tool);
            assert_eq!(command[0], "-I");
            assert!(command[1].ends_with(script), "{command:?}");
            assert_eq!(command[2..], rest);
        }
        assert!(arguments(Tool::Selftest, &home, None, &[]).is_err());
        assert!(arguments(Tool::Parity, Path::new("/nonexistent"), Some(binary), &[]).is_err());
    }
}
