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

/// How long fetched issuer keys are used before the host should refetch them
/// (seconds), whether or not any token names an unknown key id.
pub const JWKS_MAX_AGE_SECS: u64 = 300;

/// How long past a fetch the keys are still honoured (seconds). A key the
/// issuer removed stops verifying after at most this long, even while the
/// pinned location is unreachable; verification is then refused until a
/// refetch succeeds. Bounds revocation latency independently of the token
/// lifetime.
pub const JWKS_MAX_STALE_SECS: u64 = 900;

/// Least time between two refetch attempts (seconds), so neither tokens
/// naming unknown key ids nor an unreachable issuer can drive a fetch storm.
pub const JWKS_RETRY_SECS: u64 = 60;

/// Resolves the request's `Authorization` header to a trusted principal at
/// `now` (Unix seconds), recording it for the policy.
pub trait PrincipalResolver {
    /// The verified subject, or `None`; every read then denies by default.
    fn resolve(&self, authorization: Option<&str>, now: u64) -> Option<String>;

    /// Forget the request's principal. Required: the dispatch calls it when
    /// a request ends, on every exit path ([`RequestScope`]).
    fn end_request(&self);
}

/// The lifetime of one request's verified principal: it ends when this
/// guard drops, on every return path and during unwinding.
pub struct RequestScope<'a>(&'a dyn PrincipalResolver);

impl<'a> RequestScope<'a> {
    /// Begin a request on `principals`.
    #[must_use]
    pub const fn new(principals: &'a dyn PrincipalResolver) -> Self {
        Self(principals)
    }
}

impl Drop for RequestScope<'_> {
    fn drop(&mut self) {
        self.0.end_request();
    }
}

/// A deployable cell without a configured resolver: nobody is verified.
pub struct NoPrincipals(pub Arc<CallerSlot>);

impl PrincipalResolver for NoPrincipals {
    fn resolve(&self, _authorization: Option<&str>, _now: u64) -> Option<String> {
        self.0.record(None);
        None
    }

    fn end_request(&self) {
        self.0.record(None);
    }
}

/// Verifies signed host tokens for one cell.
pub struct TokenPrincipals {
    issuer: String,
    cell: CellScope,
    slot: Arc<CallerSlot>,
    keys: Mutex<Keys>,
}

/// The issuer keys in force and when they were fetched.
struct Keys {
    verifier: TokenVerifier,
    kids: BTreeSet<String>,
    /// Unix seconds of the last successful install.
    fetched: u64,
    /// Unix seconds of the last refetch the host was asked to make.
    attempted: Option<u64>,
    /// A token named a key id the JWKS lacks since the last report.
    unknown_kid: bool,
    /// The highest `now` any call has presented since the last install.
    high_water: u64,
    /// The clock was seen to move backward since the last install. Wall-clock
    /// ages cannot be trusted after that, so the keys are refused as stale
    /// and a refetch is due; only a successful install clears it.
    regressed: bool,
}

impl Keys {
    /// Record `now`; a value below the highest one seen is a clock regression.
    fn observe(&mut self, now: u64) {
        if now < self.high_water {
            self.regressed = true;
        }
        self.high_water = self.high_water.max(now);
    }

    /// Whether the keys may be used at `now`: fresh by wall-clock age, and the
    /// clock has not moved backward since they were fetched.
    fn usable(&self, now: u64) -> bool {
        !self.regressed && now.saturating_sub(self.fetched) <= JWKS_MAX_STALE_SECS
    }

    /// Whether another refetch may be requested at `now`. An attempt stamped
    /// in the future (the clock moved back) does not block the next one.
    fn may_retry(&self, now: u64) -> bool {
        self.attempted
            .is_none_or(|at| at > now || now.saturating_sub(at) >= JWKS_RETRY_SECS)
    }
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
    /// Admit the pinned issuer's JWKS, fetched at `now` (Unix seconds), for
    /// `cell`.
    ///
    /// # Errors
    /// The document is not a JWKS or publishes no Ed25519 key.
    pub fn new(
        issuer: &str,
        jwks: &str,
        cell: CellScope,
        slot: Arc<CallerSlot>,
        now: u64,
    ) -> Result<Self, Refusal> {
        let verifier = TokenVerifier::from_jwks(issuer, HOST_AUDIENCE, jwks, &STUDIO_ROLES)?;
        Ok(Self {
            issuer: issuer.to_owned(),
            cell,
            slot,
            keys: Mutex::new(Keys {
                verifier,
                kids: kids(jwks),
                fetched: now,
                attempted: None,
                unknown_kid: false,
                high_water: now,
                regressed: false,
            }),
        })
    }

