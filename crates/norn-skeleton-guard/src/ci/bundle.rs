//! Deterministic bundle packing and sequential execution.
use serde::{Deserialize, Serialize};
use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

// Measured Boat task durations and cold test-build cost. Also used by the
// hosted planner in core.sh until that job can use a prebuilt runner.
const WEIGHTS: &[(&str, u32)] = &[
    ("test/heavy-1", 13),
    ("test/heavy-2", 244),
    ("test/heavy-3", 90),
    ("test/heavy-4", 35),
    ("test/heavy-5", 26),
    ("test/heavy-6", 57),
    ("test/rest-1", 348),
    ("test/rest-2", 348),
    ("test/rest-3", 348),
    ("phase/doctest", 48),
    ("phase/features", 791),
    ("phase/duckdb", 630),
    ("phase/wasm", 239),
    ("phase/clippy", 119),
    ("phase/doc", 88),
    ("phase/guards", 532),
    ("phase/integration-authoring", 1030),
    ("phase/integration-wasm", 707),
    ("phase/integration-clients", 170),
    ("phase/provenance", 13),
    ("phase/test-shards", 14),
    ("phase/browser", 596),
];
const COMPILE_SECONDS: u32 = 216;
const TARGET_SECONDS: u32 = 660;
const MAX_BUNDLES: usize = 3;

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum Mode {
    Full,
    Affected,
}

#[derive(Deserialize, Serialize)]
struct Plan {
    mode: Mode,
    shard: Vec<String>,
    phase: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    packages: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rest_shards: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
    #[serde(flatten)]
    extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct Bundle {
    id: String,
    tasks: Vec<String>,
}

#[derive(Clone, Copy)]
struct Task {
    name: &'static str,
    weight: u32,
}

impl Task {
    fn shares_build(self, all_packages: bool) -> bool {
        self.name.starts_with("test/") || (all_packages && self.name == "phase/test-shards")
    }
}

#[derive(Default)]
struct Bin {
    tasks: Vec<Task>,
}

impl Bin {
    fn added_cost(&self, task: Task) -> u32 {
        if task.shares_build(true) && !self.tasks.iter().any(|task| task.shares_build(true)) {
            task.weight.saturating_add(COMPILE_SECONDS)
        } else {
            task.weight
        }
    }

