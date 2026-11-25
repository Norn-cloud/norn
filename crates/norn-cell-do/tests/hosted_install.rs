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
use norn_cell::StoreOptions;
use norn_cell::engine::NativeEngine;
use norn_cell_do::base;
use norn_cell_do::policy::DEMO_GRANTS;
use norn_cell_do::principal::TokenPrincipals;
use norn_cell_do::server::{Request, Response};
use norn_cell_do::tenant::Tenant;
use serde_json::{Value, json};

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
    let sub = format!("{role}-of-{org}");
    let claims = json!({ "iss": ISSUER, "aud": "norn-host", "sub": sub, "iat": NOW - 10,
            "exp": NOW + 290, "norn": { "kind": "user", "org": org, "role": role,
            "amr": "password", "accountablePerson": sub } });
    let head = URL_SAFE_NO_PAD.encode(json!({ "alg": "EdDSA", "kid": "k1" }).to_string());
    let body = URL_SAFE_NO_PAD.encode(claims.to_string());
    let signature = key().sign(format!("{head}.{body}").as_bytes());
    format!(
        "Bearer {head}.{body}.{}",
        URL_SAFE_NO_PAD.encode(signature.to_bytes())
    )
}

pub(crate) fn user(role: &str) -> String {
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

pub(crate) struct Cell {
    tenant: Tenant<NativeEngine>,
    tokens: TokenPrincipals,
}

impl Cell {
    fn opened(path: &Path) -> Self {
        let file = path.to_owned();
        let tenant = Tenant::open(
            scope(),
            &DEMO_GRANTS,
            Box::new(move || {
                NativeEngine::open(&file, StoreOptions::default())
                    .map_err(|error| error.to_string())
            }),
            Some(&HANDLE_KEY),
        )
        .unwrap();
        let tokens = TokenPrincipals::new(ISSUER, &jwks(), scope(), tenant.caller(), NOW).unwrap();
        Self { tenant, tokens }
    }

    pub(crate) fn call(
        &mut self,
        method: &str,
        path: &str,
        auth: Option<&str>,
        body: &str,
    ) -> Response {
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

    pub(crate) fn change(
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

    pub(crate) fn version(&mut self) -> u64 {
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

pub(crate) fn json_of(response: &Response) -> Value {
    serde_json::from_str(&response.body).unwrap()
}

pub(crate) fn provisioned(path: &Path) -> Cell {
    let mut cell = Cell::opened(path);
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
    assert_ne!(diagnostics.len(), 0, "{refusal}");
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
/// The approval gating the batches pack's action.
fn record_gate(roles: &[&str]) -> Value {
    json!({ "kind": "approval", "id": "lab.batch.record_approval", "payload": {
        "action": RECORD, "decider": { "roles": roles },
        "question": "Record this batch?", "risk": "high" } })
}

impl Cell {
    fn change_with(&mut self, path: &str, approvals: &[Value], expected: Option<u64>) -> Response {
        let mut body = json!({ "artifacts": batches_pack(), "approvals": approvals });
        if let Some(version) = expected {
            body["expectedVersion"] = json!(version);
        }
        let auth = user("owner");
        self.call("POST", path, Some(&auth), &body.to_string())
    }
}

/// #1469: a pack's approval installs with its action in one transaction. A
/// refusal anywhere (an unknown role, a sign-off of other words, a stale
/// revision) publishes nothing: the action is not live at all. A good apply
/// makes it live already gated: its first call waits for a decision.
#[test]
fn a_new_action_is_never_live_without_its_gate() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cell.sqlite");
    let mut cell = provisioned(&path);
    let base = cell.version();

    let planned = cell.change_with(PLAN_PATH, &[record_gate(&["admin"])], None);
    assert_eq!(planned.status, 200, "{}", planned.body);
    let plan = json_of(&planned);
    let approval = &plan["approvals"][0];
    assert_eq!(approval["id"], "lab.batch.record_approval", "{plan}");
    assert!(approval["restatement"].as_str().unwrap().contains(RECORD));
    let sha = approval["restatementSha256"].as_str().unwrap().to_owned();
    let signed = |roles: &[&str], sha: &str, expected: Option<u64>| {
        let mut gate = record_gate(roles);
        gate["signoff"] = json!({ "restatementSha256": sha });
        gate["expectedVersion"] = json!(expected);
        gate
    };
    let untouched = |cell: &mut Cell| {
        assert_eq!(cell.version(), base, "nothing was published");
        assert_ne!(cell.batches().status, 200, "the action's type is not live");
        let refused = cell.batch("u1", "100.000", "98.000", "2.000");
        assert!(
            refused.status >= 400,
            "the action is not callable: {}",
            refused.body
        );
    };

    // A role the cell does not declare: refused at plan and at apply.
    let unknown = cell.change_with(APPLY_PATH, &[signed(&["auditor"], &sha, None)], Some(base));
    assert_eq!(unknown.status, 422, "{}", unknown.body);
    untouched(&mut cell);
    // Signing other words, or no words at all.
    let other = cell.change_with(
        APPLY_PATH,
        &[signed(&["admin"], &"0".repeat(64), None)],
        Some(base),
    );
    assert_eq!(
        json_of(&other)["code"],
        "definition.signoff_mismatch",
        "{}",
        other.body
    );
    untouched(&mut cell);
    let unsigned = cell.change_with(APPLY_PATH, &[record_gate(&["admin"])], Some(base));
    assert_eq!(
        json_of(&unsigned)["code"],
        "definition.signoff_required",
        "{}",
        unsigned.body
    );
    untouched(&mut cell);
    // A revision the approval does not have.
    let stale = cell.change_with(APPLY_PATH, &[signed(&["admin"], &sha, Some(4))], Some(base));
    assert_eq!(
        json_of(&stale)["code"],
        "definition.version_conflict",
        "{}",
        stale.body
    );
    untouched(&mut cell);

    let applied = cell.change_with(APPLY_PATH, &[signed(&["admin"], &sha, None)], Some(base));
    assert_eq!(applied.status, 200, "{}", applied.body);
    assert_eq!(json_of(&applied)["definitionsVersion"], base + 1);
    assert_eq!(
        cell.version(),
        base + 1,
        "one version: the set and its gate"
    );
    let first = cell.batch("g1", "100.000", "98.000", "2.000");
    assert_eq!(
        first.status, 202,
        "the first call already waits: {}",
        first.body
    );
    assert_eq!(
        json_of(&cell.batches())["records"].as_array().map(Vec::len),
        Some(0),
        "a waiting call commits nothing"
    );
    // The same apply again is unchanged, gate included.
    let again = cell.change_with(
        APPLY_PATH,
        &[signed(&["admin"], &sha, Some(1))],
        Some(base + 1),
    );
    assert_eq!(json_of(&again)["outcome"], "unchanged", "{}", again.body);
}

/// #1469: an apply whose gate cannot install leaves the new action out
/// entirely, and no call commits it ungated at any point. The tenant serves
/// one request at a time (`Tenant::handle` takes `&mut self`, as the Durable
/// Object's input gate does), so the only moments a call can arrive are
/// between requests; a member's call is made at every one of them, after the
/// plan, after each refused apply (a stale set with a well-signed gate, the
/// gate twice, a gate on an action the set lacks) and after the apply that
/// installs. None commits: before the install the action is not callable,
/// after it every call waits for a decision.
#[test]
fn a_failed_apply_installs_no_ungated_action_and_no_call_commits() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cell.sqlite");
    let mut cell = provisioned(&path);
    let base = cell.version();
    let mut calls = 0_u32;
    let mut call_between = |cell: &mut Cell| {
        calls += 1;
        let answer = cell.batch(&format!("c{calls}"), "100.000", "98.000", "2.000");
        assert_ne!(answer.status, 200, "a call committed: {}", answer.body);
        let recorded = json_of(&cell.batches())["records"]
            .as_array()
            .map_or(0, Vec::len);
        assert_eq!(recorded, 0, "no batch was recorded");
        answer.status
    };

    let planned = cell.change_with(PLAN_PATH, &[record_gate(&["admin"])], None);
    assert_eq!(planned.status, 200, "{}", planned.body);
    let sha = json_of(&planned)["approvals"][0]["restatementSha256"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut gate = record_gate(&["admin"]);
    gate["signoff"] = json!({ "restatementSha256": sha });
    gate["expectedVersion"] = Value::Null;
    assert!(
        call_between(&mut cell) >= 400,
        "not callable before the install"
    );

    // A gate on an action the set does not declare refuses the set with it.
    let mut stray = gate.clone();
    stray["id"] = json!("lab.batch.stray_approval");
    stray["payload"]["action"] = json!("lab.batch.unknown");
    let refusals: [(Vec<Value>, u64); 3] = [
        // The gate is admissible and signed, but the set's revision moved.
        (vec![gate.clone()], base + 1),
        // The gate twice: the change is refused whole.
        (vec![gate.clone(), gate.clone()], base),
        (vec![gate.clone(), stray], base),
    ];
    for (approvals, expected) in &refusals {
        let refused = cell.change_with(APPLY_PATH, approvals, Some(*expected));
        assert!(
            (400..500).contains(&refused.status),
            "refused: {}",
            refused.body
        );
        assert_eq!(
            cell.version(),
            base,
            "nothing was published: {}",
            refused.body
        );
        assert!(
            call_between(&mut cell) >= 400,
            "not callable after a refused apply"
        );
    }

    let applied = cell.change_with(APPLY_PATH, &[gate], Some(base));
    assert_eq!(applied.status, 200, "{}", applied.body);
    for _ in 0..3 {
        assert_eq!(call_between(&mut cell), 202, "every call waits on the gate");
    }
}

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

/// #1469 S3 through MCP: the hosted plan and apply tools carry the pack's
/// approvals and their sign-offs to the same routes as HTTP. A refused
/// approval publishes nothing; a signed apply makes the action live already
/// gated, so its first call waits.
#[test]
fn an_agent_applies_a_pack_with_its_gate_through_mcp() {
    let directory = tempfile::tempdir().unwrap();
    let mut cell = provisioned(&directory.path().join("cell.sqlite"));
    let base = cell.version();
    let planned = cell.mcp(
        "owner",
        "norn_plan_definitions",
        &json!({"artifacts": batches_pack(), "approvals": [record_gate(&["admin"])]}),
    );
    assert_eq!(planned["isError"], false, "{planned}");
    let approval = &planned["structuredContent"]["approvals"][0];
    assert_eq!(approval["id"], "lab.batch.record_approval", "{planned}");
    assert_eq!(approval["unchanged"], false, "{planned}");
    let sha = approval["restatementSha256"].as_str().unwrap().to_owned();
    let signed = |roles: &[&str]| {
        let mut gate = record_gate(roles);
        gate["signoff"] = json!({ "restatementSha256": sha });
        gate
    };

    let refused = cell.mcp(
        "owner",
        "norn_apply_definitions",
        &json!({"artifacts": batches_pack(), "expectedVersion": base, "approvals": [signed(&["auditor"])]}),
    );
    assert_eq!(refused["isError"], true, "{refused}");
    assert_eq!(cell.version(), base, "nothing was published");
    assert!(
        cell.batch("m0", "100.000", "98.000", "2.000").status >= 400,
        "the action is not live"
    );

    let applied = cell.mcp(
        "owner",
        "norn_apply_definitions",
        &json!({"artifacts": batches_pack(), "expectedVersion": base, "approvals": [signed(&["admin"])]}),
    );
    assert_eq!(applied["isError"], false, "{applied}");
    assert_eq!(
        cell.version(),
        base + 1,
        "one version: the set and its gate"
    );
    let first = cell.batch("m1", "100.000", "98.000", "2.000");
    assert_eq!(first.status, 202, "the first call waits: {}", first.body);
}

/// #1469 S3: a plan reports each approval changed or unchanged by its own
/// payload, signed or not; a gate-only change plans the next version, and
/// the same gate again plans nothing.
#[test]
fn a_plan_reports_each_gate_change() {
    let directory = tempfile::tempdir().unwrap();
    let mut cell = provisioned(&directory.path().join("cell.sqlite"));
    let base = cell.version();
    let fresh = json_of(&cell.change_with(PLAN_PATH, &[record_gate(&["admin"])], None));
    assert_eq!(fresh["approvals"][0]["unchanged"], false, "{fresh}");
    let sha = fresh["approvals"][0]["restatementSha256"].clone();
    let mut gate = record_gate(&["admin"]);
    gate["signoff"] = json!({ "restatementSha256": sha });
    let applied = cell.change_with(APPLY_PATH, &[gate], Some(base));
    assert_eq!(applied.status, 200, "{}", applied.body);
    let installed = cell.version();

    let same = json_of(&cell.change_with(PLAN_PATH, &[record_gate(&["admin"])], None));
    assert_eq!(same["approvals"][0]["unchanged"], true, "{same}");
    assert_eq!(same["unchanged"], true, "{same}");
    assert_eq!(same["nextVersion"], installed, "{same}");

    let revised = json_of(&cell.change_with(PLAN_PATH, &[record_gate(&["admin", "member"])], None));
    assert_eq!(revised["approvals"][0]["unchanged"], false, "{revised}");
    assert_eq!(revised["approvals"][0]["fromRevision"], 1, "{revised}");
    assert_eq!(
        revised["unchanged"], false,
        "a gate-only change installs: {revised}"
    );
    assert_eq!(revised["nextVersion"], installed + 1, "{revised}");
}

/// #1469 S9: a gate on text `"1"` and one on the number `1` compare
/// differently, so their restatements differ: a sign-off of one's words is
/// refused for the other, over HTTP and over MCP, and nothing is published.
#[test]
fn a_signoff_pins_the_operand_type() {
    let directory = tempfile::tempdir().unwrap();
    let mut cell = provisioned(&directory.path().join("cell.sqlite"));
    let base = cell.version();
    let gate = |value: Value| {
        let mut gate = record_gate(&["admin"]);
        gate["payload"]["when"] = json!({ "property": "unit", "op": "eq", "value": value });
        gate
    };
    let sha_of = |cell: &mut Cell, value: Value| {
        let plan = json_of(&cell.change_with(PLAN_PATH, &[gate(value)], None));
        plan["approvals"][0]["restatementSha256"]
            .as_str()
            .unwrap_or_else(|| panic!("{plan}"))
            .to_owned()
    };
    let text = sha_of(&mut cell, json!("1"));
    let number = sha_of(&mut cell, json!(1));
    assert_ne!(text, number, "text and a number restate apart");
    let mut crossed = gate(json!(1));
    crossed["signoff"] = json!({ "restatementSha256": text });

    let http = cell.change_with(APPLY_PATH, &[crossed.clone()], Some(base));
    assert_eq!(
        json_of(&http)["code"],
        "definition.signoff_mismatch",
        "{}",
        http.body
    );
    assert_eq!(cell.version(), base, "nothing was published");
    let mcp = cell.mcp(
        "owner",
        "norn_apply_definitions",
        &json!({"artifacts": batches_pack(), "expectedVersion": base, "approvals": [crossed]}),
    );
    assert_eq!(mcp["isError"], true, "{mcp}");
    assert!(
        mcp.to_string().contains("definition.signoff_mismatch"),
        "{mcp}"
    );
    assert_eq!(cell.version(), base, "nothing was published");
}

/// #1469 S2 over HTTP: one gate per action is of the change's final set. A
/// change that moves gate G off the pack's action while a new G' gates it
/// plans and applies in one version; both revisions are compare-and-set.
#[test]
fn a_change_moves_a_gate_and_replaces_it() {
    const OTHER: &str = "canon.valuation_policy.set";
    let directory = tempfile::tempdir().unwrap();
    let mut cell = provisioned(&directory.path().join("cell.sqlite"));
    let base = cell.version();
    let gate = |id: &str, action: &str| {
        json!({ "kind": "approval", "id": id, "payload": {
            "action": action, "decider": { "roles": ["admin"] },
            "question": "Record this batch?", "risk": "high" } })
    };
    let sign = |cell: &mut Cell, gates: Vec<Value>, expected: u64| {
        let plan = json_of(&cell.change_with(PLAN_PATH, &gates, None));
        let approvals = plan["approvals"]
            .as_array()
            .unwrap_or_else(|| panic!("{plan}"));
        let signed: Vec<Value> = gates
            .into_iter()
            .zip(approvals)
            .map(|(mut gate, planned)| {
                gate["signoff"] = json!({ "restatementSha256": planned["restatementSha256"] });
                gate["expectedVersion"] = planned["fromRevision"].clone();
                gate
            })
            .collect();
        cell.change_with(APPLY_PATH, &signed, Some(expected))
    };
    let first = sign(&mut cell, vec![gate("lab.g", RECORD)], base);
    assert_eq!(first.status, 200, "{}", first.body);
    let moved = sign(
        &mut cell,
        vec![gate("lab.g", OTHER), gate("lab.g2", RECORD)],
        base + 1,
    );
    assert_eq!(moved.status, 200, "{}", moved.body);
    assert_eq!(
        cell.version(),
        base + 2,
        "the move and its replacement: one version"
    );
    let twice = cell.change_with(
        PLAN_PATH,
        &[gate("lab.g", RECORD), gate("lab.g2", RECORD)],
        None,
    );
    assert_eq!(twice.status, 422, "two gates on one action: {}", twice.body);
}

// PRINCIPAL-INPUT-1 (#1091): the synthetic review pack on the hosted cell.
#[path = "support/review_pack.rs"]
mod review_pack;
use review_pack::{APPROVE, DOCUMENT, admin_role, approval_rule, approve_action, review_base};

impl Cell {
    fn applied(&mut self, artifacts: &[Value], expected: u64) {
        let applied = self.change(APPLY_PATH, Some("owner"), artifacts, Some(expected));
        assert_eq!(applied.status, 200, "{}", applied.body);
        assert_eq!(self.version(), expected + 1);
    }

    fn act(&mut self, action: &str, role: &str, input: &Value, key: &str) -> Response {
        let body = json!({ "input": input, "idempotencyKey": key });
        let path = format!("/v1/actions/{action}");
        self.call("POST", &path, Some(&user(role)), &body.to_string())
    }

    /// Record document `id`, then approve it as `role` under `key`.
    fn approve(&mut self, id: &str, role: &str, key: &str) -> Value {
        let recorded = self.act(
            "review.document.record",
            "member",
            &json!({ "id": id, "owner": "user:member-of-org_a" }),
            &format!("record-{id}"),
        );
        assert_eq!(recorded.status, 200, "{}", recorded.body);
        let approved = self.act(APPROVE, role, &json!({ "id": id }), key);
        assert_eq!(approved.status, 200, "{}", approved.body);
        json_of(&approved)
    }

    /// The stored record of `object_type` whose identity or key ends in `id`.
    fn stored(&mut self, object_type: &str, id: &str) -> Value {
        let path = format!("/v1/objects/{object_type}");
        let page = self.call("GET", &path, Some(&user("admin")), "");
        assert_eq!(page.status, 200, "{}", page.body);
        json_of(&page)["records"]
            .as_array()
            .unwrap()
            .iter()
            .find(|record| record["id"].as_str().is_some_and(|key| key.ends_with(id)))
            .expect("the stored record")
            .clone()
    }
}

/// S1: the call record's `principalInputs` column arrives with the first set
/// that declares principal inputs and stays under any later set, so a
/// rollback installs, a restart reopens, and every recorded call replays.
#[test]
fn principal_inputs_survive_a_rollback_and_a_restart() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cell.sqlite");
    let mut cell = provisioned(&path);
    let base = cell.version();
    let calls = norn_kernel::ACTION_CALL_TYPE;
    let ruled = [
        approval_rule("0.1.0", true),
        approve_action("0.2.0", Some("0.1.0")),
    ];
    let plain = [approval_rule("0.2.0", false), approve_action("0.1.0", None)];

    // Without: the approval pins no decision, and a call commits.
    let mut without = review_base(&admin_role());
    without.push(approve_action("0.1.0", None));
    cell.applied(&without, base);
    assert_eq!(cell.approve("r1", "member", "a1")["outcome"], "committed");
    assert!(
        cell.stored(calls, "a1")["values"]
            .get("principalInputs")
            .is_none()
    );

    // With: an admin's approval is confirmed and a member's is not; each
    // receipt keeps its caller's own resolved value.
    cell.applied(&ruled, base + 1);
    assert_eq!(cell.approve("r2", "admin", "a2")["outcome"], "committed");
    assert_eq!(cell.approve("r3", "member", "a3")["outcome"], "committed");
    assert_eq!(cell.stored(DOCUMENT, "r2")["values"]["confirmed"], true);
    assert_eq!(cell.stored(DOCUMENT, "r3")["values"]["confirmed"], false);
    let recorded = cell.stored(calls, "a2")["values"]["principalInputs"].clone();
    assert_eq!(recorded["principal"], "user:admin-of-org_a", "{recorded}");
    assert_eq!(recorded["values"], json!({ "isAdmin": true }));
    assert_eq!(
        recorded["subject"],
        json!({ "objectType": DOCUMENT, "key": "r2" })
    );
    assert!(
        recorded["policyVersion"]
            .as_str()
            .unwrap()
            .starts_with("roles=studio-roles/1;grants="),
        "{recorded}"
    );
    assert_eq!(
        cell.stored(calls, "a3")["values"]["principalInputs"]["values"],
        json!({ "isAdmin": false })
    );

    // Without again (a rollback): the set declares no principal inputs, the
    // stored values are kept, and the install is not refused.
    cell.applied(&plain, base + 2);
    assert_eq!(cell.approve("r4", "member", "a4")["outcome"], "committed");
    let replayed = cell.act(APPROVE, "member", &json!({ "id": "r1" }), "a1");
    assert_eq!(
        json_of(&replayed)["outcome"],
        "replayed",
        "{}",
        replayed.body
    );
    assert_eq!(
        cell.stored(calls, "a2")["values"]["principalInputs"],
        recorded
    );

    // HOST-REOPEN-1: a restarted host reopens the cell under the model its
    // installed revision recorded, with the kept column and its values.
    drop(cell);
    let mut cell = Cell::opened(&path);
    assert_eq!(cell.version(), base + 3);
    assert_eq!(
        cell.stored(calls, "a2")["values"]["principalInputs"],
        recorded
    );
    let replayed = cell.act(APPROVE, "member", &json!({ "id": "r4" }), "a4");
    assert_eq!(
        json_of(&replayed)["outcome"],
        "replayed",
        "{}",
        replayed.body
    );

    // With again: the calls recorded under that decision replay from their
    // receipts, with nothing resolved again.
    cell.applied(&ruled, base + 3);
    for (id, role, key) in [("r2", "admin", "a2"), ("r3", "member", "a3")] {
        let replayed = cell.act(APPROVE, role, &json!({ "id": id }), key);
        assert_eq!(
            json_of(&replayed)["outcome"],
            "replayed",
            "{}",
            replayed.body
        );
    }
    assert_eq!(
        cell.stored(calls, "a2")["values"]["principalInputs"],
        recorded
    );
}

/// S3: a request never supplies the principal. Both routes that take a
/// decision or action body refuse a `principal` member at its pointer,
/// before anything is decoded, looked up or committed.
#[test]
fn a_request_body_carrying_principal_is_refused_on_the_action_and_decide_routes() {
    let directory = tempfile::tempdir().unwrap();
    let mut cell = provisioned(&directory.path().join("cell.sqlite"));
    let base = cell.version();
    let mut pack = review_base(&admin_role());
    pack.extend([
        approval_rule("0.1.0", true),
        approve_action("0.2.0", Some("0.1.0")),
    ]);
    cell.applied(&pack, base);
    assert_eq!(cell.approve("r1", "member", "a1")["outcome"], "committed");

    let member = user("member");
    let action = format!("/v1/actions/{APPROVE}");
    for (path, body, pointer) in [
        (
            action.as_str(),
            json!({ "input": { "id": "r9" }, "idempotencyKey": "p1",
                "principal": { "isAdmin": true } }),
            "/principal",
        ),
        (
            action.as_str(),
            json!({ "input": { "id": "r9", "principal": { "isAdmin": true } },
                "idempotencyKey": "p2" }),
            "/input/principal",
        ),
        (
            "/v1/decisions/unknown:decide",
            json!({ "choice": "approve", "idempotencyKey": "p3",
                "principal": { "isAdmin": true } }),
            "/principal",
        ),
    ] {
        let refused = cell.call("POST", path, Some(&member), &body.to_string());
        assert_eq!(refused.status, 400, "{path}: {}", refused.body);
        let refusal = json_of(&refused);
        assert_eq!(refusal["code"], "request.invalid", "{refusal}");
        assert_eq!(
            refusal["diagnostics"][0]["span"]["pointer"], pointer,
            "{refusal}"
        );
        assert!(!refused.body.contains("isAdmin"), "{}", refused.body);
    }
    // Nothing was committed under the refused keys.
    for key in ["p1", "p2"] {
        let page = cell.call(
            "GET",
            &format!("/v1/objects/{}", norn_kernel::ACTION_CALL_TYPE),
            Some(&user("admin")),
            "",
        );
        assert!(
            !page.body.contains(&format!("{APPROVE}/{key}")),
            "{}",
            page.body
        );
    }
}

/// S4: a principal catalog the host's action policy could not compile is
/// refused at admission, before anything is installed, so it never takes the
/// cell's governed actions down.
#[test]
fn a_catalog_that_disagrees_with_the_access_rules_is_refused_before_install() {
    let directory = tempfile::tempdir().unwrap();
    let mut cell = provisioned(&directory.path().join("cell.sqlite"));
    let base = cell.version();
    // The access rules compare a text property with the principal's `id`, so
    // `id` is a string; the catalog declares it an integer.
    let owners = json!({ "ruleId": "owners", "actions": [APPROVE], "resourceType": DOCUMENT,
        "scope": { "dimensions": ["org", "environment"] },
        "allOf": [{ "resource": { "name": "owner", "op": "eq",
            "value": { "principalAttribute": "id" } } }] });
    let with_catalog = |catalog: Value| {
        let mut pack = review_base(&catalog);
        pack[1]["payload"]["rules"]
            .as_array_mut()
            .unwrap()
            .push(owners.clone());
        pack.push(approve_action("0.1.0", None));
        pack
    };
    for (catalog, code) in [
        (
            json!([{ "name": "id", "type": "long", "source": { "kind": "attribute" } }]),
            "artifact_set.policy.principal_catalog_conflict",
        ),
        (
            json!([{ "name": "cost-center", "type": "string", "source": { "kind": "attribute" } }]),
            "policy.validation.invalid_payload",
        ),
    ] {
        for route in [PLAN_PATH, APPLY_PATH] {
            let expected = (route == APPLY_PATH).then_some(base);
            let refused = cell.change(
                route,
                Some("owner"),
                &with_catalog(catalog.clone()),
                expected,
            );
            assert_eq!(refused.status, 422, "{route}: {}", refused.body);
            let refusal = json_of(&refused);
            assert_eq!(refusal["code"], "defs.refused", "{refusal}");
            let named = refusal["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .any(|diagnostic| diagnostic["code"] == code);
            assert!(named, "{route}: {refusal}");
        }
        assert_eq!(cell.version(), base, "nothing was installed");
    }
    // The same rules with `id` declared a string install and serve actions.
    let agreeing = json!([{ "name": "id", "type": "string", "source": { "kind": "attribute" } }]);
    cell.applied(&with_catalog(agreeing), base);
    let recorded = cell.act(
        "review.document.record",
        "member",
        &json!({ "id": "r1", "owner": "user:member-of-org_a" }),
        "record-r1",
    );
    assert_eq!(recorded.status, 200, "{}", recorded.body);
}

/// `lowerFlow`'s output for the melt flow (a decision, then one
/// `norn.action@1` step), retargeted at the review pack: the owner decides,
/// and the step approves document `r1`.
fn approval_flow() -> Value {
    let lowered = include_str!("../../../tools/norn-ts/fixtures/flows/melt.ir.json")
        .replace("constitution.melt.convert", APPROVE)
        .replace(
            "{id: 'melt-7', input_fine_g: '12', output_fine_g: '10'}",
            "{id: 'r1'}",
        )
        .replace("decider: {roles: ['admin']}", "decider: {roles: ['owner']}");
    let mut artifacts: Vec<Value> = serde_json::from_str(&lowered).unwrap();
    for artifact in &mut artifacts {
        artifact["scope"] = json!({ "orgId": "org_a", "environmentId": "prod" });
    }
    artifacts[1]["payload"]["outputSchema"]["properties"]["commands"]["items"]["properties"]["input"]
        ["properties"]["convert"]["properties"]["input"] = json!({ "type": "object",
        "additionalProperties": false, "required": ["id"],
        "properties": { "id": { "type": "string" } } });
    json!({ "kind": "flow", "id": "ops.approve_melt", "payload": {
        "flow": { "id": "ops.approve_melt", "version": "1.0.0", "title": "Approve a document" },
        "artifacts": artifacts } })
}

impl Cell {
    /// The review pack whose approval pins the decision that reads
    /// `principal.isAdmin`, installed with `approvals`, and document `r1`
    /// recorded.
    fn ruled(&mut self, approvals: &[Value]) {
        let base = self.version();
        let mut pack = review_base(&admin_role());
        pack.extend([
            approval_rule("0.1.0", true),
            approve_action("0.2.0", Some("0.1.0")),
        ]);
        let owner = user("owner");
        let mut approvals = approvals.to_vec();
        let planned = json!({ "artifacts": pack, "approvals": approvals });
        let planned = self.call("POST", PLAN_PATH, Some(&owner), &planned.to_string());
        assert_eq!(planned.status, 200, "{}", planned.body);
        for (index, approval) in approvals.iter_mut().enumerate() {
            let sha = json_of(&planned)["approvals"][index]["restatementSha256"].clone();
            approval["signoff"] = json!({ "restatementSha256": sha });
        }
        let body = json!({ "artifacts": pack, "approvals": approvals, "expectedVersion": base });
        let applied = self.call("POST", APPLY_PATH, Some(&owner), &body.to_string());
        assert_eq!(applied.status, 200, "{}", applied.body);
        let recorded = self.act(
            "review.document.record",
            "member",
            &json!({ "id": "r1", "owner": "user:member-of-org_a" }),
            "record-r1",
        );
        assert_eq!(recorded.status, 200, "{}", recorded.body);
    }

    /// The owner approves decision `id`.
    fn decided(&mut self, id: &str) -> Value {
        let decided = self.call(
            "POST",
            &format!("/v1/decisions/{id}:decide"),
            Some(&user("owner")),
            &json!({ "choice": "approve", "idempotencyKey": "owner-1" }).to_string(),
        );
        assert_eq!(decided.status, 200, "{}", decided.body);
        json_of(&decided)
    }

    /// Whether any stored call record of `action` keeps principal inputs.
    fn recorded_principal_inputs(&mut self, action: &str) -> Vec<Value> {
        let path = format!("/v1/objects/{}", norn_kernel::ACTION_CALL_TYPE);
        let page = self.call("GET", &path, Some(&user("admin")), "");
        assert_eq!(page.status, 200, "{}", page.body);
        json_of(&page)["records"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|record| record["values"]["actionId"] == action)
            .filter_map(|record| record["values"].get("principalInputs").cloned())
            .collect()
    }
}

/// F2: an approved call is released only on its requester's own request
/// (`as_present`, APPROVAL-RECOVERY-1), never on the approver's, so its
/// principal inputs resolve from that request's verified principal. Here an
/// admin's approval waits for the owner; the owner's decide runs nothing; the
/// admin's retry commits with `isAdmin` resolved for the admin.
#[test]
fn an_approved_call_resolves_principal_inputs_on_its_requesters_own_retry() {
    let directory = tempfile::tempdir().unwrap();
    let mut cell = provisioned(&directory.path().join("cell.sqlite"));
    cell.ruled(&[
        json!({ "kind": "approval", "id": "review.document.approve_approval", "payload": {
        "action": APPROVE, "decider": { "roles": ["owner"] },
        "question": "Approve this document?", "risk": "high" } }),
    ]);

    let parked = cell.act(APPROVE, "admin", &json!({ "id": "r1" }), "a1");
    assert_eq!(parked.status, 202, "the call waits: {}", parked.body);
    let id = json_of(&parked)["decision"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    // The approver's request is not the requester's: it runs nothing, and
    // nothing is resolved for the approver.
    let decided = cell.decided(&id);
    assert_eq!(
        cell.stored(DOCUMENT, "r1")["values"]["status"],
        "draft",
        "{decided}"
    );
    assert_eq!(cell.recorded_principal_inputs(APPROVE), Vec::<Value>::new());

    // The requester's own retry: its live, verified principal is resolved.
    let released = cell.act(APPROVE, "admin", &json!({ "id": "r1" }), "a1");
    assert_eq!(released.status, 200, "{}", released.body);
    assert_eq!(
        json_of(&released)["outcome"],
        "committed",
        "{}",
        released.body
    );
    let document = cell.stored(DOCUMENT, "r1");
    assert_eq!(document["values"]["status"], "approved", "{document}");
    assert_eq!(document["values"]["confirmed"], true, "{document}");
    let recorded = cell.recorded_principal_inputs(APPROVE);
    assert_eq!(recorded.len(), 1, "{recorded:?}");
    assert_eq!(
        recorded[0]["principal"], "user:admin-of-org_a",
        "{recorded:?}"
    );
    assert_eq!(recorded[0]["values"], json!({ "isAdmin": true }));
}

/// Flow steps fail closed, through the whole wiring: the run's starter is an
/// admin, the owner's decide drives the `norn.action@1` step, and the step
/// calls the approval as the recorded starter (`Steps::as_principal`). That
/// caller carries the roles of an earlier request, so `ReadPolicy` resolves
/// nothing, the kernel refuses `decision.principal_input_unavailable`, and
/// nothing is committed, although the starter does hold the role.
#[test]
fn a_flow_step_calling_for_its_starter_resolves_no_principal_input() {
    let directory = tempfile::tempdir().unwrap();
    let mut cell = provisioned(&directory.path().join("cell.sqlite"));
    cell.ruled(&[]);
    let owner = user("owner");
    let mut flow = approval_flow();
    let admitted = cell.call(
        "POST",
        "/v1/definitions:admit",
        Some(&owner),
        &flow.to_string(),
    );
    assert_eq!(admitted.status, 200, "{}", admitted.body);
    flow["signoff"] = json!({ "restatementSha256": json_of(&admitted)["restatementSha256"] });
    let installed = cell.call(
        "POST",
        "/v1/definitions:install",
        Some(&owner),
        &flow.to_string(),
    );
    assert_eq!(installed.status, 200, "{}", installed.body);

    let admin = user("admin");
    let started = cell.call(
        "POST",
        "/v1/runs",
        Some(&admin),
        &json!({ "flow": "ops.approve_melt", "runId": "run-1" }).to_string(),
    );
    assert_eq!(started.status, 201, "{}", started.body);
    let pending = cell.tenant.handle(
        &cell.tokens,
        &Request {
            method: "GET",
            path: "/v1/decisions",
            query: &format!("{QUERY}&state=pending"),
            authorization: Some(&owner),
            now: NOW,
            body: "",
        },
    );
    assert_eq!(pending.status, 200, "{}", pending.body);
    let id = json_of(&pending)["items"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let decided = cell.decided(&id);

    let run = cell.call("GET", "/v1/runs/run-1", Some(&admin), "");
    assert_eq!(run.status, 200, "{}", run.body);
    let run = json_of(&run);
    assert_eq!(run["state"], "failed_refused", "{decided}\n{run}");
    let step = &run["steps"][1];
    assert_eq!(
        (&step["id"], &step["status"]),
        (&json!("convert"), &json!("failed")),
        "{run}"
    );
    // The run keeps why its step failed: the kernel's refusal, by code.
    let failure = cell.stored("ops.ApproveMeltRun", "run-1")["values"]["slots"]["failure"].clone();
    assert_eq!(
        failure,
        json!({ "_tag": "Refused", "code": "decision.principal_input_unavailable" }),
        "{decided}"
    );
    let document = cell.stored(DOCUMENT, "r1");
    assert_eq!(document["values"]["status"], "draft", "{document}");
    assert_eq!(document["values"]["confirmed"], false, "{document}");
    assert_eq!(cell.recorded_principal_inputs(APPROVE), Vec::<Value>::new());
}
