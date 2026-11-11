//! HOST-ACTION-1 over the cell's fetch dispatch: `POST /v1/actions/{actionId}`
//! with a verified fixture principal, the kernel's Cedar PDP deciding the call
//! from that principal alone, and the documents Studio's client parses
//! (`apps/studio/test/fixtures/action-call.json`, read by
//! `apps/studio/test/actions.test.ts`).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "dispatch correspondence"
)]
use std::collections::BTreeMap;
use std::sync::Arc;

use norn_api::authz::{AccessRequest, Authorizer, DenyAll, Operation};
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
    self, ActorKind, AuthzSchema, CapabilityGrant, Delegation, Pdp, PdpActionAuthorizer,
    PdpDecision, PrincipalInput, ResourceInput, ScopeKey,
};
use norn_kernel::{
    ActionAuthorizer, ActionAuthzRequest, ActionDenial, ActionReadAuthzRequest,
    ActionReadTypeAuthzRequest, FactKey,
};
use serde_json::{Value, json};

#[path = "../../norn-api/tests/support/action_cell.rs"]
mod action_cell;
use action_cell::{DERIVES_LOSS, MELT, MELT_ACTION, OMITS_LOSS, melt_body, melt_store};

const OPERATOR: &str = "Bearer fixture:operator@demo";
const VIEWER: &str = "Bearer fixture:viewer@demo";
const NAMES: &[(&str, &[&str])] = &[("operator@demo", &["member"]), ("viewer@demo", &["viewer"])];
const SCOPE: &str = "orgId=org_demo&environmentId=dev";
const ROUTE: &str = "/v1/actions/constitution.melt.convert";
const FIXTURE: &str = include_str!("../../../apps/studio/test/fixtures/action-call.json");

fn key() -> ScopeKey {
    ScopeKey {
        org: "org_demo".into(),
        environment: Some("dev".into()),
        tenant: None,
        workspace: None,
        business_unit: None,
    }
}

/// Server policy: the kernel's Cedar PDP, compiled from an `AccessIR` rule
/// against the Cedar schema projected from the cell's own model and action,
/// granting only `operator@demo`. The host operation gate admits any verified
/// principal; the kernel PDP decides the call itself.
struct CellPolicy {
    pdp: Pdp,
}

impl CellPolicy {
    fn compile(store: &CellStore<NativeEngine>) -> Self {
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
        let projection =
            norn_projections::project_cedar_schema(&types, &interfaces, &[action.clone()]);
        let host = norn_projections::project_host_access_schema(&types);
        let schema =
            AuthzSchema::from_projected_cedarschema_fragments(&[&projection.schema, &host])
                .unwrap();
        let access: norn_ir::access::AccessIr = serde_json::from_value(json!({
            "accessVersion": "0.1",
            "policyId": "test.melt.access",
            "rules": [
                {"ruleId": "convert", "actions": [MELT_ACTION], "resourceType": MELT,
                 "scope": {"dimensions": ["org", "environment"]}, "allOf": []},
                {"ruleId": "read_melt", "actions": ["norn.read"], "resourceType": MELT,
                 "scope": {"dimensions": ["org", "environment"]}, "allOf": []}
            ]
        }))
        .unwrap();
        let grants = [CapabilityGrant {
            subject_id: "operator@demo".into(),
            actor_kind: ActorKind::User,
            agent_session_id: None,
            scope: Some(key()),
            capabilities: vec![MELT_ACTION.into(), "norn.read".into()],
            binding: None,
        }];
        Self {
            pdp: Pdp::new(authz::compile(&schema, &access).unwrap(), &grants).unwrap(),
        }
    }

    fn request(actor: &str, action: &str, target: &FactKey) -> authz::AccessRequest {
        authz::AccessRequest {
            principal: PrincipalInput {
                subject_id: actor.into(),
                actor_kind: ActorKind::User,
                agent_session_id: None,
                roles: vec![],
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
        }
    }

    fn read(&self, actor: &str, target: &FactKey) -> Result<(), ActionDenial> {
        match self.pdp.decide(&Self::request(actor, "norn.read", target)) {
            PdpDecision::Allow => Ok(()),
            PdpDecision::Deny(_) => Err(ActionDenial::Invisible),
        }
    }
}

impl Authorizer for CellPolicy {
    fn authorize(&self, request: &AccessRequest<'_>) -> Result<(), Refusal> {
        match (&request.operation, request.actor) {
            (Operation::CallAction(_), Some(_)) => Ok(()),
            _ => DenyAll.authorize(request),
        }
    }
    fn action(&self, request: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        PdpActionAuthorizer::new(self.pdp.clone(), |r: &ActionAuthzRequest<'_>| {
            Ok(Self::request(r.actor, &r.action.action_id, r.target))
        })
        .authorize(request)
    }
    fn action_read(&self, request: &ActionReadAuthzRequest<'_>) -> Result<(), ActionDenial> {
        self.read(request.actor, request.target)
    }
    fn action_read_type(
        &self,
        request: &ActionReadTypeAuthzRequest<'_>,
    ) -> Result<(), ActionDenial> {
        self.read(
            request.actor,
            &FactKey::new(request.object_type.as_str(), "collection"),
        )
    }
}

/// The cell's disclosure runtime: its handle key, and no collection grant,
/// so every reader sees keyed receipt handles.
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
    handles: Handles,
}

fn cell() -> Cell {
    cell_with(DERIVES_LOSS)
}

