//! HOST-ACTION-2 (#1252): on a provisioned tenant cell, `POST
//! /v1/actions/{actionId}` is decided by the scope's own policy through the
//! kernel PDP, for the principal a verified host token names.
//!
//! - A policy that grants only `admin` admits an admin, and refuses member,
//!   viewer, owner, a service principal and another organization's admin.
//! - A service principal holds only the roles the declared table lets a
//!   service hold (`member`, `viewer`); a policy naming `member` admits it.
//! - An action without an `access` ref is uncallable; a policy cannot grant an
//!   action another policy governs; a pin to another policy version refuses.
//! - Every refusal is the one opaque action denial, the same bytes as for an
//!   action that does not exist, and commits nothing.
//!
//! These are refusal-path and isolation pins on a new authority (D79): each
//! names one decision of the server's permission system.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "action correspondence"
)]

use std::path::Path;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signer, SigningKey};
use norn_api::provision::PROVISION_PATH;
use norn_api::scope::CellScope;
use norn_cell::StoreOptions;
use norn_cell::engine::NativeEngine;
use norn_cell_do::actions::ActionPolicy;
use norn_cell_do::base;
use norn_cell_do::policy::DEMO_GRANTS;
use norn_cell_do::principal::TokenPrincipals;
use norn_cell_do::server::{Request, Response};
use norn_cell_do::tenant::Tenant;
use serde_json::{Value, json};

const ISSUER: &str = "https://studio-preview.example";
const NOW: u64 = 1_800_000_000;
const MELT_ACTION: &str = "constitution.melt.convert";
const MELT: &str = "constitution.Melt";
const POLICY: &str = "constitution.melt.access.policy";
const HANDLE_KEY: [u8; 32] = [7; 32];

fn key() -> SigningKey {
    SigningKey::from_bytes(&[1; 32])
}

fn jwks() -> String {
    json!({ "keys": [{ "kty": "OKP", "crv": "Ed25519", "alg": "EdDSA", "kid": "k1",
                       "x": URL_SAFE_NO_PAD.encode(key().verifying_key().as_bytes()) }] })
    .to_string()
}

fn token(kind: &str, org: &str, role: &str) -> String {
    let amr = if kind == "service" {
        "api_key"
    } else {
        "password"
    };
    let claims = json!({ "iss": ISSUER, "aud": "norn-host", "sub": format!("{kind}-{role}-of-{org}"),
            "iat": NOW - 10, "exp": NOW + 290,
            "norn": { "kind": kind, "org": org, "role": role, "amr": amr } });
    let head = URL_SAFE_NO_PAD.encode(json!({ "alg": "EdDSA", "kid": "k1" }).to_string());
    let body = URL_SAFE_NO_PAD.encode(claims.to_string());
    let signature = key().sign(format!("{head}.{body}").as_bytes());
    format!(
        "Bearer {head}.{body}.{}",
        URL_SAFE_NO_PAD.encode(signature.to_bytes())
    )
}

fn user(role: &str) -> String {
    token("user", "org_a", role)
}

fn scope() -> CellScope {
    CellScope {
        org_id: "org_a".into(),
        environment_id: "prod".into(),
    }
}

const QUERY: &str = "orgId=org_a&environmentId=prod";

/// The melt artifacts, re-scoped to the tenant, with the action's `access`
/// set by `access` (or removed).
fn melt(access: Option<&Value>) -> Vec<Value> {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../norn-cell/tests/fixtures/constitution.json"
    ))
    .unwrap();
    fixture["withLoss"]
        .as_array()
        .unwrap()
        .iter()
        .cloned()
        .map(|mut artifact| {
            artifact["scope"] = json!({ "orgId": "org_a", "environmentId": "prod" });
            if artifact["kind"] == "action" {
                let payload = artifact["payload"].as_object_mut().unwrap();
                payload.remove("access");
                if let Some(access) = access {
                    payload.insert("access".into(), access.clone());
                }
            }
            artifact
        })
        .collect()
}

