//! `norn test`: declarative scenario runner over isolated in-memory `LocalCell`s.
//!
//! Each scenario runs against a fresh `DevHost<NativeEngine>` with a deterministic
//! `ManualClock` (never wall-clock time) and fail-closed authorization.

use std::path::PathBuf;

use clap::Args;
use norn_api::actions::ActionQuery;
use norn_api::local_cell::{LocalActor, LocalCell};
use norn_api::object_query::ObjectQueryParams;
use norn_api::routes::DefinitionsQuery;
use norn_api::scope::ScopeQuery;
use serde_json::{Map, Value};

use crate::authoring::CommandOutput;

use super::pack::{PackInput, PackLoadError, encode_definitions_change_body, load_pack_input};
use super::scenario_assert::{
    StepExpect, check_action_result, check_apply_result, check_defs_result,
    check_get_object_result, check_list_objects_result, check_plan_result, render_json_summary,
    render_text_summary, step_report_entry,
};
use super::scenario_parse::load_scenario;
use super::{BootFailure, boot_local_cell};

/// One parsed, validated scenario ready to execute on an isolated `LocalCell`.
#[derive(Debug)]
pub(super) struct Scenario {
    pub(super) name: String,
    pub(super) source_path: PathBuf,
    pub(super) org: String,
    pub(super) env: String,
    pub(super) clock_ms: Option<u64>,
    pub(super) pack: Option<PackInput>,
    pub(super) base_packs: Vec<PathBuf>,
    pub(super) steps: Vec<ScenarioStep>,
}

/// Optional per-step scope override (defaults to the scenario's org/env).
#[derive(Debug, Default)]
pub(super) struct StepScope {
    pub(super) org: Option<String>,
    pub(super) env: Option<String>,
}

/// Payload for a `defs` scenario step.
#[derive(Debug)]
pub(super) struct DefsStep {
    pub(super) actor: Option<LocalActor>,
    pub(super) scope: StepScope,
    pub(super) version: Option<u64>,
    pub(super) expect: StepExpect,
}

/// Payload for a `plan` scenario step.
#[derive(Debug)]
pub(super) struct PlanStep {
    pub(super) actor: Option<LocalActor>,
    pub(super) scope: StepScope,
    pub(super) pack: PackInput,
    pub(super) expect: StepExpect,
}

/// Payload for an `apply` scenario step.
#[derive(Debug)]
pub(super) struct ApplyStep {
    pub(super) actor: Option<LocalActor>,
    pub(super) scope: StepScope,
    pub(super) pack: PackInput,
    pub(super) expected_version: Option<u64>,
    pub(super) expect: StepExpect,
}

/// Payload for an `action` scenario step.
#[derive(Debug)]
pub(super) struct ActionStep {
    pub(super) action_id: String,
    pub(super) idempotency_key: String,
    pub(super) actor: Option<LocalActor>,
    pub(super) scope: StepScope,
    pub(super) version: Option<u64>,
    pub(super) input: Value,
    pub(super) expect: StepExpect,
}

/// Payload for a `get_object` scenario step.
#[derive(Debug)]
pub(super) struct GetObjectStep {
    pub(super) object_type: String,
    pub(super) id: String,
    pub(super) actor: Option<LocalActor>,
    pub(super) scope: StepScope,
    pub(super) expect: StepExpect,
}

/// Payload for a `list_objects` scenario step.
#[derive(Debug)]
pub(super) struct ListObjectsStep {
    pub(super) object_type: String,
    pub(super) actor: Option<LocalActor>,
    pub(super) scope: StepScope,
    pub(super) limit: Option<String>,
    pub(super) sort: Option<String>,
    pub(super) filter: Option<String>,
    pub(super) expect: StepExpect,
}

/// One validated scenario step (boxed payloads keep every variant uniform in size).
#[derive(Debug)]
pub(super) enum ScenarioStep {
    AdvanceClock { ms: u64 },
    Defs(Box<DefsStep>),
    Plan(Box<PlanStep>),
    Apply(Box<ApplyStep>),
    Action(Box<ActionStep>),
    GetObject(Box<GetObjectStep>),
    ListObjects(Box<ListObjectsStep>),
}

/// Arguments for `norn test`.
#[derive(Debug, Args)]
pub(crate) struct TestArgs {
    /// One or more scenario JSON files or directories of `*.json` scenarios.
    #[arg(required = true)]
    pub(crate) scenarios: Vec<PathBuf>,
    /// Default authoring source (`.ts`/`.py` file or directory) when a scenario omits `pack`.
    #[arg(long, conflicts_with = "artifacts")]
    pub(crate) source: Option<PathBuf>,
    /// Default compiled JSON artifact file or directory when a scenario omits `pack`.
    #[arg(long, conflicts_with = "source")]
    pub(crate) artifacts: Option<PathBuf>,
    /// Base pack(s) (JSON or `TypeScript`/Python) installed before the app pack.
    #[arg(long = "base")]
    pub(crate) base: Vec<PathBuf>,
    /// Default organization scope when a scenario omits `org`.
    #[arg(long)]
    pub(crate) org: Option<String>,
    /// Default environment scope when a scenario omits `env`.
    #[arg(long)]
    pub(crate) env: Option<String>,
    /// Emit structured JSON summary instead of human-readable output.
    #[arg(long)]
    pub(crate) json: bool,
}

