//! QUESTION-STEP-1: read-only evaluation of parked `norn.question@1` flow steps
//! through [`QuestionRuntime`] under the run starter's authority, with
//! `STEP-PIN-1` definition pinning, `PRIVACY-1`/`PRIVACY-3`/`PRIVACY-4`
//! disclosure guards, and `#1905` `RequiredSources` freshness enforcement.
//!
//! A `norn.question@1` step never mutates business state: it reads a bounded
//! snapshot through the shared read authority, refuses unauthorized readers or
//! restricted markings with `Denied` (never leaking values), refuses stale or
//! incomplete required sources with typed `StaleSource`, and refuses pin
//! mismatches or other deterministic errors with `Refused { code }`.

use std::collections::{BTreeMap, BTreeSet};

use http::StatusCode;
use norn_cell::compute::{Engine, Freshness, QueryResult};
use norn_cell::engine::SqlEngine;
use norn_cell::flow_run::{QuestionFreshnessGuard, QuestionStep, QuestionStepPin, RunStarter};
use norn_cell::retained::RetainedLifecycle;
use norn_cell::{CellStore, MAIN_WORLD};
use norn_checker::metric::compute::QuestionPlan;
use norn_ir::metric::{DimensionPath, FilterOperator, MetricFilter};
use norn_ir::{PropertyId, QuestionIR, RuntimeObject, RuntimeValue, UtcTimestamp};
use norn_kernel::flow_turn::StepOutcome;

use crate::authz::{ReadCapability, ReadRequest, read_denied};
use crate::flow_driver::AsStarter;
use crate::host::DevHost;
use crate::privacy::DisclosedAnswer;
use crate::question_routes::{
    MARKED_HISTORY, QuestionAnswer, QuestionRuntime, admit_named_question, answer_plan,
    check_ad_hoc, handle_key, mapped,
};
use crate::refusal::{Refusal, RefusalCategory};
use crate::scope::ScopeQuery;

/// Stable `question_step.*` refusal codes for parked `norn.question@1` steps.
pub mod question_step_codes {
    /// The question definition or underlying model changed or was retired after
    /// the flow run started (`STEP-PIN-1`).
    pub const DEFINITION_CHANGED: &str = "question_step.definition_changed";
    /// The active question definition or installed definitions version does not
    /// match the explicit `pin` carried by the `norn.question@1` step (`STEP-PIN-1`).
    pub const PIN_MISMATCH: &str = "question_step.pin_mismatch";
    /// The retained definitions set for the flow run's starting definitions
    /// version is unavailable or unreadable (`STEP-PIN-1`).
    pub const PIN_UNAVAILABLE: &str = "question_step.pin_unavailable";
    /// This cell's question runtime does not publish ad-hoc/retained questions.
    pub const UNAVAILABLE: &str = "question_step.unavailable";
    /// Dynamic `params` were supplied for a pre-admitted named question that
    /// does not support parameter bindings.
    pub const UNSUPPORTED_PARAMS: &str = "question_step.unsupported_params";
    /// A `params` entry has a non-scalar value or invalid property name.
    pub const INVALID_PARAM: &str = "question_step.invalid_param";
    /// The step's `asOf` timestamp is invalid or disagrees with a pre-admitted
    /// named question's effective cutoff.
    pub const AS_OF_MISMATCH: &str = "question_step.as_of_mismatch";
    /// The serialized `Answered` output exceeds `MAX_RECEIPT_SUMMARY_BYTES`.
    pub const OUTPUT_TOO_LARGE: &str = "question_step.output_too_large";
    /// Required external sources are stale or incomplete (`QUERY-FRESH-1`).
    pub const STALE_SOURCE: &str = "core.compute.stale_input";
}

/// Coordinates and inputs for one parked `norn.question@1` step.
pub(crate) struct QuestionStepContext<'a> {
    pub(crate) input: &'a RuntimeValue,
    pub(crate) starter: Option<&'a RunStarter>,
    pub(crate) run_definitions_version: Option<u64>,
}

