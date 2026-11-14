//! The pitch's invoice moment on a hosted cell: a document a law holds back
//! is read and corrected through the cell's own routes.
//!
//! The documents pack is provisioned on a tenant cell and its scans are loaded
//! through the cell's own mapping. INV-0042 prints three lines that sum to
//! 72.00 but was read as 720; `docs.ScannedDocument.total_matches_lines` holds
//! it back. Then:
//!
//! - its plain read is the one 404 (held-back values stay out of every
//!   answer), while an admin reads it as quarantined, with its open finding,
//!   through its findings and its history;
//! - a correction to 70.00 is refused with its reason (the law and its
//!   values) for an admin, never the opaque denial; a member is refused the
//!   same way without the detail, and a viewer, whom the pack does not let
//!   correct, gets the one opaque denial;
//! - the correction to 72.00 commits, resolves the finding and releases the
//!   document: its plain read then serves the corrected total.
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
const LAW: &str = "docs.ScannedDocument.total_matches_";
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

/// The documents pack with the core artifacts it names, interfaces and types
/// before the artifacts that use them.
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
    documents.extend(paths.iter().map(|path| artifact(path)));
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

/// As [`loaded`], on a host keyed with `handle_key` (none: a host opened
/// without the stage's lineage key).
fn loaded_with(path: &Path, handle_key: Option<&[u8]>) -> Cell {
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
    let mut cell = Cell { tenant, tokens };
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
}

const DOCUMENT: &str = "/v1/objects/docs.ScannedDocument/SCAN-INV-0042";

#[test]
fn a_held_back_invoice_is_read_and_corrected_through_its_own_routes() {
    let directory = tempfile::tempdir().unwrap();
    let mut cell = loaded(&directory.path().join("a.sqlite"));

    let (status, plain) = cell.get(DOCUMENT, "admin");
    assert_eq!(status, 404, "held back: {plain}");
    let (status, findings) = cell.get(&format!("{DOCUMENT}/findings"), "admin");
    assert_eq!(status, 200, "{findings}");
    assert_eq!(findings["quarantined"], true, "{findings}");
    assert!(findings.to_string().contains(LAW), "{findings}");
    let (status, history) = cell.get(&format!("{DOCUMENT}/history"), "admin");
    assert_eq!(status, 200, "{history}");
    assert!(
        history.to_string().contains("720"),
        "the imported total: {history}"
    );

    let (status, refusal) = cell.correct("admin", "70.00");
    assert_eq!(status, 422, "{refusal}");
    assert!(
        refusal["reason"]["law"]
            .as_str()
            .is_some_and(|law| law.starts_with(LAW)),
        "{refusal}"
    );
    assert_eq!(refusal["reason"]["values"]["total"], json!(70), "{refusal}");
    let (status, member) = cell.correct("member", "70.00");
    assert_eq!(status, 422, "{member}");
    assert!(member.get("reason").is_none_or(Value::is_null), "{member}");
    let (status, viewer) = cell.correct("viewer", "70.00");
    assert_eq!(status, 403, "{viewer}");

    let (status, receipt) = cell.correct("admin", "72.00");
    assert_eq!(status, 200, "{receipt}");
    assert_eq!(receipt["outcome"], "committed", "{receipt}");
    let (status, released) = cell.get(DOCUMENT, "member");
    assert_eq!(status, 200, "released: {released}");
    assert_eq!(released["values"]["total"], json!(72), "{released}");
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
    let (status, findings) = cell.get(&format!("{DOCUMENT}/findings"), "admin");
    assert_eq!(
        (status, &findings["quarantined"]),
        (200, &json!(true)),
        "{findings}"
    );
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
