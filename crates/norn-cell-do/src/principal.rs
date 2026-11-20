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
//! the requester up at the organization's authority (Studio) with a fresh
//! one-use challenge bound to the exact attempt; Studio answers, member or
//! not, with a statement the same issuer signs for the audience
//! [`AUTHORITY_AUDIENCE`], naming that challenge and attempt. The cell
//! verifies it as it verifies a host token, as the answer to that challenge
//! only, and keeps what it establishes for the rest of the request in the
//! caller slot. Without a configured lookup the authority is
//! [`Authority::Unknown`].

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use norn_api::principal::{
    ATTESTATION_LEEWAY_SECONDS, AuthorityChallenge, MAX_ATTESTATION_SECONDS, Membership,
    STUDIO_ROLES, TokenVerifier,
};
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
    /// No longer a member of this cell's organization, as the authority's
    /// signed statement for this exact lookup says: the approved call is
    /// refused for good.
    Revoked,
    /// The lookup failed, did not answer, or answered with anything that
    /// does not verify as a signed statement for this exact lookup: nothing
    /// runs, nothing final is recorded, and the call stays approved for a
    /// retry.
    Unavailable,
    /// The host looks requesters up and has no answer for this attempt
    /// yet: the attempt asks for one ([`PrincipalResolver::ask_authority`])
    /// and runs nothing; the host looks the requester up and resumes it
    /// ([`Resume`]) with the answer ([`TokenPrincipals::provide_authority`]).
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
    /// seconds) for the one attempt `binding` names (an approved call's
    /// decision and command, or a run's step call), from the organization's
    /// authority, never from the roles it carries. An answer is for exactly
    /// that attempt and the request serving it. [`Authority::Unknown`] for a
    /// resolver without a lookup.
    fn authority(&self, _requester: &RunStarter, _binding: &str, _now: u64) -> Authority {
        Authority::Unknown
    }

    /// APPROVAL-LIVE-1: ask the host to look `requester` up for the attempt
    /// `binding` names, with a fresh one-use challenge, and to resume as
    /// `resume` says once it has the answer; a resolver without a host
    /// lookup ignores it.
    fn ask_authority(&self, _requester: &RunStarter, _binding: &str, _resume: Resume, _now: u64) {}
}

/// APPROVAL-LIVE-1: how the host resumes an attempt that asked for a
/// requester's authority, once it has the answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resume {
    /// Nothing was recorded yet: the host serves the same request again.
    Request,
    /// The request recorded what it decided; only the drive of this run is
    /// resumed, so nothing the request wrote is replayed.
    Drive(String),
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
        self.0.forget_answers();
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

/// APPROVAL-LIVE-1: the most challenges outstanding at once; the oldest is
/// dropped first, and an answer to it is no answer.
const MAX_CHALLENGES: usize = 16;

/// APPROVAL-LIVE-1: the host's membership lookups.
#[derive(Default)]
struct Lookup {
    /// Entropy the host supplied when it enabled lookups
    /// ([`TokenPrincipals::enable_lookup`]); `None` while lookups are off.
    seed: Option<String>,
    /// Challenges issued so far (the nonce counter).
    issued: u64,
    /// Challenges issued and not yet answered; each answers once. What an
    /// answer establishes is kept in the caller slot for the rest of the
    /// request it was provided for ([`CallerSlot::remember`]).
    challenges: Vec<Challenge>,
    /// The lookup the last attempt asked for: the host looks it up and
    /// resumes it.
    wanted: Option<Wanted>,
}

/// One outstanding challenge: what it asks and how the host resumes it.
struct Challenge {
    nonce: String,
    subject: String,
    kind: String,
    binding: String,
    resume: Resume,
    issued_at: u64,
}

/// The lookup the host is asked to make: the requester, the cell's fresh
/// one-use challenge, the attempt it is bound to, and how the host resumes
/// once it has the answer (`request`: serve the request again; `drive`:
/// resume only the run's drive).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Wanted {
    /// Its subject (`user:<id>` or `service:<key id>`).
    pub subject: String,
    /// `user` or `service`.
    pub kind: String,
    /// The challenge the answer must name.
    pub challenge: String,
    /// The attempt the answer must name.
    pub binding: String,
    /// `request` or `drive`.
    pub resume: &'static str,
}

