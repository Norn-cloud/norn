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
//! - the same token verifications (S2), and no storage statement at all;
//! - no engine call (olapd is never asked for an outsider), no continuation
//!   or sync left for the host, and no deadline moved (S8);
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

// #1485 S6: fixtures write through the kernel path, never the raw door.
#[path = "../../norn-cell/tests/support/governed.rs"]
mod governed;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signer, SigningKey};
use norn_api::scope::CellScope;
use norn_cell::dataset_manifest::{
    DatasetManifest, MANIFEST_VERSION, ManifestCounts, ManifestFile, ManifestSource,
};
use norn_cell::engine::{NativeEngine, ReadIsolation, SqlEngine, SqlValue};
use norn_cell::{CellError, CommitBatch, FactWriteRow, MAIN_WORLD};
use norn_cell_do::analytics::{self, DemoQuestions};
use norn_cell_do::connect::Connect;
use norn_cell_do::policy::{CallerSlot, DEMO_GRANTS};
use norn_cell_do::principal::{PrincipalResolver, TokenPrincipals};
use norn_cell_do::seed;
use norn_cell_do::server::{Request, Response, Route};
use norn_cell_do::tenant::{ACCESS_DECIDE_PATH, Gate, Served, Tenant};
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
    fn definition_writes<T>(
        &self,
        f: impl FnOnce() -> Result<T, CellError>,
    ) -> Result<T, CellError> {
        self.inner.definition_writes(f)
    }

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
    let sub = format!("{role}-of-{org}");
    let mut norn = json!({ "kind": kind, "org": org, "role": role, "amr": amr });
    // As Studio issues it: a user's token names the person accountable for it.
    if kind == "user" {
        norn["accountablePerson"] = json!(sub);
    }
    let claims = json!({ "iss": ISSUER, "aud": "norn-host", "sub": sub,
            "iat": NOW - 10, "exp": NOW + 290, "norn": norn });
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

/// The host's token resolver, counting every verification a request asks
/// for (S2: a foreign token is verified as often at B as at a vacant scope).
struct Counted {
    inner: TokenPrincipals,
    resolves: Cell<u64>,
}

impl PrincipalResolver for Counted {
    fn resolve(&self, authorization: Option<&str>, now: u64) -> Option<String> {
        self.resolves.set(self.resolves.get() + 1);
        self.inner.resolve(authorization, now)
    }
    fn end_request(&self) {
        self.inner.end_request();
    }
    fn caller(&self) -> Option<&CallerSlot> {
        self.inner.caller()
    }
}

// ------------------------------------------------------------------- cells

/// One scope's cell as the deployed host serves it: its tenant, token
/// resolver and source/upload routes, with the analytics engine enabled.
struct Host {
    tenant: Tenant<Counting>,
    tokens: Counted,
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
    /// Token verifications the request asked for (S2).
    resolves: u64,
    /// Whether it left work for the host to continue: a fetch to make and
    /// resume, or a sync to run (S8).
    continued: bool,
    /// Whether the cell's next deadline moved: an alarm the host would arm (S8).
    scheduled: bool,
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
        "NORN_S3_ACCESS_KEY_ID": "key-id",
        "NORN_S3_SECRET_ACCESS_KEY": "secret",
    });
    Connect::new(&scope("org_a", "prod"), "[]", &vars.to_string())
        .unwrap_or_else(|error| unreachable!("the configuration is well formed: {error}"))
}