fn pin_mismatch() -> Refusal {
    Refusal::new(
        StatusCode::CONFLICT,
        question_step_codes::PIN_MISMATCH,
        "the active question definition does not match the parked flow step's pin",
        "retarget the step pin or install the pinned definition version",
    )
}

fn definition_changed() -> Refusal {
    Refusal::new(
        StatusCode::CONFLICT,
        question_step_codes::DEFINITION_CHANGED,
        "the parked flow step's question definition changed after the run started",
        "start a new run or restore a compatible question definition before retrying",
    )
}

fn pin_unavailable() -> Refusal {
    Refusal::new(
        StatusCode::CONFLICT,
        question_step_codes::PIN_UNAVAILABLE,
        "the flow run's starting definitions set is unavailable to verify step compatibility",
        "restore the retained starting definitions set before retrying the parked step",
    )
}

fn question_unavailable() -> Refusal {
    Refusal::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        question_step_codes::UNAVAILABLE,
        "this cell does not publish questions for flow question steps",
        "configure a question runtime before running flows with norn.question@1 steps",
    )
}

fn question_not_found() -> Refusal {
    Refusal::new(
        StatusCode::NOT_FOUND,
        "question.not_found",
        "no such admitted question",
        "name an active retained question definition or an admitted named question",
    )
}

fn invalid_param(detail: &str) -> Refusal {
    Refusal::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        question_step_codes::INVALID_PARAM,
        "a question step parameter is invalid",
        detail,
    )
}

fn unsupported_params() -> Refusal {
    Refusal::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        question_step_codes::UNSUPPORTED_PARAMS,
        "named questions do not accept dynamic parameter bindings",
        "omit params or target a retained question definition",
    )
}

fn as_of_mismatch() -> Refusal {
    Refusal::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        question_step_codes::AS_OF_MISMATCH,
        "the step's asOf cutoff is invalid or does not match the admitted question",
        "supply a valid RFC 3339 UTC timestamp matching the question contract",
    )
}

fn output_too_large() -> Refusal {
    Refusal::new(
        StatusCode::PAYLOAD_TOO_LARGE,
        question_step_codes::OUTPUT_TOO_LARGE,
        "the question step answer exceeds the receipt summary size bound",
        "narrow the question's grouping or time window",
    )
}

fn refused_failure(code: &str) -> StepOutcome {
    StepOutcome::Failure(BTreeMap::from([
        (
            "_tag".to_owned(),
            RuntimeValue::String("Refused".to_owned()),
        ),
        ("code".to_owned(), RuntimeValue::String(code.to_owned())),
    ]))
}

fn denied_failure() -> StepOutcome {
    StepOutcome::Failure(BTreeMap::from([(
        "_tag".to_owned(),
        RuntimeValue::String("Denied".to_owned()),
    )]))
}

fn stale_failure(freshness: &str, complete: bool, sources: Vec<String>) -> StepOutcome {
    StepOutcome::Failure(BTreeMap::from([
        (
            "_tag".to_owned(),
            RuntimeValue::String("StaleSource".to_owned()),
        ),
        (
            "code".to_owned(),
            RuntimeValue::String(question_step_codes::STALE_SOURCE.to_owned()),
        ),
        (
            "freshness".to_owned(),
            RuntimeValue::String(freshness.to_owned()),
        ),
        ("complete".to_owned(), RuntimeValue::Bool(complete)),
        (
            "sources".to_owned(),
            RuntimeValue::Array(sources.into_iter().map(RuntimeValue::String).collect()),
        ),
    ]))
}

