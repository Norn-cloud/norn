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
//!
//! ACTION-CATALOG-STATE-1: with `objectId` (and `objectKind`), the list is the
//! actions that can act on that object now: a `fire_transition` action is
//! left out unless the object's current lifecycle state is one its
//! transition fires from. The object is read with the reader's own authority
//! (`GET /v1/objects/{type}/{id}`), so an object the reader cannot see is
//! refused exactly as that read is, and its state is never disclosed. Guards
//! are not evaluated here: the call decides them.
use crate::{
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
    let (version, retained) = {
        let _epoch = host.definition_read()?;
        let cell = host.cell()?;
        // ADMIT-CACHE-1 (#1533): verified once per definitions version.
        host.retained(&cell.store().read())?
            .ok_or_else(action_denied)?
    };
    let actions = retained
        .actions
        .iter()
        .filter(|(_, action)| {
            host.authorizer
                .authorize(&AccessRequest {
                    scope: host.scope(),
                    actor: Some(actor),
                    operation: Operation::CallAction(action.action_id.clone()),
                })
                .is_ok()
        })
        .cloned()
        .collect();
    Ok(CallableActions { version, actions })
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
    /// With `objectKind`: only actions that can act on this object in its
    /// current lifecycle state (ACTION-CATALOG-STATE-1).
    #[param(nullable = false)]
    #[serde(default)]
    pub object_id: Option<String>,
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
    /// Input fields that name an existing record (a link the action creates
    /// to it), so a form offers a picker of that type's records
    /// (ACTION-CATALOG-LINKS-1). Omitted when none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_links: Vec<NornActionInputLink>,
    /// For a `fire_transition` action: the lifecycle state its transition
    /// leaves the object in, so a receipt can say what the record is now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_state: Option<String>,
}

/// One input field that names a record of `objectKind`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NornActionInputLink {
    /// The top-level input field holding the record's id.
    pub field: String,
    /// The object type it names.
    pub object_kind: String,
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

/// The created links whose target is an existing record: a required slice
/// read of the target type keyed by the link's input field, which the commit
/// refuses when the record is absent. A target the same proposal creates
/// (a composed transition's `created` write) is not one, so a form keeps its
/// caller-supplied id rather than a picker of stored records.
fn existing_targets(action: &ActionRoot) -> Vec<NornActionInputLink> {
    action
        .create_links
        .iter()
        .filter(|link| {
            action.slice.iter().any(|entry| {
                !entry.optional
                    && entry.object_type.as_str() == link.target_type
                    && matches!(&entry.key, norn_ir::SliceKey::Property { from, property }
                        if from == "input" && property.as_str() == link.target_id_field)
            })
        })
        .map(|link| NornActionInputLink {
            field: link.target_id_field.clone(),
            object_kind: link.target_type.clone(),
        })
        .collect()
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
        input_links: existing_targets(action),
        to_state: None,
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
    list_actions_on(host, query, actor, None)
}

/// [`list_actions`], narrowed to one object's current state when the query
/// names `objectId` (ACTION-CATALOG-STATE-1). The object is read through
/// `access`, the reader's object gate; without one, `objectId` is refused.
///
/// # Errors
/// As [`list_actions`]; the object read's own refusal (one opaque 403 or
/// 404); `objectId` without `objectKind` or without an object gate (400).
pub fn list_actions_on<E: SqlEngine>(
    host: &DevHost<E>,
    query: &ActionCatalogQuery,
    actor: Option<&str>,
    access: Option<&dyn crate::object_routes::ObjectAccess>,
) -> Result<NornActionCatalog, Refusal> {
    let states = match (&query.object_id, &query.object_kind, access) {
        (None, _, _) => None,
        (Some(id), Some(kind), Some(access)) => {
            Some(current_states(host, query, actor, kind, id, access)?)
        }
        (Some(_), _, _) => {
            return Err(Refusal::invalid(
                "objectId narrows the catalog of one objectKind on a host that serves object reads",
            ));
        }
    };
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
        .filter(|item| {
            states.as_ref().is_none_or(|states| {
                callable
                    .actions
                    .iter()
                    .find(|(_, action)| action.action_id == item.action_id)
                    .is_none_or(|(_, action)| fires_from(action, states))
            })
        })
        .collect::<Vec<_>>();
    items.sort_by(|a, b| a.action_id.cmp(&b.action_id));
    let targets = transition_targets(host)?;
    for item in &mut items {
        item.to_state = callable
            .actions
            .iter()
            .find(|(_, action)| action.action_id == item.action_id)
            .and_then(|(_, action)| match &action.effect_program {
                Some(ActionEffectProgram::FireTransition {
                    object_kind,
                    lifecycle,
                    transition,
                    ..
                }) => targets
                    .get(&(
                        object_kind.clone(),
                        lifecycle.as_str().to_owned(),
                        transition.as_str().to_owned(),
                    ))
                    .cloned(),
                _ => None,
            });
    }
    Ok(NornActionCatalog {
        definitions_version: callable.version,
        items,
    })
}

