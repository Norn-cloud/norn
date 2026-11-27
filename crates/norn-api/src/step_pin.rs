//! STEP-PIN-1 (D2): pin parked `norn.action@1` flow steps to the exact
//! `(artifact_id, version, effective_digest)` of the action definition that
//! was active when the flow run started (and any explicit `pin` declared on
//! the step), failing closed with a typed conflict refusal if the active
//! catalog drifted to a different action contract.

use http::StatusCode;
use norn_cell::engine::SqlEngine;
use norn_cell::flow_run::ActionStepPin;
use norn_ir::{ActionRoot, ArtifactEnvelope, RuntimeValue};
use norn_kernel::cell_host::CellHost;
use norn_kernel::{ACTION_CALL_TYPE, ActionCallRecord, FactKey};

use crate::action_admission::RetainedActions;
use crate::host::{ActionCell, DevHost};
use crate::refusal::Refusal;

/// Stable `action_step.*` refusal codes for parked step pin enforcement (`STEP-PIN-1`).
pub mod step_pin_codes {
    /// The action definition (its artifact id, version, payload, writer,
    /// governing laws, or written entity contract) changed or was retired
    /// after the flow run started, so the parked step cannot safely execute
    /// under the active catalog.
    pub const DEFINITION_CHANGED: &str = "action_step.definition_changed";
    /// The active action definition (or installed definitions version) does
    /// not match the explicit `pin` carried by the `norn.action@1` step.
    pub const PIN_MISMATCH: &str = "action_step.pin_mismatch";
    /// The retained definitions set for the flow run's starting definitions
    /// version is unavailable or unreadable, so equivalence cannot be proven.
    pub const PIN_UNAVAILABLE: &str = "action_step.pin_unavailable";
}

/// STEP-PIN-1: target-definition pin verified atomically inside
/// [`crate::actions::call_action_bound`] under the same `definition_read` and
/// `host.cell` hold as execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StepActionPin {
    /// The cell definitions version recorded on the run's starting commit
    /// (`run.created_commit_seq` receipt `definitions_version`).
    pub(crate) run_definitions_version: Option<u64>,
    /// Explicit pin declared in the `norn.action@1` step input, if any.
    pub(crate) explicit: Option<ActionStepPin>,
}

pub(crate) fn definition_changed() -> Refusal {
    Refusal::new(
        StatusCode::CONFLICT,
        step_pin_codes::DEFINITION_CHANGED,
        "the parked flow step's action definition changed after the run started",
        "start a new run or restore a compatible action definition before retrying",
    )
}

pub(crate) fn pin_mismatch() -> Refusal {
    Refusal::new(
        StatusCode::CONFLICT,
        step_pin_codes::PIN_MISMATCH,
        "the active action definition does not match the parked flow step's pin",
        "retarget the step pin or install the pinned definition version",
    )
}

pub(crate) fn pin_unavailable() -> Refusal {
    Refusal::new(
        StatusCode::CONFLICT,
        step_pin_codes::PIN_UNAVAILABLE,
        "the flow run's starting definitions set is unavailable to verify step compatibility",
        "restore the retained starting definitions set before retrying the parked step",
    )
}

/// Resolve `action_id` from `admitted` while enforcing `STEP-PIN-1` for
/// unapproved parked flow step dispatches: refuses `action_step.pin_mismatch`
/// or `action_step.definition_changed` when the action was retired or the
/// explicit `definitionsVersion` drifted, and returns `Ok(None)` when the
/// caller should see the normal opaque `action_denied`.
pub(crate) fn resolve_step_action<'a, E: SqlEngine>(
    host: &DevHost<E>,
    cell: &ActionCell<E>,
    installed_version: u64,
    admitted: &'a RetainedActions,
    action_id: &str,
    step_pin: Option<&StepActionPin>,
) -> Result<Option<(&'a ArtifactEnvelope, &'a ActionRoot)>, Refusal> {
    let found = admitted
        .actions
        .iter()
        .find(|(_, action)| action.action_id == action_id)
        .map(|(artifact, action)| (artifact, action));
    let Some(pin) = step_pin else {
        return Ok(found);
    };
    let Some(target) = found else {
        check_missing_active_action(host, cell, installed_version, action_id, pin)?;
        return Ok(None);
    };
    verify_explicit_definitions_version(installed_version, pin)?;
    Ok(Some(target))
}

/// Verify explicit `pin.definitionsVersion` before `query.version` runs so an
/// explicit step pin produces `action_step.pin_mismatch` rather than a generic
/// stale-version error.
fn verify_explicit_definitions_version(
    installed_version: u64,
    pin: &StepActionPin,
) -> Result<(), Refusal> {
    if pin
        .explicit
        .as_ref()
        .and_then(|explicit| explicit.definitions_version)
        .is_some_and(|expected| expected != installed_version)
    {
        return Err(pin_mismatch());
    }
    Ok(())
}

