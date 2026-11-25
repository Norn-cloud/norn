//! One opened tenant cell: the native counterpart of the Durable Object
//! binding's `NornCell` (`norn-cell-do/src/wasm.rs`), with the same
//! decisions over the same `Tenant` dispatch. It lives on the actor's cell
//! thread and never awaits; the actor ([`crate::actor`]) does every I/O.

use norn_api::scope::CellScope;
use norn_cell_do::analytics::{self, DemoQuestions};
use norn_cell_do::policy::DEMO_GRANTS;
use norn_cell_do::principal::{PrincipalResolver, TokenPrincipals};
use norn_cell_do::tenant::Tenant;
use norn_cell_do::{connect, seed, server};

use crate::engine::RivetEngine;

enum Principals {
    Tokens(TokenPrincipals),
    #[cfg(feature = "fixtures")]
    Fixtures(norn_cell_do::principal::FixturePrincipals),
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

/// How a cell is opened: its scope, the pinned token issuer and the JWKS
/// fetched at `jwks_at_ms`, and the stage's lineage key.
#[derive(Clone)]
pub struct OpenArgs {
    /// The scope the router named.
    pub scope: CellScope,
    /// `NORN_HOST_ISSUER`.
    pub issuer: Option<String>,
    /// The issuer's key document.
    pub jwks: Option<String>,
    /// When it was fetched (Unix milliseconds), or now.
    pub jwks_at_ms: u64,
    /// `NORN_LINEAGE_KEY`.
    pub lineage_key: Option<String>,
}

impl std::fmt::Debug for OpenArgs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenArgs")
            .field("scope", &self.scope)
            .field("issuer", &self.issuer)
            .finish_non_exhaustive()
    }
}

/// A finite reply, as `wasm.rs`'s `Reply`.
#[derive(Debug, Clone, Default)]
pub struct Reply {
    /// HTTP status.
    pub status: u16,
    /// JSON body.
    pub body: String,
    /// The pinned JWKS should be refetched, then the request retried once.
    pub refresh_jwks: bool,
    /// A sync to run in the background.
    pub start_sync: Option<String>,
    /// A continuation token and the request to fetch for it (JSON).
    pub then: Option<(String, String)>,
    /// An analytics engine call (an `EngineCall` as JSON) to perform first.
    pub engine: Option<String>,
    /// Why the cell does not open or serves no action, for the operator.
    pub policy_error: Option<String>,
}

impl Reply {
    fn plain(response: server::Response) -> Self {
        Self {
            status: response.status,
            body: response.body,
            ..Self::default()
        }
    }
}

/// What an alarm run did (TIMER-ALARM-1).
#[derive(Debug, Clone, Default)]
pub struct Due {
    /// It committed something: live subscriptions are answered again.
    pub committed: bool,
    /// When the next alarm should fire (Unix milliseconds).
    pub next: Option<u64>,
    /// What stayed due, and why, for the operator's log.
    pub failed: Option<String>,
}

/// One request as the actor delivers it to the cell.
#[derive(Debug, Clone)]
pub struct Call {
    /// HTTP method.
    pub method: String,
    /// Path.
    pub path: String,
    /// Raw query, without `?`.
    pub query: String,
    /// The `Authorization` header.
    pub authorization: Option<String>,
    /// The host clock, Unix milliseconds.
    pub now_ms: u64,
    /// The body, for `POST`.
    pub body: Option<String>,
}

impl Call {
    fn request(&self) -> server::Request<'_> {
        server::Request {
            method: &self.method,
            path: &self.path,
            query: &self.query,
            authorization: self.authorization.as_deref(),
            now: self.now_ms / 1000,
            body: self.body.as_deref().unwrap_or_default(),
        }
    }
}

/// One opened tenant cell over a [`RivetEngine`].
pub struct CellCore {
    tenant: Tenant<RivetEngine>,
    principals: Principals,
    connect: Option<connect::Connect>,
}

