//! Connector credentials on the native host (D126, #1450): the vault, the
//! OAuth authorization-code + PKCE flow, single-flight refresh, and the one
//! egress client every connector call goes through.
//!
//! Native only: the egress client resolves and connects itself, which a
//! Workers Durable Object cannot (its `fetch` is the runtime's). The hosted
//! wiring follows on the same types.
//!
//! - [`vault`]: per-cell envelope encryption under a host KEK.
//! - [`egress`]: pinned-address, redirect-safe outbound HTTP.
//! - [`providers`]: OAuth metadata generated from `OpenConnector` v1.9.4.
//! - [`service`]: credentials, status, OAuth, refresh, audit.
//!
//! Routes ([`Routes`]; scoped by `orgId` and `environmentId` like every
//! other):
//! - `GET /v1/connections/{id}/credentials`: [`service::Status`] only.
//! - `PUT /v1/connections/{id}/credentials`: [`service::SetCredentials`].
//! - `DELETE /v1/connections/{id}/credentials`: disconnect and revoke.
//! - `POST /v1/connections/{id}/oauth:start`: `{provider, scopes?, params?}`
//!   → `{authorizeUrl, expiresAtMs}`.
//! - `GET /v1/connections/oauth:callback?state=…&code=…`: the provider's
//!   browser redirect. It carries no credential, so it only parks the code
//!   (OAUTH-HANDOFF-1).
//! - `POST /v1/connections/{id}/oauth:complete`: `{state}`, by the principal
//!   who started, with its own credential → status.
//!
//! CONN-AUTHZ-1: every route but the callback is decided as
//! `Operation::ManageConnection(id)` by the cell's policy before the vault
//! is opened or a body is read; a denied caller gets exactly what a scope
//! with no cell answers. The callback opens no vault and reads no row.
//! OAUTH-HANDOFF-1: a provider's redirect reaches the callback from the
//! person's browser without the platform credential. The callback checks
//! only the shape of `state` and `code` and parks the code in host memory,
//! keyed by the state's digest, for at most [`service::STATE_TTL_MS`]; the
//! first code for a state wins and at most [`MAX_PARKED`] wait. Nothing is
//! exchanged or stored until the principal who started calls
//! `oauth:complete` with its own credential; the state is then consumed in
//! the transaction that checks that principal, the connection, the expiry
//! and the generation (OAUTH-STATE-1). The client polls `oauth:complete`
//! while it answers `connection.oauth_pending`.
//! CONN-CONFIG-1: provider OAuth clients are host configuration
//! (`NORN_OAUTH_<PROVIDER>_CLIENT_ID` / `_CLIENT_SECRET`, the provider id
//! upper-cased with `.` as `_`), and so is the callback URL
//! (`NORN_OAUTH_REDIRECT_URL`); never request input.
//! CONN-SECRET-ENV-1: this module reads the vault and OAuth variables
//! ([`Config::from_env`]); a host spawning a child process removes every
//! variable [`secret_var`] names from the child's environment.

pub mod egress;
pub mod providers;
pub mod service;
pub mod vault;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use norn_api::authz::Operation;
use norn_api::definition_routes::authorize;
use norn_api::host::DevHost;
use norn_api::scope::ScopeQuery;
use norn_cell::engine::SqlEngine;
use serde::Deserialize;
use serde_json::json;

use crate::principal::{PrincipalResolver, RequestScope};
use crate::server::{self, Request, Response};
use egress::{Egress, Lookup};
use service::{ClientCredentials, ConnError, Connections, SetCredentials};
use vault::{KeyWrap, LocalKeyring, Secret, Vault, VaultError};

/// The callback URL providers redirect to.
pub const REDIRECT_URL_VAR: &str = "NORN_OAUTH_REDIRECT_URL";
/// The callback route.
pub const CALLBACK_PATH: &str = "/v1/connections/oauth:callback";
/// Every OAuth client variable starts with this.
pub const OAUTH_VAR_PREFIX: &str = "NORN_OAUTH_";
/// Most provider codes parked at once (OAUTH-HANDOFF-1).
pub const MAX_PARKED: usize = 256;

