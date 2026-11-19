//! One cell's connections: credentials in the vault, status beside them, the
//! OAuth authorization-code flow, refresh and the audit trail.
//!
//! CONN-WRITE-ONLY-1: credentials go in ([`Connections::set`], the OAuth
//! callback) and are used by connector calls ([`Connections::access_token`]);
//! nothing returns them. [`Status`] carries only `last4`, scopes, expiry,
//! last use and last error.
//! CONN-AUDIT-1: every set, use, refresh, revoke, OAuth start/connect and
//! failure appends one `_norn_connection_audit` row: who, when, which
//! connection, what happened. Never a credential, a code or a verifier.
//! OAUTH-STATE-1: an authorization starts with a random state bound to the
//! starting principal, the connection and the cell, kept hashed for ten
//! minutes with its PKCE (S256) verifier sealed in the vault. The callback
//! consumes it once: a second use, an expired state, or another principal is
//! refused and nothing is exchanged.
//! REFRESH-ONE-1: refreshes of one connection are single-flight in the
//! owning cell (the cell's host is its single writer): concurrent callers
//! wait for the one in flight and read its result, so one token-endpoint
//! call is made. The result is written only if the connection's revision is
//! still the one the refresh read (a revoke or a new credential wins);
//! rotated refresh tokens are saved in that same write. A refused refresh
//! (an OAuth error answer) marks the connection `reconnect_needed`.

use std::collections::BTreeMap;
use std::fmt;
use std::fmt::Write as _;
use std::sync::{Arc, Condvar, Mutex, PoisonError};

use norn_cell::engine::{SqlEngine, SqlValue};
use oauth2::basic::BasicClient;
use oauth2::{
    AuthType, AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, PkceCodeChallenge,
    PkceCodeVerifier, RedirectUrl, RefreshToken, RequestTokenError, RevocationUrl, Scope,
    StandardRevocableToken, TokenResponse as _, TokenUrl,
};
use serde::{Deserialize, Serialize};
use sha2::Digest as _;

use super::egress::{Egress, EgressError};
use super::providers::{ClientAuth, Provider};
use super::vault::{Secret, Vault, VaultError};

/// One audit row: when (Unix ms), event, actor, detail.
pub type AuditRow = (u64, String, String, Option<String>);

/// How long an OAuth state lives.
pub const STATE_TTL_MS: u64 = 10 * 60 * 1000;
/// An access token this close to expiry is refreshed before use.
pub const REFRESH_SKEW_MS: u64 = 60 * 1000;

/// A connection's state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    /// Credentials are stored and usable.
    Connected,
    /// The provider refused a refresh; a person must connect again.
    ReconnectNeeded,
}

impl State {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Connected => "connected",
            Self::ReconnectNeeded => "reconnect_needed",
        }
    }
}

/// What `GET /v1/connections/{id}/credentials` answers: status, never a
/// credential.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// The connection id.
    pub id: String,
    /// The provider, when connected.
    pub provider: Option<String>,
    /// `connected`, `reconnect_needed` or `not_connected`.
    pub state: String,
    /// The credential's last four characters.
    pub last4: Option<String>,
    /// Granted scopes.
    pub scopes: Vec<String>,
    /// When the access token expires (Unix ms).
    pub expires_at_ms: Option<u64>,
    /// When a connector call last used it (Unix ms).
    pub last_used_ms: Option<u64>,
    /// The last failure's code, never a provider body.
    pub last_error: Option<String>,
}

/// Why a connection call refused. No variant carries secret material.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnError {
    /// 400: the request is malformed (the message says how).
    Invalid(String),
    /// 404: no such connection or provider.
    NotFound,
    /// 409: the connection must be connected again.
    ReconnectNeeded,
    /// 409: another writer changed the connection first.
    Conflict,
    /// 400: the OAuth state is unknown, used, expired or another principal's.
    BadState,
    /// 502: the provider refused or failed (an OAuth error code, or why).
    Provider(String),
    /// 503: the host has no vault key or provider client configured.
    Unavailable(String),
    /// 500.
    Vault(VaultError),
    /// The egress client refused or failed.
    Egress(EgressError),
}

impl fmt::Display for ConnError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid(why) | Self::Unavailable(why) => write!(f, "{why}"),
            Self::NotFound => f.write_str("no such connection"),
            Self::ReconnectNeeded => f.write_str("the connection must be connected again"),
            Self::Conflict => f.write_str("the connection changed; retry"),
            Self::BadState => f.write_str("the authorization is unknown, used or expired"),
            Self::Provider(why) => write!(f, "the provider refused: {why}"),
            Self::Vault(error) => error.fmt(f),
            Self::Egress(error) => error.fmt(f),
        }
    }
}

impl From<VaultError> for ConnError {
    fn from(error: VaultError) -> Self {
        Self::Vault(error)
    }
}