    fn cost(&self) -> u32 {
        self.tasks
            .iter()
            .map(|task| task.weight)
            .sum::<u32>()
            .saturating_add(if self.tasks.iter().any(|task| task.shares_build(true)) {
                COMPILE_SECONDS
            } else {
                0
            })
    }
}

fn pack(plan: &Plan) -> Result<Vec<Bundle>, String> {
    let known: BTreeMap<_, _> = WEIGHTS
        .iter()
        .map(|&(name, weight)| (name, Task { name, weight }))
        .collect();
    let mut seen = BTreeSet::new();
    let mut tasks = Vec::new();
    for (prefix, names) in [("test", &plan.shard), ("phase", &plan.phase)] {
        for name in names {
            let key = format!("{prefix}/{name}");
            let task = known
                .get(key.as_str())
                .ok_or_else(|| format!("unknown planned task {key}"))?;
            if !seen.insert(task.name) {
                return Err(format!("duplicate planned task {key}"));
            }
            tasks.push(*task);
        }
    }
    if matches!(plan.mode, Mode::Full) && tasks.len() != known.len() {
        return Err("full bundle plan omitted a registered task".into());
    }
    if tasks.is_empty() {
        return Ok(Vec::new());
    }
    tasks.sort_by_key(|task| (Reverse(Bin::default().added_cost(*task)), task.name));
    let limit = MAX_BUNDLES.min((tasks.len() / 2).max(1));
    let mut best = Vec::new();
    let mut best_cost = u32::MAX;
    for count in 1..=limit {
        let bins = pack_count(&tasks, count)?;
        let cost = bins
            .iter()
            .map(Bin::cost)
            .max()
            .unwrap_or(0)
            .max(TARGET_SECONDS);
        if cost < best_cost {
            best = bins;
            best_cost = cost;
        }
    }
    let bundles: Vec<_> = best
        .into_iter()
        .enumerate()
        .filter(|(_, bin)| !bin.tasks.is_empty())
        .map(|(index, bin)| Bundle {
            id: format!("b{index}"),
            tasks: bin
                .tasks
                .into_iter()
                .map(|task| task.name.to_owned())
                .collect(),
        })
        .collect();
    let mut packed: Vec<_> = bundles
        .iter()
        .flat_map(|bundle| bundle.tasks.iter().map(String::as_str))
        .collect();
    packed.sort_unstable();
    if !packed.into_iter().eq(seen) {
        return Err("bundle inventory dropped or duplicated a planned task".into());
    }
    Ok(bundles)
}

fn pack_count(tasks: &[Task], count: usize) -> Result<Vec<Bin>, String> {
    let mut bins: Vec<_> = (0..count).map(|_| Bin::default()).collect();
    for &task in tasks {
        let bin = bins
            .iter_mut()
            .min_by_key(|bin| bin.cost().saturating_add(bin.added_cost(task)))
            .ok_or("no bundle for task")?;
        bin.tasks.push(task);
    }
    if tasks.len() / 2 >= count {
        for index in 0..count {
            let recipient = bins.get(index).ok_or("missing recipient bundle")?;
            if recipient.tasks.len() != 1 {
                continue;
            }
            let (donor, _, task) = bins
                .iter()
                .enumerate()
                .filter(|(_, bin)| bin.tasks.len() > 2)
                .flat_map(|(index, bin)| {
                    bin.tasks.iter().map(move |&task| (index, bin.cost(), task))
                })
                .min_by_key(|&(donor, cost, task)| {
                    (recipient.added_cost(task), Reverse(cost), task.name, donor)
                })
                .ok_or("no donor for singleton bundle")?;
            bins.get_mut(donor)
                .ok_or("missing donor bundle")?
                .tasks
                .retain(|item| item.name != task.name);
            bins.get_mut(index)
                .ok_or("missing recipient bundle")?
                .tasks
                .push(task);
        }
    }
    Ok(bins)
}

struct PlanFormatter;

impl serde_json::ser::Formatter for PlanFormatter {
    fn begin_array_value<W: ?Sized + Write>(
        &mut self,
        writer: &mut W,
        first: bool,
    ) -> io::Result<()> {
        if first {
            Ok(())
        } else {
            writer.write_all(b", ")
        }
    }

    fn begin_object_key<W: ?Sized + Write>(
        &mut self,
        writer: &mut W,
        first: bool,
    ) -> io::Result<()> {
        if first {
            Ok(())
        } else {
            writer.write_all(b", ")
        }
    }

    fn begin_object_value<W: ?Sized + Write>(&mut self, writer: &mut W) -> io::Result<()> {
        writer.write_all(b": ")
    }
}

#[derive(Serialize)]
struct Packed<'a> {
    #[serde(flatten)]
    plan: &'a Plan,
    bundle: &'a [Bundle],
}

/// Print a deterministic packed plan.
///
/// # Errors
/// Returns an error for a malformed plan or failed write.
pub fn print_plan(raw: &str) -> Result<(), String> {
    write_plan(raw, io::stdout().lock())
}

fn write_plan(raw: &str, mut out: impl Write) -> Result<(), String> {
    let mut plan: Plan = serde_json::from_str(raw).map_err(|error| error.to_string())?;
    let bundles = pack(&plan)?;
    plan.extra.remove("bundle");
    Packed {
        plan: &plan,
        bundle: &bundles,
    }
    .serialize(&mut serde_json::Serializer::with_formatter(
        &mut out,
        PlanFormatter,
    ))
    .map_err(|error| error.to_string())?;
    writeln!(out).map_err(|error| error.to_string())
}

fn checked(raw: &str) -> Result<Vec<Bundle>, String> {
    let mut plan: Plan = serde_json::from_str(raw).map_err(|error| error.to_string())?;
    let expected = pack(&plan)?;
    let supplied: Vec<Bundle> = serde_json::from_value(
        plan.extra
            .remove("bundle")
            .ok_or("missing bundle inventory")?,
    )
    .map_err(|error| error.to_string())?;
    if supplied != expected {
        return Err("bundle inventory differs from the deterministic plan".into());
    }
    Ok(expected)
}

/// A rejected bundle or a failed phase.
#[derive(Debug)]
pub struct Failure {
    /// Exit code reported by the phase, or 1 if bundle validation or setup failed.
    pub code: u8,
    /// Diagnostic for the failing task or validation check.
    pub message: String,
}

