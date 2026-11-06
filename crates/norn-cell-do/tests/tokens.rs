//! Verified host tokens (#817) through the cell's dispatch: the Studio
//! Worker's signed five-minute token becomes a principal, its role becomes a
//! grant only through the declared tables, and every failure is one denial.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::arithmetic_side_effects,
    reason = "token correspondence"
)]
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signer, SigningKey};
use norn_api::host::DevHost;
use norn_cell::engine::NativeEngine;
use norn_cell_do::policy::{DEMO_GRANTS, ReadPolicy};
use norn_cell_do::principal::TokenPrincipals;
use norn_cell_do::{seed, server};
use serde_json::{Value, json};

const ISSUER: &str = "https://studio-preview.example";
const NOW: u64 = 1_800_000_000;
const SCOPE: &str = "orgId=org_demo&environmentId=dev";

fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn jwks(keys: &[(u8, &str)]) -> String {
    let keys: Vec<Value> = keys
        .iter()
        .map(|(seed, kid)| {
            json!({ "kty": "OKP", "crv": "Ed25519", "alg": "EdDSA", "kid": kid,
                    "x": URL_SAFE_NO_PAD.encode(key(*seed).verifying_key().as_bytes()) })
        })
        .collect();
    json!({ "keys": keys }).to_string()
}

fn sign(seed: u8, kid: &str, claims: &Value) -> String {
    let head = URL_SAFE_NO_PAD.encode(json!({ "alg": "EdDSA", "kid": kid }).to_string());
    let body = URL_SAFE_NO_PAD.encode(claims.to_string());
    let signature = key(seed).sign(format!("{head}.{body}").as_bytes());
    format!(
        "Bearer {head}.{body}.{}",
        URL_SAFE_NO_PAD.encode(signature.to_bytes())
    )
}

fn claims(kind: &str, org: &str, role: &str, amr: &str) -> Value {
    json!({ "iss": ISSUER, "aud": "norn-host", "sub": "u1", "iat": NOW - 10, "exp": NOW + 290,
            "norn": { "kind": kind, "org": org, "role": role, "amr": amr } })
}

struct Demo {
    host: DevHost<NativeEngine>,
    tokens: TokenPrincipals,
    caller: std::sync::Arc<norn_cell_do::policy::CallerSlot>,
    object_types: Vec<String>,
}

fn demo() -> Demo {
    let scope = seed::demo_scope();
    let store = seed::provision(NativeEngine::open_in_memory().unwrap(), &scope).unwrap();
    let policy = ReadPolicy::compile(store.model().object_types(), &scope, &DEMO_GRANTS).unwrap();
    let tokens =
        TokenPrincipals::new(ISSUER, &jwks(&[(1, "k1")]), scope.clone(), policy.caller()).unwrap();
    let caller = policy.caller();
    let object_types = store
        .model()
        .object_types()
        .map(|object_type| object_type.name.as_str().to_owned())
        .collect();
    let host = DevHost::from_store(store, scope, |command| command.payload.clone())
        .unwrap()
        .with_authorizer(policy);
    Demo {
        host,
        tokens,
        caller,
        object_types,
    }
}

fn defs(demo: &Demo, authorization: Option<&str>) -> server::Response {
    let request = server::Request {
        method: "GET",
        path: "/v1/definitions",
        query: SCOPE,
        authorization,
        now: NOW,
        body: "",
    };
    server::handle(&demo.host, &demo.tokens, &request)
}

#[test]
fn a_signed_studio_session_reads_through_its_declared_role() {
    let demo = demo();
    for role in ["owner", "admin", "member", "viewer"] {
        let token = sign(1, "k1", &claims("user", "org_demo", role, "password"));
        assert_eq!(defs(&demo, Some(&token)).status, 200, "{role}");
    }
    let service = sign(1, "k1", &claims("service", "org_demo", "viewer", "api_key"));
    assert_eq!(defs(&demo, Some(&service)).status, 200);
}

#[test]
fn every_rejected_token_gets_the_anonymous_denial() {
    let demo = demo();
    let anonymous = defs(&demo, None);
    assert_eq!(anonymous.status, 403);
    let mut expired = claims("user", "org_demo", "admin", "password");
    expired["iat"] = json!(NOW - 900);
    expired["exp"] = json!(NOW - 600);
    let mut long_lived = claims("user", "org_demo", "admin", "password");
    long_lived["exp"] = json!(NOW + 3_600);
    let mut other_issuer = claims("user", "org_demo", "admin", "password");
    other_issuer["iss"] = json!("https://attacker.example");
    let mut other_audience = claims("user", "org_demo", "admin", "password");
    other_audience["aud"] = json!("someone-else");
    for (case, token) in [
        ("expired", sign(1, "k1", &expired)),
        ("over five minutes", sign(1, "k1", &long_lived)),
        (
            "forged",
            sign(9, "k1", &claims("user", "org_demo", "admin", "password")),
        ),
        (
            "unknown key id",
            sign(9, "k9", &claims("user", "org_demo", "admin", "password")),
        ),
        (
            "foreign organization",
            sign(1, "k1", &claims("user", "org_other", "admin", "password")),
        ),
        (
            "undeclared role",
            sign(1, "k1", &claims("user", "org_demo", "analyst", "password")),
        ),
        (
            "service as admin",
            sign(1, "k1", &claims("service", "org_demo", "admin", "api_key")),
        ),
        ("other issuer", sign(1, "k1", &other_issuer)),
        ("other audience", sign(1, "k1", &other_audience)),
        ("not a bearer", "Basic dTE6cGFzcw==".to_owned()),
    ] {
        let refused = defs(&demo, Some(&token));
        assert_eq!(
            (refused.status, &refused.body),
            (403, &anonymous.body),
            "{case}"
        );
    }
}

