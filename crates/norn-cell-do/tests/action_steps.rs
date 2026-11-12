//! ACTION-STEP-1 through the DO fetch dispatch: a run's `norn.action@1` step
//! calls a governed action under the authority of whoever started the run,
//! for the whole run. The policy here decides an action for the cell's
//! current verified caller, as the hosted cells' does, so a step that ran as
//! the request's own caller (the decider) instead of the recorded starter
//! would be decided for the decider. A decision adds the decider's approval
//! as a condition and lends the run none of the decider's rights.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "dispatch correspondence"
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

#[path = "../../norn-api/tests/support/action_cell.rs"]
#[allow(
    dead_code,
    reason = "the shared fixture module serves the action tests too"
)]
mod action_cell;
use action_cell::{DERIVES_LOSS, MELT, MELT_ACTION, melt_store};

const SCOPE: &str = "orgId=org_demo&environmentId=dev";
/// May convert melts; starts runs and installs flows.
const OPERATOR: Option<&str> = Some("Bearer fixture:operator@steps");
/// May convert melts and decides.
const APPROVER: Option<&str> = Some("Bearer fixture:approver@steps");
/// May start runs, but may not convert melts.
const MEMBER: Option<&str> = Some("Bearer fixture:member@steps");
const NAMES: &[(&str, &[&str])] = &[
    ("operator@steps", &["admin"]),
    ("approver@steps", &["admin"]),
    ("member@steps", &["member"]),
];
/// The subjects the melt action grant names.
const CONVERTERS: [&str; 2] = ["operator@steps", "approver@steps"];
/// `lowerFlow`'s output for `tools/norn-ts/fixtures/flows/melt.builder.ts`.
const LOWERED: &str = include_str!("../../../tools/norn-ts/fixtures/flows/melt.ir.json");
const FLOW_ID: &str = "ops.approve_melt";
const NOW: u64 = 1_800_000_000;

fn key() -> ScopeKey {
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
                "../../norn-cell/tests/fixtures/constitution.json"
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
struct Handles(EffectiveCatalog);

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
}

struct Cell {
    host: DevHost<NativeEngine>,
    principals: FixturePrincipals,
    caller: Arc<CallerSlot>,
    handles: Handles,
    kinds: DefinitionKinds,
}

fn cell() -> Cell {
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

/// One request the way the published host serves it: run and decision
/// routes through `handle_runs` with the cell's caller slot, the rest
/// through the ordinary dispatch.
fn call(cell: &Cell, method: &str, path: &str, auth: Option<&str>, body: &str) -> (u16, Value) {
    let request = server::Request {
        method,
        path,
        query: SCOPE,
        authorization: auth,
        now: NOW,
        body,
    };
    let Response { status, body } = norn_cell_do::runs::handle_runs(
        &cell.host,
        &cell.principals,
        Some(&cell.handles),
        &cell.caller,
        &request,
    )
    .unwrap_or_else(|| {
        server::handle_all(
            &cell.host,
            &cell.principals,
            Some(&cell.handles),
            &cell.kinds,
            &request,
        )
    });
    (status, serde_json::from_str(&body).unwrap())
}

fn install(cell: &Cell) {
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

/// Start `run` as `starter`, then have the approver approve it.
fn start_and_approve(cell: &Cell, starter: Option<&str>, run: &str) -> Value {
    let (status, opened) = call(
        cell,
        "POST",
        "/v1/runs",
        starter,
        &json!({ "flow": FLOW_ID, "runId": run }).to_string(),
    );
    assert_eq!(status, 201, "{opened}");
    assert_eq!(opened["state"], json!("awaiting_perform_ask_review"));
    let (_, page) = call(cell, "GET", "/v1/decisions", APPROVER, "");
    assert_eq!(page["total"], json!(1), "{page}");
    let id = page["items"][0]["id"].as_str().unwrap().to_owned();
    let (status, decided) = call(
        cell,
        "POST",
        &format!("/v1/decisions/{id}:decide"),
        APPROVER,
        &json!({ "choice": "approve", "idempotencyKey": "k-1" }).to_string(),
    );
    assert_eq!(status, 200, "{decided}");
    decided
}

fn melt(cell: &Cell) -> u16 {
    call(
        cell,
        "GET",
        "/v1/objects/constitution.Melt/melt-7",
        OPERATOR,
        "",
    )
    .0
}

/// STEP-AS-STARTER: approved, the step converts the melt under the starter's
/// authority and the run completes; the decider's own caller is back after.
#[test]
fn an_approved_step_runs_as_the_starter() {
    let cell = cell();
    let decided = start_and_approve(&cell, OPERATOR, "melt-run-1");
    assert_eq!(decided["run"]["state"], json!("completed"), "{decided}");
    assert_eq!(melt(&cell), 200);
    let (_, run) = call(&cell, "GET", "/v1/runs/melt-run-1", OPERATOR, "");
    let steps: Vec<(&str, &str, &str)> = run["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|step| {
            (
                step["id"].as_str().unwrap(),
                step["kind"].as_str().unwrap(),
                step["status"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        steps,
        [
            ("ask_review", "decide", "done"),
            ("convert", "perform", "done"),
            ("succeed", "succeed", "done")
        ],
        "{run}"
    );
    // Each request ends with no caller recorded: the step's starter never
    // outlives its one call, and the request's own principal ends with it.
    assert_eq!(cell.caller.current(), None);
}

/// STEP-NO-LAUNDERING (coordinator ruling): a starter without the right to
/// convert cannot get a conversion run by routing it after a decision whose
/// decider has that right. The step is denied as the starter, the run fails,
/// and nothing is converted.
#[test]
fn a_decider_lends_the_run_none_of_its_rights() {
    let cell = cell();
    let decided = start_and_approve(&cell, MEMBER, "melt-run-2");
    assert_eq!(decided["run"]["state"], json!("failed_denied"), "{decided}");
    assert_eq!(melt(&cell), 404);
    let (_, run) = call(&cell, "GET", "/v1/runs/melt-run-2", MEMBER, "");
    assert_eq!(run["terminal"], json!(true));
    let convert = &run["steps"][1];
    assert_eq!(
        (
            &convert["id"],
            &convert["status"],
            &convert["error"]["code"]
        ),
        (&json!("convert"), &json!("failed"), &json!("failed_denied")),
        "{run}"
    );
}
