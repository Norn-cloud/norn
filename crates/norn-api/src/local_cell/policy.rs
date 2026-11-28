//! Fail-closed scope, role, object-read, and disclosure policy for [`super::LocalCell`].

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use norn_cell::engine::NativeEngine;
use norn_cell::{CellStore, ObjectRecord};
use norn_kernel::authz::{
    ActorKind, Binding, ResourceInput, ScopeKey, UnboundProfile, VerifiedPrincipal,
};
use norn_kernel::{
    ActionAuthzRequest, ActionDenial, ActionReadAuthzRequest, ActionReadTypeAuthzRequest,
};
use sha2::{Digest as _, Sha256};

use crate::authz::{
    AccessRequest, Authorizer, DenyAll, Operation, ReadCapability, ReadRequest,
    RefusalDetailRequest, read_denied,
};
use crate::object_routes::ObjectAccess;
use crate::question_routes::{DEFAULT_MIN_DISCLOSED_BUCKET, QuestionRuntime, QuestionSnapshot};
use crate::refusal::{Refusal, codes};
use crate::scope::CellScope;

use super::access_eval::{EvalStage, PolicySnapshot, kernel_scope_matches_cell};

pub(super) const LOCAL_ROLE_TABLE: &str = "norn-local-roles/1";
pub(super) const STANDARD_ROLES: [&str; 4] = ["viewer", "member", "admin", "owner"];

/// Verified caller identity and explicit roles for one local cell operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalActor {
    /// Non-blank subject identifier.
    pub user_id: String,
    /// Explicit roles conferred on this caller (empty roles fail closed).
    pub roles: Vec<String>,
}

impl LocalActor {
    /// Construct a caller with explicit `user_id` and `roles`.
    #[must_use]
    pub fn new(user_id: impl Into<String>, roles: Vec<String>) -> Self {
        Self {
            user_id: user_id.into(),
            roles,
        }
    }

    /// Construct a local owner caller used for trusted initial pack install.
    #[must_use]
    pub fn owner(user_id: impl Into<String>) -> Self {
        Self {
            user_id: user_id.into(),
            roles: vec!["owner".to_owned()],
        }
    }

    pub(super) fn is_valid(&self) -> bool {
        !self.user_id.trim().is_empty()
            && !self.roles.is_empty()
            && self.roles.iter().all(|role| !role.trim().is_empty())
    }

    pub(super) fn has_any(&self, allowed: &[&str]) -> bool {
        self.roles
            .iter()
            .any(|held| allowed.iter().any(|candidate| held == candidate))
    }

    pub(super) fn has_any_owned(&self, allowed: &[String]) -> bool {
        self.roles.iter().any(|held| allowed.contains(held))
    }
}

thread_local! {
    static ACTIVE_ACTOR: RefCell<Option<LocalActor>> = const { RefCell::new(None) };
}

struct ActorGuard(Option<LocalActor>);

impl Drop for ActorGuard {
    fn drop(&mut self) {
        let previous = self.0.take();
        ACTIVE_ACTOR.with(|slot| {
            let _replaced = slot.replace(previous);
        });
    }
}

pub(super) fn with_actor<T>(actor: Option<&LocalActor>, run: impl FnOnce() -> T) -> T {
    let next = actor.filter(|candidate| candidate.is_valid()).cloned();
    let previous = ACTIVE_ACTOR.with(|slot| slot.replace(next));
    let _guard = ActorGuard(previous);
    run()
}

pub(super) fn current_actor() -> Option<LocalActor> {
    ACTIVE_ACTOR.with(|slot| slot.borrow().clone())
}

pub(super) struct LocalPolicy {
    pub(super) scope: CellScope,
    pub(super) policies: Arc<RwLock<PolicySnapshot>>,
}

impl LocalPolicy {
    fn active_matching(expected_actor: Option<&str>) -> Option<LocalActor> {
        let active = current_actor()?;
        let expected = expected_actor?;
        (active.is_valid() && active.user_id == expected).then_some(active)
    }

    fn authorize_action(
        &self,
        actor: &LocalActor,
        action_id: &str,
        stage: &EvalStage<'_>,
    ) -> Result<(), String> {
        let Ok(snapshot) = self.policies.read() else {
            return Err(format!(
                "{}: local policy lock poisoned",
                codes::AUTHZ_DENIED
            ));
        };
        snapshot.authorize_action(actor, action_id, stage)
    }
}

