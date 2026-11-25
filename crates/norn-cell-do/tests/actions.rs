//! HOST-ACTION-1 over the cell's fetch dispatch: `POST /v1/actions/{actionId}`
//! with a verified fixture principal, the kernel's Cedar PDP deciding the call
//! from that principal alone, and the documents Studio's client parses
//! (`apps/studio/test/fixtures/action-call.json`, read by
//! `apps/studio/test/actions.test.ts` and served verbatim, with the status its
//! `status` member pins, by the Playwright stand-in `apps/studio/e2e/action-cell.ts`).
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
    self, ActorKind, AuthzSchema, CapabilityGrant, Pdp, PdpActionAuthorizer, PdpDecision,
    ResourceInput, ScopeKey,
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

fn fixture_path() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../apps/studio/test/fixtures/action-call.json")
}

/// Regenerate `apps/studio/test/fixtures/action-call.json` directly from the
/// live routes when `NORN_BLESS=1` is set, then load it from disk.
fn load_or_bless_fixture() -> Value {
    if std::env::var("NORN_BLESS").as_deref() == Ok("1") {
        let first = cell();
        let commit_body = melt_body("melt-1", "12", "10", "4b7a2c1e-0000-4000-8000-000000000001");
        let (commit_status, committed) = post(&first, Some(OPERATOR), &commit_body);
        assert_eq!(commit_status, 200, "{committed}");
        let changed = melt_body("melt-1", "12", "9", "4b7a2c1e-0000-4000-8000-000000000001");
        let (conflict_status, conflict) = post(&first, Some(OPERATOR), &changed);
        let (invalid_key_status, invalid_key) = post(
            &first,
            Some(OPERATOR),
            r#"{"input":{},"idempotencyKey":""}"#,
        );

        let second = cell();
        let refusal_body = melt_body("melt-4", "12", "10", "refusals");
        let (denied_status, denied) = post(&second, Some(VIEWER), &refusal_body);
        let (invalid_input_status, invalid_input) = post(
            &second,
            Some(OPERATOR),
            &melt_body("melt-4", "12,5", "10", "invalid-input"),
        );
        let (precondition_status, precondition) = post(
            &second,
            Some(OPERATOR),
            &melt_body("melt-4", "4", "9", "precondition"),
        );
        let (stale_status, stale) = send(
            &second,
            "POST",
            ROUTE,
            "orgId=org_demo&environmentId=dev&version=99",
            Some(OPERATOR),
            &refusal_body,
        );
        let (route_not_found_status, route_not_found) = send(
            &second,
            "GET",
            "/v1/actions/constitution.melt.convert/history",
            SCOPE,
            Some(OPERATOR),
            "",
        );

        let document = json!({
            "committed": normalized(committed),
            "conflict": conflict,
            "status": {
                "conflict": conflict_status,
                "invalidKey": invalid_key_status,
                "denied": denied_status,
                "invalidInput": invalid_input_status,
                "precondition": precondition_status,
                "stale": stale_status,
                "routeNotFound": route_not_found_status,
            },
            "denied": denied,
            "invalidInput": invalid_input,
            "precondition": precondition,
            "stale": stale,
            "routeNotFound": route_not_found,
            "invalidKey": invalid_key,
        });
        let path = fixture_path();
        let text = format!("{}\n", serde_json::to_string_pretty(&document).unwrap());
        let tmp = path.with_extension(format!("json.tmp.{}", std::process::id()));
        std::fs::write(&tmp, &text).unwrap();
        std::fs::rename(&tmp, &path).unwrap();
    }
    serde_json::from_str(&std::fs::read_to_string(fixture_path()).unwrap()).unwrap()
}

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
        authz::AccessRequest::new(
            &authz::VerifiedPrincipal::new(actor, ActorKind::User, None, vec![], BTreeMap::new()),
            Some(key()),
            action,
            action,
            ResourceInput {
                object_type: target.object_type.as_str().into(),
                id: target.key.clone(),
                scope: key(),
                attributes: BTreeMap::new(),
            },
            BTreeMap::new(),
            authz::Binding::Unbound(authz::UnboundProfile::HostedAction),
        )
    }

    fn decide_read(&self, actor: &str, target: &FactKey) -> Result<(), ActionDenial> {
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
        self.decide_read(request.actor, request.target)
    }
    fn action_read_type(
        &self,
        request: &ActionReadTypeAuthzRequest<'_>,
    ) -> Result<(), ActionDenial> {
        self.decide_read(
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
    let (status, body) = send_raw(cell, method, path, query, authorization, body);
    (status, serde_json::from_str(&body).unwrap())
}

/// The exact response bytes, for byte-for-byte comparisons.
fn send_raw(
    cell: &Cell,
    method: &str,
    path: &str,
    query: &str,
    authorization: Option<&str>,
    body: &str,
) -> (u16, String) {
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
    (status, body)
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
    let fixture = load_or_bless_fixture();
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
    served(&fixture, "conflict", &post(&cell, Some(OPERATOR), &changed));
    served(
        &fixture,
        "invalidKey",
        &post(&cell, Some(OPERATOR), r#"{"input":{},"idempotencyKey":""}"#),
    );
}

/// The served response is the fixture's document `name`, with the status the
/// fixture's `status` member pins for it.
fn served(fixture: &Value, name: &str, (status, document): &(u16, Value)) {
    assert_eq!(
        Some(u64::from(*status)),
        fixture["status"][name].as_u64(),
        "{name}: {document}"
    );
    assert_eq!(document, &fixture[name], "{name}");
}

/// Every refusal Studio's action stand-in serves, as the route serves it to
/// a caller without the refusal-detail grant. The stand-in imports these
/// documents, so a change to the route fails here until the fixture follows.
#[test]
fn studio_refusals_match_the_client_contract() {
    let fixture = load_or_bless_fixture();
    let cell = cell();
    let body = melt_body("melt-4", "12", "10", "refusals");
    // An unknown action, an ungranted caller and a foreign scope: one opaque
    // denial. An absent or hidden target is the same document
    // (`norn_api::actions::action_denied`).
    let unknown = "/v1/actions/constitution.melt.unknown";
    served(
        &fixture,
        "denied",
        &send(&cell, "POST", unknown, SCOPE, Some(OPERATOR), &body),
    );
    served(&fixture, "denied", &post(&cell, Some(VIEWER), &body));
    let foreign = "orgId=org_other&environmentId=dev";
    served(
        &fixture,
        "denied",
        &send(&cell, "POST", ROUTE, foreign, Some(OPERATOR), &body),
    );
    // An exact decimal travels as its text; `12,5` is no number.
    let invalid = melt_body("melt-4", "12,5", "10", "invalid-input");
    served(
        &fixture,
        "invalidInput",
        &post(&cell, Some(OPERATOR), &invalid),
    );
    // More fine metal out than in breaks the action's declared precondition.
    let above = melt_body("melt-4", "4", "9", "precondition");
    served(
        &fixture,
        "precondition",
        &post(&cell, Some(OPERATOR), &above),
    );
    let pinned = "orgId=org_demo&environmentId=dev&version=99";
    served(
        &fixture,
        "stale",
        &send(&cell, "POST", ROUTE, pinned, Some(OPERATOR), &body),
    );
    // A path under the action route that names no route is not served.
    served(
        &fixture,
        "routeNotFound",
        &send(
            &cell,
            "GET",
            "/v1/actions/constitution.melt.convert/history",
            SCOPE,
            Some(OPERATOR),
            "",
        ),
    );
    // Nothing above committed.
    let (status, first) = post(&cell, Some(OPERATOR), &body);
    assert_eq!(
        (status, first["outcome"].as_str()),
        (200, Some("committed"))
    );
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
    let unknown = |authorization| {
        send_raw(
            &cell,
            "POST",
            "/v1/actions/constitution.melt.unknown",
            SCOPE,
            Some(authorization),
            &body,
        )
    };
    denied(send(
        &cell,
        "POST",
        "/v1/actions/constitution.melt.unknown",
        SCOPE,
        Some(OPERATOR),
        &body,
    ));
    // A verified principal the host admits but the PDP grants nothing: the
    // kernel's refusal of a known action is byte for byte the unknown-action
    // document, for the viewer and for an operator alike.
    let forbidden = send_raw(&cell, "POST", ROUTE, SCOPE, Some(VIEWER), &body);
    assert_eq!(forbidden, unknown(VIEWER));
    assert_eq!(forbidden, unknown(OPERATOR));
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

/// ACTION-CATALOG-1: a server policy whose call gate admits only
/// `operator@demo`; every other decision is the cell policy's.
struct OperatorCalls(CellPolicy);

impl Authorizer for OperatorCalls {
    fn authorize(&self, request: &AccessRequest<'_>) -> Result<(), Refusal> {
        match (&request.operation, request.actor) {
            (Operation::CallAction(_), Some("operator@demo")) => Ok(()),
            (Operation::CallAction(_), _) => DenyAll.authorize(request),
            _ => self.0.authorize(request),
        }
    }
    fn action(&self, request: &ActionAuthzRequest<'_>) -> Result<(), ActionDenial> {
        self.0.action(request)
    }
    fn action_read(&self, request: &ActionReadAuthzRequest<'_>) -> Result<(), ActionDenial> {
        self.0.action_read(request)
    }
    fn action_read_type(
        &self,
        request: &ActionReadTypeAuthzRequest<'_>,
    ) -> Result<(), ActionDenial> {
        self.0.action_read_type(request)
    }
}

fn catalog(cell: &Cell, authorization: Option<&str>, query: &str) -> (u16, Value) {
    send(cell, "GET", "/v1/actions", query, authorization, "")
}

fn listed(document: &Value) -> Vec<&str> {
    document["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["actionId"].as_str().unwrap())
        .collect()
}

#[test]
fn the_catalog_lists_each_action_as_a_form_reads_it() {
    let cell = cell();
    let (status, document) = catalog(&cell, Some(OPERATOR), SCOPE);
    assert_eq!(status, 200, "{document}");
    assert!(document["definitionsVersion"].as_u64().is_some());
    assert_eq!(
        document["items"][0],
        json!({
            "actionId": MELT_ACTION,
            "title": "Record a fine-metal conversion",
            "description": document["items"][0]["description"],
            "objectKind": MELT,
            "effect": "upsert_input_object",
            "objectIdField": "id",
            "inputSchema": document["items"][0]["inputSchema"],
            "uiEffects": {"irreversible": false, "destructive": false, "money": []},
        })
    );
    assert_eq!(document["items"][0]["inputSchema"]["type"], "object");
    let (_, other) = catalog(
        &cell,
        Some(OPERATOR),
        &format!("{SCOPE}&objectKind=constitution.Other"),
    );
    assert_eq!(listed(&other).len(), 0);
    let (_, melts) = catalog(&cell, Some(OPERATOR), &format!("{SCOPE}&objectKind={MELT}"));
    assert_eq!(listed(&melts), [MELT_ACTION]);
}

#[test]
fn the_catalog_is_the_call_gate_and_never_a_grant() {
    // The cell policy's gate admits any verified principal, so the viewer is
    // listed the action; the kernel's PDP still refuses its call.
    let open = cell();
    let (_, viewer) = catalog(&open, Some(VIEWER), SCOPE);
    assert_eq!(listed(&viewer), [MELT_ACTION]);
    let (status, refused) = post(
        &open,
        Some(VIEWER),
        &melt_body("melt-v", "12", "10", "viewer-1"),
    );
    assert_eq!(
        (status, refused["code"].as_str()),
        (403, Some("authz.denied"))
    );

    // A gate that refuses the viewer lists nothing to it, and a direct call it
    // was never offered is refused as well.
    let store = melt_store(
        DERIVES_LOSS,
        norn_api::action_admission::prepare_action_model,
    );
    let policy = OperatorCalls(CellPolicy::compile(&store));
    let scope = CellScope {
        org_id: "org_demo".into(),
        environment_id: "dev".into(),
    };
    let gated = Cell {
        host: DevHost::from_store(store, scope, |command| command.payload.clone())
            .unwrap()
            .with_authorizer(policy),
        principals: FixturePrincipals::new(NAMES, Arc::new(CallerSlot::default())),
        handles: Handles(EffectiveCatalog::default()),
    };
    let (status, viewer) = catalog(&gated, Some(VIEWER), SCOPE);
    assert_eq!(status, 200);
    assert_eq!(listed(&viewer).len(), 0);
    let (status, refused) = post(
        &gated,
        Some(VIEWER),
        &melt_body("melt-g", "12", "10", "viewer-2"),
    );
    assert_eq!(
        (status, refused["code"].as_str()),
        (403, Some("authz.denied"))
    );
    let (_, operator) = catalog(&gated, Some(OPERATOR), SCOPE);
    assert_eq!(listed(&operator), [MELT_ACTION]);
}

#[test]
fn the_catalog_refuses_as_a_call_would() {
    let cell = cell();
    let denied =
        |(status, document): (u16, Value)| (status, document["code"].as_str().map(str::to_owned));
    let forbidden = (403, Some("authz.denied".to_owned()));
    assert_eq!(denied(catalog(&cell, None, SCOPE)), forbidden);
    assert_eq!(
        denied(catalog(&cell, Some("Bearer fixture:nobody@demo"), SCOPE)),
        forbidden
    );
    assert_eq!(
        denied(catalog(
            &cell,
            Some(OPERATOR),
            "orgId=org_other&environmentId=dev"
        )),
        forbidden
    );
    assert_eq!(
        send(&cell, "POST", "/v1/actions", SCOPE, Some(OPERATOR), "{}").0,
        405
    );
    assert_eq!(
        catalog(&cell, Some(OPERATOR), &format!("{SCOPE}&role=admin")).0,
        400
    );
}

/// MCP (#744): the action's projected tool calls the same governed service as
/// `POST /v1/actions/{id}`: one commit, one receipt, the same refusals.
#[test]
fn an_mcp_action_tool_calls_the_same_governed_service() {
    let cell = cell();
    let rpc = |auth: &str, message: &Value| {
        let (status, body) = send_raw(
            &cell,
            "POST",
            "/v1/mcp",
            SCOPE,
            Some(auth),
            &message.to_string(),
        );
        assert_eq!(status, 200, "{body}");
        serde_json::from_str::<Value>(&body).unwrap()
    };
    let tool_of = |auth: &str| {
        rpc(
            auth,
            &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {"limit": 100}}),
        )["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|tool| tool["_meta"]["norn/actionId"] == "constitution.melt.convert")
            .cloned()
    };
    let tool = tool_of(OPERATOR).expect("the operator may call the melt action");
    let name = tool["name"].as_str().unwrap().to_owned();
    assert!(name.starts_with("act_constitution_melt_convert"), "{name}");
    assert_eq!(tool["_meta"]["ui"]["resourceUri"], "ui://norn/receipt.html");
    assert_eq!(tool["inputSchema"]["type"], "object");

    let key = "4b7a2c1e-0000-4000-8000-0000000000aa";
    let invoke = |auth: &str, id: &str, key: &str| {
        rpc(
            auth,
            &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {
                "name": name, "arguments": {"id": id, "input_fine_g": "12", "output_fine_g": "10"},
                "_meta": {"norn/idempotencyKey": key}
            }}),
        )
    };
    let called = invoke(OPERATOR, "melt-9", key);
    assert_eq!(called["result"]["isError"], false, "{called}");
    let receipt = called["result"]["structuredContent"].clone();
    assert_eq!(receipt["outcome"], "committed");
    // The same call over HTTP is the same call: the original receipt, no second commit.
    let (status, replayed) = post(&cell, Some(OPERATOR), &melt_body("melt-9", "12", "10", key));
    assert_eq!(status, 200);
    assert_eq!(replayed["outcome"], "replayed");
    assert_eq!(replayed["receipt"], receipt["receipt"]);

    // A viewer is refused exactly as a direct call is, or is never offered the tool.
    let (_, direct) = post(
        &cell,
        Some(VIEWER),
        &melt_body("melt-10", "12", "10", "k-viewer"),
    );
    if tool_of(VIEWER).is_some() {
        let refused = invoke(VIEWER, "melt-10", "k-viewer");
        assert_eq!(refused["result"]["isError"], true, "{refused}");
        assert_eq!(refused["result"]["structuredContent"], direct);
    } else {
        assert_eq!(
            invoke(VIEWER, "melt-10", "k-viewer")["error"]["code"],
            -32602
        );
    }
}

fn mcp_rpc(cell: &Cell, auth: &str, message: &Value) -> Value {
    let (status, body) = send_raw(
        cell,
        "POST",
        "/v1/mcp",
        SCOPE,
        Some(auth),
        &message.to_string(),
    );
    assert_eq!(status, 200, "{body}");
    serde_json::from_str::<Value>(&body).unwrap()
}

fn melt_mcp_tool_name(cell: &Cell) -> String {
    mcp_rpc(
        cell,
        OPERATOR,
        &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {"limit": 100}}),
    )["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["_meta"]["norn/actionId"] == "constitution.melt.convert")
        .and_then(|tool| tool["name"].as_str())
        .unwrap()
        .to_owned()
}

