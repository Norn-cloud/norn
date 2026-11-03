//! Host authorization; absent configuration always denies.

use crate::{
    refusal::{Refusal, codes},
    scope::CellScope,
};

/// Host operation subject to trusted server authorization.
pub enum Operation {
    /// Read the admitted definitions.
    ReadDefinitions,
    /// Invoke the named admitted action.
    CallAction(String),
}

/// A scoped host request. The actor is supplied by the trusted transport.
pub struct AccessRequest<'a> {
    /// Already admitted cell scope.
    pub scope: &'a CellScope,
    /// Authenticated principal, if available.
    pub actor: Option<&'a str>,
    /// Requested operation.
    pub operation: Operation,
}

/// HOST-AUTH-1: trusted server policy; every kernel callback denies by default.
pub trait Authorizer: Send + Sync {
    /// Current and proposed state authorization, delegated by the kernel.
    ///
    /// # Errors
    /// A denied call returns the server policy denial.
    fn action(
        &self,
        _request: &norn_kernel::ActionAuthzRequest<'_>,
    ) -> Result<(), norn_kernel::ActionDenial> {
        Err(norn_kernel::ActionDenial::Forbidden {
            reason: codes::AUTHZ_DENIED.into(),
        })
    }
    /// Object disclosure for declared action reads.
    ///
    /// # Errors
    /// An invisible object refuses the action read.
    fn action_read(
        &self,
        _request: &norn_kernel::ActionReadAuthzRequest<'_>,
    ) -> Result<(), norn_kernel::ActionDenial> {
        Err(norn_kernel::ActionDenial::Invisible)
    }
    /// Collection disclosure before loading declared reads.
    ///
    /// # Errors
    /// An inaccessible collection refuses before loading rows.
    fn action_read_type(
        &self,
        _request: &norn_kernel::ActionReadTypeAuthzRequest<'_>,
    ) -> Result<(), norn_kernel::ActionDenial> {
        Err(norn_kernel::ActionDenial::Invisible)
    }

    /// Decide before any target is loaded.
    ///
    /// # Errors
    /// A denied request returns the original stable refusal.
    fn authorize(&self, request: &AccessRequest<'_>) -> Result<(), Refusal>;
}

/// Default policy: every request refuses.
pub struct DenyAll;

impl Authorizer for DenyAll {
    fn authorize(&self, _request: &AccessRequest<'_>) -> Result<(), Refusal> {
        Err(Refusal::new(
            http::StatusCode::FORBIDDEN,
            codes::AUTHZ_DENIED,
            "the request is not authorized",
            "the host denies access unless an explicit authorizer admits it",
        ))
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::indexing_slicing, reason = "authorization contract fixtures")]
mod tests {
    use super::*;
    #[test]
    fn absent_host_policy_denies_authenticated_and_anonymous_operations() {
        let scope = CellScope { org_id: "org_demo".into(), environment_id: "dev".into() };
        for actor in [None, Some("demo.viewer")] {
            for operation in [Operation::ReadDefinitions, Operation::CallAction("demo.action".into())] {
                let refusal = DenyAll.authorize(&AccessRequest { scope: &scope, actor, operation }).expect_err("explicit policy required");
                assert_eq!(refusal.code, codes::AUTHZ_DENIED);
            }
        }
    }
    #[test]
    fn host_read_grant_does_not_supply_kernel_action_or_disclosure_grants() {
        struct ReadMetadata;
        impl Authorizer for ReadMetadata {
            fn authorize(&self, _request: &AccessRequest<'_>) -> Result<(), Refusal> { Ok(()) }
        }
        let envelope: serde_json::Value = serde_json::from_str(include_str!("../../../fixtures/ir/commerce-sales-order-submit-invoice.action.json")).expect("fixture");
        let action = serde_json::from_value(envelope["payload"].clone()).expect("action");
        let scope = std::collections::BTreeMap::new();
        let input = std::collections::BTreeMap::new();
        let target = norn_kernel::FactKey::new("demo.Order", "one");
        let request = norn_kernel::ActionAuthzRequest { action: &action, scope: &scope, actor: "viewer", input: &input, target: &target, target_values: None };
        assert_eq!(ReadMetadata.action(&request), Err(norn_kernel::ActionDenial::Forbidden { reason: "authz.denied".into() }));
        let read = norn_kernel::ActionReadAuthzRequest { action: &action, scope: &scope, actor: "viewer", input: &input, declared_type: &target.object_type, target: &target, target_values: None };
        assert_eq!(ReadMetadata.action_read(&read), Err(norn_kernel::ActionDenial::Invisible));
        let read_type = norn_kernel::ActionReadTypeAuthzRequest { action: &action, scope: &scope, actor: "viewer", input: &input, object_type: &target.object_type };
        assert_eq!(ReadMetadata.action_read_type(&read_type), Err(norn_kernel::ActionDenial::Invisible));
    }
}
