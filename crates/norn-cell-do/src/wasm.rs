//! The Durable Object binding: one [`NornCell`] per DO instance.
//!
//! The DO constructor passes its root `ctx.storage`; the cell keeps it for the
//! object's lifetime. The DO's input/output gates and single-threaded event
//! loop make this cell the only writer; `transactionSync` is the commit.
//!
//! Callers are verified host tokens ([`TokenPrincipals`]); the issuer and its
//! JWKS come from the Worker's pinned configuration, and the JWKS is refetched
//! on a bounded schedule ([`TokenPrincipals::refresh_due`]). Fixture
//! principals exist only in builds with the `fixtures` feature (local smoke
//! tests). The analytics cell also needs the stage's lineage key
//! (`NORN_LINEAGE_KEY`, at least 32 bytes) and refuses to open without it.
//!
//! The two demo scopes are seeded by this build. Every other scope is a
//! [`Tenant`]: vacant, writing nothing, until its organization's owner
//! provisions it (`POST /v1/cell:provision`).

use norn_api::scope::CellScope;
use norn_cell::workers::WorkersEngine;
use wasm_bindgen::prelude::*;

use crate::analytics::{self, DemoQuestions};
use crate::policy::DEMO_GRANTS;
use crate::principal::{PrincipalResolver, TokenPrincipals};
use crate::tenant::Tenant;
use crate::{seed, server};

enum Principals {
    Tokens(TokenPrincipals),
    #[cfg(feature = "fixtures")]
    Fixtures(crate::principal::FixturePrincipals),
}

#[allow(
    clippy::infallible_destructuring_match,
    reason = "the fixture variant exists only in fixture builds"
)]
impl Principals {
    fn resolver(&self) -> &dyn PrincipalResolver {
        match self {
            Self::Tokens(tokens) => tokens,
            #[cfg(feature = "fixtures")]
            Self::Fixtures(fixtures) => fixtures,
        }
    }
}

/// One opened tenant cell.
#[wasm_bindgen]
pub struct NornCell {
    tenant: Tenant<WorkersEngine>,
    principals: Principals,
}

fn rejected(error: impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&error.to_string())
}

/// A finite HTTP reply.
#[wasm_bindgen(getter_with_clone)]
pub struct Reply {
    /// HTTP status.
    pub status: u16,
    /// JSON body.
    pub body: String,
    /// The pinned JWKS should be refetched (an unknown key id was seen and the
    /// retry interval has passed); the host refetches, then retries once.
    #[wasm_bindgen(js_name = refreshJwks)]
    pub refresh_jwks: bool,
}

#[wasm_bindgen]
impl NornCell {
    /// Open the cell for the DO's own scope: a demo scope is seeded on first
    /// use; any other scope is served if it was provisioned, else vacant.
    /// `issuer` and `jwks` are the pinned token issuer and its key document
    /// (fetched at `now_ms`, Unix milliseconds); without the `fixtures` feature
    /// both are required. `lineage_key` is the stage secret
    /// (`NORN_LINEAGE_KEY`) that keys the analytics cell's lineage handles.
    ///
    /// # Errors
    /// Seeding failed, the storage holds a cell this host cannot serve, no
    /// usable issuer keys were supplied, or the analytics cell has no lineage
    /// key of at least 32 bytes.
    #[wasm_bindgen(constructor)]
    pub fn new(
        storage: JsValue,
        org_id: String,
        environment_id: String,
        issuer: Option<String>,
        jwks: Option<String>,
        lineage_key: Option<String>,
        now_ms: f64,
    ) -> Result<Self, JsValue> {
        let lineage_key = lineage_key.map(String::into_bytes);
        let scope = CellScope {
            org_id,
            environment_id,
        };
        let tenant = (if scope == seed::demo_scope() {
            let store = seed::provision(WorkersEngine::new(storage), &scope).map_err(rejected)?;
            Tenant::seeded(scope.clone(), &DEMO_GRANTS, store, None)
        } else if scope == analytics::analytics_scope() {
            let analytics::Analytics {
                store,
                plan,
                definitions,
                catalog,
            } = analytics::provision(WorkersEngine::new(storage), &scope).map_err(rejected)?;
            let questions = DemoQuestions::new(plan, scope.clone())
                .try_with_handle_key(lineage_key.as_deref())
                .map_err(rejected)?
                .with_metrics(definitions, catalog);
            Tenant::seeded(scope.clone(), &DEMO_GRANTS, store, Some(questions))
        } else {
            Tenant::open(
                scope.clone(),
                &DEMO_GRANTS,
                Box::new(move || Ok(WorkersEngine::new(storage.clone()))),
                lineage_key.as_deref(),
            )
        })
        .map_err(rejected)?;
        let slot = tenant.caller();
        let principals = match (issuer, jwks) {
            (Some(issuer), Some(jwks)) => Principals::Tokens(
                TokenPrincipals::new(&issuer, &jwks, scope, slot, seconds(now_ms))
                    .map_err(|refusal| JsValue::from_str(&refusal.code))?,
            ),
            #[cfg(feature = "fixtures")]
            (None, None) => Principals::Fixtures(crate::principal::FixturePrincipals::new(
                crate::principal::DEMO_FIXTURES,
                slot,
            )),
            _ => {
                return Err(JsValue::from_str(
                    "the token issuer and its JWKS are required",
                ));
            }
        };
        Ok(Self { tenant, principals })
    }

