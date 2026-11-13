//! The constitution melt cell behind the DO fetch dispatch, with a policy
//! that decides every action for the cell's current verified caller, as the
//! hosted cells' does, and the `melt` flow installed: shared by the action
//! step (ACTION-STEP-1) and approval (APPROVAL-1) tests.
#![allow(
    dead_code,
    unused_imports,
    reason = "each test target uses part of the shared cell"
)]
use std::collections::BTreeMap;
use std::sync::Arc;

use norn_api::authz::{AccessRequest, Authorizer, DenyAll, Operation, ReadRequest};
use norn_api::definition_routes::DefinitionKinds;
use norn_api::host::DevHost;
use norn_api::question_routes::{AdHocQuestions, QuestionRuntime, QuestionSnapshot};
use norn_api::refusal::Refusal;
use norn_api::scope::CellScope;
use norn_cell::engine::NativeEngine;
use norn_cell::flow_run::RunStarter;
use norn_cell::{CellStore, ObjectRecord};
use norn_cell_do::policy::CallerSlot;
use norn_cell_do::principal::FixturePrincipals;
use norn_cell_do::server::{self, Response};
use norn_checker::EffectiveCatalog;
use norn_checker::metric::compute::QuestionPlan;
use norn_ir::MetricDefinition;
use norn_kernel::authz::{
    self, ActorKind, AuthzSchema, CapabilityGrant, Delegation, Pdp, PdpDecision, PrincipalInput,
    ResourceInput, ScopeKey,
};
use norn_kernel::{
    ActionAuthzRequest, ActionDenial, ActionReadAuthzRequest, ActionReadTypeAuthzRequest, FactKey,
};
use serde_json::{Value, json};

#[path = "../../../norn-api/tests/support/action_cell.rs"]
#[allow(
    dead_code,
    reason = "the shared fixture module serves the action tests too"
)]
pub(crate) mod action_cell;
pub(crate) use action_cell::{DERIVES_LOSS, MELT, MELT_ACTION, melt_body, melt_store};

pub(crate) const SCOPE: &str = "orgId=org_demo&environmentId=dev";
/// May convert melts; starts runs and installs flows.
pub(crate) const OPERATOR: Option<&str> = Some("Bearer fixture:operator@steps");
/// May convert melts and decides.
pub(crate) const APPROVER: Option<&str> = Some("Bearer fixture:approver@steps");
/// May start runs, but may not convert melts.
pub(crate) const MEMBER: Option<&str> = Some("Bearer fixture:member@steps");
pub(crate) const NAMES: &[(&str, &[&str])] = &[
    ("operator@steps", &["admin"]),
    ("approver@steps", &["admin"]),
    ("member@steps", &["member"]),
];
/// The subjects the melt action grant names.
pub(crate) const CONVERTERS: [&str; 2] = ["operator@steps", "approver@steps"];
/// `lowerFlow`'s output for `tools/norn-ts/fixtures/flows/melt.builder.ts`.
pub(crate) const LOWERED: &str =
    include_str!("../../../../tools/norn-ts/fixtures/flows/melt.ir.json");
pub(crate) const FLOW_ID: &str = "ops.approve_melt";
pub(crate) const NOW: u64 = 1_800_000_000;

pub(crate) fn key() -> ScopeKey {
    ScopeKey {
        org: "org_demo".into(),
        environment: Some("dev".into()),
        tenant: None,
        workspace: None,
        business_unit: None,
    }
}

/// Decides every action for the cell's current verified caller (the slot),
/// through the kernel's Cedar PDP granting only [`CONVERTERS`]; reads are
/// open, definitions are written by the operator, runs are started by any
/// verified principal, and a decision by a holder of its declared roles.
struct StepPolicy {
    pdp: Pdp,
    caller: Arc<CallerSlot>,
}

