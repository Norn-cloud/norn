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
    /// Admit, install or retire a retained (non-kernel) definition.
    WriteDefinitions,
    /// Start a run of the named installed flow (RUN-ROUTES-1). A run holds no
    /// grants of its own: this decides only who may start one.
    StartRun(String),
    /// Create the cell for the request's scope from a submitted definition
    /// set (HOST-PROVISION-1). Decided before the cell exists, so never by
    /// the cell's own policy.
    ProvisionCell,
    /// Start a sync of the named source connection, or sign, complete, load
    /// or read an upload (`upload`): a write of the cell's sources, decided
    /// like a definitions write.
    SyncSource(String),
    /// Set, read the status of, connect (OAuth) or revoke the named
    /// connection's credentials (CONN-AUTHZ-1). Decided by the host's policy
    /// before any body is read; credentials are never returned to anyone.
    ManageConnection(String),
    /// Revoke at the provider the grant the named connection holds
    /// (GRANT-REVOKE-1). The grant can back sibling connections, so this is
    /// cell-wide authority: a policy grants it only to a principal who
    /// manages every connection of the cell (its owners and admins), never
    /// for holding [`Operation::ManageConnection`] on the one connection.
    RevokeConnectionGrant(String),
    /// Decide a decision a flow run waits on, whose request declares these
    /// decider roles and law (DECISIONS-1). Deciding adds a condition to the
    /// run; it grants the run nothing.
    Decide {
        /// The roles the request declares; the caller must hold one.
        roles: Vec<String>,
        /// The law the request names, if any.
        law: Option<String>,
    },
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

/// Verified parent Cedar grant identities backing a newly bound automation
/// delegation (Core 05 `AUTH-ATTENUATE-1`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedAutomationGrant {
    /// Sorted, deduplicated, non-empty Cedar/AccessIR policy rule ids whose
    /// explicit `Allow` decision authorized the installing author across every
    /// type in [`AutomationGrantRequest::matched_types`].
    pub parent_grant_ids: Vec<String>,
}

