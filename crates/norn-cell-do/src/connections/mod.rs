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
//! - `DELETE /v1/connections/{id}/credentials`: disconnect, locally only;
//!   `?revokeGrant=true` also revokes the grant at the provider, which can
//!   end sibling connections on that grant (GRANT-REVOKE-1), so it is also
//!   decided as `Operation::RevokeConnectionGrant(id)`, authority over every
//!   connection of the cell, before anything is deleted.
//! - `POST /v1/connections/{id}/oauth:start`: `{provider, scopes?, params?}`
//!   → `{authorizeUrl, expiresAtMs, completionKey}`.
//! - `GET /v1/connections/oauth:callback?state=…&code=…`: the provider's
//!   browser redirect. It carries no credential, so it only parks the code
//!   (OAUTH-HANDOFF-1).
//! - `POST /v1/connections/{id}/oauth:complete`: `{state, completionKey}`,
//!   by the principal who started, with its own credential → status.
//!
//! CONN-AUTHZ-1: every route but the callback is decided as
//! `Operation::ManageConnection(id)` by the cell's policy before the vault
//! is opened or a body is read; a denied caller gets exactly what a scope
//! with no cell answers. The callback opens no vault and reads no row.
//! OAUTH-HANDOFF-1: a provider's redirect reaches the callback from the
//! person's browser without the platform credential. An authorized
//! `oauth:start` reserves its state in host memory (keyed by the state's
//! digest, bound to the principal, the connection and the completion key's
//! digest) until [`service::STATE_TTL_MS`] passes. The callback parks a code
//! only for a live reservation: an unknown or expired state is refused and
//! nothing is stored, so callbacks can't fill the store. At most
//! [`MAX_PARKED`] reservations live, at most [`MAX_PER_PRINCIPAL`] per
//! principal (a principal's oldest is dropped for its next), and expired
//! ones are dropped on every connection request. A start is admitted
//! against those bounds before it writes its pending row and sealed
//! verifier, so a refused start stores nothing; an evicted or expired
//! reservation's row and sealed verifier are deleted with it. The first code for a state
//! wins. Nothing is exchanged or stored until the principal who started
//! calls `oauth:complete` with its own credential and the completion key
//! (OAUTH-SESSION-1); a caller whose binding doesn't match spends nothing.
//! The state is then consumed in the transaction that checks that
//! principal, the connection, the key, the expiry and the generation
//! (OAUTH-STATE-1). The client polls `oauth:complete` while it answers
//! `connection.oauth_pending`.
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
use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError};

use norn_api::authz::Operation;
use norn_api::definition_routes::authorize;
use norn_api::host::DevHost;
use norn_api::scope::ScopeQuery;
use norn_cell::engine::{SqlEngine, SqlValue};
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
/// Most authorizations reserved (and codes parked) at once
/// (OAUTH-HANDOFF-1).
pub const MAX_PARKED: usize = 256;
/// Most authorizations one principal has reserved at once
/// (OAUTH-HANDOFF-1).
pub const MAX_PER_PRINCIPAL: usize = 8;

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

/// Host variables as read at startup, every value wiped when dropped
/// (CONN-SECRET-ENV-1): the KEKs and client secrets among them never outlive
/// [`Config::from_env`] in this copy.
pub struct SecretVars(pub BTreeMap<String, String>);

impl Drop for SecretVars {
    fn drop(&mut self) {
        for value in self.0.values_mut() {
            zeroize::Zeroize::zeroize(value);
        }
    }
}

