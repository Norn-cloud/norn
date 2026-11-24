//! Execution engine, Inbox proposal parking, ledger run history, and commit /
//! alarm hooks for `AUTOMATION-1`.

use std::collections::BTreeMap;

use norn_cell::engine::SqlEngine;
use norn_cell::{MAIN_WORLD, ObjectRecord, StoreReader};
use norn_ir::{ActionEffectProgram, RuntimeObject, RuntimeValue};
use norn_kernel::cell_host::{CellCommit, CellHost};
use norn_kernel::{
    Command, CommandId, Effect, FactEvent, FreshnessPolicy, Generation, IdempotencyKey, LogicalTime,
};
use serde_json::Value;

use crate::actions::{ActionAnswer, ActionCall, ActionQuery, ActionRequest, call_action_bound};
use crate::automation_definitions::{
    AutomationPayload, AutomationThen, AutomationWhen, MAX_EXPIRES_IN_MS,
};
use crate::automations::{
    AUTOMATION_RUN_EVENT, AUTOMATION_STEP, AutomationRunOutcome, AutomationRunRecord,
    COMMAND_PREFIX, MAX_AUTOMATION_SCAN, eval_record_match, guard_passes, record_last_commit_ms,
};
use crate::decisions::{
    DecidedCall, DecidedCallState, DecisionCall, DecisionContext, DecisionDocument, DecisionFlow,
    DecisionOptionDocument, DecisionOptionEffect, DecisionRisk, DecisionRun, DecisionStep,
    DecisionSubject, OpenDecision,
};
use crate::definition_routes::MAX_LIST_LIMIT;
use crate::host::{ActionCell, DevHost};
use crate::question_routes::QuestionRuntime;
use crate::refusal::Refusal;

const LEDGER_PAGE: usize = 500;

/// An automation proposal carried inside a parked `norn.decision@1` command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HeldProposal {
    pub(crate) definition: String,
    pub(crate) revision: u64,
    pub(crate) title: String,
    pub(crate) object_type: String,
    pub(crate) object_id: String,
    pub(crate) object_version: u64,
    pub(crate) action: String,
    pub(crate) input: RuntimeObject,
    pub(crate) idempotency_key: String,
}

/// Decode the `automation` member of a parked `norn.decision@1` command.
#[must_use]
pub(crate) fn held_proposal(value: &RuntimeValue) -> Option<HeldProposal> {
    let text = |name: &str| {
        value
            .get(name)
            .and_then(RuntimeValue::as_str)
            .map(str::to_owned)
    };
    let Some(RuntimeValue::Object(input)) = value.get("input") else {
        return None;
    };
    Some(HeldProposal {
        definition: text("definition")?,
        revision: norn_ir::runtime::decode(value.get("revision")?).ok()?,
        title: text("title")?,
        object_type: text("objectType")?,
        object_id: text("objectId")?,
        object_version: norn_ir::runtime::decode(value.get("objectVersion")?).ok()?,
        action: text("action")?,
        input: input.clone(),
        idempotency_key: text("idempotencyKey")?,
    })
}

/// Project an open automation proposal decision to the wire [`DecisionDocument`].
pub(crate) fn proposal_document(
    decision: &OpenDecision,
    proposal: &HeldProposal,
) -> DecisionDocument {
    let risk = match decision.request.risk {
        norn_cell::decision_ledger::Risk::Low => DecisionRisk::Low,
        norn_cell::decision_ledger::Risk::Medium => DecisionRisk::Medium,
        norn_cell::decision_ledger::Risk::High => DecisionRisk::High,
    };
    let options = decision
        .request
        .options
        .iter()
        .map(|option| DecisionOptionDocument {
            id: option.id.clone(),
            label: option.label.clone(),
            scope: option.scope.clone(),
            effect: DecisionOptionEffect {
                action: option.effect.action.clone(),
                binding: option.effect.binding.clone(),
            },
        })
        .collect();
    DecisionDocument {
        id: decision.id.clone(),
        run: DecisionRun {
            id: decision.run_id.clone(),
            generation: decision.generation.0,
            state: None,
        },
        flow: DecisionFlow {
            rule_id: proposal.definition.clone(),
            version: proposal.revision.to_string(),
            title: Some(proposal.title.clone()),
        },
        step: DecisionStep {
            id: decision.step.clone(),
            label: Some("Proposed action".to_owned()),
        },
        question: decision.request.question.clone(),
        risk,
        options,
        agent_pick: decision.request.agent_pick.clone(),
        requested_at_ms: decision.requested_at_ms,
        expires_at_ms: decision.expires_at_ms,
        context: DecisionContext {
            subjects: vec![DecisionSubject {
                object_type: proposal.object_type.clone(),
                id: proposal.object_id.clone(),
            }],
        },
        input_schema: None,
        call: Some(DecisionCall {
            action: proposal.action.clone(),
            definition: proposal.definition.clone(),
            input: RuntimeValue::Object(proposal.input.clone()),
            trace: None,
        }),
    }
}