/// A policy artifact `id` whose one rule lets `roles` call `actions` on melts.
fn policy(id: &str, actions: &[&str], roles: &[&str]) -> Value {
    let policy_id = id.trim_end_matches(".policy");
    json!({
        "artifactId": id, "dependencies": [], "irVersion": "0.1", "kind": "policy",
        "lifecycle": "active", "name": "Melt access", "namespace": "constitution",
        "payload": { "accessVersion": "0.1", "policyId": policy_id, "rules": [{
            "ruleId": "callers", "actions": actions, "resourceType": MELT,
            "scope": { "dimensions": ["org", "environment"] },
            "allOf": [{ "role": { "anyOf": roles } }] }] },
        "provenance": { "source": "authoring_sdk", "sourcePath": "tests/tenant_actions.rs" },
        "scope": { "orgId": "org_a", "environmentId": "prod" },
        "version": "1.0.0"
    })
}

fn access() -> Value {
    json!({ "artifactId": POLICY, "version": "1.0.0" })
}

struct Cell {
    tenant: Tenant<NativeEngine>,
    tokens: TokenPrincipals,
}

impl Cell {
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

    fn melt(&mut self, action: &str, auth: Option<&str>, id: &str) -> Response {
        let body = format!(
            r#"{{"input":{{"id":"{id}","input_fine_g":"100","output_fine_g":"98"}},"idempotencyKey":"{id}"}}"#
        );
        self.call("POST", &format!("/v1/actions/{action}"), auth, &body)
    }

    fn melts(&mut self) -> Value {
        let page = self.call(
            "GET",
            &format!("/v1/objects/{MELT}"),
            Some(&user("admin")),
            "",
        );
        assert_eq!(page.status, 200, "{}", page.body);
        serde_json::from_str(&page.body).unwrap()
    }
}

/// The tenant cell stored at `path`, opened as the host opens it.
fn opened(path: &Path) -> Cell {
    let file = path.to_owned();
    let tenant = Tenant::open(
        scope(),
        &DEMO_GRANTS,
        Box::new(move || {
            NativeEngine::open(&file, StoreOptions::default()).map_err(|error| error.to_string())
        }),
        Some(&HANDLE_KEY),
    )
    .unwrap();
    let tokens = TokenPrincipals::new(ISSUER, &jwks(), scope(), tenant.caller(), NOW).unwrap();
    Cell { tenant, tokens }
}

/// The owner's provisioning of the cell at `path` with `artifacts`, and
/// what it answered.
fn provision(path: &Path, artifacts: &[Value]) -> (Cell, Response) {
    let mut cell = opened(path);
    let body = json!({
        "baseSha256": base::trusted_base().unwrap().sha256(),
        "artifacts": artifacts,
    })
    .to_string();
    let response = cell.call("POST", PROVISION_PATH, Some(&user("owner")), &body);
    (cell, response)
}

/// A tenant cell at `path` provisioned by its owner with `artifacts`.
fn provisioned(path: &Path, artifacts: &[Value]) -> Cell {
    let (cell, response) = provision(path, artifacts);
    assert_eq!(response.status, 200, "{}", response.body);
    cell
}

/// An admin installs a retained flow on the cell: a new definitions version
/// whose model is the flow's admission. The flow is the lowered `norn-ts`
/// "Approve a refund" fixture (one step performing the `norn.decision@1`
/// built-in), so it needs no installed binding.
fn install_flow(cell: &mut Cell) {
    let mut artifacts: Vec<Value> =
        serde_json::from_str(include_str!("fixtures/ask.flow.json")).unwrap();
    for artifact in &mut artifacts {
        artifact["scope"] = json!({ "orgId": "org_a", "environmentId": "prod" });
    }
    let body = json!({ "kind": "flow", "id": "ops.approve_refund", "payload": {
        "flow": { "id": "ops.approve_refund", "version": "1.0.0", "title": "Approve a refund",
                  "steps": [{ "id": "ask_review", "label": "Review the refund" }] },
        "artifacts": artifacts } });
    let admitted = cell.call(
        "POST",
        "/v1/definitions:admit",
        Some(&user("admin")),
        &body.to_string(),
    );
    assert_eq!(admitted.status, 200, "{}", admitted.body);
    let digest =
        serde_json::from_str::<Value>(&admitted.body).unwrap()["restatementSha256"].clone();
    let mut body = body;
    body["signoff"] = json!({ "restatementSha256": digest });
    let installed = cell.call(
        "POST",
        "/v1/definitions:install",
        Some(&user("admin")),
        &body.to_string(),
    );
    assert_eq!(installed.status, 200, "{}", installed.body);
}

