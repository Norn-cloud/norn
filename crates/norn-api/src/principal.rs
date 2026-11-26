//! HOST-PRINCIPAL (#817): the trusted transport's identity seam.
//!
//! The Studio Worker signs, per request, a short-lived `EdDSA` JWT for the
//! session or API key it has verified. The host checks it against the
//! issuer's published JWKS (signature, `iss`, `aud`, `exp`, `nbf`) and maps
//! it to a Norn principal. The token says who the subject is, which
//! organization the request is scoped to and the subject's membership role
//! there; Norn roles come only from the declared, versioned [`RoleTable`](crate::principal::RoleTable), so an
//! undeclared role grants nothing. A service (API key) can never hold an
//! approving role. Every failure is the same indistinguishable denial, before
//! any data access; authorization itself stays with the kernel PDP.

use std::collections::BTreeMap;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ed25519_dalek::{Signature, VerifyingKey};
use http::StatusCode;
use norn_kernel::authz::{ActorKind, AttrValue};
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
    /// `password`, `magic_link`, `oauth` or `api_key`.
    pub auth_method: String,
    /// The table version the roles came from.
    pub role_table: &'static str,
    /// AUTH-INDEPENDENT-PERSON (#618): the accountable person the issuer
    /// names for a user (`norn.accountablePerson`); `None` when it names none.
    pub accountable_person: Option<String>,
    /// Verified scoped principal attributes signed by the issuer
    /// (`norn.attributes`), never read from request inputs.
    pub attributes: BTreeMap<String, AttrValue>,
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
    #[serde(default, rename = "accountablePerson")]
    accountable_person: Option<String>,
    #[serde(default, deserialize_with = "deserialize_principal_attributes")]
    attributes: BTreeMap<String, AttrValue>,
}

const MAX_PRINCIPAL_ATTRIBUTES: usize = 64;
const MAX_PRINCIPAL_SET_ITEMS: usize = 256;

fn valid_attribute_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    name != "id"
        && (first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

enum ScalarAttr {
    Bool(bool),
    Long(i64),
    String(String),
}

impl ScalarAttr {
    const fn kind(&self) -> u8 {
        match self {
            Self::Bool(_) => 0,
            Self::Long(_) => 1,
            Self::String(_) => 2,
        }
    }

    fn into_attr(self) -> AttrValue {
        match self {
            Self::Bool(value) => AttrValue::Bool(value),
            Self::Long(value) => AttrValue::Long(value),
            Self::String(value) => AttrValue::String(value),
        }
    }
}

impl<'de> Deserialize<'de> for ScalarAttr {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ScalarVisitor;

        impl serde::de::Visitor<'_> for ScalarVisitor {
            type Value = ScalarAttr;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a boolean, signed 64-bit integer, or string")
            }

            fn visit_bool<E: serde::de::Error>(self, value: bool) -> Result<Self::Value, E> {
                Ok(ScalarAttr::Bool(value))
            }

            fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<Self::Value, E> {
                Ok(ScalarAttr::Long(value))
            }

            fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Self::Value, E> {
                let signed = i64::try_from(value)
                    .map_err(|_| E::custom("principal attribute integer out of range"))?;
                Ok(ScalarAttr::Long(signed))
            }

            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(ScalarAttr::String(value.to_owned()))
            }

            fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Self::Value, E> {
                Ok(ScalarAttr::String(value))
            }
        }

        deserializer.deserialize_any(ScalarVisitor)
    }
}

struct PrincipalAttr(AttrValue);

impl<'de> Deserialize<'de> for PrincipalAttr {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ValueVisitor;

        impl<'de> serde::de::Visitor<'de> for ValueVisitor {
            type Value = PrincipalAttr;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("a boolean, integer, string, or homogeneous array of scalars")
            }

            fn visit_bool<E: serde::de::Error>(self, value: bool) -> Result<Self::Value, E> {
                Ok(PrincipalAttr(AttrValue::Bool(value)))
            }

            fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<Self::Value, E> {
                Ok(PrincipalAttr(AttrValue::Long(value)))
            }

            fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<Self::Value, E> {
                let signed = i64::try_from(value)
                    .map_err(|_| E::custom("principal attribute integer out of range"))?;
                Ok(PrincipalAttr(AttrValue::Long(signed)))
            }

            fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                Ok(PrincipalAttr(AttrValue::String(value.to_owned())))
            }

            fn visit_string<E: serde::de::Error>(self, value: String) -> Result<Self::Value, E> {
                Ok(PrincipalAttr(AttrValue::String(value)))
            }

            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut seq: A,
            ) -> Result<Self::Value, A::Error> {
                let mut items = Vec::new();
                let mut expected_kind: Option<u8> = None;
                while let Some(elem) = seq.next_element::<ScalarAttr>()? {
                    if items.len() >= MAX_PRINCIPAL_SET_ITEMS {
                        return Err(serde::de::Error::custom(
                            "too many principal attribute set items",
                        ));
                    }
                    let kind = elem.kind();
                    if let Some(expected) = expected_kind {
                        if expected != kind {
                            return Err(serde::de::Error::custom(
                                "mixed types in principal attribute set",
                            ));
                        }
                    } else {
                        expected_kind = Some(kind);
                    }
                    items.push(elem.into_attr());
                }
                Ok(PrincipalAttr(AttrValue::Set(items)))
            }
        }

        deserializer.deserialize_any(ValueVisitor)
    }
}

/// Deserialize `norn.attributes` rejecting duplicate keys, oversized maps,
/// invalid names (including `"id"`), and unsupported values before insertion.
fn deserialize_principal_attributes<'de, D>(
    deserializer: D,
) -> Result<BTreeMap<String, AttrValue>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct AttrMapVisitor;

    impl<'de> serde::de::Visitor<'de> for AttrMapVisitor {
        type Value = BTreeMap<String, AttrValue>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a map of typed principal attributes without duplicate keys")
        }

        fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
        where
            A: serde::de::MapAccess<'de>,
        {
            let mut decoded = BTreeMap::new();
            while let Some(name) = map.next_key::<String>()? {
                if decoded.len() >= MAX_PRINCIPAL_ATTRIBUTES {
                    return Err(serde::de::Error::custom("too many principal attributes"));
                }
                if !valid_attribute_name(&name) {
                    return Err(serde::de::Error::custom("invalid principal attribute name"));
                }
                if decoded.contains_key(&name) {
                    return Err(serde::de::Error::custom("duplicate principal attribute"));
                }
                let attr = map.next_value::<PrincipalAttr>()?.0;
                decoded.insert(name, attr);
            }
            Ok(decoded)
        }
    }

    deserializer.deserialize_map(AttrMapVisitor)
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
        // `oauth`: a person's own grant to an assistant they connected (MCP
        // authorization); the token still names that person and their role.
        let methods: &[&str] = if actor_kind == ActorKind::Service {
            &["api_key"]
        } else {
            &["password", "magic_link", "oauth"]
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
        let subject_id = format!("{prefix}:{}", claims.sub);
        // AUTH-INDEPENDENT-PERSON: only the issuer's explicit claim names the
        // person behind a user; the subject (a session, an assistant's grant)
        // never stands in for one, and a service key names none.
        let accountable_person = match norn.accountable_person {
            Some(_) if actor_kind == ActorKind::Service => return None,
            Some(person) => {
                Some(norn_kernel::validate_accountable_person(Some(&person))?.to_owned())
            }
            None => None,
        };
        Some(Principal {
            subject_id,
            actor_kind,
            org_id: norn.org,
            roles: roles.iter().map(|role| (*role).to_owned()).collect(),
            auth_method: norn.amr,
            role_table: self.roles.version,
            accountable_person,
            attributes: norn.attributes,
        })
    }
}

#[cfg(test)]
#[path = "principal_tests.rs"]
mod tests;
