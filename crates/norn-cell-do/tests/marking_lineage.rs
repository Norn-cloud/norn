//! #1355: a hosted cell provisioned with the privacy set (`canon.Party`'s
//! `display_name` marked `pii.contact`) and fed by `ERPNext`, then Shopify, each
//! through the cell's own mappings, as `POST /v1/observations:load` lands them.
//!
//! - A Holding declares no marking: none of its values carries `pii.contact`
//!   and its profile is of the values, though the page that wrote it read
//!   the marked Parties it links to (before the fix, every Holding property
//!   came back as a pseudonym).
//! - Every Party, fed by `ERPNext` and again by Shopify, keeps its unmarked
//!   `kind` clear.
//! - The marked `display_name` stays marked on every Party: a member's list,
//!   record and facets show the pseudonym, never the value.
//!
//! Lineage pins on the privacy boundary (D79): each names one label a read
//! path decides by.
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
const PII: &str = "pii.contact";
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

fn read(path: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(root().join(path)).expect(path)).expect(path)
}

/// The recorded three-systems tenant set with the privacy set: the
/// `canon.Party` overlay, the markings catalog and the reveal policy.
fn privacy_set() -> Vec<Value> {
    let recorded = read("fixtures/demo/explorer/three-systems.definitions.json");
    let mut set: Vec<Value> = recorded
        .as_array()
        .unwrap()
        .iter()
        .filter(|artifact| {
            !norn_api::kernel_set::is_reserved(artifact["artifactId"].as_str().unwrap())
        })
        .map(|artifact| {
            if artifact["artifactId"] == "canon.Party.entity" {
                read("registry/privacy/overlays/canon.Party.entity.json")
            } else {
                artifact.clone()
            }
        })
        .collect();
    set.push(read(
        "registry/privacy/artifacts/privacy.markings.entity.json",
    ));
    set.push(read(
        "registry/privacy/artifacts/privacy.reveal.policy.json",
    ));
    set
}

struct Cell {
    tenant: Tenant<NativeEngine>,
    tokens: TokenPrincipals,
    connect: Connect,
}

impl Cell {
    fn call(&mut self, method: &str, path: &str, auth: &str, body: &str) -> Response {
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

    /// An admin's load of one system's recorded export.
    fn load(&mut self, system: &str) {
        let export = read(&format!("registry/packs/{system}/synthetic.json"));
        let body = json!({ "profile": export["profile"], "snapshot": export["snapshot"] });
        let body = body.to_string();
        let admin = user("admin");
        let request = Request {
            method: "POST",
            path: "/v1/observations:load",
            query: QUERY,
            authorization: Some(&admin),
            now: NOW,
            body: &body,
        };
        let routed = self
            .connect
            .route(
                self.tenant.serving_host(),
                &self.tokens,
                &request,
                NOW * 1000,
            )
            .expect("a connect route");
        assert_eq!(
            routed.response.status, 200,
            "{system}: {}",
            routed.response.body
        );
        let landed: Value = serde_json::from_str(&routed.response.body).unwrap();
        assert_eq!(landed["landed"], landed["rows"], "{system}: {landed}");
    }

    /// A member's page of `object_type`: each record's id and what the
    /// server masked in it.
    fn masked(&mut self, object_type: &str) -> Vec<(String, Value)> {
        let page = self.call(
            "GET",
            &format!("/v1/objects/{object_type}"),
            &user("member"),
            "",
        );
        assert_eq!(page.status, 200, "{}", page.body);
        let page: Value = serde_json::from_str(&page.body).unwrap();
        page["records"]
            .as_array()
            .unwrap()
            .iter()
            .map(|record| {
                (
                    record["id"].as_str().unwrap_or_default().to_owned(),
                    record.get("masked").cloned().unwrap_or(Value::Null),
                )
            })
            .collect()
    }
}

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
    let vars = json!({
        "NORN_S3_ENDPOINT": "https://storage.example.test",
        "NORN_S3_BUCKET": "uploads-bucket",
        "NORN_SECRET_S3_ACCESS_KEY_ID": "key-id",
        "NORN_SECRET_S3_SECRET_ACCESS_KEY": "secret",
    });
    let connect = Connect::new("[]", &vars.to_string()).expect("configuration");
    let mut cell = Cell {
        tenant,
        tokens,
        connect,
    };
    let body = json!({
        "baseSha256": base::trusted_base().unwrap().sha256(),
        "artifacts": privacy_set(),
    })
    .to_string();
    let provisioned = cell.call("POST", PROVISION_PATH, &user("owner"), &body);
    assert_eq!(provisioned.status, 200, "{}", provisioned.body);
    // ERPNext first, as the alpha loads them: its Parties are written by
    // pages that read no marked value; the later systems' pages read them.
    for system in ["erpnext", "shopify"] {
        cell.load(system);
    }
    cell
}