fn with_policy(roles: &[&str]) -> Vec<Value> {
    let mut artifacts = melt(Some(&access()));
    artifacts.push(policy(POLICY, &[MELT_ACTION], roles));
    artifacts
}

/// What a call to an action that does not exist answers: the one denial.
fn denial(cell: &mut Cell) -> Response {
    let response = cell.melt("constitution.no_such_action", Some(&user("admin")), "x");
    assert_eq!(response.status, 403);
    response
}

#[test]
fn the_scopes_policy_decides_a_call() {
    let directory = tempfile::tempdir().unwrap();
    let mut cell = provisioned(&directory.path().join("a.sqlite"), &with_policy(&["admin"]));
    let denied = denial(&mut cell);
    let refused: [(&str, Option<String>); 8] = [
        ("anonymous", None),
        ("member", Some(user("member"))),
        ("viewer", Some(user("viewer"))),
        ("owner", Some(user("owner"))),
        ("service member", Some(token("service", "org_a", "member"))),
        ("service admin", Some(token("service", "org_a", "admin"))),
        (
            "another organization's admin",
            Some(token("user", "org_b", "admin")),
        ),
        ("not a token", Some("Bearer nonsense".to_owned())),
    ];
    for (who, auth) in &refused {
        let response = cell.melt(
            MELT_ACTION,
            auth.as_deref(),
            &format!("refused-{}", who.len()),
        );
        assert_eq!(response, denied, "{who}");
    }
    assert_eq!(cell.melts()["records"].as_array().map(Vec::len), Some(0));
    let admin = cell.melt(MELT_ACTION, Some(&user("admin")), "m1");
    assert_eq!(admin.status, 200, "{}", admin.body);
    let receipt: Value = serde_json::from_str(&admin.body).unwrap();
    assert_eq!(receipt["outcome"], "committed", "{receipt}");
    assert_eq!(cell.melts()["records"].as_array().map(Vec::len), Some(1));
    // The same call replays; nobody else's attempt commits anything.
    let again = cell.melt(MELT_ACTION, Some(&user("admin")), "m1");
    assert_eq!(
        serde_json::from_str::<Value>(&again.body).unwrap()["outcome"],
        "replayed"
    );
    assert_eq!(cell.melts()["records"].as_array().map(Vec::len), Some(1));
}

/// A service principal holds only service roles (`member`, `viewer`); a
/// policy that names `member` admits it, as it admits a member user.
#[test]
fn a_member_policy_admits_members_and_member_services() {
    let directory = tempfile::tempdir().unwrap();
    let mut cell = provisioned(
        &directory.path().join("a.sqlite"),
        &with_policy(&["member"]),
    );
    let denied = denial(&mut cell);
    for (auth, id) in [
        (user("member"), "u"),
        (token("service", "org_a", "member"), "s"),
    ] {
        let response = cell.melt(MELT_ACTION, Some(&auth), id);
        assert_eq!(response.status, 200, "{}", response.body);
    }
    for auth in [
        user("viewer"),
        token("service", "org_a", "viewer"),
        user("admin"),
    ] {
        assert_eq!(cell.melt(MELT_ACTION, Some(&auth), "v"), denied);
    }
}

/// An action is callable only through the policy its own `access` ref names.
#[test]
fn an_action_is_callable_only_through_its_own_policy() {
    let directory = tempfile::tempdir().unwrap();
    let other = "constitution.other.access.policy";
    let cases: [(&str, Vec<Value>); 3] = [
        ("no access ref", {
            let mut artifacts = melt(None);
            artifacts.push(policy(POLICY, &[MELT_ACTION], &["admin"]));
            artifacts
        }),
        ("another policy grants it", {
            let mut artifacts = melt(Some(&access()));
            artifacts.push(policy(POLICY, &[MELT_ACTION], &["owner"]));
            artifacts.push(policy(other, &[MELT_ACTION], &["admin"]));
            artifacts
        }),
        ("pinned to another version", {
            let mut artifacts = melt(Some(&json!({ "artifactId": POLICY, "version": "9.9.9" })));
            artifacts.push(policy(POLICY, &[MELT_ACTION], &["admin"]));
            artifacts
        }),
    ];
    for (index, (case, artifacts)) in cases.iter().enumerate() {
        let mut cell = provisioned(&directory.path().join(format!("{index}.sqlite")), artifacts);
        let denied = denial(&mut cell);
        assert_eq!(
            cell.melt(MELT_ACTION, Some(&user("admin")), "m"),
            denied,
            "{case}"
        );
        assert_eq!(
            cell.melts()["records"].as_array().map(Vec::len),
            Some(0),
            "{case}"
        );
    }
}