/// Evaluate one parked `norn.question@1` step read-only under `starter`'s
/// authority (`as_starter`) and return the typed [`StepOutcome`] to settle.
///
/// # Errors
/// Only internal host failures (the step stays pending for the next drive).
/// All domain, permission, marking, pin, and freshness refusals settle as typed
/// `StaleSource`, `Denied`, or `Refused` outcomes.
pub(crate) fn dispatch_question_step<E: SqlEngine>(
    host: &DevHost<E>,
    ctx: &QuestionStepContext<'_>,
    as_starter: AsStarter<'_>,
    disclosure: &dyn QuestionRuntime<E>,
) -> Result<StepOutcome, Refusal> {
    let parsed = match norn_cell::flow_run::parse_question_step(ctx.input) {
        Ok(step) => step,
        Err(refused) => return Ok(refused_failure(refused.code())),
    };
    let Some(starter) = ctx.starter else {
        return Ok(denied_failure());
    };
    let mut outcome: Option<StepOutcome> = None;
    let mut run_read_only = || -> Result<(), Refusal> {
        outcome = Some(evaluate_as_starter(
            host,
            &parsed,
            starter,
            ctx.run_definitions_version,
            disclosure,
        )?);
        Ok(())
    };
    match as_starter(starter, &mut run_read_only) {
        None => Ok(denied_failure()),
        Some(Ok(())) => outcome.ok_or_else(|| Refusal::host("question step produced no outcome")),
        Some(Err(refused)) => classify_refusal(refused),
    }
}

fn classify_refusal(refused: Refusal) -> Result<StepOutcome, Refusal> {
    if is_marking_or_disclosure_refusal(&refused) {
        return Ok(denied_failure());
    }
    if refused.code == question_step_codes::STALE_SOURCE {
        return Ok(stale_failure("incomplete", false, Vec::new()));
    }
    match refused.category {
        RefusalCategory::Internal => Err(refused),
        RefusalCategory::Permission | RefusalCategory::Visibility => Ok(denied_failure()),
        RefusalCategory::Precondition
        | RefusalCategory::Conflict
        | RefusalCategory::Invalid
        | RefusalCategory::Limit => Ok(refused_failure(&refused.code)),
    }
}

fn is_marking_or_disclosure_refusal(refused: &Refusal) -> bool {
    refused.code == MARKED_HISTORY
        || refused.code == "privacy.marked_column"
        || refused
            .diagnostics
            .iter()
            .any(|diag| diag.code == norn_checker::metric::codes::MARKED_FIELD)
}

fn evaluate_as_starter<E: SqlEngine>(
    host: &DevHost<E>,
    step: &QuestionStep,
    starter: &RunStarter,
    run_definitions_version: Option<u64>,
    disclosure: &dyn QuestionRuntime<E>,
) -> Result<StepOutcome, Refusal> {
    let scope = ScopeQuery {
        org_id: Some(host.scope().org_id.clone()),
        environment_id: Some(host.scope().environment_id.clone()),
    };
    let key = handle_key(disclosure)?;

    // Check whether the question id names a retained `"question"` definition
    // (no nested definition_read lock before admission helpers).
    let has_retained_question = {
        let _epoch = host.definition_read()?;
        let cell = host.cell()?;
        cell.store()
            .read()
            .retained_definition(&step.question, None)
            .map_err(Refusal::host)?
            .is_some_and(|rec| rec.definition.kind == "question")
    };

    let (plan, disclosed) = if has_retained_question {
        evaluate_retained_question(
            host,
            &scope,
            starter,
            step,
            run_definitions_version,
            disclosure,
            key,
        )?
    } else {
        evaluate_named_question(
            host,
            &scope,
            starter,
            step,
            run_definitions_version,
            disclosure,
        )?
    };

    let result = match disclosed.as_answer() {
        QuestionAnswer::Live(res) => res,
        QuestionAnswer::Historical(hist) => &hist.result,
    };

    // PRIVACY-1 / PRIVACY-3 / DATA-VALUES-1: any restricted member or suppressed
    // bucket denies the flow step rather than leaking partial/withheld aggregates.
    if result
        .rows
        .values()
        .any(|row| row.suppressed || row.members.values().any(|member| member.restricted))
    {
        return Ok(denied_failure());
    }

    // QUERY-FRESH-1 (#1905): enforce model-derived RequiredSources freshness guard.
    let stale_sources = {
        let cell = host.cell()?;
        collect_stale_sources(cell.store(), &plan, result, step.freshness.as_ref())?
    };
    if let Some((freshness_label, complete, sources)) =
        check_freshness_guard(result, step.freshness.as_ref(), stale_sources)
    {
        return Ok(stale_failure(freshness_label, complete, sources));
    }

    build_answered_output(&step.question, result)
}