/// STEP-PIN-1 retry recovery: if `attempt` already committed `action_id`
/// under `idempotency_key` before an interruption prior to step settlement,
/// recover its committed target `(object_type, object_id, version)` directly
/// from the cell's ledger so a subsequent definition change cannot turn an
/// already-committed attempt into a pin refusal.
pub(crate) fn committed_step_outcome<E: SqlEngine>(
    cell: &ActionCell<E>,
    action_id: &str,
    idempotency_key: &str,
    attempt: &crate::approvals::StepAttempt,
) -> Result<Option<(String, String, u64)>, Refusal> {
    let key = FactKey::new(ACTION_CALL_TYPE, format!("{action_id}/{idempotency_key}"));
    let recorded = CellHost::load(cell, &key).map_err(Refusal::host)?;
    let Some(record) = recorded
        .values
        .as_ref()
        .and_then(ActionCallRecord::from_values)
    else {
        return Ok(None);
    };
    if record.action_id != action_id
        || !crate::flow_driver::owns(cell.store(), action_id, idempotency_key, attempt)?
    {
        return Ok(None);
    }
    crate::actions::recorded_target(cell.store(), &record, idempotency_key).map(Some)
}

/// Check why `action_id` is absent from the active catalog when executing a
/// parked flow step: fail closed with `action_step.pin_mismatch` if an
/// explicit pin was declared, `action_step.pin_unavailable` if run-start
/// evidence is missing, or `action_step.definition_changed` if the action
/// existed in the run's starting definitions set and was retired.
fn check_missing_active_action<E: SqlEngine>(
    host: &DevHost<E>,
    cell: &ActionCell<E>,
    installed_version: u64,
    action_id: &str,
    pin: &StepActionPin,
) -> Result<(), Refusal> {
    if pin.explicit.is_some() {
        return Err(pin_mismatch());
    }
    let started_version = pin.run_definitions_version.ok_or_else(pin_unavailable)?;
    if started_version == installed_version {
        return Ok(());
    }
    let (start_actions, _) = load_retained_version(host, cell, started_version)?;
    if start_actions
        .actions
        .iter()
        .any(|(_, action)| action.action_id == action_id)
    {
        return Err(definition_changed());
    }
    Ok(())
}

/// Build the active type catalog, compute the action's effective digest, and
/// verify that `(artifact, effective_digest, installed_version)` satisfies
/// both any explicit `pin` and the implicit run-start action contract pinned
/// at `run_definitions_version`.
pub(crate) fn prepare_verified_catalog<E: SqlEngine>(
    host: &DevHost<E>,
    cell: &ActionCell<E>,
    installed_version: u64,
    admitted: &RetainedActions,
    active_artifact: &ArtifactEnvelope,
    active_action: &ActionRoot,
    step_pin: Option<&StepActionPin>,
) -> Result<(norn_kernel::TypeCatalog, String), Refusal> {
    let model = cell.store().model().clone();
    // ADMIT-CACHE-1: the model's definition work is derived once per model.
    let catalog = model
        .type_catalog()
        .with_governing_laws(model.law_artifacts())
        .with_action_rules(&admitted.rules)
        .with_transition_guards(&admitted.guards);
    let active_digest =
        crate::action_admission::effective_digest(admitted, active_action, &model, &catalog)?;
    if let Some(pin) = step_pin {
        verify_step_pin(
            host,
            cell,
            installed_version,
            active_artifact,
            active_action,
            &active_digest,
            pin,
        )?;
    }
    Ok((catalog, active_digest))
}

/// Verify that the active `(artifact, effective_digest, installed_version)`
/// satisfies both the explicit `pin` (if present) and the implicit run-start
/// action contract pinned at `run_definitions_version`.
pub(crate) fn verify_step_pin<E: SqlEngine>(
    host: &DevHost<E>,
    cell: &ActionCell<E>,
    installed_version: u64,
    active_artifact: &ArtifactEnvelope,
    active_action: &ActionRoot,
    active_digest: &str,
    pin: &StepActionPin,
) -> Result<(), Refusal> {
    if let Some(explicit) = &pin.explicit {
        verify_explicit_fields(installed_version, active_artifact, active_digest, explicit)?;
    }
    let started_version = pin.run_definitions_version.ok_or_else(pin_unavailable)?;
    // Fast path: definitions have not changed since the run started.
    if started_version == installed_version {
        return Ok(());
    }
    let (start_actions, start_model) = load_retained_version(host, cell, started_version)?;
    let Some((start_artifact, start_action)) = start_actions
        .actions
        .iter()
        .find(|(_, action)| action.action_id == active_action.action_id)
    else {
        return Err(definition_changed());
    };
    let start_catalog = start_model
        .type_catalog()
        .with_governing_laws(start_model.law_artifacts())
        .with_action_rules(&start_actions.rules)
        .with_transition_guards(&start_actions.guards);
    let start_digest = crate::action_admission::effective_digest(
        &start_actions,
        start_action,
        &start_model,
        &start_catalog,
    )
    .map_err(|_| pin_unavailable())?;
    if start_artifact.artifact_id != active_artifact.artifact_id
        || start_artifact.version != active_artifact.version
        || start_digest != active_digest
    {
        return Err(definition_changed());
    }
    Ok(())
}