impl Authorizer for LocalPolicy {
    fn authorize(&self, request: &AccessRequest<'_>) -> Result<(), Refusal> {
        if request.scope != &self.scope {
            return DenyAll.authorize(request);
        }
        let Some(actor) = Self::active_matching(request.actor) else {
            return DenyAll.authorize(request);
        };
        let allowed = match &request.operation {
            Operation::ReadDefinitions => actor.has_any(&STANDARD_ROLES),
            Operation::WriteDefinitions
            | Operation::SyncSource(_)
            | Operation::ManageConnection(_)
            | Operation::RevokeConnectionGrant(_) => actor.has_any(&["admin", "owner"]),
            Operation::ProvisionCell => actor.has_any(&["owner"]),
            Operation::CallAction(action_id) => self
                .authorize_action(
                    &actor,
                    action_id,
                    &EvalStage::PreStorage {
                        scope: request.scope,
                    },
                )
                .is_ok(),
            Operation::StartRun(_) => actor.has_any(&["member", "admin", "owner"]),
            Operation::Decide { roles, .. } => {
                actor.has_any(&STANDARD_ROLES) && actor.has_any_owned(roles)
            }
        };
        if allowed {
            Ok(())
        } else {
            DenyAll.authorize(request)
        }
    }

    fn read(&self, request: &ReadRequest<'_>) -> Result<(), Refusal> {
        let Some(actor) = Self::active_matching(Some(&request.access.principal().subject_id))
        else {
            return Err(read_denied());
        };
        let allowed = match request.capability {
            ReadCapability::Population => actor.has_any(&STANDARD_ROLES),
            ReadCapability::Values => actor.has_any(&["member", "admin", "owner"]),
            ReadCapability::Sources => actor.has_any(&["admin", "owner"]),
        };
        if allowed { Ok(()) } else { Err(read_denied()) }
    }

    fn action(&self, request: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        let Some(actor) = Self::active_matching(Some(request.actor)) else {
            return Err(ActionDenial::Forbidden {
                reason: codes::AUTHZ_DENIED.to_owned(),
            });
        };
        let stage = EvalStage::Kernel {
            cell_scope: &self.scope,
            request_scope: request.scope,
            target_type: request.target.object_type.as_str(),
            target_values: request.target_values,
        };
        self.authorize_action(&actor, &request.action.action_id, &stage)
            .map_err(|reason| ActionDenial::Forbidden { reason })
    }

    fn action_read(&self, request: &ActionReadAuthzRequest<'_>) -> Result<(), ActionDenial> {
        let Some(actor) = Self::active_matching(Some(request.actor)) else {
            return Err(ActionDenial::Invisible);
        };
        if kernel_scope_matches_cell(&self.scope, request.scope).is_err() {
            return Err(ActionDenial::Invisible);
        }
        let pre_storage = EvalStage::PreStorage { scope: &self.scope };
        if actor.has_any(&["member", "admin", "owner"])
            || self
                .authorize_action(&actor, &request.action.action_id, &pre_storage)
                .is_ok()
        {
            Ok(())
        } else {
            Err(ActionDenial::Invisible)
        }
    }

    fn action_read_type(
        &self,
        request: &ActionReadTypeAuthzRequest<'_>,
    ) -> Result<(), ActionDenial> {
        let Some(actor) = Self::active_matching(Some(request.actor)) else {
            return Err(ActionDenial::Invisible);
        };
        if kernel_scope_matches_cell(&self.scope, request.scope).is_err() {
            return Err(ActionDenial::Invisible);
        }
        let pre_storage = EvalStage::PreStorage { scope: &self.scope };
        if actor.has_any(&["member", "admin", "owner"])
            || self
                .authorize_action(&actor, &request.action.action_id, &pre_storage)
                .is_ok()
        {
            Ok(())
        } else {
            Err(ActionDenial::Invisible)
        }
    }

