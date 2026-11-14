//! DIAG-DISCLOSE-1 on a hosted cell (#1353): a governed action's refusal
//! detail is disclosed by the PDP under the declared `REFUSAL_DISCLOSERS`
//! policy, never by an endpoint or a client claim.
//!
//! The documents pack is provisioned on a tenant cell and its scans are loaded
//! through the cell's own mapping. Invoice INV-0041 prints lines 18.50 +
//! 12.00 and a total of 30.50; a correction of its total to 30.00 breaks
//! `canon.ItemizedDocument.total_matches_lines`, the canon concept's law the
//! scanned document inherits (and, as its journal entry JE-0041 books 30.50,
//! `docs.ScannedDocument.total_matches_ledger`). Every role the pack lets call
//! the correction is refused the same way and nothing commits, but only an
//! admin or owner reads which law refused, on which document, with which
//! values. A member (analyst) gets the redacted refusal, and a viewer (guest),
//! who may not call it, the one opaque action denial.
//!
//! Refusal-path pins on one decision of the server's permission system (D79).
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

    /// A correction of INV-0041's total to `total` by `role`.
    fn correct(&mut self, role: &str, total: &str) -> (u16, Value) {
        let body = json!({ "idempotencyKey": format!("inv-0041-{role}-{total}"),
            "input": { "id": "SCAN-INV-0041", "currency": "USD", "total": total,
                       "correction_reason": "typo",
                       "correction_evidence": "SCAN-INV-0041~total" } });
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

#[test]
fn only_admins_and_owners_read_a_law_refusal() {
    let directory = tempfile::tempdir().unwrap();
    let mut cell = loaded(&directory.path().join("a.sqlite"));

    for role in ["admin", "owner"] {
        let (status, refusal) = cell.correct(role, "30.00");
        assert_eq!(status, 422, "{role}: {refusal}");
        let reason = &refusal["reason"];
        // The ledger law or the lines law, whichever the kernel checks first:
        // both govern the document, and 30.00 breaks both.
        assert!(
            reason["law"]
                .as_str()
                .is_some_and(|law| LAWS.contains(&law)),
            "{role}: {refusal}"
        );
        assert!(
            reason["subject"].to_string().contains("SCAN-INV-0041"),
            "{role}: {refusal}"
        );
        // The refused total and the values the document holds it against:
        // its booked entry's amount, or every line amount the law summed.
        assert_eq!(reason["values"]["total"], json!(30), "{role}: {refusal}");
        assert!(
            reason["values"]["ledger_entry.amount"] == json!(30.5)
                || reason["values"]["lines.amount"] == json!([18.5, 12]),
            "{role}: {refusal}"
        );
    }

    // A member may call the correction and is refused the same way, without
    // the law, its subject or its values.
    let (status, member) = cell.correct("member", "30.00");
    let (admin_status, admin) = cell.correct("admin", "30.00");
    assert_eq!(status, admin_status, "{member}");
    assert_eq!(member["code"], admin["code"], "{member}");
    assert!(member.get("reason").is_none_or(Value::is_null), "{member}");
    let text = member.to_string();
    assert!(
        LAWS.iter().all(|law| !text.contains(law)) && !text.contains("30.5"),
        "{member}"
    );

    // A viewer may not call it: the one opaque denial, no detail.
    let (status, viewer) = cell.correct("viewer", "30.00");
    assert_eq!(status, 403, "{viewer}");
    assert!(viewer.get("reason").is_none_or(Value::is_null), "{viewer}");
    assert!(
        LAWS.iter().all(|law| !viewer.to_string().contains(law)),
        "{viewer}"
    );

    // Nothing above committed: the consistent correction is the first.
    let (status, receipt) = cell.correct("admin", "30.50");
    assert_eq!(status, 200, "{receipt}");
    assert_ne!(receipt["outcome"], "refused", "{receipt}");
}
