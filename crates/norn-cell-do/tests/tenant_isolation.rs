//! TENANT-ISOLATION-1 (#1450: the audit found no cross-tenant suite): a
//! principal of tenant A addressing tenant B gets exactly what a scope with no
//! cell answers, and no data, on every route the cell serves.
//!
//! The suite is table-driven over the host's own route list: [`probes`]
//! matches [`Route`] exhaustively, so a route added without a row does not
//! compile, and every row must name its route ([`Route::of`]). MCP tools are
//! rows too, checked against the catalog the cell lists to its owner.
//!
//! Every request goes through [`Tenant::serve`], the dispatch the Durable
//! Object binding calls (source and upload routes, then the engine routes,
//! then the cell), with the analytics engine (olapd, D102) enabled. Each
//! answer is compared with the same request at a vacant cell of B's scope:
//!
//! - the same status and the same bytes;
//! - the same work: the storage statements the request ran (an outsider's
//!   request reads nothing at B, as nothing exists to read at a vacant
//!   scope), so no timing shape tells the two apart;
//! - no engine call (olapd is never asked for an outsider);
//! - none of B's identifiers or data in the body.
//!
//! Inside A's own cell, a member naming B's scope, or B's decision and run
//! ids, is answered exactly as one naming a scope or ids that exist nowhere.
//! The live path (`apps/norn-cell-do/subscribe.mjs`) re-asks these same GET
//! routes as the subscriber; its side is pinned in
//! `packages/norn-demo-ui/test/live-host.test.ts`.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::too_many_lines,
    reason = "tenant isolation correspondence"
)]

use std::cell::Cell;
use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signer, SigningKey};
use norn_api::scope::CellScope;
use norn_cell::engine::{NativeEngine, ReadIsolation, SqlEngine, SqlValue};
use norn_cell::trusted::TrustedCommit as _;
use norn_cell::{CellError, CommitBatch, FactWriteRow, MAIN_WORLD};
use norn_cell_do::connect::Connect;
use norn_cell_do::policy::DEMO_GRANTS;
use norn_cell_do::principal::TokenPrincipals;
use norn_cell_do::seed;
use norn_cell_do::server::{Request, Response, Route};
use norn_cell_do::tenant::{ACCESS_DECIDE_PATH, Served, Tenant};
use serde_json::{Value, json};

const ISSUER: &str = "https://studio-preview.example";
const LINEAGE_KEY: [u8; 32] = [7; 32];
const NOW: u64 = 1_800_000_000;
/// An item only tenant B holds.
const B_ITEM: &str = "B-ONLY-ITEM-7";

// ------------------------------------------------------------------ engine

/// The native engine, counting every storage call a request makes.
struct Counting {
    inner: NativeEngine,
    calls: Rc<Cell<u64>>,
}

impl Counting {
    fn new(calls: &Rc<Cell<u64>>) -> Self {
        Self {
            inner: NativeEngine::open_in_memory().unwrap(),
            calls: Rc::clone(calls),
        }
    }

    fn tick(&self) {
        self.calls.set(self.calls.get() + 1);
    }
}

impl SqlEngine for Counting {
    fn execute(&self, sql: &str, params: &[SqlValue]) -> Result<(), CellError> {
        self.tick();
        self.inner.execute(sql, params)
    }
    fn query(&self, sql: &str, params: &[SqlValue]) -> Result<Vec<Vec<SqlValue>>, CellError> {
        self.tick();
        self.inner.query(sql, params)
    }
    fn execute_batch(&self, sql: &str) -> Result<(), CellError> {
        self.tick();
        self.inner.execute_batch(sql)
    }
    fn transaction<T, X: From<CellError>>(&self, f: impl FnOnce() -> Result<T, X>) -> Result<T, X> {
        self.tick();
        self.inner.transaction(f)
    }
    fn platform_table_globs(&self) -> &'static [&'static str] {
        self.inner.platform_table_globs()
    }
    fn read_isolation(&self) -> ReadIsolation {
        self.inner.read_isolation()
    }
    fn read_cache_revision(&self) -> Result<Option<(u64, u64)>, CellError> {
        self.tick();
        self.inner.read_cache_revision()
    }
}