impl std::fmt::Debug for SecretVars {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretVars")
            .field("names", &self.0.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
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
        let vars = SecretVars(
            std::env::vars()
                .filter(|(name, _)| name.starts_with("NORN_"))
                .collect(),
        );
        Self::from_vars(&vars.0, allow_dev_key)
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
        let redirect_url = match vars
            .get(REDIRECT_URL_VAR)
            .filter(|url| !url.trim().is_empty())
        {
            Some(raw) => {
                let trimmed = raw.trim();
                let parsed = url::Url::parse(trimmed)
                    .map_err(|_error| format!("{REDIRECT_URL_VAR} is not a valid URL"))?;
                if !matches!(parsed.scheme(), "https" | "http")
                    || parsed.host().is_none()
                    || !parsed.username().is_empty()
                    || parsed.password().is_some()
                    || parsed.fragment().is_some()
                {
                    return Err(format!(
                        "{REDIRECT_URL_VAR} must be an http(s) URL with a host and no credentials or fragment"
                    ));
                }
                Some(trimmed.to_owned())
            }
            None => None,
        };
        Ok(Some(Self {
            keys: Arc::new(keys),
            egress_allowed: norn_api::sources::allowed_host_list(
                vars.get(norn_api::sources::ALLOWED_HOSTS_VAR)
                    .map_or("", String::as_str),
            )?,
            clients,
            redirect_url,
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
        Connections::new(
            vault,
            egress,
            self.providers.clone(),
            self.clients.clone(),
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

fn query(query: &str) -> (ScopeQuery, BTreeMap<String, String>, bool) {
    let mut scope = ScopeQuery {
        org_id: None,
        environment_id: None,
    };
    let mut rest = BTreeMap::new();
    let mut duplicate_callback = false;
    for (key, value) in form_urlencoded::parse(query.as_bytes()) {
        match key.as_ref() {
            "orgId" => scope.org_id = Some(value.into_owned()),
            "environmentId" => scope.environment_id = Some(value.into_owned()),
            other => {
                if rest.insert(other.to_owned(), value.into_owned()).is_some()
                    && matches!(other, "state" | "code")
                {
                    duplicate_callback = true;
                }
            }
        }
    }
    (scope, rest, duplicate_callback)
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
    completion_key: String,
}

impl Drop for CompleteBody {
    fn drop(&mut self) {
        zeroize::Zeroize::zeroize(&mut self.completion_key);
    }
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

/// One authorization an authorized `oauth:start` reserved
/// (OAUTH-HANDOFF-1).
struct Reservation {
    actor: String,
    connection: String,
    key_hash: String,
    expires_ms: u64,
    code: Option<Secret>,
}

/// Who may claim a reservation (OAUTH-SESSION-1).
pub struct Binding<'a> {
    /// The calling principal.
    pub actor: &'a str,
    /// The connection id of the route.
    pub connection: &'a str,
    /// The completion key the caller presents.
    pub key: &'a str,
}

/// What [`Handoffs::claim`] found.
pub enum Claim {
    /// No live reservation for this binding: unknown, expired, spent or
    /// another session's. Nothing was spent.
    Unknown,
    /// Reserved for this binding, but the provider hasn't redirected yet.
    Waiting,
    /// The parked code, now removed.
    Ready(Secret),
}

/// Authorizations reserved by `oauth:start` and the provider codes parked
/// for them by the callback (OAUTH-HANDOFF-1). Host memory only; never the
/// vault.
#[derive(Default)]
pub struct Handoffs {
    parked: Mutex<Parked>,
}

#[derive(Default)]
struct Parked {
    reserved: BTreeMap<String, Reservation>,
    /// Starts admitted and not yet reserved or abandoned.
    admitted: usize,
}

/// One start admitted by [`Handoffs::admit`] before anything is persisted
/// (OAUTH-HANDOFF-1): it holds a slot until [`Admission::reserve`] fills it
/// or it is dropped.
pub struct Admission<'a> {
    handoffs: &'a Handoffs,
    armed: bool,
}

impl Drop for Admission<'_> {
    fn drop(&mut self) {
        if self.armed {
            let mut parked = self.handoffs.lock();
            parked.admitted = parked.admitted.saturating_sub(1);
        }
    }
}

impl Admission<'_> {
    /// Reserve `state` for `binding` until `expires_ms`. The principal's
    /// oldest reservations beyond [`MAX_PER_PRINCIPAL`] go; their state
    /// digests are returned so their stored records are deleted too.
    #[must_use]
    pub fn reserve(
        mut self,
        state: &str,
        binding: &Binding<'_>,
        expires_ms: u64,
        now_ms: u64,
    ) -> Vec<String> {
        let mut parked = self.handoffs.lock();
        Handoffs::purge(&mut parked.reserved, now_ms);
        let mut own: Vec<(u64, String)> = parked
            .reserved
            .iter()
            .filter(|(_, reservation)| reservation.actor == binding.actor)
            .map(|(hash, reservation)| (reservation.expires_ms, hash.clone()))
            .collect();
        own.sort();
        let excess = own
            .len()
            .saturating_add(1)
            .saturating_sub(MAX_PER_PRINCIPAL);
        let evicted: Vec<String> = own.into_iter().take(excess).map(|(_, hash)| hash).collect();
        for hash in &evicted {
            parked.reserved.remove(hash);
        }
        parked.reserved.insert(
            service::state_hash(state),
            Reservation {
                actor: binding.actor.to_owned(),
                connection: binding.connection.to_owned(),
                key_hash: service::state_hash(binding.key),
                expires_ms,
                code: None,
            },
        );
        parked.admitted = parked.admitted.saturating_sub(1);
        self.armed = false;
        evicted
    }
}

impl Handoffs {
    fn lock(&self) -> std::sync::MutexGuard<'_, Parked> {
        self.parked.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Drop expired reservations; how many went.
    fn purge(reserved: &mut BTreeMap<String, Reservation>, now_ms: u64) -> usize {
        let before = reserved.len();
        reserved.retain(|_, reservation| now_ms < reservation.expires_ms);
        before.saturating_sub(reserved.len())
    }

    /// Drop every expired reservation and its code; how many went.
    pub fn sweep(&self, now_ms: u64) -> usize {
        Self::purge(&mut self.lock().reserved, now_ms)
    }

    /// How many reservations live.
    pub fn len(&self, now_ms: u64) -> usize {
        let mut parked = self.lock();
        Self::purge(&mut parked.reserved, now_ms);
        parked.reserved.len()
    }

    /// Whether none lives.
    pub fn is_empty(&self, now_ms: u64) -> bool {
        self.len(now_ms) == 0
    }

    /// Admit one more start by `actor` before it persists anything: `None`
    /// when [`MAX_PARKED`] reservations and admitted starts live, counting
    /// the one of `actor`'s that its reservation would replace
    /// ([`MAX_PER_PRINCIPAL`]). A refused start changes nothing.
    pub fn admit(&self, actor: &str, now_ms: u64) -> Option<Admission<'_>> {
        let mut parked = self.lock();
        Self::purge(&mut parked.reserved, now_ms);
        let own = parked
            .reserved
            .values()
            .filter(|reservation| reservation.actor == actor)
            .count();
        let replaced = own.saturating_add(1).saturating_sub(MAX_PER_PRINCIPAL);
        let live = parked
            .reserved
            .len()
            .saturating_sub(replaced)
            .saturating_add(parked.admitted);
        if live >= MAX_PARKED {
            return None;
        }
        parked.admitted = parked.admitted.saturating_add(1);
        Some(Admission {
            handoffs: self,
            armed: true,
        })
    }

    /// Park `code` for `state`: `true` only for a live reservation (the
    /// first code wins); an unknown or expired state stores nothing.
    pub fn park(&self, state: &str, code: &str, now_ms: u64) -> bool {
        let mut parked = self.lock();
        Self::purge(&mut parked.reserved, now_ms);
        let Some(reservation) = parked.reserved.get_mut(&service::state_hash(state)) else {
            return false;
        };
        if reservation.code.is_none() {
            reservation.code = Some(Secret::from(code.to_owned()));
        }
        true
    }

    /// Claim the code parked for `state` by `binding` (OAUTH-SESSION-1):
    /// the reservation is checked first and removed only when its code is
    /// handed out.
    pub fn claim(&self, state: &str, binding: &Binding<'_>, now_ms: u64) -> Claim {
        let mut parked = self.lock();
        let reserved = &mut parked.reserved;
        Self::purge(reserved, now_ms);
        let hash = service::state_hash(state);
        let Some(reservation) = reserved.get(&hash) else {
            return Claim::Unknown;
        };
        if reservation.actor != binding.actor
            || reservation.connection != binding.connection
            || reservation.key_hash != service::state_hash(binding.key)
        {
            return Claim::Unknown;
        }
        if reservation.code.is_none() {
            return Claim::Waiting;
        }
        reserved
            .remove(&hash)
            .and_then(|reservation| reservation.code)
            .map_or(Claim::Unknown, Claim::Ready)
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

    /// The reserved authorizations (OAUTH-HANDOFF-1).
    pub const fn handoffs(&self) -> &Handoffs {
        &self.handoffs
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
        // OAUTH-HANDOFF-1: an expired reservation's stored records go with it.
        if self.handoffs.sweep(now_ms) > 0
            && let Some(connections) = self.connections()
        {
            let _ignored = connections.forget_pending(&[], now_ms);
        }
        let (scope, params, duplicate_callback) = query(request.query);
        if let Route::Callback = route {
            return Some(self.callback(request, &params, duplicate_callback, now_ms));
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
        let disconnect = matches!(route, Route::Credentials(_)) && request.method == "DELETE";
        let revoke_grant = match params.get("revokeGrant").map(String::as_str) {
            Some("true") => disconnect,
            None | Some("false") => false,
            Some(_) if disconnect => {
                return Some(refused(&ConnError::Invalid(
                    "revokeGrant is true or false".to_owned(),
                )));
            }
            Some(_) => false,
        };
        // GRANT-REVOKE-1: revoking the grant can end sibling connections,
        // so it is decided at grant scope, before anything is deleted or the
        // vault opens.
        if revoke_grant
            && authorize(
                host,
                &scope,
                Some(&actor),
                Operation::RevokeConnectionGrant(id.to_owned()),
            )
            .is_err()
        {
            return Some(coded(
                403,
                "connection.grant_forbidden",
                "revoking the grant at the provider needs authority over every connection of the cell; disconnect without revokeGrant",
            ));
        }
        if let Err(error) = &self.serving {
            return Some(coded(
                503,
                "connection.unavailable",
                &format!("the connection configuration is malformed: {error}"),
            ));
        }
        let opened = self.opened.get_or_init(|| {
            let opened = (self.open)(&host.scope().key());
            if let Ok(connections) = &opened {
                let _ignored = connections.forget_pending(&[], now_ms);
            }
            opened
        });
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
                .revoke(&actor, id, revoke_grant, now_ms)
                .map(|status| json!(status)),
            (Route::Start(_), _) => self.start(connections, &actor, id, request.body, now_ms),
            (Route::Complete(_) | Route::Callback, _) => {
                return Some(self.complete(connections, &actor, id, request.body, now_ms));
            }
        };
        Some(match result {
            Ok(body) => reply(200, &body),
            Err(error) => refused(&error),
        })
    }

    /// `oauth:start`, its state reserved for the caller (OAUTH-HANDOFF-1)
    /// with the completion key it answers (OAUTH-SESSION-1).
    fn start(
        &self,
        connections: &Connections<E>,
        actor: &str,
        id: &str,
        body: &str,
        now_ms: u64,
    ) -> Result<serde_json::Value, ConnError> {
        let body = serde_json::from_str::<StartBody>(body).map_err(|_parse| {
            ConnError::Invalid("the body is not {provider, scopes?, params?}".to_owned())
        })?;
        // OAUTH-HANDOFF-1: admitted before the pending row and the sealed
        // verifier are written, so a refused start stores nothing.
        let Some(admission) = self.handoffs.admit(actor, now_ms) else {
            return Err(ConnError::Unavailable(
                "too many authorizations are waiting; retry shortly".to_owned(),
            ));
        };
        let started =
            connections.start(actor, id, &body.provider, body.scopes, &body.params, now_ms)?;
        let (Some(state), Some(key)) = (
            started.state.expose_str(),
            started.completion_key.expose_str(),
        ) else {
            return Err(ConnError::BadState);
        };
        let binding = Binding {
            actor,
            connection: id,
            key,
        };
        let evicted = admission.reserve(state, &binding, started.expires_at_ms, now_ms);
        // The evicted and the expired go from the store too; what this
        // misses expires and goes on a later request.
        let _ignored = connections.forget_pending(&evicted, now_ms);
        Ok(json!({
            "authorizeUrl": started.authorize_url,
            "expiresAtMs": started.expires_at_ms,
            "completionKey": key,
        }))
    }

    /// `oauth:complete`: the binding is checked before the parked code is
    /// taken (OAUTH-SESSION-1); pending until the provider redirected.
    fn complete(
        &self,
        connections: &Connections<E>,
        actor: &str,
        id: &str,
        body: &str,
        now_ms: u64,
    ) -> Response {
        let Ok(body) = serde_json::from_str::<CompleteBody>(body) else {
            return refused(&ConnError::Invalid(
                "the body is not {state, completionKey}".to_owned(),
            ));
        };
        let binding = Binding {
            actor,
            connection: id,
            key: &body.completion_key,
        };
        let result = match self.handoffs.claim(&body.state, &binding, now_ms) {
            Claim::Unknown => Err(ConnError::BadState),
            Claim::Waiting => {
                return coded(
                    409,
                    "connection.oauth_pending",
                    "the provider has not redirected back yet; retry",
                );
            }
            Claim::Ready(code) => {
                connections.complete(actor, id, &body.state, &body.completion_key, &code, now_ms)
            }
        };
        match result {
            Ok(status) => reply(200, &json!(status)),
            Err(error) => refused(&error),
        }
    }

    /// OAUTH-HANDOFF-1: park the provider's code; no vault, no row.
    fn callback(
        &self,
        request: &Request<'_>,
        params: &BTreeMap<String, String>,
        duplicate_callback: bool,
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
        if duplicate_callback || !state_shaped(state) || !code_shaped(code) {
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
                400,
                "connection.oauth_state",
                "this authorization is unknown or expired; start connecting again",
            )
        }
    }
}

#[derive(Default)]
pub(super) struct Flight {
    pub(super) done: Mutex<Option<Result<(), ConnError>>>,
    pub(super) ready: Condvar,
}

pub(super) struct FlightGuard<'a> {
    pub(super) flights: &'a Mutex<BTreeMap<String, Arc<Flight>>>,
    pub(super) id: &'a str,
    pub(super) flight: &'a Arc<Flight>,
    pub(super) settled: bool,
}

impl FlightGuard<'_> {
    pub(super) fn finish(&mut self, result: &Result<(), ConnError>) {
        self.flights
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(self.id);
        *self
            .flight
            .done
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(result.clone());
        self.flight.ready.notify_all();
        self.settled = true;
    }
}

