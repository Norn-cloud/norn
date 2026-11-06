//! HOST-PRINCIPAL (#817): the trusted transport's identity seam.
//!
//! The Studio Worker signs, per request, a short-lived `EdDSA` JWT for the
//! session or API key it has verified. The host checks it against the
//! issuer's published JWKS (signature, `iss`, `aud`, `exp`, `nbf`) and maps
//! it to a Norn principal. The token says who the subject is, which
//! organization the request is scoped to and the subject's membership role
//! there; Norn roles come only from the declared, versioned [`RoleTable`], so an
//! undeclared role grants nothing. A service (API key) can never hold an
//! approving role. Every failure is the same indistinguishable denial, before
//! any data access; authorization itself stays with the kernel PDP.

use std::collections::BTreeMap;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signature, VerifyingKey};
use http::StatusCode;
use norn_kernel::authz::ActorKind;
use serde::Deserialize;

use crate::{
    refusal::{Refusal, codes},
    scope::CellScope,
};

/// Seconds of clock skew tolerated on `exp`, `nbf` and `iat`.
pub const LEEWAY_SECONDS: u64 = 30;
/// Longest token lifetime the host admits, whatever the issuer says.
pub const MAX_LIFETIME_SECONDS: u64 = 300;

/// Declared mapping from an organization membership role to Norn roles.
pub struct RoleTable {
    /// Identity of this table; a change of mapping is a new version.
    pub version: &'static str,
    /// Membership role, Norn roles it confers, and whether a service may hold it.
    pub entries: &'static [(&'static str, &'static [&'static str], bool)],
}

/// The hosted Studio's roles (`studio.workspace.access`). API keys are viewers or members only.
pub const STUDIO_ROLES: RoleTable = RoleTable {
    version: "studio-roles/1",
    entries: &[
        ("owner", &["owner"], false),
        ("admin", &["admin"], false),
        ("member", &["member"], true),
        ("viewer", &["viewer"], true),
    ],
};

/// A verified Norn principal for one request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    /// `user:<id>` or `service:<api key id>`.
    pub subject_id: String,
    /// User or service.
    pub actor_kind: ActorKind,
    /// The organization the request is scoped to; always the cell's.
    pub org_id: String,
    /// Norn roles from the declared table.
    pub roles: Vec<String>,
    /// `password`, `magic_link` or `api_key`.
    pub auth_method: String,
    /// The table version the roles came from.
    pub role_table: &'static str,
}

/// Where tokens come from and which keys may sign them.
pub struct TokenVerifier {
    issuer: String,
    audience: String,
    keys: BTreeMap<String, VerifyingKey>,
    roles: &'static RoleTable,
}

#[derive(Deserialize)]
struct Header {
    alg: String,
    kid: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Audience {
    One(String),
    Many(Vec<String>),
}

#[derive(Deserialize)]
struct Claims {
    iss: String,
    aud: Audience,
    sub: String,
    exp: u64,
    iat: u64,
    nbf: Option<u64>,
    norn: NornClaims,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NornClaims {
    kind: String,
    org: String,
    role: String,
    amr: String,
}

#[derive(Deserialize)]
struct Jwks {
    keys: Vec<Jwk>,
}

#[derive(Deserialize)]
struct Jwk {
    kty: String,
    crv: Option<String>,
    x: Option<String>,
    kid: Option<String>,
}

fn denied() -> Refusal {
    Refusal::new(
        StatusCode::FORBIDDEN,
        codes::AUTHZ_DENIED,
        "the request is not authorized",
        "the request carries no valid identity for this cell",
    )
}

impl TokenVerifier {
    /// Admit the issuer's JWKS document; only Ed25519 keys with a `kid` are kept.
    ///
    /// # Errors
    /// A document that is not a JWKS, or one with no usable key, refuses.
    pub fn from_jwks(
        issuer: &str,
        audience: &str,
        jwks_json: &str,
        roles: &'static RoleTable,
    ) -> Result<Self, Refusal> {
        let jwks: Jwks = serde_json::from_str(jwks_json).map_err(Refusal::host)?;
        let keys: BTreeMap<_, _> = jwks
            .keys
            .into_iter()
            .filter(|key| key.kty == "OKP" && key.crv.as_deref() == Some("Ed25519"))
            .filter_map(|key| {
                let bytes: [u8; 32] = URL_SAFE_NO_PAD.decode(key.x?).ok()?.try_into().ok()?;
                Some((key.kid?, VerifyingKey::from_bytes(&bytes).ok()?))
            })
            .collect();
        if keys.is_empty() {
            return Err(Refusal::host("the issuer publishes no Ed25519 signing key"));
        }
        Ok(Self {
            issuer: issuer.into(),
            audience: audience.into(),
            keys,
            roles,
        })
    }

    /// Verify one bearer token for this cell at `now` (Unix seconds).
    ///
    /// # Errors
    /// Any malformed, forged, expired, foreign-scope or undeclared-role token
    /// returns the same `authz.denied` refusal.
    pub fn principal(&self, token: &str, cell: &CellScope, now: u64) -> Result<Principal, Refusal> {
        self.verify(token, cell, now).ok_or_else(denied)
    }

    fn verify(&self, token: &str, cell: &CellScope, now: u64) -> Option<Principal> {
        let (signed, signature) = token.rsplit_once('.')?;
        let (header, payload) = signed.split_once('.')?;
        if payload.contains('.') {
            return None;
        }
        let head: Header = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(header).ok()?).ok()?;
        if head.alg != "EdDSA" {
            return None;
        }
        let key = self.keys.get(&head.kid)?;
        let signature = Signature::from_slice(&URL_SAFE_NO_PAD.decode(signature).ok()?).ok()?;
        key.verify_strict(signed.as_bytes(), &signature).ok()?;
        let claims: Claims = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).ok()?).ok()?;
        let audience_ok = match &claims.aud {
            Audience::One(aud) => *aud == self.audience,
            Audience::Many(auds) => auds.contains(&self.audience),
        };
        let late = now.saturating_add(LEEWAY_SECONDS);
        let fresh = claims.exp.saturating_add(LEEWAY_SECONDS) > now
            && claims.iat <= late
            && claims.nbf.is_none_or(|nbf| nbf <= late)
            && claims.exp.saturating_sub(claims.iat) <= MAX_LIFETIME_SECONDS;
        if claims.iss != self.issuer || !audience_ok || !fresh || claims.sub.trim().is_empty() {
            return None;
        }
        let norn = claims.norn;
        if norn.org != cell.org_id {
            return None;
        }
        let (actor_kind, prefix) = match norn.kind.as_str() {
            "user" => (ActorKind::User, "user"),
            "service" => (ActorKind::Service, "service"),
            _ => return None,
        };
        let methods: &[&str] = if actor_kind == ActorKind::Service {
            &["api_key"]
        } else {
            &["password", "magic_link"]
        };
        if !methods.contains(&norn.amr.as_str()) {
            return None;
        }
        let (_, roles, service_may_hold) = self
            .roles
            .entries
            .iter()
            .find(|(role, _, _)| *role == norn.role)?;
        if actor_kind == ActorKind::Service && !service_may_hold {
            return None;
        }
        Some(Principal {
            subject_id: format!("{prefix}:{}", claims.sub),
            actor_kind,
            org_id: norn.org,
            roles: roles.iter().map(|role| (*role).to_owned()).collect(),
            auth_method: norn.amr,
            role_table: self.roles.version,
        })
    }
}

#[cfg(test)]
#[path = "principal_tests.rs"]
mod tests;
