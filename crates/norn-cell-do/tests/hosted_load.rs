//! The hosted load, end to end and natively: a cell provisioned the way the
//! hosted stage provisions a tenant (the host's trusted base, plus the
//! registry packs as the tenant's submission), then the committed
//! `registry/packs/{erpnext,shopify,odoo}/synthetic.json` snapshots, loaded
//! through `POST /v1/observations:load` in bounded slices (a slice the load
//! refuses as too large is halved). Every row must land: a quarantined row here is a
//! row a hosted tenant would quarantine.
//!
//! The tenant submits the cell-owned evidence types (`norn.ObservedLinkClaim`,
//! `norn.SourceEvidence`) only while the trusted base does not carry them, so
//! the set stays admissible either way.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::too_many_lines,
    reason = "hosted load correspondence"
)]

use std::collections::BTreeMap;
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
use norn_cell_do::server::Request;
use norn_cell_do::tenant::Tenant;
use serde_json::{Value, json};

const ISSUER: &str = "https://studio-preview.example";
const LINEAGE_KEY: [u8; 32] = [7; 32];
const NOW: u64 = 1_800_000_000;
/// The per-load row bound `POST /v1/observations:load` enforces.
const MAX_LOAD_ROWS: usize = 100;
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

fn owner(org: &str) -> String {
    let claims = json!({ "iss": ISSUER, "aud": "norn-host", "sub": format!("owner-of-{org}"),
            "iat": NOW - 10, "exp": NOW + 290,
            "norn": { "kind": "user", "org": org, "role": "owner", "amr": "password" } });
    let head = URL_SAFE_NO_PAD.encode(json!({ "alg": "EdDSA", "kid": "k1" }).to_string());
    let body = URL_SAFE_NO_PAD.encode(claims.to_string());
    let signature = key().sign(format!("{head}.{body}").as_bytes());
    format!(
        "Bearer {head}.{body}.{}",
        URL_SAFE_NO_PAD.encode(signature.to_bytes())
    )
}

/// The tenant's submission: every pack artifact outside the base and the
/// reserved namespaces, plus the cell-owned types the base lacks.
fn tenant_set() -> Vec<Value> {
    let base = base::trusted_base().unwrap();
    let mut files: Vec<PathBuf> = Vec::new();
    for pack in base::TENANT_PACKS {
        let dir = repo().join("registry/packs").join(pack).join("artifacts");
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "json") {
                files.push(path);
            }
        }
    }
    for name in base::CELL_OWNED {
        files.push(repo().join(format!("registry/core/artifacts/{name}.json")));
    }
    let mut set = BTreeMap::new();
    for path in files {
        let artifact: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let id = artifact["artifactId"].as_str().unwrap().to_owned();
        if base.ids().contains(&id) || norn_api::kernel_set::is_reserved(&id) {
            continue;
        }
        set.insert(id, artifact);
    }
    set.into_values().collect()
}

/// The committed synthetic snapshots of the three systems.
fn dataset() -> Vec<(String, Value)> {
    ["erpnext", "shopify", "odoo"]
        .into_iter()
        .map(|system| {
            let path = repo().join(format!("registry/packs/{system}/synthetic.json"));
            let text = std::fs::read_to_string(path).unwrap();
            (system.to_owned(), serde_json::from_str(&text).unwrap())
        })
        .collect()
}

