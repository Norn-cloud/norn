//! Authentication, ephemeral staging, and main-world replay for `FORK-MERGE-1`.

use std::collections::BTreeMap;

use norn_ir::RuntimeValue;
use norn_kernel::cell_host::{FactKey, JsonObject};
use norn_kernel::{
    ACTION_CALL_TYPE, ActionAuthorizer, ActionCall, ActionCallRecord, ActionOutcome, ActionRefusal,
    CellHost as _, Command, DecisionRefusal, DecisionTurn, FactEvent, LogicalTime, Prepared, World,
    commit_action, prepare_action_intent,
};

use super::{
    AuthenticatedForkCommand, ForkActionCall, ForkMergeRefusalReason, MaterialOutcome,
    PromotedCommand, is_fork_scope_event, merge_refused, scope_binding_event,
};
use crate::admitted::AdmittedDefinitions;
use crate::engine::SqlEngine;
use crate::error::CellError;
use crate::export::ExportRecord;
use crate::fork::{ForkEngine, ForkPin};
use crate::host::SqliteCell;
use crate::read::LedgerHead;
use crate::store::{CommitPrecondition, FactWriteRow};

pub(super) fn authenticate_fork_commands<'a, S: SqlEngine>(
    fork: &SqliteCell<FactEvent, JsonObject, JsonObject, S>,
    pin: &ForkPin,
    fork_commits: u64,
    commands: &'a [ForkActionCall],
) -> Result<Vec<AuthenticatedForkCommand<'a>>, CellError> {
    let expected_count = fork_commits.saturating_sub(pin.base.commits);
    let supplied_count = u64::try_from(commands.len()).unwrap_or(u64::MAX);
    if supplied_count != expected_count {
        return Err(merge_refused(
            ForkMergeRefusalReason::UnauthenticatedForkHistory {
                commit_seq: 0,
                detail: format!(
                    "fork admitted {expected_count} commit(s) after base {}, but caller supplied {supplied_count} command(s)",
                    pin.base.commits
                ),
            },
        ));
    }
    let mut authenticated = Vec::with_capacity(commands.len());
    for (offset, command) in commands.iter().enumerate() {
        let offset_u64 = u64::try_from(offset)
            .map_err(|_err| CellError::OutOfRange("fork promotion command index"))?;
        let seq = pin
            .base
            .commits
            .checked_add(1)
            .and_then(|start| start.checked_add(offset_u64))
            .ok_or(CellError::OutOfRange("fork promotion commit seq"))?;
        let Some(export) = fork.store().local_export_record(seq)? else {
            return Err(merge_refused(
                ForkMergeRefusalReason::UnauthenticatedForkHistory {
                    commit_seq: seq,
                    detail: "missing local export record for fork commit".to_owned(),
                },
            ));
        };
        authenticated.push(authenticate_one_commit(seq, command, &export)?);
    }
    Ok(authenticated)
}