impl StepPolicy {
    fn compile(store: &CellStore<NativeEngine>, caller: Arc<CallerSlot>) -> Self {
        let types: Vec<_> = store
            .model()
            .object_types()
            .filter(|object_type| object_type.name.as_str() == MELT)
            .cloned()
            .collect();
        let interfaces: Vec<_> = store.model().interfaces().cloned().collect();
        let action: norn_ir::ActionRoot = serde_json::from_value(
            serde_json::from_str::<Value>(include_str!(
                "../../../norn-cell/tests/fixtures/constitution.json"
            ))
            .unwrap()["withLoss"][2]["payload"]
                .clone(),
        )
        .unwrap();
        let projection = norn_projections::project_cedar_schema(
            &types,
            &interfaces,
            std::slice::from_ref(&action),
        );
        let host = norn_projections::project_host_access_schema(&types);
        let schema =
            AuthzSchema::from_projected_cedarschema_fragments(&[&projection.schema, &host])
                .unwrap();
        let access: norn_ir::access::AccessIr = serde_json::from_value(json!({
            "accessVersion": "0.1",
            "policyId": "test.melt.steps",
            "rules": [
                {"ruleId": "convert", "actions": [MELT_ACTION], "resourceType": MELT,
                 "scope": {"dimensions": ["org", "environment"]}, "allOf": []},
                {"ruleId": "read_melt", "actions": ["norn.read"], "resourceType": MELT,
                 "scope": {"dimensions": ["org", "environment"]}, "allOf": []}
            ]
        }))
        .unwrap();
        let grants: Vec<CapabilityGrant> = CONVERTERS
            .iter()
            .map(|subject| CapabilityGrant {
                subject_id: (*subject).into(),
                actor_kind: ActorKind::User,
                agent_session_id: None,
                scope: Some(key()),
                capabilities: vec![MELT_ACTION.into(), "norn.read".into()],
                binding: None,
            })
            .collect();
        Self {
            pdp: Pdp::new(authz::compile(&schema, &access).unwrap(), &grants).unwrap(),
            caller,
        }
    }

    /// The PDP request for the slot's verified caller, never the actor a
    /// request names.
    fn request(&self, action: &str, target: &FactKey) -> Option<authz::AccessRequest> {
        let caller = self.caller.current()?;
        Some(authz::AccessRequest {
            principal: PrincipalInput {
                subject_id: caller.subject_id,
                actor_kind: ActorKind::User,
                agent_session_id: None,
                roles: caller.roles,
                attributes: BTreeMap::new(),
            },
            scope: Some(key()),
            capability: action.into(),
            action_id: action.into(),
            resource: ResourceInput {
                object_type: target.object_type.as_str().into(),
                id: target.key.clone(),
                scope: key(),
                attributes: BTreeMap::new(),
            },
            context: BTreeMap::new(),
            binding: None,
            delegation: Delegation::Direct,
        })
    }

    fn decide(
        &self,
        action: &str,
        target: &FactKey,
        denial: ActionDenial,
    ) -> Result<(), ActionDenial> {
        match self
            .request(action, target)
            .map(|request| self.pdp.decide(&request))
        {
            Some(PdpDecision::Allow) => Ok(()),
            Some(PdpDecision::Deny(_)) | None => Err(denial),
        }
    }

    fn holds(&self, actor: &str, roles: &[String]) -> bool {
        self.caller
            .current()
            .filter(|caller| caller.subject_id == actor)
            .is_some_and(|caller| caller.roles.iter().any(|held| roles.contains(held)))
    }
}

impl Authorizer for StepPolicy {
    fn authorize(&self, request: &AccessRequest<'_>) -> Result<(), Refusal> {
        let granted = match (&request.operation, request.actor) {
            (
                Operation::ReadDefinitions | Operation::StartRun(_) | Operation::CallAction(_),
                Some(_),
            ) => true,
            (Operation::WriteDefinitions, Some(actor)) => actor == "operator@steps",
            (Operation::Decide { roles, .. }, Some(actor)) => self.holds(actor, roles),
            _ => false,
        };
        if granted {
            Ok(())
        } else {
            DenyAll.authorize(request)
        }
    }
    fn read(&self, _request: &ReadRequest<'_>) -> Result<(), Refusal> {
        Ok(())
    }
    fn verified_principal(&self, actor: &str) -> Option<RunStarter> {
        let caller = self.caller.current()?;
        (caller.subject_id == actor).then(|| RunStarter {
            subject: caller.subject_id,
            actor_kind: caller.actor_kind.to_owned(),
            roles: caller.roles,
            role_table: caller.role_table.to_owned(),
        })
    }
    fn action(&self, request: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        self.decide(
            &request.action.action_id,
            request.target,
            ActionDenial::Forbidden {
                reason: norn_api::refusal::codes::AUTHZ_DENIED.into(),
            },
        )
    }
    fn action_read(&self, request: &ActionReadAuthzRequest<'_>) -> Result<(), ActionDenial> {
        self.decide("norn.read", request.target, ActionDenial::Invisible)
    }
    fn action_read_type(
        &self,
        request: &ActionReadTypeAuthzRequest<'_>,
    ) -> Result<(), ActionDenial> {
        self.decide(
            "norn.read",
            &FactKey::new(request.object_type.as_str(), "collection"),
            ActionDenial::Invisible,
        )
    }
}

/// The cell's disclosure runtime: its handle key only.
pub(crate) struct Handles(EffectiveCatalog);