impl From<norn_cell::error::CellError> for ConnError {
    fn from(error: norn_cell::error::CellError) -> Self {
        storage(error)
    }
}

fn storage(error: impl fmt::Display) -> ConnError {
    ConnError::Vault(VaultError::Storage(error.to_string()))
}

/// A provider OAuth client registered by the host.
pub struct ClientCredentials {
    /// The client id.
    pub client_id: String,
    /// The client secret (none for a public client).
    pub client_secret: Option<Secret>,
}

/// `PUT /v1/connections/{id}/credentials`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SetCredentials {
    /// The provider id.
    pub provider: String,
    /// The access token or API key.
    pub access_token: String,
    /// A refresh token, if the provider issued one.
    #[serde(default)]
    pub refresh_token: Option<String>,
    /// When the access token expires (Unix ms).
    #[serde(default)]
    pub expires_at_ms: Option<u64>,
    /// Granted scopes.
    #[serde(default)]
    pub scopes: Vec<String>,
    /// Provider URL parameters (Shopify's `shopSubdomain`).
    #[serde(default)]
    pub params: BTreeMap<String, String>,
}

impl fmt::Debug for SetCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SetCredentials")
            .field("provider", &self.provider)
            .field("tokens", &"[redacted]")
            .finish_non_exhaustive()
    }
}

/// What the vault seals for a connection.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Tokens {
    access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refresh_token: Option<String>,
}

impl Drop for Tokens {
    fn drop(&mut self) {
        // Best effort: overwrite before the allocation is freed.
        let wipe = |text: &mut String| {
            let mut bytes = std::mem::take(text).into_bytes();
            bytes.fill(0);
        };
        wipe(&mut self.access_token);
        if let Some(refresh) = self.refresh_token.as_mut() {
            wipe(refresh);
        }
    }
}

#[derive(Debug, Clone)]
struct Record {
    provider: String,
    state: State,
    last4: Option<String>,
    scopes: Vec<String>,
    expires_at_ms: Option<u64>,
    last_used_ms: Option<u64>,
    last_error: Option<String>,
    revision: i64,
    params: BTreeMap<String, String>,
}

const DDL: &str = r#"
CREATE TABLE IF NOT EXISTS "_norn_connection" (
  "id" TEXT PRIMARY KEY,
  "provider" TEXT NOT NULL,
  "state" TEXT NOT NULL,
  "last4" TEXT,
  "scopes" TEXT NOT NULL,
  "expires_at_ms" INTEGER,
  "last_used_ms" INTEGER,
  "last_error" TEXT,
  "revision" INTEGER NOT NULL,
  "params" TEXT NOT NULL,
  "updated_ms" INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS "_norn_connection_audit" (
  "seq" INTEGER PRIMARY KEY,
  "at_ms" INTEGER NOT NULL,
  "connection" TEXT NOT NULL,
  "event" TEXT NOT NULL,
  "actor" TEXT NOT NULL,
  "detail" TEXT
);
CREATE TABLE IF NOT EXISTS "_norn_oauth_state" (
  "state_hash" TEXT PRIMARY KEY,
  "connection" TEXT NOT NULL,
  "provider" TEXT NOT NULL,
  "actor" TEXT NOT NULL,
  "scopes" TEXT NOT NULL,
  "params" TEXT NOT NULL,
  "expires_ms" INTEGER NOT NULL
);
"#;

fn ms(value: u64) -> SqlValue {
    SqlValue::Integer(i64::try_from(value).unwrap_or(i64::MAX))
}

fn opt_ms(value: Option<u64>) -> SqlValue {
    value.map_or(SqlValue::Null, ms)
}

fn opt_text(value: Option<&str>) -> SqlValue {
    value.map_or(SqlValue::Null, |text| SqlValue::Text(text.to_owned()))
}

fn get_text(row: &[SqlValue], index: usize) -> Option<String> {
    match row.get(index) {
        Some(SqlValue::Text(text)) => Some(text.clone()),
        _ => None,
    }
}

fn get_int(row: &[SqlValue], index: usize) -> Option<i64> {
    match row.get(index) {
        Some(SqlValue::Integer(value)) => Some(*value),
        _ => None,
    }
}

fn get_ms(row: &[SqlValue], index: usize) -> Option<u64> {
    get_int(row, index).and_then(|value| u64::try_from(value).ok())
}

fn last4(token: &str) -> Option<String> {
    let chars: Vec<char> = token.chars().collect();
    let start = chars.len().checked_sub(4).filter(|_| chars.len() >= 12)?;
    chars.get(start..).map(|tail| tail.iter().collect())
}

fn slot(id: &str) -> String {
    format!("connection:{id}")
}

fn state_hash(state: &str) -> String {
    let digest = sha2::Sha256::digest(state.as_bytes());
    digest.iter().fold(String::new(), |mut hex, byte| {
        let _infallible = write!(hex, "{byte:02x}");
        hex
    })
}

/// A connection id: 1-64 of `[A-Za-z0-9._-]`.
#[must_use]
pub fn valid_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

#[derive(Default)]
struct Flight {
    done: Mutex<Option<Result<(), ConnError>>>,
    ready: Condvar,
}

/// One cell's connections.
pub struct Connections<E: SqlEngine> {
    vault: Mutex<Vault<E>>,
    egress: Egress,
    providers: BTreeMap<String, Provider>,
    clients: BTreeMap<String, ClientCredentials>,
    redirect_url: Option<String>,
    flights: Mutex<BTreeMap<String, Arc<Flight>>>,
}

type Client = BasicClient<
    oauth2::EndpointSet,
    oauth2::EndpointNotSet,
    oauth2::EndpointNotSet,
    oauth2::EndpointMaybeSet,
    oauth2::EndpointSet,
>;

fn oauth_error<RE: oauth2::ErrorResponse + 'static>(
    error: RequestTokenError<EgressError, RE>,
    code_of: impl Fn(&RE) -> String,
) -> ConnError {
    match error {
        RequestTokenError::ServerResponse(response) => ConnError::Provider(code_of(&response)),
        RequestTokenError::Request(egress) => ConnError::Egress(egress),
        RequestTokenError::Parse(_, _) => {
            ConnError::Provider("unreadable token response".to_owned())
        }
        RequestTokenError::Other(_) => ConnError::Provider("token request failed".to_owned()),
    }
}