    /// Replace the keys after a refetch of the pinned JWKS location at `now`
    /// (Unix seconds). A key the new document omits stops verifying at once.
    /// This is the only thing that clears an observed clock regression.
    ///
    /// # Errors
    /// The new document is unusable; the previous keys stay in force, and
    /// age on (or stay refused after a clock regression).
    pub fn replace_jwks(&self, jwks: &str, now: u64) -> Result<(), Refusal> {
        let verifier = TokenVerifier::from_jwks(&self.issuer, HOST_AUDIENCE, jwks, &STUDIO_ROLES)?;
        let mut keys = self.keys.lock().map_err(Refusal::host)?;
        keys.verifier = verifier;
        keys.kids = kids(jwks);
        keys.fetched = now;
        keys.high_water = now;
        keys.regressed = false;
        Ok(())
    }

    /// Whether the keys have reached [`JWKS_MAX_AGE_SECS`] and the host should
    /// refetch the pinned JWKS now, before serving. Independent of any token.
    /// Granting it starts the retry interval, so a failed refetch is retried
    /// after [`JWKS_RETRY_SECS`], not on every request. After an observed
    /// clock regression a refetch is due regardless of age.
    pub fn refresh_due(&self, now: u64) -> bool {
        let Ok(mut keys) = self.keys.lock() else {
            return false;
        };
        keys.observe(now);
        let aged = keys.regressed || now.saturating_sub(keys.fetched) >= JWKS_MAX_AGE_SECS;
        let due = aged && keys.may_retry(now);
        if due {
            keys.attempted = Some(now);
        }
        due
    }

    /// Whether a token since the last call named a key id the JWKS lacks and a
    /// refetch is allowed now (at most one per [`JWKS_RETRY_SECS`]): the host
    /// refetches the pinned JWKS, then retries the request once.
    pub fn take_unknown_kid(&self, now: u64) -> bool {
        let Ok(mut keys) = self.keys.lock() else {
            return false;
        };
        keys.observe(now);
        let seen = std::mem::take(&mut keys.unknown_kid);
        let allowed = seen && keys.may_retry(now);
        if allowed {
            keys.attempted = Some(now);
        }
        allowed
    }

    fn verify(&self, token: &str, now: u64) -> Option<Caller> {
        let mut keys = self.keys.lock().ok()?;
        keys.observe(now);
        // Keys older than the stale bound are not honoured: a removed key
        // must not verify forever because the issuer cannot be reached. Nor
        // are keys once the clock has moved backward: their age is unknown
        // until a refetch succeeds.
        if !keys.usable(now) {
            return None;
        }
        if token_kid(token).is_some_and(|kid| !keys.kids.contains(&kid)) {
            keys.unknown_kid = true;
        }
        let principal = keys.verifier.principal(token, &self.cell, now).ok()?;
        Some(Caller {
            subject_id: principal.subject_id,
            roles: principal.roles,
            actor_kind: principal.actor_kind.as_str(),
            role_table: principal.role_table,
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
    fn end_request(&self) {
        self.slot.record(None);
    }
}

/// Synthetic principals for tests and local smoke builds, named as
/// `Authorization: Bearer fixture:<name>`, each with its Norn roles.
pub struct FixturePrincipals {
    names: &'static [(&'static str, &'static [&'static str])],
    slot: Arc<CallerSlot>,
    /// Whether the fixtures belong to this cell's organization.
    member: bool,
}

/// The role table fixture principals report.
pub const FIXTURE_ROLES: &str = "fixtures/1";

/// The organization the demo fixtures belong to.
pub const FIXTURE_ORG: &str = "org_demo";

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
        Self {
            names,
            slot,
            member: true,
        }
    }

    /// The fixtures as members of `org` only, as a verified host token is:
    /// at another organization's cell they are nobody.
    #[must_use]
    pub fn of_org(mut self, org: &str, cell: &CellScope) -> Self {
        self.member = cell.org_id == org;
        self
    }
}

impl PrincipalResolver for FixturePrincipals {
    fn resolve(&self, authorization: Option<&str>, _now: u64) -> Option<String> {
        let caller = authorization
            .filter(|_| self.member)
            .and_then(|value| value.strip_prefix("Bearer fixture:"))
            .and_then(|name| self.names.iter().find(|(known, _)| *known == name))
            .map(|(name, roles)| Caller {
                subject_id: (*name).to_owned(),
                roles: roles.iter().map(|role| (*role).to_owned()).collect(),
                actor_kind: "user",
                role_table: FIXTURE_ROLES,
            });
        let subject = caller.as_ref().map(|caller| caller.subject_id.clone());
        self.slot.record(caller);
        subject
    }
    fn end_request(&self) {
        self.slot.record(None);
    }
}