fn evaluate_retained_question<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    starter: &RunStarter,
    step: &QuestionStep,
    run_definitions_version: Option<u64>,
    disclosure: &dyn QuestionRuntime<E>,
    key: &[u8],
) -> Result<(QuestionPlan, DisclosedAnswer), Refusal> {
    let ad_hoc = disclosure
        .as_ad_hoc()
        .filter(|runtime| runtime.publishes_questions())
        .ok_or_else(question_unavailable)?;
    let initial_record = {
        let _epoch = host.definition_read()?;
        let cell = host.cell()?;
        cell.store()
            .read()
            .retained_definition(&step.question, None)
            .map_err(Refusal::host)?
            .filter(|rec| rec.definition.kind == "question")
            .ok_or_else(question_not_found)?
    };
    let mut question_ir: QuestionIR =
        serde_json::from_str(&initial_record.definition.payload).map_err(Refusal::host)?;
    apply_step_overrides(&mut question_ir, step)?;

    // Authorize population BEFORE checking pins or revealing retirement (zero
    // existence oracle for unauthorized starters).
    let admitted = check_ad_hoc(host, scope, &starter.subject, &question_ir, ad_hoc)?;
    require_values_access(
        host,
        disclosure,
        &starter.subject,
        &question_ir.object_set.0,
    )?;

    // Hold one definition_read epoch across STEP-PIN-1 re-verification and evaluation.
    let _epoch = host.definition_read()?;
    let (record, installed_version, active_model_hash) = {
        let cell = host.cell()?;
        let reader = cell.store().read();
        let record = reader
            .retained_definition(&step.question, None)
            .map_err(Refusal::host)?
            .filter(|rec| rec.definition.kind == "question")
            .ok_or_else(question_not_found)?;
        let installed_version = reader.active_definitions_version().map_err(Refusal::host)?;
        let active_model_hash = reader
            .installed_definitions(None)
            .map_err(Refusal::host)?
            .map(|set| set.model_hash);
        (record, installed_version, active_model_hash)
    };
    if record.definition.revision != initial_record.definition.revision
        || record.definition.payload_sha256 != initial_record.definition.payload_sha256
    {
        return Err(definition_changed());
    }
    verify_retained_pin(
        host,
        &record,
        installed_version,
        active_model_hash.as_deref(),
        run_definitions_version,
        step.pin.as_ref(),
    )?;

    let plan = admit_plan_or_refuse(admitted)?;
    require_values_access(host, disclosure, &starter.subject, plan.grain().as_str())?;

    let disclosed = answer_plan(
        host,
        disclosure,
        &starter.subject,
        &step.question,
        &plan,
        key,
    )?;
    let buckets = match disclosed.as_answer() {
        QuestionAnswer::Live(res) => res.rows.len(),
        QuestionAnswer::Historical(hist) => hist.result.rows.len(),
    };
    if buckets > ad_hoc.max_buckets() {
        return Err(Refusal::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "question.bucket_bound_exceeded",
            "the answer has more buckets than this cell serves",
            "narrow the question: a shorter time window, a coarser grain or fewer groups",
        ));
    }
    Ok((plan, disclosed))
}

fn admit_plan_or_refuse(
    admitted: Result<QuestionPlan, Vec<norn_checker::Diagnostic>>,
) -> Result<QuestionPlan, Refusal> {
    match admitted {
        Ok(plan) => Ok(plan),
        Err(findings) => {
            if findings
                .iter()
                .any(|f| f.code.as_str() == norn_checker::metric::codes::MARKED_FIELD)
            {
                return Err(read_denied());
            }
            Err(Refusal::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "question.refused",
                "the question is not admissible",
                "each diagnostic names the rule and where in the question it applies",
            )
            .with_diagnostics(&findings, "/question"))
        }
    }
}

