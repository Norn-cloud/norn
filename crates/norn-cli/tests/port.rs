//! `norn port` against real processes: which script runs, under which
//! interpreter flags and environment, and what status comes back.
//!
//! The tool directory here is a stand-in that reports how it was started, so
//! the tests need `python3` on `PATH` and nothing else.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::disallowed_methods,
    missing_docs,
    reason = "process-level contract assertions spawn the built binary"
)]

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Reports its own start-up state, or exits the way the arguments ask.
const REPORTER: &str = r#"
import json, os, sys
if "--exit" in sys.argv:
    sys.exit(int(sys.argv[sys.argv.index("--exit") + 1]))
if "--signal" in sys.argv:
    os.kill(os.getpid(), 15)
print(json.dumps({"argv": sys.argv[1:], "isolated": sys.flags.isolated, "path": sys.path,
                  "cwd": os.getcwd(), "env": sorted(os.environ)}))
"#;

/// Leaves a marker beside the decoy tree's root if it ever runs.
const DECOY: &str = r#"
import pathlib
root = pathlib.Path(__file__).resolve()
while not (root / "DECOY-ROOT").exists():
    root = root.parent
(root / ("EXECUTED-" + pathlib.Path(__file__).name)).write_text("ran")
"#;

/// A fresh directory, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("norn-port-test-{}-{name}", std::process::id()));
        let _stale = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }

    fn write(&self, relative: &str, text: &str) {
        let path = self.0.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _gone = std::fs::remove_dir_all(&self.0);
    }
}

/// A tool directory whose every tool is the reporter.
fn reporter_home(name: &str) -> Scratch {
    let home = Scratch::new(name);
    for script in ["port.py", "code_ontology.py", "selftest.py"] {
        home.write(script, REPORTER);
    }
    home
}

/// A directory to analyse that tries every way to get its own code run: the
/// tool's path under the working directory, and Python start-up hooks.
fn hostile_tree(name: &str) -> Scratch {
    let tree = Scratch::new(name);
    tree.write("DECOY-ROOT", "");
    for script in [
        "tools/norn-port/port.py",
        "tools/norn-port/code_ontology.py",
        "tools/norn-port/selftest.py",
        "sitecustomize.py",
        "usercustomize.py",
        "startup.py",
        "json.py",
    ] {
        tree.write(script, DECOY);
    }
    tree
}

fn executed(tree: &Scratch) -> Vec<String> {
    std::fs::read_dir(&tree.0)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("EXECUTED"))
        .collect()
}

fn norn(working: &Path, home: Option<&Path>, arguments: &[&str]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_norn"));
    command
        .current_dir(working)
        .arg("port")
        .args(arguments)
        .env_remove("NORN_PORT_HOME")
        .env_remove("NORN_PORT_PYTHON")
        .env("PYTHONPATH", working)
        .env("PYTHONSTARTUP", working.join("startup.py"))
        .env("NORN_TEST_SECRET", "must not reach the tool");
    if let Some(home) = home {
        command.env("NORN_PORT_HOME", home);
    }
    command.output().unwrap()
}

fn report(output: &Output) -> serde_json::Value {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn the_analysed_tree_is_data_and_cannot_supply_the_tool_or_python_start_up() {
    let home = reporter_home("home");
    let tree = hostile_tree("tree");
    let state = report(&norn(
        &tree.0,
        Some(&home.0),
        &["code-ontology", ".", "--flag"],
    ));
    // The installation's tool ran, with the caller's arguments unchanged.
    assert_eq!(state["argv"], serde_json::json!([".", "--flag"]));
    assert_eq!(state["cwd"].as_str(), tree.0.to_str());
    // Isolated mode: neither the working directory nor PYTHONPATH is importable.
    assert_eq!(state["isolated"], 1);
    let path: Vec<&str> = state["path"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry.as_str().unwrap())
        .collect();
    assert!(!path.contains(&""), "{path:?}");
    assert!(
        !path.iter().any(|entry| Path::new(entry) == tree.0),
        "{path:?}"
    );
    // Only the short fixed environment arrives.
    let variables: Vec<&str> = state["env"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry.as_str().unwrap())
        .collect();
    for dropped in [
        "PYTHONPATH",
        "PYTHONSTARTUP",
        "NORN_TEST_SECRET",
        "NORN_PORT_HOME",
        "PATH",
        "HOME",
    ] {
        assert!(!variables.contains(&dropped), "{dropped} reached the tool");
    }
    assert_eq!(executed(&tree), Vec::<String>::new());
}

#[test]
fn without_an_explicit_home_the_working_directory_is_still_never_searched() {
    let tree = hostile_tree("cwd");
    // Whether or not this build sits in a checkout that has the real tools,
    // the copy under the working directory must not run.
    for tool in ["scoreboard", "code-ontology"] {
        let _output = norn(&tree.0, None, &[tool]);
    }
    assert_eq!(executed(&tree), Vec::<String>::new());
}

#[test]
fn a_relative_home_is_refused_before_anything_runs() {
    let tree = hostile_tree("relative");
    let output = norn(&tree.0, Some(Path::new("tools/norn-port")), &["scoreboard"]);
    assert_eq!(output.status.code(), Some(1));
    let message = String::from_utf8_lossy(&output.stderr);
    assert!(message.contains("absolute path"), "{message}");
    assert_eq!(executed(&tree), Vec::<String>::new());
}

#[test]
fn the_exit_status_is_the_tools_own() {
    let home = reporter_home("status");
    let exited = norn(&home.0, Some(&home.0), &["scoreboard", "--exit", "7"]);
    assert_eq!(exited.status.code(), Some(7));
    #[cfg(unix)]
    {
        // SIGTERM is 15; a signal is reported as 128 plus its number.
        let killed = norn(&home.0, Some(&home.0), &["scoreboard", "--signal"]);
        assert_eq!(killed.status.code(), Some(143));
    }
}

#[test]
fn parity_and_selftest_are_handed_this_binary() {
    let home = reporter_home("binary");
    let binary = Path::new(env!("CARGO_BIN_EXE_norn"))
        .canonicalize()
        .unwrap();
    let binary = binary.to_string_lossy();
    let parity = report(&norn(&home.0, Some(&home.0), &["parity", "--defs", "x"]));
    assert_eq!(
        parity["argv"],
        serde_json::json!(["parity", "--norn", binary, "--defs", "x"])
    );
    let selftest = report(&norn(&home.0, Some(&home.0), &["selftest"]));
    assert_eq!(selftest["argv"], serde_json::json!(["--norn", binary]));
    let scoreboard = report(&norn(&home.0, Some(&home.0), &["scoreboard"]));
    assert_eq!(scoreboard["argv"], serde_json::json!(["scoreboard"]));
}
