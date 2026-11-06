//! Principal resolution: the single seam from a transport credential to a
//! verified Norn principal.
//!
//! [`TokenPrincipals`] verifies the Studio Worker's signed host token (#817,
//! HOST-PRINCIPAL) with `norn_api::principal::TokenVerifier`: Ed25519 against
//! the issuer's JWKS, the pinned issuer and audience, a lifetime of at most five
//! minutes, the token's organization equal to this cell's, and the membership
//! role mapped through the declared `studio-roles/1` table. The issuer and the
//! JWKS location are host configuration; nothing in a token chooses them
//! (`iss` must equal the pinned issuer; `jku`/`x5u` are never read).
//!
//! A resolver records the verified principal into the policy's
//! [`CallerSlot`], where the declared grant table turns its roles into
//! grants. [`FixturePrincipals`] is for tests and local smoke builds only.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use norn_api::principal::{STUDIO_ROLES, TokenVerifier};
use norn_api::refusal::Refusal;
use norn_api::scope::CellScope;

use crate::policy::{Caller, CallerSlot};

/// The audience every host token must name.
pub const HOST_AUDIENCE: &str = "norn-host";

/// Resolves the request's `Authorization` header to a trusted principal at
/// `now` (Unix seconds), recording it for the policy.
pub trait PrincipalResolver {
    /// The verified subject, or `None`; every read then denies by default.
    fn resolve(&self, authorization: Option<&str>, now: u64) -> Option<String>;
}

/// Verifies signed host tokens for one cell.
pub struct TokenPrincipals {
    issuer: String,
    cell: CellScope,
    slot: Arc<CallerSlot>,
    keys: Mutex<(TokenVerifier, BTreeSet<String>)>,
    unknown_kid: Mutex<bool>,
}

/// The key ids a JWKS document publishes.
fn kids(jwks: &str) -> BTreeSet<String> {
    serde_json::from_str::<serde_json::Value>(jwks)
        .ok()
        .and_then(|document| {
            document
                .get("keys")
                .and_then(serde_json::Value::as_array)
                .cloned()
        })
        .unwrap_or_default()
        .iter()
        .filter_map(|key| {
            key.get("kid")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .collect()
}

/// The `kid` a token's header names, without verifying anything.
fn token_kid(token: &str) -> Option<String> {
    let header = URL_SAFE_NO_PAD.decode(token.split('.').next()?).ok()?;
    let header: serde_json::Value = serde_json::from_slice(&header).ok()?;
    header.get("kid")?.as_str().map(str::to_owned)
}

impl TokenPrincipals {
    /// Admit the pinned issuer's JWKS for `cell`.
    ///
    /// # Errors
    /// The document is not a JWKS or publishes no Ed25519 key.
    pub fn new(
        issuer: &str,
        jwks: &str,
        cell: CellScope,
        slot: Arc<CallerSlot>,
    ) -> Result<Self, Refusal> {
        let verifier = TokenVerifier::from_jwks(issuer, HOST_AUDIENCE, jwks, &STUDIO_ROLES)?;
        Ok(Self {
            issuer: issuer.to_owned(),
            cell,
            slot,
            keys: Mutex::new((verifier, kids(jwks))),
            unknown_kid: Mutex::new(false),
        })
    }

    /// Replace the keys after a refetch of the pinned JWKS location.
    ///
    /// # Errors
    /// The new document is unusable; the previous keys stay in force.
    pub fn replace_jwks(&self, jwks: &str) -> Result<(), Refusal> {
        let verifier = TokenVerifier::from_jwks(&self.issuer, HOST_AUDIENCE, jwks, &STUDIO_ROLES)?;
        let mut keys = self.keys.lock().map_err(Refusal::host)?;
        *keys = (verifier, kids(jwks));
        Ok(())
    }

    /// Whether a token since the last call named a key id the JWKS lacks: the
    /// host may refetch the pinned JWKS (rate-limited by the caller).
    pub fn take_unknown_kid(&self) -> bool {
        self.unknown_kid
            .lock()
            .is_ok_and(|mut seen| std::mem::take(&mut *seen))
    }

    fn verify(&self, token: &str, now: u64) -> Option<Caller> {
        let keys = self.keys.lock().ok()?;
        if token_kid(token).is_some_and(|kid| !keys.1.contains(&kid))
            && let Ok(mut seen) = self.unknown_kid.lock()
        {
            *seen = true;
        }
        let principal = keys.0.principal(token, &self.cell, now).ok()?;
        Some(Caller {
            subject_id: principal.subject_id,
            roles: principal.roles,
        })
    }
}

impl PrincipalResolver for TokenPrincipals {
    fn resolve(&self, authorization: Option<&str>, now: u64) -> Option<String> {
        let caller = authorization
            .and_then(|value| value.strip_prefix("Bearer "))
            .and_then(|token| self.verify(token, now));
        let subject = caller.as_ref().map(|caller| caller.subject_id.clone());
        self.slot.record(caller);
        subject
    }
}

/// Synthetic principals for tests and local smoke builds, named as
/// `Authorization: Bearer fixture:<name>`, each with its Norn roles.
pub struct FixturePrincipals {
    names: &'static [(&'static str, &'static [&'static str])],
    slot: Arc<CallerSlot>,
}

/// The fixture principals: `viewer@demo` holds `admin`, `guest@demo` `viewer`.
pub const DEMO_FIXTURES: &[(&str, &[&str])] =
    &[("viewer@demo", &["admin"]), ("guest@demo", &["viewer"])];

impl FixturePrincipals {
    /// Fixture principals recording into `slot`.
    #[must_use]
    pub const fn new(
        names: &'static [(&'static str, &'static [&'static str])],
        slot: Arc<CallerSlot>,
    ) -> Self {
        Self { names, slot }
    }
}

impl PrincipalResolver for FixturePrincipals {
    fn resolve(&self, authorization: Option<&str>, _now: u64) -> Option<String> {
        let caller = authorization
            .and_then(|value| value.strip_prefix("Bearer fixture:"))
            .and_then(|name| self.names.iter().find(|(known, _)| *known == name))
            .map(|(name, roles)| Caller {
                subject_id: (*name).to_owned(),
                roles: roles.iter().map(|role| (*role).to_owned()).collect(),
            });
        let subject = caller.as_ref().map(|caller| caller.subject_id.clone());
        self.slot.record(caller);
        subject
    }
}
