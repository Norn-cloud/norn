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
//!
//! APPROVAL-LIVE-1: a resolver also answers a parked call's requester's
//! authority now ([`PrincipalResolver::authority`]), so an approval runs
//! the call under the requester's current membership and roles, never the
//! ones recorded when it parked. [`TokenPrincipals`] asks its host to look
//! the requester up at the organization's authority (Studio), which answers
//! with a membership attestation the same issuer signs for the audience
//! [`AUTHORITY_AUDIENCE`]; the cell verifies it as it verifies a host
//! token. Without a configured lookup the authority is
//! [`Authority::Unknown`] and nothing runs.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use norn_api::principal::{STUDIO_ROLES, TokenVerifier};
use norn_api::refusal::Refusal;
use norn_api::scope::CellScope;

use norn_cell::flow_run::RunStarter;

use crate::policy::{Caller, CallerSlot};

/// The audience every host token must name.
pub const HOST_AUDIENCE: &str = "norn-host";

/// The audience of a membership attestation (APPROVAL-LIVE-1): the issuer's
/// answer that a subject is a member of the cell's organization now, with
/// its role. It never authenticates a request.
pub const AUTHORITY_AUDIENCE: &str = "norn-authority";

/// APPROVAL-LIVE-1: a parked call's requester's authority, as the
/// organization's authority answers it now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Authority {
    /// A member now: the requester verified with its current roles.
    Current(Caller),
    /// No longer a member of this cell's organization: the approved call is
    /// refused for good.
    Revoked,
    /// The lookup failed or did not answer: nothing runs, and the call
    /// stays approved for a retry.
    Unavailable,
    /// The host looks requesters up and has no answer for this request
    /// yet: a request that records nothing first asks for one
    /// ([`PrincipalResolver::ask_authority`]), and the host serves it again
    /// with the answer ([`TokenPrincipals::provide_authority`]).
    Pending,
    /// This host has no authority lookup: nothing runs, and the call waits
    /// for the requester's own request.
    Unknown,
}

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

    /// The slot this resolver records the verified caller into, which the
    /// cell's policy reads: a run's starter is recorded from it, and a run's
    /// action steps act as that starter through it (ACTION-STEP-1). `None`
    /// for a resolver with no slot, whose runs' action steps are denied.
    fn caller(&self) -> Option<&CallerSlot> {
        None
    }

    /// APPROVAL-LIVE-1: `requester`'s authority now (`now` in Unix
    /// seconds), from the organization's authority, never from the roles it
    /// carries. [`Authority::Unknown`] for a resolver without a lookup.
    fn authority(&self, _requester: &RunStarter, _now: u64) -> Authority {
        Authority::Unknown
    }

    /// APPROVAL-LIVE-1: ask the host to look `requester` up and serve the
    /// request again with the answer. Called only before anything is
    /// recorded (a decide that would run the requester's approved call), so
    /// serving the request again is the same request; a resolver without a
    /// host lookup ignores it.
    fn ask_authority(&self, _requester: &RunStarter) {}
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

    fn caller(&self) -> Option<&CallerSlot> {
        Some(&self.0)
    }
}

/// Verifies signed host tokens for one cell.
pub struct TokenPrincipals {
    issuer: String,
    cell: CellScope,
    slot: Arc<CallerSlot>,
    keys: Mutex<Keys>,
    lookup: Mutex<Lookup>,
}

/// APPROVAL-LIVE-1: the host's membership lookups for one request.
#[derive(Default)]
struct Lookup {
    /// The host looks requesters up ([`TokenPrincipals::enable_lookup`]).
    enabled: bool,
    /// The answers the host provided for the request being served, by
    /// subject; forgotten when it ends.
    answers: Vec<AuthorityAnswer>,
    /// The requester a request asked for and had no answer for: the host
    /// looks it up and serves the request again.
    wanted: Option<Wanted>,
}

/// The requester whose authority the host is asked to look up.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Wanted {
    /// Its subject (`user:<id>` or `service:<key id>`).
    pub subject: String,
    /// `user` or `service`.
    pub kind: String,
}