/// The variable naming `provider`'s OAuth client id or secret (`suffix`).
#[must_use]
pub fn client_var(provider: &str, suffix: &str) -> String {
    format!(
        "{OAUTH_VAR_PREFIX}{}_{suffix}",
        provider.to_ascii_uppercase().replace(['.', '-'], "_")
    )
}

/// Whether `name` is a vault or OAuth client variable, which a host never
/// passes to a child process (CONN-SECRET-ENV-1).
#[must_use]
pub fn secret_var(name: &str) -> bool {
    name.starts_with(vault::VAR_PREFIX) || name.starts_with(OAUTH_VAR_PREFIX)
}

/// Everything a host reads from its configuration to serve connections.
pub struct Config {
    keys: Arc<dyn KeyWrap>,
    egress_allowed: Vec<String>,
    clients: BTreeMap<String, ClientCredentials>,
    redirect_url: Option<String>,
    providers: BTreeMap<String, providers::Provider>,
}

impl Config {
    /// [`Self::from_vars`] over this process's `NORN_*` environment
    /// (CONN-SECRET-ENV-1).
    ///
    /// # Errors
    /// As [`Self::from_vars`].
    pub fn from_env(allow_dev_key: bool) -> Result<Option<Self>, String> {
        let vars: BTreeMap<String, String> = std::env::vars()
            .filter(|(name, _)| name.starts_with("NORN_"))
            .collect();
        Self::from_vars(&vars, allow_dev_key)
    }

    /// Read the host's variables; `None` when no vault KEK is configured
    /// (the host then serves no connection route). `allow_dev_key` admits
    /// the development KEK (`norn dev` only).
    ///
    /// # Errors
    /// A malformed variable, named; a configured egress proxy
    /// (EGRESS-PROXY-1).
    pub fn from_vars(
        vars: &BTreeMap<String, String>,
        allow_dev_key: bool,
    ) -> Result<Option<Self>, String> {
        egress::refuse_proxy(vars.get(egress::PROXY_VAR).map(String::as_str))
            .map_err(|e| e.to_string())?;
        let Some(keys) = LocalKeyring::from_vars(vars, allow_dev_key).map_err(|e| e.to_string())?
        else {
            return Ok(None);
        };
        let providers = providers::load()?;
        let clients = providers
            .keys()
            .filter_map(|id| {
                let client_id = vars.get(&client_var(id, "CLIENT_ID"))?.trim().to_owned();
                let client_secret = vars
                    .get(&client_var(id, "CLIENT_SECRET"))
                    .filter(|secret| !secret.is_empty())
                    .map(|secret| Secret::from(secret.clone()));
                (!client_id.is_empty()).then(|| {
                    (
                        id.clone(),
                        ClientCredentials {
                            client_id,
                            client_secret,
                        },
                    )
                })
            })
            .collect();
        Ok(Some(Self {
            keys: Arc::new(keys),
            egress_allowed: norn_api::sources::allowed_host_list(
                vars.get(norn_api::sources::ALLOWED_HOSTS_VAR)
                    .map_or("", String::as_str),
            )?,
            clients,
            redirect_url: vars
                .get(REDIRECT_URL_VAR)
                .cloned()
                .filter(|url| !url.is_empty()),
            providers,
        }))
    }

    /// The connections of the cell `scope` over `engine` (its storage),
    /// resolving names with `lookup`. Rewraps DEKs under a retired KEK id.
    ///
    /// # Errors
    /// Vault or configuration failures.
    pub fn open<E: SqlEngine>(
        &self,
        engine: E,
        scope: &str,
        lookup: Arc<dyn Lookup>,
    ) -> Result<Connections<E>, String> {
        let vault =
            Vault::open(engine, scope, Arc::clone(&self.keys)).map_err(|e| e.to_string())?;
        vault.rewrap().map_err(|e| e.to_string())?;
        let egress = Egress::new(self.egress_allowed.clone(), lookup);
        let clients = self
            .clients
            .iter()
            .map(|(id, client)| {
                (
                    id.clone(),
                    ClientCredentials {
                        client_id: client.client_id.clone(),
                        client_secret: client.client_secret.clone(),
                    },
                )
            })
            .collect();
        Connections::new(
            vault,
            egress,
            self.providers.clone(),
            clients,
            self.redirect_url.clone(),
        )
        .map_err(|e| e.to_string())
    }
}

