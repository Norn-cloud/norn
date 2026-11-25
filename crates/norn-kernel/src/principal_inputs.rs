//! PRINCIPAL-INPUT-1 (#1091): a pinned decision's declared principal inputs
//! in the action commit path.
//!
//! The kernel never reads the principal from a call. When the action's
//! pinned decision declares principal inputs, the kernel asks the host
//! ([`crate::ActionAuthorizer::resolve_principal_inputs`]) for exactly those
//! inputs, checks the answer (every declared name, no other, a named
//! principal and policy version, this call's subject), records it on the
//! call record ([`ActionPrincipalInputs`]) and binds it to the decision
//! input. Anything else refuses `decision.principal_input_unavailable` with
//! nothing committed; there is no default. An exact retry replays the
//! recorded call, so the recorded values are never resolved again.

use std::collections::BTreeMap;

use norn_ir::ActionRoot;
use norn_ir::rule::PrincipalInputDeclaration;
use serde::{Deserialize, Serialize};

use crate::action_commit::{ActionAuthorizer, ActionCall, ActionRefusal};
use crate::cell_host::{FactKey, JsonObject, JsonValue};
use crate::synchronous_rule::SynchronousRule;

/// What the host is asked to resolve for one call.
#[derive(Debug, Clone, Copy)]
pub struct PrincipalInputsRequest<'a> {
    /// The action whose pinned decision declares the inputs.
    pub action: &'a ActionRoot,
    /// The call's explicit scope.
    pub scope: &'a BTreeMap<String, String>,
    /// The calling actor, as the trusted transport verified it.
    pub actor: &'a str,
    /// The decision's subject: the object the call targets. A `holdsRole`
    /// `at: resource` is decided at this object's scope.
    pub subject: &'a FactKey,
    /// The declared inputs, in declaration order.
    pub inputs: &'a [PrincipalInputDeclaration],
}

/// The principal inputs resolved for one committed call, as recorded on its
/// call record. They are the acting principal's own values for the declared
/// names only; replay reads them and never resolves again.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActionPrincipalInputs {
    /// The acting principal the values belong to. For an agent acting on
    /// someone's behalf this is the narrower delegated principal.
    pub principal: String,
    /// The policy version the values were resolved under (the host's role
    /// mapping, grants and principal attribute catalog).
    pub policy_version: String,
    /// The decision's subject.
    pub subject: FactKey,
    /// The resolved value of every declared input, by name. Preserve source
    /// text and use an exact-number JSON parser for its values.
    pub values: JsonObject,
}

/// The host could not resolve a declared input. Names the input only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnresolvedPrincipalInput {
    /// The declared input's name.
    pub input: String,
}

impl ActionPrincipalInputs {
    pub(crate) fn to_runtime_value(&self) -> JsonValue {
        let string = |value: &str| JsonValue::String(value.to_owned());
        JsonValue::Object(JsonObject::from([
            ("principal".to_owned(), string(&self.principal)),
            ("policyVersion".to_owned(), string(&self.policy_version)),
            (
                "subject".to_owned(),
                JsonValue::Object(JsonObject::from([
                    (
                        "objectType".to_owned(),
                        string(self.subject.object_type.as_str()),
                    ),
                    ("key".to_owned(), string(&self.subject.key)),
                ])),
            ),
            ("values".to_owned(), JsonValue::Object(self.values.clone())),
        ]))
    }
}

/// Resolve the pinned decision's declared inputs through the host and check
/// the answer. `None` when the decision declares none.
pub(crate) fn resolve<X>(
    rule: &SynchronousRule,
    call: &ActionCall<'_>,
    authorizer: &dyn ActionAuthorizer,
    subject: &FactKey,
) -> Result<Option<ActionPrincipalInputs>, ActionRefusal<X>> {
    let inputs = rule.principal_inputs();
    let Some(first) = inputs.first() else {
        return Ok(None);
    };
    let unavailable =
        |input: &PrincipalInputDeclaration| ActionRefusal::PrincipalInputUnavailable {
            input: input.name.clone(),
        };
    let resolved = authorizer
        .resolve_principal_inputs(&PrincipalInputsRequest {
            action: call.action,
            scope: &call.scope,
            actor: &call.actor,
            subject,
            inputs,
        })
        .map_err(|unresolved| {
            // Only a declared name is repeated: the host's text is not trusted
            // to be free of values.
            unavailable(
                inputs
                    .iter()
                    .find(|input| input.name == unresolved.input)
                    .unwrap_or(first),
            )
        })?;
    if let Some(missing) = inputs
        .iter()
        .find(|input| !resolved.values.contains_key(&input.name))
    {
        return Err(unavailable(missing));
    }
    if resolved.values.len() != inputs.len()
        || resolved.principal.trim().is_empty()
        || resolved.policy_version.trim().is_empty()
        || resolved.subject != *subject
    {
        return Err(unavailable(first));
    }
    Ok(Some(resolved))
}