/// The host's answer for one requester: a membership attestation, or the
/// organization's statement that the subject is no member; anything else
/// is a lookup that did not answer.
#[derive(Debug, Clone, serde::Deserialize)]
struct AuthorityAnswer {
    subject: String,
    #[serde(default)]
    token: Option<String>,
    #[serde(default)]
    member: Option<bool>,
}

/// The issuer keys in force and when they were fetched.
struct Keys {
    verifier: TokenVerifier,
    /// Membership attestations: the same keys, [`AUTHORITY_AUDIENCE`].
    attestations: TokenVerifier,
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
        let attestations =
            TokenVerifier::from_jwks(issuer, AUTHORITY_AUDIENCE, jwks, &STUDIO_ROLES)?;
        Ok(Self {
            issuer: issuer.to_owned(),
            cell,
            slot,
            lookup: Mutex::new(Lookup::default()),
            keys: Mutex::new(Keys {
                verifier,
                attestations,
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
        let attestations =
            TokenVerifier::from_jwks(&self.issuer, AUTHORITY_AUDIENCE, jwks, &STUDIO_ROLES)?;
        let mut keys = self.keys.lock().map_err(Refusal::host)?;
        keys.verifier = verifier;
        keys.attestations = attestations;
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

    /// APPROVAL-LIVE-1: the host looks requesters up at the organization's
    /// authority (it is configured with the lookup's location and
    /// credential). Until then every requester's authority is
    /// [`Authority::Unknown`].
    pub fn enable_lookup(&self) {
        if let Ok(mut lookup) = self.lookup.lock() {
            lookup.enabled = true;
        }
    }

    /// APPROVAL-LIVE-1: the host's answer for the requester it was asked to
    /// look up, as JSON `{subject, token}` (a membership attestation),
    /// `{subject, member: false}` (no member), or `{subject}` (the lookup
    /// did not answer). It holds for the next request served, which is the
    /// one that asked for it, served again; a malformed answer is ignored,
    /// so that request finds the lookup unavailable.
    pub fn provide_authority(&self, answer: &str) {
        let Ok(mut lookup) = self.lookup.lock() else {
            return;
        };
        let parsed = serde_json::from_str::<AuthorityAnswer>(answer).ok();
        let subject = parsed.as_ref().map_or_else(
            || lookup.wanted.as_ref().map(|wanted| wanted.subject.clone()),
            |answer| Some(answer.subject.clone()),
        );
        let Some(subject) = subject else {
            return;
        };
        lookup.answers.retain(|known| known.subject != subject);
        lookup.answers.push(parsed.unwrap_or(AuthorityAnswer {
            subject,
            token: None,
            member: None,
        }));
    }

    /// APPROVAL-LIVE-1: the requester the last request asked to be looked
    /// up, once: the host looks it up and serves that request again with
    /// [`Self::provide_authority`].
    pub fn take_authority_wanted(&self) -> Option<Wanted> {
        self.lookup.lock().ok()?.wanted.take()
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
        // An answer holds for the one request it was provided for.
        if let Ok(mut lookup) = self.lookup.lock() {
            lookup.answers.clear();
        }
    }

    fn caller(&self) -> Option<&CallerSlot> {
        Some(&self.slot)
    }

    fn authority(&self, requester: &RunStarter, now: u64) -> Authority {
        let Ok(lookup) = self.lookup.lock() else {
            return Authority::Unavailable;
        };
        if !lookup.enabled {
            return Authority::Unknown;
        }
        let Some(answer) = lookup
            .answers
            .iter()
            .find(|answer| answer.subject == requester.subject)
            .cloned()
        else {
            return Authority::Pending;
        };
        drop(lookup);
        match (answer.token, answer.member) {
            (Some(token), _) => self
                .attested(&token, now)
                .filter(|caller| {
                    caller.subject_id == requester.subject
                        && caller.actor_kind == requester.actor_kind
                })
                .map_or(Authority::Unavailable, Authority::Current),
            (None, Some(false)) => Authority::Revoked,
            (None, _) => Authority::Unavailable,
        }
    }

    fn ask_authority(&self, requester: &RunStarter) {
        self.ask(requester);
    }
}

impl TokenPrincipals {
    /// [`PrincipalResolver::ask_authority`], once the lookup is enabled.
    fn ask(&self, requester: &RunStarter) {
        if let Ok(mut lookup) = self.lookup.lock()
            && lookup.enabled
        {
            lookup.wanted = Some(Wanted {
                subject: requester.subject.clone(),
                kind: requester.actor_kind.clone(),
            });
        }
    }

    /// The member a membership attestation names, verified at `now` with
    /// the keys in force; `None` for anything that does not verify.
    fn attested(&self, token: &str, now: u64) -> Option<Caller> {
        let mut keys = self.keys.lock().ok()?;
        keys.observe(now);
        if !keys.usable(now) {
            return None;
        }
        let principal = keys.attestations.membership(token, &self.cell, now).ok()?;
        Some(Caller {
            subject_id: principal.subject_id,
            roles: principal.roles,
            actor_kind: principal.actor_kind.as_str(),
            role_table: principal.role_table,
        })
    }
}

/// Synthetic principals for tests and local smoke builds, named as
/// `Authorization: Bearer fixture:<name>`, each with its Norn roles.
pub struct FixturePrincipals {
    names: &'static [(&'static str, &'static [&'static str])],
    slot: Arc<CallerSlot>,
    /// Whether the fixtures belong to this cell's organization.
    member: bool,
    /// How a requester's authority is answered (APPROVAL-LIVE-1).
    lookup: FixtureLookup,
}

/// How [`FixturePrincipals`] answer a requester's authority now
/// (APPROVAL-LIVE-1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FixtureLookup {
    /// No lookup: [`Authority::Unknown`].
    None,
    /// From the fixture table: a named subject is a member with its roles,
    /// any other is no member.
    Table,
    /// The lookup does not answer: [`Authority::Unavailable`].
    Unavailable,
}