#[derive(Clone, Copy)]
enum Route<'a> {
    Credentials(&'a str),
    Start(&'a str),
    Complete(&'a str),
    Callback,
}

fn route_of(path: &str) -> Option<Route<'_>> {
    if path == CALLBACK_PATH {
        return Some(Route::Callback);
    }
    let rest = path.strip_prefix("/v1/connections/")?;
    if let Some(id) = rest.strip_suffix("/credentials") {
        return Some(Route::Credentials(id));
    }
    if let Some(id) = rest.strip_suffix("/oauth:complete") {
        return Some(Route::Complete(id));
    }
    rest.strip_suffix("/oauth:start").map(Route::Start)
}

fn query(query: &str) -> (ScopeQuery, BTreeMap<String, String>) {
    let mut scope = ScopeQuery {
        org_id: None,
        environment_id: None,
    };
    let mut rest = BTreeMap::new();
    for (key, value) in form_urlencoded::parse(query.as_bytes()) {
        match key.as_ref() {
            "orgId" => scope.org_id = Some(value.into_owned()),
            "environmentId" => scope.environment_id = Some(value.into_owned()),
            other => {
                rest.insert(other.to_owned(), value.into_owned());
            }
        }
    }
    (scope, rest)
}

fn reply(status: u16, body: &serde_json::Value) -> Response {
    Response {
        status,
        body: body.to_string(),
    }
}

fn coded(status: u16, code: &str, message: &str) -> Response {
    reply(status, &json!({"code": code, "message": message}))
}

fn refused(error: &ConnError) -> Response {
    let (status, code) = match error {
        ConnError::Invalid(_) => (400, "connection.invalid"),
        ConnError::Unsupported(_) => (400, "connection.provider_unsupported"),
        ConnError::NotFound => (404, "connection.not_found"),
        ConnError::ReconnectNeeded => (409, "connection.reconnect_needed"),
        ConnError::Conflict => (409, "connection.conflict"),
        ConnError::BadState => (400, "connection.oauth_state"),
        ConnError::Provider(_) => (502, "connection.provider_refused"),
        ConnError::Unavailable(_) => (503, "connection.unavailable"),
        ConnError::Egress(_) => (502, "connection.egress"),
        ConnError::Vault(VaultError::Integrity | VaultError::UnknownKey(_)) => {
            (500, "connection.vault_key")
        }
        ConnError::Vault(_) => (500, "connection.vault"),
    };
    // CONN-ERROR-1: the message is local text or a fixed code.
    coded(status, code, &error.to_string())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StartBody {
    provider: String,
    #[serde(default)]
    scopes: Option<Vec<String>>,
    #[serde(default)]
    params: BTreeMap<String, String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CompleteBody {
    state: String,
}

/// An OAuth `state` as `oauth2` mints it: 1-128 URL-safe characters.
fn state_shaped(state: &str) -> bool {
    (1..=128).contains(&state.len())
        && state
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

/// A provider's authorization code: 1-2048 visible ASCII characters.
fn code_shaped(code: &str) -> bool {
    (1..=2048).contains(&code.len()) && code.bytes().all(|b| b.is_ascii_graphic())
}

/// Provider codes parked by the callback until the starting principal
/// completes (OAUTH-HANDOFF-1). Host memory only; never the vault.
#[derive(Default)]
pub struct Handoffs {
    parked: Mutex<BTreeMap<String, (Secret, u64)>>,
}

impl Handoffs {
    fn purge(parked: &mut BTreeMap<String, (Secret, u64)>, now_ms: u64) {
        parked.retain(|_, (_, at)| now_ms < at.saturating_add(service::STATE_TTL_MS));
    }

    /// Park `code` for `state`; the first code for a state wins. `false`
    /// when [`MAX_PARKED`] codes already wait.
    pub fn park(&self, state: &str, code: &str, now_ms: u64) -> bool {
        let mut parked = self.parked.lock().unwrap_or_else(PoisonError::into_inner);
        Self::purge(&mut parked, now_ms);
        let hash = service::state_hash(state);
        if parked.contains_key(&hash) {
            return true;
        }
        if parked.len() >= MAX_PARKED {
            return false;
        }
        parked.insert(hash, (Secret::from(code.to_owned()), now_ms));
        true
    }

    /// Take the code parked for `state`, if one is and it is fresh.
    pub fn take(&self, state: &str, now_ms: u64) -> Option<Secret> {
        let mut parked = self.parked.lock().unwrap_or_else(PoisonError::into_inner);
        Self::purge(&mut parked, now_ms);
        parked
            .remove(&service::state_hash(state))
            .map(|(code, _)| code)
    }
}

type Opener<E> = Box<dyn Fn(&str) -> Result<Connections<E>, String> + Send + Sync>;

/// A native host's connection routes: authorization first (CONN-AUTHZ-1),
/// the vault opened on the first authorized request, the callback handoff
/// in memory (OAUTH-HANDOFF-1).
pub struct Routes<E: SqlEngine> {
    serving: Result<bool, String>,
    open: Opener<E>,
    opened: OnceLock<Result<Connections<E>, String>>,
    handoffs: Handoffs,
}

impl<E: SqlEngine> Routes<E> {
    /// Routes over `config` (from [`Config::from_env`] or
    /// [`Config::from_vars`]): none when it is `Ok(None)`; its error answered
    /// to authorized callers when it is malformed. `engine` opens the cell's
    /// storage; names resolve with `lookup`.
    pub fn new(
        config: Result<Option<Config>, String>,
        engine: impl Fn() -> Result<E, String> + Send + Sync + 'static,
        lookup: Arc<dyn Lookup>,
    ) -> Self {
        match config {
            Ok(Some(config)) => {
                Self::with_opener(move |scope| config.open(engine()?, scope, Arc::clone(&lookup)))
            }
            Ok(None) => Self {
                serving: Ok(false),
                open: Box::new(|_| Err("no connections".to_owned())),
                opened: OnceLock::new(),
                handoffs: Handoffs::default(),
            },
            Err(error) => Self {
                serving: Err(error),
                open: Box::new(|_| Err("no connections".to_owned())),
                opened: OnceLock::new(),
                handoffs: Handoffs::default(),
            },
        }
    }

    /// Routes over the connections `open` returns for a cell scope key,
    /// called once, on the first authorized request.
    pub fn with_opener(
        open: impl Fn(&str) -> Result<Connections<E>, String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            serving: Ok(true),
            open: Box::new(open),
            opened: OnceLock::new(),
            handoffs: Handoffs::default(),
        }
    }

    /// Whether the vault has been opened (by an authorized request).
    pub fn vault_opened(&self) -> bool {
        self.opened.get().is_some()
    }

    /// The opened connections, once an authorized request opened them.
    pub fn connections(&self) -> Option<&Connections<E>> {
        self.opened.get().and_then(|opened| opened.as_ref().ok())
    }

    /// Route `request` if it is a connection route; `None` otherwise.
    pub fn route<H: SqlEngine>(
        &self,
        host: &DevHost<H>,
        principals: &dyn PrincipalResolver,
        request: &Request<'_>,
        now_ms: u64,
    ) -> Option<Response> {
        let route = route_of(request.path)?;
        if self.serving == Ok(false) {
            return None;
        }
        let _request = RequestScope::new(principals);
        let (scope, params) = query(request.query);
        if let Route::Callback = route {
            return Some(self.callback(request, &params, now_ms));
        }
        let (Route::Credentials(id) | Route::Start(id) | Route::Complete(id)) = route else {
            return Some(server::outside::<H>(request));
        };
        let method_ok = matches!(
            (&route, request.method),
            (Route::Credentials(_), "GET" | "PUT" | "DELETE")
                | (Route::Start(_) | Route::Complete(_), "POST")
        );
        let actor = principals.resolve(request.authorization, request.now);
        // CONN-AUTHZ-1: decided before the vault opens, answered like a
        // scope with no cell.
        let allowed = method_ok
            && service::valid_id(id)
            && authorize(
                host,
                &scope,
                actor.as_deref(),
                Operation::ManageConnection(id.to_owned()),
            )
            .is_ok();
        let (true, Some(actor)) = (allowed, actor) else {
            return Some(server::outside::<H>(request));
        };
        if let Err(error) = &self.serving {
            return Some(coded(
                503,
                "connection.unavailable",
                &format!("the connection configuration is malformed: {error}"),
            ));
        }
        let opened = self.opened.get_or_init(|| (self.open)(&host.scope().key()));
        let connections = match opened {
            Ok(connections) => connections,
            Err(error) => {
                return Some(coded(
                    503,
                    "connection.unavailable",
                    &format!("the connection vault did not open: {error}"),
                ));
            }
        };
        let result = match (route, request.method) {
            (Route::Credentials(_), "GET") => connections.status(id).map(|status| json!(status)),
            (Route::Credentials(_), "PUT") => serde_json::from_str::<SetCredentials>(request.body)
                .map_err(|_parse| {
                    ConnError::Invalid("the body is not a credentials object".to_owned())
                })
                .and_then(|body| connections.set(&actor, id, body, now_ms))
                .map(|status| json!(status)),
            (Route::Credentials(_), _) => connections
                .revoke(&actor, id, now_ms)
                .map(|status| json!(status)),
            (Route::Start(_), _) => serde_json::from_str::<StartBody>(request.body)
                .map_err(|_parse| {
                    ConnError::Invalid("the body is not {provider, scopes?, params?}".to_owned())
                })
                .and_then(|body| {
                    connections.start(
                        &actor,
                        id,
                        &body.provider,
                        body.scopes,
                        &body.params,
                        now_ms,
                    )
                })
                .map(|(url, expires)| json!({"authorizeUrl": url, "expiresAtMs": expires})),
            (Route::Complete(_) | Route::Callback, _) => {
                let Ok(body) = serde_json::from_str::<CompleteBody>(request.body) else {
                    return Some(refused(&ConnError::Invalid(
                        "the body is not {state}".to_owned(),
                    )));
                };
                let Some(code) = self.handoffs.take(&body.state, now_ms) else {
                    return Some(coded(
                        409,
                        "connection.oauth_pending",
                        "the provider has not redirected back yet; retry",
                    ));
                };
                connections
                    .complete(&actor, id, &body.state, &code, now_ms)
                    .map(|status| json!(status))
            }
        };
        Some(match result {
            Ok(body) => reply(200, &body),
            Err(error) => refused(&error),
        })
    }

    /// OAUTH-HANDOFF-1: park the provider's code; no vault, no row.
    fn callback(
        &self,
        request: &Request<'_>,
        params: &BTreeMap<String, String>,
        now_ms: u64,
    ) -> Response {
        if request.method != "GET" {
            return server::outside::<E>(request);
        }
        let (Some(state), Some(code)) = (params.get("state"), params.get("code")) else {
            // The person declined at the provider, or the redirect is malformed.
            return coded(
                400,
                "connection.oauth_callback",
                "the provider did not return an authorization code",
            );
        };
        if !state_shaped(state) || !code_shaped(code) {
            return coded(
                400,
                "connection.oauth_callback",
                "the provider did not return an authorization code",
            );
        }
        if self.handoffs.park(state, code, now_ms) {
            coded(
                200,
                "connection.oauth_received",
                "Return to Norn to finish connecting.",
            )
        } else {
            coded(
                503,
                "connection.unavailable",
                "too many authorizations are waiting; retry shortly",
            )
        }
    }
}

#[cfg(test)]
mod tests {
    /// VAULT-KEK-LOAD-1: only the vault module names the KEK variables.
    #[test]
    fn only_the_vault_reads_the_kek() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut stack = vec![root];
        let mut offenders = Vec::new();
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).expect("src") {
                let path = entry.expect("entry").path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|ext| ext == "rs")
                    && !path.ends_with("connections/vault.rs")
                {
                    let text = std::fs::read_to_string(&path).expect("source");
                    if text.contains(concat!("NORN_", "VAULT_KEK")) {
                        offenders.push(path.display().to_string());
                    }
                }
            }
        }
        assert!(
            offenders.is_empty(),
            "the KEK is named outside the vault: {offenders:?}"
        );
    }
}
