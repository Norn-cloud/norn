//! Deterministic bundle packing and sequential execution.
use serde::{Deserialize, Serialize};
use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

/// Every task a plan can select, in the planner's order.
const TASKS: [&str; 29] = [
    "test/heavy-1",
    "test/heavy-2",
    "test/heavy-3",
    "test/heavy-4",
    "test/heavy-5",
    "test/heavy-6",
    "test/rest-1",
    "test/rest-2",
    "test/rest-3",
    "test/rest-4",
    "test/rest-5",
    "test/rest-6",
    "test/rest-7",
    "test/rest-8",
    "phase/doctest",
    "phase/features",
    "phase/duckdb",
    "phase/wasm",
    "phase/clippy",
    "phase/doc",
    "phase/guards",
    "phase/rivet",
    "phase/integration-authoring",
    "phase/integration-wasm",
    "phase/integration-clients",
    "phase/provenance",
    "phase/test-shards",
    "phase/browser-1",
    "phase/browser-2",
];
/// Measured task seconds and shared build costs, generated from green merge-group bundle logs
/// by `scripts/bundle_durations.py`. Any newly-split tasks whose weights are temporary estimates
/// pending post-merge regeneration must be listed explicitly in `estimated_tasks`. The hosted
/// planner in core.sh reads the same file.
const DURATIONS: &str = include_str!("bundle-durations.json");
const TARGET_SECONDS: u32 = 300;
const MAX_BUNDLES: usize = 10;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Durations {
    generated_by: String,
    runs: Vec<u64>,
    statistic: String,
    test_build_seconds: u32,
    wasm_pkg_seconds: u32,
    #[serde(default)]
    estimated_tasks: BTreeSet<String>,
    task_seconds: BTreeMap<String, u32>,
}

/// The cost model: each task's own seconds and the builds a bundle shares.
struct Model {
    tasks: BTreeMap<&'static str, Task>,
    test_build: u32,
    wasm_pkg: u32,
}

impl Model {
    fn load() -> Result<Self, String> {
        Self::parse(DURATIONS)
    }

    /// The model of a duration table: refused unless it names its measured runs, statistic
    /// and generator, and times exactly the planner's tasks.
    fn parse(raw: &str) -> Result<Self, String> {
        let table: Durations =
            serde_json::from_str(raw).map_err(|error| format!("bundle-durations.json: {error}"))?;
        if table.runs.is_empty() || table.statistic.is_empty() || table.generated_by.is_empty() {
            return Err("bundle-durations.json names no measured runs".into());
        }
        let mut tasks = BTreeMap::new();
        for name in TASKS {
            let &weight = table
                .task_seconds
                .get(name)
                .ok_or_else(|| format!("bundle-durations.json has no seconds for {name}"))?;
            tasks.insert(name, Task { name, weight });
        }
        if table.task_seconds.len() != tasks.len() {
            return Err("bundle-durations.json times a task the planner does not know".into());
        }
        if let Some(unknown) = table
            .estimated_tasks
            .iter()
            .find(|name| !tasks.contains_key(name.as_str()))
        {
            return Err(format!(
                "bundle-durations.json marks unknown task {unknown} as estimated"
            ));
        }
        Ok(Self {
            tasks,
            test_build: table.test_build_seconds,
            wasm_pkg: table.wasm_pkg_seconds,
        })
    }

    fn cost(&self, tasks: &[Task], all_packages: bool) -> u32 {
        let mut total = tasks
            .iter()
            .fold(0_u32, |acc, task| acc.saturating_add(task.weight));
        if tasks
            .iter()
            .any(|task| task.build_group(all_packages) == BuildGroup::Test)
        {
            total = total.saturating_add(self.test_build);
        }
        for task in tasks {
            if task.needs_own_test_build(all_packages) {
                total = total.saturating_add(self.test_build);
            }
        }
        if tasks.iter().any(|task| task.compiles_wasm_pkg()) {
            total = total.saturating_add(self.wasm_pkg);
        }
        total
    }
}