fn cell_with(writer: &str) -> Cell {
    let store = melt_store(writer, norn_api::action_admission::prepare_action_model);
    let policy = CellPolicy::compile(&store);
    let scope = CellScope {
        org_id: "org_demo".into(),
        environment_id: "dev".into(),
    };
    let host = DevHost::from_store(store, scope, |command| command.payload.clone())
        .unwrap()
        .with_authorizer(policy);
    Cell {
        host,
        principals: FixturePrincipals::new(NAMES, Arc::new(CallerSlot::default())),
        handles: Handles(EffectiveCatalog::default()),
    }
}

fn send(
    cell: &Cell,
    method: &str,
    path: &str,
    query: &str,
    authorization: Option<&str>,
    body: &str,
) -> (u16, Value) {
    let Response { status, body } = server::handle_with(
        &cell.host,
        &cell.principals,
        Some(&cell.handles),
        &server::Request {
            method,
            path,
            query,
            authorization,
            now: 1_700_000_000,
            body,
        },
    );
    (status, serde_json::from_str(&body).unwrap())
}

fn post(cell: &Cell, authorization: Option<&str>, body: &str) -> (u16, Value) {
    send(cell, "POST", ROUTE, SCOPE, authorization, body)
}

/// The receipt handle is HMAC output over this run's ledger; the fixture
/// carries a stand-in, every other member is compared exactly.
fn normalized(mut receipt: Value) -> Value {
    let handle = receipt["receipt"]["receiptHash"].as_str().unwrap();
    assert!(
        !handle.is_empty()
            && handle
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    );
    receipt["receipt"]["receiptHash"] = Value::String("<keyed receipt handle>".into());
    receipt
}

#[test]
fn studio_call_commits_replays_and_matches_the_client_contract() {
    let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    let cell = cell();
    let body = melt_body("melt-1", "12", "10", "4b7a2c1e-0000-4000-8000-000000000001");
    let (status, committed) = post(&cell, Some(OPERATOR), &body);
    assert_eq!(status, 200, "{committed}");
    assert_eq!(normalized(committed.clone()), fixture["committed"]);
    let (status, replayed) = post(&cell, Some(OPERATOR), &body);
    assert_eq!(status, 200);
    assert_eq!(replayed["outcome"], "replayed");
    assert_eq!(replayed["receipt"], committed["receipt"]);

    let changed = melt_body("melt-1", "12", "9", "4b7a2c1e-0000-4000-8000-000000000001");
    let (status, conflict) = post(&cell, Some(OPERATOR), &changed);
    assert_eq!(status, 409);
    assert_eq!(conflict, fixture["conflict"]);

    let (status, invalid) = post(&cell, Some(OPERATOR), r#"{"input":{},"idempotencyKey":""}"#);
    assert_eq!(status, 400);
    assert_eq!(invalid, fixture["invalidKey"]);
}

#[test]
fn the_route_refuses_before_any_effect() {
    let cell = cell();
    let body = melt_body("melt-2", "12", "10", "refusals");
    let denied = |(status, document): (u16, Value)| {
        assert_eq!(status, 403, "{document}");
        assert_eq!(document["code"], "authz.denied");
    };
    denied(post(&cell, None, &body));
    denied(post(&cell, Some("Bearer fixture:nobody@demo"), &body));
    denied(send(
        &cell,
        "POST",
        ROUTE,
        "orgId=org_other&environmentId=dev",
        Some(OPERATOR),
        &body,
    ));
    denied(send(
        &cell,
        "POST",
        "/v1/actions/constitution.melt.unknown",
        SCOPE,
        Some(OPERATOR),
        &body,
    ));
    // A verified principal the PDP grants nothing: the kernel refuses the call.
    let (status, forbidden) = post(&cell, Some(VIEWER), &body);
    assert_eq!(
        (status, forbidden["code"].as_str()),
        (403, Some("action.permission_denied"))
    );
    // A writer that records no loss breaks the fine-mass law: 10 != 8 + 0.
    let law = melt_body("melt-3", "10", "8", "law");
    let (status, violated) = post(&cell_with(OMITS_LOSS), Some(OPERATOR), &law);
    assert_eq!(
        (status, violated["code"].as_str()),
        (422, Some("core.laws.violated"))
    );
    let (status, stale) = send(
        &cell,
        "POST",
        ROUTE,
        "orgId=org_demo&environmentId=dev&version=99",
        Some(OPERATOR),
        &body,
    );
    assert_eq!((status, stale["code"].as_str()), (409, Some("defs.stale")));
    let oversized = format!(
        r#"{{"input":{{"id":"{}"}},"idempotencyKey":"big"}}"#,
        "x".repeat(norn_api::actions::MAX_ACTION_BODY_BYTES)
    );
    let (status, large) = post(&cell, Some(OPERATOR), &oversized);
    assert_eq!(
        (status, large["code"].as_str()),
        (413, Some("request.too_large"))
    );
    let (status, method) = send(&cell, "GET", ROUTE, SCOPE, Some(OPERATOR), "");
    assert_eq!(
        (status, method["code"].as_str()),
        (405, Some("route.method_not_allowed"))
    );
    let (status, unknown) = send(
        &cell,
        "POST",
        "/v1/actions/a/b",
        SCOPE,
        Some(OPERATOR),
        &body,
    );
    assert_eq!(
        (status, unknown["code"].as_str()),
        (404, Some("route.not_found"))
    );
    // Nothing above committed: the first real call is the cell's first melt.
    let (status, first) = post(&cell, Some(OPERATOR), &body);
    assert_eq!(status, 200, "{first}");
    assert_eq!(first["outcome"], "committed");
    assert_eq!(first["version"], 1);
}