impl QuestionRuntime<NativeEngine> for Handles {
    fn plan(&self, _question: &str) -> Result<QuestionPlan, Refusal> {
        Err(norn_api::authz::read_denied())
    }
    fn snapshot(
        &self,
        _store: &CellStore<NativeEngine>,
        _plan: &QuestionPlan,
        _actor: &str,
    ) -> Result<QuestionSnapshot, Refusal> {
        Err(norn_api::authz::read_denied())
    }
    fn access(
        &self,
        _scope: &CellScope,
        _actor: &str,
        _question: &str,
        _capability: norn_api::authz::ReadCapability,
        _row: Option<&ObjectRecord>,
    ) -> Result<authz::AccessRequest, Refusal> {
        Err(norn_api::authz::read_denied())
    }
    fn min_disclosed_bucket(&self) -> usize {
        3
    }
    fn handle_key(&self) -> &[u8] {
        &[9; 32]
    }
}

impl AdHocQuestions<NativeEngine> for Handles {
    fn definitions(&self) -> &[MetricDefinition] {
        &[]
    }
    fn catalog(&self) -> &EffectiveCatalog {
        &self.0
    }
    fn max_rows(&self) -> u32 {
        0
    }
    fn max_buckets(&self) -> usize {
        0
    }
    fn admit(
        &self,
        question: &norn_ir::QuestionIR,
        max_rows: u32,
    ) -> Result<norn_checker::metric::compute::QuestionPlan, Vec<norn_checker::Diagnostic>> {
        norn_api::question_routes::admit_declared::<NativeEngine>(self, question, max_rows)
    }
    fn unit_partition(&self, _metric: &str) -> Vec<String> {
        Vec::new()
    }
}

pub(crate) struct Cell {
    pub(crate) host: DevHost<NativeEngine>,
    pub(crate) principals: FixturePrincipals,
    pub(crate) caller: Arc<CallerSlot>,
    pub(crate) handles: Handles,
    pub(crate) kinds: DefinitionKinds,
}

pub(crate) fn cell() -> Cell {
    let store = melt_store(
        DERIVES_LOSS,
        norn_api::action_admission::prepare_action_model,
    );
    let caller = Arc::new(CallerSlot::default());
    let policy = StepPolicy::compile(&store, Arc::clone(&caller));
    let scope = CellScope {
        org_id: "org_demo".into(),
        environment_id: "dev".into(),
    };
    let host = DevHost::from_store(store, scope, |command| command.payload.clone())
        .unwrap()
        .with_authorizer(policy);
    let cell = Cell {
        host,
        principals: FixturePrincipals::new(NAMES, Arc::clone(&caller)),
        caller,
        handles: Handles(EffectiveCatalog::default()),
        kinds: DefinitionKinds::default()
            .with_roles(["viewer", "member", "admin", "owner"])
            .with_restater(norn_api::definition_routes::checker_restatement)
            .with_builtins(norn_cell::builtin_registered),
    };
    install(&cell);
    cell
}

/// One request through the published dispatch; its resolver carries the
/// cell's caller slot.
pub(crate) fn call(
    cell: &Cell,
    method: &str,
    path: &str,
    auth: Option<&str>,
    body: &str,
) -> (u16, Value) {
    call_with(cell, method, path, SCOPE, auth, body)
}

pub(crate) fn call_with(
    cell: &Cell,
    method: &str,
    path: &str,
    query: &str,
    auth: Option<&str>,
    body: &str,
) -> (u16, Value) {
    call_at(cell, (method, path, query), auth, body, NOW)
}

/// [`call_with`] at `now` (Unix seconds).
pub(crate) fn call_at(
    cell: &Cell,
    (method, path, query): (&str, &str, &str),
    auth: Option<&str>,
    body: &str,
    now: u64,
) -> (u16, Value) {
    let request = server::Request {
        method,
        path,
        query,
        authorization: auth,
        now,
        body,
    };
    let Response { status, body } = server::handle_all(
        &cell.host,
        &cell.principals,
        Some(&cell.handles),
        &cell.kinds,
        &request,
    );
    (status, serde_json::from_str(&body).unwrap())
}

pub(crate) fn install(cell: &Cell) {
    let artifacts: Vec<Value> = serde_json::from_str::<Vec<Value>>(LOWERED)
        .unwrap()
        .into_iter()
        .map(|mut artifact| {
            artifact["scope"] = json!({ "orgId": "org_demo", "environmentId": "dev" });
            artifact
        })
        .collect();
    let mut body = json!({ "kind": "flow", "id": FLOW_ID, "payload": {
        "flow": { "id": FLOW_ID, "version": "1.0.0", "title": "Approve a melt" },
        "artifacts": artifacts } });
    let (_, admitted) = call(
        cell,
        "POST",
        "/v1/definitions:admit",
        OPERATOR,
        &body.to_string(),
    );
    body["signoff"] = json!({ "restatementSha256": admitted["restatementSha256"] });
    let (status, installed) = call(
        cell,
        "POST",
        "/v1/definitions:install",
        OPERATOR,
        &body.to_string(),
    );
    assert_eq!(status, 200, "{installed}");
}
