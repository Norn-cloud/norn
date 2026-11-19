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
//! - [`providers`]: OAuth metadata generated from OpenConnector v1.9.4.
//! - [`service`]: credentials, status, OAuth, refresh, audit.
//!
//! Routes (scoped by `orgId` and `environmentId` like every other):
//! - `GET /v1/connections/{id}/credentials`: [`service::Status`] only.
//! - `PUT /v1/connections/{id}/credentials`: [`service::SetCredentials`].
//! - `DELETE /v1/connections/{id}/credentials`: revoke.
//! - `POST /v1/connections/{id}/oauth:start`: `{provider, scopes?, params?}`
//!   → `{authorizeUrl, expiresAtMs}`.
//! - `GET /v1/connections/oauth:callback?state=…&code=…`: the provider's
//!   redirect, on the platform domain, carrying the user's own credential.
//!
//! CONN-AUTHZ-1: every route is decided as `Operation::ManageConnection` by
//! the cell's policy before its body is read; a denied caller gets exactly
//! what a scope with no cell answers. The callback is decided for the
//! connection its state names, and the state must be the caller's own.
//! CONN-CONFIG-1: provider OAuth clients are host configuration
//! (`NORN_OAUTH_<PROVIDER>_CLIENT_ID` / `_CLIENT_SECRET`, the provider id
//! upper-cased with `.` as `_`), and so is the callback URL
//! (`NORN_OAUTH_REDIRECT_URL`); never request input.

pub mod egress;
pub mod providers;
pub mod service;
pub mod vault;

use std::collections::BTreeMap;
use std::sync::Arc;

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

/// The variable naming `provider`'s OAuth client id or secret (`suffix`).
#[must_use]
pub fn client_var(provider: &str, suffix: &str) -> String {
    format!(
        "NORN_OAUTH_{}_{suffix}",
        provider.to_ascii_uppercase().replace(['.', '-'], "_")
    )
}

/// Everything a host reads from its configuration to serve connections.
pub struct Config {
    keys: Arc<dyn KeyWrap>,
    egress_allowed: Vec<String>,
    proxy: Option<String>,
    clients: BTreeMap<String, ClientCredentials>,
    redirect_url: Option<String>,
    providers: BTreeMap<String, providers::Provider>,
}

impl Config {
    /// Read the host's variables; `None` when no vault KEK is configured
    /// (the host then serves no connection route). `allow_dev_key` admits
    /// the development KEK (`norn dev` only).
    ///
    /// # Errors
    /// A malformed variable, named.
    pub fn from_vars(
        vars: &BTreeMap<String, String>,
        allow_dev_key: bool,
    ) -> Result<Option<Self>, String> {
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
            proxy: vars.get(egress::PROXY_VAR).cloned(),
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
        let egress = Egress::new(self.egress_allowed.clone(), self.proxy.as_deref(), lookup)
            .map_err(|e| e.to_string())?;
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

enum Route<'a> {
    Credentials(&'a str),
    Start(&'a str),
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

fn refused(error: &ConnError) -> Response {
    let (status, code) = match error {
        ConnError::Invalid(_) => (400, "connection.invalid"),
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
    reply(status, &json!({"code": code, "message": error.to_string()}))
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

impl<E: SqlEngine> Connections<E> {
    /// Route `request` if it is a connection route; `None` otherwise.
    pub fn route(
        &self,
        host: &DevHost<E>,
        principals: &dyn PrincipalResolver,
        request: &Request<'_>,
        now_ms: u64,
    ) -> Option<Response> {
        let route = route_of(request.path)?;
        let _request = RequestScope::new(principals);
        let (scope, params) = query(request.query);
        let actor = principals.resolve(request.authorization, request.now);
        let target = match &route {
            Route::Credentials(id) | Route::Start(id) => Some((*id).to_owned()),
            Route::Callback => params
                .get("state")
                .and_then(|state| self.pending_connection(state).ok().flatten()),
        };
        let method_ok = matches!(
            (&route, request.method),
            (Route::Credentials(_), "GET" | "PUT" | "DELETE")
                | (Route::Start(_), "POST")
                | (Route::Callback, "GET")
        );
        // CONN-AUTHZ-1: decided first, answered like a scope with no cell.
        let allowed = method_ok
            && target.as_deref().is_some_and(service::valid_id)
            && target.as_ref().is_some_and(|id| {
                authorize(
                    host,
                    &scope,
                    actor.as_deref(),
                    Operation::ManageConnection(id.clone()),
                )
                .is_ok()
            });
        let (true, Some(actor), Some(id)) = (allowed, actor, target) else {
            return Some(server::outside::<E>(request));
        };
        let result = match (route, request.method) {
            (Route::Credentials(_), "GET") => self.status(&id).map(|status| (200, json!(status))),
            (Route::Credentials(_), "PUT") => serde_json::from_str::<SetCredentials>(request.body)
                .map_err(|_| ConnError::Invalid("the body is not a credentials object".to_owned()))
                .and_then(|body| self.set(&actor, &id, body, now_ms))
                .map(|status| (200, json!(status))),
            (Route::Credentials(_), _) => self
                .revoke(&actor, &id, now_ms)
                .map(|status| (200, json!(status))),
            (Route::Start(_), _) => serde_json::from_str::<StartBody>(request.body)
                .map_err(|_| {
                    ConnError::Invalid("the body is not {provider, scopes?, params?}".to_owned())
                })
                .and_then(|body| {
                    self.start(
                        &actor,
                        &id,
                        &body.provider,
                        body.scopes,
                        body.params,
                        now_ms,
                    )
                })
                .map(|(url, expires)| (200, json!({"authorizeUrl": url, "expiresAtMs": expires}))),
            (Route::Callback, _) => match (params.get("state"), params.get("code")) {
                (Some(state), Some(code)) => self
                    .callback(&actor, state, code, now_ms)
                    .map(|status| (200, json!(status))),
                _ => Err(ConnError::BadState),
            },
        };
        Some(match result {
            Ok((status, body)) => reply(status, &body),
            Err(error) => refused(&error),
        })
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