#[test]
fn a_property_carries_only_the_markings_of_the_values_it_was_computed_from() {
    let directory = tempfile::tempdir().unwrap();
    let mut cell = loaded(&directory.path().join("a.sqlite"));

    // A member reads every Party's name as its pseudonym and its kind as is,
    // whichever source fed it.
    let parties = cell.masked("canon.Party");
    assert_eq!(parties.len(), 3, "{parties:?}");
    for (id, masked) in &parties {
        assert_eq!(
            masked["display_name"]["mode"], "pseudonym",
            "{id}: {masked}"
        );
        assert_eq!(
            masked["display_name"]["markings"],
            json!([PII]),
            "{id}: {masked}"
        );
        assert!(
            masked.get("kind").is_none(),
            "{id}: kind is unmarked: {masked}"
        );
    }
    // The record read, the facets and the profile agree: the name is the
    // pseudonym (never the source's "Owner", "Custodian" or "Customer"), the
    // kind is the value.
    let names = ["Owner", "Custodian", "Customer"];
    let record = cell.call(
        "GET",
        "/v1/objects/canon.Party/demo%2Fparty%2Fcustomer",
        &user("member"),
        "",
    );
    assert_eq!(record.status, 200, "{}", record.body);
    let record: Value = serde_json::from_str(&record.body).unwrap();
    assert_eq!(
        record["masked"]["display_name"]["mode"], "pseudonym",
        "{record}"
    );
    assert!(!names.contains(&record["values"]["display_name"].as_str().unwrap()));
    assert_eq!(record["values"]["kind"], "organization", "{record}");
    let facets = cell.call("GET", "/v1/objects/canon.Party:facets", &user("member"), "");
    assert_eq!(facets.status, 200, "{}", facets.body);
    let facets: Value = serde_json::from_str(&facets.body).unwrap();
    for facet in facets["facets"].as_array().unwrap() {
        for value in facet["values"].as_array().unwrap() {
            let shown = value["value"].as_str().unwrap_or_default();
            match facet["property"].as_str() {
                Some("display_name") => assert!(!names.contains(&shown), "{facets}"),
                Some("kind") => assert_eq!(shown, "organization", "{facets}"),
                _ => {}
            }
        }
    }

    // A Holding declares no marking: nothing of it is masked.
    let holdings = cell.masked("canon.Holding");
    assert!(!holdings.is_empty());
    for (id, masked) in &holdings {
        assert!(
            masked.as_object().is_none_or(serde_json::Map::is_empty),
            "{id}: {masked}"
        );
    }
    // Its profile is of the values, never of pseudonyms.
    let profile = cell.call(
        "GET",
        "/v1/objects/canon.Holding:profile",
        &user("member"),
        "",
    );
    assert_eq!(profile.status, 200, "{}", profile.body);
    let profile: Value = serde_json::from_str(&profile.body).unwrap();
    let observed = profile["properties"]
        .as_array()
        .unwrap()
        .iter()
        .find(|property| property["property"] == "observed_at")
        .expect("observed_at is profiled");
    assert_eq!(observed["min"], "2026-11-01T00:00:00Z", "{profile}");
    assert_eq!(observed["max"], "2026-11-01T00:00:00Z", "{profile}");
}
