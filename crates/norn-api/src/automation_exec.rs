//! Execution engine, Inbox proposal parking, ledger run history, and commit /
//! alarm hooks for `AUTOMATION-1`.

use std::collections::BTreeMap;

use norn_cell::engine::SqlEngine;
use norn_cell::{MAIN_WORLD, ObjectRecord, StoreReader};
use norn_ir::{RuntimeObject, RuntimeValue};
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
    COMMAND_PREFIX, HeldProposal, MAX_AUTOMATION_SCAN, action_meta, build_action_input,
    dedupe_hash, eval_record_match, guard_passes, held_proposal, json_to_runtime,
    record_last_commit_ms, run_entry_id,
};
use crate::decisions::{DecidedCall, DecidedCallState};
use crate::definition_routes::MAX_LIST_LIMIT;
use crate::host::{ActionCell, DevHost};
use crate::question_routes::QuestionRuntime;
use crate::refusal::{Refusal, RefusalCategory};

const LEDGER_PAGE: usize = 500;
const MAX_HISTORY_SCAN_EVENTS: u64 = 10_000;

fn run_idempotency_key(dedupe_key: &str) -> String {
    format!("norn.automation.run/{dedupe_key}")
}

fn approved_run_idempotency_key(decision_id: &str) -> String {
    format!("norn.automation.approved/{decision_id}")
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

/// Earliest logical timestamp (ms) when any enabled `stale` + `propose`
/// automation will become eligible for an un-fired record (AUTOMATION-1).
pub(crate) fn next_stale_wake<E: SqlEngine>(cell: &ActionCell<E>) -> Result<Option<u64>, Refusal> {
    let reader = cell.store().read();
    let automations = active_automations_from(&reader)?;
    let mut earliest: Option<u64> = None;
    for (automation_id, revision, payload) in automations {
        let AutomationWhen::Stale {
            after_ms,
            property,
            state,
        } = &payload.when
        else {
            continue;
        };
        if !payload.enabled
            || !matches!(payload.then, AutomationThen::Propose { .. })
            || !reader.model().has_type(&payload.object_type)
        {
            continue;
        }
        let rows = reader
            .list_after(MAIN_WORLD, &payload.object_type, None, MAX_AUTOMATION_SCAN)
            .map_err(Refusal::host)?;
        for row in rows {
            if let (Some(prop), Some(wanted)) = (property, state)
                && row.values.get(prop).and_then(RuntimeValue::as_str) != Some(wanted.as_str())
            {
                continue;
            }
            if !guard_passes(&payload, &row) {
                continue;
            }
            let dedupe_key = dedupe_hash(
                &automation_id,
                revision,
                &row.object_type,
                &row.id,
                row.version,
            )?;
            if already_recorded(&reader, &dedupe_key)? {
                continue;
            }
            if let Some(last_commit) = record_last_commit_ms(&reader, &row) {
                let due_at = last_commit.saturating_add(*after_ms);
                earliest = Some(earliest.map_or(due_at, |cur| cur.min(due_at)));
            }
        }
    }
    Ok(earliest)
}

/// Read the already-recorded execution outcome of an approved automation
/// proposal in O(1) via its idempotency receipt, or `None` if not yet run.
pub(crate) fn approved_proposal_call<E: SqlEngine>(
    cell: &ActionCell<E>,
    decision_id: &str,
) -> Result<Option<DecidedCall>, Refusal> {
    let reader = cell.store().read();
    let key = approved_run_idempotency_key(decision_id);
    let Some(receipt) = reader
        .receipt_by_idempotency_key(MAIN_WORLD, &key)
        .map_err(Refusal::host)?
    else {
        return Ok(None);
    };
    let after = receipt.body.first_event_seq.saturating_sub(1);
    let count = usize::try_from(receipt.body.event_count)
        .unwrap_or(1)
        .max(1);
    let entries = reader.ledger_page(after, count).map_err(Refusal::host)?;
    for entry in entries {
        if entry.event.get("type").and_then(RuntimeValue::as_str) != Some(AUTOMATION_RUN_EVENT) {
            continue;
        }
        let Some(run_val) = entry.event.get("run") else {
            continue;
        };
        let Ok(record) = norn_ir::runtime::decode::<AutomationRunRecord>(run_val) else {
            continue;
        };
        return Ok(Some(match record.outcome {
            AutomationRunOutcome::Committed => DecidedCall {
                committed: true,
                code: None,
                state: DecidedCallState::Committed,
            },
            AutomationRunOutcome::Refused => DecidedCall {
                committed: false,
                code: record.refusal_code,
                state: DecidedCallState::Refused,
            },
            AutomationRunOutcome::Proposed | AutomationRunOutcome::Skipped => DecidedCall {
                committed: false,
                code: None,
                state: DecidedCallState::NotExecuted,
            },
        }));
    }
    Ok(None)
}

fn load_approved_proposal<E: SqlEngine>(
    cell: &ActionCell<E>,
    decision_id: &str,
) -> Result<Option<HeldProposal>, Refusal> {
    let reader = cell.store().read();
    let Some(row) = reader
        .decision_resolution(MAIN_WORLD, decision_id)
        .map_err(Refusal::host)?
    else {
        return Ok(None);
    };
    if row.resolved.choice.as_deref() != Some(crate::approvals::APPROVE)
        || !row.resolved.command_id.starts_with(COMMAND_PREFIX)
    {
        return Ok(None);
    }
    let Some(cmd) = reader
        .outbox_command(&row.resolved.command_id)
        .map_err(Refusal::host)?
    else {
        return Ok(None);
    };
    Ok(cmd
        .command
        .get("payload")
        .and_then(|payload| payload.get("automation"))
        .and_then(held_proposal))
}

/// Execute or recover an automation-proposed action after a human decider
/// approves it in the Inbox, under the deciding human's verified authority.
/// Replays and recoveries after interruption never execute the action twice:
/// recorded outcomes return immediately, and the action call itself uses a
/// deterministic idempotency key bound to the proposal.
pub(crate) fn run_approved_proposal<E: SqlEngine>(
    host: &DevHost<E>,
    decision_id: &str,
    decider: &str,
    disclosure: &dyn QuestionRuntime<E>,
    now: LogicalTime,
) -> Result<Option<DecidedCall>, Refusal> {
    let proposal = {
        let cell = host.cell()?;
        if let Some(done) = approved_proposal_call(&cell, decision_id)? {
            return Ok(Some(done));
        }
        let Some(proposal) = load_approved_proposal(&cell, decision_id)? else {
            return Ok(None);
        };
        proposal
    };
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
    let (decided_call, run_record) =
        classify_approved_outcome(&proposal, outcome, now.as_millis())?;
    if let Some(run_record) = run_record {
        let mut cell = host.cell()?;
        record_run_event(
            &mut cell,
            &approved_run_idempotency_key(decision_id),
            &run_record,
            now,
        )?;
    }
    Ok(Some(decided_call))
}

fn classify_approved_outcome(
    proposal: &HeldProposal,
    outcome: Result<ActionAnswer, Refusal>,
    now_ms: u64,
) -> Result<(DecidedCall, Option<AutomationRunRecord>), Refusal> {
    let dedupe_key = format!(
        "approved:{}:{}",
        proposal.definition, proposal.idempotency_key
    );
    let meta = (
        proposal.definition.as_str(),
        proposal.revision,
        "decision_approved",
    );
    let subject = (
        proposal.object_type.as_str(),
        proposal.object_id.as_str(),
        proposal.object_version,
        proposal.action.as_str(),
    );
    match outcome {
        Ok(ActionAnswer::Receipt(receipt)) => Ok((
            DecidedCall {
                committed: true,
                code: None,
                state: DecidedCallState::Committed,
            },
            Some(make_run(
                &dedupe_key,
                meta,
                subject,
                AutomationRunOutcome::Committed,
                (None, Some(receipt.receipt.commit_seq), None, None),
                receipt.receipt.recorded_at_ms,
            )?),
        )),
        Ok(ActionAnswer::Pending(pending)) => Ok((
            DecidedCall {
                committed: false,
                code: None,
                state: DecidedCallState::NotExecuted,
            },
            Some(make_run(
                &dedupe_key,
                meta,
                subject,
                AutomationRunOutcome::Proposed,
                (Some(pending.decision.id), None, None, None),
                now_ms,
            )?),
        )),
        Err(refusal)
            if matches!(
                refusal.category,
                RefusalCategory::Internal
                    | RefusalCategory::Permission
                    | RefusalCategory::Visibility
            ) =>
        {
            Ok((
                DecidedCall {
                    committed: false,
                    code: Some(refusal.code),
                    state: DecidedCallState::NotExecuted,
                },
                None,
            ))
        }
        Err(refusal) => Ok((
            DecidedCall {
                committed: false,
                code: Some(refusal.code.clone()),
                state: DecidedCallState::Refused,
            },
            Some(make_run(
                &dedupe_key,
                meta,
                subject,
                AutomationRunOutcome::Refused,
                (None, None, Some(refusal.code), Some(refusal.message)),
                now_ms,
            )?),
        )),
    }
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
        if already_recorded(&cell.store().read(), &dedupe_key)? {
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
    record_run_event(
        &mut cell,
        &run_idempotency_key(dedupe_key),
        &run_record,
        now,
    )?;
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
    idempotency_key: &str,
    run_record: &AutomationRunRecord,
    now: LogicalTime,
) -> Result<(), Refusal> {
    let _ = crate::actions::turn_clock(cell, now)?;
    let event = run_event_value(run_record)?;
    cell.record_idempotent_event(idempotency_key, event)
        .map_err(Refusal::host)?;
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
    head_events: u64,
) -> Result<Vec<AutomationRunRecord>, Refusal> {
    let mut after = head_events.saturating_sub(MAX_HISTORY_SCAN_EVENTS);
    let mut found = Vec::new();
    let max_pages = usize::try_from(MAX_HISTORY_SCAN_EVENTS)
        .unwrap_or(10_000)
        .div_ceil(LEDGER_PAGE)
        .saturating_add(1);
    for _ in 0..max_pages {
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

/// O(1) deduplication check via the cell's indexed receipt idempotency key and
/// outbox command primary key (zero ledger scan).
fn already_recorded<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
    dedupe_key: &str,
) -> Result<bool, Refusal> {
    let key = run_idempotency_key(dedupe_key);
    if reader
        .receipt_by_idempotency_key(MAIN_WORLD, &key)
        .map_err(Refusal::host)?
        .is_some()
    {
        return Ok(true);
    }
    let cmd_id = format!("{COMMAND_PREFIX}{dedupe_key}");
    Ok(reader
        .outbox_command(&cmd_id)
        .map_err(Refusal::host)?
        .is_some())
}

fn active_automations<E: SqlEngine>(
    host: &DevHost<E>,
) -> Result<Vec<(String, u64, AutomationPayload)>, Refusal> {
    let cell = host.cell()?;
    active_automations_from(&cell.store().read())
}

fn active_automations_from<E: SqlEngine>(
    reader: &StoreReader<'_, E>,
) -> Result<Vec<(String, u64, AutomationPayload)>, Refusal> {
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