impl<E: SqlEngine> Connections<E> {
    /// The connections of the cell whose vault is `vault`.
    ///
    /// # Errors
    /// Storage failures creating the connection tables.
    pub fn new(
        vault: Vault<E>,
        egress: Egress,
        providers: BTreeMap<String, Provider>,
        clients: BTreeMap<String, ClientCredentials>,
        redirect_url: Option<String>,
    ) -> Result<Self, ConnError> {
        vault.engine().execute_batch(DDL).map_err(storage)?;
        Ok(Self {
            vault: Mutex::new(vault),
            egress,
            providers,
            clients,
            redirect_url,
            flights: Mutex::new(BTreeMap::new()),
        })
    }

    fn with_vault<T>(
        &self,
        f: impl FnOnce(&Vault<E>) -> Result<T, ConnError>,
    ) -> Result<T, ConnError> {
        let vault = self.vault.lock().unwrap_or_else(PoisonError::into_inner);
        f(&vault)
    }

    /// Rewrap the cell's data keys under the current KEK id
    /// (VAULT-ROTATE-1); the number rewrapped.
    ///
    /// # Errors
    /// A data key under an id the keyring no longer lists, or storage.
    pub fn rewrap(&self) -> Result<usize, ConnError> {
        self.with_vault(|vault| Ok(vault.rewrap()?))
    }

    fn audit(
        vault: &Vault<E>,
        at_ms: u64,
        connection: &str,
        event: &str,
        actor: &str,
        detail: Option<&str>,
    ) -> Result<(), ConnError> {
        vault
            .engine()
            .execute(
                r#"INSERT INTO "_norn_connection_audit" ("at_ms", "connection", "event", "actor", "detail") VALUES (?1, ?2, ?3, ?4, ?5)"#,
                &[
                    ms(at_ms),
                    SqlValue::Text(connection.to_owned()),
                    SqlValue::Text(event.to_owned()),
                    SqlValue::Text(actor.to_owned()),
                    opt_text(detail),
                ],
            )
            .map_err(storage)
    }

    /// The audit trail of `connection`: `(at_ms, event, actor, detail)`.
    ///
    /// # Errors
    /// Storage failures.
    pub fn audit_trail(&self, connection: &str) -> Result<Vec<AuditRow>, ConnError> {
        self.with_vault(|vault| {
            Ok(vault
                .engine()
                .query(
                    r#"SELECT "at_ms", "event", "actor", "detail" FROM "_norn_connection_audit" WHERE "connection" = ?1 ORDER BY "seq""#,
                    &[SqlValue::Text(connection.to_owned())],
                )
                .map_err(storage)?
                .iter()
                .map(|row| {
                    (
                        get_ms(row, 0).unwrap_or_default(),
                        get_text(row, 1).unwrap_or_default(),
                        get_text(row, 2).unwrap_or_default(),
                        get_text(row, 3),
                    )
                })
                .collect())
        })
    }

