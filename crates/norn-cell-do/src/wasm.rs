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

use norn_api::definition_routes::DefinitionKinds;
use norn_api::host::DevHost;
use norn_api::question_routes::AdHocQuestions;
use norn_api::scope::CellScope;
use norn_cell::workers::WorkersEngine;
use wasm_bindgen::prelude::*;

use crate::analytics::{self, DemoQuestions};
use crate::policy::{DEMO_GRANTS, ReadPolicy};
use crate::principal::{PrincipalResolver, TokenPrincipals};
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
    host: DevHost<WorkersEngine>,
    /// The cell's admitted questions, when it publishes any.
    questions: Option<DemoQuestions>,
    principals: Principals,
    /// The verified caller the policy reads, for `GET /v1/access`.
    caller: std::sync::Arc<crate::policy::CallerSlot>,
    /// The cell's collections, for `GET /v1/access`.
    object_types: Vec<String>,
    /// The retained-definition kinds this cell admits.
    kinds: DefinitionKinds,
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
    /// Open (provisioning on first use) the cell for the DO's own scope.
    /// `issuer` and `jwks` are the pinned token issuer and its key document
    /// (fetched at `now_ms`, Unix milliseconds); without the `fixtures` feature
    /// both are required. `lineage_key` is the stage secret
    /// (`NORN_LINEAGE_KEY`) that keys the analytics cell's lineage handles.
    ///
    /// # Errors
    /// The scope has no published cell, provisioning failed, no usable issuer
    /// keys were supplied, or the analytics cell has no lineage key of at
    /// least 32 bytes.
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
        let engine = WorkersEngine::new(storage);
        let (store, questions) = if scope == seed::demo_scope() {
            (seed::provision(engine, &scope).map_err(rejected)?, None)
        } else if scope == analytics::analytics_scope() {
            let analytics::Analytics {
                store,
                plan,
                definitions,
                catalog,
            } = analytics::provision(engine, &scope).map_err(rejected)?;
            let questions = DemoQuestions::new(plan, scope.clone())
                .try_with_handle_key(lineage_key.as_deref())
                .map_err(rejected)?
                .with_metrics(definitions, catalog);
            (store, Some(questions))
        } else {
            return Err(JsValue::from_str("no cell is published for this scope"));
        };
        // Data reads are decided by the kernel PDP over this cell's own model.
        let policy = ReadPolicy::compile(store.model().object_types(), &scope, &DEMO_GRANTS)
            .map_err(rejected)?;
        let slot = policy.caller();
        let caller = policy.caller();
        let object_types = store
            .model()
            .object_types()
            .map(|object_type| object_type.name.as_str().to_owned())
            .collect();
        let principals = match (issuer, jwks) {
            (Some(issuer), Some(jwks)) => Principals::Tokens(
                TokenPrincipals::new(&issuer, &jwks, scope.clone(), slot, seconds(now_ms))
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
        let host = DevHost::from_store(store, scope, |command| command.payload.clone())
            .map_err(|refusal| JsValue::from_str(&refusal.code))?
            .with_authorizer(policy);
        Ok(Self {
            host,
            questions,
            principals,
            caller,
            object_types,
            // Dashboards are built in; agents wait for lane CANVAS's checker.
            kinds: DefinitionKinds::default()
                // Sign-offs verify against the checker's words, as Studio shows
                // them; D59: a flow is installed by signing the server's words for it.
                .with_restater(norn_api::definition_routes::checker_restatement)
                // A binding runs as a role of the cell's declared grant table.
                .with_roles(
                    crate::policy::DEMO_GRANTS
                        .entries
                        .iter()
                        .map(|(role, _)| *role),
                ),
        })
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
        &self,
        method: &str,
        path: &str,
        query: &str,
        authorization: Option<String>,
        now_ms: f64,
        body: Option<String>,
    ) -> Reply {
        let questions = self
            .questions
            .as_ref()
            .map(|questions| -> &dyn AdHocQuestions<WorkersEngine> { questions });
        let resolver = self.principals.resolver();
        let request = server::Request {
            method,
            path,
            query,
            authorization: authorization.as_deref(),
            now: seconds(now_ms),
            body: body.as_deref().unwrap_or_default(),
        };
        let response = if path == "/v1/access" {
            server::access(
                &self.host,
                resolver,
                &self.caller,
                &self.object_types,
                &request,
            )
        } else {
            server::handle_all(&self.host, resolver, questions, &self.kinds, &request)
        };
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
