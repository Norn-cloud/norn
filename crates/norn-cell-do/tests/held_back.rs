//! The pitch's invoice moment on a hosted cell, under the canon concept's
//! law (`canon.ItemizedDocument.total_matches_lines`, LAW-INTERFACE-1): a
//! document the law holds back is read and corrected through the cell's own
//! routes, as Studio's quarantined-record page does.
//!
//! The documents pack is provisioned on a tenant cell and its scans are loaded
//! through the cell's own mapping. INV-0042 prints lines 30.00 + 40.00 + 2.00
//! and INV-0043 one line of 72.00, but the import read both totals (and
//! INV-0043's line) as 720. The lines law holds INV-0042 back, and the ledger
//! law (its journal entry books 72.00) INV-0043. Then, also after the
//! host reopens the cell:
//!
//! - the plain read of each is the one 404 (held-back values stay out of
//!   every answer), while an admin reads it as quarantined, with its open
//!   findings, through its findings and its history;
//! - a correction of INV-0042 to 70.00 is refused with its reason (the law
//!   and its values) for an admin, never the opaque denial; a member is
//!   refused the same way without the detail, and a viewer, whom the pack does
//!   not let correct, gets the one opaque denial;
//! - "Use 72.00" commits: INV-0042's total, and INV-0043's line with its
//!   total; each is released, and its plain read serves the corrected values.
//!
//! A host opened without the stage's lineage key cannot commit a receipt: it
//! tells a verified member so (typed, retryable), never the opaque 403.
//!
//! Correspondence pins on the governed correction path (D79).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    reason = "route correspondence"
)]

use std::path::{Path, PathBuf};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signer, SigningKey};
use norn_api::provision::PROVISION_PATH;
use norn_api::scope::CellScope;
use norn_cell::StoreOptions;
use norn_cell::engine::NativeEngine;
use norn_cell_do::base;
use norn_cell_do::connect::Connect;
use norn_cell_do::policy::DEMO_GRANTS;
use norn_cell_do::principal::TokenPrincipals;
use norn_cell_do::server::{Request, Response};
use norn_cell_do::tenant::Tenant;
use serde_json::{Value, json};

const ISSUER: &str = "https://studio-preview.example";
const NOW: u64 = 1_800_000_000;
const QUERY: &str = "orgId=org_a&environmentId=prod";
const CORRECT: &str = "docs.scanned_document.correct";
const LAWS: [&str; 2] = [
    "canon.ItemizedDocument.total_matches_lines",
    "docs.ScannedDocument.total_matches_ledger",
];
const HANDLE_KEY: [u8; 32] = [7; 32];

fn key() -> SigningKey {
    SigningKey::from_bytes(&[1; 32])
}

fn jwks() -> String {
    json!({ "keys": [{ "kty": "OKP", "crv": "Ed25519", "alg": "EdDSA", "kid": "k1",
                       "x": URL_SAFE_NO_PAD.encode(key().verifying_key().as_bytes()) }] })
    .to_string()
}

fn user(role: &str) -> String {
    let claims = json!({ "iss": ISSUER, "aud": "norn-host", "sub": format!("user-{role}"),
            "iat": NOW - 10, "exp": NOW + 290,
            "norn": { "kind": "user", "org": "org_a", "role": role, "amr": "password" } });
    let head = URL_SAFE_NO_PAD.encode(json!({ "alg": "EdDSA", "kid": "k1" }).to_string());
    let body = URL_SAFE_NO_PAD.encode(claims.to_string());
    let signature = key().sign(format!("{head}.{body}").as_bytes());
    format!(
        "Bearer {head}.{body}.{}",
        URL_SAFE_NO_PAD.encode(signature.to_bytes())
    )
}

fn scope() -> CellScope {
    CellScope {
        org_id: "org_a".into(),
        environment_id: "prod".into(),
    }
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn artifact(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).expect("artifact")).expect("JSON")
}

