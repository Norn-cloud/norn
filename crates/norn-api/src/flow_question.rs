//! QUESTION-STEP-1: read-only evaluation of parked `norn.question@1` flow steps
//! through [`crate::question_routes::QuestionRuntime`] under the run starter's authority, with
//! `STEP-PIN-1` dependency-closure pinning, `PRIVACY-1`/`PRIVACY-3`/`PRIVACY-4`
//! disclosure guards, and `#1905` `RequiredSources` freshness enforcement.
//!
//! A `norn.question@1` step never mutates business state: it reads a bounded
//! snapshot through the shared read authority, refuses unauthorized readers or
//! restricted markings with `Denied` (never leaking values), refuses stale or
//! incomplete required sources with typed `StaleSource`, and refuses pin
//! mismatches, undeclared/predicate-replacing params, or other deterministic
//! errors with `Refused { code }`.

mod freshness;
mod pin;

use std::collections::{BTreeMap, BTreeSet};

use http::StatusCode;
use norn_cell::compute::{ALL_MEMBERS, QueryResult};
use norn_cell::engine::SqlEngine;
use norn_cell::flow_run::{QuestionStep, RunQuestionRead, RunQuestionSource, RunStarter};
use norn_cell::{ObjectRecord, StoreReader};
use norn_checker::metric::compute::QuestionPlan;
use norn_ir::{QuestionIR, RuntimeValue, UtcTimestamp};
use norn_kernel::flow_turn::StepOutcome;

use crate::authz::{ReadCapability, ReadRequest, read_denied};
use crate::flow_driver::AsStarter;
use crate::host::DevHost;
use crate::question_routes::{
    MARKED_HISTORY, QuestionAnswer, QuestionRuntime, admit_named_question, allowed,
    answer_plan_inner, check_ad_hoc, handle_key, mapped,
};
use crate::refusal::{Refusal, RefusalCategory};
use crate::scope::ScopeQuery;

use self::freshness::{
    build_answered_output, denied_failure, evaluate_freshness_guard, refused_failure, stale_failure,
};
use self::pin::{verify_named_pin, verify_retained_pin};

/// Stable `question_step.*` refusal codes for parked `norn.question@1` steps.
pub mod question_step_codes {
    /// The question definition or any artifact in its dependency closure
    /// changed or was retired after the flow run started (`STEP-PIN-1`).
    pub const DEFINITION_CHANGED: &str = "question_step.definition_changed";
    /// The active question definition or installed definitions version does not
    /// match the explicit `pin` carried by the `norn.question@1` step (`STEP-PIN-1`).
    pub const PIN_MISMATCH: &str = "question_step.pin_mismatch";
    /// The retained definitions set for the flow run's starting definitions
    /// version is unavailable or unreadable (`STEP-PIN-1`).
    pub const PIN_UNAVAILABLE: &str = "question_step.pin_unavailable";
    /// This cell's question runtime does not publish ad-hoc/retained questions.
    pub const UNAVAILABLE: &str = "question_step.unavailable";
    /// Dynamic `params` were supplied for a question that declares no parameter
    /// bindings.
    pub const UNSUPPORTED_PARAMS: &str = "question_step.unsupported_params";
    /// A `params` entry has a non-scalar value, invalid name, or attempts to
    /// replace an admitted question predicate.
    pub const INVALID_PARAM: &str = "question_step.invalid_param";
    /// The step's `asOf` timestamp is invalid or disagrees with a pre-admitted
    /// named question's effective cutoff.
    pub const AS_OF_MISMATCH: &str = "question_step.as_of_mismatch";
    /// The serialized `Answered` output exceeds `MAX_RECEIPT_SUMMARY_BYTES`.
    pub const OUTPUT_TOO_LARGE: &str = "question_step.output_too_large";
    /// Required external sources are stale or incomplete (`QUERY-FRESH-1`).
    pub const STALE_SOURCE: &str = "core.compute.stale_input";
}

/// Same-snapshot evidence collected inside `question_routes::answer_plan_inner`
/// under one held cell lock and snapshot bracket.
#[derive(Default)]
pub(crate) struct StepEvidence {
    pub(crate) sources_authorized: bool,
    pub(crate) required_sources: norn_cell::compute::RequiredSources,
    pub(crate) has_marked_evidence: bool,
    pub(crate) has_restricted_bucket: bool,
    pub(crate) has_connection_type: bool,
    pub(crate) inherited_sources: Vec<RunQuestionSource>,
}

