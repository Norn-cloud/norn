//! `norn port`: forward to the porting tools shipped with this binary.
//!
//! The CLI links none of that code. It finds the tool directory from the Norn
//! installation, never from the working directory or the tree being analysed,
//! runs the tool with an isolated interpreter and a short explicit
//! environment, and exits with the tool's own status.
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

/// The tool directory: `explicit` when given, otherwise a fixed place
/// relative to the running binary. The working directory is never consulted.
fn home(explicit: Option<OsString>, binary: Option<&Path>) -> Result<PathBuf, String> {
    if let Some(explicit) = explicit {
        let explicit = PathBuf::from(explicit);
        if !explicit.is_absolute() {
            return Err(format!(
                "{HOME} must be an absolute path, not {}",
                explicit.display()
            ));
        }
        return Ok(explicit);
    }
    let prefix = binary
        .and_then(Path::parent)
        .and_then(Path::parent)
        .ok_or("the running binary's location is unknown")?;
    // An installed `<prefix>/bin/norn` keeps the tools in `<prefix>/share`.
    let installed = prefix.join("share/norn/port");
    if installed.join("port.py").is_file() {
        return Ok(installed);
    }
    // A Cargo build at `<checkout>/target/<profile>/norn`.
    let checkout = prefix.parent().map(|root| root.join("tools/norn-port"));
    if let Some(checkout) = checkout
        && checkout.join("port.py").is_file()
    {
        return Ok(checkout);
    }
    Err(format!(
        "the porting tools are not installed with this binary (no {}); set {HOME} to the absolute path of tools/norn-port in a Norn checkout",
        installed.display()
    ))
}

/// The interpreter: `explicit` when given, otherwise the first `python3` in
/// an absolute `PATH` entry. A relative entry would resolve against the
/// working directory, so it is skipped.
fn interpreter(explicit: Option<OsString>, path: Option<&OsStr>) -> Result<PathBuf, String> {
    if let Some(explicit) = explicit {
        let explicit = PathBuf::from(explicit);
        if !explicit.is_absolute() {
            return Err(format!(
                "{PYTHON} must be an absolute path, not {}",
                explicit.display()
            ));
        }
        return Ok(explicit);
    }
    path.into_iter()
        .flat_map(|path| std::env::split_paths(path))
        .filter(|directory| directory.is_absolute())
        .map(|directory| directory.join("python3"))
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| {
            format!(
                "no python3 in an absolute PATH entry; set {PYTHON} to an interpreter's absolute path"
            )
        })
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
    let binary = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .ok();
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
    fn without_an_explicit_home_only_the_binary_location_is_searched() {
        // A Cargo build inside this checkout finds the checkout's tools.
        let built = root().join("target/debug/norn");
        let found = home(None, Some(&built)).expect("checkout layout");
        assert_eq!(found, root().join("tools/norn-port"));
        // A binary anywhere else finds nothing, whatever the working directory holds.
        let error = home(None, Some(Path::new("/opt/elsewhere/bin/norn"))).expect_err("no tools");
        assert!(error.contains("NORN_PORT_HOME"), "{error}");
        assert!(home(None, None).is_err());
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