/// Evaluate enabled `event` and `lifecycle_entered` automations after an action
/// commits a record. Best-effort per automation: individual refusals are
/// recorded in run history and never fail the already-committed action.
pub(crate) fn on_action_committed<E: SqlEngine>(
    host: &DevHost<E>,
    actor: &str,
    action_id: &str,
    (object_type, object_id, object_version): (&str, &str, u64),
    disclosure: &dyn QuestionRuntime<E>,
    now: LogicalTime,
) {
    let Ok((record, previous_values, candidates)) =
        load_commit_candidates(host, object_type, object_id, object_version)
    else {
        return;
    };
    let Some(record) = record else {
        return;
    };
    for (automation_id, revision, payload, field, title) in candidates {
        let trigger = match &payload.when {
            AutomationWhen::Event {
                event,
                action_id: wanted_action,
            } => match event.as_str() {
                "created" if previous_values.is_none() => Some("event"),
                "updated" if previous_values.is_some() => Some("event"),
                "action" if wanted_action.as_deref() == Some(action_id) => Some("event"),
                _ => None,
            },
            AutomationWhen::LifecycleEntered { property, state } => {
                let now_in = record.values.get(property).and_then(RuntimeValue::as_str)
                    == Some(state.as_str());
                let was_in = previous_values
                    .as_ref()
                    .and_then(|prev| prev.get(property))
                    .and_then(RuntimeValue::as_str)
                    == Some(state.as_str());
                (now_in && !was_in).then_some("lifecycle_entered")
            }
            AutomationWhen::Stale { .. } => None,
        };
        let Some(trigger) = trigger else {
            continue;
        };
        if !guard_passes(&payload, &record) {
            continue;
        }
        // Trigger evaluation is best-effort after the initiating action has already committed.
        drop(fire_for_record(
            host,
            &FireTarget {
                automation_id: &automation_id,
                revision,
                payload: &payload,
                trigger,
                action_id_field: &field,
                action_title: &title,
                record: &record,
            },
            Some(actor),
            Some(disclosure),
            now,
        ));
    }
}

/// Evaluate enabled `stale` + `propose` automations during a cell alarm wake.
pub(crate) fn run_due_stale<E: SqlEngine>(
    host: &DevHost<E>,
    now: LogicalTime,
) -> Result<u32, Refusal> {
    let automations = active_automations(host)?;
    let mut fired = 0_u32;
    for (automation_id, revision, payload) in automations {
        if !payload.enabled
            || !matches!(payload.when, AutomationWhen::Stale { .. })
            || !matches!(payload.then, AutomationThen::Propose { .. })
        {
            continue;
        }
        let (rows, field, title, commit_times, now_ms) = {
            let cell = host.cell()?;
            let reader = cell.store().read();
            if !reader.model().has_type(&payload.object_type) {
                continue;
            }
            let Ok((field, title)) = action_meta(
                host,
                &reader,
                &payload.object_type,
                payload.then.action_id(),
            ) else {
                continue;
            };
            let rows = reader
                .list_after(MAIN_WORLD, &payload.object_type, None, MAX_AUTOMATION_SCAN)
                .map_err(Refusal::host)?;
            let mut times = BTreeMap::new();
            for row in &rows {
                if let Some(ts) = record_last_commit_ms(&reader, row) {
                    times.insert(row.id.clone(), ts);
                }
            }
            (
                rows,
                field,
                title,
                times,
                now.as_millis().max(cell.now().as_millis()),
            )
        };
        for row in rows {
            if eval_record_match(&payload, &row, commit_times.get(&row.id).copied(), now_ms)
                .is_none()
            {
                continue;
            }
            let run = fire_for_record(
                host,
                &FireTarget {
                    automation_id: &automation_id,
                    revision,
                    payload: &payload,
                    trigger: "stale",
                    action_id_field: &field,
                    action_title: &title,
                    record: &row,
                },
                None,
                None,
                LogicalTime::from_millis(now_ms),
            )?;
            if run.outcome == AutomationRunOutcome::Proposed {
                fired = fired.saturating_add(1);
            }
        }
    }
    Ok(fired)
}