fn host(tenant: Tenant<Counting>, calls: Rc<Cell<u64>>) -> Host {
    let mut tenant = tenant.with_receipt_handles(Some(&LINEAGE_KEY)).unwrap();
    // OLAP-CRED-1: the engine calls carry this cell's own credential.
    tenant.enable_analytics(
        norn_api::olap_credential::Issuer::parse(&format!(
            "k1:{}",
            URL_SAFE_NO_PAD.encode([7_u8; 32])
        ))
        .unwrap(),
        norn_cell::analytics::AnalyticalEngineId::DuckDb,
    );
    let tokens = Counted {
        inner: TokenPrincipals::new(
            ISSUER,
            &jwks(),
            tenant.scope().clone(),
            tenant.caller(),
            NOW,
        )
        .unwrap(),
        resolves: Cell::new(0),
    };
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
        store = governed::commit_owned(
            store,
            &CommitBatch {
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
            },
        )
        .unwrap()
        .0;
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
    drop(host.tenant.take_engine_call(NOW * 1000));
    let due = host.tenant.next_due().unwrap();
    host.calls.set(0);
    host.tokens.resolves.set(0);
    let request = Request {
        method,
        path,
        query,
        authorization: auth,
        now: NOW,
        body,
    };
    let (response, continued) =
        match host
            .tenant
            .serve(Some(&mut host.connect), &host.tokens, &request, NOW * 1000)
        {
            Served::Connect(routed) => (
                routed.response,
                routed.then.is_some() || routed.start_sync.is_some(),
            ),
            Served::Cell(response) => (response, false),
        };
    let calls = host.calls.get();
    let resolves = host.tokens.resolves.get();
    let response = Response {
        status: response.status,
        body: without_incident_ids(&response.body),
    };
    Answer {
        response,
        calls,
        engine: host.tenant.take_engine_call(NOW * 1000).is_some(),
        resolves,
        continued,
        scheduled: host.tenant.next_due().unwrap() != due,
    }
}