    /// Install a refetched JWKS from the pinned location, fetched at `now_ms`.
    ///
    /// # Errors
    /// The document is unusable; the previous keys stay in force and keep
    /// aging, so they are refused once past the stale bound.
    #[wasm_bindgen(js_name = replaceJwks)]
    pub fn replace_jwks(&self, jwks: &str, now_ms: f64) -> Result<(), JsValue> {
        match &self.principals {
            Principals::Tokens(tokens) => tokens
                .replace_jwks(jwks, seconds(now_ms))
                .map_err(|refusal| JsValue::from_str(&refusal.code)),
            #[cfg(feature = "fixtures")]
            Principals::Fixtures(_) => Ok(()),
        }
    }

    /// Whether the keys have reached their maximum age and the host should
    /// refetch the pinned JWKS before serving, whether or not any token names
    /// an unknown key id. Rate-limited, so a failing refetch retries on an
    /// interval; the host installs the result with `replaceJwks`.
    #[wasm_bindgen(js_name = jwksDue)]
    #[must_use]
    pub fn jwks_due(&self, now_ms: f64) -> bool {
        match &self.principals {
            Principals::Tokens(tokens) => tokens.refresh_due(seconds(now_ms)),
            #[cfg(feature = "fixtures")]
            Principals::Fixtures(_) => false,
        }
    }

    /// Serve one request at `now_ms` (the host clock, Unix milliseconds);
    /// every outcome is a status and a JSON document.
    #[must_use]
    #[allow(
        clippy::needless_pass_by_value,
        reason = "wasm-bindgen passes an optional string by value"
    )]
    pub fn fetch(
        &mut self,
        method: &str,
        path: &str,
        query: &str,
        authorization: Option<String>,
        now_ms: f64,
        body: Option<String>,
    ) -> Reply {
        let request = server::Request {
            method,
            path,
            query,
            authorization: authorization.as_deref(),
            now: seconds(now_ms),
            body: body.as_deref().unwrap_or_default(),
        };
        let response = self.tenant.handle(self.principals.resolver(), &request);
        let refresh_jwks = match &self.principals {
            Principals::Tokens(tokens) => tokens.take_unknown_kid(seconds(now_ms)),
            #[cfg(feature = "fixtures")]
            Principals::Fixtures(_) => false,
        };
        Reply {
            status: response.status,
            body: response.body,
            refresh_jwks,
        }
    }
}

#[allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a host clock in milliseconds since the epoch, clamped to non-negative"
)]
fn seconds(now_ms: f64) -> u64 {
    (now_ms.max(0.0) / 1000.0) as u64
}
