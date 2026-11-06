#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    reason = "token fixtures must build before verification assertions"
)]
use super::*;
use ed25519_dalek::{Signer, SigningKey};
use serde_json::json;

const ISSUER: &str = "https://studio-preview.norn.cloud";
const NOW: u64 = 1_800_000_000;

fn key(seed: u8) -> SigningKey {
    SigningKey::from_bytes(&[seed; 32])
}

fn jwks() -> String {
    let x = URL_SAFE_NO_PAD.encode(key(1).verifying_key().as_bytes());
    json!({ "keys": [{ "kty": "OKP", "crv": "Ed25519", "x": x, "kid": "k1", "alg": "EdDSA" }] })
        .to_string()
}

fn verifier() -> TokenVerifier {
    TokenVerifier::from_jwks(ISSUER, "norn-host", &jwks(), &STUDIO_ROLES).unwrap()
}

fn cell() -> CellScope {
    CellScope {
        org_id: "org_a".into(),
        environment_id: "preview".into(),
    }
}

fn sign(signer: &SigningKey, header: &serde_json::Value, claims: &serde_json::Value) -> String {
    let head = URL_SAFE_NO_PAD.encode(header.to_string());
    let body = URL_SAFE_NO_PAD.encode(claims.to_string());
    let signature = signer.sign(format!("{head}.{body}").as_bytes());
    format!(
        "{head}.{body}.{}",
        URL_SAFE_NO_PAD.encode(signature.to_bytes())
    )
}

fn claims(kind: &str, role: &str, amr: &str) -> serde_json::Value {
    json!({ "iss": ISSUER, "aud": "norn-host", "sub": "u1", "iat": NOW - 10, "exp": NOW + 290,
            "norn": { "kind": kind, "org": "org_a", "role": role, "amr": amr } })
}

fn token(claims: &serde_json::Value) -> String {
    sign(&key(1), &json!({ "alg": "EdDSA", "kid": "k1" }), claims)
}

fn resolve(token: &str) -> Result<Principal, Refusal> {
    verifier().principal(token, &cell(), NOW)
}

#[test]
fn a_signed_session_token_maps_through_the_declared_role_table() {
    let principal = resolve(&token(&claims("user", "member", "password"))).unwrap();
    assert_eq!(
        principal,
        Principal {
            subject_id: "user:u1".into(),
            actor_kind: ActorKind::User,
            org_id: "org_a".into(),
            roles: vec!["member".into()],
            auth_method: "password".into(),
            role_table: "studio-roles/1",
        }
    );
    let link = resolve(&token(&claims("user", "admin", "magic_link"))).unwrap();
    assert_eq!(link.roles, ["admin"]);
}

#[test]
fn an_api_key_is_a_service_that_never_approves() {
    let service = resolve(&token(&claims("service", "viewer", "api_key"))).unwrap();
    assert_eq!(
        (service.subject_id.as_str(), service.actor_kind),
        ("service:u1", ActorKind::Service)
    );
    assert!(resolve(&token(&claims("service", "admin", "api_key"))).is_err());
    assert!(resolve(&token(&claims("service", "owner", "api_key"))).is_err());
    assert!(resolve(&token(&claims("service", "viewer", "password"))).is_err());
    assert!(resolve(&token(&claims("user", "viewer", "api_key"))).is_err());
}

#[test]
fn forged_foreign_stale_and_undeclared_tokens_deny_identically() {
    let mut cases: Vec<(&str, String)> = vec![
        (
            "other key",
            sign(
                &key(2),
                &json!({ "alg": "EdDSA", "kid": "k1" }),
                &claims("user", "admin", "password"),
            ),
        ),
        (
            "unknown kid",
            sign(
                &key(1),
                &json!({ "alg": "EdDSA", "kid": "k9" }),
                &claims("user", "admin", "password"),
            ),
        ),
        (
            "alg none",
            format!(
                "{}.",
                token(&claims("user", "admin", "password"))
                    .rsplit_once('.')
                    .unwrap()
                    .0
            ),
        ),
        ("not a JWT", "garbage".into()),
        (
            "extra segment",
            format!("{}.x", token(&claims("user", "admin", "password"))),
        ),
    ];
    let mut variant = |name: &'static str, edit: fn(&mut serde_json::Value)| {
        let mut body = claims("user", "admin", "password");
        edit(&mut body);
        cases.push((name, token(&body)));
    };
    variant("wrong issuer", |c| c["iss"] = json!("https://evil.example"));
    variant("wrong audience", |c| c["aud"] = json!("someone-else"));
    variant("expired", |c| c["exp"] = json!(NOW - 31));
    variant("issued in the future", |c| c["iat"] = json!(NOW + 120));
    variant("not yet valid", |c| c["nbf"] = json!(NOW + 120));
    variant("lives too long", |c| c["exp"] = json!(NOW + 3_600));
    variant("another organization", |c| {
        c["norn"]["org"] = json!("org_b");
    });
    variant("undeclared role", |c| {
        c["norn"]["role"] = json!("superuser");
    });
    variant("unknown kind", |c| c["norn"]["kind"] = json!("system"));
    variant("blank subject", |c| c["sub"] = json!(" "));
    variant("extra Norn claim", |c| {
        c["norn"]["roles"] = json!(["admin"]);
    });
    variant("no Norn claims (a plain session JWT)", |c| {
        c.as_object_mut().unwrap().remove("norn");
    });
    let expected = resolve("garbage").unwrap_err();
    assert_eq!(expected.code, codes::AUTHZ_DENIED);
    for (name, case) in cases {
        let refusal = resolve(&case).expect_err(name);
        assert_eq!(
            (refusal.status, refusal.code.as_str()),
            (expected.status, expected.code.as_str()),
            "{name}"
        );
    }
}

#[test]
fn a_jwks_without_ed25519_keys_is_refused() {
    let rsa =
        json!({ "keys": [{ "kty": "RSA", "kid": "r1", "n": "AQAB", "e": "AQAB" }] }).to_string();
    assert!(TokenVerifier::from_jwks(ISSUER, "norn-host", &rsa, &STUDIO_ROLES).is_err());
    assert!(TokenVerifier::from_jwks(ISSUER, "norn-host", "{}", &STUDIO_ROLES).is_err());
}
