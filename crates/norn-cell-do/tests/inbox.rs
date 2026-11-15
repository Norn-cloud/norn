//! INBOX-1: `GET /v1/inbox` on a hosted cell with the documents pack, under
//! the canon law `canon.ItemizedDocument.total_matches_lines` and the pack's
//! ledger law, as Studio's Home and the MCP surface read it.
//!
//! - An admin sees INV-0042 in its law's group, with the finding in words
//!   ("total 720.00 ≠ lines 72.00") and the fix the server derived from the
//!   law: `docs.scanned_document.correct` with the total 72.00, the record's
//!   currency and the evidence the total was read from. The fix, called with
//!   a reason, commits, and INV-0042 leaves the inbox.
//! - A member lists the held records by id only: no finding, no values, no
//!   fix (the findings read needs Sources). A viewer, who reads no values,
//!   sees no held-back group at all, and a role the cell grants nothing sees
//!   nothing either.
//! - With the vendor marked `pii.contact`, every caller sees the vendor's
//!   pseudonym, never its name.
//! - A connection whose connector is not installed is not set up, so it is
//!   not a failed source: only a genuinely failed sync is listed and counted.
//!
//! Correspondence pins on the route (D79): each names one gate the inbox
//! summarizes; the fix's arithmetic is a property test in `norn-api`.
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
const LINES_LAW: &str = "canon.ItemizedDocument.total_matches_lines";
const LEDGER_LAW: &str = "docs.ScannedDocument.total_matches_ledger";
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

/// The documents pack with the core and canon artifacts it names, as
/// `held_back.rs` provisions it; with `marked`, the scanned document's vendor
/// carries `pii.contact` and the cell declares the privacy set.
fn documents(marked: bool) -> Vec<Value> {
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
    documents.extend(paths.iter().map(|path| {
        let mut artifact = artifact(path);
        if marked && artifact["artifactId"] == "docs.ScannedDocument.entity" {
            for property in artifact["payload"]["objectType"]["properties"]
                .as_array_mut()
                .unwrap()
            {
                if property["id"] == "vendor" {
                    property["markings"] = json!(["pii.contact"]);
                }
            }
        }
        artifact
    }));
    documents.push(artifact(
        &canon.join("canon.ItemizedDocument.total_matches_lines.law.json"),
    ));
    if marked {
        let privacy = root().join("registry/privacy/artifacts");
        documents.insert(0, artifact(&privacy.join("privacy.markings.entity.json")));
        documents.push(artifact(&privacy.join("privacy.reveal.policy.json")));
    }
    documents
}

struct Cell {
    tenant: Tenant<NativeEngine>,
    tokens: TokenPrincipals,
}

impl Cell {
    fn send(&mut self, method: &str, path: &str, auth: &str, body: &str) -> Response {
        self.tenant.handle(
            &self.tokens,
            &Request {
                method,
                path,
                query: QUERY,
                authorization: Some(auth),
                now: NOW,
                body,
            },
        )
    }

    /// The caller's inbox, and its body as text.
    fn inbox(&mut self, role: &str) -> (u16, Value, String) {
        let response = self.send("GET", "/v1/inbox", &user(role), "");
        let body = serde_json::from_str(&response.body).unwrap_or(Value::Null);
        (response.status, body, response.body)
    }
}

/// A tenant cell provisioned by its owner with the documents pack, its scans
/// loaded by an admin through the cell's own mapping.
fn loaded(path: &Path, marked: bool) -> Cell {
    loaded_with(path, marked, "[]", &[]).0
}

/// [`loaded`] with the connections `sources` (`NORN_SOURCES`) and `extra`
/// artifacts provisioned after the pack; the cell's connect module too.
fn loaded_with(path: &Path, marked: bool, sources: &str, extra: &[Value]) -> (Cell, Connect) {
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
        "artifacts": documents(marked).into_iter().chain(extra.iter().cloned()).collect::<Vec<_>>(),
    })
    .to_string();
    let provisioned = cell.send("POST", PROVISION_PATH, &user("owner"), &body);
    assert_eq!(provisioned.status, 200, "{}", provisioned.body);

    let vars = json!({
        "NORN_S3_ENDPOINT": "https://storage.example.test",
        "NORN_S3_BUCKET": "uploads-bucket",
        "NORN_SECRET_S3_ACCESS_KEY_ID": "key-id",
        "NORN_SECRET_S3_SECRET_ACCESS_KEY": "secret",
    });
    let mut connect = Connect::new(sources, &vars.to_string()).expect("configuration");
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
    (cell, connect)
}

/// The held-back group of `law` in an inbox.
fn group<'a>(inbox: &'a Value, law: &str) -> Option<&'a Value> {
    inbox["items"]
        .as_array()?
        .iter()
        .find(|item| item["kind"] == "held_back" && item["ruleRef"]["lawId"] == law)
}