impl CellCore {
    /// Open the cell of `args.scope` over the engines `engine` yields: a demo
    /// scope is seeded on first use; any other scope is served when it was
    /// provisioned, else vacant (nothing is written).
    ///
    /// # Errors
    /// Seeding failed, the storage holds a cell this host cannot serve, no
    /// usable issuer keys were supplied, or the analytics cell has no
    /// lineage key of at least 32 bytes.
    pub fn open(
        engine: impl Fn() -> RivetEngine + 'static,
        args: OpenArgs,
    ) -> Result<Self, String> {
        norn_cell_do::incident::install_panic_log();
        let lineage_key = args.lineage_key.map(String::into_bytes);
        let scope = args.scope;
        let tenant = (if scope == seed::demo_scope() {
            let store = seed::provision(engine(), &scope).map_err(|e| e.to_string())?;
            Tenant::seeded(scope.clone(), &DEMO_GRANTS, store, None)
                .and_then(|tenant| tenant.with_receipt_handles(lineage_key.as_deref()))
        } else if scope == analytics::analytics_scope() {
            let analytics::Analytics {
                store,
                plan,
                definitions,
                catalog,
            } = analytics::provision(engine(), &scope).map_err(|e| e.to_string())?;
            let questions = DemoQuestions::new(plan, scope.clone())
                .try_with_handle_key(lineage_key.as_deref())
                .map_err(|e| e.to_string())?
                .with_metrics(definitions, catalog);
            Tenant::seeded(scope.clone(), &DEMO_GRANTS, store, Some(questions))
        } else {
            Tenant::open(
                scope.clone(),
                &DEMO_GRANTS,
                Box::new(move || Ok(engine())),
                lineage_key.as_deref(),
            )
        })
        .map_err(|e| e.to_string())?;
        let slot = tenant.caller();
        let principals = match (args.issuer, args.jwks) {
            (Some(issuer), Some(jwks)) => Principals::Tokens(
                TokenPrincipals::new(&issuer, &jwks, scope, slot, args.jwks_at_ms / 1000)
                    .map_err(|refusal| refusal.code)?,
            ),
            #[cfg(feature = "fixtures")]
            (None, None) => Principals::Fixtures(
                norn_cell_do::principal::FixturePrincipals::new(
                    norn_cell_do::principal::DEMO_FIXTURES,
                    slot,
                )
                .of_org(norn_cell_do::principal::FIXTURE_ORG, &scope),
            ),
            _ => return Err("the token issuer and its JWKS are required".to_owned()),
        };
        Ok(Self {
            tenant,
            principals,
            connect: None,
        })
    }

    /// D102: answer questions on the tenant analytics engine. OLAP-CRED-1:
    /// `signing_key` is the host's key (`NORN_OLAP_SIGNING_KEY`); every
    /// engine call carries a credential minted for this cell's tenant.
    ///
    /// # Errors
    /// A missing or malformed signing key; the cell then has no analytics.
    pub fn enable_analytics(&mut self, olap: &crate::config::Olap) -> Result<(), String> {
        // D136: the engine the deployment selected (`NORN_OLAP_ENGINE`).
        let engine = norn_cell::analytics::AnalyticalEngineId::from_name(
            olap.engine.as_deref().unwrap_or("duckdb"),
        )?;
        let issuer = norn_api::olap_credential::Issuer::parse(
            olap.signing_key.as_deref().ok_or_else(|| "NORN_OLAP_SIGNING_KEY is required".to_owned())?,
        )
        .map_err(|error| format!("NORN_OLAP_SIGNING_KEY: {error}"))?;
        self.tenant.enable_analytics(issuer, engine);
        Ok(())
    }

    /// Configure the cell's source connections and the variables they read.
    ///
    /// # Errors
    /// Malformed configuration; the cell then serves without connections.
    pub fn configure_connect(&mut self, config: &str, vars: &str) -> Result<(), String> {
        self.connect = Some(connect::Connect::new(config, vars)?);
        Ok(())
    }

    /// Whether this cell is provisioned and served.
    #[must_use]
    pub fn serving(&self) -> bool {
        self.tenant.serving_host().is_some()
    }

    /// The verified ledger head as JSON (conformance builds only).
    ///
    /// # Errors
    /// The scope is vacant or the ledger does not verify.
    #[cfg(feature = "conformance")]
    pub fn ledger_head(&self) -> Result<String, String> {
        let host = self
            .tenant
            .serving_host()
            .ok_or_else(|| "the cell is vacant".to_owned())?;
        let head = host.verified_head().map_err(|refusal| refusal.code)?;
        serde_json::to_string(&head).map_err(|e| e.to_string())
    }

    /// Serve one request.
    pub fn fetch(&mut self, call: &Call) -> Reply {
        let request = call.request();
        if let Some(connect) = self.connect.as_mut()
            && let Some(routed) = connect.route(
                self.tenant.serving_host(),
                self.principals.resolver(),
                &request,
                call.now_ms,
            )
        {
            return Reply {
                status: routed.response.status,
                body: routed.response.body,
                start_sync: routed.start_sync,
                then: routed.then.and_then(|(token, planned)| {
                    serde_json::to_string(&planned)
                        .ok()
                        .map(|json| (token, json))
                }),
                ..Reply::default()
            };
        }
        let uploads = self
            .connect
            .as_ref()
            .map(|connect| -> &dyn norn_api::analytics_routes::Uploads { connect });
        let response = self
            .tenant
            .handle_analytics(self.principals.resolver(), uploads, &request)
            .unwrap_or_else(|| self.tenant.handle(self.principals.resolver(), &request));
        let policy_error = self
            .tenant
            .open_error()
            .or_else(|| self.tenant.action_policy_error())
            .map(str::to_owned);
        let refresh_jwks = match &self.principals {
            Principals::Tokens(tokens) => tokens.take_unknown_kid(call.now_ms / 1000),
            #[cfg(feature = "fixtures")]
            Principals::Fixtures(_) => false,
        };
        let engine = self
            .tenant
            .take_engine_call(call.now_ms)
            .and_then(|call| serde_json::to_string(&call).ok());
        Reply {
            status: response.status,
            body: response.body,
            refresh_jwks,
            engine,
            policy_error,
            ..Reply::default()
        }
    }

