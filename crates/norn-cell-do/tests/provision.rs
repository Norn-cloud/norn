//! HOST-PROVISION-1: tenant provisioning on the network host, through the
//! same [`Tenant`] the Durable Object binding drives.
//!
//! - Only the declared provisioner (an owner of the scope's organization)
//!   creates a cell; everyone else gets the one opaque denial and nothing is
//!   written.
//! - A submission carries no reserved-namespace artifact and names the host's
//!   trusted base; the host supplies the base itself.
//! - Provisioning is atomic under storage faults, replays an identical retry
//!   (also after a restart) and refuses a different one.
//! - A scope with no cell is indistinguishable from a provisioned one to a
//!   caller nobody verified; a verified member is told `cell.not_provisioned`.
//! - Two scopes are isolated: one's owner is nobody at the other.
//!
//! These are refusal-path and isolation pins on a new authority (D79: each
//! names one decision of the server's permission system, which no existing
//! property covers); the storage-fault law is a sweep over every statement
//! of the provisioning transaction.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::too_many_lines,
    reason = "provisioning correspondence"
)]

use std::path::{Path, PathBuf};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signer, SigningKey};
use norn_api::provision::{self, MAX_PROVISION_BYTES, PROVISION_PATH, TrustedBase};
use norn_api::scope::CellScope;
use norn_cell::StoreOptions;
use norn_cell::engine::{
    DoEngine, DoStorageEmulator, EngineFault, EngineFaultKind, FaultEngine, FaultSwitch,
    NativeEngine, SqlEngine, SqlValue,
};
use norn_cell_do::policy::DEMO_GRANTS;
use norn_cell_do::principal::TokenPrincipals;
use norn_cell_do::server::{Request, Response};
use norn_cell_do::tenant::Tenant;
use norn_cell_do::{base, seed};
use serde_json::{Value, json};

const ISSUER: &str = "https://studio-preview.example";
/// A stage lineage key, as the deployed host passes one.
const LINEAGE_KEY: [u8; 32] = [7; 32];
const NOW: u64 = 1_800_000_000;
/// The recorded three-systems set: its artifacts outside the reserved
/// namespaces are a tenant's submission.
const RECORDED: &str =
    include_str!("../../../fixtures/demo/explorer/three-systems.definitions.json");

/// A tenant's artifact envelopes, with or without the label catalog (two
/// admissible sets that differ).
fn tenant_set(labels: bool) -> Vec<String> {
    let artifacts: Vec<Value> = serde_json::from_str(RECORDED).unwrap();
    let set: Vec<String> = artifacts
        .iter()
        .filter(|artifact| {
            !norn_api::kernel_set::is_reserved(artifact["artifactId"].as_str().unwrap())
        })
        .filter(|artifact| labels || artifact["kind"] != "labels")
        .map(Value::to_string)
        .collect();
    assert!(set.len() > 5);
    set
}

fn full() -> Vec<String> {
    tenant_set(true)
}

fn plain() -> Vec<String> {
    let plain = tenant_set(false);
    assert!(
        plain.len() < full().len(),
        "the recording carries a label catalog"
    );
    plain
}

// ------------------------------------------------------------------ tokens

fn key() -> SigningKey {
    SigningKey::from_bytes(&[1; 32])
}

fn jwks() -> String {
    json!({ "keys": [{ "kty": "OKP", "crv": "Ed25519", "alg": "EdDSA", "kid": "k1",
                       "x": URL_SAFE_NO_PAD.encode(key().verifying_key().as_bytes()) }] })
    .to_string()
}