fn verify_explicit_fields(
    installed_version: u64,
    active_artifact: &ArtifactEnvelope,
    active_digest: &str,
    explicit: &ActionStepPin,
) -> Result<(), Refusal> {
    if explicit
        .definitions_version
        .is_some_and(|expected| expected != installed_version)
        || explicit
            .artifact_id
            .as_deref()
            .is_some_and(|expected| expected != active_artifact.artifact_id)
        || explicit
            .version
            .as_deref()
            .is_some_and(|expected| expected != active_artifact.version)
        || explicit
            .digest
            .as_deref()
            .is_some_and(|expected| expected != active_digest)
    {
        return Err(pin_mismatch());
    }
    Ok(())
}

fn load_retained_version<E: SqlEngine>(
    host: &DevHost<E>,
    cell: &ActionCell<E>,
    version: u64,
) -> Result<(RetainedActions, norn_cell::Model), Refusal> {
    let set = cell
        .store()
        .read()
        .installed_definitions(Some(version))
        .map_err(Refusal::host)?
        .ok_or_else(pin_unavailable)?;
    let actions = RetainedActions::load(&set, host.scope()).map_err(|_| pin_unavailable())?;
    let RuntimeValue::Array(sources) =
        RuntimeValue::from_json_text(&set.canonical_set).map_err(|_| pin_unavailable())?
    else {
        return Err(pin_unavailable());
    };
    let artifacts = sources
        .iter()
        .map(norn_ir::runtime::decode)
        .collect::<Result<Vec<ArtifactEnvelope>, _>>()
        .map_err(|_| pin_unavailable())?;
    let model = crate::kernel_set::serving_model_installed(&artifacts, &set.model_hash)
        .map_err(|_| pin_unavailable())?;
    Ok((actions, model))
}

/// S5a: the record under a step's call key was not committed by that step
/// attempt's own dispatch, so it is never the step's outcome.
pub(crate) fn foreign_step_record() -> Refusal {
    Refusal::new(
        StatusCode::CONFLICT,
        "action.idempotency_conflict",
        "another call holds this step's call key",
        "the record under the step's call key was not committed by the step's own call",
    )
}

/// S5a: a step attempt's call answers only with the record its own dispatch
/// committed, now or on an earlier try, never another's under its key.
pub(crate) fn require_owned<E: SqlEngine>(
    store: &norn_cell::CellStore<E>,
    action_id: &str,
    idempotency_key: &str,
    attempt: Option<&crate::approvals::StepAttempt>,
) -> Result<(), Refusal> {
    match attempt {
        Some(attempt) if !crate::flow_driver::owns(store, action_id, idempotency_key, attempt)? => {
            Err(foreign_step_record())
        }
        _owned_or_direct => Ok(()),
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "assembles norn_kernel::ActionCall from the verified call_action_bound inputs"
)]
pub(crate) fn build_kernel_call<'a, E: SqlEngine>(
    host: &DevHost<E>,
    cell: &ActionCell<E>,
    artifact: &'a ArtifactEnvelope,
    action: &'a ActionRoot,
    actor: &str,
    input: &norn_ir::RuntimeObject,
    idempotency_key: &str,
    now: norn_kernel::LogicalTime,
    trace: Option<norn_ir::TraceContext>,
) -> norn_kernel::ActionCall<'a> {
    norn_kernel::ActionCall {
        action,
        action_artifact_id: &artifact.artifact_id,
        action_version: &artifact.version,
        scope: std::collections::BTreeMap::from([
            ("orgId".into(), host.scope().org_id.clone()),
            ("environmentId".into(), host.scope().environment_id.clone()),
        ]),
        actor: actor.to_owned(),
        approved_intent: None,
        input: input.clone(),
        idempotency_key: idempotency_key.to_owned(),
        object_type: None,
        turn: norn_kernel::DecisionTurn {
            id: format!("{}/{idempotency_key}", action.action_id),
            now,
            // L5 (Fable S2): the turn's world is the store's pin, never assumed.
            world: cell.store().world(),
            consumes: None,
        },
        trace,
    }
}