/// The record `id` of a group.
fn listed<'a>(group: &'a Value, id: &str) -> Option<&'a Value> {
    group["records"]
        .as_array()?
        .iter()
        .find(|record| record["id"] == id)
}

fn section<'a>(inbox: &'a Value, kind: &str) -> &'a Value {
    inbox["sections"]
        .as_array()
        .unwrap()
        .iter()
        .find(|section| section["kind"] == kind)
        .unwrap()
}

#[test]
fn held_back_records_are_grouped_by_law_with_the_fix_only_for_who_may_make_it() {
    let directory = tempfile::tempdir().unwrap();
    let mut cell = loaded(&directory.path().join("a.sqlite"), false);

    // An admin: the law's group, the finding in words and the derived fix.
    let (status, inbox, _) = cell.inbox("admin");
    assert_eq!(status, 200, "{inbox}");
    let lines = group(&inbox, LINES_LAW).expect("the lines law's group");
    assert_eq!(lines["objectType"], "docs.ScannedDocument", "{lines}");
    assert_eq!(
        lines["target"], "/objects/docs.ScannedDocument?population=held_back",
        "{lines}"
    );
    assert!(lines["count"].as_u64().unwrap() >= 1, "{lines}");
    let record = listed(lines, "SCAN-INV-0042").expect("INV-0042 is held by the lines law");
    assert_eq!(
        record["target"],
        "/sources/docs.ScannedDocument/SCAN-INV-0042"
    );
    assert_eq!(
        record["finding"]["words"], "total 720.00 ≠ lines 72.00",
        "{record}"
    );
    assert_eq!(record["values"]["total"], json!(720), "{record}");
    let fix = &record["suggestedFix"];
    assert_eq!(fix["actionId"], "docs.scanned_document.correct", "{record}");
    assert_eq!(fix["label"], "Use 72.00", "{record}");
    assert_eq!(fix["property"], "total", "{record}");
    assert_eq!(
        fix["arguments"],
        json!({ "id": "SCAN-INV-0042", "total": "72.00", "currency": "USD",
                "correction_evidence": "SCAN-INV-0042~total" }),
        "{record}"
    );
    assert_eq!(fix["needs"], json!(["correction_reason"]), "{record}");
    let ledger = group(&inbox, LEDGER_LAW).expect("the ledger law's group");
    let disputed = listed(ledger, "SCAN-INV-0043").expect("INV-0043 is held by the ledger law");
    // "Use 72.00" under the ledger law would break the lines law (its line still reads
    // 720): the server tries the fix against the record's other laws and offers none.
    assert!(disputed.get("suggestedFix").is_none(), "{disputed}");
    let others = disputed["otherFindings"]
        .as_array()
        .expect("the laws that disagree");
    assert!(!others.is_empty(), "{disputed}");
    // INV-0042's fix keeps every other law it takes, so it is still offered (above).
    let held = section(&inbox, "held_back");
    assert!(held["count"].as_u64().unwrap() >= 2, "{held}");
    assert_eq!(held["approximate"], false, "{held}");

    // A member lists the held records, and nothing value-derived.
    let (status, member, _) = cell.inbox("member");
    assert_eq!(status, 200, "{member}");
    let record = listed(group(&member, LINES_LAW).unwrap(), "SCAN-INV-0042").unwrap();
    assert_eq!(
        record
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>(),
        std::collections::BTreeSet::from(["id", "objectType", "target"]),
        "{record}"
    );

    // A viewer reads no values: quarantine is value-level, so no group.
    let (status, viewer, body) = cell.inbox("viewer");
    assert_eq!(status, 200, "{viewer}");
    assert!(group(&viewer, LINES_LAW).is_none(), "{viewer}");
    assert_eq!(section(&viewer, "held_back")["count"], 0, "{viewer}");
    assert!(!body.contains("SCAN-INV"), "{body}");
    assert!(!body.contains("suggestedFix"), "{body}");

    // A role the cell grants nothing reads no type: nothing of them, not even a count.
    let (status, guest, body) = cell.inbox("guest");
    if status == 200 {
        assert_eq!(section(&guest, "held_back")["count"], 0, "{guest}");
    } else {
        assert_eq!(status, 403, "{guest}");
    }
    assert!(!body.contains("SCAN-INV"), "{body}");

    // "Use 72.00": the fix as the server computed it, with the caller's reason.
    let mut input = fix["arguments"].clone();
    input["correction_reason"] = json!("The scan prints 72.00.");
    let correction = json!({ "idempotencyKey": "inbox-inv-0042", "input": input }).to_string();
    let receipt = cell.send(
        "POST",
        "/v1/actions/docs.scanned_document.correct",
        &user("admin"),
        &correction,
    );
    assert_eq!(receipt.status, 200, "{}", receipt.body);
    let (_, inbox, _) = cell.inbox("admin");
    assert!(
        group(&inbox, LINES_LAW).is_none_or(|lines| listed(lines, "SCAN-INV-0042").is_none()),
        "INV-0042 is released: {inbox}"
    );
    assert!(
        listed(group(&inbox, LEDGER_LAW).unwrap(), "SCAN-INV-0043").is_some(),
        "{inbox}"
    );
}