#[test]
fn an_unknown_key_id_asks_for_one_refetch_and_new_keys_take_effect() {
    let demo = demo();
    let rotated = sign(2, "k2", &claims("user", "org_demo", "admin", "password"));
    assert_eq!(defs(&demo, Some(&rotated)).status, 403);
    assert!(demo.tokens.take_unknown_kid(), "an unknown kid is reported");
    assert!(!demo.tokens.take_unknown_kid(), "and reported once");
    // A known kid with a bad signature is not a reason to refetch.
    let forged = sign(9, "k1", &claims("user", "org_demo", "admin", "password"));
    assert_eq!(defs(&demo, Some(&forged)).status, 403);
    assert!(!demo.tokens.take_unknown_kid());
    demo.tokens
        .replace_jwks(&jwks(&[(1, "k1"), (2, "k2")]))
        .unwrap();
    assert_eq!(defs(&demo, Some(&rotated)).status, 200);
    assert!(
        demo.tokens.replace_jwks("{\"keys\":[]}").is_err(),
        "an empty JWKS keeps the old keys"
    );
    assert_eq!(defs(&demo, Some(&rotated)).status, 200);
}

#[test]
fn a_verified_caller_does_not_outlive_its_request() {
    let demo = demo();
    let token = sign(1, "k1", &claims("user", "org_demo", "admin", "password"));
    assert_eq!(defs(&demo, Some(&token)).status, 200);
    // The next, anonymous request records nobody: the previous caller is gone.
    assert_eq!(defs(&demo, None).status, 403);
}

fn access(demo: &Demo, query: &str, authorization: Option<&str>) -> server::Response {
    let request = server::Request {
        method: "GET",
        path: "/v1/access",
        query,
        authorization,
        now: NOW,
        body: "",
    };
    server::access(
        &demo.host,
        &demo.tokens,
        &demo.caller,
        &demo.object_types,
        &request,
    )
}

/// `GET /v1/access`: the verified principal and the cell's actual PDP decision
/// per capability, with the grant-table role that grants it.
#[test]
fn the_access_document_reports_each_roles_decisions() {
    let demo = demo();
    let viewer = sign(1, "k1", &claims("user", "org_demo", "viewer", "password"));
    let response = access(&demo, SCOPE, Some(&viewer));
    assert_eq!(response.status, 200, "{}", response.body);
    let document: Value = serde_json::from_str(&response.body).unwrap();
    assert_eq!(
        document["principal"],
        json!({ "subjectId": "user:u1", "actorKind": "user", "roles": ["viewer"], "roleTable": "studio-roles/1" })
    );
    assert_eq!(
        (document["policy"].as_str(), document["grantTable"].as_str()),
        (Some("demo.host.read"), Some("norn-cell-do-grants/2"))
    );
    assert_eq!(
        document["capabilities"],
        json!({
            "population": { "decision": "permit", "rule": "norn-cell-do-grants/2:viewer" },
            "values": { "decision": "deny", "rule": null },
            "sources": { "decision": "deny", "rule": null },
        })
    );
    let admin = sign(1, "k1", &claims("user", "org_demo", "admin", "password"));
    let document: Value = serde_json::from_str(&access(&demo, SCOPE, Some(&admin)).body).unwrap();
    for capability in ["population", "values", "sources"] {
        assert_eq!(
            document["capabilities"][capability],
            json!({ "decision": "permit", "rule": "norn-cell-do-grants/2:admin" }),
            "{capability}"
        );
    }
}

#[test]
fn anonymous_foreign_and_out_of_scope_access_reads_share_the_denial() {
    let demo = demo();
    let shared = defs(&demo, None);
    assert_eq!(shared.status, 403);
    let admin = sign(1, "k1", &claims("user", "org_demo", "admin", "password"));
    let foreign = sign(1, "k1", &claims("user", "org_other", "admin", "password"));
    for (query, authorization) in [
        (SCOPE, None),
        (SCOPE, Some(foreign.as_str())),
        ("orgId=org_other&environmentId=dev", Some(admin.as_str())),
        ("orgId=org_demo", Some(admin.as_str())),
    ] {
        let refused = access(&demo, query, authorization);
        assert_eq!(
            (refused.status, &refused.body),
            (403, &shared.body),
            "{query} {authorization:?}"
        );
    }
    assert_eq!(
        access(&demo, &format!("{SCOPE}&sql=1"), Some(&admin)).status,
        400
    );
}