fn authenticate_one_commit<'a>(
    seq: u64,
    command: &'a ForkActionCall,
    export: &ExportRecord,
) -> Result<AuthenticatedForkCommand<'a>, CellError> {
    if export.definition_install.is_some() || export.batch.consumes.is_some() {
        return Err(merge_refused(
            ForkMergeRefusalReason::UnauthenticatedForkHistory {
                commit_seq: seq,
                detail: "definition installs and delivery consumptions cannot be promoted via action replay".to_owned(),
            },
        ));
    }
    let expected_scope_event = scope_binding_event(&command.idempotency_key, &command.scope)?;
    let scope_events: Vec<&RuntimeValue> = export
        .batch
        .events
        .iter()
        .filter(|event| is_fork_scope_event(event))
        .collect();
    if scope_events.as_slice() != [&expected_scope_event] {
        return Err(merge_refused(
            ForkMergeRefusalReason::UnauthenticatedForkHistory {
                commit_seq: seq,
                detail: "receipt-bound authorization scope event is missing or disagrees with supplied scope".to_owned(),
            },
        ));
    }
    let action_writes: Vec<(&str, &JsonObject)> = export
        .batch
        .writes
        .iter()
        .filter_map(|write| match write {
            FactWriteRow::Put {
                object_type,
                key,
                values,
            } if object_type == ACTION_CALL_TYPE => Some((key.as_str(), values)),
            FactWriteRow::Put { .. } | FactWriteRow::Delete { .. } => None,
        })
        .collect();
    let [(record_key, record_values)] = action_writes.as_slice() else {
        return Err(merge_refused(
            ForkMergeRefusalReason::UnauthenticatedForkHistory {
                commit_seq: seq,
                detail: "fork commit is not a single governed action call".to_owned(),
            },
        ));
    };
    if *record_key != command.idempotency_key {
        return Err(merge_refused(
            ForkMergeRefusalReason::UnauthenticatedForkHistory {
                commit_seq: seq,
                detail: format!(
                    "expected idempotency key `{record_key}`, caller supplied `{}`",
                    command.idempotency_key
                ),
            },
        ));
    }
    let Some(record) = ActionCallRecord::from_values(record_values) else {
        return Err(merge_refused(
            ForkMergeRefusalReason::UnauthenticatedForkHistory {
                commit_seq: seq,
                detail: "stored norn.ActionCall record could not be decoded".to_owned(),
            },
        ));
    };
    let object_type_matches = command
        .object_type
        .as_ref()
        .is_none_or(|expected| *expected == record.object.object_type);
    if record.action_id != command.action_id
        || record.actor != command.actor
        || record.input != command.input
        || record.trace != command.trace
        || !object_type_matches
    {
        return Err(merge_refused(
            ForkMergeRefusalReason::UnauthenticatedForkHistory {
                commit_seq: seq,
                detail: "supplied action call disagrees with fork's committed ActionCall record"
                    .to_owned(),
            },
        ));
    }
    let read_versions = collect_read_versions(export);
    let material_outcome = MaterialOutcome::from_export(export, &record);
    Ok(AuthenticatedForkCommand {
        call: command,
        fork_commit_seq: seq,
        read_versions,
        material_outcome,
    })
}

fn collect_read_versions(export: &ExportRecord) -> BTreeMap<FactKey, u64> {
    let mut read_versions = BTreeMap::new();
    let property_expectations = export
        .batch
        .marking_property_reads
        .iter()
        .map(|(exp, _)| exp);
    for expectation in export
        .batch
        .marking_reads
        .iter()
        .chain(&export.batch.marking_value_reads)
        .chain(&export.batch.marking_reference_reads)
        .chain(property_expectations)
    {
        if expectation.key.object_type.as_str() == ACTION_CALL_TYPE {
            continue;
        }
        read_versions.insert(expectation.key.clone(), expectation.expected.0);
    }
    read_versions
}

