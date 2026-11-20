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
use crate::tenant::{Gate, Served, Tenant};
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
    /// Token principals for `scope` from the pinned `issuer` and its `jwks`
    /// (fetched at `now_ms`), recording into `slot`; fixture principals in a
    /// `fixtures` build given neither.
    fn of(
        issuer: Option<String>,
        jwks: Option<String>,
        scope: CellScope,
        slot: std::sync::Arc<crate::policy::CallerSlot>,
        now_ms: f64,
    ) -> Result<Self, JsValue> {
        match (issuer, jwks) {
            (Some(issuer), Some(jwks)) => Ok(Self::Tokens(
                TokenPrincipals::new(&issuer, &jwks, scope, slot, seconds(now_ms))
                    .map_err(|refusal| JsValue::from_str(&refusal.code))?,
            )),
            #[cfg(feature = "fixtures")]
            (None, None) => Ok(Self::Fixtures(
                crate::principal::FixturePrincipals::new(crate::principal::DEMO_FIXTURES, slot)
                    .of_org(crate::principal::FIXTURE_ORG, &scope),
            )),
            _ => Err(JsValue::from_str(
                "the token issuer and its JWKS are required",
            )),
        }
    }

    fn resolver(&self) -> &dyn PrincipalResolver {
        match self {
            Self::Tokens(tokens) => tokens,
            #[cfg(feature = "fixtures")]
            Self::Fixtures(fixtures) => fixtures,
        }
    }

    fn replace_jwks(&self, jwks: &str, now_ms: f64) -> Result<(), JsValue> {
        match self {
            Self::Tokens(tokens) => tokens
                .replace_jwks(jwks, seconds(now_ms))
                .map_err(|refusal| JsValue::from_str(&refusal.code)),
            #[cfg(feature = "fixtures")]
            Self::Fixtures(_) => Ok(()),
        }
    }

    fn jwks_due(&self, now_ms: f64) -> bool {
        match self {
            Self::Tokens(tokens) => tokens.refresh_due(seconds(now_ms)),
            #[cfg(feature = "fixtures")]
            Self::Fixtures(_) => false,
        }
    }

    fn take_unknown_kid(&self, now_ms: f64) -> bool {
        match self {
            Self::Tokens(tokens) => tokens.take_unknown_kid(seconds(now_ms)),
            #[cfg(feature = "fixtures")]
            Self::Fixtures(_) => false,
        }
    }
}

/// One opened tenant cell.
#[wasm_bindgen]
pub struct NornCell {
    tenant: Tenant<WorkersEngine>,
    principals: Principals,
    /// The cell's source connections and uploads, once configured (#921).
    connect: Option<crate::connect::Connect>,
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
    /// A sync to run in the background; the host drives it with `syncStep`.
    #[wasm_bindgen(js_name = startSync)]
    pub start_sync: Option<String>,
    /// A continuation: the host fetches `thenRequest` exactly as planned and
    /// calls `resume` with this token; that reply replaces this one.
    #[wasm_bindgen(js_name = thenToken)]
    pub then_token: Option<String>,
    /// The request to fetch, as JSON `{method, url, headers, body}`.
    #[wasm_bindgen(js_name = thenRequest)]
    pub then_request: Option<String>,
    /// D102: the analytics engine call the Worker performs before re-entering
    /// with `resume` (an `EngineCall` as JSON); `status` and `body` are then
    /// not the answer.
    pub engine: Option<String>,
    /// Why the stored cell does not open, or why its definitions installed
    /// no action policy: for the operator's log only, never part of the reply.
    #[wasm_bindgen(js_name = policyError)]
    pub policy_error: Option<String>,
}