/// The documents pack with the core and canon artifacts it names, interfaces
/// and types before the artifacts that use them.
fn documents() -> Vec<Value> {
    let core = root().join("registry/core/artifacts");
    let mut documents: Vec<Value> = [
        "norn.SourceEvidence.interface.json",
        "norn.SourceDocument.interface.json",
        "norn.ObservedLinkClaim.entity.json",
    ]
    .iter()
    .map(|name| artifact(&core.join(name)))
    .collect();
    let mut paths: Vec<PathBuf> =
        std::fs::read_dir(root().join("registry/packs/documents/artifacts"))
            .expect("pack artifacts")
            .map(|entry| entry.expect("entry").path())
            .collect();
    paths.sort();
    paths.sort_by_key(|path| {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if name.ends_with(".interface.json") {
            0
        } else if name.ends_with(".entity.json") {
            1
        } else {
            2
        }
    });
    // The canon itemized-document concept the pack's documents implement,
    // and its law (LAW-INTERFACE-1), after the types it governs.
    let canon = root().join("registry/packs/canon/artifacts");
    documents.splice(
        3..3,
        [
            "canon.ItemizedDocument.interface.json",
            "canon.LineItem.interface.json",
        ]
        .iter()
        .map(|name| artifact(&canon.join(name))),
    );
    documents.extend(paths.iter().map(|path| artifact(path)));
    documents.push(artifact(
        &canon.join("canon.ItemizedDocument.total_matches_lines.law.json"),
    ));
    documents
}

struct Cell {
    tenant: Tenant<NativeEngine>,
    tokens: TokenPrincipals,
}

impl Cell {
    fn call(&mut self, path: &str, auth: &str, body: &str) -> Response {
        self.tenant.handle(
            &self.tokens,
            &Request {
                method: "POST",
                path,
                query: QUERY,
                authorization: Some(auth),
                now: NOW,
                body,
            },
        )
    }

    /// A correction of INV-0042's total to `total` by `role`.
    fn correct(&mut self, role: &str, total: &str) -> (u16, Value) {
        let body = json!({ "idempotencyKey": format!("inv-0042-{role}-{total}"),
            "input": { "id": "SCAN-INV-0042", "currency": "USD", "total": total,
                       "correction_reason": "typo",
                       "correction_evidence": "SCAN-INV-0042~total" } });
        let response = self.call(
            &format!("/v1/actions/{CORRECT}"),
            &user(role),
            &body.to_string(),
        );
        (
            response.status,
            serde_json::from_str(&response.body).unwrap_or(Value::Null),
        )
    }
}

/// A tenant cell provisioned by its owner with the documents pack, its scans
/// loaded by an admin through the cell's own mapping.
fn loaded(path: &Path) -> Cell {
    loaded_with(path, Some(&HANDLE_KEY))
}

/// The tenant cell stored at `path`, opened as the host opens it, keyed
/// with `handle_key` (none: a host opened without the stage's lineage key).
fn opened(path: &Path, handle_key: Option<&[u8]>) -> Cell {
    let file = path.to_owned();
    let tenant = Tenant::open(
        scope(),
        &DEMO_GRANTS,
        Box::new(move || {
            NativeEngine::open(&file, StoreOptions::default()).map_err(|error| error.to_string())
        }),
        handle_key,
    )
    .unwrap();
    let tokens = TokenPrincipals::new(ISSUER, &jwks(), scope(), tenant.caller(), NOW).unwrap();
    Cell { tenant, tokens }
}