/// A referenced policy that does not compile yields an error, never a
/// policy: the host then serves no action.
#[test]
fn a_policy_that_does_not_compile_yields_none() {
    let mut artifacts = melt(Some(&access()));
    let mut broken = policy(POLICY, &[MELT_ACTION], &["admin"]);
    broken["payload"]["rules"][0]["resourceType"] = json!("constitution.NoSuchType");
    artifacts.push(broken);
    let envelopes: Vec<norn_ir::ArtifactEnvelope> =
        serde_json::from_value(Value::Array(artifacts)).unwrap();
    let mut all: Vec<norn_ir::ArtifactEnvelope> = base::SOURCES
        .iter()
        .map(|text| serde_json::from_str(text).unwrap())
        .collect();
    all.extend(envelopes);
    let model = norn_api::kernel_set::kernel_model(&all).unwrap();
    assert!(ActionPolicy::compile(&model, &all, &scope()).is_err());
    // Without a policy that names it, nothing is callable at all.
    let unreferenced = melt(None);
    let envelopes: Vec<norn_ir::ArtifactEnvelope> =
        serde_json::from_value(Value::Array(unreferenced)).unwrap();
    assert!(
        ActionPolicy::compile(&model, &envelopes, &scope())
            .unwrap()
            .is_none()
    );
}

/// The cell registers the compiled-in built-ins, and only them.
#[test]
fn the_compiled_in_builtins_are_registered() {
    assert!(norn_cell::builtin_registered("norn.decision", 1));
    assert!(!norn_cell::builtin_registered("norn.decision", 2));
    assert!(!norn_cell::builtin_registered("tenant.anything", 1));
}

/// The kernel's call records (who called what, with which input and
/// idempotency key) are read by admins and owners only.
#[test]
fn kernel_call_records_are_read_by_admins_and_owners_only() {
    let directory = tempfile::tempdir().unwrap();
    let mut cell = provisioned(
        &directory.path().join("a.sqlite"),
        &with_policy(&["member"]),
    );
    assert_eq!(
        cell.melt(MELT_ACTION, Some(&user("member")), "m1").status,
        200
    );
    let calls = format!("/v1/objects/{}", norn_kernel::ACTION_CALL_TYPE);
    for role in ["member", "viewer"] {
        let response = cell.call("GET", &calls, Some(&user(role)), "");
        assert_ne!(response.status, 200, "{role}: {}", response.body);
        assert!(!response.body.contains("m1"), "{role}");
    }
    for role in ["admin", "owner"] {
        let response = cell.call("GET", &calls, Some(&user(role)), "");
        assert_eq!(response.status, 200, "{role}: {}", response.body);
        let page: Value = serde_json::from_str(&response.body).unwrap();
        assert_eq!(page["records"].as_array().map(Vec::len), Some(1), "{role}");
    }
}

/// An idempotency key another caller already used is refused: a second
/// caller never replays, or learns, the first caller's call.
#[test]
fn a_second_caller_reusing_an_idempotency_key_is_refused() {
    let directory = tempfile::tempdir().unwrap();
    let mut cell = provisioned(
        &directory.path().join("a.sqlite"),
        &with_policy(&["member", "admin"]),
    );
    assert_eq!(
        cell.melt(MELT_ACTION, Some(&user("member")), "k1").status,
        200
    );
    let reused = cell.melt(MELT_ACTION, Some(&user("admin")), "k1");
    let refusal: Value = serde_json::from_str(&reused.body).unwrap();
    assert_eq!(
        (reused.status, refusal["code"].as_str()),
        (409, Some("action.idempotency_conflict")),
        "{}",
        reused.body
    );
    assert_eq!(cell.melts()["records"].as_array().map(Vec::len), Some(1));
}