#[wasm_bindgen]
impl NornCell {
    /// Whether this build accepts fixture principals ([`crate::fixtures_enabled`]):
    /// the Worker refuses to open a cell of such a build outside a smoke or
    /// test stage, and the deploy script refuses to deploy it.
    #[must_use]
    #[wasm_bindgen(js_name = fixturesEnabled)]
    pub fn fixtures_enabled() -> bool {
        crate::fixtures_enabled()
    }

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
        crate::incident::install_panic_log();
        let lineage_key = lineage_key.map(String::into_bytes);
        let scope = CellScope {
            org_id,
            environment_id,
        };
        let tenant = (if scope == seed::demo_scope() {
            let store = seed::provision(WorkersEngine::new(storage), &scope).map_err(rejected)?;
            // The demo cell publishes no questions: its decisions and action
            // calls answer with the lineage-keyed receipt handles.
            Tenant::seeded(scope.clone(), &DEMO_GRANTS, store, None)
                .and_then(|tenant| tenant.with_receipt_handles(lineage_key.as_deref()))
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
        let principals = Principals::of(issuer, jwks, scope, tenant.caller(), now_ms)?;
        Ok(Self {
            tenant,
            principals,
            connect: None,
        })
    }

    /// D102: answer questions on the tenant analytics engine the Worker
    /// reaches (`NORN_OLAP_URL`); without it this cell has no analytics.
    #[wasm_bindgen(js_name = enableAnalytics)]
    pub fn enable_analytics(&mut self) {
        self.tenant.enable_analytics();
    }

    /// The cell's verified ledger head as JSON (`{seq, hash, commits}`), for
    /// the host conformance suite (`apps/norn-cell-do/conformance`). Smoke
    /// builds only: a deployable cell answers nothing outside its routes.
    ///
    /// # Errors
    /// The scope is vacant or the ledger does not verify.
    #[cfg(feature = "fixtures")]
    #[wasm_bindgen(js_name = ledgerHead)]
    pub fn ledger_head(&self) -> Result<String, JsValue> {
        let host = self
            .tenant
            .serving_host()
            .ok_or_else(|| JsValue::from_str("the cell is vacant"))?;
        let head = host
            .verified_head()
            .map_err(|refusal| JsValue::from_str(&refusal.code))?;
        serde_json::to_string(&head).map_err(rejected)
    }

    /// Re-enter a request with the engine's answer to the call it ended with.
    #[must_use]
    #[allow(
        clippy::needless_pass_by_value,
        clippy::too_many_arguments,
        reason = "wasm-bindgen passes strings by value; a re-entry carries the request and the answer"
    )]
    #[wasm_bindgen(js_name = resumeEngine)]
    pub fn resume_engine(
        &mut self,
        method: &str,
        path: &str,
        query: &str,
        authorization: Option<String>,
        now_ms: f64,
        body: Option<String>,
        call: &str,
        status: u16,
        answer: String,
    ) -> Reply {
        if let Ok(call) = serde_json::from_str::<norn_api::analytics_routes::EngineCall>(call) {
            self.tenant
                .resume_engine(norn_api::analytics_routes::EngineAnswer {
                    op: call.op,
                    digest: call.digest,
                    status,
                    body: answer,
                });
        }
        self.fetch(method, path, query, authorization, now_ms, body)
    }

    /// Install a refetched JWKS from the pinned location, fetched at `now_ms`.
    ///
    /// # Errors
    /// The document is unusable; the previous keys stay in force and keep
    /// aging, so they are refused once past the stale bound.
    #[wasm_bindgen(js_name = replaceJwks)]
    pub fn replace_jwks(&self, jwks: &str, now_ms: f64) -> Result<(), JsValue> {
        self.principals.replace_jwks(jwks, now_ms)
    }

    /// Whether the keys have reached their maximum age and the host should
    /// refetch the pinned JWKS before serving, whether or not any token names
    /// an unknown key id. Rate-limited, so a failing refetch retries on an
    /// interval; the host installs the result with `replaceJwks`.
    #[wasm_bindgen(js_name = jwksDue)]
    #[must_use]
    pub fn jwks_due(&self, now_ms: f64) -> bool {
        self.principals.jwks_due(now_ms)
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
        // #921: the cell's source and upload routes, decided by its own
        // policy; then D102's engine routes (loads read the cell's own
        // uploads); then the cell's other routes: one portable dispatch.
        let served = self.tenant.serve(
            self.connect.as_mut(),
            self.principals.resolver(),
            &request,
            millis(now_ms),
        );
        let response = match served {
            Served::Cell(response) => response,
            Served::Connect(routed) => {
                let (then_token, then_request) =
                    routed.then.map_or((None, None), |(token, planned)| {
                        (Some(token), serde_json::to_string(&planned).ok())
                    });
                return Reply {
                    status: routed.response.status,
                    body: routed.response.body,
                    refresh_jwks: false,
                    start_sync: routed.start_sync,
                    then_token,
                    then_request,
                    engine: None,
                    policy_error: None,
                };
            }
        };
        let policy_error = self
            .tenant
            .open_error()
            .or_else(|| self.tenant.action_policy_error())
            .map(str::to_owned);
        let refresh_jwks = self.principals.take_unknown_kid(now_ms);
        let engine = self
            .tenant
            .take_engine_call()
            .and_then(|call| serde_json::to_string(&call).ok());
        Reply {
            status: response.status,
            body: response.body,
            refresh_jwks,
            start_sync: None,
            then_token: None,
            then_request: None,
            engine,
            policy_error,
        }
    }

    /// Configure the cell's source connections (`NORN_SOURCES`, a JSON
    /// array) and the server variables they read (`NORN_SECRET_*` and
    /// `NORN_S3_*`, as one JSON object). The values stay in the cell.
    ///
    /// # Errors
    /// Malformed configuration; the cell then serves without connections.
    #[wasm_bindgen(js_name = configureConnect)]
    pub fn configure_connect(&mut self, config: &str, vars: &str) -> Result<(), JsValue> {
        self.connect = Some(crate::connect::Connect::new(config, vars).map_err(rejected)?);
        Ok(())
    }

    /// SYNC-ONE-1: claim the next connection due on its schedule at `now_ms`
    /// (checked and marked in this one call), as its own principal.
    #[wasm_bindgen(js_name = syncDue)]
    pub fn sync_due(&mut self, now_ms: f64) -> Option<String> {
        let host = self.tenant.serving_host()?;
        let connect = self.connect.as_mut()?;
        // A schedule the cell's records cannot be read back for claims nothing.
        connect.restore_schedule(host).ok()?;
        connect.due(millis(now_ms))
    }

    /// TIMER-ALARM-1: whether this object serves a provisioned cell. Only
    /// then does the host keep its scope for alarms; an open of a vacant
    /// scope (anyone may name one) leaves no trace.
    #[must_use]
    pub fn serving(&self) -> bool {
        self.tenant.serving_host().is_some()
    }

    /// TIMER-ALARM-1: when the Durable Object's alarm should next fire (Unix
    /// milliseconds): the cell's earliest deadline or its next scheduled
    /// sync, whichever is sooner, possibly already passed; `undefined` when
    /// nothing is pending. The host arms it after every request, after a
    /// sync ends and when the object opens.
    ///
    /// # Errors
    /// A host failure reading the cell.
    #[wasm_bindgen(js_name = nextDue)]
    pub fn next_due(&mut self, now_ms: f64) -> Result<Option<f64>, JsValue> {
        let cell = self
            .tenant
            .next_due()
            .map_err(|refusal| rejected(refusal.code))?;
        Ok(earliest(cell, self.sync_next(now_ms)?))
    }

    /// TIMER-ALARM-1: the alarm fired. Run the cell's work due at `now_ms`
    /// (reservation expiries, flow timers, decision deadlines) through its
    /// ordinary commit paths, and say when the alarm should fire next. A
    /// re-delivered alarm finds nothing due and commits nothing. The host
    /// then claims a due sync with `syncDue`.
    ///
    /// # Errors
    /// A host failure reading the cell; the runtime retries the alarm.
    #[wasm_bindgen(js_name = runDue)]
    pub fn run_due(&mut self, now_ms: f64) -> Result<Due, JsValue> {
        let work = self
            .tenant
            .run_due(millis(now_ms))
            .map_err(|refusal| rejected(refusal.code))?;
        Ok(Due {
            committed: work.committed(),
            next: earliest(work.next, self.sync_next(now_ms)?),
            failed: (!work.failed.is_empty()).then(|| work.failed.join("; ")),
        })
    }

    /// The next scheduled sync, with the schedule read back from the cell.
    fn sync_next(&mut self, now_ms: f64) -> Result<Option<u64>, JsValue> {
        let (Some(host), Some(connect)) = (self.tenant.serving_host(), self.connect.as_mut())
        else {
            return Ok(None);
        };
        connect.restore_schedule(host).map_err(rejected)?;
        Ok(connect.next_sync_due(millis(now_ms)))
    }

    /// Advance the running sync of `id`; `status` and `body` answer the
    /// request the previous step planned. Returns `{"request": …}` or
    /// `{"done": …}`.
    #[wasm_bindgen(js_name = syncStep)]
    #[must_use]
    #[allow(
        clippy::needless_pass_by_value,
        reason = "wasm-bindgen passes owned optional values"
    )]
    pub fn sync_step(
        &mut self,
        id: &str,
        status: Option<u16>,
        body: Option<Vec<u8>>,
        now_ms: f64,
        rows: Option<u32>,
    ) -> String {
        let response = status.map(|status| (status, body.as_deref().unwrap_or_default()));
        let rows = rows.and_then(|rows| usize::try_from(rows).ok());
        match self.connect.as_mut() {
            Some(connect) => connect.step(
                self.tenant.serving_host(),
                id,
                response,
                (millis(now_ms), rows),
            ),
            None => String::from(r#"{"done":{"outcome":"failed"}}"#),
        }
    }

    /// Continue a request after the host fetched what the cell planned.
    #[must_use]
    #[allow(
        clippy::needless_pass_by_value,
        reason = "wasm-bindgen passes owned bytes"
    )]
    pub fn resume(&mut self, token: &str, status: u16, body: Vec<u8>, now_ms: f64) -> Reply {
        let response = match self.connect.as_mut() {
            Some(connect) => connect.resume(
                self.tenant.serving_host(),
                token,
                status,
                &body,
                millis(now_ms),
            ),
            None => server::Response {
                status: 404,
                body: String::from("{}"),
            },
        };
        Reply {
            status: response.status,
            body: response.body,
            refresh_jwks: false,
            start_sync: None,
            then_token: None,
            then_request: None,
            engine: None,
            policy_error: None,
        }
    }
}