pub(super) fn stage_replay<S: ForkEngine>(
    main: &SqliteCell<FactEvent, JsonObject, JsonObject, S>,
    pin: &ForkPin,
    main_head: &LedgerHead,
    definitions: &AdmittedDefinitions,
    authorizer: &dyn ActionAuthorizer,
    commands: &[AuthenticatedForkCommand<'_>],
    start_time_ms: u64,
) -> Result<Vec<Vec<String>>, CellError> {
    let staging_world = format!("__norn_promote_stage:{}", pin.world);
    let staging_store = main.store().fork(&staging_world, main_head)?;
    let mut staging: SqliteCell<FactEvent, JsonObject, JsonObject, S> = SqliteCell::new(
        staging_store,
        &main.scope,
        |command: &Command<JsonObject>| command.payload.clone(),
    )?;
    staging.advance_to(LogicalTime::from_millis(start_time_ms));
    let catalog = definitions.catalog();
    let mut all_moved = Vec::with_capacity(commands.len());
    for (idx, auth_cmd) in commands.iter().enumerate() {
        let moved_reads = detect_moved_reads(&staging, &auth_cmd.read_versions)?;
        let turn_time = replay_turn_time(start_time_ms, idx)?;
        staging.advance_to(turn_time);
        verify_main_world_intent(
            &staging,
            definitions,
            &catalog,
            auth_cmd,
            authorizer,
            turn_time,
            &moved_reads,
        )?;
        let fork_world = staging.store().world();
        let outcome = run_replay_call(
            &mut staging,
            definitions,
            &catalog,
            auth_cmd,
            authorizer,
            turn_time,
            fork_world,
            false,
            &moved_reads,
        )?;
        verify_staged_material_outcome(&staging, auth_cmd, &outcome, &moved_reads)?;
        all_moved.push(moved_reads);
    }
    Ok(all_moved)
}

#[allow(
    clippy::too_many_arguments,
    reason = "read-only main-world intent check needs cell, catalog, command, authorizer, time and moved reads"
)]
fn verify_main_world_intent<S: SqlEngine>(
    staging: &SqliteCell<FactEvent, JsonObject, JsonObject, S>,
    definitions: &AdmittedDefinitions,
    catalog: &norn_kernel::TypeCatalog<'_>,
    auth_cmd: &AuthenticatedForkCommand<'_>,
    authorizer: &dyn ActionAuthorizer,
    turn_time: LogicalTime,
    moved_reads: &[String],
) -> Result<(), CellError> {
    let Some(admitted) = definitions.action(&auth_cmd.call.action_id) else {
        return Err(merge_refused(ForkMergeRefusalReason::ActionNotAdmitted {
            action_id: auth_cmd.call.action_id.clone(),
        }));
    };
    let call = ActionCall {
        action: &admitted.root,
        action_artifact_id: &admitted.artifact_id,
        action_version: &admitted.version,
        scope: auth_cmd.call.scope.clone(),
        actor: auth_cmd.call.actor.clone(),
        approved_intent: None,
        input: auth_cmd.call.input.clone(),
        idempotency_key: auth_cmd.call.idempotency_key.clone(),
        object_type: auth_cmd.call.object_type.clone(),
        turn: DecisionTurn {
            id: auth_cmd.call.idempotency_key.clone(),
            now: turn_time,
            world: World::Main,
            consumes: None,
        },
        trace: auth_cmd.call.trace.clone(),
    };
    let prepared = prepare_action_intent(staging, catalog, &call, authorizer)
        .map_err(|refusal| map_action_refusal(auth_cmd.call, refusal, moved_reads))?;
    match prepared {
        Prepared::Commit(intent) => {
            if let Some(expected) = &auth_cmd.call.approved_intent
                && &intent.digest != expected
            {
                return Err(merge_refused(ForkMergeRefusalReason::ReplayRejected {
                    action_id: auth_cmd.call.action_id.clone(),
                    idempotency_key: auth_cmd.call.idempotency_key.clone(),
                    code: "action.approved_intent_changed".to_owned(),
                    detail: "the prepared main commit disagrees with the approved intent digest"
                        .to_owned(),
                }));
            }
            Ok(())
        }
        Prepared::Replayed(_) => Err(merge_refused(ForkMergeRefusalReason::ReplayRejected {
            action_id: auth_cmd.call.action_id.clone(),
            idempotency_key: auth_cmd.call.idempotency_key.clone(),
            code: "action.idempotency_conflict".to_owned(),
            detail: "idempotency key was already committed on main before promotion".to_owned(),
        })),
    }
}

fn with_commit_precondition<S: SqlEngine, T>(
    main: &mut SqliteCell<FactEvent, JsonObject, JsonObject, S>,
    precondition: CommitPrecondition,
    act: impl FnOnce(&mut SqliteCell<FactEvent, JsonObject, JsonObject, S>) -> T,
) -> T {
    main.store().commit_precondition.replace(Some(precondition));
    let output = act(main);
    main.store().commit_precondition.replace(None);
    output
}