/// A flow installed after an action changes the definitions version: the
/// action policy is recompiled for it and decides as before, the action still
/// commits, and a reopened cell serves the same decisions over the same
/// records.
#[test]
fn a_flow_install_and_a_reopen_keep_the_action_and_its_policy() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("a.sqlite");
    let mut cell = provisioned(&path, &with_policy(&["admin"]));
    let committed = |cell: &mut Cell, id: &str| {
        let response = cell.melt(MELT_ACTION, Some(&user("admin")), id);
        assert_eq!(response.status, 200, "{id}: {}", response.body);
        let receipt: Value = serde_json::from_str(&response.body).unwrap();
        assert_eq!(receipt["outcome"], "committed", "{id}: {receipt}");
    };
    committed(&mut cell, "m1");
    install_flow(&mut cell);
    committed(&mut cell, "m2");
    let denied = denial(&mut cell);
    assert_eq!(cell.melt(MELT_ACTION, Some(&user("member")), "x1"), denied);
    assert_eq!(cell.tenant.action_policy_error(), None);
    drop(cell);
    let mut cell = opened(&path);
    committed(&mut cell, "m3");
    let denied = denial(&mut cell);
    assert_eq!(cell.melt(MELT_ACTION, Some(&user("member")), "x2"), denied);
    assert_eq!(cell.melts()["records"].as_array().map(Vec::len), Some(3));
}

/// A `rel` or `context` condition never permits on a tenant cell. The host
/// projects no relationship metadata and supplies an empty context, so the
/// strict Cedar validator finds the rule impossible: the policy does not
/// compile, the cell serves no action (even to the admin the rule's role
/// names), nothing commits, and the operator is told why.
#[test]
fn relationship_and_context_conditions_never_permit() {
    let directory = tempfile::tempdir().unwrap();
    let conditions = [
        (
            "rel",
            json!({ "rel": { "from": "resource", "role": "owner" } }),
        ),
        (
            "context",
            json!({ "context": { "name": "channel", "op": "eq", "value": "web" } }),
        ),
    ];
    for (case, condition) in conditions {
        let mut artifacts = melt(Some(&access()));
        let mut gated = policy(POLICY, &[MELT_ACTION], &["admin"]);
        gated["payload"]["rules"][0]["allOf"]
            .as_array_mut()
            .unwrap()
            .push(condition);
        artifacts.push(gated);
        let (mut cell, provisioned) =
            provision(&directory.path().join(format!("{case}.sqlite")), &artifacts);
        assert_eq!(provisioned.status, 200, "{case}: {}", provisioned.body);
        let denied = denial(&mut cell);
        assert_eq!(
            cell.melt(MELT_ACTION, Some(&user("admin")), "m"),
            denied,
            "{case}"
        );
        let reason = cell.tenant.action_policy_error().unwrap_or_default();
        assert!(reason.contains("policy is impossible"), "{case}: {reason}");
        assert_eq!(
            cell.melts()["records"].as_array().map(Vec::len),
            Some(0),
            "{case}"
        );
    }
}

