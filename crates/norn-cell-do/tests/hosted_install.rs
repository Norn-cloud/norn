//! HOST-INSTALL-1: a running tenant cell takes a pack through
//! `POST /v1/cell/definitions:plan` and `:apply`, on the same [`Tenant`] the
//! Durable Object binding drives.
//!
//! End to end on a provisioned cell, with the batches fixture pack: a broken
//! pack is refused with the checker's diagnostic and changes nothing;
//! a member may not apply; the owner's plan names the new type; an admin's
//! apply installs it as the next revision with a receipt; a batch that loses
//! mass is refused and a correct one accepted; a restart reopens the cell at
//! the applied revision; and a law change the stored batches break is held back
//! with findings naming them.
//!
//! These pin decisions of the server's permission and admission system that
//! no existing property covers (D79).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::too_many_lines,
    clippy::arithmetic_side_effects,
    reason = "hosted install correspondence"
)]

use std::path::Path;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signer, SigningKey};
use norn_api::cell_definitions::{APPLY_PATH, PLAN_PATH};
use norn_api::provision::PROVISION_PATH;
use norn_api::scope::CellScope;
use norn_cell::engine::{NativeEngine, ReadIsolation, SqlEngine, SqlValue};
use norn_cell::{CellError, StoreOptions};
use norn_cell_do::base;
use norn_cell_do::policy::DEMO_GRANTS;
use norn_cell_do::principal::TokenPrincipals;
use norn_cell_do::server::{Request, Response};
use norn_cell_do::tenant::{OpenEngine, Tenant};
use serde_json::{Value, json};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

const ISSUER: &str = "https://studio-preview.example";
const NOW: u64 = 1_800_000_000;
const HANDLE_KEY: [u8; 32] = [7; 32];
const QUERY: &str = "orgId=org_a&environmentId=prod";
const BATCH: &str = "lab.Batch";
const RECORD: &str = "lab.batch.record";
const RECORDED: &str =
    include_str!("../../../fixtures/demo/explorer/three-systems.definitions.json");
const BATCHES: [&str; 4] = [
    include_str!("../../../fixtures/packs/batches/artifacts/lab.Batch.entity.json"),
    include_str!("../../../fixtures/packs/batches/artifacts/lab.batch.access.policy.json"),
    include_str!("../../../fixtures/packs/batches/artifacts/lab.batch.mass_conserved.law.json"),
    include_str!("../../../fixtures/packs/batches/artifacts/lab.batch.record.action.json"),
];

fn key() -> SigningKey {
    SigningKey::from_bytes(&[1; 32])
}

fn jwks() -> String {
    json!({ "keys": [{ "kty": "OKP", "crv": "Ed25519", "alg": "EdDSA", "kid": "k1",
                       "x": URL_SAFE_NO_PAD.encode(key().verifying_key().as_bytes()) }] })
    .to_string()
}

fn token(org: &str, role: &str) -> String {
    let claims = json!({ "iss": ISSUER, "aud": "norn-host", "sub": format!("{role}-of-{org}"),
            "iat": NOW - 10, "exp": NOW + 290,
            "norn": { "kind": "user", "org": org, "role": role, "amr": "password" } });
    let head = URL_SAFE_NO_PAD.encode(json!({ "alg": "EdDSA", "kid": "k1" }).to_string());
    let body = URL_SAFE_NO_PAD.encode(claims.to_string());
    let signature = key().sign(format!("{head}.{body}").as_bytes());
    format!(
        "Bearer {head}.{body}.{}",
        URL_SAFE_NO_PAD.encode(signature.to_bytes())
    )
}

fn user(role: &str) -> String {
    token("org_a", role)
}

fn scope() -> CellScope {
    CellScope {
        org_id: "org_a".into(),
        environment_id: "prod".into(),
    }
}