/// The host's answer to one challenge: the organization's signed statement
/// (member with a role, or not a member), or nothing when the lookup did
/// not answer.
#[derive(Debug, Clone, serde::Deserialize)]
struct AuthorityAnswer {
    challenge: String,
    #[serde(default)]
    token: Option<String>,
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
    /// credential), and supplies `seed`, at least 32 bytes of fresh entropy,
    /// from which the cell derives its unpredictable one-use challenges.
    /// Until then (or with a shorter seed) every requester's authority is
    /// [`Authority::Unknown`].
    pub fn enable_lookup(&self, seed: &[u8]) {
        if seed.len() < 32 {
            return;
        }
        if let Ok(mut lookup) = self.lookup.lock() {
            lookup.seed = Some(URL_SAFE_NO_PAD.encode(seed));
            self.slot.require_live(true);
        }
    }

    /// APPROVAL-LIVE-1: the host's answer to a challenge it was asked to
    /// look up, as JSON `{challenge, token}` (the authority's signed
    /// statement) or `{challenge}` (the lookup did not answer), received at
    /// `now`. The challenge is spent whatever the answer says. The answer is
    /// verified now, against that challenge's requester and attempt only,
    /// and what it establishes holds for the request being served (it is
    /// forgotten when that request ends): a member with its current role, a
    /// signed "no member", or, for anything else, unavailable. Returns how
    /// the host resumes the attempt that asked: serve the request again, or
    /// resume a run's drive; `None` for a malformed answer or one to no
    /// outstanding challenge, which is ignored.
    pub fn provide_authority(&self, answer: &str, now: u64) -> Option<Resume> {
        let parsed = serde_json::from_str::<AuthorityAnswer>(answer).ok()?;
        let challenge = {
            let mut lookup = self.lookup.lock().ok()?;
            let at = lookup
                .challenges
                .iter()
                .position(|challenge| challenge.nonce == parsed.challenge)?;
            lookup.challenges.remove(at)
        };
        let in_time = now
            <= challenge
                .issued_at
                .saturating_add(MAX_ATTESTATION_SECONDS)
                .saturating_add(ATTESTATION_LEEWAY_SECONDS);
        let verdict = parsed
            .token
            .filter(|_| in_time)
            .and_then(|token| self.attested(&token, &challenge, now))
            .map_or(Authority::Unavailable, |membership| {
                verdict_of(membership, &challenge)
            });
        self.slot
            .remember(&challenge.subject, &challenge.binding, verdict);
        Some(challenge.resume)
    }

    /// APPROVAL-LIVE-1: the lookup the last attempt asked for, once: the
    /// host looks it up and resumes it with [`Self::provide_authority`].
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
        self.slot.forget_answers();
    }

    fn caller(&self) -> Option<&CallerSlot> {
        Some(&self.slot)
    }

    fn authority(&self, requester: &RunStarter, binding: &str, _now: u64) -> Authority {
        let enabled = self
            .lookup
            .lock()
            .map_or(true, |lookup| lookup.seed.is_some());
        if !enabled {
            return Authority::Unknown;
        }
        self.slot
            .verdict(&requester.subject, binding)
            .unwrap_or(Authority::Pending)
    }

    fn ask_authority(&self, requester: &RunStarter, binding: &str, resume: Resume, now: u64) {
        let Ok(mut lookup) = self.lookup.lock() else {
            return;
        };
        let Some(seed) = lookup.seed.clone() else {
            return;
        };
        // One lookup per pass: the first attempt that needs one asks.
        if lookup.wanted.is_some() {
            return;
        }
        lookup.issued = lookup.issued.saturating_add(1);
        let Ok(nonce) = norn_ir::canonical::domain_hash_hex(
            b"norn.authority.challenge/1\0",
            &(seed, lookup.issued, &requester.subject, binding),
        ) else {
            return;
        };
        if lookup.challenges.len() >= MAX_CHALLENGES {
            lookup.challenges.remove(0);
        }
        let label = match resume {
            Resume::Request => "request",
            Resume::Drive(_) => "drive",
        };
        lookup.challenges.push(Challenge {
            nonce: nonce.clone(),
            subject: requester.subject.clone(),
            kind: requester.actor_kind.clone(),
            binding: binding.to_owned(),
            resume,
            issued_at: now,
        });
        lookup.wanted = Some(Wanted {
            subject: requester.subject.clone(),
            kind: requester.actor_kind.clone(),
            challenge: nonce,
            binding: binding.to_owned(),
            resume: label,
        });
    }
}