    fn record(vault: &Vault<E>, id: &str) -> Result<Option<Record>, ConnError> {
        let rows = vault
            .engine()
            .query(
                r#"SELECT "provider", "state", "last4", "scopes", "expires_at_ms", "last_used_ms", "last_error", "revision", "params" FROM "_norn_connection" WHERE "id" = ?1"#,
                &[SqlValue::Text(id.to_owned())],
            )
            .map_err(storage)?;
        let Some(row) = rows.first() else {
            return Ok(None);
        };
        Ok(Some(Record {
            provider: get_text(row, 0).unwrap_or_default(),
            state: if get_text(row, 1).as_deref() == Some("reconnect_needed") {
                State::ReconnectNeeded
            } else {
                State::Connected
            },
            last4: get_text(row, 2),
            scopes: get_text(row, 3)
                .and_then(|text| serde_json::from_str(&text).ok())
                .unwrap_or_default(),
            expires_at_ms: get_ms(row, 4),
            last_used_ms: get_ms(row, 5),
            last_error: get_text(row, 6),
            revision: get_int(row, 7).unwrap_or_default(),
            params: get_text(row, 8)
                .and_then(|text| serde_json::from_str(&text).ok())
                .unwrap_or_default(),
        }))
    }

    fn write_record(
        vault: &Vault<E>,
        id: &str,
        record: &Record,
        now_ms: u64,
    ) -> Result<(), ConnError> {
        vault
            .engine()
            .execute(
                r#"INSERT OR REPLACE INTO "_norn_connection" ("id", "provider", "state", "last4", "scopes", "expires_at_ms", "last_used_ms", "last_error", "revision", "params", "updated_ms") VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)"#,
                &[
                    SqlValue::Text(id.to_owned()),
                    SqlValue::Text(record.provider.clone()),
                    SqlValue::Text(record.state.as_str().to_owned()),
                    opt_text(record.last4.as_deref()),
                    SqlValue::Text(serde_json::to_string(&record.scopes).unwrap_or_else(|_| "[]".to_owned())),
                    opt_ms(record.expires_at_ms),
                    opt_ms(record.last_used_ms),
                    opt_text(record.last_error.as_deref()),
                    SqlValue::Integer(record.revision),
                    SqlValue::Text(serde_json::to_string(&record.params).unwrap_or_else(|_| "{}".to_owned())),
                    ms(now_ms),
                ],
            )
            .map_err(storage)
    }

    /// Store `tokens` for `id` as a new revision (one engine transaction).
    fn store(
        vault: &Vault<E>,
        id: &str,
        mut record: Record,
        tokens: &Tokens,
        expected: Option<i64>,
        now_ms: u64,
    ) -> Result<Record, ConnError> {
        let plaintext = Secret::new(serde_json::to_vec(tokens).map_err(storage)?);
        vault.engine().transaction(|| {
            let current = Self::record(vault, id)?.map(|record| record.revision);
            if let Some(expected) = expected
                && current != Some(expected)
            {
                return Err(ConnError::Conflict);
            }
            record.revision = current.unwrap_or(0).saturating_add(1);
            vault.put(&slot(id), record.revision, plaintext.expose(), now_ms)?;
            Self::write_record(vault, id, &record, now_ms)?;
            Ok(record)
        })
    }

    fn provider(&self, id: &str) -> Result<&Provider, ConnError> {
        self.providers.get(id).ok_or(ConnError::NotFound)
    }

    /// `PUT`: store credentials for `id` (CONN-WRITE-ONLY-1).
    ///
    /// # Errors
    /// An unknown provider or malformed input, or vault failures.
    pub fn set(
        &self,
        actor: &str,
        id: &str,
        body: SetCredentials,
        now_ms: u64,
    ) -> Result<Status, ConnError> {
        let provider = self.provider(&body.provider)?;
        if body.access_token.is_empty() || body.access_token.len() > 8192 {
            return Err(ConnError::Invalid(
                "accessToken must be 1-8192 bytes".to_owned(),
            ));
        }
        if !provider.offers(&body.scopes) {
            return Err(ConnError::Invalid(
                "a scope is not one the provider offers".to_owned(),
            ));
        }
        provider
            .fill(&provider.token_url, &body.params)
            .map_err(ConnError::Invalid)?;
        let record = Record {
            provider: body.provider.clone(),
            state: State::Connected,
            last4: last4(&body.access_token),
            scopes: body.scopes.clone(),
            expires_at_ms: body.expires_at_ms,
            last_used_ms: None,
            last_error: None,
            revision: 0,
            params: body.params.clone(),
        };
        let tokens = Tokens {
            access_token: body.access_token.clone(),
            refresh_token: body.refresh_token.clone(),
        };
        drop(body);
        self.with_vault(|vault| {
            Self::store(vault, id, record, &tokens, None, now_ms)?;
            Self::audit(vault, now_ms, id, "credential.set", actor, None)?;
            Ok(())
        })?;
        self.status(id)
    }

    /// `GET`: the status of `id`.
    ///
    /// # Errors
    /// Storage failures.
    pub fn status(&self, id: &str) -> Result<Status, ConnError> {
        self.with_vault(|vault| {
            Ok(match Self::record(vault, id)? {
                Some(record) => Status {
                    id: id.to_owned(),
                    provider: Some(record.provider),
                    state: record.state.as_str().to_owned(),
                    last4: record.last4,
                    scopes: record.scopes,
                    expires_at_ms: record.expires_at_ms,
                    last_used_ms: record.last_used_ms,
                    last_error: record.last_error,
                },
                None => Status {
                    id: id.to_owned(),
                    provider: None,
                    state: "not_connected".to_owned(),
                    last4: None,
                    scopes: Vec::new(),
                    expires_at_ms: None,
                    last_used_ms: None,
                    last_error: None,
                },
            })
        })
    }

