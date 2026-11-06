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
    /// Start a run of the named installed flow (RUN-ROUTES-1).
    StartRun(String),
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

/// Independent disclosure decisions; none implies either other capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadCapability {
    /// Membership in an authorized numeric or object population.
    Population,
    /// All canonical property values of a member.
    Values,
    /// Authenticated source row/document provenance.
    Sources,
}

/// A PDP request assembled by trusted server identity/resource mapping.
pub struct ReadRequest<'a> {
    /// The disclosure being decided.
    pub capability: ReadCapability,
    /// Existing kernel PDP input, never client claims.
    pub access: &'a norn_kernel::authz::AccessRequest,
}

/// Trusted principal and admitted action whose complete diagnostic is requested.
pub struct RefusalDetailRequest<'a> {
    /// Physical cell scope.
    pub scope: &'a CellScope,
    /// Actor resolved by the trusted transport.
    pub actor: &'a str,
    /// Admitted action identity; never an operand interpreted as a resource.
    pub action_id: &'a str,
}

/// HOST-AUTH-1: trusted server policy; every kernel callback denies by default.
pub trait Authorizer: Send + Sync {
    /// HOST-READ-1: metadata/action authority supplies no data disclosure grant.
    /// # Errors
    /// Denies unless a trusted policy explicitly supplies this capability.
    fn read(&self, _request: &ReadRequest<'_>) -> Result<(), Refusal> {
        Err(read_denied())
    }
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

    /// DIAG-DISCLOSE-1: only an explicit scoped PDP grant discloses a diagnostic.
    /// Action/read grants do not imply this decision; absent policy denies.
    fn disclose_refusal_detail(&self, _request: &RefusalDetailRequest<'_>) -> bool {
        false
    }

    /// Decide before any target is loaded.
    ///
    /// # Errors
    /// A denied request returns the original stable refusal.
    fn authorize(&self, request: &AccessRequest<'_>) -> Result<(), Refusal>;
}

pub(crate) fn read_denied() -> Refusal {
    Refusal::new(
        http::StatusCode::FORBIDDEN,
        codes::AUTHZ_DENIED,
        "the read request is not authorized",
        "access to the requested read is denied",
    )
}

/// The server policy adapted to the kernel action boundary.
pub struct KernelAuthorizer<'a>(
    /// The same trusted policy used for host operations.
    pub &'a dyn Authorizer,
);

impl norn_kernel::ActionAuthorizer for KernelAuthorizer<'_> {
    fn authorize(
        &self,
        request: &norn_kernel::ActionAuthzRequest<'_>,
    ) -> Result<(), norn_kernel::ActionDenial> {
        self.0.action(request)
    }
    fn authorize_read(
        &self,
        request: &norn_kernel::ActionReadAuthzRequest<'_>,
    ) -> Result<(), norn_kernel::ActionDenial> {
        self.0.action_read(request)
    }
    fn authorize_read_type(
        &self,
        request: &norn_kernel::ActionReadTypeAuthzRequest<'_>,
    ) -> Result<(), norn_kernel::ActionDenial> {
        self.0.action_read_type(request)
    }
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
#[allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "authorization contract fixtures"
)]
mod tests {
    use super::*;
    #[test]
    fn absent_host_policy_denies_authenticated_and_anonymous_operations() {
        let scope = CellScope {
            org_id: "org_demo".into(),
            environment_id: "dev".into(),
        };
        for actor in [None, Some("demo.viewer")] {
            for operation in [
                Operation::ReadDefinitions,
                Operation::CallAction("demo.action".into()),
            ] {
                let refusal = DenyAll
                    .authorize(&AccessRequest {
                        scope: &scope,
                        actor,
                        operation,
                    })
                    .expect_err("explicit policy required");
                assert_eq!(refusal.code, codes::AUTHZ_DENIED);
            }
        }
    }
    #[test]
    fn host_read_grant_does_not_supply_kernel_action_or_disclosure_grants() {
        struct ReadMetadata;
        impl Authorizer for ReadMetadata {
            fn authorize(&self, _request: &AccessRequest<'_>) -> Result<(), Refusal> {
                Ok(())
            }
        }
        let cell_scope = CellScope {
            org_id: "org_demo".into(),
            environment_id: "dev".into(),
        };
        assert!(
            ReadMetadata
                .authorize(&AccessRequest {
                    scope: &cell_scope,
                    actor: Some("viewer"),
                    operation: Operation::ReadDefinitions
                })
                .is_ok()
        );
        let kernel = KernelAuthorizer(&ReadMetadata);
        let envelope: serde_json::Value = serde_json::from_str(include_str!(
            "../../../fixtures/ir/commerce-sales-order-submit-invoice.action.json"
        ))
        .expect("fixture");
        let action = serde_json::from_value(envelope["payload"].clone()).expect("action");
        let scope = std::collections::BTreeMap::new();
        let input = std::collections::BTreeMap::new();
        let target = norn_kernel::FactKey::new("demo.Order", "one");
        let request = norn_kernel::ActionAuthzRequest {
            action: &action,
            scope: &scope,
            actor: "viewer",
            input: &input,
            target: &target,
            target_values: None,
        };
        assert_eq!(
            ReadMetadata.action(&request),
            Err(norn_kernel::ActionDenial::Forbidden {
                reason: "authz.denied".into()
            })
        );
        assert_eq!(
            norn_kernel::ActionAuthorizer::authorize(&kernel, &request),
            ReadMetadata.action(&request)
        );
        let read = norn_kernel::ActionReadAuthzRequest {
            action: &action,
            scope: &scope,
            actor: "viewer",
            input: &input,
            declared_type: &target.object_type,
            target: &target,
            target_values: None,
        };
        assert_eq!(
            ReadMetadata.action_read(&read),
            Err(norn_kernel::ActionDenial::Invisible)
        );
        assert_eq!(
            norn_kernel::ActionAuthorizer::authorize_read(&kernel, &read),
            ReadMetadata.action_read(&read)
        );
        let read_type = norn_kernel::ActionReadTypeAuthzRequest {
            action: &action,
            scope: &scope,
            actor: "viewer",
            input: &input,
            object_type: &target.object_type,
        };
        assert_eq!(
            ReadMetadata.action_read_type(&read_type),
            Err(norn_kernel::ActionDenial::Invisible)
        );
        assert_eq!(
            norn_kernel::ActionAuthorizer::authorize_read_type(&kernel, &read_type),
            ReadMetadata.action_read_type(&read_type)
        );
    }
}