/// A call the policy grants still reads only what its caller may read: an
/// action whose precondition slice reads an existing melt is refused to a
/// viewer (the policy grants the call, but a viewer holds no Values grant on
/// `constitution.Melt`) with the one opaque denial, and nothing commits; the
/// same grant commits for a member, who holds Values.
#[test]
fn a_call_reads_its_slice_only_with_the_callers_read_grant() {
    const RECHECK: &str = "constitution.melt.recheck";
    let directory = tempfile::tempdir().unwrap();
    let mut artifacts = melt(Some(&access()));
    // The melt records which earlier melt it rechecks (`of`), so the stored
    // input fits the type and a granted call commits (the positive control).
    for artifact in &mut artifacts {
        if artifact["artifactId"] == "constitution.Melt.entity" {
            artifact["payload"]["objectType"]["properties"]
                .as_array_mut()
                .unwrap()
                .push(json!({ "id": "of", "type": { "kind": "text" }, "default": "" }));
        }
    }
    let mut recheck = artifacts
        .iter()
        .find(|artifact| artifact["kind"] == "action")
        .cloned()
        .unwrap();
    recheck["artifactId"] = json!(format!("{RECHECK}.action"));
    let payload = &mut recheck["payload"];
    payload["actionId"] = json!(RECHECK);
    payload["inputSchema"]["properties"]["of"] = json!({ "type": "string" });
    payload["inputSchema"]["required"]
        .as_array_mut()
        .unwrap()
        .push(json!("of"));
    payload["slice"] = json!([{ "name": "earlier", "objectType": MELT,
        "key": { "property": { "from": "input", "property": "of" } } }]);
    artifacts.push(recheck);
    let mut rules = policy(POLICY, &[MELT_ACTION], &["admin"]);
    let mut readers = rules["payload"]["rules"][0].clone();
    readers["ruleId"] = json!("readers");
    readers["actions"] = json!([RECHECK]);
    readers["allOf"] = json!([{ "role": { "anyOf": ["viewer", "member"] } }]);
    rules["payload"]["rules"]
        .as_array_mut()
        .unwrap()
        .push(readers);
    artifacts.push(rules);
    let (mut cell, provisioned) = provision(&directory.path().join("a.sqlite"), &artifacts);
    assert_eq!(provisioned.status, 200, "{}", provisioned.body);
    assert_eq!(cell.tenant.action_policy_error(), None);
    assert_eq!(
        cell.melt(MELT_ACTION, Some(&user("admin")), "earlier")
            .status,
        200
    );
    let recheck = |cell: &mut Cell, role: &str, id: &str| {
        let body = format!(
            r#"{{"input":{{"id":"{id}","of":"earlier","input_fine_g":"100","output_fine_g":"98"}},"idempotencyKey":"{id}"}}"#
        );
        cell.call(
            "POST",
            &format!("/v1/actions/{RECHECK}"),
            Some(&user(role)),
            &body,
        )
    };
    let denied = denial(&mut cell);
    assert_eq!(recheck(&mut cell, "viewer", "v"), denied);
    assert_eq!(cell.melts()["records"].as_array().map(Vec::len), Some(1));
    // The positive control: the same grant commits for a member, who holds
    // Values on `constitution.Melt`. Without the `readers` rule it would not.
    let member = recheck(&mut cell, "member", "m");
    assert_eq!(member.status, 200, "{}", member.body);
    assert_eq!(cell.melts()["records"].as_array().map(Vec::len), Some(2));
}

/// A referenced policy that does not compile leaves the cell serving no
/// action, and the reason is kept for the operator, not the caller.
#[test]
fn a_policy_compile_failure_is_reported_to_the_operator() {
    let directory = tempfile::tempdir().unwrap();
    let mut artifacts = melt(Some(&access()));
    let mut broken = policy(POLICY, &[MELT_ACTION], &["admin"]);
    let rule = broken["payload"]["rules"][0].clone();
    broken["payload"]["rules"]
        .as_array_mut()
        .unwrap()
        .push(rule);
    artifacts.push(broken);
    let (mut cell, provisioned) = provision(&directory.path().join("a.sqlite"), &artifacts);
    assert_eq!(provisioned.status, 200, "{}", provisioned.body);
    let denied = denial(&mut cell);
    assert_eq!(cell.melt(MELT_ACTION, Some(&user("admin")), "m"), denied);
    let reason = cell.tenant.action_policy_error().map(str::to_owned);
    assert!(
        reason
            .as_deref()
            .is_some_and(|reason| reason.contains("authz.access.rule_duplicate")),
        "{reason:?}"
    );
    assert!(!denied.body.contains(reason.as_deref().unwrap_or_default()));
    // The failure is the policy's, so it is settled for this definitions
    // version: a second request does not recompile it (#1297 r4 nit).
    let settled = cell.tenant.action_policy_version();
    let compiles = cell.tenant.action_policy_compiles();
    assert!(settled.is_some());
    assert_eq!(cell.melt(MELT_ACTION, Some(&user("admin")), "n"), denied);
    assert_eq!(cell.tenant.action_policy_version(), settled);
    assert_eq!(cell.tenant.action_policy_compiles(), compiles);
    // A new definitions version compiles again, once, and still refuses.
    install_flow(&mut cell);
    assert_eq!(cell.melt(MELT_ACTION, Some(&user("admin")), "o"), denied);
    let moved = cell.tenant.action_policy_version();
    assert!(
        moved.is_some() && moved != settled,
        "{settled:?} -> {moved:?}"
    );
    assert_eq!(cell.tenant.action_policy_compiles(), compiles + 1);
    assert!(
        cell.tenant
            .action_policy_error()
            .is_some_and(|reason| reason.contains("authz.access.rule_duplicate")),
        "{:?}",
        cell.tenant.action_policy_error()
    );
}

/// A dashboard tile's question, as Studio saves one: any `QuestionIR`.
fn tile_question() -> Value {
    let mut question: Value = serde_json::from_str(include_str!(
        "../../../fixtures/ir/metrics/sales-by-store-day.question.json"
    ))
    .unwrap();
    question["objectSet"] = json!(MELT);
    question
}

