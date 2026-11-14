//! HOST-INSTALL-1: a running tenant cell takes a pack through
//! `POST /v1/cell/definitions:plan` and `:apply`, on the same [`Tenant`] the
//! Durable Object binding drives.
//!
//! The pitch's second moment, end to end on a provisioned cell: a broken
//! jewelry pack is refused with the checker's diagnostic and changes nothing;
//! a member may not apply; the owner's plan names the new type; an admin's
//! apply installs it as the next revision with a receipt; a melt that loses
//! metal is refused and a correct one accepted; a restart reopens the cell at
//! the applied revision; and a law change the stored melts break is held back
//! with findings naming them.
//!
//! These pin decisions of the server's permission and admission system that
//! no existing property covers (D79).
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::too_many_lines,
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
const MELT: &str = "jewelry.Melt";
const RECORD: &str = "jewelry.melt.record";
const RECORDED: &str =
    include_str!("../../../fixtures/demo/explorer/three-systems.definitions.json");
const JEWELRY: [&str; 4] = [
    include_str!("../../../registry/packs/jewelry/artifacts/jewelry.Melt.entity.json"),
    include_str!("../../../registry/packs/jewelry/artifacts/jewelry.melt.access.policy.json"),
    include_str!(
        "../../../registry/packs/jewelry/artifacts/jewelry.melt.fine_mass_conserved.law.json"
    ),
    include_str!("../../../registry/packs/jewelry/artifacts/jewelry.melt.record.action.json"),
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

/// The three-systems tenant set: the canon the jewelry pack specializes.
fn tenant_set() -> Vec<Value> {
    let artifacts: Vec<Value> = serde_json::from_str(RECORDED).unwrap();
    artifacts
        .into_iter()
        .filter(|artifact| {
            !norn_api::kernel_set::is_reserved(artifact["artifactId"].as_str().unwrap())
        })
        .collect()
}

/// The jewelry pack as the registry carries it: its action callable on a
/// tenant cell through the pack's own access policy.
fn jewelry() -> Vec<Value> {
    JEWELRY
        .iter()
        .map(|text| serde_json::from_str(text).unwrap())
        .collect()
}

/// The pack with its law's predicate replaced by `expression`.
fn with_law(expression: &str) -> Vec<Value> {
    let mut artifacts = jewelry();
    artifacts[2]["payload"]["response"]["expression"] = json!(expression);
    artifacts
}

struct Cell {
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

    fn melt(&mut self, id: &str, input: &str, output: &str, loss: &str) -> Response {
        let body = json!({ "input": { "id": id, "metal": "gold", "input_fine_g": input,
            "output_fine_g": output, "loss_fine_g": loss, "loss_reason": null },
            "idempotencyKey": id });
        self.call(
            "POST",
            &format!("/v1/actions/{RECORD}"),
            Some(&user("member")),
            &body.to_string(),
        )
    }

    fn melts(&mut self) -> Response {
        self.call(
            "GET",
            &format!("/v1/objects/{MELT}"),
            Some(&user("admin")),
            "",
        )
    }
}

fn json_of(response: &Response) -> Value {
    serde_json::from_str(&response.body).unwrap()
}

fn provisioned(path: &Path) -> Cell {
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
    assert_eq!(cell.version(), 1);
    // Before the pack, the cell serves no melts.
    assert_ne!(cell.melts().status, 200);

    // A broken pack: the law reads a property the Melt does not have. Plan
    // and apply refuse it with the checker's diagnostic, placed in the
    // submission; nothing changes.
    let broken = with_law("self.input_fine_g == self.output_fine_g + self.loss_g");
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
    let applied = cell.change(APPLY_PATH, Some("owner"), &broken, Some(1));
    assert_eq!(applied.status, 422);
    assert_eq!(json_of(&applied)["diagnostics"], refusal["diagnostics"]);
    assert_eq!(cell.version(), 1);

    // Only a definition writer (owner, admin) plans or applies; anyone else
    // gets the one opaque denial, whatever the body.
    let denied = cell.change(APPLY_PATH, None, &jewelry(), Some(1));
    assert_eq!(denied.status, 403);
    for role in ["member", "viewer"] {
        for route in [PLAN_PATH, APPLY_PATH] {
            let response = cell.change(route, Some(role), &jewelry(), Some(1));
            assert_eq!(response, denied, "{role} {route}");
        }
    }
    let other_org = cell.call(
        "POST",
        APPLY_PATH,
        Some(&token("org_b", "owner")),
        &json!({ "artifacts": jewelry(), "expectedVersion": 1 }).to_string(),
    );
    assert_eq!(other_org, denied);
    assert_eq!(cell.version(), 1);

    // The owner's plan: four artifacts added, the Melt a new type.
    let planned = cell.change(PLAN_PATH, Some("owner"), &jewelry(), None);
    assert_eq!(planned.status, 200, "{}", planned.body);
    let plan = json_of(&planned);
    assert_eq!(plan["baseVersion"], 1);
    assert_eq!(plan["nextVersion"], 2);
    assert_eq!(plan["unchanged"], false);
    assert_eq!(plan["added"].as_array().unwrap().len(), 4, "{plan}");
    assert_eq!(plan["newObjectTypes"], json!([MELT]));
    assert_eq!(cell.version(), 1, "a plan writes nothing");

    // Apply needs the planned revision; an admin applies it.
    let unplanned = cell.change(APPLY_PATH, Some("admin"), &jewelry(), None);
    assert_eq!(unplanned.status, 400);
    let stale = cell.change(APPLY_PATH, Some("admin"), &jewelry(), Some(0));
    assert_eq!(json_of(&stale)["code"], "defs.stale");
    let applied = cell.change(APPLY_PATH, Some("admin"), &jewelry(), Some(1));
    assert_eq!(applied.status, 200, "{}", applied.body);
    let receipt = json_of(&applied);
    assert_eq!(receipt["outcome"], "installed");
    assert_eq!(receipt["definitionsVersion"], 2);
    assert_eq!(receipt["receiptSha256"], plan["receiptSha256"]);
    assert_eq!(cell.version(), 2);
    // The same apply again installs nothing.
    let again = cell.change(APPLY_PATH, Some("admin"), &jewelry(), Some(2));
    assert_eq!(json_of(&again)["outcome"], "unchanged", "{}", again.body);
    assert_eq!(cell.version(), 2);

    // The new type is served at once, and its action is callable: a melt
    // that loses metal is refused by the law, a correct one is accepted.
    assert_eq!(cell.melts().status, 200);
    let lossy = cell.melt("m1", "100.000", "98.000", "0.000");
    assert!(lossy.status >= 400, "{}", lossy.body);
    assert_eq!(
        json_of(&cell.melts())["records"].as_array().map(Vec::len),
        Some(0)
    );
    let correct = cell.melt("m2", "100.000", "98.000", "2.000");
    assert_eq!(correct.status, 200, "{}", correct.body);
    assert_eq!(json_of(&correct)["outcome"], "committed");
    assert_eq!(
        json_of(&cell.melts())["records"].as_array().map(Vec::len),
        Some(1)
    );

    // HOST-REOPEN-1: a restarted host reopens the cell at the applied
    // revision, with the melt and its law.
    drop(cell);
    let mut cell = Cell::opened(&path);
    assert_eq!(cell.version(), 2);
    assert_eq!(
        json_of(&cell.melts())["records"].as_array().map(Vec::len),
        Some(1)
    );
    let lossy = cell.melt("m3", "50.000", "49.000", "0.000");
    assert!(lossy.status >= 400, "{}", lossy.body);

    // HOST-INSTALL-LIVE-1: a law the stored melt breaks is held back, with a
    // finding naming the melt; nothing changes.
    let stricter = with_law("self.input_fine_g == self.output_fine_g");
    let held = cell.change(PLAN_PATH, Some("owner"), &stricter, None);
    assert_eq!(held.status, 409, "{}", held.body);
    let held = json_of(&held);
    assert_eq!(held["code"], "defs.live_data_refused");
    assert_eq!(
        held["diagnostics"][0]["span"]["pointer"],
        format!("/objects/{MELT}/m2"),
        "{held}"
    );
    let applied = cell.change(APPLY_PATH, Some("owner"), &stricter, Some(2));
    assert_eq!(json_of(&applied)["code"], "defs.live_data_refused");
    assert_eq!(cell.version(), 2);
    // A law the stored melt keeps installs, with the stored melt checked.
    let looser = with_law("self.input_fine_g >= self.output_fine_g + self.loss_fine_g");
    let applied = cell.change(APPLY_PATH, Some("owner"), &looser, Some(2));
    assert_eq!(applied.status, 200, "{}", applied.body);
    assert_eq!(json_of(&applied)["plan"]["checkedObjects"], 1);
    assert_eq!(
        json_of(&applied)["plan"]["changed"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(cell.version(), 3);
}

/// A reserved artifact or an empty change is refused before admission.
#[test]
fn a_reserved_or_empty_change_is_refused() {
    let directory = tempfile::tempdir().unwrap();
    let mut cell = provisioned(&directory.path().join("cell.sqlite"));
    let mut forged = jewelry();
    forged[0]["artifactId"] = json!("core.forged");
    let refused = cell.change(PLAN_PATH, Some("owner"), &forged, None);
    assert_eq!(refused.status, 400);
    assert_eq!(json_of(&refused)["code"], "cell.reserved_artifact");
    let empty = cell.change(PLAN_PATH, Some("owner"), &[], None);
    assert_eq!(empty.status, 400);
    let mut twice = jewelry();
    twice.push(twice[0].clone());
    assert_eq!(
        cell.change(PLAN_PATH, Some("owner"), &twice, None).status,
        400
    );
    assert_eq!(cell.version(), 1);
}
