//! ACTION-CATALOG-1: the retained actions an actor may call, for forms and tool
//! lists (`GET /v1/actions`).
//!
//! The list is decided by exactly the gate `call_action` applies before it
//! reads anything: a verified actor, the request scope admitted against the
//! physical cell, and the host authorizer's `Operation::CallAction` for each
//! action. There is no second permission check and no client filter. It is a
//! convenience, never a grant: every call is decided again when it is made, by
//! the same gate and by the kernel's per-target authorization, laws and
//! preconditions, so an action missing from the list is still refused when it
//! is called directly, and one on the list may still be refused for its target.
use crate::{
    action_admission::RetainedActions,
    actions::{ActionQuery, action_denied},
    authz::{AccessRequest, Operation},
    host::DevHost,
    refusal::Refusal,
};
use norn_cell::engine::SqlEngine;
use norn_ir::{ActionEffectProgram, ActionRoot, ActionUiEffect, ArtifactEnvelope};
use serde::{Deserialize, Serialize};
use utoipa::openapi::Type;
use utoipa::openapi::schema::{AdditionalProperties, Object, ObjectBuilder, SchemaType};
use utoipa::{IntoParams, ToSchema};

/// The retained actions one actor may call in one definitions revision.
#[derive(Debug)]
pub struct CallableActions {
    /// The active definitions version the list was read from; a call may pin it.
    pub version: u64,
    /// Each callable action with its retained artifact identity, in retained order.
    pub actions: Vec<(ArtifactEnvelope, ActionRoot)>,
}

/// The retained actions `actor` may call in the cell's active revision: the
/// gate `call_action` applies, per action, and nothing else.
///
/// # Errors
/// `authz.denied` (403) for a missing actor, a scope the cell does not admit
/// or a cell with nothing installed, exactly as a call would be refused;
/// `host.error` for a store failure.
pub fn callable_actions<E: SqlEngine>(
    host: &DevHost<E>,
    query: &ActionQuery,
    actor: Option<&str>,
) -> Result<CallableActions, Refusal> {
    let actor = actor
        .filter(|actor| !actor.trim().is_empty())
        .ok_or_else(action_denied)?;
    host.scope()
        .admit(query.org_id.as_deref(), query.environment_id.as_deref())
        .map_err(|_refused| action_denied())?;
    let installed = host.active_definitions()?.ok_or_else(action_denied)?;
    let retained = RetainedActions::load(&installed, host.scope())?;
    let actions = retained
        .actions
        .into_iter()
        .filter(|(_, action)| {
            host.authorizer
                .authorize(&AccessRequest {
                    scope: host.scope(),
                    actor: Some(actor),
                    operation: Operation::CallAction(action.action_id.clone()),
                })
                .is_ok()
        })
        .collect();
    Ok(CallableActions {
        version: installed.version,
        actions,
    })
}

/// `GET /v1/actions` query: the scope, and optionally one target object type.
#[derive(Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActionCatalogQuery {
    /// Organization, matched against physical scope.
    #[param(required = true, nullable = false)]
    pub org_id: Option<String>,
    /// Environment, matched against physical scope.
    #[param(required = true, nullable = false)]
    pub environment_id: Option<String>,
    /// Only actions whose effect targets this object type.
    #[param(nullable = false)]
    pub object_kind: Option<String>,
}

/// How an action's admitted effect program changes its target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum NornActionEffect {
    /// Create or update the object the input names.
    UpsertInputObject,
    /// Fire one declared lifecycle transition of the object the input names.
    FireTransition,
    /// Change a parent object and its owned rows together.
    MutateParentWithRows,
}

/// The effects a form must handle before it calls, from the action's own definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornActionUiEffects {
    /// The action declares an effect that cannot be reversed.
    pub irreversible: bool,
    /// The action may destroy existing data (declared, or a deleting write).
    pub destructive: bool,
    /// Currency identities its monetary effects use.
    pub money: Vec<String>,
}

/// One action the actor may call, as a form needs it.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornActionSummary {
    /// The admitted action identity, the path segment of its call.
    pub action_id: String,
    /// Its title, or its identity when it declares none.
    pub title: String,
    /// Its description, when it declares one.
    pub description: Option<String>,
    /// The object type its effect targets.
    pub object_kind: String,
    /// How its effect changes that object.
    pub effect: NornActionEffect,
    /// The input field that names the target object.
    pub object_id_field: String,
    /// The input a caller supplies: the admitted input schema, in the action's
    /// closed input dialect, without the fields the server derives from the
    /// verified actor (ACTION-ACTOR-1), which a caller never supplies.
    #[schema(schema_with = input_schema_schema)]
    pub input_schema: serde_json::Value,
    /// The effects a caller must handle.
    pub ui_effects: NornActionUiEffects,
}