/// Authenticated evaluation bundle for one parked `norn.question@1` step,
/// captured under one cell lock and verified against one snapshot.
pub(crate) struct QuestionStepEval {
    pub(crate) disclosed: crate::privacy::DisclosedAnswer,
    pub(crate) all_values_authorized: bool,
    pub(crate) evidence: StepEvidence,
}

/// Coordinates and inputs for one parked `norn.question@1` step.
pub(crate) struct QuestionStepContext<'a> {
    pub(crate) input: &'a RuntimeValue,
    pub(crate) starter: Option<&'a RunStarter>,
    pub(crate) run_definitions_version: Option<u64>,
}

/// Result of dispatching one parked `norn.question@1` step: the typed outcome
/// to settle, plus the transitive source object types read (persisted into
/// `_norn_flow_run_source` so Run readers must also hold inherited clearance on
/// the question's direct and transitive sources, and `Sources` clearance when
/// `StaleSource.sources` or an inherited source requires it).
pub(crate) struct QuestionDispatch {
    pub(crate) outcome: StepOutcome,
    pub(crate) sources: Vec<RunQuestionSource>,
}

impl QuestionDispatch {
    const fn failure(outcome: StepOutcome) -> Self {
        Self {
            outcome,
            sources: Vec::new(),
        }
    }
}

/// Build the settling ledger events that persist `sources` (partitioned by
/// `requires_sources`) into `_norn_flow_run_source`.
pub(crate) fn question_read_events(
    run_id: &str,
    object_type: &str,
    sources: &[RunQuestionSource],
) -> Result<Vec<RuntimeValue>, Refusal> {
    let mut plain = Vec::new();
    let mut with_sources = Vec::new();
    for req in sources {
        if req.requires_sources {
            with_sources.push(req.source_type.clone());
        } else {
            plain.push(req.source_type.clone());
        }
    }
    let mut events = Vec::new();
    for (source_types, requires_sources) in [(plain, false), (with_sources, true)] {
        if !source_types.is_empty() {
            events.push(
                RunQuestionRead {
                    run_id: run_id.to_owned(),
                    object_type: object_type.to_owned(),
                    source_types,
                    requires_sources,
                }
                .event()
                .map_err(Refusal::host)?,
            );
        }
    }
    Ok(events)
}

pub(crate) fn pin_mismatch() -> Refusal {
    Refusal::new(
        StatusCode::CONFLICT,
        question_step_codes::PIN_MISMATCH,
        "the active question definition does not match the parked flow step's pin",
        "retarget the step pin or install the pinned definition version",
    )
}

pub(crate) fn definition_changed() -> Refusal {
    Refusal::new(
        StatusCode::CONFLICT,
        question_step_codes::DEFINITION_CHANGED,
        "the parked flow step's question definition changed after the run started",
        "start a new run or restore a compatible question definition before retrying",
    )
}