fn evaluate_named_question<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    starter: &RunStarter,
    step: &QuestionStep,
    run_definitions_version: Option<u64>,
    disclosure: &dyn QuestionRuntime<E>,
) -> Result<(QuestionPlan, DisclosedAnswer), Refusal> {
    // `admit_named_question` authorizes Population and acquires one definition_read epoch.
    let (_epoch, actor, key, plan) = admit_named_question(
        host,
        scope,
        Some(&starter.subject),
        &step.question,
        disclosure,
    )?;
    require_values_access(host, disclosure, actor, &step.question)?;

    let (installed_version, active_model_hash) = {
        let cell = host.cell()?;
        let reader = cell.store().read();
        let installed_version = reader.active_definitions_version().map_err(Refusal::host)?;
        let active_model_hash = reader
            .installed_definitions(None)
            .map_err(Refusal::host)?
            .map(|set| set.model_hash);
        (installed_version, active_model_hash)
    };
    verify_named_pin(
        host,
        &step.question,
        &plan,
        installed_version,
        active_model_hash.as_deref(),
        run_definitions_version,
        step.pin.as_ref(),
    )?;

    if let Some(as_of) = step.as_of.as_deref()
        && (norn_checker::transform::instant(as_of).is_none()
            || as_of != plan.question().time_window.end.as_str())
    {
        return Err(as_of_mismatch());
    }
    if step
        .params
        .as_ref()
        .is_some_and(|params| !params.is_empty())
    {
        return Err(unsupported_params());
    }

    let disclosed = answer_plan(host, disclosure, actor, &step.question, &plan, key)?;
    Ok((plan, disclosed))
}

fn require_values_access<E: SqlEngine>(
    host: &DevHost<E>,
    runtime: &dyn QuestionRuntime<E>,
    actor: &str,
    resource: &str,
) -> Result<(), Refusal> {
    let access = mapped(host, runtime, actor, resource, ReadCapability::Values, None)?;
    if !host.read_permitted(&ReadRequest {
        capability: ReadCapability::Values,
        access: &access,
    }) {
        return Err(read_denied());
    }
    Ok(())
}

fn apply_step_overrides(question_ir: &mut QuestionIR, step: &QuestionStep) -> Result<(), Refusal> {
    if let Some(as_of) = step.as_of.as_deref() {
        if norn_checker::transform::instant(as_of).is_none() {
            return Err(as_of_mismatch());
        }
        question_ir.time_window.end = UtcTimestamp(as_of.to_owned());
    }
    if let Some(params) = &step.params {
        for (key, value) in params {
            if key.trim().is_empty() || key.chars().any(char::is_control) {
                return Err(invalid_param(
                    "parameter names must be non-empty identifiers",
                ));
            }
            match value {
                RuntimeValue::String(_)
                | RuntimeValue::Number(_)
                | RuntimeValue::Bool(_)
                | RuntimeValue::Null => {}
                RuntimeValue::Array(_) | RuntimeValue::Object(_) => {
                    return Err(invalid_param(
                        "parameter bindings must be scalar values (string, number, boolean, or null)",
                    ));
                }
            }
            question_ir.filters.retain(|existing| {
                !(existing.path.links.is_empty() && existing.path.property.as_str() == key)
            });
            question_ir.filters.push(MetricFilter {
                path: DimensionPath {
                    links: Vec::new(),
                    property: PropertyId(key.clone()),
                },
                operator: FilterOperator::Eq,
                value: value.clone(),
            });
        }
    }
    Ok(())
}