/// The three-systems tenant set: the canon the batches pack specializes.
fn tenant_set() -> Vec<Value> {
    let artifacts: Vec<Value> = serde_json::from_str(RECORDED).unwrap();
    artifacts
        .into_iter()
        .filter(|artifact| {
            !norn_api::kernel_set::is_reserved(artifact["artifactId"].as_str().unwrap())
        })
        .collect()
}

/// The batches pack as the registry carries it: its action callable on a
/// tenant cell through the pack's own access policy.
fn batches_pack() -> Vec<Value> {
    BATCHES
        .iter()
        .map(|text| serde_json::from_str(text).unwrap())
        .collect()
}

/// The pack with its law's predicate replaced by `expression`.
fn with_law(expression: &str) -> Vec<Value> {
    let mut artifacts = batches_pack();
    artifacts[2]["payload"]["response"]["expression"] = json!(expression);
    artifacts
}

struct Cell<E: SqlEngine = NativeEngine> {
    tenant: Tenant<E>,
    tokens: TokenPrincipals,
}

impl Cell {
    fn opened(path: &Path) -> Self {
        let file = path.to_owned();
        Self::over(Box::new(move || {
            NativeEngine::open(&file, StoreOptions::default()).map_err(|error| error.to_string())
        }))
    }
}

impl<E: SqlEngine> Cell<E> {
    fn over(open: OpenEngine<E>) -> Self {
        let tenant = Tenant::open(scope(), &DEMO_GRANTS, open, Some(&HANDLE_KEY)).unwrap();
        let tokens = TokenPrincipals::new(ISSUER, &jwks(), scope(), tenant.caller(), NOW).unwrap();
        Self { tenant, tokens }
    }

    fn call(&mut self, method: &str, path: &str, auth: Option<&str>, body: &str) -> Response {
        self.tenant.handle(
            &self.tokens,
            &Request {
                method,
                path,
                query: QUERY,
                authorization: auth,
                now: NOW,
                body,
            },
        )
    }

    fn change(
        &mut self,
        path: &str,
        role: Option<&str>,
        artifacts: &[Value],
        expected: Option<u64>,
    ) -> Response {
        let mut body = json!({ "artifacts": artifacts });
        if let Some(version) = expected {
            body["expectedVersion"] = json!(version);
        }
        let auth = role.map(user);
        self.call("POST", path, auth.as_deref(), &body.to_string())
    }

    fn version(&mut self) -> u64 {
        let read = self.call("GET", "/v1/definitions", Some(&user("admin")), "");
        assert_eq!(read.status, 200, "{}", read.body);
        json_of(&read)["version"].as_u64().unwrap()
    }

    fn batch(&mut self, id: &str, input: &str, output: &str, loss: &str) -> Response {
        let body = json!({ "input": { "id": id, "material": "copper", "input_g": input,
            "output_g": output, "loss_g": loss, "loss_reason": null },
            "idempotencyKey": id });
        self.call(
            "POST",
            &format!("/v1/actions/{RECORD}"),
            Some(&user("member")),
            &body.to_string(),
        )
    }

    fn batches(&mut self) -> Response {
        self.call(
            "GET",
            &format!("/v1/objects/{BATCH}"),
            Some(&user("admin")),
            "",
        )
    }
}

fn json_of(response: &Response) -> Value {
    serde_json::from_str(&response.body).unwrap()
}

fn provisioned(path: &Path) -> Cell {
    provision(Cell::opened(path))
}

fn provision<E: SqlEngine>(mut cell: Cell<E>) -> Cell<E> {
    let body =
        json!({ "baseSha256": base::trusted_base().unwrap().sha256(), "artifacts": tenant_set() });
    let response = cell.call(
        "POST",
        PROVISION_PATH,
        Some(&user("owner")),
        &body.to_string(),
    );
    assert_eq!(response.status, 200, "{}", response.body);
    cell
}

