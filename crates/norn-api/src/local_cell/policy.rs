//! Fail-closed scope, role, object-read, and disclosure policy for [`super::LocalCell`].

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};

use norn_cell::engine::NativeEngine;
use norn_cell::{CellStore, ObjectRecord};
use norn_ir::access::{AccessCondition, AccessRule, PolicyRoot};
use norn_ir::{ActionRoot, ArtifactKind};
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
use crate::kernel_set;
use crate::object_routes::ObjectAccess;
use crate::question_routes::{DEFAULT_MIN_DISCLOSED_BUCKET, QuestionRuntime, QuestionSnapshot};
use crate::refusal::{Refusal, codes};
use crate::scope::CellScope;

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

    fn has_any(&self, allowed: &[&str]) -> bool {
        self.roles
            .iter()
            .any(|held| allowed.iter().any(|candidate| held == candidate))
    }

    fn has_any_owned(&self, allowed: &[String]) -> bool {
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
            *slot.borrow_mut() = previous;
        });
    }
}

pub(super) fn with_actor<T>(actor: Option<&LocalActor>, run: impl FnOnce() -> T) -> T {
    let next = actor.filter(|candidate| candidate.is_valid()).cloned();
    let previous = ACTIVE_ACTOR.with(|slot| slot.borrow_mut().replace(next));
    let _guard = ActorGuard(previous);
    run()
}

fn current_actor() -> Option<LocalActor> {
    ACTIVE_ACTOR.with(|slot| slot.borrow().clone())
}

/// Role conditions extracted from active `AccessIR` policies for governed actions.
#[derive(Debug, Default)]
pub(super) struct PolicySnapshot {
    /// For each action that declares an `access` policy, the list of rules' role
    /// allowlists (`Some(roles)` for a `RoleCondition`, `None` for a rule with no
    /// role conjunct). An empty outer `Vec` means the action's access policy was
    /// missing or had no matching rule (fail closed).
    governed: BTreeMap<String, Vec<Option<Vec<String>>>>,
}

impl PolicySnapshot {
    pub(super) fn from_canonical_set(canonical_set: &str) -> Result<Self, Refusal> {
        let artifacts = kernel_set::retained_artifacts(canonical_set).map_err(Refusal::host)?;
        let mut policies: BTreeMap<&str, Vec<AccessRule>> = BTreeMap::new();
        for artifact in &artifacts {
            if artifact.kind() != ArtifactKind::Policy {
                continue;
            }
            let decoded = serde_json::to_value(artifact.payload.to_json_value())
                .and_then(serde_json::from_value::<PolicyRoot>);
            if let Ok(PolicyRoot::Access(access)) = decoded {
                policies.insert(artifact.artifact_id.as_str(), access.rules);
            }
        }
        let mut governed = BTreeMap::new();
        for artifact in &artifacts {
            if artifact.kind() != ArtifactKind::Action {
                continue;
            }
            let Ok(action) = serde_json::to_value(artifact.payload.to_json_value())
                .and_then(serde_json::from_value::<ActionRoot>)
            else {
                continue;
            };
            let Some(access_ref) = action.access else {
                continue;
            };
            let allowlists = policies
                .get(access_ref.artifact_id.as_str())
                .map_or_else(Vec::new, |rules| {
                    extract_action_allowlists(rules, &action.action_id)
                });
            governed.insert(action.action_id, allowlists);
        }
        Ok(Self { governed })
    }

    fn allows_action(&self, actor: &LocalActor, action_id: &str) -> bool {
        if let Some(rules) = self.governed.get(action_id) {
            return rules.iter().any(|required| match required {
                Some(roles) => actor.has_any_owned(roles),
                None => actor.has_any(&STANDARD_ROLES),
            });
        }
        actor.has_any(&["member", "admin", "owner"])
    }
}

fn extract_action_allowlists(rules: &[AccessRule], action_id: &str) -> Vec<Option<Vec<String>>> {
    let mut allowlists = Vec::new();
    for rule in rules {
        if !rule.actions.iter().any(|id| id == action_id) {
            continue;
        }
        let role_lists: Vec<&[String]> = rule
            .all_of
            .iter()
            .filter_map(|cond| match cond {
                AccessCondition::Role(role) => Some(role.any_of.as_slice()),
                AccessCondition::Rel(_)
                | AccessCondition::Context(_)
                | AccessCondition::Resource(_)
                | AccessCondition::Marking(_)
                | AccessCondition::Entitlement(_) => None,
            })
            .collect();
        if role_lists.is_empty() {
            allowlists.push(None);
        } else if let Some(first) = role_lists.first() {
            let intersected: Vec<String> = first
                .iter()
                .filter(|role| role_lists.iter().all(|list| list.contains(role)))
                .cloned()
                .collect();
            if !intersected.is_empty() {
                allowlists.push(Some(intersected));
            }
        }
    }
    allowlists
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

    fn allows_action(&self, actor: &LocalActor, action_id: &str) -> bool {
        let Ok(snapshot) = self.policies.read() else {
            return false;
        };
        snapshot.allows_action(actor, action_id)
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
            Operation::CallAction(action_id) => self.allows_action(&actor, action_id),
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
        let denied = || ActionDenial::Forbidden {
            reason: codes::AUTHZ_DENIED.to_owned(),
        };
        let Some(actor) = Self::active_matching(Some(request.actor)) else {
            return Err(denied());
        };
        if self.allows_action(&actor, &request.action.action_id) {
            Ok(())
        } else {
            Err(denied())
        }
    }

    fn action_read(&self, request: &ActionReadAuthzRequest<'_>) -> Result<(), ActionDenial> {
        let Some(actor) = Self::active_matching(Some(request.actor)) else {
            return Err(ActionDenial::Invisible);
        };
        if actor.has_any(&["member", "admin", "owner"])
            || self.allows_action(&actor, &request.action.action_id)
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
        if actor.has_any(&["member", "admin", "owner"])
            || self.allows_action(&actor, &request.action.action_id)
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
