//! Local session authentication and fail-closed authority/scope header
//! validation for `norn dev`.

use std::collections::BTreeMap;

use norn_api::local_cell::LocalActor;
use norn_api::refusal::Refusal;
use subtle::ConstantTimeEq as _;

use super::http_wire::{HttpResponse, refusal_response, synthetic_refusal};

pub(super) const DEV_TOKEN_ENV: &str = "NORN_DEV_TOKEN";

const SINGLE_VALUED_HEADERS: [&str; 16] = [
    "authorization",
    "x-norn-dev-token",
    "x-norn-org",
    "x-norn-org-id",
    "x-norn-env",
    "x-norn-environment-id",
    "x-norn-tenant",
    "x-norn-tenant-id",
    "x-norn-actor",
    "x-norn-actor-id",
    "x-norn-user",
    "x-norn-user-id",
    "x-norn-roles",
    "x-norn-role",
    "content-length",
    "transfer-encoding",
];

const ACTOR_HEADERS: [&str; 6] = [
    "x-norn-actor",
    "x-norn-actor-id",
    "x-norn-user",
    "x-norn-user-id",
    "x-norn-roles",
    "x-norn-role",
];

/// Local session authentication secret required on every `norn dev` HTTP request.
#[derive(Debug, Clone)]
pub(super) struct DevAuth {
    session_token: String,
    from_env: bool,
}

impl DevAuth {
    /// Load `NORN_DEV_TOKEN` if set (rejecting empty/whitespace/control values),
    /// else generate a fresh 32-byte cryptographically random hex session token.
    pub(super) fn from_env_or_generate() -> Result<Self, String> {
        match std::env::var(DEV_TOKEN_ENV) {
            Ok(raw) => {
                if raw.is_empty()
                    || raw
                        .bytes()
                        .any(|b| b.is_ascii_whitespace() || b.is_ascii_control())
                {
                    return Err(format!(
                        "{DEV_TOKEN_ENV} must be a non-empty token without whitespace or control characters"
                    ));
                }
                Ok(Self {
                    session_token: raw,
                    from_env: true,
                })
            }
            Err(std::env::VarError::NotPresent) => Ok(Self {
                session_token: generate_random_hex_token()?,
                from_env: false,
            }),
            Err(std::env::VarError::NotUnicode(_)) => {
                Err(format!("{DEV_TOKEN_ENV} must be valid UTF-8"))
            }
        }
    }

    #[cfg(test)]
    pub(super) fn with_token(token: impl Into<String>) -> Self {
        Self {
            session_token: token.into(),
            from_env: false,
        }
    }

    pub(super) fn token(&self) -> &str {
        &self.session_token
    }

    pub(super) const fn is_from_env(&self) -> bool {
        self.from_env
    }

    pub(super) fn verify(&self, candidate: &str) -> bool {
        constant_time_eq(self.session_token.as_bytes(), candidate.as_bytes())
    }
}

#[cfg(unix)]
fn generate_random_hex_token() -> Result<String, String> {
    use std::io::Read as _;
    let mut bytes = [0_u8; 32];
    let mut urandom = std::fs::File::open("/dev/urandom")
        .map_err(|err| format!("failed to open /dev/urandom for session token: {err}"))?;
    urandom
        .read_exact(&mut bytes)
        .map_err(|err| format!("failed to read random bytes for session token: {err}"))?;
    Ok(encode_hex(&bytes))
}

#[cfg(not(unix))]
fn generate_random_hex_token() -> Result<String, String> {
    Err(format!(
        "set {DEV_TOKEN_ENV} on non-Unix hosts to authenticate local norn dev requests"
    ))
}

#[cfg(unix)]
fn encode_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len().saturating_mul(2));
    for &byte in bytes {
        let hi = usize::from(byte >> 4);
        let lo = usize::from(byte & 0x0f);
        if let (Some(&h), Some(&l)) = (HEX.get(hi), HEX.get(lo)) {
            out.push(char::from(h));
            out.push(char::from(l));
        }
    }
    out
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    bool::from(left.ct_eq(right))
}