#[test]
fn a_pack_is_planned_and_applied_on_a_running_cell() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cell.sqlite");
    let mut cell = provisioned(&path);
    // Provisioning installs the cell's own sources pack after its set (#1358).
    let base = cell.version();
    // Before the pack, the cell serves no batches.
    assert_ne!(cell.batches().status, 200);

    // A broken pack: the law reads a property the Batch does not have. Plan
    // and apply refuse it with the checker's diagnostic, placed in the
    // submission; nothing changes.
    let broken = with_law("self.input_g == self.output_g + self.loss_kg");
    let planned = cell.change(PLAN_PATH, Some("owner"), &broken, None);
    assert_eq!(planned.status, 422, "{}", planned.body);
    let refusal = json_of(&planned);
    assert_eq!(refusal["code"], "defs.refused");
    let diagnostics = refusal["diagnostics"].as_array().unwrap();
    assert!(!diagnostics.is_empty(), "{refusal}");
    assert!(
        diagnostics.iter().any(|d| d["span"]["pointer"]
            .as_str()
            .is_some_and(|pointer| pointer.starts_with("/artifacts/2"))),
        "the finding names the broken law: {refusal}"
    );
    let applied = cell.change(APPLY_PATH, Some("owner"), &broken, Some(base));
    assert_eq!(applied.status, 422);
    assert_eq!(json_of(&applied)["diagnostics"], refusal["diagnostics"]);
    assert_eq!(cell.version(), base);

    // Only a definition writer (owner, admin) plans or applies; anyone else
    // gets the one opaque denial, whatever the body.
    let denied = cell.change(APPLY_PATH, None, &batches_pack(), Some(base));
    assert_eq!(denied.status, 403);
    for role in ["member", "viewer"] {
        for route in [PLAN_PATH, APPLY_PATH] {
            let response = cell.change(route, Some(role), &batches_pack(), Some(base));
            assert_eq!(response, denied, "{role} {route}");
        }
    }
    let other_org = cell.call(
        "POST",
        APPLY_PATH,
        Some(&token("org_b", "owner")),
        &json!({ "artifacts": batches_pack(), "expectedVersion": 1 }).to_string(),
    );
    assert_eq!(other_org, denied);
    assert_eq!(cell.version(), base);

    // The owner's plan: four artifacts added, the Batch a new type.
    let planned = cell.change(PLAN_PATH, Some("owner"), &batches_pack(), None);
    assert_eq!(planned.status, 200, "{}", planned.body);
    let plan = json_of(&planned);
    assert_eq!(plan["baseVersion"], base);
    assert_eq!(plan["nextVersion"], base + 1);
    assert_eq!(plan["unchanged"], false);
    assert_eq!(plan["added"].as_array().unwrap().len(), 4, "{plan}");
    assert_eq!(plan["newObjectTypes"], json!([BATCH]));
    assert_eq!(cell.version(), base, "a plan writes nothing");

    // Apply needs the planned revision; an admin applies it.
    let unplanned = cell.change(APPLY_PATH, Some("admin"), &batches_pack(), None);
    assert_eq!(unplanned.status, 400);
    let stale = cell.change(APPLY_PATH, Some("admin"), &batches_pack(), Some(base - 1));
    assert_eq!(json_of(&stale)["code"], "defs.stale");
    let applied = cell.change(APPLY_PATH, Some("admin"), &batches_pack(), Some(base));
    assert_eq!(applied.status, 200, "{}", applied.body);
    let receipt = json_of(&applied);
    assert_eq!(receipt["outcome"], "installed");
    assert_eq!(receipt["definitionsVersion"], base + 1);
    assert_eq!(receipt["receiptSha256"], plan["receiptSha256"]);
    assert_eq!(cell.version(), base + 1);
    // The same apply again installs nothing.
    let again = cell.change(APPLY_PATH, Some("admin"), &batches_pack(), Some(base + 1));
    assert_eq!(json_of(&again)["outcome"], "unchanged", "{}", again.body);
    assert_eq!(cell.version(), base + 1);

    // The new type is served at once, and its action is callable: a batch
    // that loses mass is refused by the law, a correct one is accepted.
    assert_eq!(cell.batches().status, 200);
    let lossy = cell.batch("m1", "100.000", "98.000", "0.000");
    assert!(lossy.status >= 400, "{}", lossy.body);
    assert_eq!(
        json_of(&cell.batches())["records"].as_array().map(Vec::len),
        Some(0)
    );
    let correct = cell.batch("m2", "100.000", "98.000", "2.000");
    assert_eq!(correct.status, 200, "{}", correct.body);
    assert_eq!(json_of(&correct)["outcome"], "committed");
    assert_eq!(
        json_of(&cell.batches())["records"].as_array().map(Vec::len),
        Some(1)
    );

    // HOST-REOPEN-1: a restarted host reopens the cell at the applied
    // revision, with the batch and its law.
    drop(cell);
    let mut cell = Cell::opened(&path);
    assert_eq!(cell.version(), base + 1);
    assert_eq!(
        json_of(&cell.batches())["records"].as_array().map(Vec::len),
        Some(1)
    );
    let lossy = cell.batch("m3", "50.000", "49.000", "0.000");
    assert!(lossy.status >= 400, "{}", lossy.body);

    // HOST-INSTALL-LIVE-1: a law the stored batch breaks is held back, with a
    // finding naming the batch; nothing changes.
    let stricter = with_law("self.input_g == self.output_g");
    let held = cell.change(PLAN_PATH, Some("owner"), &stricter, None);
    assert_eq!(held.status, 409, "{}", held.body);
    let held = json_of(&held);
    assert_eq!(held["code"], "defs.live_data_refused");
    assert_eq!(
        held["diagnostics"][0]["span"]["pointer"],
        format!("/objects/{BATCH}/m2"),
        "{held}"
    );
    let applied = cell.change(APPLY_PATH, Some("owner"), &stricter, Some(base + 1));
    assert_eq!(json_of(&applied)["code"], "defs.live_data_refused");
    assert_eq!(cell.version(), base + 1);
    // A law the stored batch keeps installs, with the stored batch checked.
    let looser = with_law("self.input_g >= self.output_g + self.loss_g");
    let applied = cell.change(APPLY_PATH, Some("owner"), &looser, Some(base + 1));
    assert_eq!(applied.status, 200, "{}", applied.body);
    assert_eq!(json_of(&applied)["plan"]["checkedObjects"], 1);
    assert_eq!(
        json_of(&applied)["plan"]["changed"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(cell.version(), base + 2);
}

/// A reserved artifact or an empty change is refused before admission.
#[test]
fn a_reserved_or_empty_change_is_refused() {
    let directory = tempfile::tempdir().unwrap();
    let mut cell = provisioned(&directory.path().join("cell.sqlite"));
    let base = cell.version();
    let mut forged = batches_pack();
    forged[0]["artifactId"] = json!("core.forged");
    let refused = cell.change(PLAN_PATH, Some("owner"), &forged, None);
    assert_eq!(refused.status, 400);
    assert_eq!(json_of(&refused)["code"], "cell.reserved_artifact");
    let empty = cell.change(PLAN_PATH, Some("owner"), &[], None);
    assert_eq!(empty.status, 400);
    let mut twice = batches_pack();
    twice.push(twice[0].clone());
    assert_eq!(
        cell.change(PLAN_PATH, Some("owner"), &twice, None).status,
        400
    );
    assert_eq!(cell.version(), base);
}

impl Cell {
    /// One MCP `tools/call` (or `tools/list`, with `name` empty) as `role`.
    fn mcp(&mut self, role: &str, name: &str, arguments: &Value) -> Value {
        let message = if name.is_empty() {
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {"limit": 100}})
        } else {
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": name, "arguments": arguments}})
        };
        let response = self.call("POST", "/v1/mcp", Some(&user(role)), &message.to_string());
        assert_eq!(response.status, 200, "{}", response.body);
        json_of(&response)["result"].clone()
    }
}