impl Drop for FlightGuard<'_> {
    fn drop(&mut self) {
        if !self.settled {
            self.finish(&Err(ConnError::Unavailable("refresh aborted".to_owned())));
        }
    }
}

pub(super) fn sql_ms(value: u64) -> SqlValue {
    SqlValue::Integer(i64::try_from(value).unwrap_or(i64::MAX))
}

pub(super) fn sql_opt_ms(value: Option<u64>) -> SqlValue {
    value.map_or(SqlValue::Null, sql_ms)
}

pub(super) fn sql_opt_text(value: Option<&str>) -> SqlValue {
    value.map_or(SqlValue::Null, |text| SqlValue::Text(text.to_owned()))
}

pub(super) fn sql_text(value: &str) -> SqlValue {
    SqlValue::Text(value.to_owned())
}

pub(super) fn sql_get_text(row: &[SqlValue], index: usize) -> Option<String> {
    match row.get(index) {
        Some(SqlValue::Text(text)) => Some(text.clone()),
        _ => None,
    }
}

pub(super) fn sql_get_int(row: &[SqlValue], index: usize) -> Option<i64> {
    match row.get(index) {
        Some(SqlValue::Integer(value)) => Some(*value),
        _ => None,
    }
}

pub(super) fn sql_get_ms(row: &[SqlValue], index: usize) -> Option<u64> {
    sql_get_int(row, index).and_then(|value| u64::try_from(value).ok())
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
        assert_eq!(
            offenders.len(),
            0,
            "the KEK is named outside the vault: {offenders:?}"
        );
    }
}
