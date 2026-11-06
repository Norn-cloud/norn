//! Explicit diagnostic disclosure through the same scoped AccessIR/Cedar PDP.
use crate::{
    authz::{AccessRequest, Authorizer, RefusalDetailRequest},
    refusal::Refusal,
};
use norn_ir::access::{DISCLOSE_REFUSAL_DETAIL_ACTION, REFUSAL_DETAIL_RESOURCE};
use norn_kernel::authz::{self, AttrValue, Pdp, PdpDecision, ScopeKey};

/// Compose an existing server authorizer with its PDP and trusted claim mapper.
/// The mapper supplies principal/session/role/delegation claims; none are inferred.
pub struct PdpRefusalAuthorizer<A, F> {
    inner: A,
    pdp: Pdp,
    build_request: F,
}
impl<A, F> PdpRefusalAuthorizer<A, F> {
    /// Use the server's admitted PDP and principal-resolution seam.
    #[must_use]
    pub const fn new(inner: A, pdp: Pdp, build_request: F) -> Self {
        Self {
            inner,
            pdp,
            build_request,
        }
    }
}
impl<A: Authorizer, F> Authorizer for PdpRefusalAuthorizer<A, F>
where
    F: for<'a> Fn(&RefusalDetailRequest<'a>) -> Option<authz::AccessRequest> + Send + Sync,
{
    fn authorize(&self, request: &AccessRequest<'_>) -> Result<(), Refusal> {
        self.inner.authorize(request)
    }
    fn read(&self, request: &crate::authz::ReadRequest<'_>) -> Result<(), Refusal> {
        self.inner.read(request)
    }
    fn action(
        &self,
        request: &norn_kernel::ActionAuthzRequest<'_>,
    ) -> Result<(), norn_kernel::ActionDenial> {
        self.inner.action(request)
    }
    fn action_read(
        &self,
        request: &norn_kernel::ActionReadAuthzRequest<'_>,
    ) -> Result<(), norn_kernel::ActionDenial> {
        self.inner.action_read(request)
    }
    fn action_read_type(
        &self,
        request: &norn_kernel::ActionReadTypeAuthzRequest<'_>,
    ) -> Result<(), norn_kernel::ActionDenial> {
        self.inner.action_read_type(request)
    }
    fn disclose_refusal_detail(&self, request: &RefusalDetailRequest<'_>) -> bool {
        let Some(access) = (self.build_request)(request) else {
            return false;
        };
        let scope = ScopeKey {
            org: request.scope.org_id.clone(),
            environment: Some(request.scope.environment_id.clone()),
            tenant: None,
            workspace: None,
            business_unit: None,
        };
        access.principal.subject_id == request.actor
            && access.action_id == DISCLOSE_REFUSAL_DETAIL_ACTION
            && access.capability == DISCLOSE_REFUSAL_DETAIL_ACTION
            && access.resource.object_type == REFUSAL_DETAIL_RESOURCE
            && access.resource.id == request.action_id
            && access.context.get("governedAction")
                == Some(&AttrValue::String(request.action_id.into()))
            && access.scope.as_ref() == Some(&scope)
            && access.resource.scope == scope
            && matches!(self.pdp.decide(&access), PdpDecision::Allow)
    }
}

/// Project the shared refusal@2 under the explicit diagnostic decision.
/// Visibility/internal refusals remain opaque even when this permission exists.
#[must_use]
pub fn project_refusal(
    authorizer: &dyn Authorizer,
    request: &RefusalDetailRequest<'_>,
    refusal: norn_kernel::Refusal,
) -> Refusal {
    Refusal::from_kernel(refusal, authorizer.disclose_refusal_detail(request))
}

#[cfg(test)]
#[path = "refusal_disclosure_tests.rs"]
mod tests;