// ------------------------------------------------------------------ tokens

fn key() -> SigningKey {
    SigningKey::from_bytes(&[1; 32])
}

fn jwks() -> String {
    json!({ "keys": [{ "kty": "OKP", "crv": "Ed25519", "alg": "EdDSA", "kid": "k1",
                       "x": URL_SAFE_NO_PAD.encode(key().verifying_key().as_bytes()) }] })
    .to_string()
}

fn signed(signer: &SigningKey, kind: &str, org: &str, role: &str) -> String {
    let amr = if kind == "service" {
        "api_key"
    } else {
        "password"
    };
    let claims = json!({ "iss": ISSUER, "aud": "norn-host", "sub": format!("{role}-of-{org}"),
            "iat": NOW - 10, "exp": NOW + 290,
            "norn": { "kind": kind, "org": org, "role": role, "amr": amr } });
    let head = URL_SAFE_NO_PAD.encode(json!({ "alg": "EdDSA", "kid": "k1" }).to_string());
    let body = URL_SAFE_NO_PAD.encode(claims.to_string());
    let signature = signer.sign(format!("{head}.{body}").as_bytes());
    format!(
        "Bearer {head}.{body}.{}",
        URL_SAFE_NO_PAD.encode(signature.to_bytes())
    )
}

fn user(org: &str, role: &str) -> String {
    signed(&key(), "user", org, role)
}

// ------------------------------------------------------------------- cells

/// One scope's cell as the deployed host serves it: its tenant, token
/// resolver and source/upload routes, with the analytics engine enabled.
struct Host {
    tenant: Tenant<Counting>,
    tokens: TokenPrincipals,
    connect: Connect,
    calls: Rc<Cell<u64>>,
}

/// What one request answered, and what it cost.
#[derive(Debug, PartialEq, Eq)]
struct Answer {
    response: Response,
    /// Storage calls the request made.
    calls: u64,
    /// Whether it asked the analytics engine (olapd) anything.
    engine: bool,
}

fn scope(org: &str, environment: &str) -> CellScope {
    CellScope {
        org_id: org.into(),
        environment_id: environment.into(),
    }
}

fn query(scope: &CellScope) -> String {
    format!(
        "orgId={}&environmentId={}",
        scope.org_id, scope.environment_id
    )
}

fn connect() -> Connect {
    let vars = json!({
        "NORN_S3_ENDPOINT": "https://storage.example.test",
        "NORN_S3_BUCKET": "uploads-bucket",
        "NORN_SECRET_S3_ACCESS_KEY_ID": "key-id",
        "NORN_SECRET_S3_SECRET_ACCESS_KEY": "secret",
    });
    Connect::new("[]", &vars.to_string()).unwrap()
}

fn host(tenant: Tenant<Counting>, calls: Rc<Cell<u64>>) -> Host {
    let mut tenant = tenant.with_receipt_handles(Some(&LINEAGE_KEY)).unwrap();
    tenant.enable_analytics();
    let tokens = TokenPrincipals::new(
        ISSUER,
        &jwks(),
        tenant.scope().clone(),
        tenant.caller(),
        NOW,
    )
    .unwrap();
    Host {
        tenant,
        tokens,
        connect: connect(),
        calls,
    }
}