#[test]
fn a_marked_name_is_a_pseudonym_for_every_caller_and_the_fix_stays() {
    let directory = tempfile::tempdir().unwrap();
    let mut cell = loaded(&directory.path().join("a.sqlite"), true);
    for role in ["admin", "member"] {
        let (status, inbox, body) = cell.inbox(role);
        assert_eq!(status, 200, "{role}: {inbox}");
        for vendor in ["Northgate Packaging", "Meridian Freight"] {
            assert!(!body.contains(vendor), "{role}: {body}");
        }
        assert!(group(&inbox, LINES_LAW).is_some(), "{role}: {inbox}");
    }
    let (_, inbox, _) = cell.inbox("admin");
    let record = listed(group(&inbox, LINES_LAW).unwrap(), "SCAN-INV-0042").unwrap();
    assert!(
        record["values"]["vendor"]
            .as_str()
            .is_some_and(|vendor| vendor.starts_with("Customer #")),
        "{record}"
    );
    assert_eq!(record["masked"]["vendor"]["mode"], "pseudonym", "{record}");
    // The total and its lines are unmarked, so the fix is still derived.
    assert_eq!(
        record["finding"]["words"], "total 720.00 ≠ lines 72.00",
        "{record}"
    );
    assert_eq!(
        record["suggestedFix"]["arguments"]["total"], "72.00",
        "{record}"
    );
}

/// INBOX-SOURCES-1: a connection whose connector is not installed is not set
/// up (Studio's Sources page says so); it has not failed, so the inbox
/// neither lists nor counts it. A connection whose read was refused has
/// failed and stays listed, with the section's count matching.
#[test]
fn a_connection_whose_connector_is_not_installed_is_not_a_failed_source() {
    let directory = tempfile::tempdir().unwrap();
    let sources = json!([
        { "id": "docs.scans", "system": "extractor", "label": "Scanned documents",
          "connector": "docs.scans.connector", "mapping": "docs.scans.mapping",
          "instance": "demo", "baseUrl": "https://extractor.example.test" },
        { "id": "erpnext.sandbox", "system": "erpnext", "label": "ERPNext sandbox",
          "connector": "erpnext.sandbox.connector", "mapping": "erpnext.sandbox.mapping",
          "instance": "sandbox", "baseUrl": "https://erp.example.test" },
    ])
    .to_string();
    let connector =
        artifact(&root().join("registry/packs/documents-pull/artifacts/docs.scans.connector.json"));
    let (mut cell, mut connect) = loaded_with(
        &directory.path().join("a.sqlite"),
        false,
        &sources,
        &[connector],
    );
    let admin = user("admin");
    for id in ["docs.scans", "erpnext.sandbox"] {
        let path = format!("/v1/sources/{id}:sync");
        let request = Request {
            method: "POST",
            path: &path,
            query: QUERY,
            authorization: Some(&admin),
            now: NOW,
            body: "",
        };
        let routed = connect
            .route(
                cell.tenant.serving_host(),
                &cell.tokens,
                &request,
                NOW * 1000,
            )
            .expect("a connect route");
        assert_eq!(routed.response.status, 202, "{}", routed.response.body);
        let mut answer: Value = serde_json::from_str(&connect.step(
            cell.tenant.serving_host(),
            id,
            None,
            (NOW * 1000, None),
        ))
        .unwrap();
        // The installed connector asks the extractor, which refuses the read.
        if answer.get("request").is_some() {
            answer = serde_json::from_str(&connect.step(
                cell.tenant.serving_host(),
                id,
                Some((401, b"{}")),
                (NOW * 1000, None),
            ))
            .unwrap();
        }
        assert_eq!(answer["done"]["outcome"], "failed", "{id}: {answer}");
    }

    let (status, inbox, _) = cell.inbox("admin");
    assert_eq!(status, 200, "{inbox}");
    let failed: Vec<&Value> = inbox["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|item| item["kind"] == "source_error")
        .collect();
    assert_eq!(failed.len(), 1, "{inbox}");
    assert_eq!(failed[0]["id"], "docs.scans", "{inbox}");
    assert_eq!(
        failed[0]["values"]["last_error_code"], "source.http_status",
        "{inbox}"
    );
    let section = section(&inbox, "source_error");
    assert_eq!(section["count"], 1, "{section}");
    assert_eq!(section["shown"], 1, "{section}");
}