/// `GET /v1/actions`: the callable actions, by action identity.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornActionCatalog {
    /// The definitions version this list was read from.
    pub definitions_version: u64,
    /// The callable actions an effect program can run, ordered by identity.
    pub items: Vec<NornActionSummary>,
}

// `schema_with` replaces the field's schema, doc comment included, so the
// helper restates it. The input schema is a JSON Schema object whose members
// this document does not enumerate, so it says so (`additionalProperties`).
fn input_schema_schema() -> Object {
    ObjectBuilder::new()
        .schema_type(SchemaType::Type(Type::Object))
        .additional_properties(Some(AdditionalProperties::FreeForm(true)))
        .description(Some(
            "The input a caller supplies: the admitted input schema, in the action's closed input dialect, without the fields the server derives from the verified actor (ACTION-ACTOR-1), which a caller never supplies.",
        ))
        .build()
}

/// The admitted input schema without the fields derived from the verified
/// actor: the kernel refuses a caller value for them, so a form never offers one.
fn caller_input_schema(action: &ActionRoot) -> Result<serde_json::Value, Refusal> {
    let mut schema = serde_json::to_value(&action.input_schema).map_err(Refusal::host)?;
    let derived = norn_kernel::actor_derived_fields(action);
    if let Some(properties) = schema
        .get_mut("properties")
        .and_then(serde_json::Value::as_object_mut)
    {
        properties.retain(|field, _| !derived.contains(field));
    }
    if let Some(required) = schema
        .get_mut("required")
        .and_then(serde_json::Value::as_array_mut)
    {
        required.retain(|field| field.as_str().is_none_or(|field| !derived.contains(field)));
    }
    Ok(schema)
}

fn summary(action: &ActionRoot) -> Result<Option<NornActionSummary>, Refusal> {
    let (effect, object_kind, object_id_field) = match &action.effect_program {
        Some(ActionEffectProgram::UpsertInputObject {
            object_kind,
            object_id_field,
            ..
        }) => (
            NornActionEffect::UpsertInputObject,
            object_kind,
            object_id_field,
        ),
        Some(ActionEffectProgram::FireTransition {
            object_kind,
            object_id_field,
            ..
        }) => (
            NornActionEffect::FireTransition,
            object_kind,
            object_id_field,
        ),
        Some(ActionEffectProgram::MutateParentWithRows {
            object_kind,
            object_id_field,
            ..
        }) => (
            NornActionEffect::MutateParentWithRows,
            object_kind,
            object_id_field,
        ),
        // Without an effect program the kernel has nothing it can run for a form.
        None => return Ok(None),
    };
    let effects = action.effective_ui_effects();
    Ok(Some(NornActionSummary {
        action_id: action.action_id.clone(),
        title: action
            .title
            .clone()
            .unwrap_or_else(|| action.action_id.clone()),
        description: action.description.clone(),
        object_kind: object_kind.clone(),
        effect,
        object_id_field: object_id_field.clone(),
        input_schema: caller_input_schema(action)?,
        ui_effects: NornActionUiEffects {
            irreversible: effects.contains(&ActionUiEffect::Irreversible),
            destructive: effects.contains(&ActionUiEffect::Destructive),
            money: effects
                .iter()
                .filter_map(|effect| {
                    if let ActionUiEffect::Money { currency_ref } = effect {
                        Some(currency_ref.clone())
                    } else {
                        None
                    }
                })
                .collect(),
        },
    }))
}

/// `GET /v1/actions`: the actions the caller may call, each as a form needs it.
///
/// # Errors
/// As [`callable_actions`]; `host.error` when a retained action cannot be projected.
#[utoipa::path(get, path = "/v1/actions", operation_id = "list_actions", tag = "crate::actions", params(ActionCatalogQuery), responses((status = 200, description = "The actions the caller may call now. Each call is still decided when it is made.", body = NornActionCatalog), (status = 400, description = "The request was refused as invalid.", body = Refusal), (status = 403, description = "The request was refused: scope or authority denied.", body = Refusal), (status = 500, description = "The request was refused: host failure.", body = Refusal)))]
pub fn list_actions<E: SqlEngine>(
    host: &DevHost<E>,
    query: &ActionCatalogQuery,
    actor: Option<&str>,
) -> Result<NornActionCatalog, Refusal> {
    let callable = callable_actions(
        host,
        &ActionQuery {
            org_id: query.org_id.clone(),
            environment_id: query.environment_id.clone(),
            version: None,
        },
        actor,
    )?;
    let mut items = callable
        .actions
        .iter()
        .map(|(_, action)| summary(action))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .flatten()
        .filter(|item| {
            query
                .object_kind
                .as_deref()
                .is_none_or(|kind| kind == item.object_kind)
        })
        .collect::<Vec<_>>();
    items.sort_by(|a, b| a.action_id.cmp(&b.action_id));
    Ok(NornActionCatalog {
        definitions_version: callable.version,
        items,
    })
}