/// A tenant cell keys receipt handles with the stage's lineage key but
/// publishes no questions: an admin's dashboard with a question tile, or a
/// saved question, is refused with the base's 422 finding
/// `definition.question_unavailable`, never an authorization denial
/// (#1297 r2 B1).
#[test]
fn a_question_install_on_a_tenant_cell_is_unavailable_not_denied() {
    let directory = tempfile::tempdir().unwrap();
    let mut cell = provisioned(&directory.path().join("a.sqlite"), &with_policy(&["admin"]));
    let dashboard = json!({ "kind": "dashboard", "id": "studio.dashboard.melts", "payload": {
        "title": "Melts", "tiles": [{ "id": "t1", "title": "Loss", "question": tile_question(),
            "view": { "kind": "chart", "chart": "line" }, "layout": { "x": 0, "y": 0, "w": 6, "h": 4 } }] } });
    let question =
        json!({ "kind": "question", "id": "studio.question.loss", "payload": tile_question() });
    for (case, body) in [("dashboard", &dashboard), ("question", &question)] {
        for path in ["/v1/definitions:admit", "/v1/definitions:install"] {
            let response = cell.call("POST", path, Some(&user("admin")), &body.to_string());
            let document: Value = serde_json::from_str(&response.body).unwrap();
            let codes: Vec<&str> = document["diagnostics"]
                .as_array()
                .map(|found| found.iter().filter_map(|d| d["code"].as_str()).collect())
                .unwrap_or_default();
            // `:admit` reports its findings (200, not admitted); `:install`
            // refuses with them (422).
            let expected = if path.ends_with(":admit") { 200 } else { 422 };
            assert_eq!(response.status, expected, "{case} {path}: {document}");
            assert_ne!(
                document["admitted"],
                json!(true),
                "{case} {path}: {document}"
            );
            assert!(
                codes.contains(&"definition.question_unavailable"),
                "{case} {path}: {document}"
            );
        }
    }
}

/// `GET /v1/access` on a provisioned tenant cell answers every role the
/// table grants: the kernel's call records (admin and owner only) are not
/// among the collections it decides over (#1297 r2 B2).
#[test]
fn every_role_reads_its_access_on_a_tenant_cell() {
    let directory = tempfile::tempdir().unwrap();
    let mut cell = provisioned(&directory.path().join("a.sqlite"), &with_policy(&["admin"]));
    for (who, auth, values) in [
        ("viewer", user("viewer"), false),
        ("member", user("member"), true),
        ("service member", token("service", "org_a", "member"), true),
        ("service viewer", token("service", "org_a", "viewer"), false),
        ("admin", user("admin"), true),
        ("owner", user("owner"), true),
    ] {
        let response = cell.call("GET", "/v1/access", Some(&auth), "");
        assert_eq!(response.status, 200, "{who}: {}", response.body);
        let document: Value = serde_json::from_str(&response.body).unwrap();
        let decision = |name: &str| document["capabilities"][name]["decision"].clone();
        assert_eq!(decision("population"), json!("permit"), "{who}: {document}");
        let expected = if values { "permit" } else { "deny" };
        assert_eq!(decision("values"), json!(expected), "{who}: {document}");
    }
}

/// A flow install adds its run type to a live tenant cell, and the read
/// rules and `/v1/access` follow at once, with no restart (#1297 r2 S2).
#[test]
fn a_flow_install_is_readable_without_a_restart() {
    let directory = tempfile::tempdir().unwrap();
    let mut cell = provisioned(&directory.path().join("a.sqlite"), &with_policy(&["admin"]));
    let runs = "/v1/objects/ops.ApproveRefundRun";
    assert_ne!(cell.call("GET", runs, Some(&user("admin")), "").status, 200);
    install_flow(&mut cell);
    let after = cell.call("GET", runs, Some(&user("admin")), "");
    assert_eq!(after.status, 200, "{}", after.body);
    // The admin's grants now hold over the new collection too.
    let access = cell.call("GET", "/v1/access", Some(&user("admin")), "");
    assert_eq!(access.status, 200, "{}", access.body);
    let document: Value = serde_json::from_str(&access.body).unwrap();
    assert_eq!(
        document["capabilities"]["values"]["decision"],
        json!("permit"),
        "{document}"
    );
}

