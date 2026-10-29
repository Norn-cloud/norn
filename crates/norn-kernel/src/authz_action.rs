//! Adapter from the Cedar PDP to the kernel's action-call authorization port.
//!
//! The shell supplies a trusted request builder because an
//! [`ActionAuthzRequest`] does not carry all authorization claims. In
//! particular, the shell must supply actor kind, session, roles, capability,
//! delegation, typed principal/resource attributes, and context. The shell is
//! also responsible for deciding whether the actor may see the target; it can
//! return [`ActionDenial::Invisible`] from the builder for that case. This
//! module does not infer identity, visibility, or grants and does not provide
//! defaults.

use norn_authz::{AccessRequest, DenyReason, Pdp, PdpDecision, ScopeKey};

use crate::{ActionAuthorizer, ActionAuthzRequest, ActionDenial};

/// An action authorizer backed by one admitted Cedar PDP and trusted shell
/// request mapping.
pub struct PdpActionAuthorizer<F> {
    pdp: Pdp,
    build_request: F,
}

impl<F> PdpActionAuthorizer<F>
where
    F: for<'a> Fn(&ActionAuthzRequest<'a>) -> Result<AccessRequest, ActionDenial>,
{
    /// Build an action authorizer from the current PDP and a trusted shell
    /// mapper. The mapper must not infer or default missing authorization
    /// claims. It should return [`ActionDenial::Invisible`] when its visibility
    /// check says the target may not be seen.
    #[must_use]
    pub const fn new(pdp: Pdp, build_request: F) -> Self {
        Self { pdp, build_request }
    }
}

impl<F> ActionAuthorizer for PdpActionAuthorizer<F>
where
    F: for<'a> Fn(&ActionAuthzRequest<'a>) -> Result<AccessRequest, ActionDenial>,
{
    fn authorize(&self, action_request: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        let access_request = (self.build_request)(action_request)?;
        if !matches_action_request(action_request, &access_request) {
            return Err(ActionDenial::Forbidden {
                reason: DenyReason::InvalidRequest.code().to_owned(),
            });
        }

        match self.pdp.decide(&access_request) {
            PdpDecision::Allow => Ok(()),
            PdpDecision::Deny(reason) => Err(ActionDenial::Forbidden {
                reason: reason.code().to_owned(),
            }),
        }
    }
}

/// Ensure a trusted mapper has not changed the kernel-bound action, actor,
/// target, or scope while adding the remaining trusted PDP claims.
fn matches_action_request(
    action_request: &ActionAuthzRequest<'_>,
    access_request: &AccessRequest,
) -> bool {
    let Some(scope) = scope_from_action_request(action_request.scope) else {
        return false;
    };

    access_request.principal.subject_id == action_request.actor
        && access_request.action_id == action_request.action.action_id
        && access_request.resource.object_type == action_request.target.object_type.as_str()
        && access_request.resource.id == action_request.target.key
        && access_request.scope.as_ref() == Some(&scope)
}

/// Convert the action boundary's named scope dimensions exactly. Unknown
/// dimensions fail closed instead of being silently dropped.
fn scope_from_action_request(
    scope: &std::collections::BTreeMap<String, String>,
) -> Option<ScopeKey> {
    if scope.keys().any(|key| {
        !matches!(
            key.as_str(),
            "orgId" | "tenantId" | "workspaceId" | "environmentId" | "businessUnitId"
        )
    }) {
        return None;
    }

    Some(ScopeKey {
        org: scope.get("orgId")?.clone(),
        tenant: scope.get("tenantId").cloned(),
        workspace: scope.get("workspaceId").cloned(),
        environment: scope.get("environmentId").cloned(),
        business_unit: scope.get("businessUnitId").cloned(),
    })
}