/// Trusted author and target action checked when admitting or installing an
/// automation definition (Core 05 `AUTH-ATTENUATE-1`).
#[derive(Debug, Clone, Copy)]
pub struct AutomationGrantRequest<'a> {
    /// Physical cell scope.
    pub scope: &'a CellScope,
    /// Authenticated author resolved by the trusted transport.
    pub actor: &'a str,
    /// Admitted action the automation would invoke.
    pub action: &'a norn_ir::ActionRoot,
    /// Declared object type the automation watches.
    pub object_type: &'a str,
    /// Every concrete object type the automation rule can match (the declared
    /// `object_type` and every subtype extending it). The host policy must
    /// obtain an explicit PDP `Allow` for every entry.
    pub matched_types: &'a [String],
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

    /// PRINCIPAL-INPUT-1 (#1091): the declared principal inputs of a pinned
    /// decision, resolved from this request's verified principal and the
    /// PDP. Never read from anything the request carries.
    ///
    /// # Errors
    /// The default resolves nothing: the call refuses
    /// `decision.principal_input_unavailable`.
    fn principal_inputs(
        &self,
        request: &norn_kernel::PrincipalInputsRequest<'_>,
    ) -> Result<norn_kernel::ActionPrincipalInputs, norn_kernel::UnresolvedPrincipalInput> {
        Err(norn_kernel::UnresolvedPrincipalInput {
            input: request
                .inputs
                .first()
                .map_or_default(|input| input.name.clone()),
        })
    }

    /// The cell's model changed: an install committed new object types (a
    /// flow's Run type, for one). Called under the definitions write epoch,
    /// before any request sees the new model, so a policy compiled over the
    /// model's types recompiles here and decides the new types like the
    /// others. A policy that cannot recompile keeps its rules, so the new
    /// types stay denied.
    fn model_changed(&self, _object_types: &[norn_ir::ObjectTypeDef]) {}

    /// The principal this host verified for `actor` in the current request:
    /// who a call of an approval-gated action parks for, and under whose
    /// authority it commits once approved (APPROVAL-1). Never rebuilt from
    /// anything the request carries. `None`, the default, for a host without
    /// one: its gated calls are denied.
    fn verified_principal(&self, _actor: &str) -> Option<norn_cell::flow_run::RunStarter> {
        None
    }

    /// DEFS-REOPEN-1: whether this policy can serve a cell whose model has
    /// `object_types`, checked before an install writes it. A model the
    /// policy cannot serve would leave the cell unopenable after a restart.
    ///
    /// # Errors
    /// The policy does not compile for the candidate model.
    fn admits_model(&self, _object_types: &[norn_ir::ObjectTypeDef]) -> Result<(), Refusal> {
        Ok(())
    }

    /// PRIVACY-2: whether the PDP allows `actor` `norn.reveal` on the marking
    /// `marking_id` in this cell. Absent policy denies; a document then shows
    /// the marking's projection and never offers a reveal.
    fn reveal(&self, _actor: &str, _marking_id: &str) -> bool {
        false
    }

    /// DIAG-DISCLOSE-1: only an explicit scoped PDP grant discloses a diagnostic.
    /// Action/read grants do not imply this decision; absent policy denies.
    fn disclose_refusal_detail(&self, _request: &RefusalDetailRequest<'_>) -> bool {
        false
    }

    /// Core 05 `AUTH-ATTENUATE-1`: whether `request.actor` holds an explicit
    /// scoped PDP grant for `request.action` on every concrete type in
    /// `request.matched_types`, permitting them to bind an attenuated,
    /// revocable automation delegation for it. Defaults to deny; never borrowed
    /// from a later triggering actor and never satisfied by a non-PDP fallback.
    ///
    /// # Errors
    /// Denies unless the host policy explicitly verifies the author's grant
    /// through the PDP and returns non-empty parent grant identities.
    fn authorize_automation_grant(
        &self,
        _request: &AutomationGrantRequest<'_>,
    ) -> Result<VerifiedAutomationGrant, Refusal> {
        Err(read_denied())
    }

    /// RECEIPT-CONSUME-1 (T2-RES-1 step 8): recheck whether a Function
    /// command's authority may still apply a retained receipt or consume its
    /// pending `norn.function/v1` delivery at `request.now`.
    ///
    /// Defaults to deny: production hosts fail closed until a per-function
    /// service-authority / Ed25519 executor key policy explicitly admits the
    /// command's authority.
    ///
    /// # Errors
    /// Denies with `authz.denied` unless the trusted host policy explicitly
    /// authorizes `request`.
    fn result(
        &self,
        _request: &norn_kernel::result_reconciliation::ResultAuthzRequest<'_>,
    ) -> Result<(), norn_kernel::result_reconciliation::ResultDenial> {
        Err(norn_kernel::result_reconciliation::ResultDenial {
            reason: codes::AUTHZ_DENIED.into(),
        })
    }

    /// Decide before any target is loaded.
    ///
    /// # Errors
    /// A denied request returns the original stable refusal.
    fn authorize(&self, request: &AccessRequest<'_>) -> Result<(), Refusal>;
}

/// The one opaque read denial: the same document whatever was denied, so a
/// refusal never tells an ungranted resource from an absent one.
#[must_use]
pub fn read_denied() -> Refusal {
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
    fn resolve_principal_inputs(
        &self,
        request: &norn_kernel::PrincipalInputsRequest<'_>,
    ) -> Result<norn_kernel::ActionPrincipalInputs, norn_kernel::UnresolvedPrincipalInput> {
        self.0.principal_inputs(request)
    }
}

fn enforce_result_scope_and_identity(
    scope_key: &str,
    request: &norn_kernel::result_reconciliation::ResultAuthzRequest<'_>,
) -> Result<(), norn_kernel::result_reconciliation::ResultDenial> {
    if request.tenant != scope_key
        || request.authority.id.trim().is_empty()
        || request.command.trim().is_empty()
    {
        return Err(norn_kernel::result_reconciliation::ResultDenial {
            reason: codes::AUTHZ_DENIED.into(),
        });
    }
    Ok(())
}

