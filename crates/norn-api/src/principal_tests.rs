#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::arithmetic_side_effects,
    clippy::indexing_slicing,
    clippy::panic,
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

const CHALLENGE: AuthorityChallenge<'static> = AuthorityChallenge {
    nonce: "n-1",
    binding: "approval:dec_1:cmd_1",
};

/// An attestation as Studio's authority signs it: one minute, answering
/// [`CHALLENGE`].
fn attestation(kind: &str, member: bool, role: Option<&str>) -> serde_json::Value {
    let mut norn = json!({ "kind": kind, "org": "org_a", "amr": MEMBERSHIP_METHOD,
        "member": member, "nonce": CHALLENGE.nonce, "bind": CHALLENGE.binding });
    if let Some(role) = role {
        norn["role"] = json!(role);
    }
    json!({ "iss": ISSUER, "aud": "norn-authority", "sub": "u1", "iat": NOW - 5, "exp": NOW + 55,
            "norn": norn })
}

fn attests() -> TokenVerifier {
    TokenVerifier::from_jwks(ISSUER, "norn-authority", &jwks(), &STUDIO_ROLES).unwrap()
}

fn attest(claims: &serde_json::Value, now: u64) -> Result<Membership, Refusal> {
    attests().membership(&token(claims), &cell(), now, &CHALLENGE)
}

/// APPROVAL-LIVE-1: a membership attestation verifies only as one, under its
/// own audience, and never passes as a host token; a host token never
/// passes as an attestation.
#[test]
fn a_membership_attestation_is_never_a_host_token() {
    let member = attestation("user", true, Some("admin"));
    let Membership::Member(principal) = attest(&member, NOW).unwrap() else {
        panic!("not a member");
    };
    assert_eq!(
        (principal.subject_id.as_str(), principal.roles.as_slice()),
        ("user:u1", ["admin".to_owned()].as_slice())
    );
    let Membership::Member(service) =
        attest(&attestation("service", true, Some("member")), NOW).unwrap()
    else {
        panic!("not a member");
    };
    assert_eq!(service.subject_id, "service:u1");
    // An attestation is no host token, under either audience.
    assert!(attests().principal(&token(&member), &cell(), NOW).is_err());
    let mut host_audience = member.clone();
    host_audience["aud"] = json!("norn-host");
    assert!(resolve(&token(&host_audience)).is_err());
    // A host token is no attestation.
    let session = claims("user", "admin", "password");
    assert!(attest(&session, NOW).is_err());
    assert!(
        verifier()
            .membership(&token(&session), &cell(), NOW, &CHALLENGE)
            .is_err()
    );
    // Another organization's or a forged attestation denies.
    let mut foreign = member.clone();
    foreign["norn"]["org"] = json!("org_b");
    assert!(attest(&foreign, NOW).is_err());
    let forged = sign(&key(2), &json!({ "alg": "EdDSA", "kid": "k1" }), &member);
    assert!(
        attests()
            .membership(&forged, &cell(), NOW, &CHALLENGE)
            .is_err()
    );
}

/// APPROVAL-LIVE-1 (R1-04): "no member" is a signed statement bound to the
/// same lookup as a member's answer; it carries no role.
#[test]
fn a_negative_attestation_is_signed_and_names_no_role() {
    assert_eq!(
        attest(&attestation("user", false, None), NOW).unwrap(),
        Membership::NotMember {
            subject_id: "user:u1".into(),
            actor_kind: ActorKind::User,
        }
    );
    assert!(attest(&attestation("user", false, Some("admin")), NOW).is_err());
    assert!(attest(&attestation("user", true, None), NOW).is_err());
    let forged = sign(
        &key(2),
        &json!({ "alg": "EdDSA", "kid": "k1" }),
        &attestation("user", false, None),
    );
    assert!(
        attests()
            .membership(&forged, &cell(), NOW, &CHALLENGE)
            .is_err()
    );
}

/// APPROVAL-LIVE-1 (R1-05): an attestation answers one challenge and one
/// attempt, and lives at most a minute with a few seconds of skew: never the
/// host token's five minutes.
#[test]
fn an_attestation_answers_only_its_challenge_within_a_minute() {
    for member in [true, false] {
        let role = member.then_some("admin");
        let answer = attestation("user", member, role);
        assert!(attest(&answer, NOW).is_ok());
        let other_nonce = AuthorityChallenge {
            nonce: "n-2",
            binding: CHALLENGE.binding,
        };
        let other_call = AuthorityChallenge {
            nonce: CHALLENGE.nonce,
            binding: "approval:dec_1:cmd_2",
        };
        for challenge in [other_nonce, other_call] {
            assert!(
                attests()
                    .membership(&token(&answer), &cell(), NOW, &challenge)
                    .is_err()
            );
        }
        let mut unbound = answer.clone();
        unbound["norn"]["nonce"] = json!("");
        let empty = AuthorityChallenge {
            nonce: "",
            binding: CHALLENGE.binding,
        };
        assert!(
            attests()
                .membership(&token(&unbound), &cell(), NOW, &empty)
                .is_err()
        );
        // A host token's lifetime is no attestation's.
        let mut long = answer.clone();
        long["exp"] = json!(NOW + 295);
        assert!(attest(&long, NOW).is_err());
        // Expired past the small leeway.
        assert!(attest(&answer, NOW + 55 + ATTESTATION_LEEWAY_SECONDS).is_err());
        assert!(attest(&answer, NOW + 55 + ATTESTATION_LEEWAY_SECONDS - 1).is_ok());
        // Issued in the future past the leeway.
        assert!(attest(&answer, NOW - 5 - ATTESTATION_LEEWAY_SECONDS - 1).is_err());
    }
}