/// Post-merge single-bundle R2 cache warming phases (`core.sh plan --cache-warm`, #1951).
/// Each runs with `BuildGroup::None`, so `execute` prunes `target/` between every boundary.
const CACHE_WARM_PHASES: [&str; 9] = [
    "cache-warm-test",
    "cache-warm-guard",
    "cache-warm-clippy",
    "cache-warm-doc",
    "cache-warm-wasm",
    "cache-warm-wasm-pkg",
    "cache-warm-duckdb",
    "cache-warm-rivet",
    "cache-warm-features",
];
const CACHE_WARM_TASKS: [&str; 9] = [
    "phase/cache-warm-test",
    "phase/cache-warm-guard",
    "phase/cache-warm-clippy",
    "phase/cache-warm-doc",
    "phase/cache-warm-wasm",
    "phase/cache-warm-wasm-pkg",
    "phase/cache-warm-duckdb",
    "phase/cache-warm-rivet",
    "phase/cache-warm-features",
];

#[derive(Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum Mode {
    Full,
    Affected,
    CacheWarm,
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

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum BuildGroup {
    Test,
    WasmPkg,
    None,
}

#[derive(Clone, Copy)]
struct Task {
    name: &'static str,
    weight: u32,
}

impl Task {
    fn build_group(self, all_packages: bool) -> BuildGroup {
        if self.name.starts_with("test/")
            || (all_packages && matches!(self.name, "phase/test-shards" | "phase/guards"))
        {
            BuildGroup::Test
        } else if matches!(
            self.name,
            "phase/integration-authoring"
                | "phase/integration-wasm"
                | "phase/integration-clients"
                | "phase/browser-1"
                | "phase/browser-2"
        ) {
            BuildGroup::WasmPkg
        } else {
            BuildGroup::None
        }
    }

    fn needs_own_test_build(self, all_packages: bool) -> bool {
        !all_packages && matches!(self.name, "phase/test-shards" | "phase/guards")
    }

    fn compiles_wasm_pkg(self) -> bool {
        matches!(
            self.name,
            "phase/integration-authoring"
                | "phase/integration-wasm"
                | "phase/browser-1"
                | "phase/browser-2"
        )
    }
}

#[derive(Default)]
struct Bin {
    tasks: Vec<Task>,
}

impl Bin {
    fn added_cost(&self, model: &Model, task: Task, all_packages: bool) -> u32 {
        self.cost_with(model, &[task], all_packages)
            .saturating_sub(self.cost(model, all_packages))
    }

    fn cost_with(&self, model: &Model, extra: &[Task], all_packages: bool) -> u32 {
        let mut tasks = self.tasks.clone();
        tasks.extend_from_slice(extra);
        model.cost(&tasks, all_packages)
    }

    fn cost(&self, model: &Model, all_packages: bool) -> u32 {
        model.cost(&self.tasks, all_packages)
    }
}