/// "Add batch tracking" from an agent (#520): the pack planned and applied
/// through MCP answers exactly what the routes answer, only a definition
/// writer is offered the tools, and the pack's action is a tool afterwards.
#[test]
fn an_agent_plans_and_applies_a_pack_through_mcp() {
    let directory = tempfile::tempdir().unwrap();
    let mut cell = provisioned(&directory.path().join("cell.sqlite"));
    let base = cell.version();
    let listed = cell.mcp("member", "", &json!({}));
    assert!(
        !listed.to_string().contains("norn_plan_definitions"),
        "{listed}"
    );

    let planned = cell.mcp(
        "owner",
        "norn_plan_definitions",
        &json!({"artifacts": batches_pack()}),
    );
    assert_eq!(planned["isError"], false, "{planned}");
    let route = json_of(&cell.change(PLAN_PATH, Some("owner"), &batches_pack(), None));
    assert_eq!(planned["structuredContent"], route);
    assert_eq!(planned["structuredContent"]["baseVersion"], json!(base));

    // A member calling the tool by name: it is not in their catalog, so the
    // protocol refuses it before any route runs (no result, a JSON-RPC error).
    let denied = cell.mcp(
        "member",
        "norn_apply_definitions",
        &json!({"artifacts": batches_pack(), "expectedVersion": base}),
    );
    assert!(denied.is_null(), "{denied}");
    let broken = cell.mcp(
        "owner",
        "norn_plan_definitions",
        &json!({"artifacts": with_law("self.input_g == self.output_g + self.loss_kg")}),
    );
    assert_eq!(
        broken["structuredContent"]["code"], "defs.refused",
        "{broken}"
    );
    let stale = cell.mcp(
        "owner",
        "norn_apply_definitions",
        &json!({"artifacts": batches_pack()}),
    );
    assert_eq!(
        stale["isError"], true,
        "expectedVersion is required: {stale}"
    );

    let applied = cell.mcp(
        "owner",
        "norn_apply_definitions",
        &json!({"artifacts": batches_pack(), "expectedVersion": base}),
    );
    assert_eq!(applied["isError"], false, "{applied}");
    assert_eq!(
        applied["structuredContent"]["outcome"], "installed",
        "{applied}"
    );
    assert!(cell.version() > base);
    let after = cell.mcp("admin", "", &json!({}));
    assert!(
        after["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["_meta"]["norn/actionId"] == RECORD),
        "the pack's action is a tool now"
    );
    // An agent whose tool list predates the install calls the new action by id,
    // through the same route and policy: the law refuses, then commits.
    let batch = |id: &str, loss: &str| {
        json!({"action": RECORD, "input": {"id": id, "material": "copper",
            "input_g": "100.000", "output_g": "99.000", "loss_g": loss, "loss_reason": "spill"}})
    };
    let refused = cell.mcp("member", "norn_call_action", &batch("batch-mcp-1", "0.000"));
    assert_eq!(
        refused["structuredContent"]["code"], "core.laws.violated",
        "{refused}"
    );
    let committed = cell.mcp("member", "norn_call_action", &batch("batch-mcp-2", "1.000"));
    assert_eq!(
        committed["structuredContent"]["outcome"], "committed",
        "{committed}"
    );
    let unknown = cell.mcp(
        "member",
        "norn_call_action",
        &json!({"action": "lab.batch.erase", "input": {}}),
    );
    assert_eq!(unknown["isError"], true, "{unknown}");
}

/// A native engine whose next `failures` source-resolution reconciliations
/// fail at their last step (clearing the pending mark): an interrupted
/// reconciliation, injected from the test alone.
struct Interrupting {
    inner: NativeEngine,
    failures: Arc<AtomicUsize>,
}

impl SqlEngine for Interrupting {
    fn execute(&self, sql: &str, params: &[SqlValue]) -> Result<(), CellError> {
        let clears_mark = sql.starts_with("DELETE")
            && params
                .iter()
                .any(|p| matches!(p, SqlValue::Text(t) if t == "source_resolution_pending"));
        if clears_mark
            && self
                .failures
                .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
                    left.checked_sub(1)
                })
                .is_ok()
        {
            return Err(CellError::Engine("reconciliation interrupted".to_owned()));
        }
        self.inner.execute(sql, params)
    }
    fn query(&self, sql: &str, params: &[SqlValue]) -> Result<Vec<Vec<SqlValue>>, CellError> {
        self.inner.query(sql, params)
    }
    fn execute_batch(&self, sql: &str) -> Result<(), CellError> {
        self.inner.execute_batch(sql)
    }
    fn transaction<T, X: From<CellError>>(&self, f: impl FnOnce() -> Result<T, X>) -> Result<T, X> {
        self.inner.transaction(f)
    }
    fn read_isolation(&self) -> ReadIsolation {
        self.inner.read_isolation()
    }
    fn read_cache_revision(&self) -> Result<Option<(u64, u64)>, CellError> {
        self.inner.read_cache_revision()
    }
}