fn verify_retained_pin<E: SqlEngine>(
    host: &DevHost<E>,
    record: &norn_cell::retained::RetainedDefinitionRecord,
    installed_version: Option<u64>,
    active_model_hash: Option<&str>,
    run_definitions_version: Option<u64>,
    explicit: Option<&QuestionStepPin>,
) -> Result<(), Refusal> {
    let active_ver = installed_version.ok_or_else(pin_unavailable)?;
    if record.definition.lifecycle != RetainedLifecycle::Active {
        if explicit.is_some() {
            return Err(pin_mismatch());
        }
        let started = run_definitions_version.ok_or_else(pin_unavailable)?;
        let cell = host.cell()?;
        let was_active = cell
            .store()
            .read()
            .retained_definition_at_version(&record.definition.id, started)
            .map_err(Refusal::host)?
            .is_some_and(|prior| {
                prior.definition.kind == "question"
                    && prior.definition.lifecycle == RetainedLifecycle::Active
            });
        return Err(if was_active {
            definition_changed()
        } else {
            question_not_found()
        });
    }
    if let Some(pin) = explicit {
        let rev_str = record.definition.revision.to_string();
        if pin
            .definitions_version
            .is_some_and(|expected| expected != active_ver)
            || pin
                .artifact_id
                .as_deref()
                .is_some_and(|expected| expected != record.definition.id)
            || pin
                .version
                .as_deref()
                .is_some_and(|expected| expected != rev_str)
            || pin
                .digest
                .as_deref()
                .is_some_and(|expected| expected != record.definition.payload_sha256)
        {
            return Err(pin_mismatch());
        }
    }
    let started_version = run_definitions_version.ok_or_else(pin_unavailable)?;
    if started_version == active_ver {
        return Ok(());
    }
    let cell = host.cell()?;
    let reader = cell.store().read();
    let start_set = reader
        .installed_definitions(Some(started_version))
        .map_err(Refusal::host)?
        .ok_or_else(pin_unavailable)?;
    let active_hash = active_model_hash.ok_or_else(pin_unavailable)?;
    let start_retained = reader
        .retained_definition_at_version(&record.definition.id, started_version)
        .map_err(Refusal::host)?
        .ok_or_else(definition_changed)?;
    if start_retained.definition.kind != "question"
        || start_retained.definition.lifecycle != RetainedLifecycle::Active
        || start_retained.definition.revision != record.definition.revision
        || start_retained.definition.payload_sha256 != record.definition.payload_sha256
        || start_set.model_hash != active_hash
    {
        return Err(definition_changed());
    }
    Ok(())
}

fn verify_named_pin<E: SqlEngine>(
    host: &DevHost<E>,
    question_id: &str,
    plan: &QuestionPlan,
    installed_version: Option<u64>,
    active_model_hash: Option<&str>,
    run_definitions_version: Option<u64>,
    explicit: Option<&QuestionStepPin>,
) -> Result<(), Refusal> {
    if let Some(pin) = explicit
        && (pin
            .definitions_version
            .is_some_and(|expected| Some(expected) != installed_version)
            || pin.artifact_id.as_deref().is_some_and(|expected| {
                expected != question_id && expected != plan.question().name
            })
            || pin.version.as_deref().is_some_and(|expected| {
                installed_version.map(|v| v.to_string()).as_deref() != Some(expected)
            })
            || pin.digest.as_deref().is_some_and(|expected| {
                active_model_hash.map(|h| h.strip_prefix("sha256:").unwrap_or(h)) != Some(expected)
            }))
    {
        return Err(pin_mismatch());
    }
    let Some(active_ver) = installed_version else {
        return Ok(());
    };
    let started_version = run_definitions_version.ok_or_else(pin_unavailable)?;
    if started_version == active_ver {
        return Ok(());
    }
    let cell = host.cell()?;
    let start_set = cell
        .store()
        .read()
        .installed_definitions(Some(started_version))
        .map_err(Refusal::host)?
        .ok_or_else(pin_unavailable)?;
    let active_hash = active_model_hash.ok_or_else(pin_unavailable)?;
    if start_set.model_hash != active_hash {
        return Err(definition_changed());
    }
    Ok(())
}