/// Production [`norn_kernel::result_reconciliation::ResultAuthorizer`]
/// installed on every [`crate::host::DevHost`] cell (`RECEIPT-CONSUME-1`).
///
/// Enforces tenant-scope confinement (`request.tenant == scope_key`) and
/// non-empty authority/command identity before delegating to the host's
/// [`Authorizer::result`], which fails closed by default. Follow-up per-function
/// Ed25519 key distribution and revocation checks plug in either by implementing
/// [`Authorizer::result`] or by overriding via
/// [`crate::host::DevHost::with_result_authorizer`] /
/// [`crate::host::DevHost::set_result_authorizer`].
pub struct HostResultAuthorizer {
    scope_key: String,
    authorizer: std::sync::Arc<dyn Authorizer>,
}

impl HostResultAuthorizer {
    /// Bind `authorizer` to the cell's physical `scope_key`.
    #[must_use]
    pub fn new(scope_key: impl Into<String>, authorizer: std::sync::Arc<dyn Authorizer>) -> Self {
        Self {
            scope_key: scope_key.into(),
            authorizer,
        }
    }

    pub(crate) fn boxed(
        scope_key: impl Into<String>,
        authorizer: std::sync::Arc<dyn Authorizer>,
    ) -> norn_cell::ResultAuthority {
        Box::new(Self::new(scope_key, authorizer))
    }
}

impl norn_kernel::result_reconciliation::ResultAuthorizer for HostResultAuthorizer {
    fn authorize(
        &self,
        request: &norn_kernel::result_reconciliation::ResultAuthzRequest<'_>,
    ) -> Result<(), norn_kernel::result_reconciliation::ResultDenial> {
        enforce_result_scope_and_identity(&self.scope_key, request)?;
        self.authorizer.result(request)
    }
}

/// Scope- and identity-enforcing wrapper around any custom
/// [`norn_cell::ResultAuthority`] installed on a [`crate::host::DevHost`].
struct ScopedResultAuthorizer {
    scope_key: String,
    inner: norn_cell::ResultAuthority,
}

impl ScopedResultAuthorizer {
    fn boxed(
        scope_key: impl Into<String>,
        inner: norn_cell::ResultAuthority,
    ) -> norn_cell::ResultAuthority {
        Box::new(Self {
            scope_key: scope_key.into(),
            inner,
        })
    }
}

impl norn_kernel::result_reconciliation::ResultAuthorizer for ScopedResultAuthorizer {
    fn authorize(
        &self,
        request: &norn_kernel::result_reconciliation::ResultAuthzRequest<'_>,
    ) -> Result<(), norn_kernel::result_reconciliation::ResultDenial> {
        enforce_result_scope_and_identity(&self.scope_key, request)?;
        self.inner.authorize(request)
    }
}

pub(crate) fn bind_result_authz<E: norn_cell::engine::SqlEngine>(
    cell: &mut crate::host::ActionCell<E>,
    scope: &CellScope,
    authorizer: &std::sync::Arc<dyn Authorizer>,
) {
    cell.set_result_authorizer(Some(HostResultAuthorizer::boxed(
        scope.key(),
        std::sync::Arc::clone(authorizer),
    )));
}

impl<E: norn_cell::engine::SqlEngine> crate::host::DevHost<E> {
    pub(crate) fn rebind_authorizer(&mut self, authorizer: std::sync::Arc<dyn Authorizer>) {
        if let Ok(mut slot) = self.cell.lock()
            && let Some(cell) = slot.as_mut()
        {
            bind_result_authz(cell, &self.scope, &authorizer);
        }
        self.authorizer = authorizer;
    }

