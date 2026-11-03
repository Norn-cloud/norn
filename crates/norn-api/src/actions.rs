//! One epoch-pinned governed action path for HTTP, MCP and the remote CLI.
use crate::{
    authz::{AccessRequest, Operation},
    host::DevHost,
    refusal::Refusal,
    routes::DefinitionsQuery,
};
use norn_cell::engine::SqlEngine;
use norn_ir::RuntimeObject;
use norn_kernel::{ActionOutcome, DecisionTurn, LogicalTime, Refusable, TypeCatalog, World};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Add kernel-owned receipt and owned-row schemas before provisioning the store.
/// Preserves business definitions, interfaces, laws and binding indexes.
/// # Errors
/// Tenant declarations using SQLite-equivalent reserved names or invalid models refuse.
pub fn prepare_action_model(model: &norn_cell::Model) -> Result<norn_cell::Model, Refusal> {
    use norn_ir::{ObjectTypeDef, ObjectTypeName, Property, PropertyId, PropertyType};
    let mut snapshot = model.definitions_snapshot();
    for (name, text_fields, json_fields) in [
        (
            norn_kernel::ACTION_CALL_TYPE,
            &[
                "actionId",
                "actionArtifactId",
                "actionVersion",
                "effectKind",
                "actor",
                "turn",
                "transition",
                "from",
                "to",
                "ruleBinding",
            ][..],
            &[
                "object",
                "input",
                "effectiveInput",
                "derivedValues",
                "preconditionOutcomes",
                "rowOperations",
            ][..],
        ),
        (
            norn_kernel::OWNED_ROWS_TYPE,
            &["parentType", "parentKey", "collectionId"][..],
            &["rowIds"][..],
        ),
    ] {
        if snapshot
            .object_types
            .iter()
            .any(|ty| ty.name.as_str().eq_ignore_ascii_case(name))
            || snapshot
                .interfaces
                .iter()
                .any(|ty| ty.name.as_str().eq_ignore_ascii_case(name))
        {
            return Err(Refusal::invalid("reserved kernel object type"));
        }
        let properties = text_fields
            .iter()
            .map(|id| (*id, PropertyType::Text { format: None }))
            .chain(
                json_fields
                    .iter()
                    .map(|id| (*id, PropertyType::JsonExt { schema: None })),
            )
            .map(|(id, property_type)| Property {
                id: PropertyId(id.into()),
                property_type,
                description: None,
                constraints: norn_ir::PropertyConstraints {
                    nullable: true,
                    ..Default::default()
                },
                default: None,
                markings: vec![],
                computed: None,
            })
            .collect();
        snapshot.object_types.push(ObjectTypeDef {
            name: ObjectTypeName(name.into()),
            role: None,
            description: None,
            status: norn_ir::ResourceStatus::default(),
            extends: None,
            implements: vec![],
            properties,
            links: vec![],
            owned_collections: vec![],
            identity: vec![],
            invariants: vec![],
            lifecycles: vec![],
            markings: vec![],
            accounting_leg: None,
        });
    }
    norn_cell::Model::from_snapshot(&snapshot).map_err(Refusal::host)
}

/// Exact action invocation; the actor comes exclusively from the transport.
#[derive(Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActionRequest {
    /// Admitted action identity.
    pub action_id: String,
    /// Exact business arguments.
    #[schema(schema_with = action_input_schema)]
    pub input: RuntimeObject,
    /// Stable retry identity.
    pub idempotency_key: String,
    /// Required admitted revision pin.
    pub version: u64,
}

/// Successful committed or replayed kernel record.
#[derive(Serialize, utoipa::ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ActionResponse {
    /// Whether this was an identical retry.
    pub replayed: bool,
    /// Kernel call record without transport reinterpretation.
    #[schema(schema_with = action_record_schema)]
    pub record: norn_kernel::ActionCallRecord,
}
fn action_input_schema() -> utoipa::openapi::Ref {
    utoipa::openapi::Ref::from_schema_name("NornActionInput")
}
fn action_record_schema() -> utoipa::openapi::Ref {
    utoipa::openapi::Ref::from_schema_name("NornActionCallRecord")
}
/// Commit or replay an admitted action under the host's definition epoch.
/// Trusted transports supply the actor; authorization and storage remain server-owned.
/// # Errors
/// Scope, policy, version, arguments or kernel refusal.
#[utoipa::path(post, path = "/v1/actions", request_body = ActionRequest,
    responses(
        (status = 200, body = ActionResponse),
        (status = 400, body = Refusal),
        (status = 403, body = Refusal),
        (status = 404, body = Refusal),
        (status = 409, body = Refusal),
        (status = 422, body = Refusal),
        (status = 429, body = Refusal),
        (status = 500, body = Refusal)
    ))]
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
    if installed.version != request.version
        || scope
            .version
            .is_some_and(|version| version != request.version)
    {
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
    let (artifact, action) = admitted
        .actions
        .iter()
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
    .map_err(|error| {
        crate::refusal_disclosure::project_refusal(
            host.authorizer.as_ref(),
            &crate::authz::RefusalDetailRequest {
                scope: host.scope(),
                actor,
                action_id: &action.action_id,
            },
            error.refusal(),
        )
    })?;
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

#[cfg(all(test, not(target_arch = "wasm32")))]
#[path = "actions_tests.rs"]
mod tests;