    fn verified_principal(&self, actor: &str) -> Option<norn_cell::flow_run::RunStarter> {
        let active = Self::active_matching(Some(actor))?;
        Some(norn_cell::flow_run::RunStarter {
            subject: active.user_id.clone(),
            actor_kind: "user".to_owned(),
            roles: active.roles,
            role_table: LOCAL_ROLE_TABLE.to_owned(),
            accountable_person: Some(active.user_id),
        })
    }

    fn disclose_refusal_detail(&self, request: &RefusalDetailRequest<'_>) -> bool {
        Self::active_matching(Some(request.actor))
            .is_some_and(|actor| actor.has_any(&["admin", "owner"]))
    }
}

const fn read_capability_name(capability: ReadCapability) -> &'static str {
    match capability {
        ReadCapability::Population => "norn.read.population",
        ReadCapability::Values => "norn.read.values",
        ReadCapability::Sources => "norn.read.sources",
    }
}

fn build_read_access(
    scope: &CellScope,
    actor: &str,
    capability: ReadCapability,
    object_type: &str,
    row: Option<&ObjectRecord>,
) -> norn_kernel::authz::AccessRequest {
    let key = ScopeKey {
        org: scope.org_id.clone(),
        environment: Some(scope.environment_id.clone()),
        tenant: None,
        workspace: None,
        business_unit: None,
    };
    let (res_type, res_id) = row.map_or_else(
        || (object_type.to_owned(), object_type.to_owned()),
        |record| (record.object_type.clone(), record.id.clone()),
    );
    norn_kernel::authz::AccessRequest::new(
        &VerifiedPrincipal::new(actor, ActorKind::User, None, Vec::new(), BTreeMap::new()),
        Some(key.clone()),
        read_capability_name(capability),
        "norn.read",
        ResourceInput {
            object_type: res_type,
            id: res_id,
            scope: key,
            attributes: BTreeMap::new(),
        },
        BTreeMap::new(),
        Binding::Unbound(UnboundProfile::HostedRead),
    )
}

pub(super) struct LocalObjectAccess;

impl ObjectAccess for LocalObjectAccess {
    fn access(
        &self,
        scope: &CellScope,
        actor: &str,
        capability: ReadCapability,
        object_type: &str,
        row: Option<&ObjectRecord>,
    ) -> Result<norn_kernel::authz::AccessRequest, Refusal> {
        Ok(build_read_access(
            scope,
            actor,
            capability,
            object_type,
            row,
        ))
    }
}

pub(super) struct LocalDisclosure {
    pub(super) handle_key: [u8; 32],
}

impl LocalDisclosure {
    pub(super) fn for_scope(scope: &CellScope) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(b"norn-local-handle-key/1\0");
        hasher.update(scope.org_id.as_bytes());
        hasher.update(b"\0");
        hasher.update(scope.environment_id.as_bytes());
        let digest = hasher.finalize();
        let mut handle_key = [0_u8; 32];
        handle_key.copy_from_slice(&digest);
        Self { handle_key }
    }
}

impl QuestionRuntime<NativeEngine> for LocalDisclosure {
    fn plan(
        &self,
        _question: &str,
    ) -> Result<norn_checker::metric::compute::QuestionPlan, Refusal> {
        Err(read_denied())
    }

    fn snapshot(
        &self,
        _store: &CellStore<NativeEngine>,
        _plan: &norn_checker::metric::compute::QuestionPlan,
        _actor: &str,
    ) -> Result<QuestionSnapshot, Refusal> {
        Err(read_denied())
    }

    fn access(
        &self,
        _scope: &CellScope,
        _actor: &str,
        _question: &str,
        _capability: ReadCapability,
        _row: Option<&ObjectRecord>,
    ) -> Result<norn_kernel::authz::AccessRequest, Refusal> {
        Err(read_denied())
    }

    fn min_disclosed_bucket(&self) -> usize {
        DEFAULT_MIN_DISCLOSED_BUCKET
    }

    fn handle_key(&self) -> &[u8] {
        &self.handle_key
    }

    fn collection_access(
        &self,
        scope: &CellScope,
        actor: &str,
        capability: ReadCapability,
        object_type: &str,
    ) -> Result<norn_kernel::authz::AccessRequest, Refusal> {
        Ok(build_read_access(
            scope,
            actor,
            capability,
            object_type,
            None,
        ))
    }
}