pub(super) fn single_header<'a>(
    headers: &'a BTreeMap<String, Vec<String>>,
    name: &str,
) -> Option<&'a str> {
    headers
        .get(name)
        .and_then(|vals| vals.first())
        .map(String::as_str)
}

fn check_alias_group(
    headers: &BTreeMap<String, Vec<String>>,
    group: &[&str],
) -> Result<(), HttpResponse> {
    let mut first: Option<(&str, &str)> = None;
    for &name in group {
        if let Some(val) = single_header(headers, name) {
            match first {
                None => first = Some((name, val)),
                Some((prev_name, prev_val)) if prev_val != val => {
                    return Err(refusal_response(&Refusal::invalid(format!(
                        "conflicting `{prev_name}` and `{name}` headers"
                    ))));
                }
                Some(_) => {}
            }
        }
    }
    Ok(())
}

/// Reject duplicate authority/scope/framing headers and conflicting aliases fail-closed (`400`).
pub(super) fn validate_request_headers(
    headers: &BTreeMap<String, Vec<String>>,
) -> Result<(), HttpResponse> {
    for name in SINGLE_VALUED_HEADERS {
        if headers.get(name).is_some_and(|vals| vals.len() > 1) {
            return Err(refusal_response(&Refusal::invalid(format!(
                "duplicate `{name}` header is not allowed"
            ))));
        }
    }
    check_alias_group(headers, &["x-norn-org-id", "x-norn-org"])?;
    check_alias_group(headers, &["x-norn-environment-id", "x-norn-env"])?;
    check_alias_group(headers, &["x-norn-tenant-id", "x-norn-tenant"])?;
    check_alias_group(
        headers,
        &[
            "x-norn-actor",
            "x-norn-actor-id",
            "x-norn-user",
            "x-norn-user-id",
        ],
    )?;
    check_alias_group(headers, &["x-norn-roles", "x-norn-role"])?;

    let has_auth = headers.contains_key("authorization");
    if has_auth && headers.contains_key("x-norn-dev-token") {
        return Err(refusal_response(&Refusal::invalid(
            "conflicting authority headers: send either Authorization or X-Norn-Dev-Token, not both",
        )));
    }
    if has_auth && ACTOR_HEADERS.iter().any(|h| headers.contains_key(*h)) {
        return Err(refusal_response(&Refusal::invalid(
            "conflicting authority headers: do not combine Authorization with X-Norn-Actor/X-Norn-Roles; use X-Norn-Dev-Token instead",
        )));
    }
    Ok(())
}

/// Require a valid local session token (`X-Norn-Dev-Token` or `Authorization: Bearer <token>`).
pub(super) fn authenticate_dev_session(
    headers: &BTreeMap<String, Vec<String>>,
    auth: &DevAuth,
) -> Result<(), HttpResponse> {
    let unauthorized = || {
        synthetic_refusal(
            401,
            "authz.dev_token_required",
            "permission",
            "local dev session token required",
            "supply X-Norn-Dev-Token or Authorization: Bearer <NORN_DEV_TOKEN>",
        )
    };
    let supplied = if let Some(tok) = single_header(headers, "x-norn-dev-token") {
        tok
    } else if let Some(auth_header) = single_header(headers, "authorization") {
        let Some(bearer) = auth_header.strip_prefix("Bearer ") else {
            return Err(unauthorized());
        };
        bearer.trim()
    } else {
        return Err(unauthorized());
    };
    if !auth.verify(supplied) {
        return Err(unauthorized());
    }
    Ok(())
}

/// Extract simulated dev actor + roles after session token authentication; fails closed (`None`) if absent or incomplete.
pub(super) fn extract_actor(headers: &BTreeMap<String, Vec<String>>) -> Option<LocalActor> {
    let actor_id = single_header(headers, "x-norn-actor")?;
    let roles_raw = single_header(headers, "x-norn-roles")?;
    let trimmed = actor_id.trim();
    let roles = parse_role_list(roles_raw);
    if trimmed.is_empty() || roles.is_empty() {
        return None;
    }
    Some(LocalActor::new(trimmed, roles))
}

fn parse_role_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|role| !role.is_empty())
        .map(str::to_owned)
        .collect()
}
