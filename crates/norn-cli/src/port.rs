//! `norn port`: forward to the porting tools in `tools/norn-port`.
//!
//! The CLI links none of that code. It resolves the tool's script, runs it
//! with the caller's arguments unchanged and returns its exit status. The
//! tools read source as data; see `tools/norn-port/README.md` for what each
//! one proves and what it does not.
use clap::{Args, ValueEnum};
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command;

/// Directory holding the tools, when it is not `tools/norn-port` under the
/// working directory.
const HOME: &str = "NORN_PORT_HOME";
/// Interpreter to run the tools with, when it is not `python3` on `PATH`.
const PYTHON: &str = "NORN_PORT_PYTHON";

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
    /// Compare an original function with its Norn candidate on generated inputs.
    Parity,
    /// Count inventoried, translated and measured code units.
    Scoreboard,
    /// Emit `norn.code-ontology/1` facts for source directories.
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
}

/// The interpreter, the script and every argument after it, in order.
fn invocation(
    tool: Tool,
    home: Option<OsString>,
    python: Option<OsString>,
    own_path: Option<PathBuf>,
    args: &[OsString],
) -> Result<(OsString, Vec<OsString>), String> {
    let home = home.map_or_else(|| PathBuf::from("tools/norn-port"), PathBuf::from);
    let (script, selector) = tool.script();
    let script = home.join(script);
    if !script.is_file() {
        return Err(format!(
            "{} is not a file; run from a Norn checkout or set {HOME} to the norn-port directory",
            script.display()
        ));
    }
    let mut command = vec![script.into_os_string()];
    command.extend(selector.iter().map(OsString::from));
    if tool == Tool::Selftest {
        let own_path = own_path.ok_or("the running binary's path is unknown")?;
        command.push("--norn".into());
        command.push(own_path.into_os_string());
    }
    command.extend(args.iter().cloned());
    Ok((python.unwrap_or_else(|| "python3".into()), command))
}

/// Run the tool with inherited standard streams. `true` when it exits zero.
pub(super) fn execute(args: &PortArgs) -> Result<(String, bool), String> {
    let (python, command) = invocation(
        args.tool,
        std::env::var_os(HOME),
        std::env::var_os(PYTHON),
        std::env::current_exe().ok(),
        &args.args,
    )?;
    #[allow(
        clippy::disallowed_methods,
        reason = "this command's one capability: running the named porting tool"
    )]
    let status = Command::new(&python)
        .args(&command)
        .status()
        .map_err(|error| format!("{}: {error}", python.to_string_lossy()))?;
    Ok((String::new(), status.success()))
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "forwarding contract assertions"
)]
mod tests {
    //! The forwarding contract: which script runs, with which arguments.
    use super::{Tool, invocation};
    use std::ffi::OsString;
    use std::path::PathBuf;

    /// The repository's own tool directory.
    fn home() -> OsString {
        concat!(env!("CARGO_MANIFEST_DIR"), "/../../tools/norn-port").into()
    }

    fn strings(values: &[OsString]) -> Vec<String> {
        values
            .iter()
            .map(|value| value.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn every_tool_resolves_a_committed_script_and_keeps_arguments_in_order() {
        let passed: Vec<OsString> = vec!["--manifest".into(), "-x".into(), "a b".into()];
        for (tool, script, selector) in [
            (Tool::Parity, "port.py", Some("parity")),
            (Tool::Scoreboard, "port.py", Some("scoreboard")),
            (Tool::CodeOntology, "code_ontology.py", None),
        ] {
            let (python, command) = invocation(tool, Some(home()), None, None, &passed).unwrap();
            assert_eq!(python, "python3");
            let command = strings(&command);
            assert!(command[0].ends_with(script), "{command:?}");
            let rest: Vec<&str> = command[1..].iter().map(String::as_str).collect();
            let mut expected: Vec<&str> = selector.into_iter().collect();
            expected.extend(["--manifest", "-x", "a b"]);
            assert_eq!(rest, expected);
        }
    }

    #[test]
    fn selftest_names_the_running_binary_and_honours_the_interpreter_override() {
        let own = PathBuf::from("/opt/norn");
        let (python, command) = invocation(
            Tool::Selftest,
            Some(home()),
            Some("py".into()),
            Some(own),
            &[],
        )
        .unwrap();
        assert_eq!(python, "py");
        assert_eq!(strings(&command)[1..], ["--norn", "/opt/norn"]);
        assert!(invocation(Tool::Selftest, Some(home()), None, None, &[]).is_err());
    }

    #[test]
    fn a_missing_tool_directory_is_refused_before_anything_runs() {
        let error = invocation(Tool::Parity, Some("/nonexistent".into()), None, None, &[])
            .expect_err("no script there");
        assert!(error.contains("NORN_PORT_HOME"), "{error}");
    }
}