/// Execute an automation-proposed action after a human decider approves it in
/// the Inbox, under the deciding human's own verified authority.
pub(crate) fn run_approved_proposal<E: SqlEngine>(
    host: &DevHost<E>,
    proposal: &HeldProposal,
    decider: &str,
    disclosure: &dyn QuestionRuntime<E>,
    now: LogicalTime,
) -> Result<DecidedCall, Refusal> {
    let query = ActionQuery {
        org_id: Some(host.scope().org_id.clone()),
        environment_id: Some(host.scope().environment_id.clone()),
        version: None,
    };
    let request = ActionRequest {
        input: RuntimeValue::Object(proposal.input.clone()),
        idempotency_key: format!("{}:approved", proposal.idempotency_key),
        trace: None,
    };
    let principal = host.authorizer.verified_principal(decider);
    let outcome = call_action_bound(
        host,
        &ActionCall {
            query: &query,
            actor: Some(decider),
            principal: principal.as_ref(),
            attempt: None,
            action_id: &proposal.action,
            request: &request,
            now,
        },
        disclosure,
        None,
    );
    let now_ms = now.as_millis();
    let dedupe_key = format!(
        "approved:{}:{}",
        proposal.definition, proposal.idempotency_key
    );
    let (decided_call, run_record) = match outcome {
        Ok(ActionAnswer::Receipt(receipt)) => (
            DecidedCall {
                committed: true,
                code: None,
                state: DecidedCallState::Committed,
            },
            make_run(
                &dedupe_key,
                (&proposal.definition, proposal.revision, "decision_approved"),
                (
                    &proposal.object_type,
                    &proposal.object_id,
                    proposal.object_version,
                    &proposal.action,
                ),
                AutomationRunOutcome::Committed,
                (None, Some(receipt.receipt.commit_seq), None, None),
                receipt.receipt.recorded_at_ms,
            )?,
        ),
        Ok(ActionAnswer::Pending(pending)) => (
            DecidedCall {
                committed: false,
                code: None,
                state: DecidedCallState::NotExecuted,
            },
            make_run(
                &dedupe_key,
                (&proposal.definition, proposal.revision, "decision_approved"),
                (
                    &proposal.object_type,
                    &proposal.object_id,
                    proposal.object_version,
                    &proposal.action,
                ),
                AutomationRunOutcome::Proposed,
                (Some(pending.decision.id), None, None, None),
                now_ms,
            )?,
        ),
        Err(refusal) => (
            DecidedCall {
                committed: false,
                code: Some(refusal.code.clone()),
                state: DecidedCallState::Refused,
            },
            make_run(
                &dedupe_key,
                (&proposal.definition, proposal.revision, "decision_approved"),
                (
                    &proposal.object_type,
                    &proposal.object_id,
                    proposal.object_version,
                    &proposal.action,
                ),
                AutomationRunOutcome::Refused,
                (None, None, Some(refusal.code), Some(refusal.message)),
                now_ms,
            )?,
        ),
    };
    let mut cell = host.cell()?;
    record_run_event(&mut cell, &run_record, now)?;
    Ok(decided_call)
}

pub(crate) struct FireTarget<'a> {
    pub(crate) automation_id: &'a str,
    pub(crate) revision: u64,
    pub(crate) payload: &'a AutomationPayload,
    pub(crate) trigger: &'a str,
    pub(crate) action_id_field: &'a str,
    pub(crate) action_title: &'a str,
    pub(crate) record: &'a ObjectRecord,
}