/// ACTION-CATALOG-STATE-1: compute the callable action ids offered for each
/// readable record in `records` in its current lifecycle state, in one
/// `callable_actions` check per page. Returns `None` when the cell serves no
/// callable actions for `actor` (or has no actions installed for `object_type`),
/// so callers without actions omit `offers`. A withheld record receives `None`
/// so lifecycle state is never disclosed (D67).
pub(crate) fn offers_for_records<E: SqlEngine>(
    host: &DevHost<E>,
    scope: &crate::scope::ScopeQuery,
    actor: &str,
    object_type: &str,
    rows: &[(bool, &norn_cell::ObjectRecord)],
) -> Result<Option<Vec<Option<Vec<String>>>>, Refusal> {
    let Ok(callable) = callable_actions(
        host,
        &ActionQuery {
            org_id: scope.org_id.clone(),
            environment_id: scope.environment_id.clone(),
            version: None,
        },
        Some(actor),
    ) else {
        return Ok(None);
    };
    let targeting: Vec<&ActionRoot> = callable
        .actions
        .iter()
        .filter_map(|(_, action)| {
            let kind = match &action.effect_program {
                Some(
                    ActionEffectProgram::UpsertInputObject { object_kind, .. }
                    | ActionEffectProgram::FireTransition { object_kind, .. }
                    | ActionEffectProgram::MutateParentWithRows { object_kind, .. },
                ) => Some(object_kind.as_str()),
                None => None,
            };
            (kind == Some(object_type)).then_some(action)
        })
        .collect();
    if targeting.is_empty() {
        return Ok(None);
    }
    let lifecycles: Vec<(String, Vec<norn_ir::entity_v2::lifecycle::Transition>)> = {
        let cell = host.cell()?;
        let reader = cell.store().read();
        reader
            .model()
            .object_types()
            .filter(|def| def.name.as_str() == object_type)
            .flat_map(|def| def.lifecycles.iter())
            .map(|lifecycle| {
                (
                    lifecycle.property.as_str().to_owned(),
                    lifecycle.transitions.clone(),
                )
            })
            .collect()
    };
    let mut result = Vec::with_capacity(rows.len());
    for (readable, record) in rows {
        if !readable {
            result.push(None);
            continue;
        }
        let states: Vec<LifecycleState> = lifecycles
            .iter()
            .map(|(property, transitions)| {
                let state = record
                    .values
                    .get(property)
                    .and_then(norn_ir::RuntimeValue::as_str)
                    .map(str::to_owned);
                (property.clone(), transitions.clone(), state)
            })
            .collect();
        let mut offered: Vec<String> = targeting
            .iter()
            .filter(|action| fires_from(action, &states))
            .map(|action| action.action_id.clone())
            .collect();
        offered.sort();
        result.push(Some(offered));
    }
    Ok(Some(result))
}

/// One lifecycle of an object: its property, its declared transitions and
/// the object's current state, if it has one.
type LifecycleState = (
    String,
    Vec<norn_ir::entity_v2::lifecycle::Transition>,
    Option<String>,
);