/// What a verified statement establishes for the requester `challenge`
/// asked about: only a statement about exactly that subject and actor kind
/// counts.
fn verdict_of(membership: Membership, challenge: &Challenge) -> Authority {
    match membership {
        Membership::Member(principal)
            if principal.subject_id == challenge.subject
                && principal.actor_kind.as_str() == challenge.kind =>
        {
            Authority::Current(Caller {
                subject_id: principal.subject_id,
                roles: principal.roles,
                actor_kind: principal.actor_kind.as_str(),
                role_table: principal.role_table,
            })
        }
        Membership::NotMember {
            subject_id,
            actor_kind,
        } if subject_id == challenge.subject && actor_kind.as_str() == challenge.kind => {
            Authority::Revoked
        }
        Membership::Member(_) | Membership::NotMember { .. } => Authority::Unavailable,
    }
}

impl TokenPrincipals {
    /// The authority's statement in `token`, verified at `now` with the
    /// keys in force as the answer to exactly `challenge`; `None` for
    /// anything that does not verify.
    fn attested(&self, token: &str, challenge: &Challenge, now: u64) -> Option<Membership> {
        let mut keys = self.keys.lock().ok()?;
        keys.observe(now);
        if !keys.usable(now) {
            return None;
        }
        keys.attestations
            .membership(
                token,
                &self.cell,
                now,
                &AuthorityChallenge {
                    nonce: &challenge.nonce,
                    binding: &challenge.binding,
                },
            )
            .ok()
    }
}

/// Synthetic principals for tests and local smoke builds, named as
/// `Authorization: Bearer fixture:<name>`, each with its Norn roles.
pub struct FixturePrincipals {
    names: &'static [(&'static str, &'static [&'static str])],
    slot: Arc<CallerSlot>,
    /// Whether the fixtures belong to this cell's organization.
    member: bool,
    /// The role table the fixtures report.
    role_table: &'static str,
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
            role_table: FIXTURE_ROLES,
        }
    }

    /// The fixtures answering a requester's authority by `lookup`; with a
    /// lookup, consuming an approval needs its answer (APPROVAL-LIVE-1).
    #[must_use]
    pub fn with_lookup(mut self, lookup: FixtureLookup) -> Self {
        self.lookup = lookup;
        self.slot.require_live(lookup != FixtureLookup::None);
        self
    }

    /// The fixtures as verified from another role table (a redeployed
    /// table): the same subjects, another principal.
    #[must_use]
    pub const fn with_role_table(mut self, role_table: &'static str) -> Self {
        self.role_table = role_table;
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
            .map(|(name, roles)| fixture_caller(name, roles, self.role_table));
        let subject = caller.as_ref().map(|caller| caller.subject_id.clone());
        self.slot.record(caller);
        subject
    }
    fn end_request(&self) {
        self.slot.record(None);
        self.slot.forget_answers();
    }

    fn caller(&self) -> Option<&CallerSlot> {
        Some(&self.slot)
    }

    fn authority(&self, requester: &RunStarter, _binding: &str, _now: u64) -> Authority {
        match self.lookup {
            FixtureLookup::None => Authority::Unknown,
            FixtureLookup::Unavailable => Authority::Unavailable,
            FixtureLookup::Table => self
                .names
                .iter()
                .filter(|_| self.member && requester.actor_kind == "user")
                .find(|(name, _)| *name == requester.subject)
                .map_or(Authority::Revoked, |(name, roles)| {
                    Authority::Current(fixture_caller(name, roles, self.role_table))
                }),
        }
    }
}

fn fixture_caller(name: &str, roles: &[&str], role_table: &'static str) -> Caller {
    Caller {
        subject_id: name.to_owned(),
        roles: roles.iter().map(|role| (*role).to_owned()).collect(),
        actor_kind: "user",
        role_table,
    }
}