pub(crate) fn fire_for_record<E: SqlEngine>(
    host: &DevHost<E>,
    target: &FireTarget<'_>,
    actor: Option<&str>,
    disclosure: Option<&dyn QuestionRuntime<E>>,
    now: LogicalTime,
) -> Result<AutomationRunRecord, Refusal> {
    let dedupe_key = dedupe_hash(
        target.automation_id,
        target.revision,
        &target.record.object_type,
        &target.record.id,
        target.record.version,
    )?;
    let now_ms = now.as_millis();
    let meta = (target.automation_id, target.revision, target.trigger);
    let subject = (
        target.record.object_type.as_str(),
        target.record.id.as_str(),
        target.record.version,
        target.payload.then.action_id(),
    );
    {
        let cell = host.cell()?;
        if already_recorded(&cell.store().read(), target.automation_id, &dedupe_key)? {
            return make_run(
                &dedupe_key,
                meta,
                subject,
                AutomationRunOutcome::Skipped,
                (None, None, None, None),
                now_ms,
            );
        }
    }
    let input = build_action_input(
        &target.payload.then,
        target.action_id_field,
        &target.record.id,
    )?;
    match &target.payload.then {
        AutomationThen::Propose {
            question,
            risk,
            decider,
            expires_in_ms,
            ..
        } => {
            let mut cell = host.cell()?;
            let now = crate::actions::turn_clock(&mut cell, now)?;
            let cmd_id = format!("{COMMAND_PREFIX}{dedupe_key}");
            let decision_id = crate::decisions::decision_id(&cmd_id, 1)?;
            let run_record = make_run(
                &dedupe_key,
                meta,
                subject,
                AutomationRunOutcome::Proposed,
                (Some(decision_id), None, None, None),
                now.as_millis(),
            )?;
            park_proposal(
                &mut cell,
                target,
                &cmd_id,
                &dedupe_key,
                (question, risk, decider, *expires_in_ms),
                input,
                &run_record,
            )?;
            Ok(run_record)
        }
        AutomationThen::Run { action_id, .. } => run_automatic_action(
            host,
            target,
            &dedupe_key,
            action_id,
            input,
            (actor, disclosure),
            now,
        ),
    }
}

fn run_automatic_action<E: SqlEngine>(
    host: &DevHost<E>,
    target: &FireTarget<'_>,
    dedupe_key: &str,
    action_id: &str,
    input: RuntimeObject,
    (actor, disclosure): (Option<&str>, Option<&dyn QuestionRuntime<E>>),
    now: LogicalTime,
) -> Result<AutomationRunRecord, Refusal> {
    let now_ms = now.as_millis();
    let meta = (target.automation_id, target.revision, target.trigger);
    let subject = (
        target.record.object_type.as_str(),
        target.record.id.as_str(),
        target.record.version,
        action_id,
    );
    let run_record = if let (Some(actor), Some(disclosure)) = (actor, disclosure) {
        let query = ActionQuery {
            org_id: Some(host.scope().org_id.clone()),
            environment_id: Some(host.scope().environment_id.clone()),
            version: None,
        };
        let request = ActionRequest {
            input: RuntimeValue::Object(input),
            idempotency_key: format!("auto:{dedupe_key}"),
            trace: None,
        };
        let principal = host.authorizer.verified_principal(actor);
        let answer = call_action_bound(
            host,
            &ActionCall {
                query: &query,
                actor: Some(actor),
                principal: principal.as_ref(),
                attempt: None,
                action_id,
                request: &request,
                now,
            },
            disclosure,
            None,
        );
        match answer {
            Ok(ActionAnswer::Receipt(receipt)) => make_run(
                dedupe_key,
                meta,
                subject,
                AutomationRunOutcome::Committed,
                (None, Some(receipt.receipt.commit_seq), None, None),
                receipt.receipt.recorded_at_ms,
            )?,
            Ok(ActionAnswer::Pending(pending)) => make_run(
                dedupe_key,
                meta,
                subject,
                AutomationRunOutcome::Proposed,
                (Some(pending.decision.id), None, None, None),
                now_ms,
            )?,
            Err(refusal) => make_run(
                dedupe_key,
                meta,
                subject,
                AutomationRunOutcome::Refused,
                (None, None, Some(refusal.code), Some(refusal.message)),
                now_ms,
            )?,
        }
    } else {
        make_run(
            dedupe_key,
            meta,
            subject,
            AutomationRunOutcome::Refused,
            (
                None,
                None,
                Some("authz.denied".to_owned()),
                Some("no verified caller principal for automatic run".to_owned()),
            ),
            now_ms,
        )?
    };
    let mut cell = host.cell()?;
    record_run_event(&mut cell, &run_record, now)?;
    Ok(run_record)
}