pub(crate) fn pin_unavailable() -> Refusal {
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

pub(crate) fn question_not_found() -> Refusal {
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
        "this question declares no dynamic parameter bindings",
        "omit params or declare an explicit parameterized question contract",
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

pub(crate) fn output_too_large() -> Refusal {
    Refusal::new(
        StatusCode::PAYLOAD_TOO_LARGE,
        question_step_codes::OUTPUT_TOO_LARGE,
        "the question step answer exceeds the receipt summary size bound",
        "narrow the question's grouping or time window",
    )
}

/// Evaluate one parked `norn.question@1` step read-only under `starter`'s
/// authority (`as_starter`) and return the typed [`QuestionDispatch`] to settle.
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
) -> Result<QuestionDispatch, Refusal> {
    let parsed = match norn_cell::flow_run::parse_question_step(ctx.input) {
        Ok(step) => step,
        Err(refused) => return Ok(QuestionDispatch::failure(refused_failure(refused.code()))),
    };
    let Some(starter) = ctx.starter else {
        return Ok(QuestionDispatch::failure(denied_failure()));
    };
    let mut dispatch: Option<QuestionDispatch> = None;
    let mut run_read_only = || -> Result<(), Refusal> {
        dispatch = Some(evaluate_as_starter(
            host,
            &parsed,
            starter,
            ctx.run_definitions_version,
            disclosure,
        )?);
        Ok(())
    };
    match as_starter(starter, &mut run_read_only) {
        None => Ok(QuestionDispatch::failure(denied_failure())),
        Some(Ok(())) => dispatch.ok_or_else(|| Refusal::host("question step produced no outcome")),
        Some(Err(refused)) => classify_refusal(refused).map(QuestionDispatch::failure),
    }
}

fn classify_refusal(refused: Refusal) -> Result<StepOutcome, Refusal> {
    if is_marking_or_disclosure_refusal(&refused) {
        return Ok(denied_failure());
    }
    if refused.code == question_step_codes::STALE_SOURCE {
        return Ok(stale_failure("incomplete", false, Vec::new(), false));
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

/// Collect same-snapshot step evidence only when `for_step` is true and
/// `disclosed` succeeded, keeping `question_routes::answer_plan_inner` compact.
pub(crate) fn maybe_step_evidence<E: SqlEngine>(
    for_step: bool,
    cell: (
        &DevHost<E>,
        &dyn QuestionRuntime<E>,
        &norn_cell::CellStore<E>,
    ),
    ctx: (&str, &str, &str, &QuestionPlan),
    time: (Option<norn_cell::KnowledgeCutoff>, u64),
    disclosed: &Result<Result<crate::privacy::DisclosedAnswer, Refusal>, norn_cell::CellError>,
) -> Result<StepEvidence, Refusal> {
    if let (Ok(Ok(answer)), true) = (disclosed, for_step) {
        collect_step_evidence(cell, ctx, time, answer)
    } else {
        Ok(StepEvidence::default())
    }
}

/// Collect same-snapshot `Sources` authorization, `RequiredSources`, compact
/// `ALL_MEMBERS` `Values` authorization, and marking evidence inside
/// `question_routes::answer_plan_inner`'s held cell lock (no second store read
/// and no post-snapshot authorization race).
pub(crate) fn collect_step_evidence<E: SqlEngine>(
    (host, runtime, store): (
        &DevHost<E>,
        &dyn QuestionRuntime<E>,
        &norn_cell::CellStore<E>,
    ),
    (actor, question, world, plan): (&str, &str, &str, &QuestionPlan),
    (cutoff, now_ms): (Option<norn_cell::KnowledgeCutoff>, u64),
    disclosed: &crate::privacy::DisclosedAnswer,
) -> Result<StepEvidence, Refusal> {
    let configured = runtime.configured_connections();
    let required_sources = store
        .required_sources_for_plan_at(world, plan, cutoff, now_ms, &configured)
        .map_err(Refusal::from_compute)?;
    let (result, hist_at) = match disclosed.as_answer() {
        QuestionAnswer::Live(res) => (res, None),
        QuestionAnswer::Historical(hist) => (
            &hist.result,
            Some((hist.result.known_as_of, hist.result.context.now_ms)),
        ),
    };
    let sources_authorized = crate::privacy::source_grants::step_sources_granted(
        host,
        runtime,
        store,
        (actor, question, plan),
        (result, &required_sources, hist_at),
    )?;
    let has_marked_evidence = has_step_marked_evidence(host, &store.read(), world, plan, result)?;
    let has_restricted_bucket = has_restricted_or_unproven_bucket(
        host,
        runtime,
        actor,
        question,
        plan.grain().as_str(),
        result,
    )?;
    let inherited_sources = collect_inherited_sources(store, world, plan, result)?;
    Ok(StepEvidence {
        sources_authorized,
        required_sources,
        has_marked_evidence,
        has_restricted_bucket,
        has_connection_type: store.model().has_type("source.Connection"),
        inherited_sources,
    })
}

fn collect_inherited_sources<E: SqlEngine>(
    store: &norn_cell::CellStore<E>,
    world: &str,
    plan: &QuestionPlan,
    result: &QueryResult,
) -> Result<Vec<RunQuestionSource>, Refusal> {
    let storage_world = if store.fork_pin().is_some() {
        norn_cell::MAIN_WORLD
    } else {
        world
    };
    let reader = store.read();
    let mut merged: BTreeMap<String, bool> = BTreeMap::new();
    for ty in plan_source_types(plan) {
        for req in reader
            .object_type_question_source_types(storage_world, &ty)
            .map_err(Refusal::host)?
        {
            *merged.entry(req.source_type).or_insert(false) |= req.requires_sources;
        }
    }
    let grain = plan.grain().as_str();
    for row in result.rows.values() {
        for id in row.members.keys() {
            for req in reader
                .run_question_source_types(storage_world, grain, id)
                .map_err(Refusal::host)?
            {
                *merged.entry(req.source_type).or_insert(false) |= req.requires_sources;
            }
        }
    }
    Ok(merged
        .into_iter()
        .map(|(source_type, requires_sources)| RunQuestionSource {
            source_type,
            requires_sources,
        })
        .collect())
}

/// One parked, unsettled `norn.question@1` step queued for timer/alarm wake
/// dispatch (`QUESTION-STEP-1`).
pub(crate) struct PendingQuestionStep {
    pub(crate) command_id: String,
    pub(crate) generation: u64,
    pub(crate) run_id: String,
    pub(crate) due_at_ms: u64,
}

/// Every parked `norn.question@1` command whose result has not yet been
/// recorded, for durable timer/alarm wake scheduling and dispatch.
pub(crate) fn pending_question_steps<E: SqlEngine>(
    cell: &crate::host::ActionCell<E>,
) -> Result<Vec<PendingQuestionStep>, Refusal> {
    let reader = cell.store().read();
    let mut found = Vec::new();
    for pending in reader.parked_commands().map_err(Refusal::host)? {
        if pending.result.is_some() {
            continue;
        }
        let Some(payload) = pending.command.command.get("payload") else {
            continue;
        };
        if payload.get("function").and_then(RuntimeValue::as_str)
            != Some(norn_cell::flow_run::QUESTION_FUNCTION)
        {
            continue;
        }
        let Some(run_id) = payload.get("runId").and_then(RuntimeValue::as_str) else {
            continue;
        };
        let due_at_ms = reader
            .flow_run(norn_cell::MAIN_WORLD, run_id)
            .map_err(Refusal::host)?
            .map_or(0, |row| row.updated_at_ms);
        found.push(PendingQuestionStep {
            command_id: pending.command.command_id,
            generation: pending.command.generation,
            run_id: run_id.to_owned(),
            due_at_ms,
        });
    }
    Ok(found)
}

/// PRIVACY-1 / PRIVACY-3 / PRIVACY-4: inspect contributing rows and datasets
/// inside the evaluation snapshot for occurrence markings, missing marking
/// snapshots, dataset manifest markings, or marked mirror state on compact
/// analytics buckets. Any restricted or unproven marking fails closed (`true`
/// -> `Denied`) so marked values are never persisted into unmarked Run slots.
pub(crate) fn has_step_marked_evidence<E: SqlEngine>(
    host: &DevHost<E>,
    reader: &StoreReader<'_, E>,
    world: &str,
    plan: &QuestionPlan,
    result: &QueryResult,
) -> Result<bool, Refusal> {
    let catalog = host.marking_catalog(reader)?;
    let has_any_markings =
        !catalog.is_empty() || reader.stores_markings().map_err(Refusal::host)?;
    if !has_any_markings {
        return Ok(false);
    }
    let grain = plan.grain().as_str();
    let mut source_types = BTreeSet::from([grain]);
    for sum in plan.sums() {
        if let Some(link) = &sum.time_link {
            source_types.insert(link.target_type.as_str());
        }
    }
    for ty in &source_types {
        if let Some(current) = reader.dataset_manifest(world, ty).map_err(Refusal::host)?
            && !reader
                .effective_dataset_markings(&current.manifest)
                .map_err(Refusal::host)?
                .is_empty()
        {
            return Ok(true);
        }
    }
    for row in result.rows.values() {
        if row.member_count.is_some() && row.members.is_empty() {
            // Compact analytics bucket: require every touched source type to be
            // provably unlabeled when any marking catalog/label exists.
            if source_types
                .iter()
                .any(|ty| !crate::privacy::disclosure::mirror_unlabeled(reader, ty))
            {
                return Ok(true);
            }
            continue;
        }
        for (id, member) in &row.members {
            let current =
                reader.version(world, grain, id).map_err(Refusal::host)? == member.version;
            let version = (!current).then_some(member.version);
            let Some(occurrence) = reader
                .fact_occurrence_markings(world, grain, id, version)
                .map_err(Refusal::host)?
            else {
                return Ok(true);
            };
            if !occurrence.is_empty() {
                return Ok(true);
            }
            if reader
                .effective_markings(world, grain, id, version)
                .map_err(Refusal::host)?
                .is_none()
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn evaluate_as_starter<E: SqlEngine>(
    host: &DevHost<E>,
    step: &QuestionStep,
    starter: &RunStarter,
    run_definitions_version: Option<u64>,
    disclosure: &dyn QuestionRuntime<E>,
) -> Result<QuestionDispatch, Refusal> {
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

    let (plan, eval) = if has_retained_question {
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

    let result = match eval.disclosed.as_answer() {
        QuestionAnswer::Live(res) => res,
        QuestionAnswer::Historical(hist) => &hist.result,
    };

    // PRIVACY-1 / PRIVACY-3 / PRIVACY-4 / DATA-VALUES-1:
    // Require explicit Values authorization evidence across ALL members (and
    // `ALL_MEMBERS` for compact analytics buckets, checked inside the same held
    // snapshot bracket), zero suppressed/restricted/withheld buckets, and zero
    // restricted markings on contributing rows or datasets.
    if !eval.all_values_authorized || eval.evidence.has_marked_evidence {
        return Ok(QuestionDispatch::failure(denied_failure()));
    }

    // QUERY-FRESH-1 (#1905): enforce model-derived RequiredSources freshness
    // strictly from the SAME authenticated answer snapshot (no second store read).
    // Persist inherited source permissions on StaleSource failure too (requiring
    // `Sources` whenever `StaleSource.sources` emitted source IDs) so later Run
    // readers without clearance cannot read leaked source IDs or freshness state.
    if let Some((stale, emitted_sources)) = evaluate_freshness_guard(
        &plan.question().freshness,
        result,
        &eval.evidence.required_sources,
        step.freshness.as_ref(),
        eval.evidence.sources_authorized,
    ) {
        let sources = merge_dispatch_sources(
            &plan,
            eval.evidence.inherited_sources,
            emitted_sources,
            emitted_sources && eval.evidence.has_connection_type,
        );
        return Ok(QuestionDispatch {
            outcome: stale,
            sources,
        });
    }

    let outcome = build_answered_output(&step.question, result)?;
    let sources = merge_dispatch_sources(&plan, eval.evidence.inherited_sources, false, false);
    Ok(QuestionDispatch { outcome, sources })
}

fn merge_dispatch_sources(
    plan: &QuestionPlan,
    inherited: Vec<RunQuestionSource>,
    requires_sources: bool,
    include_connection: bool,
) -> Vec<RunQuestionSource> {
    let mut merged: BTreeMap<String, bool> = BTreeMap::new();
    for ty in plan_source_types(plan) {
        *merged.entry(ty).or_insert(false) |= requires_sources;
    }
    if include_connection {
        merged.insert("source.Connection".to_owned(), true);
    }
    for req in inherited {
        *merged.entry(req.source_type).or_insert(false) |= req.requires_sources;
    }
    merged
        .into_iter()
        .map(|(source_type, req_sources)| RunQuestionSource {
            source_type,
            requires_sources: req_sources,
        })
        .collect()
}

fn has_restricted_or_unproven_bucket<E: SqlEngine>(
    host: &DevHost<E>,
    disclosure: &dyn QuestionRuntime<E>,
    actor: &str,
    question: &str,
    grain: &str,
    result: &QueryResult,
) -> Result<bool, Refusal> {
    let mut has_compact = false;
    for row in result.rows.values() {
        if row.suppressed
            || row
                .members
                .values()
                .any(|member| member.restricted || member.amounts.is_none())
        {
            return Ok(true);
        }
        if row.member_count.is_some() && row.members.is_empty() {
            has_compact = true;
        }
    }
    if has_compact {
        let all_members = ObjectRecord {
            object_type: grain.to_owned(),
            id: ALL_MEMBERS.to_owned(),
            scope: host.scope().key(),
            version: 0,
            values: BTreeMap::new(),
        };
        let permitted = allowed(
            host,
            disclosure,
            actor,
            question,
            &all_members,
            ReadCapability::Values,
        )
        .map_err(Refusal::from_compute)?;
        if !permitted {
            return Ok(true);
        }
    }
    Ok(false)
}

fn plan_source_types(plan: &QuestionPlan) -> Vec<String> {
    let mut types = BTreeSet::from([plan.grain().as_str().to_owned()]);
    for sum in plan.sums() {
        if let Some(link) = &sum.time_link {
            types.insert(link.target_type.as_str().to_owned());
        }
    }
    for def in plan.definitions() {
        if let Some(agg) = &def.type_params.metric_aggregation_params {
            types.insert(agg.semantic_model.0.clone());
        }
    }
    types.into_iter().collect()
}

fn load_active_retained<E: SqlEngine>(
    host: &DevHost<E>,
    question_id: &str,
) -> Result<
    (
        norn_cell::retained::RetainedDefinitionRecord,
        Option<u64>,
        Option<norn_cell::InstalledDefinitionSet>,
    ),
    Refusal,
> {
    let cell = host.cell()?;
    let reader = cell.store().read();
    let record = reader
        .retained_definition(question_id, None)
        .map_err(Refusal::host)?
        .filter(|rec| rec.definition.kind == "question")
        .ok_or_else(question_not_found)?;
    let installed_version = reader.active_definitions_version().map_err(Refusal::host)?;
    let active_set = reader.installed_definitions(None).map_err(Refusal::host)?;
    Ok((record, installed_version, active_set))
}

fn evaluate_retained_question<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &ScopeQuery,
    starter: &RunStarter,
    step: &QuestionStep,
    run_definitions_version: Option<u64>,
    disclosure: &dyn QuestionRuntime<E>,
    key: &[u8],
) -> Result<(QuestionPlan, QuestionStepEval), Refusal> {
    let ad_hoc = disclosure
        .as_ad_hoc()
        .filter(|runtime| runtime.publishes_questions())
        .ok_or_else(question_unavailable)?;
    let (initial_record, initial_version) = {
        let _epoch = host.definition_read()?;
        let (record, version, _) = load_active_retained(host, &step.question)?;
        (record, version)
    };
    let mut question_ir: QuestionIR =
        serde_json::from_str(&initial_record.definition.payload).map_err(Refusal::host)?;

    // Authorize starter Population and Values BEFORE evaluating retained
    // overrides / parameter collisions, checking pins, or revealing retirement
    // (zero existence or predicate-collision oracle for unauthorized callers).
    let mut admitted = check_ad_hoc(host, scope, &starter.subject, &question_ir, ad_hoc)?;
    require_values_access(
        host,
        disclosure,
        &starter.subject,
        &question_ir.object_set.0,
    )?;

    apply_step_overrides(&mut question_ir, step)?;
    if step.as_of.is_some() {
        admitted = check_ad_hoc(host, scope, &starter.subject, &question_ir, ad_hoc)?;
    }

    // Hold one definition_read epoch across STEP-PIN-1 re-verification and evaluation.
    let _epoch = host.definition_read()?;
    let (record, installed_version, active_set) = load_active_retained(host, &step.question)?;
    if installed_version != initial_version
        || record.definition.revision != initial_record.definition.revision
        || record.definition.payload_sha256 != initial_record.definition.payload_sha256
    {
        return Err(definition_changed());
    }
    let plan = admit_plan_or_refuse(admitted)?;
    verify_retained_pin(
        host,
        &record,
        &plan,
        installed_version,
        active_set.as_ref(),
        run_definitions_version,
        step.pin.as_ref(),
        step.as_of.as_deref(),
        ad_hoc.max_rows(),
    )?;
    require_values_access(host, disclosure, &starter.subject, plan.grain().as_str())?;

    let eval = answer_plan_inner(
        host,
        disclosure,
        &starter.subject,
        &step.question,
        &plan,
        key,
        true,
    )?;
    let buckets = match eval.disclosed.as_answer() {
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
    Ok((plan, eval))
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
) -> Result<(QuestionPlan, QuestionStepEval), Refusal> {
    // `admit_named_question` authorizes Population and acquires one definition_read epoch.
    let (_epoch, actor, key, plan) = admit_named_question(
        host,
        scope,
        Some(&starter.subject),
        &step.question,
        disclosure,
    )?;
    require_values_access(host, disclosure, actor, &step.question)?;

    let (installed_version, active_set) = {
        let cell = host.cell()?;
        let reader = cell.store().read();
        let installed_version = reader.active_definitions_version().map_err(Refusal::host)?;
        let active_set = reader.installed_definitions(None).map_err(Refusal::host)?;
        (installed_version, active_set)
    };
    let max_rows = disclosure.as_ad_hoc().map_or(
        norn_ir::QuestionLimits::DEFAULT.max_scanned_records,
        crate::question_routes::AdHocQuestions::max_rows,
    );
    verify_named_pin(
        host,
        &step.question,
        &plan,
        installed_version,
        active_set.as_ref(),
        run_definitions_version,
        step.pin.as_ref(),
        max_rows,
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

    let eval = answer_plan_inner(host, disclosure, actor, &step.question, &plan, key, true)?;
    Ok((plan, eval))
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

/// Strictly enforce declared parameter bindings and `asOf` cutoffs on retained
/// `QuestionIR` definitions:
/// - Never overwrite or replace an admitted `QuestionIR` filter predicate.
/// - Refuse any undeclared parameter binding fail-closed (`QuestionIR` carries
///   no declared parameter slots).
fn apply_step_overrides(question_ir: &mut QuestionIR, step: &QuestionStep) -> Result<(), Refusal> {
    if let Some(as_of) = step.as_of.as_deref() {
        if norn_checker::transform::instant(as_of).is_none() {
            return Err(as_of_mismatch());
        }
        question_ir.time_window.end = UtcTimestamp(as_of.to_owned());
    }
    if let Some(params) = &step.params
        && !params.is_empty()
    {
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
            if question_ir.filters.iter().any(|existing| {
                existing.path.links.is_empty() && existing.path.property.as_str() == key
            }) {
                return Err(invalid_param(
                    "parameter bindings cannot replace or override admitted question predicates",
                ));
            }
        }
        return Err(unsupported_params());
    }
    Ok(())
}

#[cfg(all(test, not(target_arch = "wasm32")))]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "compact-bucket privacy unit tests"
)]
mod tests {
    use super::*;
    use crate::authz::{AccessRequest, Authorizer};
    use crate::question_routes::QuestionSnapshot;
    use crate::scope::CellScope;
    use norn_cell::compute::{Engine, Freshness, QueryContext, QueryRow};
    use norn_cell::engine::NativeEngine;
    use norn_cell::{CellStore, LedgerHead, Model};
    use norn_ir::{Decimal, ObjectTypeName};
    use norn_kernel::authz::{
        self, ActorKind, Binding, ResourceInput, ScopeKey, UnboundProfile, VerifiedPrincipal,
    };

    struct CompactAuthz {
        allow_all_members_values: bool,
    }

    impl Authorizer for CompactAuthz {
        fn authorize(&self, _request: &AccessRequest<'_>) -> Result<(), Refusal> {
            Ok(())
        }

        fn read(&self, request: &ReadRequest<'_>) -> Result<(), Refusal> {
            if request.capability == ReadCapability::Values
                && request.access.resource().id == ALL_MEMBERS
                && !self.allow_all_members_values
            {
                return Err(read_denied());
            }
            Ok(())
        }
    }

    struct StubRuntime;

    impl QuestionRuntime<NativeEngine> for StubRuntime {
        fn plan(&self, _question: &str) -> Result<QuestionPlan, Refusal> {
            Err(question_not_found())
        }

        fn snapshot(
            &self,
            _store: &CellStore<NativeEngine>,
            _plan: &QuestionPlan,
            _actor: &str,
        ) -> Result<QuestionSnapshot, Refusal> {
            Err(read_denied())
        }

        fn access(
            &self,
            scope: &CellScope,
            actor: &str,
            question: &str,
            capability: ReadCapability,
            row: Option<&ObjectRecord>,
        ) -> Result<authz::AccessRequest, Refusal> {
            let (object_type, id) = row.map_or_else(
                || ("canon.OrderLine".to_owned(), question.to_owned()),
                |rec| (rec.object_type.clone(), rec.id.clone()),
            );
            let scope_key = ScopeKey {
                org: scope.org_id.clone(),
                environment: Some(scope.environment_id.clone()),
                tenant: None,
                workspace: None,
                business_unit: None,
            };
            let action = match capability {
                ReadCapability::Population => "norn.read.population",
                ReadCapability::Values => "norn.read.values",
                ReadCapability::Sources => "norn.read.sources",
            };
            Ok(authz::AccessRequest::new(
                &VerifiedPrincipal::new(
                    actor.to_owned(),
                    ActorKind::User,
                    None,
                    vec!["member".to_owned()],
                    BTreeMap::new(),
                ),
                Some(scope_key.clone()),
                action,
                action,
                ResourceInput {
                    object_type,
                    id,
                    scope: scope_key,
                    attributes: BTreeMap::new(),
                },
                BTreeMap::new(),
                Binding::Unbound(UnboundProfile::HostedAction),
            ))
        }

        fn min_disclosed_bucket(&self) -> usize {
            3
        }

        fn handle_key(&self) -> &[u8] {
            &[7_u8; 32]
        }
    }

    fn compact_result() -> QueryResult {
        QueryResult {
            query_pin: "qp".to_owned(),
            definition_pin: "dp".to_owned(),
            engine: Engine::DataFusion,
            revision: LedgerHead::genesis(),
            effective_as_of: UtcTimestamp("2026-08-06T12:00:00Z".to_owned()),
            known_as_of: 1,
            grain: ObjectTypeName("canon.OrderLine".to_owned()),
            lineage_ref: "lr".to_owned(),
            rows: BTreeMap::from([(
                "2026-08-06".to_owned(),
                QueryRow {
                    bucket: RuntimeValue::String("2026-08-06".to_owned()),
                    totals: vec!["100".parse::<Decimal>().unwrap()],
                    value: Some("100".parse::<Decimal>().unwrap()),
                    members: BTreeMap::new(),
                    suppressed: false,
                    null_counts: Vec::new(),
                    member_count: Some(10),
                    membership_digest: Some("digest".to_owned()),
                },
            )]),
            freshness: Freshness::Exact,
            complete: true,
            context: QueryContext {
                world: "main".to_owned(),
                scope: "org_demo/dev".to_owned(),
                population_pin: "pop".to_owned(),
                authorization_epoch: 1,
                mapping_pin: "mp".to_owned(),
                now_ms: 10_000,
                source_cursors: BTreeMap::new(),
                watermarks: BTreeMap::new(),
            },
            exclusions: BTreeMap::new(),
            unlinked: 0,
            source_disagreements: None,
        }
    }

    #[test]
    fn compact_analytics_bucket_requires_all_members_values_authorization() {
        let scope = CellScope {
            org_id: "org_demo".to_owned(),
            environment_id: "dev".to_owned(),
        };
        let result = compact_result();
        let runtime = StubRuntime;

        let denied_store =
            CellStore::open_in_memory(Model::from_definitions(&[], &[]).unwrap()).unwrap();
        let denied_host =
            DevHost::from_store(denied_store, scope.clone(), |cmd| cmd.payload.clone())
                .unwrap()
                .with_authorizer(CompactAuthz {
                    allow_all_members_values: false,
                });
        assert!(
            has_restricted_or_unproven_bucket(
                &denied_host,
                &runtime,
                "viewer@demo",
                "q.compact",
                "canon.OrderLine",
                &result,
            )
            .unwrap(),
            "compact bucket with member_count >= min_disclosed must still deny when ALL_MEMBERS Values is unauthorized"
        );

        let allowed_store =
            CellStore::open_in_memory(Model::from_definitions(&[], &[]).unwrap()).unwrap();
        let allowed_host = DevHost::from_store(allowed_store, scope, |cmd| cmd.payload.clone())
            .unwrap()
            .with_authorizer(CompactAuthz {
                allow_all_members_values: true,
            });
        assert!(
            !has_restricted_or_unproven_bucket(
                &allowed_host,
                &runtime,
                "admin@demo",
                "q.compact",
                "canon.OrderLine",
                &result,
            )
            .unwrap()
        );
    }
}