/// As [`loaded`], on a host keyed with `handle_key`.
fn loaded_with(path: &Path, handle_key: Option<&[u8]>) -> Cell {
    let mut cell = opened(path, handle_key);
    let body = json!({
        "baseSha256": base::trusted_base().unwrap().sha256(),
        "artifacts": documents(),
    })
    .to_string();
    let provisioned = cell.call(PROVISION_PATH, &user("owner"), &body);
    assert_eq!(provisioned.status, 200, "{}", provisioned.body);
    assert_eq!(cell.tenant.action_policy_error(), None);

    let vars = json!({
        "NORN_S3_ENDPOINT": "https://storage.example.test",
        "NORN_S3_BUCKET": "uploads-bucket",
        "NORN_SECRET_S3_ACCESS_KEY_ID": "key-id",
        "NORN_SECRET_S3_SECRET_ACCESS_KEY": "secret",
    });
    let mut connect = Connect::new("[]", &vars.to_string()).expect("configuration");
    let scans = std::fs::read_to_string(root().join("fixtures/documents/scans.snapshot.json"))
        .expect("snapshot");
    let admin = user("admin");
    let request = Request {
        method: "POST",
        path: "/v1/observations:load",
        query: QUERY,
        authorization: Some(&admin),
        now: NOW,
        body: &scans,
    };
    let routed = connect
        .route(
            cell.tenant.serving_host(),
            &cell.tokens,
            &request,
            NOW * 1000,
        )
        .expect("a connect route");
    assert_eq!(routed.response.status, 200, "{}", routed.response.body);
    let landed: Value = serde_json::from_str(&routed.response.body).unwrap();
    assert_eq!(landed["landed"], landed["rows"], "{landed}");
    cell
}

impl Cell {
    fn get(&mut self, path: &str, role: &str) -> (u16, Value) {
        let auth = user(role);
        let response = self.tenant.handle(
            &self.tokens,
            &Request {
                method: "GET",
                path,
                query: QUERY,
                authorization: Some(&auth),
                now: NOW,
                body: "",
            },
        );
        (
            response.status,
            serde_json::from_str(&response.body).unwrap_or(Value::Null),
        )
    }

    /// The held-back document `id`: its plain read refuses, its findings and
    /// history serve it to an admin.
    fn held_back(&mut self, id: &str) {
        let document = format!("/v1/objects/docs.ScannedDocument/{id}");
        let (status, plain) = self.get(&document, "admin");
        assert_eq!(status, 404, "{id} is held back: {plain}");
        let (status, findings) = self.get(&format!("{document}/findings"), "admin");
        assert_eq!(status, 200, "{id}: {findings}");
        assert_eq!(findings["quarantined"], true, "{id}: {findings}");
        let text = findings.to_string();
        assert!(
            LAWS.iter().any(|law| text.contains(law)),
            "{id}: {findings}"
        );
        let (status, history) = self.get(&format!("{document}/history"), "admin");
        assert_eq!(status, 200, "{id}: {history}");
        assert!(
            history.to_string().contains("720"),
            "{id}: the imported total: {history}"
        );
    }

    /// The released document `id`: its plain read serves `total`.
    fn released(&mut self, id: &str, total: i64) {
        let document = format!("/v1/objects/docs.ScannedDocument/{id}");
        let (status, released) = self.get(&document, "member");
        assert_eq!(status, 200, "{id} is released: {released}");
        assert_eq!(
            released["values"]["total"],
            json!(total),
            "{id}: {released}"
        );
    }
}

#[test]
fn a_held_back_invoice_is_read_and_corrected_through_its_own_routes() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("a.sqlite");
    let mut cell = loaded(&path);
    for id in ["SCAN-INV-0042", "SCAN-INV-0043"] {
        cell.held_back(id);
    }
    // The host reopens the cell (a restart, or the next request's reload).
    drop(cell);
    let mut cell = opened(&path, Some(&HANDLE_KEY));
    for id in ["SCAN-INV-0042", "SCAN-INV-0043"] {
        cell.held_back(id);
    }

    let (status, refusal) = cell.correct("admin", "70.00");
    assert_eq!(status, 422, "{refusal}");
    assert!(
        refusal["reason"]["law"]
            .as_str()
            .is_some_and(|law| LAWS.contains(&law)),
        "{refusal}"
    );
    assert_eq!(refusal["reason"]["values"]["total"], json!(70), "{refusal}");
    let (status, member) = cell.correct("member", "70.00");
    assert_eq!(status, 422, "{member}");
    assert!(member.get("reason").is_none_or(Value::is_null), "{member}");
    let (status, viewer) = cell.correct("viewer", "70.00");
    assert_eq!(
        (status, &viewer["code"]),
        (403, &json!("authz.denied")),
        "{viewer}"
    );

    // "Use 72.00".
    let (status, receipt) = cell.correct("admin", "72.00");
    assert_eq!(status, 200, "{receipt}");
    assert_eq!(receipt["outcome"], "committed", "{receipt}");
    cell.released("SCAN-INV-0042", 72);

    // INV-0043's line was misread too: its line and its total, together.
    let body = json!({ "idempotencyKey": "inv-0043",
        "input": { "id": "SCAN-INV-0043~line-1", "currency": "USD", "amount": "72.00",
                   "document_total": "72.00",
                   "correction_reason": "Line and total both read 720; the scan says 72.00.",
                   "correction_evidence": "SCAN-INV-0043~lines.1.amount" } });
    let response = cell.call(
        "/v1/actions/docs.document_line.correct",
        &user("admin"),
        &body.to_string(),
    );
    assert_eq!(response.status, 200, "{}", response.body);
    cell.released("SCAN-INV-0043", 72);
}