fn make_run(
    dedupe_key: &str,
    (automation_id, automation_revision, trigger): (&str, u64, &str),
    (object_type, object_id, object_version, action_id): (&str, &str, u64, &str),
    outcome: AutomationRunOutcome,
    (decision_id, commit_seq, refusal_code, refusal_message): (
        Option<String>,
        Option<u64>,
        Option<String>,
        Option<String>,
    ),
    recorded_at_ms: u64,
) -> Result<AutomationRunRecord, Refusal> {
    let suffix = match outcome {
        AutomationRunOutcome::Proposed => "proposed",
        AutomationRunOutcome::Committed => "committed",
        AutomationRunOutcome::Refused => "refused",
        AutomationRunOutcome::Skipped => "skipped",
    };
    Ok(AutomationRunRecord {
        id: run_entry_id(dedupe_key, suffix)?,
        automation_id: automation_id.to_owned(),
        automation_revision,
        dedupe_key: dedupe_key.to_owned(),
        trigger: trigger.to_owned(),
        object_type: object_type.to_owned(),
        object_id: object_id.to_owned(),
        object_version,
        action_id: action_id.to_owned(),
        outcome,
        decision_id,
        commit_seq,
        refusal_code,
        refusal_message,
        recorded_at_ms,
    })
}

fn park_proposal<E: SqlEngine>(
    cell: &mut ActionCell<E>,
    target: &FireTarget<'_>,
    cmd_id: &str,
    dedupe_key: &str,
    (question, risk, decider, expires_in_ms): (
        &str,
        &str,
        &crate::automation_definitions::AutomationDecider,
        Option<u64>,
    ),
    input: RuntimeObject,
    run_record: &AutomationRunRecord,
) -> Result<(), Refusal> {
    let text = |s: &str| RuntimeValue::String(s.to_owned());
    let formatted_question = question.replace("{id}", &target.record.id);
    let mut decider_map = serde_json::Map::new();
    decider_map.insert(
        "roles".to_owned(),
        Value::Array(decider.roles.iter().cloned().map(Value::String).collect()),
    );
    if let Some(law) = &decider.law {
        decider_map.insert("law".to_owned(), Value::String(law.clone()));
    }
    let decider_json = Value::Object(decider_map);
    let scope_text = format!("{} {}", target.record.object_type, target.record.id);
    let decision_request = serde_json::json!({
        "question": formatted_question,
        "risk": risk,
        "decider": decider_json,
        "expiresInMs": expires_in_ms.unwrap_or(MAX_EXPIRES_IN_MS),
        "options": [
            {
                "id": crate::approvals::APPROVE,
                "label": format!("Run {}", target.action_title),
                "scope": scope_text,
                "effect": { "action": target.payload.then.action_id() },
            },
            {
                "id": crate::approvals::REJECT,
                "label": "Dismiss",
                "scope": scope_text,
                "effect": { "binding": "automation.dismiss" },
            },
        ],
    });
    let automation_meta: RuntimeObject = BTreeMap::from([
        ("definition".to_owned(), text(target.automation_id)),
        (
            "revision".to_owned(),
            norn_ir::runtime::encode(&target.revision).map_err(Refusal::host)?,
        ),
        ("title".to_owned(), text(target.payload.title.trim())),
        ("objectType".to_owned(), text(&target.record.object_type)),
        ("objectId".to_owned(), text(&target.record.id)),
        (
            "objectVersion".to_owned(),
            norn_ir::runtime::encode(&target.record.version).map_err(Refusal::host)?,
        ),
        ("action".to_owned(), text(target.payload.then.action_id())),
        ("input".to_owned(), RuntimeValue::Object(input)),
        (
            "idempotencyKey".to_owned(),
            text(&format!("auto:{dedupe_key}")),
        ),
    ]);
    let request_rv = json_to_runtime(&decision_request)?;
    let payload: RuntimeObject = BTreeMap::from([
        ("kind".to_owned(), text("perform")),
        (
            "function".to_owned(),
            text(crate::flow_definitions::DECISION_FUNCTION),
        ),
        ("functionVersion".to_owned(), text("1")),
        ("runId".to_owned(), text(cmd_id)),
        ("step".to_owned(), text(AUTOMATION_STEP)),
        (
            "attempt".to_owned(),
            norn_ir::runtime::encode(&1_u64).map_err(Refusal::host)?,
        ),
        ("idempotencyKey".to_owned(), text(cmd_id)),
        (
            "input".to_owned(),
            RuntimeValue::Object(BTreeMap::from([(AUTOMATION_STEP.to_owned(), request_rv)])),
        ),
        (
            "automation".to_owned(),
            RuntimeValue::Object(automation_meta),
        ),
    ]);
    let commit = CellCommit {
        effects: vec![Effect::Perform(Command {
            id: CommandId(cmd_id.to_owned()),
            generation: Generation(1),
            idempotency_key: IdempotencyKey(cmd_id.to_owned()),
            freshness: FreshnessPolicy::AppendOnly,
            deadline: None,
            payload,
        })],
        ..CellCommit::<FactEvent, RuntimeObject, RuntimeObject>::empty()
    };
    let event = run_event_value(run_record)?;
    cell.attributed(vec![event], |cell| CellHost::commit(cell, commit))
        .map_err(|error| Refusal::host(format!("{error:?}")))?;
    Ok(())
}