/// TENANT-ISOLATION-1 (S1, S3): a scope's membership gate, asked before the
/// Durable Object opens the cell's storage ([`Gate`]). It holds the scope,
/// the host's grant table, the receipt handles and the verified issuer keys,
/// never the storage: an outsider's request is answered here, with exactly
/// what a scope with no cell answers, and the cell is never opened, read,
/// armed or synced for it.
#[wasm_bindgen]
pub struct NornGate {
    gate: Gate,
    principals: Principals,
}

#[wasm_bindgen]
impl NornGate {
    /// The gate of the DO's own scope. `issuer`, `jwks`, `lineage_key` and
    /// `now_ms` are what [`NornCell::new`] is given; `sources` and `vars`
    /// what `configureConnect` is (the gate only asks whether they configure
    /// source and upload routes).
    ///
    /// # Errors
    /// No usable issuer keys, or a lineage key shorter than 32 bytes.
    #[wasm_bindgen(constructor)]
    #[allow(
        clippy::too_many_arguments,
        reason = "the gate is given what the cell is given"
    )]
    pub fn new(
        org_id: String,
        environment_id: String,
        issuer: Option<String>,
        jwks: Option<String>,
        lineage_key: Option<String>,
        sources: &str,
        vars: &str,
        now_ms: f64,
    ) -> Result<Self, JsValue> {
        crate::incident::install_panic_log();
        let lineage_key = lineage_key.map(String::into_bytes);
        let scope = CellScope {
            org_id,
            environment_id,
        };
        let connect = crate::connect::Connect::new(sources, vars).is_ok();
        let gate = Gate::new(scope.clone(), &DEMO_GRANTS, lineage_key.as_deref(), connect)
            .map_err(rejected)?;
        let principals = Principals::of(issuer, jwks, scope, gate.caller(), now_ms)?;
        Ok(Self { gate, principals })
    }

    /// `undefined` when the request reaches the cell (a verified member, or
    /// a provisioner provisioning); otherwise the outsider's whole reply,
    /// which carries no continuation, sync or engine call.
    #[must_use]
    #[allow(
        clippy::needless_pass_by_value,
        reason = "wasm-bindgen passes optional strings by value"
    )]
    pub fn check(
        &self,
        method: &str,
        path: &str,
        query: &str,
        authorization: Option<String>,
        now_ms: f64,
        body: Option<String>,
    ) -> Option<Reply> {
        let request = server::Request {
            method,
            path,
            query,
            authorization: authorization.as_deref(),
            now: seconds(now_ms),
            body: body.as_deref().unwrap_or_default(),
        };
        let response = self
            .gate
            .outsider::<WorkersEngine>(self.principals.resolver(), &request);
        let refresh_jwks = self.principals.take_unknown_kid(now_ms);
        response.map(|response| Reply {
            status: response.status,
            body: response.body,
            refresh_jwks,
            start_sync: None,
            then_token: None,
            then_request: None,
            engine: None,
            policy_error: None,
        })
    }

    /// As [`NornCell::replace_jwks`].
    ///
    /// # Errors
    /// The document is unusable; the previous keys stay in force.
    #[wasm_bindgen(js_name = replaceJwks)]
    pub fn replace_jwks(&self, jwks: &str, now_ms: f64) -> Result<(), JsValue> {
        self.principals.replace_jwks(jwks, now_ms)
    }

    /// As [`NornCell::jwks_due`].
    #[wasm_bindgen(js_name = jwksDue)]
    #[must_use]
    pub fn jwks_due(&self, now_ms: f64) -> bool {
        self.principals.jwks_due(now_ms)
    }
}

/// TIMER-ALARM-1: what an alarm did, and when the next one fires.
#[wasm_bindgen(getter_with_clone)]
#[derive(Debug, Clone)]
pub struct Due {
    /// The alarm committed something: live subscriptions are answered again.
    pub committed: bool,
    /// When the alarm should next fire (Unix milliseconds); `undefined`
    /// when nothing is pending.
    pub next: Option<f64>,
    /// Why items that were due could not be handled (they stay due), for
    /// the operator's log only.
    pub failed: Option<String>,
}

#[allow(
    clippy::as_conversions,
    clippy::cast_precision_loss,
    reason = "a Unix millisecond time is far below 2^53, exact as an f64"
)]
fn earliest(left: Option<u64>, right: Option<u64>) -> Option<f64> {
    let both = match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (left, None) => left,
        (None, right) => right,
    };
    both.map(|millis| millis as f64)
}

#[allow(
    clippy::as_conversions,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a host clock in milliseconds since the epoch, clamped to non-negative"
)]
fn millis(now_ms: f64) -> u64 {
    now_ms.max(0.0) as u64
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