    fn client(
        &self,
        provider: &Provider,
        params: &BTreeMap<String, String>,
        refresh: bool,
    ) -> Result<Client, ConnError> {
        let registered = self.clients.get(&provider.id).ok_or_else(|| {
            ConnError::Unavailable(format!("no OAuth client is configured for {}", provider.id))
        })?;
        let auth_url = provider
            .fill(&provider.authorization_url, params)
            .map_err(ConnError::Invalid)?;
        let token_template = if refresh {
            provider
                .refresh_token_url
                .as_deref()
                .unwrap_or(&provider.token_url)
        } else {
            &provider.token_url
        };
        let token_url = provider
            .fill(token_template, params)
            .map_err(ConnError::Invalid)?;
        let bad = |what: &str| ConnError::Invalid(format!("the provider's {what} is not a URL"));
        let mut client = BasicClient::new(ClientId::new(registered.client_id.clone()))
            .set_auth_uri(AuthUrl::new(auth_url).map_err(|_parse| bad("authorization URL"))?)
            .set_token_uri(TokenUrl::new(token_url).map_err(|_parse| bad("token URL"))?)
            .set_revocation_url_option(
                provider
                    .revocation_url
                    .as_ref()
                    .map(|url| provider.fill(url, params).map_err(ConnError::Invalid))
                    .transpose()?
                    .map(|url| RevocationUrl::new(url).map_err(|_parse| bad("revocation URL")))
                    .transpose()?,
            )
            .set_auth_type(match provider.client_auth {
                ClientAuth::Basic => AuthType::BasicAuth,
                ClientAuth::Post | ClientAuth::None => AuthType::RequestBody,
            });
        if provider.client_auth != ClientAuth::None
            && let Some(secret) = registered
                .client_secret
                .as_ref()
                .and_then(Secret::expose_str)
        {
            client = client.set_client_secret(ClientSecret::new(secret.to_owned()));
        }
        if let Some(redirect) = &self.redirect_url {
            client = client.set_redirect_uri(
                RedirectUrl::new(redirect.clone()).map_err(|_parse| bad("redirect URL"))?,
            );
        }
        Ok(client)
    }

    /// `POST …/oauth:start`: the provider authorization URL a person opens,
    /// with a state bound to `actor`, `id` and this cell (OAUTH-STATE-1).
    ///
    /// # Errors
    /// An unknown provider, an unoffered scope, no client or redirect URL
    /// configured, or vault failures.
    pub fn start(
        &self,
        actor: &str,
        id: &str,
        provider_id: &str,
        scopes: Option<Vec<String>>,
        params: &BTreeMap<String, String>,
        now_ms: u64,
    ) -> Result<(String, u64), ConnError> {
        let provider = self.provider(provider_id)?;
        if self.redirect_url.is_none() {
            return Err(ConnError::Unavailable(
                "no OAuth redirect URL is configured".to_owned(),
            ));
        }
        let scopes = scopes.unwrap_or_else(|| provider.scopes.clone());
        if !provider.offers(&scopes) {
            return Err(ConnError::Invalid(
                "a scope is not one the provider offers".to_owned(),
            ));
        }
        let client = self.client(provider, params, false)?;
        let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
        let mut request = client
            .authorize_url(CsrfToken::new_random)
            .set_pkce_challenge(challenge);
        if !scopes.is_empty() {
            request = request.add_scope(Scope::new(scopes.join(&provider.scope_separator)));
        }
        for (name, value) in &provider.authorization_params {
            request = request.add_extra_param(name.clone(), value.clone());
        }
        let (url, state) = request.url();
        let hash = state_hash(state.secret());
        let expires_ms = now_ms.saturating_add(STATE_TTL_MS);
        self.with_vault(|vault| {
            vault.engine().transaction(|| {
                vault.put(&format!("oauth-state:{hash}"), 0, verifier.secret().as_bytes(), now_ms)?;
                vault
                    .engine()
                    .execute(
                        r#"INSERT INTO "_norn_oauth_state" ("state_hash", "connection", "provider", "actor", "scopes", "params", "expires_ms") VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"#,
                        &[
                            SqlValue::Text(hash.clone()),
                            SqlValue::Text(id.to_owned()),
                            SqlValue::Text(provider.id.clone()),
                            SqlValue::Text(actor.to_owned()),
                            SqlValue::Text(serde_json::to_string(&scopes).map_err(storage)?),
                            SqlValue::Text(serde_json::to_string(&params).map_err(storage)?),
                            ms(expires_ms),
                        ],
                    )
                    .map_err(storage)?;
                Self::audit(vault, now_ms, id, "oauth.start", actor, Some(&provider.id))
            })
        })?;
        Ok((url.to_string(), expires_ms))
    }