/// Inspect required sources and watermarks to identify every stale, lagging,
/// missing, or incomplete `(source, dataset)` pair without disclosing cursor
/// tokens.
fn collect_stale_sources<E: SqlEngine>(
    store: &CellStore<E>,
    plan: &QuestionPlan,
    result: &QueryResult,
    guard: Option<&QuestionFreshnessGuard>,
) -> Result<Vec<String>, Refusal> {
    let required = store
        .required_sources_for_plan(MAIN_WORLD, plan)
        .map_err(Refusal::from_compute)?;
    let require_exact = guard.and_then(|g| g.require_exact).unwrap_or(false);
    let plan_age = match &plan.question().freshness {
        norn_ir::metric::FreshnessPolicy::Live { max_age_ms }
        | norn_ir::metric::FreshnessPolicy::Delayed { max_age_ms } => Some(max_age_ms.0),
        norn_ir::metric::FreshnessPolicy::AsOf { .. } => None,
    };
    let effective_age = match (plan_age, guard.and_then(|g| g.max_age_ms)) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    };
    let mut bad = BTreeSet::new();
    for (source, datasets) in &required.0 {
        if datasets.is_empty() {
            bad.insert(source.clone());
            continue;
        }
        let provided = result.context.source_cursors.get(source);
        for dataset in datasets {
            let has_cursor = provided.is_some_and(|parts| parts.contains_key(dataset));
            if !has_cursor {
                bad.insert(format!("{source}/{dataset}"));
            }
        }
    }
    for (source, partitions) in &result.context.source_cursors {
        if partitions.is_empty() {
            bad.insert(source.clone());
            continue;
        }
        for (partition, consumed) in partitions {
            let wm = result
                .context
                .watermarks
                .get(source)
                .and_then(|map| map.get(partition));
            let stale_or_incomplete = match wm {
                None => true,
                Some(w) => {
                    if !w.complete || &w.observed != consumed {
                        true
                    } else if let Some(at) = w.heartbeat_ms {
                        match result.context.now_ms.checked_sub(at) {
                            None => true,
                            Some(lag) => {
                                (require_exact && lag > 0)
                                    || effective_age.is_some_and(|max_age| lag > max_age)
                            }
                        }
                    } else {
                        true
                    }
                }
            };
            if stale_or_incomplete {
                bad.insert(format!("{source}/{partition}"));
            }
        }
    }
    Ok(bad.into_iter().collect())
}

fn check_freshness_guard(
    result: &QueryResult,
    guard: Option<&QuestionFreshnessGuard>,
    stale_sources: Vec<String>,
) -> Option<(&'static str, bool, Vec<String>)> {
    let require_exact = guard.and_then(|g| g.require_exact).unwrap_or(false);
    let step_max_age = guard.and_then(|g| g.max_age_ms);
    let age_exceeded = step_max_age.is_some_and(|max_age| {
        result
            .context
            .watermarks
            .values()
            .flat_map(BTreeMap::values)
            .any(|wm| {
                wm.heartbeat_ms
                    .and_then(|at| result.context.now_ms.checked_sub(at))
                    .is_none_or(|lag| lag > max_age)
            })
    });
    if !result.complete || matches!(result.freshness, Freshness::Incomplete) {
        return Some(("incomplete", result.complete, stale_sources));
    }
    if matches!(result.freshness, Freshness::Stale) || age_exceeded {
        return Some(("stale", result.complete, stale_sources));
    }
    if require_exact && result.freshness != Freshness::Exact {
        return Some(("fresh", result.complete, stale_sources));
    }
    None
}

fn engine_str(engine: Engine) -> &'static str {
    match engine {
        Engine::Cell => "cell",
        Engine::Ivm => "ivm",
        Engine::Duckdb => "duckdb",
        Engine::DataFusion => "datafusion",
    }
}

fn freshness_str(freshness: Freshness) -> &'static str {
    match freshness {
        Freshness::Exact => "exact",
        Freshness::Fresh => "fresh",
        Freshness::Stale => "stale",
        Freshness::Incomplete => "incomplete",
    }
}

fn encode_u64(n: u64) -> Result<RuntimeValue, Refusal> {
    norn_ir::runtime::encode(&n).map_err(Refusal::host)
}