fn record_run_event<E: SqlEngine>(
    cell: &mut ActionCell<E>,
    run_record: &AutomationRunRecord,
    now: LogicalTime,
) -> Result<(), Refusal> {
    let _ = crate::actions::turn_clock(cell, now)?;
    let event = run_event_value(run_record)?;
    let commit = CellCommit::<FactEvent, RuntimeObject, RuntimeObject>::empty();
    cell.attributed(vec![event], |cell| CellHost::commit(cell, commit))
        .map_err(|error| Refusal::host(format!("{error:?}")))?;
    Ok(())
}

fn run_event_value(run: &AutomationRunRecord) -> Result<RuntimeValue, Refusal> {
    Ok(RuntimeValue::Object(BTreeMap::from([
        (
            "type".to_owned(),
            RuntimeValue::String(AUTOMATION_RUN_EVENT.to_owned()),
        ),
        (
            "run".to_owned(),
            norn_ir::runtime::encode(run).map_err(Refusal::host)?,
        ),
    ])))
}

pub(crate) fn read_runs_from_ledger<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    automation_id: &str,
) -> Result<Vec<AutomationRunRecord>, Refusal> {
    let mut after = 0_u64;
    let mut found = Vec::new();
    loop {
        let page = reader
            .ledger_page(after, LEDGER_PAGE)
            .map_err(Refusal::host)?;
        let count = page.len();
        for entry in page {
            after = entry.seq;
            if entry.event.get("type").and_then(RuntimeValue::as_str) != Some(AUTOMATION_RUN_EVENT)
            {
                continue;
            }
            let Some(run_val) = entry.event.get("run") else {
                continue;
            };
            let Ok(record) = norn_ir::runtime::decode::<AutomationRunRecord>(run_val) else {
                continue;
            };
            if record.automation_id == automation_id {
                found.push(record);
            }
        }
        if count < LEDGER_PAGE {
            break;
        }
    }
    Ok(found)
}

fn already_recorded<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    automation_id: &str,
    dedupe_key: &str,
) -> Result<bool, Refusal> {
    let runs = read_runs_from_ledger(reader, automation_id)?;
    Ok(runs
        .iter()
        .any(|run| run.dedupe_key == dedupe_key && run.outcome != AutomationRunOutcome::Skipped))
}

fn active_automations<E: SqlEngine>(
    host: &DevHost<E>,
) -> Result<Vec<(String, u64, AutomationPayload)>, Refusal> {
    let cell = host.cell()?;
    let reader = cell.store().read();
    let mut out = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let page = reader
            .retained_definitions("automation", false, after.as_deref(), MAX_LIST_LIMIT)
            .map_err(Refusal::host)?;
        let full = page.len() >= usize::try_from(MAX_LIST_LIMIT).unwrap_or(usize::MAX);
        after = page.last().map(|r| r.definition.id.clone());
        for rec in page {
            if let Ok(payload) = serde_json::from_str::<AutomationPayload>(&rec.definition.payload)
            {
                out.push((rec.definition.id, rec.definition.revision, payload));
            }
        }
        if !full {
            return Ok(out);
        }
    }
}

type CommitCandidate = (String, u64, AutomationPayload, String, String);
type CommitCandidates = (
    Option<ObjectRecord>,
    Option<BTreeMap<String, RuntimeValue>>,
    Vec<CommitCandidate>,
);