/// The role table fixture principals report.
pub const FIXTURE_ROLES: &str = "fixtures/1";

/// The organization the demo fixtures belong to.
pub const FIXTURE_ORG: &str = "org_demo";

/// The fixture principals: `admin@demo` holds `admin`, `guest@demo` `viewer`.
pub const DEMO_FIXTURES: &[(&str, &[&str])] =
    &[("admin@demo", &["admin"]), ("guest@demo", &["viewer"])];

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
            lookup: FixtureLookup::None,
        }
    }

    /// The fixtures answering a requester's authority by `lookup`.
    #[must_use]
    pub const fn with_lookup(mut self, lookup: FixtureLookup) -> Self {
        self.lookup = lookup;
        self
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
            .map(|(name, roles)| fixture_caller(name, roles));
        let subject = caller.as_ref().map(|caller| caller.subject_id.clone());
        self.slot.record(caller);
        subject
    }
    fn end_request(&self) {
        self.slot.record(None);
    }

    fn caller(&self) -> Option<&CallerSlot> {
        Some(&self.slot)
    }

    fn authority(&self, requester: &RunStarter, _now: u64) -> Authority {
        match self.lookup {
            FixtureLookup::None => Authority::Unknown,
            FixtureLookup::Unavailable => Authority::Unavailable,
            FixtureLookup::Table => self
                .names
                .iter()
                .filter(|_| self.member && requester.actor_kind == "user")
                .find(|(name, _)| *name == requester.subject)
                .map_or(Authority::Revoked, |(name, roles)| {
                    Authority::Current(fixture_caller(name, roles))
                }),
        }
    }
}

fn fixture_caller(name: &str, roles: &[&str]) -> Caller {
    Caller {
        subject_id: name.to_owned(),
        roles: roles.iter().map(|role| (*role).to_owned()).collect(),
        actor_kind: "user",
        role_table: FIXTURE_ROLES,
    }
}
