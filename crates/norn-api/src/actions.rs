//! One epoch-pinned governed action path for HTTP, MCP and the remote CLI.
use crate::{
    authz::{AccessRequest, Operation},
    host::DevHost,
    refusal::Refusal,
    routes::DefinitionsQuery,
};
use norn_cell::engine::SqlEngine;
use norn_ir::RuntimeObject;
use norn_kernel::{
    ActionOutcome, DecisionTurn, LogicalTime, Refusable, TypeCatalog, World,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Exact action invocation; the actor comes exclusively from the transport.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActionRequest {
    /// Admitted action identity.
    pub action_id: String,
    /// Exact business arguments.
    pub input: RuntimeObject,
    /// Stable retry identity.
    pub idempotency_key: String,
    /// Required admitted revision pin.
    pub version: u64,
}

/// Successful committed or replayed kernel record.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionResponse {
    /// Whether this was an identical retry.
    pub replayed: bool,
    /// Kernel call record without transport reinterpretation.
    pub record: norn_kernel::ActionCallRecord,
}
/// Transport-neutral action service over one tenant's admitted cell.
/// HTTP, MCP and Workers/DO hosts call this same function; only adapters decode
/// transport metadata and render the returned receipt or shared refusal.
/// The authenticated actor comes from the trusted host, never action input.
/// Scope is plain Rust data; the action revision pin is `request.version`.
/// Authorization, definition epoch, kernel commit and retry replay stay here.
///
/// # Errors
/// Scope, policy, revision, argument and kernel refusals change no business state.
pub fn call_action_in_epoch<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &DefinitionsQuery,
    actor: Option<&str>,
    request: ActionRequest,
) -> Result<ActionResponse, Refusal> {
    let _epoch = host.definition_read()?;
    host.scope()
        .admit(scope.org_id.as_deref(), scope.environment_id.as_deref())?;
    host.authorizer.authorize(&AccessRequest {
        scope: host.scope(),
        actor,
        operation: Operation::CallAction(request.action_id.clone()),
    })?;
    let actor = actor
        .filter(|a| !a.trim().is_empty())
        .ok_or_else(|| Refusal::invalid("missing authenticated actor"))?;
    let mut cell = host.cell()?;
    let installed = cell
        .store()
        .read()
        .installed_definitions(None)
        .map_err(Refusal::host)?
        .ok_or_else(|| Refusal::host("missing definitions"))?;
    if installed.version != request.version {
        return Err(Refusal::new(
            http::StatusCode::CONFLICT,
            "defs.stale",
            "definitions changed; refresh the catalog",
            "the action revision pin is stale",
        ));
    }
    let admitted = crate::action_admission::RetainedActions::load(&installed, host.scope())?;
    let model = cell.store().model().clone();
    let catalog = TypeCatalog::new(model.object_types())
        .with_property_types(model.property_types())
        .with_interfaces(model.interfaces())
        .with_governing_laws(model.law_artifacts())
        .with_action_rules(&admitted.rules);
    let (artifact, action) = admitted.actions.iter()
        .find(|(_, action)| action.action_id == request.action_id)
        .ok_or_else(|| Refusal::invalid("unknown admitted action"))?;
    let now = LogicalTime::from_millis(cell.store().read().clock_floor().map_err(Refusal::host)?);
    cell.advance_to(now);
    let invocation = norn_kernel::ActionCall {
        action,
        action_artifact_id: &artifact.artifact_id,
        action_version: &artifact.version,
        scope: BTreeMap::from([
            ("orgId".into(), host.scope().org_id.clone()),
            ("environmentId".into(), host.scope().environment_id.clone()),
        ]),
        actor: actor.into(),
        input: request.input,
        idempotency_key: request.idempotency_key.clone(),
        object_type: None,
        turn: DecisionTurn {
            id: request.idempotency_key,
            now,
            world: World::Main,
            consumes: None,
        },
    };
    let outcome = norn_kernel::commit_action(
        &mut *cell,
        &catalog,
        &invocation,
        &crate::authz::KernelAuthorizer(host.authorizer.as_ref()),
    )
    .map_err(|error| kernel_refusal(error.refusal()))?;
    Ok(match outcome {
        ActionOutcome::Committed { record, .. } => ActionResponse {
            replayed: false,
            record,
        },
        ActionOutcome::Replayed { record } => ActionResponse {
            replayed: true,
            record,
        },
    })
}

fn kernel_refusal(safe: norn_kernel::Refusal) -> Refusal {
    Refusal::from_kernel(safe, false)
}


#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "actions_tests.rs"]
mod tests;