#[allow(
    clippy::too_many_arguments,
    reason = "main replay binds cell, staged head, definitions version, catalog, commands, moved reads and clock"
)]
pub(super) fn replay_into_main<S: SqlEngine>(
    main: &mut SqliteCell<FactEvent, JsonObject, JsonObject, S>,
    expected_main_head: &LedgerHead,
    expected_definitions_version: Option<u64>,
    definitions: &AdmittedDefinitions,
    authorizer: &dyn ActionAuthorizer,
    commands: &[AuthenticatedForkCommand<'_>],
    all_moved: &[Vec<String>],
    start_time_ms: u64,
) -> Result<(Vec<PromotedCommand>, LedgerHead), CellError> {
    let mut expected_head = expected_main_head.clone();
    let catalog = definitions.catalog();
    let mut promoted = Vec::with_capacity(commands.len());
    for (idx, auth_cmd) in commands.iter().enumerate() {
        let moved_reads = all_moved.get(idx).cloned().unwrap_or_default();
        let turn_time = replay_turn_time(start_time_ms, idx)?;
        main.advance_to(turn_time);
        let precondition = CommitPrecondition {
            expected_head: expected_head.clone(),
            expected_definitions_version,
        };
        let outcome = match with_commit_precondition(main, precondition, |cell| {
            run_replay_call(
                cell,
                definitions,
                &catalog,
                auth_cmd,
                authorizer,
                turn_time,
                World::Main,
                true,
                &moved_reads,
            )
        }) {
            Ok(outcome) => outcome,
            Err(error) => {
                if promoted.is_empty() {
                    return Err(error);
                }
                return Err(merge_refused(ForkMergeRefusalReason::PartialMainReplay {
                    committed: promoted,
                    failed_action_id: auth_cmd.call.action_id.clone(),
                    failed_idempotency_key: auth_cmd.call.idempotency_key.clone(),
                    detail: error.to_string(),
                }));
            }
        };
        let ActionOutcome::Committed {
            receipt,
            record,
            target_version,
        } = outcome
        else {
            return Err(partial_or_initial_refusal(
                promoted,
                auth_cmd.call,
                "action.idempotency_conflict",
                "idempotency key was already committed on main".to_owned(),
            ));
        };
        let main_commit_seq = receipt.seq.0;
        let main_receipt_hash = match receipt_hash_at(main, main_commit_seq) {
            Ok(hash) => hash,
            Err(error) => {
                promoted.push(PromotedCommand {
                    action_id: auth_cmd.call.action_id.clone(),
                    idempotency_key: auth_cmd.call.idempotency_key.clone(),
                    fork_commit_seq: auth_cmd.fork_commit_seq,
                    main_commit_seq,
                    main_receipt_hash: String::new(),
                    target: record.object,
                    target_version: target_version.0,
                    moved_reads,
                });
                return Err(merge_refused(ForkMergeRefusalReason::PartialMainReplay {
                    committed: promoted,
                    failed_action_id: auth_cmd.call.action_id.clone(),
                    failed_idempotency_key: auth_cmd.call.idempotency_key.clone(),
                    detail: format!(
                        "committed on main at seq {main_commit_seq}, but receipt lookup failed: {error}"
                    ),
                }));
            }
        };
        promoted.push(PromotedCommand {
            action_id: auth_cmd.call.action_id.clone(),
            idempotency_key: auth_cmd.call.idempotency_key.clone(),
            fork_commit_seq: auth_cmd.fork_commit_seq,
            main_commit_seq,
            main_receipt_hash,
            target: record.object,
            target_version: target_version.0,
            moved_reads,
        });
        expected_head = match main.store().verified_head() {
            Ok(head) => head,
            Err(error) => {
                return Err(merge_refused(ForkMergeRefusalReason::PartialMainReplay {
                    committed: promoted,
                    failed_action_id: auth_cmd.call.action_id.clone(),
                    failed_idempotency_key: auth_cmd.call.idempotency_key.clone(),
                    detail: format!(
                        "committed on main at seq {main_commit_seq}, but verified_head failed: {error}"
                    ),
                }));
            }
        };
    }
    Ok((promoted, expected_head))
}

fn partial_or_initial_refusal(
    committed: Vec<PromotedCommand>,
    command: &ForkActionCall,
    code: &str,
    detail: String,
) -> CellError {
    if committed.is_empty() {
        merge_refused(ForkMergeRefusalReason::ReplayRejected {
            action_id: command.action_id.clone(),
            idempotency_key: command.idempotency_key.clone(),
            code: code.to_owned(),
            detail,
        })
    } else {
        merge_refused(ForkMergeRefusalReason::PartialMainReplay {
            committed,
            failed_action_id: command.action_id.clone(),
            failed_idempotency_key: command.idempotency_key.clone(),
            detail,
        })
    }
}

fn replay_turn_time(start_time_ms: u64, idx: usize) -> Result<LogicalTime, CellError> {
    let step = u64::try_from(idx)
        .ok()
        .and_then(|value| value.checked_add(1))
        .and_then(|offset| start_time_ms.checked_add(offset))
        .ok_or(CellError::OutOfRange("fork promotion logical time"))?;
    Ok(LogicalTime::from_millis(step))
}

fn detect_moved_reads<S: SqlEngine>(
    cell: &SqliteCell<FactEvent, JsonObject, JsonObject, S>,
    read_versions: &BTreeMap<FactKey, u64>,
) -> Result<Vec<String>, CellError> {
    let mut moved = Vec::new();
    for (key, expected) in read_versions {
        let state = cell.load(key)?;
        if state.version.0 != *expected {
            moved.push(format!(
                "{key} (fork v{expected} -> main v{})",
                state.version.0
            ));
        }
    }
    Ok(moved)
}

#[allow(
    clippy::too_many_arguments,
    reason = "replay helper binds target cell, definitions, catalog, command, authorizer, clock, world and approval flag"
)]
fn run_replay_call<S: SqlEngine>(
    target_cell: &mut SqliteCell<FactEvent, JsonObject, JsonObject, S>,
    definitions: &AdmittedDefinitions,
    catalog: &norn_kernel::TypeCatalog<'_>,
    auth_cmd: &AuthenticatedForkCommand<'_>,
    authorizer: &dyn ActionAuthorizer,
    turn_time: LogicalTime,
    world: World,
    bind_approved_intent: bool,
    moved_reads: &[String],
) -> Result<ActionOutcome, CellError> {
    let Some(admitted) = definitions.action(&auth_cmd.call.action_id) else {
        return Err(merge_refused(ForkMergeRefusalReason::ActionNotAdmitted {
            action_id: auth_cmd.call.action_id.clone(),
        }));
    };
    let call = ActionCall {
        action: &admitted.root,
        action_artifact_id: &admitted.artifact_id,
        action_version: &admitted.version,
        scope: auth_cmd.call.scope.clone(),
        actor: auth_cmd.call.actor.clone(),
        approved_intent: if bind_approved_intent {
            auth_cmd.call.approved_intent.clone()
        } else {
            None
        },
        input: auth_cmd.call.input.clone(),
        idempotency_key: auth_cmd.call.idempotency_key.clone(),
        object_type: auth_cmd.call.object_type.clone(),
        turn: DecisionTurn {
            id: auth_cmd.call.idempotency_key.clone(),
            now: turn_time,
            world,
            consumes: None,
        },
        trace: auth_cmd.call.trace.clone(),
    };
    commit_action(target_cell, catalog, &call, authorizer)
        .map_err(|refusal| map_action_refusal(auth_cmd.call, refusal, moved_reads))
}