pub(super) struct ScenarioOutcome {
    pub(super) name: String,
    pub(super) file: String,
    pub(super) passed: bool,
    pub(super) startup_error: Option<String>,
    pub(super) steps: Vec<Value>,
}

/// Execute `norn test` across all specified scenario files.
pub(crate) fn execute_test(args: &TestArgs) -> CommandOutput {
    let files = match collect_scenario_files(&args.scenarios) {
        Ok(files) => files,
        Err(err) => return CommandOutput::usage_or_io_error(&err),
    };
    let mut scenarios = Vec::with_capacity(files.len());
    for file in &files {
        match load_scenario(file, args) {
            Ok(scenario) => scenarios.push(scenario),
            Err(err) => return CommandOutput::usage_or_io_error(&err),
        }
    }
    let mut outcomes = Vec::with_capacity(scenarios.len());
    for scenario in &scenarios {
        match run_scenario(scenario) {
            Ok(outcome) => outcomes.push(outcome),
            Err(err) => return CommandOutput::usage_or_io_error(&err),
        }
    }
    let all_passed = outcomes.iter().all(|outcome| outcome.passed);
    let rendered = if args.json {
        render_json_summary(&outcomes, all_passed)
    } else {
        render_text_summary(&outcomes)
    };
    if all_passed {
        CommandOutput::ok(rendered)
    } else {
        CommandOutput::refused(rendered)
    }
}

fn run_scenario(scenario: &Scenario) -> Result<ScenarioOutcome, String> {
    let cell = match boot_local_cell(
        &scenario.org,
        &scenario.env,
        scenario.clock_ms,
        None,
        &scenario.base_packs,
        scenario.pack.as_ref(),
    ) {
        Ok(cell) => cell,
        Err(BootFailure::Refused(msg)) => {
            return Ok(ScenarioOutcome {
                name: scenario.name.clone(),
                file: scenario.source_path.display().to_string(),
                passed: false,
                startup_error: Some(msg),
                steps: Vec::new(),
            });
        }
        Err(BootFailure::Io(msg)) => return Err(msg),
    };
    let mut reports = Vec::with_capacity(scenario.steps.len());
    let mut all_ok = true;
    for (idx, step) in scenario.steps.iter().enumerate() {
        let op = step_op_name(step);
        match execute_step(&cell, scenario, step) {
            Ok(Ok(())) => reports.push(step_report_entry(idx, op, true, None)),
            Ok(Err(assertion_err)) => {
                all_ok = false;
                reports.push(step_report_entry(idx, op, false, Some(&assertion_err)));
            }
            Err(io_err) => return Err(format!("{} step {idx} ({op}): {io_err}", scenario.name)),
        }
    }
    Ok(ScenarioOutcome {
        name: scenario.name.clone(),
        file: scenario.source_path.display().to_string(),
        passed: all_ok,
        startup_error: None,
        steps: reports,
    })
}

const fn step_op_name(step: &ScenarioStep) -> &'static str {
    match step {
        ScenarioStep::AdvanceClock { .. } => "advance_clock",
        ScenarioStep::Defs(_) => "defs",
        ScenarioStep::Plan(_) => "plan",
        ScenarioStep::Apply(_) => "apply",
        ScenarioStep::Action(_) => "action",
        ScenarioStep::GetObject(_) => "get_object",
        ScenarioStep::ListObjects(_) => "list_objects",
    }
}

fn step_scope_query(scenario: &Scenario, override_scope: &StepScope) -> ScopeQuery {
    ScopeQuery {
        org_id: Some(
            override_scope
                .org
                .clone()
                .unwrap_or_else(|| scenario.org.clone()),
        ),
        environment_id: Some(
            override_scope
                .env
                .clone()
                .unwrap_or_else(|| scenario.env.clone()),
        ),
    }
}

/// Outer `Result` is IO/usage error (exit 2); inner `Result` is assertion failure (exit 1).
fn execute_step(
    cell: &LocalCell,
    scenario: &Scenario,
    step: &ScenarioStep,
) -> Result<Result<(), String>, String> {
    match step {
        ScenarioStep::AdvanceClock { ms } => {
            let _next = cell.advance_clock(*ms);
            Ok(Ok(()))
        }
        ScenarioStep::Defs(s) => Ok(run_defs_step(cell, scenario, s)),
        ScenarioStep::Plan(s) => run_plan_step(cell, scenario, s),
        ScenarioStep::Apply(s) => run_apply_step(cell, scenario, s),
        ScenarioStep::Action(s) => run_action_step(cell, scenario, s),
        ScenarioStep::GetObject(s) => Ok(run_get_object_step(cell, scenario, s)),
        ScenarioStep::ListObjects(s) => Ok(run_list_objects_step(cell, scenario, s)),
    }
}