fn load_commit_candidates<E: SqlEngine>(
    host: &DevHost<E>,
    object_type: &str,
    object_id: &str,
    object_version: u64,
) -> Result<CommitCandidates, Refusal> {
    let cell = host.cell()?;
    let reader = cell.store().read();
    let record = reader
        .object(MAIN_WORLD, object_type, object_id)
        .map_err(Refusal::host)?;
    let previous = if object_version > 1 {
        reader
            .object_at_version(
                MAIN_WORLD,
                object_type,
                object_id,
                object_version.saturating_sub(1),
            )
            .map_err(Refusal::host)?
            .map(|prev| prev.values)
    } else {
        None
    };
    let mut candidates = Vec::new();
    let mut after: Option<String> = None;
    loop {
        let page = reader
            .retained_definitions("automation", false, after.as_deref(), MAX_LIST_LIMIT)
            .map_err(Refusal::host)?;
        let full = page.len() >= usize::try_from(MAX_LIST_LIMIT).unwrap_or(usize::MAX);
        after = page.last().map(|r| r.definition.id.clone());
        for rec in page {
            let Ok(payload) = serde_json::from_str::<AutomationPayload>(&rec.definition.payload)
            else {
                continue;
            };
            if !payload.enabled || payload.object_type != object_type {
                continue;
            }
            if let Ok((field, title)) =
                action_meta(host, &reader, object_type, payload.then.action_id())
            {
                candidates.push((
                    rec.definition.id,
                    rec.definition.revision,
                    payload,
                    field,
                    title,
                ));
            }
        }
        if !full {
            break;
        }
    }
    Ok((record, previous, candidates))
}

pub(crate) fn action_meta<E: SqlEngine>(
    host: &DevHost<E>,
    reader: &StoreReader<'_, E>,
    object_type: &str,
    action_id: &str,
) -> Result<(String, String), Refusal> {
    let actions = crate::automation_definitions::actions_map(host, reader, object_type)?;
    let action = actions.get(action_id).ok_or_else(|| {
        Refusal::invalid(format!(
            "action {action_id:?} is not admitted for {object_type}"
        ))
    })?;
    let field = match &action.effect_program {
        Some(
            ActionEffectProgram::UpsertInputObject {
                object_id_field, ..
            }
            | ActionEffectProgram::FireTransition {
                object_id_field, ..
            }
            | ActionEffectProgram::MutateParentWithRows {
                object_id_field, ..
            },
        ) => object_id_field.clone(),
        None => {
            return Err(Refusal::invalid(format!(
                "action {action_id:?} has no runnable effect program"
            )));
        }
    };
    let title = action
        .title
        .clone()
        .unwrap_or_else(|| action.action_id.clone());
    Ok((field, title))
}

pub(crate) fn build_action_input(
    then: &AutomationThen,
    object_id_field: &str,
    object_id: &str,
) -> Result<RuntimeObject, Refusal> {
    let mut map = RuntimeObject::new();
    for (key, value) in then.input() {
        map.insert(key.clone(), json_to_runtime(value)?);
    }
    map.insert(
        object_id_field.to_owned(),
        RuntimeValue::String(object_id.to_owned()),
    );
    Ok(map)
}

fn json_to_runtime(value: &Value) -> Result<RuntimeValue, Refusal> {
    let text = serde_json::to_string(value).map_err(Refusal::host)?;
    RuntimeValue::from_json_text(&text).map_err(Refusal::host)
}

fn dedupe_hash(
    automation_id: &str,
    revision: u64,
    object_type: &str,
    object_id: &str,
    object_version: u64,
) -> Result<String, Refusal> {
    let digest = norn_ir::canonical::domain_hash_hex(
        b"norn.automation.dedupe/1\0",
        &(
            automation_id,
            revision,
            object_type,
            object_id,
            object_version,
        ),
    )
    .map_err(Refusal::host)?;
    Ok(digest.get(..32).unwrap_or(&digest).to_owned())
}

fn run_entry_id(dedupe_key: &str, suffix: &str) -> Result<String, Refusal> {
    let digest =
        norn_ir::canonical::domain_hash_hex(b"norn.automation.run_id/1\0", &(dedupe_key, suffix))
            .map_err(Refusal::host)?;
    Ok(format!("arun_{}", digest.get(..24).unwrap_or(&digest)))
}