/// A host token for `role` in `org`, signed by the pinned issuer.
fn token(kind: &str, org: &str, role: &str) -> String {
    let amr = if kind == "service" {
        "api_key"
    } else {
        "password"
    };
    let claims = json!({ "iss": ISSUER, "aud": "norn-host", "sub": format!("{role}-of-{org}"),
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

fn user(org: &str, role: &str) -> String {
    token("user", org, role)
}

// ------------------------------------------------------------------- cells

/// One scope's cell over a file, with its token resolver.
struct Cell<E: SqlEngine> {
    tenant: Tenant<E>,
    tokens: TokenPrincipals,
}

fn scope(org: &str, environment: &str) -> CellScope {
    CellScope {
        org_id: org.into(),
        environment_id: environment.into(),
    }
}

fn query(scope: &CellScope) -> String {
    format!(
        "orgId={}&environmentId={}",
        scope.org_id, scope.environment_id
    )
}

fn with_engine<E: SqlEngine + 'static>(
    scope: &CellScope,
    open: impl Fn() -> Result<E, String> + 'static,
) -> Result<Cell<E>, String> {
    // As wasm.rs builds a deployed tenant: with the stage's lineage key, so
    // every route sees the receipt-handle runtime a deployed cell has.
    let tenant = Tenant::open(
        scope.clone(),
        &DEMO_GRANTS,
        Box::new(open),
        Some(&LINEAGE_KEY),
    )
    .map_err(|error| error.to_string())?;
    let tokens = TokenPrincipals::new(ISSUER, &jwks(), scope.clone(), tenant.caller(), NOW)
        .map_err(|refusal| refusal.code)?;
    Ok(Cell { tenant, tokens })
}

/// Open (or reopen, as a restarted host does) the cell of `scope` at `path`.
fn open(path: &Path, scope: &CellScope) -> Cell<NativeEngine> {
    try_open(path, scope).unwrap()
}

fn try_open(path: &Path, scope: &CellScope) -> Result<Cell<NativeEngine>, String> {
    let path = path.to_owned();
    with_engine(scope, move || {
        NativeEngine::open(&path, StoreOptions::default()).map_err(|error| error.to_string())
    })
}

fn call<E: SqlEngine>(
    cell: &mut Cell<E>,
    method: &str,
    path: &str,
    query: &str,
    authorization: Option<&str>,
    body: &str,
) -> Response {
    cell.tenant.handle(
        &cell.tokens,
        &Request {
            method,
            path,
            query,
            authorization,
            now: NOW,
            body,
        },
    )
}

fn json_of(response: &Response) -> Value {
    serde_json::from_str(&response.body).unwrap()
}

fn code(response: &Response) -> String {
    json_of(response)["code"].as_str().unwrap().to_owned()
}

/// A submission of `artifacts` (envelope texts) against the host's base.
fn submission(artifacts: &[String]) -> String {
    submission_on(base::trusted_base().unwrap().sha256(), artifacts)
}

fn submission_on(base_sha256: &str, artifacts: &[String]) -> String {
    let artifacts: Vec<Value> = artifacts
        .iter()
        .map(|text| serde_json::from_str(text).unwrap())
        .collect();
    json!({ "baseSha256": base_sha256, "artifacts": artifacts }).to_string()
}

fn provision<E: SqlEngine>(
    cell: &mut Cell<E>,
    authorization: Option<&str>,
    body: &str,
) -> Response {
    let query = query(cell.tenant.scope());
    call(cell, "POST", PROVISION_PATH, &query, authorization, body)
}

/// Provision the cell of `scope` at `path` as its owner, with `artifacts`.
fn provisioned(path: &Path, scope: &CellScope, artifacts: &[String]) -> Cell<NativeEngine> {
    let mut cell = open(path, scope);
    let owner = user(&scope.org_id, "owner");
    let response = provision(&mut cell, Some(&owner), &submission(artifacts));
    assert_eq!(response.status, 200, "{}", response.body);
    cell
}

fn definitions<E: SqlEngine>(cell: &mut Cell<E>, authorization: Option<&str>) -> Response {
    definitions_at(cell, authorization, NOW)
}

/// The catalog read at `now`: a cell's token resolver refuses its keys once
/// the clock has moved backwards, so a test that advances the clock reads on.
fn definitions_at<E: SqlEngine>(
    cell: &mut Cell<E>,
    authorization: Option<&str>,
    now: u64,
) -> Response {
    let query = query(cell.tenant.scope());
    cell.tenant.handle(
        &cell.tokens,
        &Request {
            method: "GET",
            path: "/v1/definitions",
            query: &query,
            authorization,
            now,
            body: "",
        },
    )
}

fn file(directory: &tempfile::TempDir, name: &str) -> PathBuf {
    directory.path().join(format!("{name}.sqlite"))
}

// -------------------------------------------------------------------- base

/// The compiled-in base is exactly the reserved-namespace artifacts the
/// seeded demo cell admits with, each byte-equal to its registry copy, and
/// it admits on its own.
#[test]
fn the_compiled_in_base_is_the_seeds_reserved_artifacts() {
    let recorded: Vec<Value> = serde_json::from_str(RECORDED).unwrap();
    let mut seeded: Vec<Value> = recorded
        .into_iter()
        .filter(|artifact| {
            norn_api::kernel_set::is_reserved(artifact["artifactId"].as_str().unwrap())
        })
        .collect();
    let mut compiled: Vec<Value> = base::SOURCES
        .iter()
        .map(|text| serde_json::from_str(text).unwrap())
        .collect();
    let id = |artifact: &Value| artifact["artifactId"].as_str().unwrap().to_owned();
    seeded.sort_by_key(id);
    compiled.sort_by_key(id);
    assert_eq!(compiled, seeded);
    let directory = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../registry/core/artifacts");
    for source in base::SOURCES {
        let artifact: Value = serde_json::from_str(source).unwrap();
        let on_disk =
            std::fs::read_to_string(directory.join(format!("{}.json", id(&artifact)))).unwrap();
        assert_eq!(source, on_disk, "{}", id(&artifact));
    }
    let trusted = base::trusted_base().unwrap();
    assert_eq!(trusted.ids().len(), base::SOURCES.len());
    assert!(
        trusted
            .ids()
            .iter()
            .all(|id| norn_api::kernel_set::is_reserved(id))
    );
    // An empty tenant set provisions: the base admits alone.
    let admitted = provision::admit_submission(&trusted, &scope("org_a", "prod"), &submission(&[]));
    assert!(admitted.is_ok(), "{:?}", admitted.err());
}

// --------------------------------------------------------------- authority

/// Only an owner of the scope's organization provisions it. Every other
/// caller gets the one opaque denial, whatever the body says, and the scope
/// stays vacant with nothing written: the owner then provisions a fresh cell.
#[test]
fn only_the_owner_of_the_scopes_organization_provisions_it() {
    let directory = tempfile::tempdir().unwrap();
    let path = file(&directory, "a");
    let scope = scope("org_a", "prod");
    let mut cell = open(&path, &scope);
    let body = submission(&full());
    let anonymous = provision(&mut cell, None, &body);
    assert_eq!(
        (anonymous.status, code(&anonymous).as_str()),
        (403, "authz.denied")
    );
    let refused: Vec<(&str, String)> = vec![
        ("admin", user("org_a", "admin")),
        ("member", user("org_a", "member")),
        ("viewer", user("org_a", "viewer")),
        ("undeclared role", user("org_a", "analyst")),
        ("another organization's owner", user("org_b", "owner")),
        ("service viewer", token("service", "org_a", "viewer")),
        ("service member", token("service", "org_a", "member")),
        ("service admin", token("service", "org_a", "admin")),
        ("service owner", token("service", "org_a", "owner")),
        ("not a token", "Bearer nonsense".to_owned()),
    ];
    for (who, authorization) in &refused {
        for body in [body.as_str(), "", "not json"] {
            let response = provision(&mut cell, Some(authorization), body);
            assert_eq!(response, anonymous, "{who}");
        }
        assert!(!cell.tenant.is_provisioned(), "{who}");
    }
    // The owner's own token, naming another scope, is denied the same way.
    let owner = user("org_a", "owner");
    for other in [
        "orgId=org_b&environmentId=prod",
        "orgId=org_a&environmentId=dev",
        "orgId=org_a",
        "",
    ] {
        let response = call(
            &mut cell,
            "POST",
            PROVISION_PATH,
            other,
            Some(&owner),
            &body,
        );
        assert_eq!(response, anonymous, "{other}");
    }
    // An expired owner token is nobody.
    let late = cell.tenant.handle(
        &cell.tokens,
        &Request {
            method: "POST",
            path: PROVISION_PATH,
            query: &query(&scope),
            authorization: Some(&owner),
            now: NOW + 400,
            body: &body,
        },
    );
    assert_eq!(late, anonymous);
    drop(cell);
    // Nothing was written: a restarted host finds the scope vacant, and the
    // owner's provisioning creates the cell (residue would refuse).
    let mut cell = open(&path, &scope);
    assert!(!cell.tenant.is_provisioned());
    let created = provision(&mut cell, Some(&owner), &body);
    assert_eq!(created.status, 200, "{}", created.body);
    let receipt = json_of(&created);
    assert_eq!(receipt["outcome"], "provisioned");
    assert_eq!(receipt["definitionsVersion"], 1);
    assert_eq!(receipt["baseSha256"], cell.tenant.base_sha256());
    assert!(!receipt["cellId"].as_str().unwrap().is_empty());
    assert_eq!(receipt["receiptSha256"].as_str().unwrap().len(), 64);
    assert!(cell.tenant.is_provisioned());
    // The cell serves its organization's roles through the declared tables.
    for role in ["owner", "admin", "member", "viewer"] {
        assert_eq!(
            definitions(&mut cell, Some(&user("org_a", role))).status,
            200,
            "{role}"
        );
    }
    let page = call(
        &mut cell,
        "GET",
        "/v1/objects/canon.Item",
        &query(&scope),
        Some(&user("org_a", "admin")),
        "",
    );
    assert_eq!(page.status, 200, "{}", page.body);
    // Provisioning stays the owner's on the provisioned cell too.
    for (who, authorization) in &refused {
        assert_eq!(
            provision(&mut cell, Some(authorization), &body),
            anonymous,
            "{who}"
        );
    }
}

// ------------------------------------------------------------- submissions

/// Every refused submission is typed, and leaves the scope vacant.
#[test]
fn a_refused_submission_is_typed_and_writes_nothing() {
    let directory = tempfile::tempdir().unwrap();
    let path = file(&directory, "a");
    let scope = scope("org_a", "prod");
    let mut cell = open(&path, &scope);
    let owner = user("org_a", "owner");
    let host_base = cell.tenant.base_sha256().to_owned();

    // A reserved id refuses before admission, even a byte-exact base artifact.
    let first = full().remove(0);
    let mut forged: Value = serde_json::from_str(&first).unwrap();
    forged["artifactId"] = json!("core.Forged.entity");
    for reserved in [forged.to_string(), base::SOURCES[0].to_owned()] {
        let response = provision(
            &mut cell,
            Some(&owner),
            &submission(&[first.clone(), reserved]),
        );
        assert_eq!(
            (response.status, code(&response).as_str()),
            (400, provision::codes::RESERVED_ARTIFACT)
        );
        assert_eq!(
            json_of(&response)["diagnostics"][0]["span"]["pointer"],
            "/artifacts/1/artifactId"
        );
    }
    // Another base digest refuses and states the host's.
    let response = provision(
        &mut cell,
        Some(&owner),
        &submission_on(&"0".repeat(64), &full()),
    );
    assert_eq!(
        (response.status, code(&response).as_str()),
        (409, provision::codes::BASE_MISMATCH)
    );
    assert!(
        json_of(&response)["explanation"]
            .as_str()
            .unwrap()
            .contains(&host_base)
    );
    // A set the checker does not admit refuses with its findings.
    let mut dangling: Value = serde_json::from_str(&first).unwrap();
    dangling["dependencies"] = json!([{ "artifactId": "tenant.Missing.entity", "kind": "entity",
                                        "required": true, "version": "0.1.0" }]);
    let mut inadmissible = full();
    inadmissible[0] = dangling.to_string();
    let response = provision(&mut cell, Some(&owner), &submission(&inadmissible));
    assert_eq!(
        (response.status, code(&response).as_str()),
        (422, provision::codes::PROVISION_REFUSED),
        "{}",
        response.body
    );
    assert!(
        !json_of(&response)["diagnostics"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    // Shape, bound, method and query.
    let large = format!("{}{}", submission(&full()), " ".repeat(MAX_PROVISION_BYTES));
    for (body, status) in [
        ("", 400),
        ("{}", 400),
        ("[]", 400),
        (r#"{"baseSha256":"x","artifacts":[1]}"#, 409),
        (large.as_str(), 413),
    ] {
        assert_eq!(
            provision(&mut cell, Some(&owner), body).status,
            status,
            "{}",
            body.len()
        );
    }
    let not_an_envelope =
        json!({ "baseSha256": host_base, "artifacts": [{ "artifactId": "tenant.x" }, 7] })
            .to_string();
    assert_eq!(
        provision(&mut cell, Some(&owner), &not_an_envelope).status,
        400
    );
    let scoped = query(&scope);
    assert_eq!(
        call(&mut cell, "GET", PROVISION_PATH, &scoped, Some(&owner), "").status,
        405
    );
    assert_eq!(
        call(
            &mut cell,
            "POST",
            PROVISION_PATH,
            &format!("{scoped}&force=1"),
            Some(&owner),
            &submission(&full())
        )
        .status,
        400
    );
    assert!(!cell.tenant.is_provisioned());
    drop(cell);
    let mut cell = open(&path, &scope);
    assert!(!cell.tenant.is_provisioned());
    assert_eq!(
        json_of(&provision(&mut cell, Some(&owner), &submission(&full())))["outcome"],
        "provisioned"
    );
}

/// An artifact scoped outside the cell (another environment of the same
/// organization, or another organization) is refused before admission, at
/// its own `scope`, and nothing is written: the cell would otherwise hold a
/// set every later read refuses, with no way to repair it.
#[test]
fn an_artifact_scoped_outside_the_cell_is_refused() {
    let directory = tempfile::tempdir().unwrap();
    let path = file(&directory, "a");
    let scope = scope("org_a", "prod");
    let mut cell = open(&path, &scope);
    let owner = user("org_a", "owner");
    for outside in [
        json!({ "orgId": "org_a", "environmentId": "dev" }),
        json!({ "orgId": "org_b" }),
        json!({ "orgId": "org_b", "environmentId": "prod" }),
    ] {
        let mut artifacts = full();
        let mut last: Value = serde_json::from_str(artifacts.last().unwrap()).unwrap();
        last["scope"] = outside.clone();
        let at = artifacts.len() - 1;
        artifacts[at] = last.to_string();
        let response = provision(&mut cell, Some(&owner), &submission(&artifacts));
        assert_eq!(
            (response.status, code(&response).as_str()),
            (422, provision::codes::PROVISION_REFUSED),
            "{outside}"
        );
        let diagnostic = &json_of(&response)["diagnostics"][0];
        assert_eq!(
            diagnostic["code"],
            provision::codes::SCOPE_CONFLICT,
            "{outside}"
        );
        assert_eq!(
            diagnostic["span"]["pointer"],
            format!("/artifacts/{at}/scope")
        );
        assert!(!cell.tenant.is_provisioned());
    }
    // An artifact scoped to the cell's own scope is inside it (the label
    // catalog, which nothing else in the set references by content).
    let mut artifacts = full();
    let labels = artifacts
        .iter()
        .position(|text| text.contains("\"locale.ar_eg.labels\""))
        .unwrap();
    let mut own: Value = serde_json::from_str(&artifacts[labels]).unwrap();
    own["scope"] = json!({ "orgId": "org_a", "environmentId": "prod" });
    artifacts[labels] = own.to_string();
    let created = provision(&mut cell, Some(&owner), &submission(&artifacts));
    assert_eq!(created.status, 200, "{}", created.body);
    assert_eq!(definitions(&mut cell, Some(&owner)).status, 200);
}

/// A tenant artifact that declares a name the trusted base declares (here
/// the base property type `core.code`) is refused by admission, so the base
/// cannot be shadowed by a non-reserved artifact id.
#[test]
fn an_artifact_declaring_a_base_name_is_refused() {
    let directory = tempfile::tempdir().unwrap();
    let mut cell = open(&file(&directory, "a"), &scope("org_a", "prod"));
    let shadow = json!({
        "irVersion": "0.1", "artifactId": "tenant.value_types.entity", "kind": "entity",
        "namespace": "tenant", "name": "Shadowing value types", "version": "0.1.0",
        "lifecycle": "active", "scope": {}, "dependencies": [],
        "provenance": { "source": "authoring_sdk", "sourcePath": "tests/provision.rs" },
        "payload": { "entityVersion": "2.0", "propertyTypes": [{
            "name": "core.code", "description": "A shadow of the base code type.",
            "type": { "kind": "text" }, "constraints": { "minLength": 1, "maxLength": 9 } }] }
    });
    let response = provision(
        &mut cell,
        Some(&user("org_a", "owner")),
        &submission(&[shadow.to_string()]),
    );
    assert_eq!(
        (response.status, code(&response).as_str()),
        (422, provision::codes::PROVISION_REFUSED),
        "{}",
        response.body
    );
    // The refusal is the checker's finding about the shadowed name.
    let diagnostics = json_of(&response)["diagnostics"].clone();
    assert!(
        diagnostics
            .as_array()
            .unwrap()
            .iter()
            .any(|diagnostic| diagnostic["message"]
                .as_str()
                .unwrap()
                .contains("core.code")),
        "{diagnostics}"
    );
    assert!(!cell.tenant.is_provisioned());
}

// ------------------------------------------------------------------- retry

/// An identical retry replays without writing, on the live cell and after a
/// restart (a reply lost after the commit); a different submission refuses;
/// the store itself refuses a second provisioning of a non-fresh engine.
#[test]
fn an_identical_retry_replays_and_a_different_one_refuses() {
    let directory = tempfile::tempdir().unwrap();
    let path = file(&directory, "a");
    let scope = scope("org_a", "prod");
    let owner = user("org_a", "owner");
    let body = submission(&full());
    let mut cell = open(&path, &scope);
    let first = json_of(&provision(&mut cell, Some(&owner), &body));
    assert_eq!(first["outcome"], "provisioned");
    let installed = definitions(&mut cell, Some(&owner));
    let provision_at = |cell: &mut Cell<NativeEngine>, body: &str, now: u64| {
        cell.tenant.handle(
            &cell.tokens,
            &Request {
                method: "POST",
                path: PROVISION_PATH,
                query: &query(&scope),
                authorization: Some(&owner),
                now,
                body,
            },
        )
    };
    let replayed = |cell: &mut Cell<NativeEngine>, now: u64| {
        let response = provision_at(cell, &body, now);
        assert_eq!(response.status, 200, "{}", response.body);
        let mut receipt = json_of(&response);
        assert_eq!(receipt["outcome"], "replayed");
        receipt["outcome"] = json!("provisioned");
        assert_eq!(receipt, first);
    };
    // Later on the host clock: the recorded install time is the cell's.
    replayed(&mut cell, NOW + 60);
    assert_eq!(definitions_at(&mut cell, Some(&owner), NOW + 60), installed);
    // The reply was lost and the host restarted: the retry meets a non-fresh
    // engine, serves it and replays.
    drop(cell);
    let mut cell = open(&path, &scope);
    assert!(cell.tenant.is_provisioned());
    let later = NOW + 120;
    replayed(&mut cell, later);
    assert_eq!(definitions_at(&mut cell, Some(&owner), later), installed);
    // A different submission never replaces the cell.
    let other = provision_at(&mut cell, &submission(&plain()), later);
    assert_eq!(
        (other.status, code(&other).as_str()),
        (409, provision::codes::PROVISION_CONFLICT)
    );
    let empty = provision_at(&mut cell, &submission(&[]), later);
    assert_eq!(empty.status, 409);
    assert_eq!(definitions_at(&mut cell, Some(&owner), later), installed);
    drop(cell);
    // The store-level contract under it: provisioning a non-fresh engine
    // again (another install time, or other definitions) refuses.
    let trusted: TrustedBase = base::trusted_base().unwrap();
    for body in [body.clone(), submission(&plain())] {
        let admitted = provision::admit_submission(&trusted, &scope, &body).unwrap();
        let engine = NativeEngine::open(&path, StoreOptions::default()).unwrap();
        let refusal =
            provision::provision_cell(engine, &scope, &trusted, &admitted, (NOW + 5) * 1000)
                .map(|(_, receipt)| receipt)
                .expect_err("a cell is provisioned once");
        assert_eq!(refusal.code, provision::codes::PROVISION_CONFLICT);
    }
    let mut cell = open(&path, &scope);
    assert_eq!(definitions(&mut cell, Some(&owner)), installed);
}

// ------------------------------------------------------------ unprovisioned

/// Requests no verified principal makes, across the host's routes.
fn outsider_requests(scope: &CellScope) -> Vec<(&'static str, String, String, &'static str)> {
    let scoped = query(scope);
    let mut requests = Vec::new();
    for query in [
        scoped.clone(),
        String::new(),
        "orgId=org_z&environmentId=prod".to_owned(),
        format!("{scoped}&unknown=1"),
        format!("{scoped}&limit=zz"),
        format!("{scoped}&version=x"),
    ] {
        for (method, path, body) in [
            ("GET", "/v1/definitions", ""),
            ("DELETE", "/v1/definitions", ""),
            ("GET", "/v1/definitions:list", ""),
            ("GET", "/v1/definitions/studio.dashboard.d1", ""),
            ("POST", "/v1/definitions:install", "{}"),
            ("POST", "/v1/definitions:admit", "not json"),
            (
                "POST",
                "/v1/definitions:install",
                r#"{"kind":"dashboard","id":"studio.dashboard.d1","payload":{"title":"","tiles":[]}}"#,
            ),
            ("POST", "/v1/definitions/studio.dashboard.d1:retire", "{}"),
            ("GET", "/v1/objects/canon.Item", ""),
            ("GET", "/v1/objects/tenant.Unknown", ""),
            ("GET", "/v1/objects/canon.Item/b1", ""),
            ("GET", "/v1/objects/canon.Item/b1/links", ""),
            ("GET", "/v1/objects/canon.Item/b1/lineage", ""),
            ("GET", "/v1/objects/canon.Item/%FF", ""),
            ("POST", "/v1/objects/canon.Item", ""),
            ("POST", "/v1/actions/canon.rename", "{}"),
            ("GET", "/v1/actions/canon.rename", ""),
            ("POST", "/v1/questions:evaluate", "{}"),
            ("GET", "/v1/questions:trace", ""),
            ("GET", "/v1/questions/q/answer", ""),
            ("GET", "/v1/questions/q/trace", ""),
            ("GET", "/v1/access", ""),
            ("POST", "/v1/access", ""),
            ("GET", "/v1/nothing", ""),
            ("GET", "/", ""),
            ("POST", PROVISION_PATH, "{}"),
            ("GET", PROVISION_PATH, ""),
        ] {
            requests.push((method, path.to_owned(), query.clone(), body));
        }
    }
    requests
}

/// A caller nobody verified cannot tell a scope with no cell from a
/// provisioned one: every route answers the same status and the same bytes.
/// A verified member of the scope's organization, and nobody else, is told
/// the scope has no cell, in a refusal of the same shape.
#[test]
fn an_unprovisioned_scope_is_told_only_to_a_verified_member() {
    let directory = tempfile::tempdir().unwrap();
    let scope = scope("org_a", "prod");
    let mut vacant = open(&file(&directory, "vacant"), &scope);
    let mut served = provisioned(&file(&directory, "served"), &scope, &full());
    let outsiders: Vec<Option<String>> = vec![
        None,
        Some(user("org_b", "owner")),
        Some(user("org_a", "analyst")),
        Some(token("service", "org_a", "owner")),
        Some("Bearer nonsense".to_owned()),
        Some("Basic b3duZXI6b3duZXI=".to_owned()),
    ];
    let mut compared = 0;
    for (method, path, query, body) in outsider_requests(&scope) {
        for outsider in &outsiders {
            let at_vacant = call(
                &mut vacant,
                method,
                &path,
                &query,
                outsider.as_deref(),
                body,
            );
            let at_served = call(
                &mut served,
                method,
                &path,
                &query,
                outsider.as_deref(),
                body,
            );
            assert_eq!(
                at_vacant, at_served,
                "{method} {path}?{query} as {outsider:?}"
            );
            assert_ne!(at_vacant.status, 200);
            assert!(!at_vacant.body.contains(provision::codes::NOT_PROVISIONED));
            compared += 1;
        }
    }
    assert!(compared > 500, "{compared}");
    assert!(!vacant.tenant.is_provisioned());

    // A verified member is told, on every route but provisioning itself.
    let denial = json_of(&definitions(&mut vacant, None));
    for role in ["owner", "admin", "member", "viewer"] {
        let member = user("org_a", role);
        for (method, path) in [
            ("GET", "/v1/definitions"),
            ("GET", "/v1/objects/canon.Item"),
            ("GET", "/v1/access"),
            ("POST", "/v1/actions/canon.rename"),
        ] {
            let response = call(
                &mut vacant,
                method,
                path,
                &query(&scope),
                Some(&member),
                "{}",
            );
            assert_eq!(
                (response.status, code(&response).as_str()),
                (404, provision::codes::NOT_PROVISIONED),
                "{role} {path}"
            );
            // The same refusal document shape as the opaque denial.
            let keys = |value: &Value| {
                value
                    .as_object()
                    .unwrap()
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
            };
            assert_eq!(keys(&json_of(&response)), keys(&denial));
        }
        // Naming another scope, the member learns nothing about this one.
        for other in [
            "orgId=org_a&environmentId=dev",
            "orgId=org_b&environmentId=prod",
            "orgId=org_a",
            "",
        ] {
            let at_vacant = call(
                &mut vacant,
                "GET",
                "/v1/definitions",
                other,
                Some(&member),
                "",
            );
            assert_eq!(
                at_vacant,
                call(&mut vacant, "GET", "/v1/definitions", other, None, ""),
                "{role} {other}"
            );
        }
    }
    // A service principal with a declared role is a member too.
    let service = token("service", "org_a", "viewer");
    assert_eq!(definitions(&mut vacant, Some(&service)).status, 404);
}

// --------------------------------------------------------------- isolation

/// Two scopes side by side. Scope A's owner is nobody at scope B: it reads
/// nothing, writes nothing and cannot provision or re-provision B; B's
/// storage never opens under A's scope.
#[test]
fn two_scopes_are_isolated_from_each_other() {
    let directory = tempfile::tempdir().unwrap();
    let (scope_a, scope_b) = (scope("org_a", "prod"), scope("org_b", "prod"));
    let (path_a, path_b) = (file(&directory, "a"), file(&directory, "b"));
    let mut a = provisioned(&path_a, &scope_a, &full());
    let mut b = provisioned(&path_b, &scope_b, &plain());
    let (owner_a, owner_b) = (user("org_a", "owner"), user("org_b", "owner"));

    let own = |cell: &mut Cell<NativeEngine>, owner: &str| json_of(&definitions(cell, Some(owner)));
    let (defs_a, defs_b) = (own(&mut a, &owner_a), own(&mut b, &owner_b));
    assert_ne!(defs_a, defs_b);
    // Each cell has its own identity.
    let replay = |cell: &mut Cell<NativeEngine>, owner: &str, artifacts: &[String]| {
        json_of(&provision(cell, Some(owner), &submission(artifacts)))
    };
    let (id_a, id_b) = (
        replay(&mut a, &owner_a, &full())["cellId"].clone(),
        replay(&mut b, &owner_b, &plain())["cellId"].clone(),
    );
    assert!(id_a.is_string() && id_b.is_string());
    assert_ne!(id_a, id_b);

    // A's owner at B, naming B's scope or its own: exactly what nobody gets.
    for (method, path, query_text, body) in outsider_requests(&scope_b)
        .into_iter()
        .chain(outsider_requests(&scope_a))
    {
        let as_a = call(&mut b, method, &path, &query_text, Some(&owner_a), body);
        let as_nobody = call(&mut b, method, &path, &query_text, None, body);
        assert_eq!(as_a, as_nobody, "{method} {path}?{query_text}");
        assert_ne!(as_a.status, 200);
    }
    // Provisioning B with A's authority, with B's own set or another.
    for artifacts in [plain(), full()] {
        let response = provision(&mut b, Some(&owner_a), &submission(&artifacts));
        assert_eq!(
            (response.status, code(&response).as_str()),
            (403, "authz.denied")
        );
    }
    // A object read by A's admin at B is denied; at A it is served.
    let admin_a = user("org_a", "admin");
    assert_eq!(
        call(
            &mut b,
            "GET",
            "/v1/objects/canon.Item",
            &query(&scope_b),
            Some(&admin_a),
            ""
        )
        .status,
        403
    );
    assert_eq!(
        call(
            &mut a,
            "GET",
            "/v1/objects/canon.Item",
            &query(&scope_a),
            Some(&admin_a),
            ""
        )
        .status,
        200
    );
    // B is exactly as it was.
    assert_eq!(own(&mut b, &owner_b), defs_b);
    assert_eq!(own(&mut a, &owner_a), defs_a);
    drop((a, b));

    // B's storage under A's scope does not open: the scope binding refuses,
    // so a mis-routed request can never be served another tenant's cell.
    let misbound = try_open(&path_b, &scope_a)
        .err()
        .expect("another scope's store");
    assert!(misbound.contains("scope"), "{misbound}");
    // And B still opens as B, unchanged.
    let mut b = open(&path_b, &scope_b);
    assert_eq!(own(&mut b, &owner_b), defs_b);

    // The same organization's other environment is its own vacant cell.
    let scope_dev = scope("org_a", "dev");
    let mut dev = open(&file(&directory, "a-dev"), &scope_dev);
    let response = definitions(&mut dev, Some(&owner_a));
    assert_eq!(
        (response.status, code(&response).as_str()),
        (404, provision::codes::NOT_PROVISIONED)
    );
    assert_eq!(
        definitions(&mut dev, Some(&owner_b)),
        definitions(&mut dev, None)
    );
}

/// The isolation boundary, pinned as it is. A host token names an
/// organization and a role, never an environment: organizations are isolated
/// by the token, while the environments of one organization are separate
/// cells (their own storage, identity and definitions) that share principals
/// and roles. One token is accepted at both; a tenant that wants the
/// environments to differ says so in its own policy.
#[test]
fn environments_of_one_organization_share_principals_and_roles() {
    let directory = tempfile::tempdir().unwrap();
    let (prod, dev) = (scope("org_a", "prod"), scope("org_a", "dev"));
    let owner = user("org_a", "owner");
    let viewer = user("org_a", "viewer");
    // The same owner token provisions both environments.
    let mut at_prod = provisioned(&file(&directory, "prod"), &prod, &full());
    let mut at_dev = open(&file(&directory, "dev"), &dev);
    let created = provision(&mut at_dev, Some(&owner), &submission(&plain()));
    assert_eq!(created.status, 200, "{}", created.body);
    // The same viewer token reads both, with its role in each.
    let (in_prod, in_dev) = (
        definitions(&mut at_prod, Some(&viewer)),
        definitions(&mut at_dev, Some(&viewer)),
    );
    assert_eq!((in_prod.status, in_dev.status), (200, 200));
    // They are different cells all the same: own identity, own definitions.
    assert_ne!(in_prod.body, in_dev.body);
    let id = |cell: &mut Cell<NativeEngine>, artifacts: &[String]| {
        json_of(&provision(cell, Some(&owner), &submission(artifacts)))["cellId"].clone()
    };
    assert_ne!(id(&mut at_prod, &full()), id(&mut at_dev, &plain()));
    // A request is served only by the cell of the scope it names.
    let crossed = call(
        &mut at_prod,
        "GET",
        "/v1/definitions",
        &query(&dev),
        Some(&viewer),
        "",
    );
    assert_eq!(
        crossed,
        call(
            &mut at_prod,
            "GET",
            "/v1/definitions",
            &query(&dev),
            None,
            ""
        )
    );
    assert_eq!(crossed.status, 403);
}

// ------------------------------------------------------------------ faults

type Faulted = FaultEngine<DoEngine<DoStorageEmulator>>;

/// Remove a SQLite store and its journal files.
fn remove_store(path: &Path) {
    for suffix in ["", "-wal", "-shm", "-journal"] {
        let file = PathBuf::from(format!("{}{suffix}", path.display()));
        if let Err(error) = std::fs::remove_file(&file) {
            assert_eq!(
                error.kind(),
                std::io::ErrorKind::NotFound,
                "{}",
                file.display()
            );
        }
    }
}

fn open_faulted(
    path: &Path,
    scope: &CellScope,
    faults: &FaultSwitch,
) -> Result<Cell<Faulted>, String> {
    let (path, faults) = (path.to_owned(), faults.clone());
    with_engine(scope, move || {
        let native = NativeEngine::open_unsynced(&path, StoreOptions::default())
            .map_err(|error| error.to_string())?;
        Ok(FaultEngine::new(
            DoEngine::new(DoStorageEmulator::new(native).with_faults(faults.clone())),
            faults.clone(),
        ))
    })
}

/// What a sweep saw: faults that left no cell, faults after the commit that
/// left the whole cell, and the cases run.
struct Swept {
    vacant: u32,
    whole: u32,
    cases: u32,
    /// Plans whose statement list ended while faults still fired: the
    /// transaction may have more statements than the list reached.
    unexhausted: Vec<String>,
}

/// HOST-PROVISION-ATOMIC through the request layer, on the Durable Object
/// transaction glue: a crash or an I/O error at each statement in `first`
/// of the request's first transaction (and `later` of its second), and a
/// failed storage commit of each, leaves either no cell (the retry creates
/// one) or the whole cell (only the reply was lost; the retry replays),
/// never a part. A fault that does not fire leaves a served cell. The
/// submission is the trusted base alone: the whole provisioning transaction
/// with the least admission work.
/// The statements tried in one transaction: past its last statement only the
/// commit point (`u32::MAX`) is left, so the rest are skipped once a fault
/// stops firing.
fn fault_sweep(first: &[u32], later: &[u32]) -> Swept {
    fault_sweep_of(
        &[
            EngineFaultKind::Crash,
            EngineFaultKind::Io,
            EngineFaultKind::Commit,
        ],
        first,
        later,
    )
}

/// [`fault_sweep`] for the fault `kinds` only (a shard of an exhaustive run).
fn fault_sweep_of(kinds: &[EngineFaultKind], first: &[u32], later: &[u32]) -> Swept {
    let directory = tempfile::tempdir().unwrap();
    let scope = scope("org_a", "prod");
    let owner = user("org_a", "owner");
    let body = submission(&[]);
    let mut plans: Vec<(EngineFaultKind, u32, &[u32])> = Vec::new();
    for kind in [EngineFaultKind::Crash, EngineFaultKind::Io] {
        if kinds.contains(&kind) {
            plans.push((kind, 0, first));
            plans.push((kind, 1, later));
        }
    }
    let commit_point = [u32::MAX];
    if kinds.contains(&EngineFaultKind::Commit) {
        for transaction in 0..2 {
            plans.push((EngineFaultKind::Commit, transaction, &commit_point));
        }
    }
    let mut swept = Swept {
        vacant: 0,
        whole: 0,
        cases: 0,
        unexhausted: Vec::new(),
    };
    for (kind, transaction, statements) in plans {
        let mut past = false;
        for &at_statement in statements {
            if past && at_statement != u32::MAX {
                continue;
            }
            swept.cases += 1;
            let at = format!("{kind:?} at transaction {transaction}, statement {at_statement}");
            let path = file(&directory, &format!("fault-{}", swept.cases));
            let faults = FaultSwitch::default();
            let mut cell = open_faulted(&path, &scope, &faults).unwrap();
            faults.arm(EngineFault {
                kind,
                transaction,
                at_statement,
            });
            let response = provision(&mut cell, Some(&owner), &body);
            let hit = faults.disarm();
            drop(cell);
            // The platform restarts the object; no fault is armed.
            let mut cell = open_faulted(&path, &scope, &faults)
                .unwrap_or_else(|error| panic!("{at}: recovery refused: {error}"));
            if hit {
                assert!(response.status >= 400, "{at}: {}", response.body);
                assert!(json_of(&response)["code"].is_string(), "{at}");
                let whole = cell.tenant.is_provisioned();
                let retry = provision(&mut cell, Some(&owner), &body);
                assert_eq!(retry.status, 200, "{at}: {}", retry.body);
                let expected = if whole { "replayed" } else { "provisioned" };
                assert_eq!(json_of(&retry)["outcome"], expected, "{at}");
                if whole {
                    swept.whole += 1;
                } else {
                    swept.vacant += 1;
                }
            } else {
                assert_eq!(response.status, 200, "{at}: {}", response.body);
                assert!(cell.tenant.is_provisioned(), "{at}");
            }
            assert_eq!(definitions(&mut cell, Some(&owner)).status, 200, "{at}");
            drop(cell);
            // An exhaustive sweep runs tens of thousands of cases: each
            // case's store goes once it is checked, or the disk fills.
            remove_store(&path);
            past = !hit;
        }
        if !past && statements.len() > 1 {
            swept
                .unexhausted
                .push(format!("{kind:?} at transaction {transaction}"));
        }
    }
    swept
}

/// [`fault_sweep`] sampled for every run: the first 8 statements of the
/// provisioning transaction, then every 61st, and its commit point. The
/// store's own correspondence
/// (`definitions_install::host_provision_atomic_at_every_storage_boundary`)
/// injects at every SQL operation of the same `provision_definitions`
/// transaction natively; [`a_fault_at_every_statement_of_provisioning_leaves_nothing_half_made`]
/// is the exhaustive host-level sweep, run on demand.
#[test]
fn a_fault_anywhere_in_provisioning_leaves_nothing_half_made() {
    let first: Vec<u32> = (0..8_u32)
        .chain((1..200_u32).map(|stride| stride * 61))
        .chain([u32::MAX])
        .collect();
    let swept = fault_sweep(&first, &[0, 1, u32::MAX]);
    assert!(
        swept.vacant >= 16,
        "the sweep must reach the provisioning transaction: {} faults left a vacant scope, {} a whole cell",
        swept.vacant,
        swept.whole
    );
}

/// [`fault_sweep`] at every statement of both transactions and their commit
/// points. Minutes in a release build:
/// `cargo test --release -p norn-cell-do --test provision -- --ignored`.
#[test]
#[ignore = "exhaustive host-level sweep; run on demand in release mode"]
fn a_fault_at_every_statement_of_provisioning_leaves_nothing_half_made() {
    // A run may be sharded: NORN_SWEEP_KIND (crash, io, commit; default all)
    // and NORN_SWEEP_FROM / NORN_SWEEP_TO (statement range; default all).
    let bound = |name: &str, default: u32| {
        std::env::var(name).map_or(default, |value| value.parse().unwrap())
    };
    let (from, to) = (bound("NORN_SWEEP_FROM", 0), bound("NORN_SWEEP_TO", 400_000));
    let kinds: Vec<EngineFaultKind> = match std::env::var("NORN_SWEEP_KIND").as_deref() {
        Ok("crash") => vec![EngineFaultKind::Crash],
        Ok("io") => vec![EngineFaultKind::Io],
        Ok("commit") => vec![EngineFaultKind::Commit],
        Ok(other) => panic!("NORN_SWEEP_KIND is crash, io or commit, not {other:?}"),
        Err(_) => vec![
            EngineFaultKind::Crash,
            EngineFaultKind::Io,
            EngineFaultKind::Commit,
        ],
    };
    let every: Vec<u32> = (from..to).chain([u32::MAX]).collect();
    let started = std::time::Instant::now();
    let swept = fault_sweep_of(&kinds, &every, &every);
    println!(
        "exhaustive provisioning sweep: {} cases, {} left no cell, {} left the whole cell, {:.1} s",
        swept.cases,
        swept.vacant,
        swept.whole,
        started.elapsed().as_secs_f64()
    );
    assert!(swept.vacant >= 16 || from > 0 || kinds == [EngineFaultKind::Commit]);
    // A shard that runs to the bound must reach the end of every transaction
    // (a fault that no longer fires); a shard ending earlier stops by design.
    if to == 400_000 {
        assert!(
            swept.unexhausted.is_empty(),
            "statements left past the bound in: {:?}",
            swept.unexhausted
        );
    }
}

// ------------------------------------------------------------- seeded cells

/// A cell this build seeds is never provisioned over: its organization's
/// owner is refused, and everyone else as everywhere.
#[test]
fn a_seeded_cell_is_not_provisioned_over() {
    let scope = seed::demo_scope();
    let store = seed::provision(NativeEngine::open_in_memory().unwrap(), &scope).unwrap();
    let tenant = Tenant::seeded(scope.clone(), &DEMO_GRANTS, store, None).unwrap();
    let tokens =
        TokenPrincipals::new(ISSUER, &jwks(), scope.clone(), tenant.caller(), NOW).unwrap();
    let mut cell = Cell { tenant, tokens };
    let owner = user(&scope.org_id, "owner");
    let before = definitions(&mut cell, Some(&owner));
    assert_eq!(before.status, 200);
    let response = provision(&mut cell, Some(&owner), &submission(&full()));
    assert_eq!(
        (response.status, code(&response).as_str()),
        (409, provision::codes::PROVISION_CONFLICT)
    );
    assert_eq!(
        provision(
            &mut cell,
            Some(&user(&scope.org_id, "admin")),
            &submission(&full())
        )
        .status,
        403
    );
    assert_eq!(definitions(&mut cell, Some(&owner)), before);
}

/// A seeded demo cell with one run waiting on its ask (deciders admin or
/// owner), started by an admin; `handles` keys its receipt coordinates.
fn waiting_demo_cell(handles: Option<&[u8]>) -> (Cell<NativeEngine>, String) {
    let scope = seed::demo_scope();
    let store = seed::provision(NativeEngine::open_in_memory().unwrap(), &scope).unwrap();
    let tenant = Tenant::seeded(scope.clone(), &DEMO_GRANTS, store, None)
        .unwrap()
        .with_receipt_handles(handles)
        .unwrap();
    let tokens =
        TokenPrincipals::new(ISSUER, &jwks(), scope.clone(), tenant.caller(), NOW).unwrap();
    let mut cell = Cell { tenant, tokens };
    let at = query(&scope);
    let owner = user(&scope.org_id, "owner");
    let artifacts: Vec<Value> = serde_json::from_str::<Vec<Value>>(include_str!(
        "../../../tools/norn-ts/fixtures/flows/ask.ir.json"
    ))
    .unwrap()
    .into_iter()
    .map(|mut artifact| {
        artifact["scope"] = json!({ "orgId": scope.org_id, "environmentId": scope.environment_id });
        artifact
    })
    .collect();
    let mut body = json!({ "kind": "flow", "id": "ops.approve_refund", "payload": {
        "flow": { "id": "ops.approve_refund", "version": "1.0.0", "title": "Approve a refund" },
        "artifacts": artifacts } });
    let admitted = call(
        &mut cell,
        "POST",
        "/v1/definitions:admit",
        &at,
        Some(&owner),
        &body.to_string(),
    );
    assert_eq!(admitted.status, 200, "{}", admitted.body);
    body["signoff"] = json!({ "restatementSha256": json_of(&admitted)["restatementSha256"] });
    let installed = call(
        &mut cell,
        "POST",
        "/v1/definitions:install",
        &at,
        Some(&owner),
        &body.to_string(),
    );
    assert_eq!(installed.status, 200, "{}", installed.body);
    let started = call(
        &mut cell,
        "POST",
        "/v1/runs",
        &at,
        Some(&user(&scope.org_id, "admin")),
        &json!({ "flow": "ops.approve_refund", "runId": "refund-1" }).to_string(),
    );
    assert_eq!(started.status, 201, "{}", started.body);
    (cell, at)
}

/// What `role` sees in the inbox, and what its :decide of the run's
/// decision answers.
fn inbox_and_decide(
    cell: &mut Cell<NativeEngine>,
    at: &str,
    role: &str,
    id: &str,
) -> (Response, Response) {
    let caller = user(&seed::demo_scope().org_id, role);
    let page = call(
        cell,
        "GET",
        "/v1/decisions",
        &format!("{at}&state=pending"),
        Some(&caller),
        "",
    );
    let decided = call(
        cell,
        "POST",
        &format!("/v1/decisions/{id}:decide"),
        at,
        Some(&caller),
        &json!({ "choice": "approve", "idempotencyKey": format!("k-{role}") }).to_string(),
    );
    (page, decided)
}

/// DECISIONS-1 on the seeded demo cell: for every role, the inbox lists
/// exactly the decisions that role's :decide accepts. The demo cell publishes
/// no questions; its lineage-keyed receipt handles are its disclosure
/// authority, so its deciders decide.
#[test]
fn every_role_lists_exactly_what_it_may_decide() {
    let admin = user(&seed::demo_scope().org_id, "admin");
    for (role, decides) in [
        ("viewer", false),
        ("member", false),
        ("admin", true),
        ("owner", true),
    ] {
        // A fresh cell per role: deciding resolves the decision.
        let (mut cell, at) = waiting_demo_cell(Some(&[7; 32]));
        let pending = format!("{at}&state=pending");
        let page = call(
            &mut cell,
            "GET",
            "/v1/decisions",
            &pending,
            Some(&admin),
            "",
        );
        let id = json_of(&page)["items"][0]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let (page, decided) = inbox_and_decide(&mut cell, &at, role, &id);
        assert_eq!(page.status, 200, "{role}: {}", page.body);
        let listed: Vec<String> = json_of(&page)["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["id"].as_str().unwrap().to_owned())
            .collect();
        let expected: Vec<String> = if decides {
            vec![id.clone()]
        } else {
            Vec::new()
        };
        assert_eq!(listed, expected, "{role}");
        let status = if decides { 200 } else { 404 };
        assert_eq!(decided.status, status, "{role}: {}", decided.body);
    }
}

/// A cell with no disclosure authority decides nothing, and lists nothing
/// either: its inbox never offers a decision its :decide refuses.
#[test]
fn a_cell_that_cannot_decide_lists_nothing() {
    let (mut cell, at) = waiting_demo_cell(None);
    let (page, decided) = inbox_and_decide(&mut cell, &at, "admin", "dec_any");
    assert_eq!(page.status, 403, "{}", page.body);
    assert_eq!(
        (decided.status, code(&decided).as_str()),
        (404, "decision.not_found")
    );
}

/// A lineage key shorter than 32 bytes refuses to open the cell rather than
/// serve weakly keyed receipt handles.
#[test]
fn a_short_lineage_key_refuses_to_open() {
    let directory = tempfile::tempdir().unwrap();
    let path = file(&directory, "a");
    let refused = Tenant::open(
        scope("org_a", "prod"),
        &DEMO_GRANTS,
        Box::new(move || {
            NativeEngine::open(&path, StoreOptions::default()).map_err(|error| error.to_string())
        }),
        Some(&[1; 16]),
    );
    assert!(refused.is_err());
}

/// DEFS-REOPEN-1, the alpha incident of 2026-11-12: a provisioned cell lands
/// rows through its mapping path, then takes a runtime-defined upload type
/// (`:load`, then `:define`), then its host restarts. The cell reopens at
/// its active definitions with every row it had; a definitions install never
/// commits a model the next open would refuse.
#[test]
fn a_runtime_defined_type_survives_a_restart() {
    use norn_api::uploads::{LoadRequest, generate, install, load_rows, parse_csv, sha256_hex};
    let directory = tempfile::tempdir().unwrap();
    let path = file(&directory, "cell");
    let scope = scope("org_a", "dev");
    let owner = user(&scope.org_id, "owner");
    // Past provisioning's logical clock, in milliseconds.
    let at = 4_000_000_000_000;
    {
        let cell = provisioned(&path, &scope, &full());
        let host = cell.tenant.serving_host().expect("a served cell");
        for (name, csv, uri, rows) in [
            (
                "Sales",
                "region,units\nnorth,3\nsouth,4\n",
                "s3://uploads/a.csv",
                true,
            ),
            ("Probe", "sku,qty\nA,1\n", "s3://uploads/b.csv", false),
        ] {
            let table = parse_csv(csv).unwrap();
            let sha = sha256_hex(csv.as_bytes());
            let request = LoadRequest {
                type_name: name.into(),
                columns: vec![],
                key: vec![],
                sha256: None,
            };
            let generated =
                generate(&table, &request, uri, &sha, "2026-11-12T12:00:00.000Z").unwrap();
            install(host, &generated, at).unwrap();
            if rows {
                let loaded = load_rows(host, &generated, &table, uri, &sha, at).unwrap();
                assert_eq!(loaded.rows_landed, 2, "{loaded:?}");
            }
        }
    }
    // The restarted host: the same file, opened at its active definitions.
    let mut cell = try_open(&path, &scope).expect("the cell reopens after a runtime-defined type");
    let response = definitions(&mut cell, Some(&owner));
    assert_eq!(response.status, 200, "{}", response.body);
    let host = cell.tenant.serving_host().expect("a served cell");
    let row = host
        .host_object("upload.Sales", "s3://uploads/a.csv#row=2")
        .unwrap()
        .expect("the landed row is kept");
    assert_eq!(
        row.get("region").and_then(norn_ir::RuntimeValue::as_str),
        Some("south")
    );
}

/// A request as the Worker hands it to the cell's connect routes.
fn post<'a>(path: &'a str, query: &'a str, authorization: &'a str, body: &'a str) -> Request<'a> {
    Request {
        method: "POST",
        path,
        query,
        authorization: Some(authorization),
        now: NOW,
        body,
    }
}

/// UPLOAD-RECORD-1 across a restart: an upload completed before the host
/// restarts is defined after it from the record the cell kept at
/// `:complete`; a digest the client sends that differs is refused, never
/// trusted in the kept one's place.
#[test]
fn a_completed_upload_survives_a_restart() {
    use norn_cell_do::connect::Connect;
    let directory = tempfile::tempdir().unwrap();
    let path = file(&directory, "cell");
    let scope = scope("org_a", "dev");
    let owner = user(&scope.org_id, "owner");
    let q = query(&scope);
    let mut set = full();
    for name in [
        "source.Synced.interface",
        "source.Connection.entity",
        "source.SyncRun.entity",
        "source.Upload.entity",
        "norn.connect.source",
        "norn.connect.mapping",
    ] {
        set.push(
            std::fs::read_to_string(format!(
                "{}/../../registry/packs/sources/artifacts/{name}.json",
                env!("CARGO_MANIFEST_DIR")
            ))
            .unwrap(),
        );
    }
    let vars = json!({
        "NORN_S3_ENDPOINT": "https://storage.example.test", "NORN_S3_BUCKET": "uploads-bucket",
        "NORN_SECRET_S3_ACCESS_KEY_ID": "key-id", "NORN_SECRET_S3_SECRET_ACCESS_KEY": "secret",
    })
    .to_string();
    let csv = b"region,units\nnorth,3\nsouth,4\n";
    let at = 4_000_000_000_000;
    let upload_id = {
        let cell = provisioned(&path, &scope, &set);
        let host = cell.tenant.serving_host().expect("a served cell");
        let mut connect = Connect::new("[]", &vars).unwrap();
        let request = r#"{"fileName": "sales.csv", "contentType": "text/csv", "size": 30}"#;
        let begun = connect
            .route(
                Some(host),
                &cell.tokens,
                &post("/v1/uploads:begin", &q, &owner, request),
                at,
            )
            .unwrap();
        assert_eq!(begun.response.status, 200, "{}", begun.response.body);
        let id = json_of(&begun.response)["uploadId"]
            .as_str()
            .unwrap()
            .to_owned();
        let complete = format!("/v1/uploads/{id}:complete");
        let routed = connect
            .route(
                Some(host),
                &cell.tokens,
                &post(&complete, &q, &owner, "{}"),
                at,
            )
            .unwrap();
        let (token, _read) = routed.then.expect("a read of the object");
        let completed = connect.resume(Some(host), &token, 200, csv, at);
        assert_eq!(completed.status, 200, "{}", completed.body);
        id
    };
    // The restarted host: the reopened cell and a Connect that never saw it.
    let cell = open(&path, &scope);
    let host = cell.tenant.serving_host().expect("a served cell");
    let kept = norn_api::sources::kept_upload(host, &upload_id)
        .unwrap()
        .expect("the kept record");
    assert_eq!(kept.sha256, norn_api::uploads::sha256_hex(csv));
    assert_eq!(kept.file_name, "sales.csv");
    assert!(!kept.owner.is_empty());
    let mut connect = Connect::new("[]", &vars).unwrap();
    let define = format!("/v1/uploads/{upload_id}:define");
    let mut define_with = |body: &str| {
        let routed = connect
            .route(
                Some(host),
                &cell.tokens,
                &post(&define, &q, &owner, body),
                at,
            )
            .unwrap();
        let (token, _read) = routed.then.expect("a read of the object");
        connect.resume(Some(host), &token, 200, csv, at)
    };
    let claimed = json!({"typeName": "Sales", "sha256": "0".repeat(64)}).to_string();
    assert_eq!(define_with(&claimed).status, 409);
    let defined = define_with(r#"{"typeName": "Sales"}"#);
    assert_eq!(defined.status, 200, "{}", defined.body);
    assert_eq!(json_of(&defined)["objectType"], "upload.Sales");
}

/// A stored cell whose layout is not the one its definitions rebuild (the
/// alpha incident of 2026-11-12) opens into a refusal state, never a retried
/// open on every request. A verified member is told `cell.unopenable` (503),
/// with the reason only for a definition writer; provisioning never overwrites
/// it; anyone else is answered exactly as for a scope with no cell.
#[test]
fn a_cell_that_does_not_open_is_refused_never_retried_or_exposed() {
    let directory = tempfile::tempdir().unwrap();
    let scope = scope("org_a", "prod");
    let path = file(&directory, "broken");
    drop(provisioned(&path, &scope, &full()));
    let engine = NativeEngine::open(&path, StoreOptions::default()).unwrap();
    engine
        .execute(
            "UPDATE _norn_meta SET value = ? WHERE key = 'ddl_hash'",
            &[SqlValue::Text("0".repeat(64))],
        )
        .unwrap();
    drop(engine);
    let mut broken = open(&path, &scope);
    let reason = broken.tenant.open_error().map(str::to_owned).unwrap();
    assert!(reason.contains("schema mismatch"), "{reason}");
    let mut vacant = open(&file(&directory, "vacant"), &scope);
    let outsiders = [
        None,
        Some(user("org_b", "owner")),
        Some(user("org_a", "analyst")),
        Some("Bearer nonsense".to_owned()),
    ];
    for (method, path, query, body) in outsider_requests(&scope) {
        for outsider in &outsiders {
            assert_eq!(
                call(
                    &mut broken,
                    method,
                    &path,
                    &query,
                    outsider.as_deref(),
                    body
                ),
                call(
                    &mut vacant,
                    method,
                    &path,
                    &query,
                    outsider.as_deref(),
                    body
                ),
                "{method} {path}?{query} as {outsider:?}"
            );
        }
    }
    for (role, told) in [
        ("member", false),
        ("viewer", false),
        ("admin", true),
        ("owner", true),
    ] {
        let response = definitions(&mut broken, Some(&user("org_a", role)));
        let document = json_of(&response);
        assert_eq!(
            (response.status, document["code"].as_str()),
            (503, Some(provision::codes::UNOPENABLE)),
            "{role}"
        );
        assert_eq!(
            document["explanation"]
                .as_str()
                .is_some_and(|text| text.contains("schema mismatch")),
            told,
            "{role}: {document}"
        );
    }
    let again = provision(
        &mut broken,
        Some(&user("org_a", "owner")),
        &submission(&full()),
    );
    assert_eq!(
        (again.status, code(&again).as_str()),
        (503, provision::codes::UNOPENABLE)
    );
    assert!(!broken.tenant.is_provisioned());
}
