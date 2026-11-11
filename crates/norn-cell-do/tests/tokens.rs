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
use norn_cell_do::principal::{
    JWKS_MAX_AGE_SECS, JWKS_MAX_STALE_SECS, JWKS_RETRY_SECS, TokenPrincipals,
};
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
    demo_with(&[(1, "k1")])
}

fn demo_with(issuer_keys: &[(u8, &str)]) -> Demo {
    let scope = seed::demo_scope();
    let store = seed::provision(NativeEngine::open_in_memory().unwrap(), &scope).unwrap();
    let policy = ReadPolicy::compile(store.model().object_types(), &scope, &DEMO_GRANTS).unwrap();
    let tokens = TokenPrincipals::new(
        ISSUER,
        &jwks(issuer_keys),
        scope.clone(),
        policy.caller(),
        NOW,
    )
    .unwrap();
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
    defs_at(demo, authorization, NOW)
}

fn defs_at(demo: &Demo, authorization: Option<&str>, now: u64) -> server::Response {
    let request = server::Request {
        method: "GET",
        path: "/v1/definitions",
        query: SCOPE,
        authorization,
        now,
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
    assert!(
        demo.tokens.take_unknown_kid(NOW),
        "an unknown kid is reported"
    );
    assert!(!demo.tokens.take_unknown_kid(NOW), "and reported once");
    // A known kid with a bad signature is not a reason to refetch.
    let forged = sign(9, "k1", &claims("user", "org_demo", "admin", "password"));
    assert_eq!(defs(&demo, Some(&forged)).status, 403);
    assert!(!demo.tokens.take_unknown_kid(NOW));
    demo.tokens
        .replace_jwks(&jwks(&[(1, "k1"), (2, "k2")]), NOW)
        .unwrap();
    assert_eq!(defs(&demo, Some(&rotated)).status, 200);
    assert!(
        demo.tokens.replace_jwks("{\"keys\":[]}", NOW).is_err(),
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

/// A token valid at `now`, signed by `seed`'s key as `kid`.
fn admin_at(seed: u8, kid: &str, now: u64) -> String {
    let mut claims = claims("user", "org_demo", "admin", "password");
    let times = claims.as_object_mut().unwrap();
    times.insert("iat".into(), json!(now - 10));
    times.insert("exp".into(), json!(now + 290));
    sign(seed, kid, &claims)
}

/// S3: a key the issuer removed stops verifying at the next max-age refetch,
/// with no new key id ever appearing: the unknown-kid path never fires.
#[test]
fn a_removed_key_stops_verifying_at_the_max_age_refetch() {
    let demo = demo_with(&[(1, "k1"), (2, "k2")]);
    // Just before the maximum age the cached key is still honoured, and no
    // refetch is due.
    let early = NOW + JWKS_MAX_AGE_SECS - 1;
    assert_eq!(
        defs_at(&demo, Some(&admin_at(2, "k2", early)), early).status,
        200
    );
    assert!(!demo.tokens.refresh_due(early));
    // At the maximum age a refetch is due on its own, once per retry interval.
    let due = NOW + JWKS_MAX_AGE_SECS;
    assert!(demo.tokens.refresh_due(due));
    assert!(!demo.tokens.refresh_due(due), "one attempt per interval");
    // The cached keys still verify while the refetch is in flight.
    let revoked = admin_at(2, "k2", due);
    assert_eq!(defs_at(&demo, Some(&revoked), due).status, 200);
    // The issuer dropped k2 and published nothing new.
    demo.tokens.replace_jwks(&jwks(&[(1, "k1")]), due).unwrap();
    assert_eq!(defs_at(&demo, Some(&revoked), due).status, 403);
    assert_eq!(
        defs_at(&demo, Some(&admin_at(1, "k1", due)), due).status,
        200
    );
    // The keys are fresh again: nothing is due until the next maximum age.
    assert!(!demo.tokens.refresh_due(due + JWKS_MAX_AGE_SECS - 1));
    assert!(demo.tokens.refresh_due(due + JWKS_MAX_AGE_SECS));
}

/// S3: when refetches keep failing, keys past the stale bound are refused,
/// even for a validly signed fresh token, until a refetch succeeds.
#[test]
fn keys_past_the_stale_bound_are_refused_until_a_refetch_succeeds() {
    let demo = demo();
    let stale_edge = NOW + JWKS_MAX_STALE_SECS;
    assert_eq!(
        defs_at(&demo, Some(&admin_at(1, "k1", stale_edge)), stale_edge).status,
        200,
        "at the bound the keys still verify"
    );
    let stale = stale_edge + 1;
    let token = admin_at(1, "k1", stale);
    let anonymous = defs(&demo, None);
    let refused = defs_at(&demo, Some(&token), stale);
    assert_eq!((refused.status, &refused.body), (403, &anonymous.body));
    // Failed refetches keep the old keys aging, and retry on an interval.
    assert!(demo.tokens.refresh_due(stale));
    assert!(demo.tokens.replace_jwks("{\"keys\":[]}", stale).is_err());
    assert!(!demo.tokens.refresh_due(stale + JWKS_RETRY_SECS - 1));
    assert!(demo.tokens.refresh_due(stale + JWKS_RETRY_SECS));
    assert_eq!(defs_at(&demo, Some(&token), stale).status, 403);
    // A successful refetch restores service.
    demo.tokens
        .replace_jwks(&jwks(&[(1, "k1")]), stale)
        .unwrap();
    assert_eq!(defs_at(&demo, Some(&token), stale).status, 200);
}

/// Tokens naming unknown key ids ask for a refetch at most once per retry
/// interval, and a max-age attempt counts against the same interval.
#[test]
fn unknown_key_ids_and_age_share_one_retry_interval() {
    let demo = demo();
    let unknown = admin_at(9, "k9", NOW);
    assert_eq!(defs(&demo, Some(&unknown)).status, 403);
    assert!(demo.tokens.take_unknown_kid(NOW));
    let soon = NOW + JWKS_RETRY_SECS - 1;
    assert_eq!(defs_at(&demo, Some(&unknown), soon).status, 403);
    assert!(!demo.tokens.take_unknown_kid(soon), "inside the interval");
    let later = NOW + JWKS_RETRY_SECS;
    assert_eq!(defs_at(&demo, Some(&unknown), later).status, 403);
    assert!(demo.tokens.take_unknown_kid(later));
}

/// S3 (non-monotonic time): a stale denial followed by a clock rollback
/// without a replacement stays denied. The rolled-back clock must not make
/// the removed key "fresh" again.
#[test]
fn a_clock_rollback_after_stale_denial_stays_denied_without_a_refetch() {
    let demo = demo();
    let stale = NOW + JWKS_MAX_STALE_SECS + 1;
    assert_eq!(
        defs_at(&demo, Some(&admin_at(1, "k1", stale)), stale).status,
        403
    );
    // The clock moves back to where the keys look fresh by wall-clock age.
    let back = NOW + 100;
    let token = admin_at(1, "k1", back);
    assert_eq!(defs_at(&demo, Some(&token), back).status, 403);
    // Neither further rolled-back requests nor the passage of time restore
    // service: only a successful refetch does.
    assert_eq!(defs_at(&demo, Some(&token), back + 1).status, 403);
    assert_eq!(defs_at(&demo, Some(&token), NOW).status, 403);
    // A refetch is due so the host can recover, once per retry interval,
    // and a failed one keeps the keys refused.
    assert!(demo.tokens.refresh_due(back));
    assert!(!demo.tokens.refresh_due(back + JWKS_RETRY_SECS - 1));
    assert!(demo.tokens.replace_jwks("{\"keys\":[]}", back).is_err());
    assert_eq!(defs_at(&demo, Some(&token), back).status, 403);
}

/// S3 (non-monotonic time): a rollback that stays above `fetched` cannot
/// extend the stale bound, with or without an earlier denial.
#[test]
fn a_clock_rollback_above_fetched_does_not_extend_the_stale_bound() {
    let demo = demo();
    // Just inside the bound, the keys verify.
    let edge = NOW + JWKS_MAX_STALE_SECS;
    assert_eq!(
        defs_at(&demo, Some(&admin_at(1, "k1", edge)), edge).status,
        200
    );
    // The clock steps back a little, and then forward past the original
    // bound. The keys were never refetched, so they must not verify, even
    // though by the rolled-back clock they would still be within 15 minutes.
    let back = edge - 120;
    assert_eq!(
        defs_at(&demo, Some(&admin_at(1, "k1", back)), back).status,
        403
    );
    let past = NOW + JWKS_MAX_STALE_SECS + 60;
    assert_eq!(
        defs_at(&demo, Some(&admin_at(1, "k1", past)), past).status,
        403
    );
    // A refresh probe observing the regression alone has the same effect.
    let probe = self::demo();
    assert!(!probe.tokens.refresh_due(NOW + 200));
    assert!(
        probe.tokens.refresh_due(NOW + 100),
        "regression forces a refetch"
    );
    let at = NOW + 100;
    assert_eq!(
        defs_at(&probe, Some(&admin_at(1, "k1", at)), at).status,
        403
    );
}

/// S3 (non-monotonic time): after a clock regression, a successful refetch
/// is the only thing that restores verification, and ages from the refetch.
#[test]
fn a_successful_refetch_recovers_verification_after_a_clock_regression() {
    let demo = demo_with(&[(1, "k1"), (2, "k2")]);
    let ahead = NOW + 200;
    assert_eq!(
        defs_at(&demo, Some(&admin_at(2, "k2", ahead)), ahead).status,
        200
    );
    let back = NOW + 50;
    assert_eq!(
        defs_at(&demo, Some(&admin_at(2, "k2", back)), back).status,
        403
    );
    // The issuer meanwhile dropped k2; the refetch carries that.
    assert!(demo.tokens.refresh_due(back));
    demo.tokens.replace_jwks(&jwks(&[(1, "k1")]), back).unwrap();
    assert_eq!(
        defs_at(&demo, Some(&admin_at(1, "k1", back)), back).status,
        200
    );
    assert_eq!(
        defs_at(&demo, Some(&admin_at(2, "k2", back)), back).status,
        403,
        "the removed key stays removed"
    );
    // The refetch is the new origin for ageing, and the normal schedule
    // resumes on forward time.
    let later = back + JWKS_MAX_AGE_SECS;
    assert!(!demo.tokens.refresh_due(later - 1));
    assert!(demo.tokens.refresh_due(later));
    let edge = back + JWKS_MAX_STALE_SECS;
    assert_eq!(
        defs_at(&demo, Some(&admin_at(1, "k1", edge)), edge).status,
        200
    );
    assert_eq!(
        defs_at(&demo, Some(&admin_at(1, "k1", edge + 1)), edge + 1).status,
        403
    );
}