    /// The connection an OAuth `state` was started for, if it is pending;
    /// the route authorizes against it before [`Self::callback`].
    ///
    /// # Errors
    /// Storage failures.
    pub fn pending_connection(&self, state: &str) -> Result<Option<String>, ConnError> {
        let hash = state_hash(state);
        self.with_vault(|vault| {
            Ok(vault
                .engine()
                .query(
                    r#"SELECT "connection" FROM "_norn_oauth_state" WHERE "state_hash" = ?1"#,
                    &[SqlValue::Text(hash)],
                )
                .map_err(storage)?
                .first()
                .and_then(|row| get_text(row, 0)))
        })
    }

    /// The callback: consume `state` once (OAUTH-STATE-1), exchange `code`
    /// with its PKCE verifier and store the tokens.
    ///
    /// # Errors
    /// [`ConnError::BadState`], or the provider's refusal.
    pub fn callback(
        &self,
        actor: &str,
        state: &str,
        code: &str,
        now_ms: u64,
    ) -> Result<Status, ConnError> {
        let hash = state_hash(state);
        // Consumed before anything else, whatever happens next.
        let (id, provider_id, scopes, params, verifier) = self.with_vault(|vault| {
            vault.engine().transaction(|| {
                let rows = vault
                    .engine()
                    .query(
                        r#"SELECT "connection", "provider", "actor", "scopes", "params", "expires_ms" FROM "_norn_oauth_state" WHERE "state_hash" = ?1"#,
                        &[SqlValue::Text(hash.clone())],
                    )
                    .map_err(storage)?;
                let row = rows.first().ok_or(ConnError::BadState)?.clone();
                let verifier = vault.get(&format!("oauth-state:{hash}"))?;
                vault
                    .engine()
                    .execute(
                        r#"DELETE FROM "_norn_oauth_state" WHERE "state_hash" = ?1"#,
                        &[SqlValue::Text(hash.clone())],
                    )
                    .map_err(storage)?;
                vault.delete(&format!("oauth-state:{hash}"))?;
                Ok((row, verifier))
            })
        })
        .and_then(|(row, verifier)| {
            let id = get_text(&row, 0).ok_or(ConnError::BadState)?;
            if get_text(&row, 2).as_deref() != Some(actor) || get_ms(&row, 5).is_none_or(|expires| expires <= now_ms) {
                let _audited = self.with_vault(|vault| Self::audit(vault, now_ms, &id, "oauth.refused", actor, Some("state")));
                return Err(ConnError::BadState);
            }
            let verifier = verifier
                .and_then(|(_, secret)| secret.expose_str().map(str::to_owned))
                .ok_or(ConnError::BadState)?;
            Ok((
                id,
                get_text(&row, 1).unwrap_or_default(),
                get_text(&row, 3).and_then(|t| serde_json::from_str::<Vec<String>>(&t).ok()).unwrap_or_default(),
                get_text(&row, 4).and_then(|t| serde_json::from_str::<BTreeMap<String, String>>(&t).ok()).unwrap_or_default(),
                verifier,
            ))
        })?;
        let provider = self.provider(&provider_id)?;
        let client = self.client(provider, &params, false)?;
        let exchanged = client
            .exchange_code(AuthorizationCode::new(code.to_owned()))
            .set_pkce_verifier(PkceCodeVerifier::new(verifier))
            .request(&|request| self.egress.send(request))
            .map_err(|error| oauth_error(error, |response| response.error().to_string()));
        let response = match exchanged {
            Ok(response) => response,
            Err(error) => {
                let detail = failure_code(&error);
                let _audited = self.with_vault(|vault| {
                    Self::audit(
                        vault,
                        now_ms,
                        &id,
                        "credential.failure",
                        actor,
                        Some(&detail),
                    )
                });
                return Err(error);
            }
        };
        let access = response.access_token().secret().clone();
        let record = Record {
            provider: provider.id.clone(),
            state: State::Connected,
            last4: last4(&access),
            scopes: response.scopes().map_or(scopes, |granted| {
                granted.iter().map(|scope| scope.to_string()).collect()
            }),
            expires_at_ms: response.expires_in().map(|left| {
                now_ms.saturating_add(u64::try_from(left.as_millis()).unwrap_or(u64::MAX))
            }),
            last_used_ms: None,
            last_error: None,
            revision: 0,
            params,
        };
        let tokens = Tokens {
            access_token: access,
            refresh_token: response.refresh_token().map(|token| token.secret().clone()),
        };
        self.with_vault(|vault| {
            Self::store(vault, &id, record, &tokens, None, now_ms)?;
            Self::audit(
                vault,
                now_ms,
                &id,
                "oauth.connected",
                actor,
                Some(&provider.id),
            )
        })?;
        self.status(&id)
    }

