//! #1423, end to end on a hosted-style tenant: a type migration with
//! defaults for live rows, a backfill of the same rows with a bumped
//! `sourceVersion` (one page sent twice, as a client retries a page it lost
//! the answer to), a law change, more rows, and then a cold open. Every
//! admitted step must leave a cell that opens: before the fix the reopen was
//! refused with `cell.unopenable` ("event payload has no unique committed
//! value proof").
//!
//! The pack is a synthetic application-funnel shape: a lifecycle with two
//! initial states whose state the mapping chooses, nullable and decimal
//! members, and defaults. The rows are generated here; no real data.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::too_many_lines,
    clippy::arithmetic_side_effects,
    reason = "hosted reopen correspondence"
)]

use std::path::{Path, PathBuf};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signer, SigningKey};
use norn_api::cell_definitions::APPLY_PATH;
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
const HANDLE_KEY: [u8; 32] = [7; 32];
const QUERY: &str = "orgId=org_a&environmentId=prod";
const MAPPING: &str = "rehearsal.applications.mapping";
const APPLICATION: &str = "rehearsal.Application";
/// The pack after the migration (the incident's rev 16).
const PACK: &str = include_str!("fixtures/rehearsal.pack.json");
/// The members the migration adds, each with a default for live rows.
const ADDED: [&str; 4] = [
    "started_on",
    "completed_count",
    "application_count",
    "completion_rate",
];
/// Core evidence types the trusted base may lack (as a hosted load provisions them).
const CELL_OWNED: [&str; 3] = [
    "norn.ObservedLinkClaim.entity.json",
    "norn.SourceDocument.interface.json",
    "norn.SourceEvidence.interface.json",
];

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn key() -> SigningKey {
    SigningKey::from_bytes(&[1; 32])
}

fn jwks() -> String {
    json!({ "keys": [{ "kty": "OKP", "crv": "Ed25519", "alg": "EdDSA", "kid": "k1",
                       "x": URL_SAFE_NO_PAD.encode(key().verifying_key().as_bytes()) }] })
    .to_string()
}