fn run_defs_step(cell: &LocalCell, scenario: &Scenario, step: &DefsStep) -> Result<(), String> {
    let sq = step_scope_query(scenario, &step.scope);
    let query = DefinitionsQuery {
        org_id: sq.org_id,
        environment_id: sq.environment_id,
        version: step.version,
    };
    check_defs_result(cell.get_defs(&query, step.actor.as_ref()), &step.expect)
}

fn run_plan_step(
    cell: &LocalCell,
    scenario: &Scenario,
    step: &PlanStep,
) -> Result<Result<(), String>, String> {
    let artifacts = load_step_pack(&step.pack, &scenario.org, &scenario.env)?;
    let body = encode_definitions_change_body(&artifacts, None);
    let sq = step_scope_query(scenario, &step.scope);
    Ok(check_plan_result(
        cell.plan_definitions(&sq, step.actor.as_ref(), &body),
        &step.expect,
    ))
}

fn run_apply_step(
    cell: &LocalCell,
    scenario: &Scenario,
    step: &ApplyStep,
) -> Result<Result<(), String>, String> {
    let artifacts = load_step_pack(&step.pack, &scenario.org, &scenario.env)?;
    let expected = step
        .expected_version
        .or_else(|| cell.active_definitions_version().ok().flatten());
    let body = encode_definitions_change_body(&artifacts, expected);
    let sq = step_scope_query(scenario, &step.scope);
    Ok(check_apply_result(
        cell.apply_definitions(&sq, step.actor.as_ref(), &body),
        &step.expect,
    ))
}

fn run_action_step(
    cell: &LocalCell,
    scenario: &Scenario,
    step: &ActionStep,
) -> Result<Result<(), String>, String> {
    let sq = step_scope_query(scenario, &step.scope);
    let query = ActionQuery {
        org_id: sq.org_id,
        environment_id: sq.environment_id,
        version: step.version,
    };
    let body = encode_action_body(&step.input, &step.idempotency_key)?;
    Ok(check_action_result(
        cell.call_action(&query, step.actor.as_ref(), &step.action_id, &body),
        &step.expect,
    ))
}

fn run_get_object_step(
    cell: &LocalCell,
    scenario: &Scenario,
    step: &GetObjectStep,
) -> Result<(), String> {
    let sq = step_scope_query(scenario, &step.scope);
    check_get_object_result(
        cell.get_object(&sq, step.actor.as_ref(), &step.object_type, &step.id),
        &step.expect,
    )
}

fn run_list_objects_step(
    cell: &LocalCell,
    scenario: &Scenario,
    step: &ListObjectsStep,
) -> Result<(), String> {
    let sq = step_scope_query(scenario, &step.scope);
    let params = ObjectQueryParams {
        limit: step.limit.clone(),
        sort: step.sort.clone(),
        filter: step.filter.clone(),
        after: None,
    };
    check_list_objects_result(
        cell.query_objects(&sq, step.actor.as_ref(), &step.object_type, &params),
        &step.expect,
    )
}

fn load_step_pack(
    pack: &PackInput,
    org: &str,
    env: &str,
) -> Result<Vec<Box<serde_json::value::RawValue>>, String> {
    load_pack_input(pack, org, env).map_err(|err| match err {
        PackLoadError::Diagnostics(doc) => format!("pack compilation diagnostics: {doc}"),
        PackLoadError::Io(msg) => msg,
    })
}

fn encode_action_body(input: &Value, idempotency_key: &str) -> Result<String, String> {
    let mut map = Map::new();
    map.insert("input".to_owned(), input.clone());
    map.insert("idempotencyKey".to_owned(), Value::from(idempotency_key));
    serde_json::to_string(&Value::Object(map)).map_err(|err| err.to_string())
}

fn collect_scenario_files(inputs: &[PathBuf]) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    for path in inputs {
        let meta =
            std::fs::symlink_metadata(path).map_err(|err| format!("{}: {err}", path.display()))?;
        if meta.file_type().is_symlink() {
            return Err(format!("{}: symlinks are refused", path.display()));
        }
        if meta.is_file() {
            files.push(path.clone());
        } else if meta.is_dir() {
            let mut children = Vec::new();
            for entry in
                std::fs::read_dir(path).map_err(|err| format!("{}: {err}", path.display()))?
            {
                let entry = entry.map_err(|err| format!("{}: {err}", path.display()))?;
                let child_path = entry.path();
                if child_path.extension().and_then(std::ffi::OsStr::to_str) != Some("json") {
                    continue;
                }
                let child_meta = std::fs::symlink_metadata(&child_path)
                    .map_err(|err| format!("{}: {err}", child_path.display()))?;
                if child_meta.file_type().is_symlink() {
                    return Err(format!("{}: symlinks are refused", child_path.display()));
                }
                if !child_meta.is_file() {
                    return Err(format!("{}: not a regular file", child_path.display()));
                }
                children.push(child_path);
            }
            children.sort();
            files.extend(children);
        } else {
            return Err(format!(
                "{}: not a regular file or directory",
                path.display()
            ));
        }
    }
    if files.is_empty() {
        return Err("no scenario JSON files found".to_owned());
    }
    Ok(files)
}