fn verify_staged_material_outcome<S: SqlEngine>(
    staging_cell: &SqliteCell<FactEvent, JsonObject, JsonObject, S>,
    auth_cmd: &AuthenticatedForkCommand<'_>,
    outcome: &ActionOutcome,
    moved_reads: &[String],
) -> Result<(), CellError> {
    let ActionOutcome::Committed {
        receipt, record, ..
    } = outcome
    else {
        return Err(merge_refused(ForkMergeRefusalReason::ReplayRejected {
            action_id: auth_cmd.call.action_id.clone(),
            idempotency_key: auth_cmd.call.idempotency_key.clone(),
            code: "action.idempotency_conflict".to_owned(),
            detail: "idempotency key was already committed on main before promotion".to_owned(),
        }));
    };
    let Some(replayed_export) = staging_cell.store().local_export_record(receipt.seq.0)? else {
        return Err(CellError::Engine(
            "staged commit lacks its local export record".to_owned(),
        ));
    };
    let replayed_outcome = MaterialOutcome::from_export(&replayed_export, record);
    if replayed_outcome != auth_cmd.material_outcome {
        let detail = if moved_reads.is_empty() {
            "re-evaluated writes, links, events, effects, or quantity results differ from the decision fork (definitions, links, or predicate slice changed)"
        } else {
            "re-evaluated writes, links, events, effects, or quantity results differ from the decision fork after moved read versions"
        };
        return Err(merge_refused(
            ForkMergeRefusalReason::MaterialOutcomeChanged {
                action_id: auth_cmd.call.action_id.clone(),
                idempotency_key: auth_cmd.call.idempotency_key.clone(),
                moved_reads: moved_reads.to_vec(),
                detail: detail.to_owned(),
            },
        ));
    }
    Ok(())
}