    /// RECEIPT-CONSUME-1: install a custom Function command-authority recheck
    /// on this host's underlying cell, always wrapped with tenant-scope and
    /// non-blank authority/command identity enforcement. Call after
    /// [`Self::with_authorizer`] when supplying both.
    #[must_use]
    pub fn with_result_authorizer(self, authorizer: norn_cell::ResultAuthority) -> Self {
        if let Ok(mut slot) = self.cell.lock()
            && let Some(cell) = slot.as_mut()
        {
            cell.set_result_authorizer(Some(ScopedResultAuthorizer::boxed(
                self.scope.key(),
                authorizer,
            )));
        }
        self
    }

    /// RECEIPT-CONSUME-1: replace the underlying cell's Function
    /// command-authority recheck in place, always wrapped with tenant-scope and
    /// non-blank authority/command identity enforcement.
    ///
    /// # Errors
    /// `host.error` if the cell lock is poisoned or the cell is unavailable.
    pub fn set_result_authorizer(
        &self,
        authorizer: norn_cell::ResultAuthority,
    ) -> Result<(), Refusal> {
        self.cell()?
            .set_result_authorizer(Some(ScopedResultAuthorizer::boxed(
                self.scope.key(),
                authorizer,
            )));
        Ok(())
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
                Operation::WriteDefinitions,
                Operation::StartRun("demo.flow".into()),
                Operation::ProvisionCell,
                Operation::Decide {
                    roles: vec!["viewer".into()],
                    law: None,
                },
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
        let matched_types = vec!["demo.Order".to_owned()];
        let grant_refusal = ReadMetadata
            .authorize_automation_grant(&AutomationGrantRequest {
                scope: &cell_scope,
                actor: "viewer",
                action: &action,
                object_type: "demo.Order",
                matched_types: &matched_types,
            })
            .expect_err("automation author grant must deny by default");
        assert_eq!(grant_refusal.code, codes::AUTHZ_DENIED);
    }