/// SOURCE-RESOLVE-1 (#1478 r5 S2, S4): `POST /v1/cell/definitions:apply`
/// whose first reconciliation attempt fails answers the committed revision
/// (never `503 defs.stale` for a revision that is already active), and the
/// next request finishes it. While a reconciliation keeps failing, a member's
/// question (a named answer or an ad-hoc evaluation over the derived metrics)
/// gets the typed, retryable refusal, never the 404 or the denial of a cell
/// that publishes no questions.
#[test]
fn an_apply_reports_its_committed_revision_and_questions_wait_for_reconciliation() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("cell.sqlite");
    let failures = Arc::new(AtomicUsize::new(0));
    let shared = Arc::clone(&failures);
    let mut cell = provision(Cell::over(Box::new(move || {
        Ok(Interrupting {
            inner: NativeEngine::open(&file, StoreOptions::default())
                .map_err(|error| error.to_string())?,
            failures: Arc::clone(&shared),
        })
    })));
    let base = cell.version();

    failures.store(1, Ordering::SeqCst);
    let applied = cell.change(APPLY_PATH, Some("admin"), &batches_pack(), Some(base));
    assert_eq!(applied.status, 200, "{}", applied.body);
    let receipt = json_of(&applied);
    assert_eq!(receipt["outcome"], "installed", "{receipt}");
    assert_eq!(receipt["definitionsVersion"], base + 1);
    assert!(receipt["plan"]["checkedObjects"].is_u64(), "{receipt}");
    assert_eq!(failures.load(Ordering::SeqCst), 0, "the attempt failed");
    assert_eq!(cell.version(), base + 1, "the next request finishes it");

    failures.store(usize::MAX, Ordering::SeqCst);
    let looser = with_law("self.input_g >= self.output_g + self.loss_g");
    let applied = cell.change(APPLY_PATH, Some("owner"), &looser, Some(base + 1));
    assert_eq!(applied.status, 200, "{}", applied.body);
    let member = user("member");
    let answer = cell.call(
        "GET",
        "/v1/questions/canon.stock_by_week/answer",
        Some(&member),
        "",
    );
    let evaluated = cell.call("POST", "/v1/questions:evaluate", Some(&member), "{}");
    for response in [&answer, &evaluated] {
        assert_eq!(response.status, 503, "{}", response.body);
        let refusal = json_of(response);
        assert_eq!(refusal["code"], "defs.stale", "{refusal}");
        assert_eq!(refusal["retryable"], true, "{refusal}");
    }
    failures.store(0, Ordering::SeqCst);
    let answer = cell.call(
        "GET",
        "/v1/questions/canon.stock_by_week/answer",
        Some(&member),
        "",
    );
    assert_ne!(answer.status, 503, "{}", answer.body);
    assert_eq!(cell.version(), base + 2);
}