/// A `resource` condition never permits on a tenant cell: the host compiles
/// policies without `EntityIR` projection metadata, so the policy does not
/// compile (`authz.access.projection_required`), the cell serves no action
/// (even to the admin the rule's role names), nothing commits, and the
/// operator is told why.
#[test]
fn a_resource_condition_never_permits() {
    let directory = tempfile::tempdir().unwrap();
    let mut artifacts = melt(Some(&access()));
    let mut gated = policy(POLICY, &[MELT_ACTION], &["admin"]);
    gated["payload"]["rules"][0]["allOf"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "resource": { "name": "id", "op": "eq", "value": "m" } }));
    artifacts.push(gated);
    let (mut cell, provisioned) = provision(&directory.path().join("a.sqlite"), &artifacts);
    assert_eq!(provisioned.status, 200, "{}", provisioned.body);
    let denied = denial(&mut cell);
    assert_eq!(cell.melt(MELT_ACTION, Some(&user("admin")), "m"), denied);
    assert_eq!(cell.melts()["records"].as_array().map(Vec::len), Some(0));
    let reason = cell.tenant.action_policy_error().unwrap_or_default();
    assert!(
        reason.contains("authz.access.projection_required"),
        "{reason}"
    );
}

/// D84: a tenant cell publishes no questions, so a named question is the
/// base's 404 `question.not_found` on a vacant scope and a provisioned one
/// alike, for every caller and with any query (#1297 r3 should 1).
#[test]
fn a_named_question_is_not_found_on_vacant_and_provisioned_cells_alike() {
    let directory = tempfile::tempdir().unwrap();
    let mut served = provisioned(
        &directory.path().join("served.sqlite"),
        &with_policy(&["admin"]),
    );
    let mut vacant = opened(&directory.path().join("vacant.sqlite"));
    for path in ["/v1/questions/q/answer", "/v1/questions/q/trace"] {
        for query in [QUERY, "orgId=org_a&environmentId=prod&unknown=1"] {
            for auth in [None, Some(user("admin")), Some(user("viewer"))] {
                let ask = |cell: &mut Cell| {
                    cell.tenant.handle(
                        &cell.tokens,
                        &Request {
                            method: "GET",
                            path,
                            query,
                            authorization: auth.as_deref(),
                            now: NOW,
                            body: "",
                        },
                    )
                };
                let at_served = ask(&mut served);
                let document: Value = serde_json::from_str(&at_served.body).unwrap();
                assert_eq!(
                    (at_served.status, document["code"].as_str()),
                    (404, Some("question.not_found")),
                    "{path}?{query} as {auth:?}"
                );
                if auth.is_none() {
                    assert_eq!(at_served, ask(&mut vacant), "{path}?{query}");
                }
            }
        }
    }
}

/// D84: a tenant cell publishes no questions, so an ad-hoc question is the
/// base's 403 whatever its body (malformed included), on a vacant scope and
/// a provisioned one alike (#1297 r4 should 1).
#[test]
fn an_ad_hoc_question_is_the_base_refusal_on_a_tenant_cell() {
    let directory = tempfile::tempdir().unwrap();
    let mut served = provisioned(
        &directory.path().join("served.sqlite"),
        &with_policy(&["admin"]),
    );
    let mut vacant = opened(&directory.path().join("vacant.sqlite"));
    for path in ["/v1/questions:evaluate", "/v1/questions:trace"] {
        for body in ["not json", "{}", r#"{"question":{"name":"q"}}"#] {
            for auth in [None, Some(user("admin")), Some(user("viewer"))] {
                let ask = |cell: &mut Cell| {
                    cell.tenant.handle(
                        &cell.tokens,
                        &Request {
                            method: "POST",
                            path,
                            query: QUERY,
                            authorization: auth.as_deref(),
                            now: NOW,
                            body,
                        },
                    )
                };
                let at_served = ask(&mut served);
                let document: Value = serde_json::from_str(&at_served.body).unwrap();
                assert_eq!(
                    (at_served.status, document["code"].as_str()),
                    (403, Some("authz.denied")),
                    "{path} {body} as {auth:?}"
                );
                if auth.is_none() {
                    assert_eq!(at_served, ask(&mut vacant), "{path} {body}");
                }
            }
        }
    }
}