fn encode_query_row(
    bucket_key: &str,
    row: &norn_cell::compute::QueryRow,
) -> Result<RuntimeValue, Refusal> {
    let member_count = row
        .member_count
        .unwrap_or_else(|| u64::try_from(row.members.len()).unwrap_or(u64::MAX));
    let totals = row
        .totals
        .iter()
        .map(|d| RuntimeValue::String(d.to_string()))
        .collect();
    let value = row
        .value
        .as_ref()
        .map_or(RuntimeValue::Null, |d| RuntimeValue::String(d.to_string()));
    Ok(RuntimeValue::Object(BTreeMap::from([
        (
            "bucketKey".to_owned(),
            RuntimeValue::String(bucket_key.to_owned()),
        ),
        ("value".to_owned(), value),
        ("totals".to_owned(), RuntimeValue::Array(totals)),
        ("memberCount".to_owned(), encode_u64(member_count)?),
        ("suppressed".to_owned(), RuntimeValue::Bool(row.suppressed)),
    ])))
}

fn build_answered_output(question_id: &str, result: &QueryResult) -> Result<StepOutcome, Refusal> {
    let mut rows = Vec::with_capacity(result.rows.len());
    for (bucket_key, row) in &result.rows {
        rows.push(encode_query_row(bucket_key, row)?);
    }
    let scalar_value = if result.rows.len() == 1 {
        result
            .rows
            .values()
            .next()
            .and_then(|row| row.value.as_ref())
            .map_or(RuntimeValue::Null, |d| RuntimeValue::String(d.to_string()))
    } else {
        RuntimeValue::Null
    };
    let revision = RuntimeValue::Object(BTreeMap::from([
        ("seq".to_owned(), encode_u64(result.revision.seq)?),
        (
            "hash".to_owned(),
            RuntimeValue::String(result.revision.hash.clone()),
        ),
        ("commits".to_owned(), encode_u64(result.revision.commits)?),
    ]));
    let row_count = u64::try_from(result.rows.len()).unwrap_or(u64::MAX);
    let authority_pin = format!(
        "{}@{}",
        result.context.population_pin, result.context.authorization_epoch
    );
    let output: RuntimeObject = BTreeMap::from([
        (
            "status".to_owned(),
            RuntimeValue::String("Answered".to_owned()),
        ),
        (
            "question".to_owned(),
            RuntimeValue::String(question_id.to_owned()),
        ),
        (
            "grain".to_owned(),
            RuntimeValue::String(result.grain.0.clone()),
        ),
        (
            "engine".to_owned(),
            RuntimeValue::String(engine_str(result.engine).to_owned()),
        ),
        (
            "freshness".to_owned(),
            RuntimeValue::String(freshness_str(result.freshness).to_owned()),
        ),
        ("complete".to_owned(), RuntimeValue::Bool(result.complete)),
        (
            "queryPin".to_owned(),
            RuntimeValue::String(result.query_pin.clone()),
        ),
        (
            "definitionPin".to_owned(),
            RuntimeValue::String(result.definition_pin.clone()),
        ),
        (
            "authorityPin".to_owned(),
            RuntimeValue::String(authority_pin),
        ),
        (
            "readToken".to_owned(),
            RuntimeValue::String(result.lineage_ref.clone()),
        ),
        ("revision".to_owned(), revision),
        (
            "effectiveAsOf".to_owned(),
            RuntimeValue::String(result.effective_as_of.0.clone()),
        ),
        ("knownAsOf".to_owned(), encode_u64(result.known_as_of)?),
        ("nowMs".to_owned(), encode_u64(result.context.now_ms)?),
        ("rowCount".to_owned(), encode_u64(row_count)?),
        ("value".to_owned(), scalar_value),
        ("rows".to_owned(), RuntimeValue::Array(rows)),
    ]);
    let encoded_size = RuntimeValue::Object(output.clone())
        .canonical_json()
        .map_err(Refusal::host)?
        .len();
    if !u64::try_from(encoded_size)
        .is_ok_and(|size| size <= norn_ir::function::MAX_RECEIPT_SUMMARY_BYTES)
    {
        return Err(output_too_large());
    }
    Ok(StepOutcome::Output(output))
}
