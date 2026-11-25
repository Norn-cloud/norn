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

use anyhow::{Context as _, Result};
use norn_api::scope::CellScope;
use norn_cell_do::principal::IssuerKeys;
use tokio::sync::OnceCell;

use crate::actor::now_ms;
use crate::config::HostConfig;
use crate::egress::outbound_client;
use crate::jwks::fetch_jwks;

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
    /// The gate `config` pins.
    ///
    /// # Errors
    /// The outbound HTTP client cannot be built.
    pub fn new(config: &HostConfig) -> Result<Self> {
        let mode = match (&config.issuer, &config.jwks_url) {
            (Some(issuer), Some(jwks_url)) => Mode::Tokens(Box::new(Tokens {
                jwks_url: jwks_url.clone(),
                issuer: issuer.clone(),
                http: outbound_client()?,
                keys: OnceCell::new(),
            })),
            (None, None) if norn_cell_do::fixtures_enabled() => Mode::Fixtures,
            _ => Mode::Closed,
        };
        Ok(Self { mode })
    }

    /// Whether the request's `Authorization` header carries a host token for
    /// `scope`. `None` is a request that names no cell scope (conformance
    /// probes, served only by fixtures builds).
    pub async fn admit(&self, authorization: Option<&str>, scope: Option<&CellScope>) -> Admission {
        let Tokens {
            jwks_url,
            issuer,
            http,
            keys,
        } = match &self.mode {
            Mode::Fixtures => return Admission::Admit,
            Mode::Closed => return Admission::Unavailable,
            Mode::Tokens(tokens) => &**tokens,
        };
        let (Some(token), Some(scope)) = (
            authorization.and_then(|value| value.strip_prefix("Bearer ")),
            scope,
        ) else {
            return Admission::Deny;
        };
        let keys = match keys
            .get_or_try_init(|| async {
                let jwks = Box::pin(fetch_jwks(http, jwks_url.clone(), true)).await?;
                IssuerKeys::new(issuer, &jwks.text, jwks.at / 1000)
                    .map_err(|refused| anyhow::anyhow!(refused.message))
                    .context("the issuer's JWKS is unusable")
            })
            .await
        {
            Ok(keys) => keys,
            Err(error) => {
                tracing::error!(error = %format!("{error:#}"), "the gate has no issuer keys");
                return Admission::Unavailable;
            }
        };
        let now = now_ms() / 1000;
        if keys.refresh_due(now) {
            refresh(keys, http, jwks_url).await;
        }
        if keys.principal(token, scope, now).is_some() {
            return Admission::Admit;
        }
        // A key the issuer rotated in since the last fetch: refetch once (at
        // most once a minute), then check again.
        if keys.take_unknown_kid(now)
            && refresh(keys, http, jwks_url).await
            && keys.principal(token, scope, now_ms() / 1000).is_some()
        {
            return Admission::Admit;
        }
        Admission::Deny
    }
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