/// The seeded cell of `scope`, with its demo rows and, when `own` names
/// one, an item only this tenant holds.
fn seeded(scope: &CellScope, own: Option<&str>) -> Host {
    let calls = Rc::new(Cell::new(0));
    let mut store = seed::provision(Counting::new(&calls), scope).unwrap();
    if let Some(id) = own {
        let text = |value: &str| norn_ir::RuntimeValue::from(value);
        let floor = store.read().clock_floor().unwrap();
        store
            .commit(&CommitBatch {
                world: MAIN_WORLD.into(),
                scope: scope.key(),
                logical_time: floor.max(1),
                writes: vec![FactWriteRow::Put {
                    object_type: "canon.Item".into(),
                    key: id.into(),
                    values: BTreeMap::from([
                        ("id".to_owned(), text(id)),
                        ("code".to_owned(), text("B-ONLY")),
                        ("label".to_owned(), text("Tenant B's own item")),
                    ]),
                }],
                ..CommitBatch::default()
            })
            .unwrap();
    }
    let tenant = Tenant::seeded(scope.clone(), &DEMO_GRANTS, store, None).unwrap();
    host(tenant, calls)
}

/// A scope with no cell: what B would be had it never been provisioned.
fn vacant(scope: &CellScope) -> Host {
    let calls = Rc::new(Cell::new(0));
    let opened = Rc::clone(&calls);
    let tenant = Tenant::open(
        scope.clone(),
        &DEMO_GRANTS,
        Box::new(move || Ok(Counting::new(&opened))),
        Some(&LINEAGE_KEY),
    )
    .unwrap();
    assert!(!tenant.is_provisioned());
    host(tenant, calls)
}

fn send(
    host: &mut Host,
    method: &str,
    path: &str,
    query: &str,
    auth: Option<&str>,
    body: &str,
) -> Answer {
    // Nothing left over from setting the cell up, or an earlier request.
    drop(host.tenant.take_engine_call());
    host.calls.set(0);
    let request = Request {
        method,
        path,
        query,
        authorization: auth,
        now: NOW,
        body,
    };
    let response =
        match host
            .tenant
            .serve(Some(&mut host.connect), &host.tokens, &request, NOW * 1000)
        {
            Served::Connect(routed) => routed.response,
            Served::Cell(response) => response,
        };
    let response = Response {
        status: response.status,
        body: without_incident_ids(&response.body),
    };
    Answer {
        response,
        calls: host.calls.get(),
        engine: host.tenant.take_engine_call().is_some(),
    }
}