    /// `DELETE`: revoke at the provider when it has a revocation endpoint
    /// (best effort, audited), then delete the credentials.
    ///
    /// # Errors
    /// Storage failures.
    pub fn revoke(&self, actor: &str, id: &str, now_ms: u64) -> Result<Status, ConnError> {
        let held = self.with_vault(|vault| {
            let Some(record) = Self::record(vault, id)? else {
                return Ok(None);
            };
            let tokens = vault
                .get(&slot(id))?
                .and_then(|(_, secret)| serde_json::from_slice::<Tokens>(secret.expose()).ok());
            Ok(Some((record, tokens)))
        })?;
        let Some((record, tokens)) = held else {
            return self.status(id);
        };
        let mut detail = None;
        if let (Some(tokens), Some(provider)) = (tokens, self.providers.get(&record.provider))
            && provider.revocation_url.is_some()
            && let Ok(client) = self.client(provider, &record.params, false)
        {
            let token = tokens.refresh_token.clone().map_or_else(
                || {
                    StandardRevocableToken::AccessToken(oauth2::AccessToken::new(
                        tokens.access_token.clone(),
                    ))
                },
                |refresh| StandardRevocableToken::RefreshToken(RefreshToken::new(refresh)),
            );
            let revoked = client
                .revoke_token(token)
                .map_err(|error| {
                    // oauth2 revokes only over https.
                    ConnError::Provider(match error {
                        oauth2::ConfigurationError::InsecureUrl(_) => {
                            "revocation_url_not_https".to_owned()
                        }
                        oauth2::ConfigurationError::MissingUrl(_) | _ => {
                            "no_revocation_endpoint".to_owned()
                        }
                    })
                })
                .and_then(|request| {
                    request
                        .request(&|request| self.egress.send(request))
                        .map_err(|error| {
                            oauth_error(error, |response| response.error().to_string())
                        })
                });
            detail = Some(match revoked {
                Ok(()) => "provider_revoked".to_owned(),
                Err(error) => format!("provider_revoke_failed:{}", failure_code(&error)),
            });
        }
        self.with_vault(|vault| {
            vault.engine().transaction(|| {
                vault.delete(&slot(id))?;
                vault
                    .engine()
                    .execute(
                        r#"DELETE FROM "_norn_connection" WHERE "id" = ?1"#,
                        &[SqlValue::Text(id.to_owned())],
                    )
                    .map_err(storage)?;
                Self::audit(
                    vault,
                    now_ms,
                    id,
                    "credential.revoke",
                    actor,
                    detail.as_deref(),
                )
            })
        })?;
        self.status(id)
    }

    /// The access token for a connector call by `actor` (a connection or
    /// flow principal), refreshed first when it is about to expire
    /// (REFRESH-ONE-1). Records the use.
    ///
    /// # Errors
    /// No such connection, [`ConnError::ReconnectNeeded`], or a refresh
    /// failure.
    pub fn access_token(&self, actor: &str, id: &str, now_ms: u64) -> Result<Secret, ConnError> {
        let (record, _) = self.read(id)?;
        let expiring = record
            .expires_at_ms
            .is_some_and(|expires| expires <= now_ms.saturating_add(REFRESH_SKEW_MS));
        if expiring {
            self.refresh(actor, id, record.revision, now_ms)?;
        }
        let (mut record, tokens) = self.read(id)?;
        record.last_used_ms = Some(now_ms);
        self.with_vault(|vault| {
            vault.engine().transaction(|| {
                vault
                    .engine()
                    .execute(
                        r#"UPDATE "_norn_connection" SET "last_used_ms" = ?1 WHERE "id" = ?2"#,
                        &[ms(now_ms), SqlValue::Text(id.to_owned())],
                    )
                    .map_err(storage)?;
                Self::audit(vault, now_ms, id, "credential.use", actor, None)
            })
        })?;
        Ok(Secret::from(tokens.access_token.clone()))
    }

    fn read(&self, id: &str) -> Result<(Record, Tokens), ConnError> {
        self.with_vault(|vault| {
            let record = Self::record(vault, id)?.ok_or(ConnError::NotFound)?;
            if record.state == State::ReconnectNeeded {
                return Err(ConnError::ReconnectNeeded);
            }
            let (revision, secret) = vault.get(&slot(id))?.ok_or(ConnError::NotFound)?;
            if revision != record.revision {
                return Err(ConnError::Conflict);
            }
            let tokens = serde_json::from_slice(secret.expose())
                .map_err(|_parse| ConnError::Vault(VaultError::Integrity))?;
            Ok((record, tokens))
        })
    }