fn pack(plan: &Plan) -> Result<Vec<Bundle>, String> {
    if plan.mode == Mode::CacheWarm {
        if !plan.shard.is_empty()
            || plan.phase != CACHE_WARM_PHASES
            || !plan.packages.as_deref().is_none_or(str::is_empty)
        {
            return Err("cache-warm plan must contain only the cache-warm phases".into());
        }
        return Ok(vec![Bundle {
            id: "b0".into(),
            tasks: CACHE_WARM_TASKS.into_iter().map(str::to_owned).collect(),
        }]);
    }
    let model = Model::load()?;
    let known = &model.tasks;
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
    let all_packages = plan.packages.as_deref().is_none_or(str::is_empty);
    tasks.sort_by_key(|task| {
        (
            Reverse(Bin::default().added_cost(&model, *task, all_packages)),
            task.name,
        )
    });
    let limit = MAX_BUNDLES.min((tasks.len() / 2).max(1));
    let mut best = Vec::new();
    let mut best_key = (u32::MAX, usize::MAX, u32::MAX);
    for count in 1..=limit {
        for coalesce_test in [false, true] {
            for coalesce_wasm in [false, true] {
                let Some(bins) = pack_count(
                    &model,
                    &tasks,
                    count,
                    coalesce_test,
                    coalesce_wasm,
                    all_packages,
                )?
                else {
                    continue;
                };
                let max_cost = bins
                    .iter()
                    .map(|bin| bin.cost(&model, all_packages))
                    .max()
                    .unwrap_or(0)
                    .max(TARGET_SECONDS);
                let total_cost = bins.iter().fold(0_u32, |acc, bin| {
                    acc.saturating_add(bin.cost(&model, all_packages))
                });
                let key = (max_cost, count, total_cost);
                if key < best_key {
                    best = bins;
                    best_key = key;
                }
            }
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

fn pack_count(
    model: &Model,
    tasks: &[Task],
    count: usize,
    coalesce_test: bool,
    coalesce_wasm: bool,
    all_packages: bool,
) -> Result<Option<Vec<Bin>>, String> {
    let mut test_group = Vec::new();
    let mut wasm_group = Vec::new();
    let mut other = Vec::new();
    for &task in tasks {
        match task.build_group(all_packages) {
            BuildGroup::Test => test_group.push(task),
            BuildGroup::WasmPkg => wasm_group.push(task),
            BuildGroup::None => other.push(task),
        }
    }
    let mut items: Vec<Vec<Task>> = Vec::new();
    if coalesce_test && !test_group.is_empty() {
        items.push(test_group);
    } else {
        items.extend(test_group.into_iter().map(|task| vec![task]));
    }
    if coalesce_wasm && !wasm_group.is_empty() {
        items.push(wasm_group);
    } else {
        items.extend(wasm_group.into_iter().map(|task| vec![task]));
    }
    items.extend(other.into_iter().map(|task| vec![task]));
    items.sort_by_key(|group| {
        (
            Reverse(model.cost(group, all_packages)),
            group.first().map_or("", |task| task.name),
        )
    });

    let mut bins: Vec<_> = (0..count).map(|_| Bin::default()).collect();
    for group in &items {
        let bin = bins
            .iter_mut()
            .min_by_key(|bin| bin.cost_with(model, group, all_packages))
            .ok_or("no bundle for task")?;
        bin.tasks.extend_from_slice(group);
    }
    if tasks.len() / 2 >= count {
        for index in 0..count {
            let recipient = bins.get(index).ok_or("missing recipient bundle")?;
            if recipient.tasks.len() != 1 {
                continue;
            }
            let Some((donor, _, task)) = bins
                .iter()
                .enumerate()
                .filter(|(_, bin)| bin.tasks.len() > 2)
                .flat_map(|(index, bin)| {
                    bin.tasks
                        .iter()
                        .map(move |&task| (index, bin.cost(model, all_packages), task))
                })
                .min_by_key(|&(donor, cost, task)| {
                    (
                        recipient.added_cost(model, task, all_packages),
                        Reverse(cost),
                        task.name,
                        donor,
                    )
                })
            else {
                return Ok(None);
            };
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
    if bins.iter().any(|bin| bin.tasks.is_empty()) {
        return Ok(None);
    }
    Ok(Some(bins))
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
            .map_or_else(|| root.join("target"), PathBuf::from);
        let all_packages =
            std::env::var_os("NORN_CI_PACKAGES").is_none_or(|value| value.is_empty());
        execute(bundle, all_packages, |task, prune| {
            if prune {
                prune_target(&root, &target).map_err(|error| format!("prune target: {error}"))?;
                report_prune(task);
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
    let model = Model::load()?;
    let mut tasks = Vec::new();
    for name in &bundle.tasks {
        let task = if let Some(&warm) = CACHE_WARM_TASKS.iter().find(|&&task| task == name) {
            Task {
                name: warm,
                weight: 0,
            }
        } else {
            *model
                .tasks
                .get(name.as_str())
                .ok_or("unknown bundle task")?
        };
        tasks.push(task);
    }
    tasks.sort_by_key(|task| task.build_group(all_packages));
    let mut previous_group = None;
    for task in tasks {
        let group = task.build_group(all_packages);
        let prune =
            previous_group.is_some_and(|previous| group == BuildGroup::None || previous != group);
        run(task, prune)?;
        previous_group = Some(group);
    }
    Ok(())
}

#[expect(
    clippy::disallowed_methods,
    reason = "the bundle executor dispatches fixed phase arguments"
)]
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

#[expect(
    clippy::disallowed_methods,
    reason = "df reports free space at the prune boundary"
)]
fn report_prune(task: Task) {
    let output = Command::new("df").args(["-Pk", "/"]).output();
    let free_space = output
        .as_ref()
        .ok()
        .map(|result| String::from_utf8_lossy(&result.stdout));
    let free_kib = free_space
        .as_deref()
        .and_then(|text| text.lines().nth(1))
        .and_then(|line| line.split_whitespace().nth(3))
        .unwrap_or_default();
    let _notice = writeln!(
        io::stdout().lock(),
        "::notice::pruned target before {}; {free_kib} KiB free",
        task.name
    );
}

fn visible(name: &std::ffi::OsStr) -> bool {
    !name.as_encoded_bytes().starts_with(b".")
}

#[expect(
    clippy::disallowed_methods,
    reason = "preserve GNU realpath semantics for missing path components"
)]
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
        if name == "doc"
            || name == "norn-checker-wasm"
            || path.join(".fingerprint").is_dir()
            || has_nested_fingerprint(&path)?
        {
            fs::remove_dir_all(path)?;
        }
    }
    Ok(())
}