fn user(role: &str) -> String {
    let claims = json!({ "iss": ISSUER, "aud": "norn-host", "sub": format!("{role}-of-org_a"),
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

/// The migrated pack (rev 16).
fn migrated() -> Vec<Value> {
    serde_json::from_str(PACK).unwrap()
}

/// The pack before the migration: without the added members, their
/// mappings and defaults, at the type's and mapping's earlier versions.
fn original() -> Vec<Value> {
    let mut artifacts = migrated();
    for artifact in &mut artifacts {
        let id = artifact["artifactId"].as_str().unwrap().to_owned();
        if id == format!("{APPLICATION}.entity") {
            artifact["version"] = json!("0.3.0");
            let properties = artifact["payload"]["objectType"]["properties"]
                .as_array_mut()
                .unwrap();
            properties.retain(|p| !ADDED.contains(&p["id"].as_str().unwrap()));
        } else if id == MAPPING {
            artifact["version"] = json!("0.4.0");
            let properties = artifact["payload"]["maps"][0]["properties"]
                .as_array_mut()
                .unwrap();
            properties.retain(|p| !ADDED.contains(&p["property"].as_str().unwrap()));
        }
        if id != MAPPING && id.starts_with(APPLICATION) && id.strip_suffix(".law").is_some() {
            artifact["version"] = json!("0.3.0");
        }
        for dependency in artifact["dependencies"].as_array_mut().unwrap() {
            if dependency["artifactId"] == format!("{APPLICATION}.entity") {
                dependency["version"] = json!("0.3.0");
            }
        }
    }
    artifacts
}

/// The law predicate change (the incident's rev 18).
fn law_changed() -> Vec<Value> {
    let mut artifacts = migrated();
    let law = artifacts
        .iter_mut()
        .find(|a| a["artifactId"] == format!("{APPLICATION}.steps_in_range.law"))
        .unwrap();
    let expression = law["payload"]["response"]["expression"]
        .as_str()
        .unwrap()
        .replace("<= 7", "<= 8");
    law["payload"]["response"]["expression"] = json!(expression);
    artifacts
}

/// The provisioning submission: the original pack, plus the cell-owned
/// evidence types the base lacks.
fn submission() -> String {
    let base = base::trusted_base().unwrap();
    let mut artifacts = original();
    for name in CELL_OWNED {
        let path = repo().join("registry/core/artifacts").join(name);
        let artifact: Value =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        if !base
            .ids()
            .contains(&artifact["artifactId"].as_str().unwrap().to_owned())
        {
            artifacts.push(artifact);
        }
    }
    json!({ "baseSha256": base.sha256(), "artifacts": artifacts }).to_string()
}

/// One synthetic application row: deterministic, and varied where the
/// mapping branches (stalled or started, null device and minutes, Yes/No).
fn row(n: usize) -> Value {
    let yes_no = |flag: bool| if flag { "Yes" } else { "No" };
    let devices = [
        Value::Null,
        json!("Android"),
        json!("iOS"),
        json!("Desktop"),
    ];
    let channel = ["Mobile app", "Web", "Assisted"][n % 3];
    let customer = ["New", "Returning"][n % 2];
    let minutes = if n.is_multiple_of(5) {
        Value::Null
    } else {
        json!(f64::from(u8::try_from(n % 90).unwrap()) / 10.0)
    };
    json!({
        "id": format!("APP-{n:04}"),
        "application_id": format!("APP-{n:04}"),
        "application_start_time": format!("2026-10-{:02} {:02}:15:00", 1 + n % 28, n % 24),
        "application_channel": channel,
        "device_type": devices[n % 4].clone(),
        "customer_type": customer,
        "steps_completed_10m": n % 8,
        "validation_errors_10m": n % 5,
        "document_upload_attempted_10m": yes_no(n.is_multiple_of(2)),
        "document_upload_success_10m": yes_no(n.is_multiple_of(4)),
        "active_minutes_10m": minutes,
        "returned_after_exit_10m": yes_no(n.is_multiple_of(3)),
        "network_interruptions_10m": n % 6,
        "support_opened_10m": yes_no(n.is_multiple_of(7)),
        "start_hour": n % 24,
        "weekend_start": yes_no(n.is_multiple_of(6)),
        "completed_within_24_hours": yes_no(!n.is_multiple_of(3)),
    })
}

struct Cell {
    tenant: Tenant<NativeEngine>,
    tokens: TokenPrincipals,
    connect: Connect,
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
        let vars = json!({
            "NORN_S3_ENDPOINT": "https://storage.example.test",
            "NORN_S3_BUCKET": "uploads-bucket",
            "NORN_S3_ACCESS_KEY_ID": "key-id",
            "NORN_S3_SECRET_ACCESS_KEY": "secret",
        });
        let connect = Connect::new(&scope(), "[]", &vars.to_string()).unwrap();
        Self {
            tenant,
            tokens,
            connect,
        }
    }

    fn call(&mut self, method: &str, path: &str, body: &str) -> Response {
        self.tenant.handle(
            &self.tokens,
            &Request {
                method,
                path,
                query: QUERY,
                authorization: Some(&user("owner")),
                now: NOW,
                body,
            },
        )
    }

    fn version(&mut self) -> u64 {
        let read = self.call("GET", "/v1/definitions", "");
        assert_eq!(read.status, 200, "{}", read.body);
        json_of(&read)["version"].as_u64().unwrap()
    }

    fn apply(&mut self, artifacts: &[Value]) {
        let expected = self.version();
        let body = json!({ "artifacts": artifacts, "expectedVersion": expected });
        let applied = self.call("POST", APPLY_PATH, &body.to_string());
        assert_eq!(applied.status, 200, "{}", applied.body);
        assert_eq!(self.version(), expected + 1);
    }

    /// One `POST /v1/observations:load` page; its answer.
    fn load(&mut self, rows: std::ops::Range<usize>, source_version: Option<&str>) -> Value {
        let snapshot = rows
            .map(|n| {
                let mut entry = json!({ "dataset": "applications", "record": row(n) });
                if let Some(version) = source_version {
                    entry["sourceVersion"] = json!(version);
                }
                entry
            })
            .collect::<Vec<_>>();
        let body = json!({ "source": "csv", "mapping": MAPPING, "connectorInstance": "dogfood",
                           "snapshot": snapshot })
        .to_string();
        let owner = user("owner");
        let host = self
            .tenant
            .serving_host()
            .expect("a provisioned cell serves");
        let routed = self
            .connect
            .route(
                Some(host),
                &self.tokens,
                &Request {
                    method: "POST",
                    path: "/v1/observations:load",
                    query: QUERY,
                    authorization: Some(&owner),
                    now: NOW,
                    body: &body,
                },
                0,
            )
            .expect("a connect route");
        assert_eq!(routed.response.status, 200, "{}", routed.response.body);
        let answer: Value = serde_json::from_str(&routed.response.body).unwrap();
        assert_eq!(answer["quarantined"], 0, "{answer}");
        answer
    }

    fn applications(&mut self) -> Vec<Value> {
        let read = self.call("GET", &format!("/v1/objects/{APPLICATION}"), "");
        assert_eq!(read.status, 200, "{}", read.body);
        json_of(&read)["records"].as_array().unwrap().clone()
    }
}

fn json_of(response: &Response) -> Value {
    serde_json::from_str(&response.body).unwrap()
}

#[test]
fn a_migrated_and_backfilled_cell_opens_cold() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cell.sqlite");
    let mut cell = Cell::opened(&path);
    let provisioned = cell.call("POST", PROVISION_PATH, &submission());
    assert_eq!(provisioned.status, 200, "{}", provisioned.body);

    // The first load, in pages.
    for page in [0..20, 20..40] {
        let loaded = cell.load(page.clone(), None);
        assert_eq!(loaded["landed"], page.len(), "{loaded}");
    }
    // A cold open before the migration is the control.
    drop(cell);
    let mut cell = Cell::opened(&path);
    assert_eq!(cell.applications().len(), 40);

    // The migration: four members with defaults for live rows.
    cell.apply(&migrated());
    // The backfill: the same rows at sourceVersion 2, so the mapping writes
    // real values. One page is sent twice, as after a lost answer.
    for page in [0..20, 20..40, 20..40] {
        cell.load(page, Some("2"));
    }
    // A law change, then rows that are new under it.
    cell.apply(&law_changed());
    let loaded = cell.load(40..50, None);
    assert_eq!(loaded["landed"], 10, "{loaded}");

    // The cold open of the incident.
    drop(cell);
    let mut cell = Cell::opened(&path);
    let applications = cell.applications();
    assert_eq!(applications.len(), 50);
    let backfilled = applications
        .iter()
        .find(|record| record["id"] == "APP-0001")
        .expect("a backfilled row");
    // The backfill wrote mapped values, not the migration's defaults.
    assert_eq!(
        backfilled["values"]["started_on"],
        json!("2026-10-02T00:00:00Z"),
        "{backfilled}"
    );
    assert_eq!(backfilled["values"]["status"], "Stalled", "{backfilled}");
}