    /// REFRESH-ONE-1: one refresh of `id` at a time; a caller that finds
    /// one in flight waits for it.
    fn refresh(&self, actor: &str, id: &str, seen: i64, now_ms: u64) -> Result<(), ConnError> {
        let (flight, leader) = {
            let mut flights = self.flights.lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(flight) = flights.get(id) {
                (Arc::clone(flight), false)
            } else {
                let flight = Arc::new(Flight::default());
                flights.insert(id.to_owned(), Arc::clone(&flight));
                (flight, true)
            }
        };
        if !leader {
            let mut done = flight.done.lock().unwrap_or_else(PoisonError::into_inner);
            while done.is_none() {
                done = flight
                    .ready
                    .wait(done)
                    .unwrap_or_else(PoisonError::into_inner);
            }
            return done.clone().unwrap_or(Ok(()));
        }
        let result = self.refresh_now(actor, id, seen, now_ms);
        self.flights
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(id);
        *flight.done.lock().unwrap_or_else(PoisonError::into_inner) = Some(result.clone());
        flight.ready.notify_all();
        result
    }

    fn refresh_now(&self, actor: &str, id: &str, seen: i64, now_ms: u64) -> Result<(), ConnError> {
        let (record, tokens) = self.read(id)?;
        // A refresh that finished since this caller read is reused.
        if record.revision != seen {
            return Ok(());
        }
        let Some(refresh) = tokens.refresh_token.clone() else {
            return self.mark_reconnect(actor, id, record, "no_refresh_token", now_ms);
        };
        let provider = self.provider(&record.provider)?;
        let client = self.client(provider, &record.params, true)?;
        let refreshed = client
            .exchange_refresh_token(&RefreshToken::new(refresh))
            .request(&|request| self.egress.send(request))
            .map_err(|error| oauth_error(error, |response| response.error().to_string()));
        let response = match refreshed {
            Ok(response) => response,
            Err(ConnError::Provider(code)) => {
                return self
                    .mark_reconnect(actor, id, record, &code, now_ms)
                    .and(Err(ConnError::ReconnectNeeded));
            }
            Err(error) => {
                let code = failure_code(&error);
                self.with_vault(|vault| {
                    vault
                        .engine()
                        .execute(
                            r#"UPDATE "_norn_connection" SET "last_error" = ?1 WHERE "id" = ?2"#,
                            &[SqlValue::Text(code.clone()), SqlValue::Text(id.to_owned())],
                        )
                        .map_err(storage)?;
                    Self::audit(vault, now_ms, id, "credential.failure", actor, Some(&code))
                })?;
                return Err(error);
            }
        };
        let access = response.access_token().secret().clone();
        let next = Record {
            last4: last4(&access),
            expires_at_ms: response.expires_in().map(|left| {
                now_ms.saturating_add(u64::try_from(left.as_millis()).unwrap_or(u64::MAX))
            }),
            last_error: None,
            scopes: response.scopes().map_or_else(
                || record.scopes.clone(),
                |granted| granted.iter().map(|scope| scope.to_string()).collect(),
            ),
            ..record
        };
        // A rotated refresh token replaces the old one in this same write.
        let tokens = Tokens {
            access_token: access,
            refresh_token: response
                .refresh_token()
                .map(|token| token.secret().clone())
                .or_else(|| tokens.refresh_token.clone()),
        };
        self.with_vault(|vault| {
            Self::store(vault, id, next, &tokens, Some(seen), now_ms)?;
            Self::audit(vault, now_ms, id, "credential.refresh", actor, None)
        })
    }

    fn mark_reconnect(
        &self,
        actor: &str,
        id: &str,
        mut record: Record,
        code: &str,
        now_ms: u64,
    ) -> Result<(), ConnError> {
        record.state = State::ReconnectNeeded;
        record.last_error = Some(code.to_owned());
        self.with_vault(|vault| {
            vault.engine().transaction(|| {
                if Self::record(vault, id)?.map(|current| current.revision) == Some(record.revision)
                {
                    Self::write_record(vault, id, &record, now_ms)?;
                }
                Self::audit(vault, now_ms, id, "credential.failure", actor, Some(code))
            })
        })
    }
}

/// A failure's short code for status and audit: never a provider body.
fn failure_code(error: &ConnError) -> String {
    match error {
        ConnError::Provider(code) => code
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
            .take(64)
            .collect(),
        ConnError::Egress(EgressError::Forbidden(_)) => "egress_forbidden".to_owned(),
        ConnError::Egress(EgressError::Unresolved(_)) => "egress_unresolved".to_owned(),
        ConnError::Egress(EgressError::Redirect(_)) => "egress_redirect".to_owned(),
        ConnError::Egress(EgressError::Transport(_) | EgressError::Config(_)) => {
            "egress_failed".to_owned()
        }
        ConnError::ReconnectNeeded => "reconnect_needed".to_owned(),
        ConnError::Invalid(_)
        | ConnError::NotFound
        | ConnError::Conflict
        | ConnError::BadState
        | ConnError::Unavailable(_)
        | ConnError::Vault(_) => "failed".to_owned(),
    }
}