#[test]
fn mcp_action_tools_propagate_trace_context() {
    let cell = cell();
    let name = melt_mcp_tool_name(&cell);
    let digest = format!("sha256:{}", "e".repeat(64));
    let trace = json!({
        "traceId": "mcp-trace-1",
        "intentRef": "goal:mcp-melt-1",
        "sessionDigest": digest,
    });

    // Per-action MCP tool carries trace through `_meta["norn/trace"]`.
    let called = mcp_rpc(
        &cell,
        OPERATOR,
        &json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": name,
                "arguments": {"id": "melt-mcp-1", "input_fine_g": "12", "output_fine_g": "10"},
                "_meta": {
                    "norn/idempotencyKey": "k-mcp-trace-1",
                    "norn/trace": trace,
                }
            }
        }),
    );
    assert_eq!(called["result"]["isError"], false, "{called}");
    assert_eq!(called["result"]["structuredContent"]["trace"], trace);

    // Generic `norn_call_action` carries trace through `arguments.trace`.
    let trace2 = json!({
        "traceId": "mcp-trace-2",
        "intentRef": "goal:mcp-melt-2",
    });
    let called2 = mcp_rpc(
        &cell,
        OPERATOR,
        &json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "norn_call_action",
                "arguments": {
                    "action": "constitution.melt.convert",
                    "input": {"id": "melt-mcp-2", "input_fine_g": "12", "output_fine_g": "10"},
                    "trace": trace2,
                },
                "_meta": { "norn/idempotencyKey": "k-mcp-trace-2" }
            }
        }),
    );
    assert_eq!(called2["result"]["isError"], false, "{called2}");
    assert_eq!(called2["result"]["structuredContent"]["trace"], trace2);
}