    #[test]
    fn host_result_authorizer_fails_closed_by_default_and_enforces_scope_and_identity() {
        use norn_ir::function::{Authority, AuthorityKind, Digest};
        use norn_kernel::LogicalTime;
        use norn_kernel::result_reconciliation::{
            ResultAuthorizer as _, ResultAuthzRequest, ResultDenial,
        };
        use std::sync::Arc;

        struct AllowWorker;
        impl Authorizer for AllowWorker {
            fn authorize(&self, _request: &AccessRequest<'_>) -> Result<(), Refusal> {
                Ok(())
            }

            fn result(&self, request: &ResultAuthzRequest<'_>) -> Result<(), ResultDenial> {
                if request.authority.id == "svc.worker" {
                    Ok(())
                } else {
                    Err(ResultDenial {
                        reason: codes::AUTHZ_DENIED.into(),
                    })
                }
            }
        }

        let manifest = Digest(format!("sha256:{}", "a".repeat(64)));
        let now = LogicalTime::from_millis(1_000);
        let worker = Authority {
            kind: AuthorityKind::Service,
            id: "svc.worker".to_owned(),
        };
        let other = Authority {
            kind: AuthorityKind::Service,
            id: "svc.other".to_owned(),
        };
        let blank = Authority {
            kind: AuthorityKind::Service,
            id: "   ".to_owned(),
        };

        let deny_all = HostResultAuthorizer::new("org_demo/dev", Arc::new(DenyAll));
        assert_eq!(
            deny_all.authorize(&ResultAuthzRequest {
                tenant: "org_demo/dev",
                authority: &worker,
                command: "cmd-1",
                manifest: &manifest,
                destination: None,
                now,
            }),
            Err(ResultDenial {
                reason: codes::AUTHZ_DENIED.into(),
            })
        );

        let scoped = HostResultAuthorizer::new("org_demo/dev", Arc::new(AllowWorker));
        assert!(
            scoped
                .authorize(&ResultAuthzRequest {
                    tenant: "org_demo/dev",
                    authority: &worker,
                    command: "cmd-1",
                    manifest: &manifest,
                    destination: None,
                    now,
                })
                .is_ok()
        );
        for (tenant, authority, command) in [
            ("org_other/dev", &worker, "cmd-1"),
            ("org_demo/dev", &other, "cmd-1"),
            ("org_demo/dev", &blank, "cmd-1"),
            ("org_demo/dev", &worker, "   "),
        ] {
            assert_eq!(
                scoped.authorize(&ResultAuthzRequest {
                    tenant,
                    authority,
                    command,
                    manifest: &manifest,
                    destination: None,
                    now,
                }),
                Err(ResultDenial {
                    reason: codes::AUTHZ_DENIED.into(),
                })
            );
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn dev_host_wires_fail_closed_result_authorizer_and_supports_override() {
        use norn_cell::{CellStore, Model};
        use norn_ir::RuntimeValue;
        use norn_ir::function::Digest;
        use norn_kernel::CellHost as _;
        use norn_kernel::receipt_binding::{ReceiptBindingError, ReceiptReference};
        use std::collections::BTreeMap;

        let scope = CellScope {
            org_id: "org_demo".into(),
            environment_id: "dev".into(),
        };
        let model = Model::from_definitions(&[], &[]).expect("empty model");
        let store = CellStore::open_in_memory(model).expect("in-memory store");
        let host = crate::host::DevHost::from_store(store, scope, |_command| BTreeMap::new())
            .expect("dev host");
        let reference =
            ReceiptReference::parse(&RuntimeValue::String(format!("sha256:{}", "a".repeat(64))))
                .expect("valid receipt reference");
        let manifest = Digest(format!("sha256:{}", "b".repeat(64)));

        // Default wiring installs HostResultAuthorizer, so resolve runs and
        // returns Unknown rather than Ok(None) (receipt_effect_unsupported).
        {
            let cell = host.cell().expect("cell guard");
            assert!(matches!(
                cell.resolve_accepted_receipt(&reference, &manifest),
                Err(ReceiptBindingError::Unknown)
            ));
        }

        // Custom overrides stay wrapped with ScopedResultAuthorizer so tenant
        // scope and non-blank identity checks are always enforced and the
        // authorizer is never cleared to None.
        struct AllowAllResults;
        impl norn_kernel::result_reconciliation::ResultAuthorizer for AllowAllResults {
            fn authorize(
                &self,
                _request: &norn_kernel::result_reconciliation::ResultAuthzRequest<'_>,
            ) -> Result<(), norn_kernel::result_reconciliation::ResultDenial> {
                Ok(())
            }
        }
        let scoped_custom =
            ScopedResultAuthorizer::boxed("org_demo/dev", Box::new(AllowAllResults));
        let worker = norn_ir::function::Authority {
            kind: norn_ir::function::AuthorityKind::Service,
            id: "svc.worker".to_owned(),
        };
        assert_eq!(
            scoped_custom.authorize(&norn_kernel::result_reconciliation::ResultAuthzRequest {
                tenant: "org_other/dev",
                authority: &worker,
                command: "cmd-1",
                manifest: &manifest,
                destination: None,
                now: norn_kernel::LogicalTime::from_millis(1_000),
            }),
            Err(norn_kernel::result_reconciliation::ResultDenial {
                reason: codes::AUTHZ_DENIED.into(),
            })
        );
        host.set_result_authorizer(Box::new(AllowAllResults))
            .expect("replace result authorizer");
        {
            let cell = host.cell().expect("cell guard");
            assert!(matches!(
                cell.resolve_accepted_receipt(&reference, &manifest),
                Err(ReceiptBindingError::Unknown)
            ));
        }

        // Rebinding via with_authorizer and with_result_authorizer keeps an
        // authorizer installed at all times.
        let host = host
            .with_authorizer(DenyAll)
            .with_result_authorizer(Box::new(AllowAllResults));
        {
            let cell = host.cell().expect("cell guard");
            assert!(matches!(
                cell.resolve_accepted_receipt(&reference, &manifest),
                Err(ReceiptBindingError::Unknown)
            ));
        }
    }
}