impl From<String> for Failure {
    fn from(message: String) -> Self {
        Self { code: 1, message }
    }
}

impl From<&str> for Failure {
    fn from(message: &str) -> Self {
        message.to_owned().into()
    }
}

/// Validate the exact bundle inventory, and run the selected bundle if given.
///
/// # Errors
/// Returns an error for an invalid plan, unknown bundle, or failed task.
pub fn run(raw: &str, selected: Option<&str>) -> Result<(), Failure> {
    let bundles = checked(raw)?;
    if let Some(id) = selected {
        let bundle = bundles
            .iter()
            .find(|bundle| bundle.id == id)
            .ok_or("unknown bundle identity")?;
        let root = std::env::current_dir().map_err(|error| error.to_string())?;
        let target = std::env::var_os("CARGO_TARGET_DIR")
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| root.join("target"));
        let all_packages =
            std::env::var_os("NORN_CI_PACKAGES").is_none_or(|value| value.is_empty());
        execute(bundle, all_packages, |task, prune| {
            if prune {
                prune_target(&root, &target).map_err(|error| format!("prune target: {error}"))?;
            }
            run_task(task)
        })?;
    }
    Ok(())
}

fn execute(
    bundle: &Bundle,
    all_packages: bool,
    mut run: impl FnMut(Task, bool) -> Result<(), Failure>,
) -> Result<(), Failure> {
    let mut tasks = Vec::new();
    for name in &bundle.tasks {
        let &(name, weight) = WEIGHTS
            .iter()
            .find(|(known, _)| known == name)
            .ok_or("unknown bundle task")?;
        tasks.push(Task { name, weight });
    }
    tasks.sort_by_key(|task| !task.shares_build(all_packages));
    let mut previous_shared = None;
    for task in tasks {
        let shared = task.shares_build(all_packages);
        let prune = previous_shared.is_some_and(|previous| !(previous && shared));
        run(task, prune)?;
        previous_shared = Some(shared);
    }
    Ok(())
}

fn run_task(task: Task) -> Result<(), Failure> {
    let mut command = Command::new("bash");
    command.arg("scripts/ci/core.sh");
    if let Some(shard) = task.name.strip_prefix("test/") {
        command.args(["test", shard]);
    } else if let Some(phase) = task.name.strip_prefix("phase/") {
        command.arg(phase);
    } else {
        return Err(format!("unknown task {}", task.name).into());
    }
    let stdout = io::stdout();
    let _group = writeln!(stdout.lock(), "::group::{}", task.name);
    let start = Instant::now();
    let status = command.status();
    let success = status.as_ref().is_ok_and(std::process::ExitStatus::success);
    let result = if success { "success" } else { "failure" };
    let _notice = writeln!(
        stdout.lock(),
        "::notice::task={} result={} seconds={}\n::endgroup::",
        task.name,
        result,
        start.elapsed().as_secs()
    );
    match status {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(Failure {
            code: status
                .code()
                .and_then(|code| u8::try_from(code).ok())
                .unwrap_or(1),
            message: format!("{} failed: {status}", task.name),
        }),
        Err(error) => Err(format!("{} failed: {error}", task.name).into()),
    }
}

fn visible(name: &std::ffi::OsStr) -> bool {
    !name.as_encoded_bytes().starts_with(b".")
}

fn prune_target(root: &Path, target: &Path) -> io::Result<()> {
    let workspace_target = root.canonicalize()?.join("target");
    // Match the shell's realpath -m behavior for missing components as well
    // as links and parent references.
    let output = Command::new("realpath")
        .args(["-zm", "--"])
        .arg(target)
        .current_dir(root)
        .output()?;
    let resolved = workspace_target.as_os_str().as_encoded_bytes();
    if !output.status.success() || output.stdout.strip_suffix(&[0]) != Some(resolved) {
        let _warning = writeln!(
            io::stdout().lock(),
            "::warning::refusing to prune {}: only {} may be pruned",
            target.display(),
            workspace_target.display()
        );
        return Ok(());
    }
    let entries = match fs::read_dir(&workspace_target) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        if !entry.file_type()?.is_dir()
            || !visible(&name)
            || name.as_encoded_bytes().starts_with(b"verification-")
        {
            continue;
        }
        let path = entry.path();
        if name == "doc" || path.join(".fingerprint").is_dir() || has_nested_fingerprint(&path)? {
            fs::remove_dir_all(path)?;
        }
    }
    Ok(())
}