fn has_nested_fingerprint(path: &Path) -> io::Result<bool> {
    for child in fs::read_dir(path)? {
        let child = child?;
        if visible(&child.file_name())
            && child.path().join(".fingerprint").symlink_metadata().is_ok()
        {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Byte-for-byte outputs from core.sh: full, norn-api, and norn-cell affected plans.
    const PYTHON_PLANS: &[&str] = &[
        concat!(
            r#"{"mode": "full", "shard": ["heavy-1", "heavy-2", "heavy-3", "heavy-4", "heavy-5", "heavy-6", "rest-1", "rest-2", "rest-3", "rest-4", "rest-5", "rest-6", "rest-7", "rest-8"], "phase": ["doctest", "features", "duckdb", "wasm", "clippy", "doc", "rivet", "guards", "integration-authoring", "integration-wasm", "integration-clients", "browser-1", "browser-2", "provenance", "test-shards"], "packages": "", "rest_shards": 8, "reason": "complete suite", "bundle": [{"id": "b0", "tasks": ["phase/integration-authoring", "phase/provenance"]}, {"id": "b1", "tasks": ["phase/integration-wasm", "phase/integration-clients", "phase/doctest"]}, {"id": "b2", "tasks": ["phase/guards", "test/rest-7"]}, {"id": "b3", "tasks": ["phase/browser-1", "phase/rivet", "phase/doc"]}, {"id": "b4", "tasks": ["phase/browser-2", "phase/wasm", "phase/clippy"]}, {"id": "b5", "tasks": ["phase/features", "phase/duckdb"]}, {"id": "b6", "tasks": ["test/rest-1", "test/rest-4", "test/rest-8"]}, {"id": "b7", "tasks": ["test/rest-2", "test/rest-5", "test/heavy-2", "test/heavy-5"]}, {"id": "b8", "tasks": ["test/rest-3", "test/rest-6", "test/heavy-3", "test/heavy-6", "test/heavy-1", "test/heavy-4", "phase/test-shards"]}]}"#,
            "\n"
        ),
        concat!(
            r#"{"mode": "affected", "shard": ["rest-1"], "phase": ["guards", "provenance", "clippy", "doc", "rivet"], "packages": "norn-api norn-cell-do norn-cli", "rest_shards": 1, "reason": "changed: norn-api", "bundle": [{"id": "b0", "tasks": ["phase/guards", "phase/provenance"]}, {"id": "b1", "tasks": ["test/rest-1", "phase/clippy"]}, {"id": "b2", "tasks": ["phase/rivet", "phase/doc"]}]}"#,
            "\n"
        ),
        concat!(
            r#"{"mode": "affected", "shard": ["heavy-2", "heavy-3", "heavy-4", "heavy-5", "heavy-6", "rest-1", "rest-2", "rest-3", "rest-4", "rest-5", "rest-6", "rest-7", "rest-8"], "phase": ["guards", "provenance", "clippy", "doc", "duckdb", "rivet"], "packages": "norn-api norn-builtins norn-cell norn-cell-do norn-cli norn-kernel norn-testkit", "rest_shards": 8, "reason": "changed: norn-cell", "bundle": [{"id": "b0", "tasks": ["phase/guards", "phase/rivet"]}, {"id": "b1", "tasks": ["test/rest-1", "test/rest-6", "test/heavy-6"]}, {"id": "b2", "tasks": ["test/rest-2", "test/rest-7", "test/heavy-4", "phase/provenance"]}, {"id": "b3", "tasks": ["test/rest-3", "test/rest-8", "test/heavy-5", "phase/clippy"]}, {"id": "b4", "tasks": ["test/rest-4", "test/heavy-2", "test/heavy-3", "phase/doc"]}, {"id": "b5", "tasks": ["test/rest-5", "phase/duckdb"]}]}"#,
            "\n"
        ),
        concat!(
            r#"{"mode": "cache-warm", "shard": [], "phase": ["cache-warm-test", "cache-warm-guard", "cache-warm-clippy", "cache-warm-doc", "cache-warm-wasm", "cache-warm-wasm-pkg", "cache-warm-duckdb", "cache-warm-rivet", "cache-warm-features"], "packages": "", "rest_shards": 1, "reason": "main push cache warm", "bundle": [{"id": "b0", "tasks": ["phase/cache-warm-test", "phase/cache-warm-guard", "phase/cache-warm-clippy", "phase/cache-warm-doc", "phase/cache-warm-wasm", "phase/cache-warm-wasm-pkg", "phase/cache-warm-duckdb", "phase/cache-warm-rivet", "phase/cache-warm-features"]}]}"#,
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

    /// Repacking never changes what runs: the inventory is the one CI ran before the measured
    /// duration table, and for the full plan, the pinned affected plans and a spread of other
    /// task subsets, the bundles hold exactly the planned tasks, each once, in at most
    /// `MAX_BUNDLES` non-empty bundles.
    #[test]
    fn packing_runs_every_planned_task_exactly_once() {
        const BEFORE: [&str; 29] = [
            "phase/browser-1",
            "phase/browser-2",
            "phase/clippy",
            "phase/doc",
            "phase/doctest",
            "phase/duckdb",
            "phase/features",
            "phase/guards",
            "phase/integration-authoring",
            "phase/integration-clients",
            "phase/integration-wasm",
            "phase/provenance",
            "phase/rivet",
            "phase/test-shards",
            "phase/wasm",
            "test/heavy-1",
            "test/heavy-2",
            "test/heavy-3",
            "test/heavy-4",
            "test/heavy-5",
            "test/heavy-6",
            "test/rest-1",
            "test/rest-2",
            "test/rest-3",
            "test/rest-4",
            "test/rest-5",
            "test/rest-6",
            "test/rest-7",
            "test/rest-8",
        ];
        const SUBSETS: u32 = 1 << 29;
        let mut inventory = TASKS.to_vec();
        inventory.sort_unstable();
        assert_eq!(inventory, BEFORE);
        let packed = |plan: &Plan| {
            let bundles = pack(plan).expect("pack plan");
            assert!(bundles.len() <= MAX_BUNDLES);
            assert!(bundles.iter().all(|bundle| !bundle.tasks.is_empty()));
            let mut tasks: Vec<_> = bundles
                .into_iter()
                .flat_map(|bundle| bundle.tasks)
                .collect();
            tasks.sort_unstable();
            tasks
        };
        let planned = |plan: &Plan| {
            let mut tasks: Vec<_> = plan
                .shard
                .iter()
                .map(|shard| format!("test/{shard}"))
                .chain(plan.phase.iter().map(|phase| format!("phase/{phase}")))
                .collect();
            tasks.sort_unstable();
            tasks
        };
        for expected in PYTHON_PLANS {
            let plan: Plan = serde_json::from_str(expected).expect("pinned plan");
            assert_eq!(packed(&plan), planned(&plan));
            if matches!(plan.mode, Mode::Full) {
                assert_eq!(planned(&plan), BEFORE);
            }
        }
        // About 220 task subsets spread over all of them (a fixed odd stride), as affected plans.
        let mut mask = 1_u32;
        while mask < SUBSETS {
            let (mut shard, mut phase) = (Vec::new(), Vec::new());
            for (bit, task) in (0_u32..).zip(BEFORE) {
                if mask.checked_shr(bit).is_some_and(|rest| rest & 1 == 1) {
                    match task.split_once('/') {
                        Some(("test", name)) => shard.push(name.to_owned()),
                        Some((_, name)) => phase.push(name.to_owned()),
                        None => panic!("task without a kind: {task}"),
                    }
                }
            }
            let plan = Plan {
                mode: Mode::Affected,
                shard,
                phase,
                packages: None,
                rest_shards: None,
                reason: None,
                extra: BTreeMap::new(),
            };
            assert_eq!(packed(&plan), planned(&plan), "{mask:#x}");
            mask = mask.saturating_add(2_440_323);
        }
    }

    #[test]
    fn duration_tables_are_refused_unless_complete() {
        let table: serde_json::Value = serde_json::from_str(DURATIONS).expect("duration table");
        assert!(Model::parse(DURATIONS).is_ok());
        let refused = |edit: &dyn Fn(&mut serde_json::Value), expected: &str| {
            let mut edited = table.clone();
            edit(&mut edited);
            let error = Model::parse(&edited.to_string())
                .err()
                .unwrap_or_else(|| panic!("accepted a table that should fail with {expected:?}"));
            assert!(
                error.contains(expected),
                "{error} does not name {expected:?}"
            );
        };
        refused(
            &|table| {
                table["task_seconds"]
                    .as_object_mut()
                    .expect("task seconds")
                    .remove("phase/browser-1");
            },
            "has no seconds for phase/browser-1",
        );
        refused(
            &|table| table["task_seconds"]["phase/unknown"] = 1.into(),
            "times a task the planner does not know",
        );
        refused(
            &|table| table["runs"] = serde_json::json!([]),
            "names no measured runs",
        );
        refused(
            &|table| table["statistic"] = "".into(),
            "names no measured runs",
        );
        refused(
            &|table| table["generated_by"] = "".into(),
            "names no measured runs",
        );
        refused(
            &|table| table["note"] = "edited".into(),
            "unknown field `note`",
        );
        refused(
            &|table| {
                table
                    .as_object_mut()
                    .expect("table")
                    .remove("wasm_pkg_seconds");
            },
            "missing field `wasm_pkg_seconds`",
        );
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
            r#"{"mode":"cache-warm","shard":["rest-1"],"phase":["cache-warm-test","cache-warm-guard","cache-warm-clippy","cache-warm-doc","cache-warm-wasm","cache-warm-wasm-pkg","cache-warm-duckdb","cache-warm-rivet","cache-warm-features"],"bundle":[{"id":"b0","tasks":["phase/cache-warm-test","phase/cache-warm-guard","phase/cache-warm-clippy","phase/cache-warm-doc","phase/cache-warm-wasm","phase/cache-warm-wasm-pkg","phase/cache-warm-duckdb","phase/cache-warm-rivet","phase/cache-warm-features"]}]}"#,
            r#"{"mode":"cache-warm","shard":[],"phase":["cache-warm-test"],"bundle":[{"id":"b0","tasks":["phase/cache-warm-test"]}]}"#,
            r#"{"mode":"cache-warm","shard":[],"phase":["doc"],"bundle":[{"id":"b0","tasks":["phase/doc"]}]}"#,
            r#"{"mode":"cache-warm","shard":[],"phase":["cache-warm-test","cache-warm-guard","cache-warm-clippy","cache-warm-doc","cache-warm-wasm","cache-warm-wasm-pkg","cache-warm-duckdb","cache-warm-rivet","cache-warm-features"],"packages":"norn-cell","bundle":[{"id":"b0","tasks":["phase/cache-warm-test","phase/cache-warm-guard","phase/cache-warm-clippy","phase/cache-warm-doc","phase/cache-warm-wasm","phase/cache-warm-wasm-pkg","phase/cache-warm-duckdb","phase/cache-warm-rivet","phase/cache-warm-features"]}]}"#,
            r#"{"mode":"cache-warm","shard":[],"phase":["cache-warm-test","cache-warm-guard","cache-warm-clippy","cache-warm-doc","cache-warm-wasm","cache-warm-wasm-pkg","cache-warm-duckdb","cache-warm-rivet","cache-warm-features"],"bundle":[{"id":"b1","tasks":["phase/cache-warm-test","phase/cache-warm-guard","phase/cache-warm-clippy","phase/cache-warm-doc","phase/cache-warm-wasm","phase/cache-warm-wasm-pkg","phase/cache-warm-duckdb","phase/cache-warm-rivet","phase/cache-warm-features"]}]}"#,
            r#"{"mode":"affected","shard":[],"phase":["cache-warm-test"],"bundle":[{"id":"b0","tasks":["phase/cache-warm-test"]}]}"#,
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
                "phase/browser-1",
                "phase/browser-2",
                "test/rest-2",
                "phase/test-shards",
                "phase/guards",
                "phase/integration-wasm",
                "test/rest-1",
                "phase/doc",
                "phase/integration-authoring",
                "phase/integration-clients",
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
                    ("phase/guards", false),
                    ("test/rest-1", false),
                    ("phase/browser-1", true),
                    ("phase/browser-2", false),
                    ("phase/integration-wasm", false),
                    ("phase/integration-authoring", false),
                    ("phase/integration-clients", false),
                    ("phase/clippy", true),
                    ("phase/doc", true),
                ],
            ),
            (
                false,
                [
                    ("test/rest-2", false),
                    ("test/rest-1", false),
                    ("phase/browser-1", true),
                    ("phase/browser-2", false),
                    ("phase/integration-wasm", false),
                    ("phase/integration-authoring", false),
                    ("phase/integration-clients", false),
                    ("phase/clippy", true),
                    ("phase/test-shards", true),
                    ("phase/guards", true),
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

        let warm_bundle = Bundle {
            id: "b0".into(),
            tasks: CACHE_WARM_TASKS.into_iter().map(str::to_owned).collect(),
        };
        let mut warm_calls = Vec::new();
        execute(&warm_bundle, true, |task, prune| {
            warm_calls.push((task.name, prune));
            Ok(())
        })
        .expect("execute cache-warm");
        assert_eq!(
            warm_calls,
            [
                ("phase/cache-warm-test", false),
                ("phase/cache-warm-guard", true),
                ("phase/cache-warm-clippy", true),
                ("phase/cache-warm-doc", true),
                ("phase/cache-warm-wasm", true),
                ("phase/cache-warm-wasm-pkg", true),
                ("phase/cache-warm-duckdb", true),
                ("phase/cache-warm-rivet", true),
                ("phase/cache-warm-features", true),
            ]
        );
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
            target.join("other-output/pkg"),
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
                "rivet/debug/.fingerprint",
                "doc",
                "norn-checker-wasm/pkg",
                "norn-checker-wasm/engine",
            ] {
                fs::create_dir_all(target.join(dir)).expect("create Cargo output");
            }
            prune_target(&root, &configured).expect("prune");
            for dir in [
                "debug",
                "wasm32-unknown-unknown",
                "rivet",
                "doc",
                "norn-checker-wasm",
            ] {
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

    #[test]
    fn prune_target_clears_wasm_pkg_between_incompatible_groups_only() {
        let tmp = tempfile::tempdir().expect("scratch workspace");
        let root = tmp.path();
        let target = root.join("target");
        let bundle = Bundle {
            id: "b0".into(),
            tasks: [
                "phase/doc",
                "phase/browser-1",
                "phase/browser-2",
                "phase/integration-authoring",
                "phase/integration-wasm",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        };
        let wasm_pkg = target.join("norn-checker-wasm/pkg");
        let wasm_engine = target.join("norn-checker-wasm/engine");
        let mut seen = Vec::new();
        execute(&bundle, true, |task, prune| {
            if prune {
                prune_target(root, &target).expect("prune");
            }
            match task.name {
                "phase/browser-1" => {
                    assert!(!wasm_pkg.exists(), "clean before first WasmPkg task");
                    fs::create_dir_all(&wasm_pkg).expect("create wasm pkg");
                    fs::create_dir_all(&wasm_engine).expect("create wasm engine");
                }
                "phase/browser-2" | "phase/integration-authoring" | "phase/integration-wasm" => {
                    assert!(
                        wasm_pkg.is_dir() && wasm_engine.is_dir(),
                        "consecutive WasmPkg task {} must keep cached wasm outputs",
                        task.name
                    );
                }
                "phase/doc" => {
                    assert!(
                        !target.join("norn-checker-wasm").exists(),
                        "incompatible group must prune target/norn-checker-wasm"
                    );
                }
                other => return Err(format!("unexpected task {other}").into()),
            }
            seen.push((task.name, prune));
            Ok(())
        })
        .expect("execute");
        assert_eq!(
            seen,
            [
                ("phase/browser-1", false),
                ("phase/browser-2", false),
                ("phase/integration-authoring", false),
                ("phase/integration-wasm", false),
                ("phase/doc", true),
            ]
        );
    }

    #[test]
    fn integration_clients_shares_pruning_affinity_without_wasm_compile_cost() {
        let model = Model::load().expect("duration table");
        let clients = model.tasks["phase/integration-clients"];
        let authoring = model.tasks["phase/integration-authoring"];

        // Standalone phase/integration-clients bin does NOT include the WASM package build.
        let standalone = Bin {
            tasks: vec![clients],
        };
        assert_eq!(standalone.cost(&model, true), clients.weight);
        assert_eq!(
            Bin::default().added_cost(&model, clients, true),
            clients.weight
        );

        // Co-located phase/integration-authoring + phase/integration-clients charges
        // the WASM package build once and does not prune between them.
        let combined = Bin {
            tasks: vec![authoring, clients],
        };
        assert_eq!(
            combined.cost(&model, true),
            authoring.weight + clients.weight + model.wasm_pkg
        );
        assert_eq!(
            Bin {
                tasks: vec![authoring]
            }
            .cost_with(&model, &[clients], true),
            authoring.weight + clients.weight + model.wasm_pkg
        );

        // phase/guards shares the workspace test build on full plans (all_packages=true),
        // and pays its own workspace test build on narrowed affected plans (all_packages=false).
        let guards = model.tasks["phase/guards"];
        let shard = model.tasks["test/rest-1"];
        let guards_with_shard = Bin {
            tasks: vec![shard, guards],
        };
        assert_eq!(
            guards_with_shard.cost(&model, true),
            shard.weight + guards.weight + model.test_build
        );
        assert_eq!(
            guards_with_shard.cost(&model, false),
            shard.weight + guards.weight + model.test_build * 2
        );

        let bundle = Bundle {
            id: "b0".into(),
            tasks: vec![
                "phase/integration-authoring".into(),
                "phase/integration-clients".into(),
            ],
        };
        let mut calls = Vec::new();
        execute(&bundle, true, |task, prune| {
            calls.push((task.name, prune));
            Ok(())
        })
        .expect("execute");
        assert_eq!(
            calls,
            [
                ("phase/integration-authoring", false),
                ("phase/integration-clients", false),
            ]
        );
    }
}