#[test]
fn mcp_action_tools_refuse_malformed_or_conflicting_trace() {
    let cell = cell();
    let name = melt_mcp_tool_name(&cell);
    let commits_before = cell.host.verified_head().unwrap().commits;

    // Conflicting trace between arguments and `_meta` refuses fail-closed.
    let conflict = mcp_rpc(
        &cell,
        OPERATOR,
        &json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {
                "name": "norn_call_action",
                "arguments": {
                    "action": "constitution.melt.convert",
                    "input": {"id": "melt-mcp-3", "input_fine_g": "12", "output_fine_g": "10"},
                    "trace": {"traceId": "trace-a"},
                },
                "_meta": {
                    "norn/idempotencyKey": "k-mcp-trace-3",
                    "norn/trace": {"traceId": "trace-b"},
                }
            }
        }),
    );
    assert_eq!(conflict["result"]["isError"], true, "{conflict}");
    assert_eq!(cell.host.verified_head().unwrap().commits, commits_before);

    // Malformed trace (raw transcript text in sessionDigest) refuses fail-closed.
    let malformed = mcp_rpc(
        &cell,
        OPERATOR,
        &json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "tools/call",
            "params": {
                "name": name,
                "arguments": {"id": "melt-mcp-4", "input_fine_g": "12", "output_fine_g": "10"},
                "_meta": {
                    "norn/idempotencyKey": "k-mcp-trace-4",
                    "norn/trace": {"traceId": "t", "sessionDigest": "raw transcript"},
                }
            }
        }),
    );
    assert_eq!(malformed["result"]["isError"], true, "{malformed}");
    assert_eq!(cell.host.verified_head().unwrap().commits, commits_before);

    // Unexpected top-level argument (such as `traceId` instead of `trace`) refuses fail-closed.
    let unexpected_key = mcp_rpc(
        &cell,
        OPERATOR,
        &json!({
            "jsonrpc": "2.0",
            "id": 6,
            "method": "tools/call",
            "params": {
                "name": "norn_call_action",
                "arguments": {
                    "action": "constitution.melt.convert",
                    "input": {"id": "melt-mcp-5", "input_fine_g": "12", "output_fine_g": "10"},
                    "traceId": "mcp-trace-misplaced",
                },
                "_meta": {
                    "norn/idempotencyKey": "k-mcp-trace-5",
                }
            }
        }),
    );
    assert_eq!(
        unexpected_key["result"]["isError"], true,
        "{unexpected_key}"
    );
    assert_eq!(cell.host.verified_head().unwrap().commits, commits_before);
}