fn has_nested_fingerprint(path: &Path) -> io::Result<bool> {
    for child in fs::read_dir(path)? {
        let child = child?;
        if visible(&child.file_name()) && child.path().join(".fingerprint").exists() {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "test fixtures and their setup must succeed"
)]
mod tests {
    use super::*;

    // Byte-for-byte outputs from core.sh at 1437d52c4c: full, norn-api
    // (ce4ea784d^..ce4ea784d), and norn-cell (1c894feba^..1c894feba).
    const PYTHON_PLANS: &[&str] = &[
        concat!(
            r#"{"mode": "full", "shard": ["heavy-1", "heavy-2", "heavy-3", "heavy-4", "heavy-5", "heavy-6", "rest-1", "rest-2", "rest-3"], "phase": ["doctest", "features", "duckdb", "wasm", "clippy", "doc", "guards", "integration-authoring", "integration-wasm", "integration-clients", "browser", "provenance", "test-shards"], "packages": "", "rest_shards": 3, "reason": "complete suite", "bundle": [{"id": "b0", "tasks": ["phase/integration-authoring", "test/rest-1", "test/rest-3", "test/heavy-3", "test/heavy-6", "test/heavy-4", "test/heavy-5", "test/heavy-1", "phase/doc", "phase/doctest"]}, {"id": "b1", "tasks": ["phase/features", "phase/browser", "phase/guards", "phase/wasm", "phase/integration-clients"]}, {"id": "b2", "tasks": ["phase/integration-wasm", "phase/duckdb", "test/rest-2", "test/heavy-2", "phase/test-shards", "phase/clippy", "phase/provenance"]}]}"#,
            "\n"
        ),
        concat!(
            r#"{"mode": "affected", "shard": ["rest-1"], "phase": ["guards", "provenance", "clippy"], "packages": "norn-api norn-cell-do norn-cli", "rest_shards": 1, "reason": "changed: norn-api", "bundle": [{"id": "b0", "tasks": ["test/rest-1", "phase/provenance"]}, {"id": "b1", "tasks": ["phase/guards", "phase/clippy"]}]}"#,
            "\n"
        ),
        concat!(
            r#"{"mode": "affected", "shard": ["heavy-2", "heavy-3", "heavy-4", "heavy-5", "heavy-6", "rest-1", "rest-2", "rest-3"], "phase": ["guards", "provenance", "clippy", "duckdb"], "packages": "norn-api norn-builtins norn-cell norn-cell-do norn-cli norn-kernel norn-testkit", "rest_shards": 3, "reason": "changed: norn-cell", "bundle": [{"id": "b0", "tasks": ["phase/duckdb", "test/heavy-2", "test/heavy-5"]}, {"id": "b1", "tasks": ["test/rest-1", "test/rest-3", "test/heavy-3", "test/heavy-6", "test/heavy-4", "phase/clippy"]}, {"id": "b2", "tasks": ["test/rest-2", "phase/guards", "phase/provenance"]}]}"#,
            "\n"
        ),
    ];

    #[test]
    fn matches_python_on_real_plans() {
        for expected in PYTHON_PLANS {
            let (raw, _) = expected
                .split_once(", \"bundle\":")
                .expect("Python bundle field");
            let mut actual = Vec::new();
            write_plan(&format!("{raw}}}"), &mut actual).expect("pack plan");
            assert_eq!(actual, expected.as_bytes());
            assert!(checked(expected).is_ok());
        }
    }

    #[test]
    fn refuses_wrong_inventories_and_ids() {
        for raw in [
            r#"{"mode":"affected","shard":[],"phase":["unknown"],"bundle":[]}"#,
            r#"{"mode":"affected","shard":["rest-1","rest-1"],"phase":[],"bundle":[]}"#,
            r#"{"mode":"full","shard":[],"phase":[],"bundle":[]}"#,
            r#"{"mode":"affected","shard":[],"phase":["doc"]}"#,
            r#"{"mode":"affected","shard":[],"phase":["doc"],"bundle":[{"id":"b0","tasks":[]}]}"#,
            r#"{"mode":"affected","shard":[],"phase":["doc"],"bundle":[{"id":"b0","tasks":["phase/doc","phase/doc"]}]}"#,
            r#"{"mode":"affected","shard":[],"phase":["doc"],"bundle":[{"id":"b0","tasks":["phase/doc"]},{"id":"b0","tasks":["phase/doc"]}]}"#,
        ] {
            assert!(checked(raw).is_err(), "{raw}");
        }
        let raw = r#"{"mode":"affected","shard":[],"phase":["doc"],"bundle":[{"id":"b0","tasks":["phase/doc"]}]}"#;
        assert!(run(raw, Some("missing")).is_err());
    }

    #[test]
    fn groups_shared_builds_and_prunes_every_other_boundary() {
        let bundle = Bundle {
            id: "b0".into(),
            tasks: [
                "phase/clippy",
                "test/rest-2",
                "phase/test-shards",
                "test/rest-1",
                "phase/doc",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        };
        for (all_packages, expected) in [
            (
                true,
                [
                    ("test/rest-2", false),
                    ("phase/test-shards", false),
                    ("test/rest-1", false),
                    ("phase/clippy", true),
                    ("phase/doc", true),
                ],
            ),
            (
                false,
                [
                    ("test/rest-2", false),
                    ("test/rest-1", false),
                    ("phase/clippy", true),
                    ("phase/test-shards", true),
                    ("phase/doc", true),
                ],
            ),
        ] {
            let mut calls = Vec::new();
            execute(&bundle, all_packages, |task, prune| {
                calls.push((task.name, prune));
                Ok(())
            })
            .expect("execute");
            assert_eq!(calls, expected);
        }
        let mut calls = Vec::new();
        let failure = execute(&bundle, true, |task, _| {
            calls.push(task.name);
            Err("phase failed".into())
        })
        .expect_err("failed phase aborts");
        assert_eq!(failure.message, "phase failed");
        assert_eq!(calls, ["test/rest-2"]);
    }

    #[cfg(unix)]
    #[test]
    fn prunes_only_workspace_cargo_outputs() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().expect("scratch workspace");
        let home = tmp.path().join("home");
        let outside = tmp.path().join("outside");
        let root = home.join("work/norn");
        let target = root.join("target");
        let kept = [
            home.join(".cargo"),
            outside.join(".fingerprint"),
            root.join("src"),
            target.join("verification-results"),
            target.join("verification-tools/release/.fingerprint"),
            target.join("norn-checker-wasm/pkg"),
            target.join(".hidden/.fingerprint"),
        ];
        for dir in &kept {
            fs::create_dir_all(dir).expect("create preserved directory");
        }
        symlink(&outside, root.join("escape")).expect("link to outside");
        symlink(&outside, target.join("linked")).expect("link from target");
        let cases = [
            (target.clone(), true),
            (PathBuf::from("target"), true),
            (PathBuf::from("target/"), true),
            (PathBuf::from("target/../target"), true),
            (PathBuf::from("/"), false),
            (home, false),
            (root.clone(), false),
            (root.join(".."), false),
            (PathBuf::from("."), false),
            (PathBuf::from(".."), false),
            (PathBuf::from("../sibling"), false),
            (outside.clone(), false),
            (PathBuf::from("escape"), false),
            (PathBuf::from("target/debug"), false),
            (PathBuf::from("src/../../norn"), false),
        ];
        for (configured, deletes) in cases {
            for dir in [
                "debug/.fingerprint",
                "wasm32-unknown-unknown/debug/.fingerprint",
                "doc",
            ] {
                fs::create_dir_all(target.join(dir)).expect("create Cargo output");
            }
            prune_target(&root, &configured).expect("prune");
            for dir in ["debug", "wasm32-unknown-unknown", "doc"] {
                assert_eq!(target.join(dir).exists(), !deletes, "{configured:?}: {dir}");
            }
            assert!(kept.iter().all(|path| path.is_dir()), "{configured:?}");
            assert!(target.join("linked").is_symlink(), "{configured:?}");
        }
        fs::create_dir_all(outside.join("doc")).expect("outside output");
        fs::remove_dir_all(&target).expect("replace target with symlink");
        symlink(&outside, &target).expect("target symlink to outside");
        prune_target(&root, &target).expect("refuse symlink");
        assert!(outside.join("doc").is_dir());
        assert!(target.is_symlink());
    }
}
