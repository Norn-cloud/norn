//! The router's gate: the Studio-signed host token is verified for the scope
//! a request routes to before the router reads its body, looks up or creates
//! an actor, or accepts a socket upgrade. The runners are reachable from the
//! internet (the alpha's `alpha-cells` origin), so an anonymous caller must
//! not be able to make the engine create actors or make a runner buffer
//! bodies.
//!
//! The gate is the cell's own check, run early: `norn-cell-do`'s
//! [`IssuerKeys`] (the same verifier, key ages and refetch policy as each
//! cell's `TokenPrincipals`), against the pinned issuer and JWKS location,
//! with this process's shared JWKS fetch (the `jwks` module).
//! Admission here grants nothing: the cell verifies the token again and its
//! policy authorizes every read and write.
//!
//! Cold-start JWKS failure: a failed first fetch records its time and holds
//! off further outbound fetches for [`DEFAULT_COLD_BACKOFF_MS`]. Admit calls
//! inside that window return `Unavailable` without an HTTP request, so an
//! issuer outage cannot be amplified back at the JWKS URL by a request
//! burst. Concurrent cold-start callers coalesce on one shared mutex: only
//! one runs the fetch; a successful fetch clears the backoff for everyone.

use anyhow::{Context as _, Result};
use norn_api::scope::CellScope;
use norn_cell_do::principal::IssuerKeys;
use tokio::sync::{Mutex, OnceCell};

use crate::actor::now_ms;
use crate::config::HostConfig;
use crate::egress::outbound_client;
use crate::jwks::fetch_jwks;

/// Default cold-start JWKS failure backoff, in ms: an issuer outage is
/// retried at most this often per runner, however many requests arrive.
pub const DEFAULT_COLD_BACKOFF_MS: u64 = 10_000;

/// What the gate says about one request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    /// The token verifies for the scope (or the build checks fixture
    /// principals in the cell): route it.
    Admit,
    /// No token, or one that does not verify for the scope.
    Deny,
    /// The issuer's keys cannot be had: nothing is routed.
    Unavailable,
}

/// The pinned issuer and its keys, fetched on first use.
struct Tokens {
    jwks_url: String,
    issuer: String,
    http: reqwest::Client,
    keys: OnceCell<IssuerKeys>,
    /// Unix-ms of the last cold-start JWKS fetch failure. While it is within
    /// `cold_backoff_ms` of `now_ms()`, admit short-circuits to
    /// `Unavailable` and never calls `fetch_jwks`.
    cold_backoff: Mutex<Option<u64>>,
    /// Backoff cooldown after a cold-start JWKS failure (ms).
    cold_backoff_ms: u64,
}

enum Mode {
    /// The pinned issuer: the cells verify its tokens.
    Tokens(Box<Tokens>),
    /// A fixtures build without an issuer (smoke or test stages only; the
    /// cell refuses any other stage): the cell's fixture principals decide.
    Fixtures,
    /// Neither: no cell opens, so nothing is routed.
    Closed,
}

/// The router's token gate.
pub struct Gate {
    mode: Mode,
}

impl std::fmt::Debug for Gate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mode = match &self.mode {
            Mode::Tokens(_) => "tokens",
            Mode::Fixtures => "fixtures",
            Mode::Closed => "closed",
        };
        f.debug_struct("Gate").field("mode", &mode).finish()
    }
}

impl Gate {
    /// The gate `config` pins, with the default cold-start backoff.
    ///
    /// # Errors
    /// The outbound HTTP client cannot be built.
    pub fn new(config: &HostConfig) -> Result<Self> {
        Self::with_cold_backoff_ms(config, DEFAULT_COLD_BACKOFF_MS)
    }

    /// Like [`Gate::new`], with `cold_backoff_ms` the cold-start retry
    /// cooldown (0 disables the backoff: every call retries). For tests
    /// only; production builds the gate through [`Gate::new`].
    ///
    /// # Errors
    /// The outbound HTTP client cannot be built.
    #[doc(hidden)]
    pub fn with_cold_backoff_ms(config: &HostConfig, cold_backoff_ms: u64) -> Result<Self> {
        let mode = match (&config.issuer, &config.jwks_url) {
            (Some(issuer), Some(jwks_url)) => Mode::Tokens(Box::new(Tokens {
                jwks_url: jwks_url.clone(),
                issuer: issuer.clone(),
                http: outbound_client()?,
                keys: OnceCell::new(),
                cold_backoff: Mutex::new(None),
                cold_backoff_ms,
            })),
            (None, None) if norn_cell_do::fixtures_enabled() => Mode::Fixtures,
            _ => Mode::Closed,
        };
        Ok(Self { mode })
    }