    /// Re-enter a request with the engine's answer to the call it ended with.
    pub fn resume_engine(
        &mut self,
        call: &Call,
        engine_call: &str,
        status: u16,
        answer: String,
    ) -> Reply {
        if let Ok(parsed) =
            serde_json::from_str::<norn_api::analytics_routes::EngineCall>(engine_call)
        {
            self.tenant
                .resume_engine(norn_api::analytics_routes::EngineAnswer {
                    op: parsed.op,
                    digest: parsed.digest,
                    status,
                    body: answer,
                });
        }
        self.fetch(call)
    }

    /// Install a refetched JWKS fetched at `now_ms`.
    ///
    /// # Errors
    /// The document is unusable; the previous keys stay in force.
    pub fn replace_jwks(&self, jwks: &str, now_ms: u64) -> Result<(), String> {
        match &self.principals {
            Principals::Tokens(tokens) => tokens
                .replace_jwks(jwks, now_ms / 1000)
                .map_err(|refusal| refusal.code),
            #[cfg(feature = "fixtures")]
            Principals::Fixtures(_) => Ok(()),
        }
    }

    /// Whether the keys reached their maximum age and should be refetched.
    #[must_use]
    pub fn jwks_due(&self, now_ms: u64) -> bool {
        match &self.principals {
            Principals::Tokens(tokens) => tokens.refresh_due(now_ms / 1000),
            #[cfg(feature = "fixtures")]
            Principals::Fixtures(_) => false,
        }
    }

    fn is_fenced(&self) -> bool {
        self.tenant
            .serving_host()
            .is_some_and(norn_api::timers::is_fenced)
    }

    /// SYNC-ONE-1: claim the next connection due at `now_ms`.
    pub fn sync_due(&mut self, now_ms: u64) -> Option<String> {
        if self.is_fenced() {
            return None;
        }
        let host = self.tenant.serving_host()?;
        let connect = self.connect.as_mut()?;
        connect.restore_schedule(host).ok()?;
        connect.due(now_ms)
    }

    /// TIMER-ALARM-1: when the alarm should next fire (Unix milliseconds).
    ///
    /// # Errors
    /// A host failure reading the cell.
    pub fn next_due(&mut self, now_ms: u64) -> Result<Option<u64>, String> {
        if self
            .tenant
            .serving_host()
            .is_some_and(norn_api::timers::is_fence_persist_pending)
        {
            return Ok(Some(
                now_ms.saturating_add(norn_api::timers::FENCE_RETRY_MS),
            ));
        }
        let cell = self.tenant.next_due().map_err(|refusal| refusal.code)?;
        Ok(earliest(cell, self.sync_next(now_ms)?))
    }

    /// TIMER-ALARM-1: run the work due at `now_ms`.
    ///
    /// # Errors
    /// A host failure reading the cell.
    pub fn run_due(&mut self, now_ms: u64) -> Result<Due, String> {
        let work = self
            .tenant
            .run_due(now_ms)
            .map_err(|refusal| refusal.code)?;
        Ok(Due {
            committed: work.committed(),
            next: earliest(work.next, self.sync_next(now_ms)?),
            failed: (!work.failed.is_empty()).then(|| work.failed.join("; ")),
        })
    }

    fn sync_next(&mut self, now_ms: u64) -> Result<Option<u64>, String> {
        if self.is_fenced() {
            return Ok(None);
        }
        let (Some(host), Some(connect)) = (self.tenant.serving_host(), self.connect.as_mut())
        else {
            return Ok(None);
        };
        connect.restore_schedule(host)?;
        Ok(connect.next_sync_due(now_ms))
    }

    /// Advance the running sync of `id` with the response to its last plan.
    pub fn sync_step(
        &mut self,
        id: &str,
        response: Option<(u16, &[u8])>,
        now_ms: u64,
        rows: Option<usize>,
    ) -> String {
        match self.connect.as_mut() {
            Some(connect) => connect.step(self.tenant.serving_host(), id, response, (now_ms, rows)),
            None => String::from(r#"{"done":{"outcome":"failed"}}"#),
        }
    }

    /// Continue a request after the host fetched what the cell planned.
    pub fn resume(&mut self, token: &str, status: u16, body: &[u8], now_ms: u64) -> Reply {
        let response = match self.connect.as_mut() {
            Some(connect) => {
                connect.resume(self.tenant.serving_host(), token, status, body, now_ms)
            }
            None => server::Response {
                status: 404,
                body: String::from("{}"),
            },
        };
        Reply::plain(response)
    }
}

fn earliest(left: Option<u64>, right: Option<u64>) -> Option<u64> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (left, None) => left,
        (None, right) => right,
    }
}