/// `body` with each incident id (`inc-<clock>-<sequence>`, INCIDENT-1)
/// replaced by a marker: an id is fresh per internal refusal whatever the
/// scope, so it names the request, never the tenant.
fn without_incident_ids(body: &str) -> String {
    let mut out = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(at) = rest.find("\"inc-") {
        let (head, tail) = rest.split_at(at + 1);
        out.push_str(head);
        let end = tail.find('"').unwrap_or(tail.len());
        out.push_str("<incident>");
        rest = tail.get(end..).unwrap_or_default();
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
        Route::DecisionsRequested => get("/v1/decisions:requested".into()),
        Route::Decide => post(
            format!("/v1/decisions/{decision}:decide"),
            json!({"choice": "approve", "idempotencyKey": "k-1"}),
        ),
        Route::TransformRows => get(format!("/v1/transforms/{transform}/rows")),
        Route::TransformRowLineage => get(format!("/v1/transforms/{transform}/rows/{row}/lineage")),
        Route::TransformPlan => get(format!("/v1/transforms/{transform}/plan")),
        Route::ObjectExport => get(format!("/v1/objects/{kind}:export")),
        Route::DatasetLoad => post("/v1/datasets:load".into(), json!({"upload": upload})),
        Route::DatasetLineage => get(format!("/v1/datasets/{kind}/rows/1:lineage")),
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
        assert_ne!(rows, [], "{route:?} has no row");
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
    assert_ne!(names.len(), 0);
    let rows: BTreeSet<String> = mcp_tools(&ids)
        .into_iter()
        .map(|(name, _)| name.to_owned())
        .collect();
    let missing: Vec<&String> = names.difference(&rows).collect();
    assert_eq!(missing.len(), 0, "MCP tools without a row: {missing:?}");
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
                assert!(
                    !served.continued && !served.scheduled,
                    "{method} {path}: an outsider left the host work"
                );
                // S2: the token is verified once, at the gate, and no
                // storage is touched for an outsider at all.
                assert_eq!(served.calls, 0, "{method} {path}?{query}");
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

// --------------------------------------------------- live data, and the gate

/// The analytics cell as the deployed host builds it (its K1 rows, its
/// published metrics), with a loaded dataset recorded: a cell whose live
/// runtimes would shape an answer if any reached an outsider.
fn analytics_b() -> Host {
    let scope = analytics::analytics_scope();
    let calls = Rc::new(Cell::new(0));
    let analytics::Analytics {
        mut store,
        plan,
        definitions,
        catalog,
    } = analytics::provision(Counting::new(&calls), &scope).unwrap();
    // A loaded dataset is a committed manifest (#1381 MANIFEST-COMMIT-1).
    let table = store.analytics_table(analytics::TYPE).unwrap();
    let binding = BTreeMap::new();
    let (data, held) = ("ab".repeat(32), "cd".repeat(32));
    let file = |sha256: &str, rows| ManifestFile {
        uri: format!("s3://uploads-bucket/uploads/org_demo%2Fanalytics/{sha256}.parquet"),
        sha256: sha256.to_owned(),
        rows,
        bytes: 1,
    };
    let manifest = DatasetManifest {
        version: MANIFEST_VERSION.into(),
        object_type: analytics::TYPE.into(),
        load: norn_cell::analytics::dataset_cache_id(&table, &data, &held),
        source: ManifestSource {
            uri: "s3://uploads-bucket/uploads/org_demo%2Fanalytics/b-dataset.csv".into(),
            sha256: "ef".repeat(32),
            format: norn_cell::analytics::FileFormat::Csv,
            bytes: 1,
        },
        pins: store
            .dataset_pins(analytics::TYPE, &binding, "b.mapping@1")
            .unwrap(),
        binding,
        table,
        data: file(&data, 3),
        held: file(&held, 0),
        counts: ManifestCounts {
            source: 3,
            admitted: 3,
            held: BTreeMap::new(),
            findings: BTreeMap::new(),
            held_by_rules: 0,
        },
        valid_time: Vec::new(),
        markings: BTreeMap::new(),
    };
    store
        .commit_dataset_manifest(
            MAIN_WORLD,
            &scope.key(),
            norn_cell::provision::K1_CLOCK,
            &manifest,
            None,
        )
        .unwrap();
    assert_ne!(definitions, [], "the fixture publishes metrics");
    let questions = DemoQuestions::new(plan, scope.clone())
        .try_with_handle_key(Some(&LINEAGE_KEY))
        .unwrap()
        .with_metrics(definitions, catalog);
    let tenant = Tenant::seeded(scope, &DEMO_GRANTS, store, Some(questions)).unwrap();
    host(tenant, calls)
}

/// S8: at a cell that publishes metrics and holds a loaded dataset, the owner
/// sees both (the fixture is live), and every outsider is answered as at a
/// vacant scope, with no storage, engine call, continuation or alarm.
#[test]
fn an_outsider_at_a_cell_with_metrics_and_a_dataset_learns_nothing() {
    let mut b = analytics_b();
    let scope = analytics::analytics_scope();
    let mut nowhere = vacant(&scope);
    let at_b = query(&scope);
    let owner = user(&scope.org_id, "owner");
    // Positive controls: the owner lists the published metrics, and the
    // dataset's grid is the engine's (only a loaded dataset routes there).
    let metrics = send(
        &mut b,
        "GET",
        "/v1/questions:metrics",
        &at_b,
        Some(&owner),
        "",
    );
    assert_eq!(metrics.response.status, 200, "{}", metrics.response.body);
    let listed = json_of(&metrics.response)["metrics"].clone();
    assert_ne!(listed.as_array().unwrap().len(), 0, "{listed}");
    let grid = send(
        &mut b,
        "GET",
        &format!("/v1/objects/{}", analytics::TYPE),
        &at_b,
        Some(&owner),
        "",
    );
    assert!(
        grid.engine,
        "the dataset is not loaded: {:?}",
        grid.response
    );
    let ids = Ids {
        scope,
        kind: analytics::TYPE.into(),
        object: "k1-row-1".into(),
        decision: "decision-b-1".into(),
        run: "run-b-1".into(),
        definition: "demo.metrics".into(),
        action: "canon.rename".into(),
        question: analytics::QUESTION.into(),
        transform: "ops.daily".into(),
        row: "row-1".into(),
        guard: "guard-b-1".into(),
        upload: "upload-b-1".into(),
        connection: "acme.main".into(),
    };
    let mut compared = 0;
    for (method, path, body) in rows(&ids) {
        for query in [at_b.clone(), String::new()] {
            for outsider in outsiders() {
                let served = send(&mut b, method, &path, &query, outsider.as_deref(), &body);
                let absent = send(
                    &mut nowhere,
                    method,
                    &path,
                    &query,
                    outsider.as_deref(),
                    &body,
                );
                assert_eq!(served, absent, "{method} {path}?{query} as {outsider:?}");
                assert_eq!(served.calls, 0, "{method} {path}");
                assert!(!served.engine && !served.continued && !served.scheduled);
                assert!(!served.response.body.contains("b-dataset"));
                assert!(!served.response.body.contains(&"ab".repeat(32)));
                compared += 1;
            }
        }
    }
    assert!(compared > 1000, "{compared}");
}

/// S1: the storage-free gate the Worker asks before it opens a cell answers
/// every outsider exactly as the cell does (and a vacant scope does), and
/// lets a member through.
#[test]
fn the_storage_free_gate_answers_outsiders_as_the_cell_does() {
    let (mut b, ids) = tenant_b();
    let gate = Gate::new(ids.scope.clone(), &DEMO_GRANTS, Some(&LINEAGE_KEY), true).unwrap();
    let tokens = Counted {
        inner: TokenPrincipals::new(ISSUER, &jwks(), ids.scope.clone(), gate.caller(), NOW)
            .unwrap(),
        resolves: Cell::new(0),
    };
    let at_b = query(&ids.scope);
    let mut compared = 0;
    for (method, path, body) in rows(&ids) {
        for outsider in outsiders() {
            let request = Request {
                method,
                path: &path,
                query: &at_b,
                authorization: outsider.as_deref(),
                now: NOW,
                body: &body,
            };
            tokens.resolves.set(0);
            let gated = gate
                .outsider::<Counting>(&tokens, &request)
                .unwrap_or_else(|| panic!("{method} {path}: an outsider passed the gate"));
            let served = send(&mut b, method, &path, &at_b, outsider.as_deref(), &body);
            assert_eq!(
                (
                    gated.status,
                    without_incident_ids(&gated.body),
                    tokens.resolves.get()
                ),
                (
                    served.response.status,
                    served.response.body,
                    served.resolves
                ),
                "{method} {path} as {outsider:?}"
            );
            compared += 1;
        }
    }
    assert!(compared > 500, "{compared}");
    let owner = user(&ids.scope.org_id, "owner");
    let request = Request {
        method: "GET",
        path: "/v1/objects/canon.Item",
        query: &at_b,
        authorization: Some(&owner),
        now: NOW,
        body: "",
    };
    assert!(gate.outsider::<Counting>(&tokens, &request).is_none());
}

// ----------------------------------------------------- a parked call by id

/// A hosted cell of `scope`, provisioned as the deployed host provisions one
/// (the trusted base and the tenant's own set): a cell an action can park on.
fn provisioned(scope: &CellScope) -> Host {
    let mut cell = vacant(scope);
    let set: Vec<Value> = serde_json::from_str::<Vec<Value>>(include_str!(
        "../../../fixtures/demo/explorer/three-systems.definitions.json"
    ))
    .unwrap()
    .into_iter()
    .filter(|artifact| !norn_api::kernel_set::is_reserved(artifact["artifactId"].as_str().unwrap()))
    .collect();
    let body = json!({ "baseSha256": norn_cell_do::base::trusted_base().unwrap().sha256(),
        "artifacts": set });
    let provisioned = send(
        &mut cell,
        "POST",
        norn_api::provision::PROVISION_PATH,
        &query(scope),
        Some(&user(&scope.org_id, "owner")),
        &body.to_string(),
    );
    assert_eq!(
        provisioned.response.status, 200,
        "{}",
        provisioned.response.body
    );
    cell
}

/// B with one direct parked call: the batches pack applied with an approval
/// (deciders `owner`) gating its record action, and B's member calls it.
/// Its decision id.
fn park_direct_call(b: &mut Host, scope: &CellScope) -> String {
    const RECORD: &str = "lab.batch.record";
    let at = query(scope);
    let owner = user(&scope.org_id, "owner");
    let artifacts: Vec<Value> = [
        include_str!("../../../fixtures/packs/batches/artifacts/lab.Batch.entity.json"),
        include_str!("../../../fixtures/packs/batches/artifacts/lab.batch.access.policy.json"),
        include_str!("../../../fixtures/packs/batches/artifacts/lab.batch.mass_conserved.law.json"),
        include_str!("../../../fixtures/packs/batches/artifacts/lab.batch.record.action.json"),
    ]
    .iter()
    .map(|text| serde_json::from_str(text).unwrap())
    .collect();
    let mut gate = json!({ "kind": "approval", "id": "lab.batch.record_approval", "payload": {
        "action": RECORD, "decider": { "roles": ["owner"] },
        "question": "Record this batch?", "risk": "high" } });
    let change = |gate: &Value, expected: Option<u64>| {
        let mut body = json!({ "artifacts": artifacts, "approvals": [gate] });
        if let Some(version) = expected {
            body["expectedVersion"] = json!(version);
        }
        body.to_string()
    };
    let planned = send(
        b,
        "POST",
        norn_api::cell_definitions::PLAN_PATH,
        &at,
        Some(&owner),
        &change(&gate, None),
    );
    assert_eq!(planned.response.status, 200, "{}", planned.response.body);
    let plan = json_of(&planned.response);
    gate["signoff"] = json!({ "restatementSha256": plan["approvals"][0]["restatementSha256"] });
    let catalog = send(b, "GET", "/v1/definitions", &at, Some(&owner), "");
    let version = json_of(&catalog.response)["version"].as_u64().unwrap();
    let applied = send(
        b,
        "POST",
        norn_api::cell_definitions::APPLY_PATH,
        &at,
        Some(&owner),
        &change(&gate, Some(version)),
    );
    assert_eq!(applied.response.status, 200, "{}", applied.response.body);
    let call = json!({ "input": { "id": "b-parked-1", "material": "copper", "input_g": "100.000",
        "output_g": "98.000", "loss_g": "2.000", "loss_reason": null },
        "idempotencyKey": "b-parked-1" });
    let parked = send(
        b,
        "POST",
        &format!("/v1/actions/{RECORD}"),
        &at,
        Some(&user(&scope.org_id, "member")),
        &call.to_string(),
    );
    assert_eq!(parked.response.status, 202, "{}", parked.response.body);
    json_of(&parked.response)["decision"]["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// #1532 S5: B's direct parked call, named by its id. Its requester reads
/// it (the control). Every outsider at B is answered as at a vacant scope,
/// with no storage touched; B's other members, and A's members in A's own
/// cell, read no item, at exactly the cost of an id that exists nowhere.
#[test]
fn a_parked_call_by_id_is_its_requesters_alone() {
    let ids = tenant_b().1;
    let mut b = provisioned(&ids.scope);
    let parked = park_direct_call(&mut b, &ids.scope);
    let at_b = query(&ids.scope);
    let path = "/v1/decisions:requested";
    let named = format!("{at_b}&id={parked}");
    let absent = format!("{at_b}&id={}", "z".repeat(parked.len()));
    let requester = user(&ids.scope.org_id, "member");
    let own = send(&mut b, "GET", path, &named, Some(&requester), "");
    assert_eq!(own.response.status, 200, "{}", own.response.body);
    assert_eq!(json_of(&own.response)["items"][0]["id"], json!(parked));

    let mut nowhere = vacant(&ids.scope);
    for outsider in outsiders() {
        let served = send(&mut b, "GET", path, &named, outsider.as_deref(), "");
        let vacant = send(&mut nowhere, "GET", path, &named, outsider.as_deref(), "");
        assert_eq!(served, vacant, "{outsider:?}");
        assert_eq!(served.calls, 0, "{outsider:?}");
        assert!(!served.response.body.contains(&parked), "{outsider:?}");
    }
    // While it waits, and once B's owner approved it (its payload then
    // reads through the resolved decision).
    for phase in ["pending", "approved"] {
        for role in ["owner", "admin", "viewer"] {
            let member = user(&ids.scope.org_id, role);
            // Warm: a first request compiles the member's policies once.
            send(&mut b, "GET", path, &absent, Some(&member), "");
            let by_id = send(&mut b, "GET", path, &named, Some(&member), "");
            let no_id = send(&mut b, "GET", path, &absent, Some(&member), "");
            assert_eq!(
                (by_id.response.status, by_id.calls),
                (no_id.response.status, no_id.calls),
                "{phase}: {role}"
            );
            assert_eq!(
                json_of(&by_id.response)["items"],
                json!([]),
                "{phase}: {role}"
            );
            assert!(!by_id.response.body.contains(&parked), "{phase}: {role}");
        }
        if phase == "pending" {
            let decided = send(
                &mut b,
                "POST",
                &format!("/v1/decisions/{parked}:decide"),
                &at_b,
                Some(&user(&ids.scope.org_id, "owner")),
                &json!({ "choice": "approve", "idempotencyKey": "b-approve-1" }).to_string(),
            );
            assert_eq!(decided.response.status, 200, "{}", decided.response.body);
            let own = send(&mut b, "GET", path, &named, Some(&requester), "");
            assert_eq!(
                json_of(&own.response)["items"][0]["canRun"],
                json!(true),
                "{}",
                own.response.body
            );
        }
    }
    let scope_a = scope("org_a", "prod");
    let mut a = seeded(&scope_a, None);
    let at_a = query(&scope_a);
    let in_a = |id: &str| format!("{at_a}&id={id}");
    for role in ["owner", "admin", "member", "viewer"] {
        let member = user("org_a", role);
        send(
            &mut a,
            "GET",
            path,
            &in_a(&"z".repeat(parked.len())),
            Some(&member),
            "",
        );
        let by_id = send(&mut a, "GET", path, &in_a(&parked), Some(&member), "");
        let no_id = send(
            &mut a,
            "GET",
            path,
            &in_a(&"z".repeat(parked.len())),
            Some(&member),
            "",
        );
        assert_eq!(
            (by_id.response.status, by_id.calls),
            (no_id.response.status, no_id.calls),
            "{role}"
        );
        assert!(!by_id.response.body.contains(&parked), "{role}");
    }
}

/// #1486 (Two-World Non-Interference): incident identifiers returned to
/// callers are 128-bit random tokens (`inc-<32 lowercase hex>`) with no
/// isolate-wide sequence counter or clock. Triggering internal errors for
/// Tenant B between two Tenant A errors changes nothing distinguishable in
/// Tenant A's incident format or numbering, while the operator log still
/// records each incident with its monotonic `(seq N)`.
#[test]
fn incident_ids_do_not_leak_another_tenants_errors() {
    // #1710: this thread opts in to keeping its incident lines.
    norn_cell_do::incident::capture();
    let parse_opaque = |response: &Response| -> String {
        assert_eq!(response.status, 500, "{}", response.body);
        let doc = json_of(response);
        let id = doc["incidentId"]
            .as_str()
            .expect("internal refusal carries incidentId")
            .to_owned();
        let hex = id
            .strip_prefix("inc-")
            .unwrap_or_else(|| panic!("expected inc- prefix: {id}"));
        assert_eq!(
            hex.len(),
            32,
            "incident id must be 128-bit lowercase hex without a sequence suffix: {id}"
        );
        assert!(
            hex.bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')),
            "incident id must be lowercase hex only: {id}"
        );
        assert!(
            !hex.contains('-'),
            "incident id must not expose a counter separator: {id}"
        );
        id
    };

    let internal = |cause: &str| norn_api::refusal::Refusal::host(cause);

    // World 1: 17 Tenant B internal errors occur between Tenant A's two errors.
    let a1 = parse_opaque(&norn_cell_do::incident::respond(internal("tenant-a-1")));
    for index in 0..17 {
        let b = parse_opaque(&norn_cell_do::incident::respond(internal(&format!(
            "tenant-b-{index}"
        ))));
        assert_ne!(b, a1);
    }
    let a2 = parse_opaque(&norn_cell_do::incident::respond(internal("tenant-a-2")));

    // World 2: 0 Tenant B internal errors occur between Tenant A's two errors.
    let a3 = parse_opaque(&norn_cell_do::incident::respond(internal("tenant-a-3")));
    let a4 = parse_opaque(&norn_cell_do::incident::respond(internal("tenant-a-4")));

    let all: std::collections::BTreeSet<&str> =
        [a1.as_str(), a2.as_str(), a3.as_str(), a4.as_str()]
            .into_iter()
            .collect();
    assert_eq!(all.len(), 4, "incident ids must be distinct: {all:?}");

    // Operator logs retain `(seq N)` ordering strictly server-side.
    let logged = norn_cell_do::incident::recent();
    for id in [&a1, &a2, &a3, &a4] {
        let line = logged
            .iter()
            .find(|line| line.contains(id.as_str()))
            .unwrap_or_else(|| panic!("missing log line for {id}"));
        assert!(
            line.contains(" (seq "),
            "server log must retain per-isolate sequence for operators: {line}"
        );
    }
}