    /// Clear the cold-start backoff state so the next admit retries a failed
    /// JWKS fetch. For tests only (recovery paths that cannot await the real
    /// cooldown).
    #[doc(hidden)]
    pub async fn clear_cold_backoff_for_test(&self) {
        if let Mode::Tokens(tokens) = &self.mode {
            *tokens.cold_backoff.lock().await = None;
        }
    }

    /// Whether the request's `Authorization` header carries a host token for
    /// `scope`. `None` is a request that names no cell scope (conformance
    /// probes, served only by fixtures builds).
    pub async fn admit(&self, authorization: Option<&str>, scope: Option<&CellScope>) -> Admission {
        let tokens = match &self.mode {
            Mode::Fixtures => return Admission::Admit,
            Mode::Closed => return Admission::Unavailable,
            Mode::Tokens(tokens) => &**tokens,
        };
        let (Some(token), Some(scope)) = (
            authorization.and_then(|value| value.strip_prefix("Bearer ")),
            scope,
        ) else {
            tracing::debug!(
                bearer_present = authorization.is_some_and(|value| value.starts_with("Bearer ")),
                scope_named = scope.is_some(),
                "the gate denied a request (no bearer token or no scope)"
            );
            return Admission::Deny;
        };
        let Ok(keys) = cold_keys(tokens).await else {
            return Admission::Unavailable;
        };
        let now = now_ms() / 1000;
        if keys.refresh_due(now) {
            refresh(keys, &tokens.http, &tokens.jwks_url).await;
        }
        if keys.principal(token, scope, now).is_some() {
            return Admission::Admit;
        }
        // A key the issuer rotated in since the last fetch: refetch once (at
        // most once a minute), then check again.
        if keys.take_unknown_kid(now)
            && refresh(keys, &tokens.http, &tokens.jwks_url).await
            && keys.principal(token, scope, now_ms() / 1000).is_some()
        {
            return Admission::Admit;
        }
        tracing::debug!(
            bearer_present = true,
            org_id = %scope.org_id,
            environment_id = %scope.environment_id,
            "the gate denied a request (token did not verify for the scope)"
        );
        Admission::Deny
    }
}

/// The pinned issuer's keys, fetching them on cold start. Coalesces
/// concurrent cold-start callers under one mutex; once the keys are in the
/// `OnceCell`, the fast path returns them without the mutex. A cold fetch
/// failure stamps `cold_backoff`; further callers inside `cold_backoff_ms`
/// return `Err` without an outbound request.
async fn cold_keys(tokens: &Tokens) -> Result<&IssuerKeys, ()> {
    if let Some(keys) = tokens.keys.get() {
        return Ok(keys);
    }
    let mut backoff = tokens.cold_backoff.lock().await;
    if let Some(keys) = tokens.keys.get() {
        return Ok(keys);
    }
    if let Some(failed_at) = *backoff
        && now_ms().saturating_sub(failed_at) < tokens.cold_backoff_ms
    {
        return Err(());
    }
    let fetched = match Box::pin(fetch_jwks(&tokens.http, tokens.jwks_url.clone(), true)).await {
        Ok(jwks) => jwks,
        Err(error) => {
            *backoff = Some(now_ms());
            tracing::error!(
                error = %format!("{error:#}"),
                "the gate could not fetch JWKS at cold start"
            );
            return Err(());
        }
    };
    let new_keys = match IssuerKeys::new(&tokens.issuer, &fetched.text, fetched.at / 1000)
        .map_err(|refused| anyhow::anyhow!(refused.message))
        .context("the issuer's JWKS is unusable")
    {
        Ok(keys) => keys,
        Err(error) => {
            *backoff = Some(now_ms());
            tracing::error!(
                error = %format!("{error:#}"),
                "the gate has no issuer keys"
            );
            return Err(());
        }
    };
    let _installed = tokens.keys.set(new_keys);
    *backoff = None;
    tokens.keys.get().ok_or(())
}

/// Refetch the pinned JWKS into `keys`; whether new keys were installed.
async fn refresh(keys: &IssuerKeys, http: &reqwest::Client, url: &str) -> bool {
    match Box::pin(fetch_jwks(http, url.to_owned(), false)).await {
        Ok(jwks) => match keys.replace_jwks(&jwks.text, jwks.at / 1000) {
            Ok(()) => true,
            Err(refused) => {
                tracing::error!(error = %refused.message, "the refetched JWKS is unusable");
                false
            }
        },
        Err(error) => {
            tracing::error!(error = %format!("{error:#}"), "JWKS refresh failed");
            false
        }
    }
}