fn map_action_refusal(
    command: &ForkActionCall,
    refusal: ActionRefusal<CellError>,
    moved_reads: &[String],
) -> CellError {
    let code = refusal.code().to_owned();
    if let ActionRefusal::Decision(DecisionRefusal::Host(host_error)) = refusal {
        if matches!(
            host_error,
            CellError::Fenced(_)
                | CellError::Verifying(_)
                | CellError::ForkClosed { .. }
                | CellError::ForkMergeRefused { .. }
        ) {
            return host_error;
        }
        return merge_refused(ForkMergeRefusalReason::ReplayRejected {
            action_id: command.action_id.clone(),
            idempotency_key: command.idempotency_key.clone(),
            code,
            detail: host_error.to_string(),
        });
    }
    if let ActionRefusal::PermissionDenied { reason } = refusal {
        return merge_refused(ForkMergeRefusalReason::AuthorizationDenied {
            action_id: command.action_id.clone(),
            idempotency_key: command.idempotency_key.clone(),
            actor: command.actor.clone(),
            code,
            detail: reason,
        });
    }
    if matches!(refusal, ActionRefusal::TargetNotFound) && moved_reads.is_empty() {
        return merge_refused(ForkMergeRefusalReason::AuthorizationDenied {
            action_id: command.action_id.clone(),
            idempotency_key: command.idempotency_key.clone(),
            actor: command.actor.clone(),
            code,
            detail: "target not visible or no longer accessible under main's authority".to_owned(),
        });
    }
    let detail = format!("{code}: {refusal}");
    if !moved_reads.is_empty() {
        return merge_refused(ForkMergeRefusalReason::MaterialOutcomeChanged {
            action_id: command.action_id.clone(),
            idempotency_key: command.idempotency_key.clone(),
            moved_reads: moved_reads.to_vec(),
            detail,
        });
    }
    merge_refused(ForkMergeRefusalReason::ReplayRejected {
        action_id: command.action_id.clone(),
        idempotency_key: command.idempotency_key.clone(),
        code,
        detail,
    })
}

fn receipt_hash_at<S: SqlEngine>(
    main: &SqliteCell<FactEvent, JsonObject, JsonObject, S>,
    commit_seq: u64,
) -> Result<String, CellError> {
    let receipts = main
        .store()
        .read()
        .receipts_after(commit_seq.saturating_sub(1), 1)?;
    receipts
        .first()
        .filter(|receipt| receipt.body.commit_seq == commit_seq)
        .map(|receipt| receipt.hash.clone())
        .ok_or_else(|| {
            CellError::Engine(format!(
                "missing main receipt for promoted commit {commit_seq}"
            ))
        })
}