/// A host opened without the stage's lineage key keys no receipt, so no
/// governed call can commit. A verified member calling an action the policy
/// governs is told so (typed, retryable), whatever the role; another
/// organization's admin, or an action the policy does not govern, still gets
/// the one opaque denial. The held-back document stays readable for review.
#[test]
fn a_host_without_its_lineage_key_says_so_to_its_members() {
    let directory = tempfile::tempdir().unwrap();
    let mut cell = loaded_with(&directory.path().join("a.sqlite"), None);
    for role in ["admin", "member", "viewer"] {
        let (status, refusal) = cell.correct(role, "72.00");
        assert_eq!(status, 503, "{role}: {refusal}");
        assert_eq!(
            refusal["code"], "action.receipts_unconfigured",
            "{role}: {refusal}"
        );
        assert_eq!(refusal["retryable"], true, "{role}: {refusal}");
    }
    let body = json!({ "idempotencyKey": "k", "input": {} }).to_string();
    let unknown = cell.call("/v1/actions/docs.no_such_action", &user("admin"), &body);
    assert_eq!(unknown.status, 403, "{}", unknown.body);
    let outsider = cell.call(&format!("/v1/actions/{CORRECT}"), &outsider(), &body);
    assert_eq!(outsider.status, 403, "{}", outsider.body);
    assert_eq!(outsider.body, unknown.body);
    cell.held_back("SCAN-INV-0042");
}

/// Another organization's admin.
fn outsider() -> String {
    let claims = json!({ "iss": ISSUER, "aud": "norn-host", "sub": "user-admin-of-org_b",
            "iat": NOW - 10, "exp": NOW + 290,
            "norn": { "kind": "user", "org": "org_b", "role": "admin", "amr": "password" } });
    let head = URL_SAFE_NO_PAD.encode(json!({ "alg": "EdDSA", "kid": "k1" }).to_string());
    let body = URL_SAFE_NO_PAD.encode(claims.to_string());
    let signature = key().sign(format!("{head}.{body}").as_bytes());
    format!(
        "Bearer {head}.{body}.{}",
        URL_SAFE_NO_PAD.encode(signature.to_bytes())
    )
}

impl Cell {
    /// One MCP `tools/call` (or `tools/list`, with `name` empty) as `role`.
    fn mcp(&mut self, role: &str, name: &str, arguments: &Value) -> Value {
        let message = if name.is_empty() {
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {"limit": 100}})
        } else {
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": name, "arguments": arguments}})
        };
        let response = self.call("/v1/mcp", &user(role), &message.to_string());
        assert_eq!(response.status, 200, "{}", response.body);
        let body: Value = serde_json::from_str(&response.body).unwrap();
        body["result"].clone()
    }
}