/// `body` with each incident id (`inc-<clock>-<sequence>`, INCIDENT-1)
/// replaced by a marker: an id is fresh per internal refusal whatever the
/// scope, so it names the request, never the tenant.
fn without_incident_ids(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(at) = rest.find("\"inc-") {
        out.push_str(&rest[..=at]);
        let tail = &rest[at + 1..];
        let end = tail.find('"').unwrap_or(tail.len());
        out.push_str("<incident>");
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

fn json_of(response: &Response) -> Value {
    serde_json::from_str(&response.body).unwrap()
}

/// B: the seeded demo cell, with one run waiting on its ask, and what
/// identifies its data.
fn tenant_b() -> (Host, Ids) {
    let scope = seed::demo_scope();
    let mut b = seeded(&scope, Some(B_ITEM));
    let at = query(&scope);
    let owner = user(&scope.org_id, "owner");
    let artifacts: Vec<Value> = serde_json::from_str::<Vec<Value>>(include_str!(
        "../../../tools/norn-ts/fixtures/flows/ask.ir.json"
    ))
    .unwrap()
    .into_iter()
    .map(|mut artifact| {
        artifact["scope"] = json!({ "orgId": scope.org_id, "environmentId": scope.environment_id });
        artifact
    })
    .collect();
    let mut body = json!({ "kind": "flow", "id": "ops.approve_refund", "payload": {
        "flow": { "id": "ops.approve_refund", "version": "1.0.0", "title": "Approve a refund" },
        "artifacts": artifacts } });
    let admitted = send(
        &mut b,
        "POST",
        "/v1/definitions:admit",
        &at,
        Some(&owner),
        &body.to_string(),
    );
    assert_eq!(admitted.response.status, 200, "{}", admitted.response.body);
    body["signoff"] =
        json!({ "restatementSha256": json_of(&admitted.response)["restatementSha256"] });
    let installed = send(
        &mut b,
        "POST",
        "/v1/definitions:install",
        &at,
        Some(&owner),
        &body.to_string(),
    );
    assert_eq!(
        installed.response.status, 200,
        "{}",
        installed.response.body
    );
    let admin = user(&scope.org_id, "admin");
    let started = send(
        &mut b,
        "POST",
        "/v1/runs",
        &at,
        Some(&admin),
        &json!({ "flow": "ops.approve_refund", "runId": "refund-b-7" }).to_string(),
    );
    assert_eq!(started.response.status, 201, "{}", started.response.body);
    let page = send(
        &mut b,
        "GET",
        "/v1/decisions",
        &format!("{at}&state=pending"),
        Some(&admin),
        "",
    );
    assert_eq!(page.response.status, 200, "{}", page.response.body);
    let decision = json_of(&page.response)["items"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let items = send(
        &mut b,
        "GET",
        "/v1/objects/canon.Item",
        &at,
        Some(&owner),
        "",
    );
    assert_eq!(items.response.status, 200, "{}", items.response.body);
    let records = json_of(&items.response)["records"].clone();
    assert!(
        records
            .as_array()
            .unwrap()
            .iter()
            .any(|record| record["id"] == B_ITEM),
        "{records}"
    );
    let object = B_ITEM.to_owned();
    let ids = Ids {
        scope,
        kind: "canon.Item".into(),
        object,
        decision,
        run: "refund-b-7".into(),
        definition: "ops.approve_refund".into(),
        action: "canon.rename".into(),
        question: "on_hand".into(),
        transform: "ops.daily".into(),
        row: "row-1".into(),
        guard: "guard-b-1".into(),
        upload: "upload-b-1".into(),
        connection: "acme.main".into(),
    };
    (b, ids)
}

/// What a request names: a scope and the identifiers in its path and body.
#[derive(Clone, Debug)]
struct Ids {
    scope: CellScope,
    kind: String,
    object: String,
    decision: String,
    run: String,
    definition: String,
    action: String,
    question: String,
    transform: String,
    row: String,
    guard: String,
    upload: String,
    connection: String,
}

impl Ids {
    /// The same shape of request, naming only what exists nowhere: each id
    /// replaced by one of the same length.
    fn nowhere(&self) -> Self {
        let blank = |id: &str| "z".repeat(id.len());
        Self {
            scope: scope(
                &blank(&self.scope.org_id),
                &blank(&self.scope.environment_id),
            ),
            kind: self.kind.clone(),
            object: blank(&self.object),
            decision: blank(&self.decision),
            run: blank(&self.run),
            definition: self.definition.clone(),
            action: self.action.clone(),
            question: self.question.clone(),
            transform: self.transform.clone(),
            row: blank(&self.row),
            guard: blank(&self.guard),
            upload: blank(&self.upload),
            connection: blank(&self.connection),
        }
    }

    /// Every identifier this names that is the tenant's own.
    fn private(&self) -> Vec<&str> {
        vec![
            &self.object,
            &self.decision,
            &self.run,
            &self.guard,
            &self.upload,
        ]
    }

    /// `text` with every identifier of `self` replaced by a marker (the
    /// scope's parts are short words a refusal's prose may hold, so they
    /// stay: an answer that echoes a scope fails the comparison).
    fn normalized(&self, text: &str) -> String {
        let mut out = text.to_owned();
        for id in [
            &self.object,
            &self.decision,
            &self.run,
            &self.row,
            &self.guard,
            &self.upload,
            &self.connection,
        ] {
            out = out.replace(id.as_str(), "<id>");
        }
        out
    }
}

// ------------------------------------------------------------------- table

/// One request row: method, path, body.
type Probe = (&'static str, String, String);

/// The rows of `route`: its own method, with the body it takes. Exhaustive
/// over [`Route`]: a route without a row does not compile.
fn probes(route: Route, ids: &Ids) -> Vec<Probe> {
    let Ids {
        kind,
        object,
        decision,
        run,
        definition,
        action,
        question,
        transform,
        row,
        guard,
        upload,
        connection,
        ..
    } = ids;
    let get = |path: String| vec![("GET", path, String::new())];
    let post = |path: String, body: Value| vec![("POST", path, body.to_string())];
    match route {
        Route::Access => get("/v1/access".into()),
        Route::ActionCatalog => get("/v1/actions".into()),
        Route::Provision => post("/v1/cell:provision".into(), json!({})),
        Route::DefinitionsPlan => {
            post("/v1/cell/definitions:plan".into(), json!({"artifacts": []}))
        }
        Route::DefinitionsApply => post(
            "/v1/cell/definitions:apply".into(),
            json!({"artifacts": [], "expectedVersion": 1}),
        ),
        Route::Action => post(
            format!("/v1/actions/{action}"),
            json!({"input": {"id": object}}),
        ),
        Route::DefinitionsCatalog => get("/v1/definitions".into()),
        Route::DefinitionsAdmit => post("/v1/definitions:admit".into(), json!({})),
        Route::DefinitionsInstall => post("/v1/definitions:install".into(), json!({})),
        Route::DefinitionsList => get("/v1/definitions:list".into()),
        Route::Definition => get(format!("/v1/definitions/{definition}")),
        Route::DefinitionRetire => post(format!("/v1/definitions/{definition}:retire"), json!({})),
        Route::Objects => get(format!("/v1/objects/{kind}")),
        Route::ObjectProfile => get(format!("/v1/objects/{kind}:profile")),
        Route::ObjectFacets => get(format!("/v1/objects/{kind}:facets")),
        Route::Object => get(format!("/v1/objects/{kind}/{object}")),
        Route::ObjectLinks => get(format!("/v1/objects/{kind}/{object}/links")),
        Route::ObjectLineage => get(format!("/v1/objects/{kind}/{object}/lineage")),
        Route::ObjectFindings => get(format!("/v1/objects/{kind}/{object}/findings")),
        Route::ObjectHistory => get(format!("/v1/objects/{kind}/{object}/history")),
        Route::GuardPrompt => post("/v1/guard:prompt".into(), json!({"prompt": object})),
        Route::GuardRestore => post(format!("/v1/guard/{guard}:restore"), json!({})),
        Route::ObjectReveal => post(format!("/v1/objects/{kind}/{object}/reveal"), json!({})),
        Route::ObjectsQuarantined => get(format!("/v1/objects/{kind}:quarantined")),
        Route::QuestionAnswer => get(format!("/v1/questions/{question}/answer")),
        Route::QuestionTrace => get(format!("/v1/questions/{question}/trace")),
        Route::QuestionsEvaluate => post("/v1/questions:evaluate".into(), json!({})),
        Route::QuestionsTrace => post("/v1/questions:trace".into(), json!({})),
        Route::SourceSync => post(format!("/v1/sources/{connection}:sync"), json!({})),
        Route::UploadBegin => post("/v1/uploads:begin".into(), json!({"name": "rows.csv"})),
        Route::UploadRead => post("/v1/uploads:read".into(), json!({"upload": upload})),
        Route::UploadComplete => post(format!("/v1/uploads/{upload}:complete"), json!({})),
        Route::UploadLoad => post(format!("/v1/uploads/{upload}:load"), json!({})),
        Route::UploadDefine => post(format!("/v1/uploads/{upload}:define"), json!({})),
        Route::ObservationsLoad => post("/v1/observations:load".into(), json!({})),
        Route::Mcp => mcp_probes(ids),
        Route::QuestionsMetrics => get("/v1/questions:metrics".into()),
        Route::RunStart => post(
            "/v1/runs".into(),
            json!({"flow": definition, "runId": format!("{run}-x")}),
        ),
        Route::Run => get(format!("/v1/runs/{run}")),
        Route::QuestionsCatalog => get("/v1/questions:catalog".into()),
        Route::Decisions => get("/v1/decisions".into()),
        Route::Decide => post(
            format!("/v1/decisions/{decision}:decide"),
            json!({"choice": "approve", "idempotencyKey": "k-1"}),
        ),
        Route::TransformRows => get(format!("/v1/transforms/{transform}/rows")),
        Route::TransformRowLineage => get(format!("/v1/transforms/{transform}/rows/{row}/lineage")),
        Route::TransformPlan => get(format!("/v1/transforms/{transform}/plan")),
        Route::ObjectExport => get(format!("/v1/objects/{kind}:export")),
        Route::DatasetLoad => post("/v1/datasets:load".into(), json!({"upload": upload})),
        Route::TransformsCompile => post("/v1/transforms:compile".into(), json!({})),
        Route::TransformRefresh => post(format!("/v1/transforms/{transform}:refresh"), json!({})),
        Route::Inbox => get("/v1/inbox".into()),
    }
}

/// The MCP tools and the arguments each row calls it with.
fn mcp_tools(ids: &Ids) -> Vec<(&'static str, Value)> {
    let Ids {
        kind,
        object,
        question,
        action,
        ..
    } = ids;
    let target = json!({"type": kind, "id": object});
    vec![
        ("norn_catalog", json!({})),
        ("norn_inbox", json!({})),
        ("norn_list_definitions", json!({})),
        ("norn_query_objects", json!({"type": kind})),
        ("norn_get_object", target.clone()),
        ("norn_object_links", target.clone()),
        ("norn_trace_object", target.clone()),
        ("norn_profile_type", json!({"type": kind})),
        ("norn_list_held", json!({"type": kind})),
        ("norn_get_findings", target),
        ("norn_explain", json!({"code": "authz.denied"})),
        ("norn_plan_definitions", json!({"artifacts": []})),
        (
            "norn_apply_definitions",
            json!({"artifacts": [], "expectedVersion": 1}),
        ),
        ("norn_question_catalog", json!({})),
        ("norn_list_metrics", json!({})),
        ("norn_answer", json!({"question": question})),
        (
            "norn_trace_answer",
            json!({"question": question, "lineageRef": object, "bucket": 0}),
        ),
        ("norn_evaluate", json!({"question": {}})),
        (
            "norn_trace_evaluated",
            json!({"question": {}, "lineageRef": object, "bucket": 0}),
        ),
        (
            "norn_propose_definition",
            json!({"kind": "dashboard", "id": "studio.dashboard.d1", "payload": {}}),
        ),
        (
            "norn_call_action",
            json!({"action": action, "input": {"id": object}}),
        ),
    ]
}

fn mcp_probes(ids: &Ids) -> Vec<Probe> {
    let rpc = |method: &str, params: Value| {
        (
            "POST",
            "/v1/mcp".to_owned(),
            json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).to_string(),
        )
    };
    let mut rows = vec![
        rpc(
            "initialize",
            json!({"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}),
        ),
        rpc("tools/list", json!({})),
    ];
    for (name, arguments) in mcp_tools(ids) {
        rows.push(rpc(
            "tools/call",
            json!({"name": name, "arguments": arguments}),
        ));
    }
    rows
}

/// Every row of the suite: each route's own rows, the same paths with the
/// other method, and the tenant-level paths outside [`Route`].
fn rows(ids: &Ids) -> Vec<Probe> {
    let mut rows = Vec::new();
    for route in Route::ALL {
        for (method, path, body) in probes(*route, ids) {
            let other = if method == "GET" { "POST" } else { "GET" };
            rows.push((other, path.clone(), body.clone()));
            rows.push((method, path, body));
        }
    }
    rows.push((
        "POST",
        ACCESS_DECIDE_PATH.to_owned(),
        json!({"checks": [{"action": "connector.run", "resource": {"type": "connector", "id": ids.connection}}]})
            .to_string(),
    ));
    rows.push(("GET", ACCESS_DECIDE_PATH.to_owned(), String::new()));
    rows.push(("GET", "/v1/nothing".to_owned(), String::new()));
    rows
}

/// TENANT-ISOLATION-1: the table covers the host's route list, and each row
/// is served by the route it is the row of.
#[test]
fn every_route_has_a_row_that_names_it() {
    let (_, ids) = tenant_b();
    let mut covered = BTreeSet::new();
    for route in Route::ALL {
        let rows = probes(*route, &ids);
        assert!(!rows.is_empty(), "{route:?} has no row");
        for (method, path, _) in rows {
            assert_eq!(Route::of(&path), Some(*route), "{method} {path}");
            assert_eq!(method, route.signature().0, "{route:?}");
        }
        covered.insert(*route);
    }
    assert_eq!(covered.len(), Route::ALL.len());
}

/// Every tool the cell lists to its owner has a row, so a new tool cannot
/// skip the suite.
#[test]
fn every_mcp_tool_has_a_row() {
    let (mut b, ids) = tenant_b();
    let owner = user(&ids.scope.org_id, "owner");
    let listed = send(
        &mut b,
        "POST",
        "/v1/mcp",
        &query(&ids.scope),
        Some(&owner),
        &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {"limit": 100}})
            .to_string(),
    );
    assert_eq!(listed.response.status, 200, "{}", listed.response.body);
    let names: BTreeSet<String> = json_of(&listed.response)["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_owned())
        .collect();
    assert!(!names.is_empty());
    let rows: BTreeSet<String> = mcp_tools(&ids)
        .into_iter()
        .map(|(name, _)| name.to_owned())
        .collect();
    let missing: Vec<&String> = names.difference(&rows).collect();
    assert!(missing.is_empty(), "MCP tools without a row: {missing:?}");
}

// ----------------------------------------------------------------- outsider

/// The principals of another tenant, and of nobody, as B's host sees them.
fn outsiders() -> Vec<Option<String>> {
    let forged = SigningKey::from_bytes(&[9; 32]);
    let b = seed::demo_scope().org_id;
    vec![
        None,
        Some(user("org_a", "owner")),
        Some(user("org_a", "admin")),
        Some(user("org_a", "member")),
        Some(user("org_a", "viewer")),
        Some(signed(&key(), "service", "org_a", "owner")),
        // B's own organization, signed by a key B's issuer never published.
        Some(signed(&forged, "user", &b, "owner")),
        Some("Bearer nonsense".to_owned()),
    ]
}

/// A's principals at B, on every row and query: exactly a vacant scope's
/// answer, at the same cost, with no engine call and none of B's data.
#[test]
fn another_tenants_principal_at_b_learns_nothing() {
    let (mut b, ids) = tenant_b();
    let mut nowhere = vacant(&ids.scope);
    let at_b = query(&ids.scope);
    let queries = [
        at_b.clone(),
        query(&scope("org_a", "prod")),
        String::new(),
        format!("{at_b}&state=pending"),
        format!("{at_b}&limit=zz"),
    ];
    // B's own data, which no outsider's answer may carry.
    let owner = user(&ids.scope.org_id, "owner");
    let own = send(
        &mut b,
        "GET",
        "/v1/objects/canon.Item",
        &at_b,
        Some(&owner),
        "",
    );
    assert_eq!(own.response.status, 200);
    let mut compared = 0;
    for (method, path, body) in rows(&ids) {
        for query in &queries {
            for outsider in outsiders() {
                let served = send(&mut b, method, &path, query, outsider.as_deref(), &body);
                let absent = send(
                    &mut nowhere,
                    method,
                    &path,
                    query,
                    outsider.as_deref(),
                    &body,
                );
                assert_eq!(served, absent, "{method} {path}?{query} as {outsider:?}");
                assert!(
                    !served.engine,
                    "{method} {path}: olapd asked for an outsider"
                );
                for id in ids.private() {
                    assert!(
                        !served.response.body.contains(id),
                        "{method} {path}?{query}: {id} in {}",
                        served.response.body
                    );
                }
                compared += 1;
            }
        }
    }
    assert!(compared > 4000, "{compared}");
    // B's owner still reads B, unchanged by any of it.
    assert_eq!(
        send(
            &mut b,
            "GET",
            "/v1/objects/canon.Item",
            &at_b,
            Some(&owner),
            ""
        )
        .response,
        own.response
    );
    // The engine assertion has teeth: B's owner's profile asks olapd.
    let profiled = send(
        &mut b,
        "GET",
        "/v1/objects/canon.Item:profile",
        &at_b,
        Some(&owner),
        "",
    );
    assert!(profiled.engine, "{:?}", profiled.response);
}

// ------------------------------------------------------------------- inside

/// Inside A's own cell, A's members naming B's scope and ids are answered as
/// if naming a scope and ids that exist nowhere: the same status, the same
/// bytes up to the names themselves, at the same cost.
#[test]
fn a_member_naming_another_tenant_in_its_own_cell_learns_nothing() {
    let (_, b_ids) = tenant_b();
    let scope_a = scope("org_a", "prod");
    let mut a = seeded(&scope_a, None);
    // A member's first request compiles the cell's policies once; the
    // comparisons below measure steady-state work.
    let warm = send(
        &mut a,
        "GET",
        "/v1/definitions",
        &query(&scope_a),
        Some(&user("org_a", "owner")),
        "",
    );
    assert_eq!(warm.response.status, 200, "{}", warm.response.body);
    let elsewhere = b_ids.nowhere();
    let pairs = [(b_ids, elsewhere)];
    let mut compared = 0;
    for role in ["owner", "admin", "member", "viewer"] {
        let member = user("org_a", role);
        for (named, absent) in &pairs {
            let named_rows = rows(named);
            let absent_rows = rows(absent);
            assert_eq!(named_rows.len(), absent_rows.len());
            for ((method, path, body), (_, absent_path, absent_body)) in
                named_rows.iter().zip(&absent_rows)
            {
                // B's scope against one that exists nowhere.
                let to_b = send(
                    &mut a,
                    method,
                    path,
                    &query(&named.scope),
                    Some(&member),
                    body,
                );
                let to_none = send(
                    &mut a,
                    method,
                    absent_path,
                    &query(&absent.scope),
                    Some(&member),
                    absent_body,
                );
                assert_eq!(
                    (
                        to_b.response.status,
                        named.normalized(&to_b.response.body),
                        to_b.calls,
                        to_b.engine
                    ),
                    (
                        to_none.response.status,
                        absent.normalized(&to_none.response.body),
                        to_none.calls,
                        to_none.engine
                    ),
                    "{role}: {method} {path}"
                );
                assert!(
                    !(200..300).contains(&to_b.response.status),
                    "{role}: {method} {path}: {}",
                    to_b.response.body
                );
                compared += 1;
            }
            // B's ids under A's own scope, on the reads (a write under A's
            // own scope would change A): as ids that exist nowhere.
            for ((method, path, body), (_, absent_path, absent_body)) in
                named_rows.iter().zip(&absent_rows)
            {
                let read = *method == "GET" || path.ends_with(":decide");
                // Only the rows that name one of B's ids say anything here.
                let names_an_id = path != absent_path || body != absent_body;
                if !read || !names_an_id || Route::of(path).is_none() {
                    continue;
                }
                let at_a = query(&scope_a);
                let b_id = send(&mut a, method, path, &at_a, Some(&member), body);
                let no_id = send(
                    &mut a,
                    method,
                    absent_path,
                    &at_a,
                    Some(&member),
                    absent_body,
                );
                let unscoped = |ids: &Ids, text: &str| ids.normalized(text);
                assert_eq!(
                    (
                        b_id.response.status,
                        unscoped(named, &b_id.response.body),
                        b_id.calls,
                    ),
                    (
                        no_id.response.status,
                        unscoped(absent, &no_id.response.body),
                        no_id.calls,
                    ),
                    "{role}: {method} {path} under A's scope"
                );
                for id in [&named.decision, &named.run] {
                    assert!(!b_id.response.body.contains(id.as_str()), "{role}: {path}");
                }
                compared += 1;
            }
        }
    }
    assert!(compared > 400, "{compared}");
}