#[test]
fn the_synthetic_snapshots_land_whole_on_a_hosted_style_tenant() {
    let scope = CellScope {
        org_id: "org_hosted".into(),
        environment_id: "alpha".into(),
    };
    let query = format!(
        "orgId={}&environmentId={}",
        scope.org_id, scope.environment_id
    );
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("cell.sqlite");
    let mut tenant = Tenant::open(
        scope.clone(),
        &DEMO_GRANTS,
        Box::new(move || {
            NativeEngine::open(&path, StoreOptions::default()).map_err(|e| e.to_string())
        }),
        Some(&LINEAGE_KEY),
    )
    .unwrap();
    let tokens = TokenPrincipals::new(ISSUER, &jwks(), scope.clone(), tenant.caller(), NOW)
        .map_err(|refusal| refusal.code)
        .unwrap();
    let owner = owner(&scope.org_id);
    let submission = json!({
        "baseSha256": base::trusted_base().unwrap().sha256(),
        "artifacts": tenant_set(),
    })
    .to_string();
    let provisioned = tenant.handle(
        &tokens,
        &Request {
            method: "POST",
            path: PROVISION_PATH,
            query: &query,
            authorization: Some(&owner),
            now: NOW,
            body: &submission,
        },
    );
    assert_eq!(provisioned.status, 200, "{}", provisioned.body);

    let vars = json!({
        "NORN_S3_ENDPOINT": "https://storage.example.test",
        "NORN_S3_BUCKET": "uploads-bucket",
        "NORN_S3_ACCESS_KEY_ID": "key-id",
        "NORN_S3_SECRET_ACCESS_KEY": "secret",
    });
    let mut connect = Connect::new("[]", &vars.to_string()).unwrap();
    let mut quarantined = BTreeMap::<String, u64>::new();
    let (mut landed, mut total) = (0_u64, 0_u64);
    let load = |connect: &mut Connect, tenant: &Tenant<NativeEngine>, body: &str| {
        let host = tenant.serving_host().expect("a provisioned cell serves");
        let routed = connect
            .route(
                Some(host),
                &tokens,
                &Request {
                    method: "POST",
                    path: "/v1/observations:load",
                    query: &query,
                    authorization: Some(&owner),
                    now: NOW,
                    body,
                },
                0,
            )
            .expect("a connect route");
        let answer: Value = serde_json::from_str(&routed.response.body).unwrap();
        (routed.response.status, answer)
    };
    for (system, document) in dataset() {
        let header: serde_json::Map<String, Value> = document
            .as_object()
            .unwrap()
            .iter()
            .filter(|(key, _)| !matches!(key.as_str(), "snapshot" | "incremental"))
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        let rows = document["snapshot"].as_array().unwrap();
        // A slice past the per-load row bound is refused 413 before anything
        // is captured: every row still lands exactly once below.
        let mut oversized = header.clone();
        oversized.insert(
            "snapshot".into(),
            Value::Array(
                rows.iter()
                    .cycle()
                    .take(MAX_LOAD_ROWS + 1)
                    .cloned()
                    .collect(),
            ),
        );
        let (status, answer) = load(&mut connect, &tenant, &Value::Object(oversized).to_string());
        assert_eq!(status, 413, "{system}: {answer}");
        let (mut start, mut limit) = (0, MAX_LOAD_ROWS);
        while start < rows.len() {
            let count = limit.min(rows.len() - start);
            let mut body = header.clone();
            body.insert(
                "snapshot".into(),
                Value::Array(rows[start..start + count].to_vec()),
            );
            let (status, answer) = load(&mut connect, &tenant, &Value::Object(body).to_string());
            if status == 413 && count > 1 {
                // A slice past the per-load bounds is halved.
                limit = count / 2;
                continue;
            }
            assert_eq!(status, 200, "{system}: {answer}");
            landed += answer["landed"].as_u64().unwrap();
            total += u64::try_from(count).unwrap();
            for (reason, n) in answer["quarantineReasons"].as_object().unwrap() {
                *quarantined
                    .entry(format!("{system}: {reason}"))
                    .or_default() += n.as_u64().unwrap();
            }
            start += count;
        }
    }
    assert_eq!(
        quarantined.len(),
        0,
        "{landed} of {total} rows landed; quarantined: {quarantined:#?}"
    );
    // Every snapshot row is a distinct source event: each lands, once.
    assert_eq!(landed, total, "every row lands");
    assert!(
        total >= 21,
        "every committed snapshot row loads ({total} rows)"
    );
}
