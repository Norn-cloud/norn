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
            accountable_person: None,
            attributes: std::collections::BTreeMap::new(),
        }
    );
    let link = resolve(&token(&claims("user", "admin", "magic_link"))).unwrap();
    assert_eq!(link.roles, ["admin"]);
}

#[test]
fn signed_principal_attributes_decode_typed_scalars_and_sets_and_reject_spoofing() {
    use norn_kernel::authz::AttrValue;
    let mut valid = claims("user", "member", "password");
    valid["norn"]["attributes"] = json!({
        "employee_id": "E-42",
        "managed_branches": ["cairo", "alex"],
        "clearance": 3,
        "verified": true
    });
    let principal = resolve(&token(&valid)).unwrap();
    assert_eq!(
        principal.attributes.get("employee_id"),
        Some(&AttrValue::String("E-42".into()))
    );
    assert_eq!(
        principal.attributes.get("managed_branches"),
        Some(&AttrValue::Set(vec![
            AttrValue::String("cairo".into()),
            AttrValue::String("alex".into()),
        ]))
    );
    assert_eq!(
        principal.attributes.get("clearance"),
        Some(&AttrValue::Long(3))
    );
    assert_eq!(
        principal.attributes.get("verified"),
        Some(&AttrValue::Bool(true))
    );

    for bad in [
        json!({ "id": "user:spoofed" }),
        json!({ "ratio": 1.5 }),
        json!({ "nested": { "a": 1 } }),
        json!({ "raw_trick": { "$serde_json::private::RawValue": "1" } }),
        json!({ "mixed": ["a", 1] }),
        json!({ "bad-name": "x" }),
    ] {
        let mut malformed = claims("user", "member", "password");
        malformed["norn"]["attributes"] = bad;
        assert!(resolve(&token(&malformed)).is_err());
    }

    // Duplicate attribute keys in raw JSON must fail closed even when both values are valid.
    let head = URL_SAFE_NO_PAD.encode(r#"{"alg":"EdDSA","kid":"k1"}"#);
    let raw_payload = format!(
        r#"{{"iss":"{ISSUER}","aud":"norn-host","sub":"u1","iat":{},"exp":{},"norn":{{"kind":"user","org":"org_a","role":"member","amr":"password","attributes":{{"team":"a","team":"b"}}}}}}"#,
        NOW - 10,
        NOW + 290
    );
    let body = URL_SAFE_NO_PAD.encode(raw_payload.as_bytes());
    let sig = key(1).sign(format!("{head}.{body}").as_bytes());
    let dup_token = format!("{head}.{body}.{}", URL_SAFE_NO_PAD.encode(sig.to_bytes()));
    assert!(resolve(&dup_token).is_err());

    // Attribute count boundaries: 64 accepted, 65 rejected.
    let attrs_of = |count: usize| -> serde_json::Value {
        let map: serde_json::Map<String, serde_json::Value> = (0..count)
            .map(|i| (format!("k_{i}"), json!(i64::try_from(i).unwrap())))
            .collect();
        serde_json::Value::Object(map)
    };
    let mut at_attr_limit = claims("user", "member", "password");
    at_attr_limit["norn"]["attributes"] = attrs_of(64);
    assert_eq!(
        resolve(&token(&at_attr_limit)).unwrap().attributes.len(),
        64
    );
    let mut over_attr_limit = claims("user", "member", "password");
    over_attr_limit["norn"]["attributes"] = attrs_of(65);
    assert!(resolve(&token(&over_attr_limit)).is_err());

    // Set item count boundaries: 256 accepted, 257 rejected.
    let items_of = |count: usize| -> serde_json::Value {
        serde_json::Value::Array((0..count).map(|i| json!(format!("v{i}"))).collect())
    };
    let mut at_set_limit = claims("user", "member", "password");
    at_set_limit["norn"]["attributes"] = json!({ "tags": items_of(256) });
    assert!(matches!(
        resolve(&token(&at_set_limit)).unwrap().attributes.get("tags"),
        Some(AttrValue::Set(items)) if items.len() == 256
    ));
    let mut over_set_limit = claims("user", "member", "password");
    over_set_limit["norn"]["attributes"] = json!({ "tags": items_of(257) });
    assert!(resolve(&token(&over_set_limit)).is_err());
}

#[test]
fn an_assistant_grant_acts_as_its_person_and_never_as_a_service() {
    let granted = resolve(&token(&claims("user", "member", "oauth"))).unwrap();
    assert_eq!(
        (
            granted.subject_id.as_str(),
            granted.actor_kind,
            granted.roles.as_slice(),
            granted.auth_method.as_str()
        ),
        (
            "user:u1",
            ActorKind::User,
            ["member".to_owned()].as_slice(),
            "oauth"
        )
    );
    assert!(resolve(&token(&claims("service", "viewer", "oauth"))).is_err());
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

/// AUTH-INDEPENDENT-PERSON (#618): only the issuer's explicit claim names the
/// person behind a token. Two sessions naming one person resolve to that one
/// person; a user token without the claim names none (its session subject never
/// stands in); a service key naming a person, or a malformed claim, is refused.
#[test]
fn only_the_issuers_claim_names_the_accountable_person() {
    let session = |sub: &str, person: Option<&str>| {
        let mut claims = claims("user", "admin", "password");
        claims["sub"] = json!(sub);
        if let Some(person) = person {
            claims["norn"]["accountablePerson"] = json!(person);
        }
        resolve(&token(&claims)).map(|principal| principal.accountable_person)
    };
    assert_eq!(session("sess-a", Some("alice")), Ok(Some("alice".into())));
    assert_eq!(session("sess-b", Some("alice")), Ok(Some("alice".into())));
    assert_eq!(session("sess-c", None), Ok(None));
    for malformed in ["", "   ", " alice", "e\u{301}"] {
        assert!(session("sess-d", Some(malformed)).is_err(), "{malformed:?}");
    }
    let mut service = claims("service", "viewer", "api_key");
    service["norn"]["accountablePerson"] = json!("alice");
    assert!(resolve(&token(&service)).is_err());
}