/// The same moment through MCP, as an agent drives it (#520): the held record
/// and why, from the same routes and policy; the correction refused at 70.00
/// and committed at 72.00; the actor is the server's to fill, never an input.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one stage moment, read top to bottom"
)]
fn an_agent_reads_why_an_invoice_is_held_and_corrects_it_through_mcp() {
    let directory = tempfile::tempdir().unwrap();
    let mut cell = loaded(&directory.path().join("a.sqlite"));
    let listed = cell.mcp("admin", "", &json!({}));
    let tools: Vec<&Value> = listed["tools"].as_array().unwrap().iter().collect();
    let names: Vec<&str> = tools
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    for expected in [
        "norn_list_held",
        "norn_get_findings",
        "norn_explain",
        "norn_plan_definitions",
        "norn_apply_definitions",
    ] {
        assert!(
            names.contains(&expected),
            "{expected} missing from {names:?}"
        );
    }
    let correct = tools
        .iter()
        .find(|tool| tool["_meta"]["norn/actionId"] == CORRECT)
        .expect("the correction is a tool");
    let tool = correct["name"].as_str().unwrap().to_owned();
    assert!(
        correct["inputSchema"]["properties"]
            .get("corrected_by")
            .is_none(),
        "{correct}"
    );
    // A member may not write definitions: no definition tools.
    let member = cell.mcp("member", "", &json!({}));
    assert!(!member.to_string().contains("norn_plan_definitions"));

    let held = cell.mcp(
        "admin",
        "norn_list_held",
        &json!({"type": "docs.ScannedDocument"}),
    );
    assert_eq!(held["isError"], false, "{held}");
    let (_, route) = cell.get("/v1/objects/docs.ScannedDocument:quarantined", "admin");
    let ids = |value: &Value| -> Vec<String> {
        value["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["id"].as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(ids(&held["structuredContent"]), ids(&route));
    let first = &held["structuredContent"]["items"][0];
    assert!(
        LAWS.iter()
            .any(|law| first["findings"].to_string().contains(law)),
        "{first}"
    );
    assert!(
        held["structuredContent"]["remedies"]
            .as_array()
            .unwrap()
            .iter()
            .any(|remedy| remedy == &json!(tool))
    );

    let id = "SCAN-INV-0042";
    let found = cell.mcp(
        "admin",
        "norn_get_findings",
        &json!({"type": "docs.ScannedDocument", "id": id}),
    );
    let (_, route) = cell.get(
        &format!("/v1/objects/docs.ScannedDocument/{id}/findings"),
        "admin",
    );
    assert_eq!(found["structuredContent"]["findings"], route["findings"]);
    assert_eq!(found["structuredContent"]["quarantined"], true);
    // A viewer is refused exactly as the route refuses them.
    let viewer = cell.mcp(
        "viewer",
        "norn_get_findings",
        &json!({"type": "docs.ScannedDocument", "id": id}),
    );
    let (_, refusal) = cell.get(
        &format!("/v1/objects/docs.ScannedDocument/{id}/findings"),
        "viewer",
    );
    if refusal.get("code").is_some() {
        assert_eq!(viewer["isError"], true, "{viewer}");
        assert_eq!(viewer["structuredContent"], refusal);
    }

    let input = |total: &str| {
        json!({"id": id, "currency": "USD", "total": total,
        "correction_reason": "the scan prints 72.00", "correction_evidence": "SCAN-INV-0042~total"})
    };
    let refused = cell.mcp("admin", &tool, &input("70.00"));
    assert_eq!(refused["isError"], true, "{refused}");
    assert_eq!(
        refused["structuredContent"]["code"], "core.laws.violated",
        "{refused}"
    );
    let explained = cell.mcp(
        "admin",
        "norn_explain",
        &json!({"code": "core.laws.violated"}),
    );
    assert_eq!(explained["structuredContent"]["known"], true, "{explained}");
    let spoofed = cell.mcp("admin", &tool, &json!({"id": id, "currency": "USD", "total": "72.00",
        "correction_reason": "x", "correction_evidence": "SCAN-INV-0042~total", "corrected_by": "user:someone-else"}));
    assert_eq!(spoofed["isError"], true, "{spoofed}");
    let accepted = cell.mcp("admin", &tool, &input("72.00"));
    assert_eq!(accepted["isError"], false, "{accepted}");
    assert_eq!(
        accepted["structuredContent"]["outcome"], "committed",
        "{accepted}"
    );
    cell.released(id, 72);
}