/// The object's current value of each lifecycle property of its type, read
/// with the reader's authority, by lifecycle property.
fn current_states<E: SqlEngine>(
    host: &DevHost<E>,
    query: &ActionCatalogQuery,
    actor: Option<&str>,
    kind: &str,
    id: &str,
    access: &dyn crate::object_routes::ObjectAccess,
) -> Result<Vec<LifecycleState>, Refusal> {
    let record = crate::object_routes::get_object(
        host,
        &crate::scope::ScopeQuery {
            org_id: query.org_id.clone(),
            environment_id: query.environment_id.clone(),
        },
        actor,
        kind,
        id,
        access,
    )?;
    let cell = host.cell()?;
    let reader = cell.store().read();
    Ok(reader
        .model()
        .object_types()
        .filter(|object_type| object_type.name.as_str() == kind)
        .flat_map(|object_type| object_type.lifecycles.iter())
        .map(|lifecycle| {
            let property = lifecycle.property.as_str().to_owned();
            let state = record
                .values
                .get(&property)
                .and_then(norn_ir::RuntimeValue::as_str)
                .map(str::to_owned);
            (property, lifecycle.transitions.clone(), state)
        })
        .collect())
}

/// Whether `action` can act on an object in `states`: anything but a
/// `fire_transition` can; a transition can when the object's state is one it
/// fires from. A lifecycle this host does not find leaves the action listed,
/// and the call decides.
fn fires_from(action: &ActionRoot, states: &[LifecycleState]) -> bool {
    let Some(ActionEffectProgram::FireTransition {
        lifecycle,
        transition,
        ..
    }) = &action.effect_program
    else {
        return true;
    };
    let Some((_, transitions, state)) = states
        .iter()
        .find(|(property, _, _)| property.as_str() == lifecycle.as_str())
    else {
        return true;
    };
    transitions
        .iter()
        .find(|declared| declared.id.as_str() == transition.as_str())
        .is_none_or(|declared| {
            state
                .as_ref()
                .is_some_and(|state| declared.from.iter().any(|from| from == state))
        })
}

/// Every declared transition's target state, by (object type, lifecycle
/// property, transition id).
fn transition_targets<E: SqlEngine>(
    host: &DevHost<E>,
) -> Result<std::collections::BTreeMap<(String, String, String), String>, Refusal> {
    let cell = host.cell()?;
    let reader = cell.store().read();
    Ok(reader
        .model()
        .object_types()
        .flat_map(|object_type| {
            object_type.lifecycles.iter().flat_map(move |lifecycle| {
                lifecycle.transitions.iter().map(move |transition| {
                    (
                        (
                            object_type.name.as_str().to_owned(),
                            lifecycle.property.as_str().to_owned(),
                            transition.id.as_str().to_owned(),
                        ),
                        transition.to.clone(),
                    )
                })
            })
        })
        .collect())
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    reason = "registry fixture"
)]
mod tests {
    use super::*;

    fn run_create() -> ActionRoot {
        let envelope: serde_json::Value = serde_json::from_str(include_str!(
            "../../../registry/packs/tracking/artifacts/tracking.run.create.action.json"
        ))
        .unwrap();
        serde_json::from_value(envelope["payload"].clone()).unwrap()
    }

    /// ACTION-CATALOG-LINKS-1: only a target the commit reads as an existing
    /// record is a picker; a proposed target keeps its caller-supplied id.
    #[test]
    fn only_an_existing_target_is_picked() {
        let action = run_create();
        let picked = existing_targets(&action);
        assert_eq!(picked.len(), 1);
        assert_eq!(
            (picked[0].field.as_str(), picked[0].object_kind.as_str()),
            ("experiment_id", "tracking.Experiment")
        );
        let mut optional = action.clone();
        optional.slice[0].optional = true;
        assert_eq!(existing_targets(&optional), []);
        let mut proposed = action;
        proposed.slice.clear();
        assert_eq!(existing_targets(&proposed), []);
    }
}
