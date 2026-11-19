//! One cell's connections: credentials in the vault, status beside them, the
//! OAuth authorization-code flow, refresh and the audit trail.
//!
//! CONN-WRITE-ONLY-1: credentials go in ([`Connections::set`],
//! [`Connections::complete`]) and are used by connector calls
//! ([`Connections::access_token`]); nothing returns them. [`Status`] carries
//! only `last4`, scopes, expiry, last use and last error.
//! CONN-AUDIT-1: every set, use, refresh, revoke, OAuth start/connect and
//! failure appends one `_norn_connection_audit` row: who, when, which
//! connection, what happened. Never a credential, a code or a verifier. A
//! row recording a credential change commits in the same transaction as the
//! change.
//! CONN-ERROR-1: a failure is recorded and answered as a fixed local code
//! ([`failure_code`]): an OAuth error is one of RFC 6749's codes or
//! `provider_error`, a transport failure one of the egress client's kinds.
//! No provider or peer text reaches an answer, `last_error` or the audit.
//! CONN-GENERATION-1: each connection id has a generation and a revision in
//! `_norn_connection_epoch`, a row never deleted, so neither resets when a
//! connection is disconnected and connected again. Every credential write
//! takes the next revision; setting, connecting and disconnecting take the
//! next generation. A refresh writes only if both are still the ones it
//! read, an OAuth completion only if the generation is still the one its
//! start saw, and a disconnect deletes, in one transaction, the credential
//! and every pending authorization of the connection before it asks the
//! provider to revoke the old grant. Stale work never overwrites newer work.
//! CONN-SEALED-BINDING-1: the provider and its URL parameters are sealed with
//! the tokens (and with an authorization's verifier, actor, connection,
//! generation and expiry); calls use the sealed values, and a plaintext row
//! that disagrees refuses as a vault integrity failure. Editing the status
//! row can't send a credential to another endpoint.
//! OAUTH-STATE-1: an authorization starts with a random state bound to the
//! starting principal, the connection and the cell, kept hashed for ten
//! minutes with its PKCE (S256) verifier sealed in the vault. Completing
//! consumes it once, in one transaction that checks the sealed actor,
//! connection, completion key, expiry and generation: a second use, an
//! expired state, another principal or a superseded connection is refused
//! and nothing is exchanged.
//! OAUTH-SESSION-1: the start also mints a random completion key, answered
//! only to the starting caller and sealed as a digest with the verifier.
//! Completing needs the key as well as the principal, so another session of
//! the same principal can't complete the flow. The binding (principal,
//! connection, key) is checked before anything is spent: a caller whose
//! binding doesn't match deletes nothing and the flow stays completable.
//! REFRESH-ONE-1: refreshes of one connection are single-flight in the
//! owning cell (the cell's host is its single writer): concurrent callers
//! wait for the one in flight and read its result, so one token-endpoint
//! call is made. Rotated refresh tokens are saved in the same write. A
//! refused refresh (an OAuth error answer) marks the connection
//! `reconnect_needed`.
//! OAUTH-DIRECT-1: token exchange, refresh and revocation follow no redirect
//! and go only to https ([`Egress::send_direct`]).
//! GRANT-REVOKE-1: a disconnect deletes locally and calls no provider. A
//! provider's RFC 7009 revocation can end the whole grant (Google revokes
//! every token the person gave this client), which would break sibling
//! connections and replacements while their rows still say `connected`. So
//! revoking at the provider is its own explicit request
//! ([`Connections::revoke`] with `revoke_grant`), documented as ending every
//! connection on that grant; each such sibling is marked `reconnect_needed`
//! by its next refresh.

use std::collections::BTreeMap;
use std::fmt;
use std::fmt::Write as _;
use std::sync::{Arc, Condvar, Mutex, PoisonError};

use base64::Engine as _;
use norn_cell::engine::{SqlEngine, SqlValue};
use oauth2::basic::{BasicClient, BasicErrorResponseType};
use oauth2::{
    AuthType, AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, PkceCodeChallenge,
    PkceCodeVerifier, RedirectUrl, RefreshToken, RequestTokenError, Scope, TokenResponse as _,
    TokenUrl,
};
use serde::{Deserialize, Serialize};
use sha2::Digest as _;
use zeroize::Zeroize as _;

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

/// Why a connection call refused. No variant carries secret material or a
/// provider's text (CONN-ERROR-1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConnError {
    /// 400: the request is malformed (the message says how; local text).
    Invalid(String),
    /// 404: no such connection or provider.
    NotFound,
    /// 400: the provider is withheld (PROVIDER-WITHHELD-1); its reason.
    Unsupported(String),
    /// 409: the connection must be connected again.
    ReconnectNeeded,
    /// 409: another writer changed the connection first.
    Conflict,
    /// 400: the OAuth state is unknown, used, expired, another principal's
    /// or for a superseded connection.
    BadState,
    /// 502: the provider refused or failed: a fixed code.
    Provider(&'static str),
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
            Self::Invalid(why) | Self::Unavailable(why) | Self::Unsupported(why) => {
                write!(f, "{why}")
            }
            Self::NotFound => f.write_str("no such connection"),
            Self::ReconnectNeeded => f.write_str("the connection must be connected again"),
            Self::Conflict => f.write_str("the connection changed; retry"),
            Self::BadState => f.write_str("the authorization is unknown, used or expired"),
            Self::Provider(code) => write!(f, "the provider refused: {code}"),
            Self::Vault(VaultError::Integrity) => f.write_str("a sealed value did not open"),
            Self::Vault(_) => f.write_str("the connection vault failed"),
            // Fixed text: a host name or address stays out of answers.
            Self::Egress(error) => write!(f, "the outbound call failed: {}", egress_code(error)),
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
    /// Provider URL parameters.
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

impl Drop for SetCredentials {
    fn drop(&mut self) {
        self.access_token.zeroize();
        if let Some(refresh) = self.refresh_token.as_mut() {
            refresh.zeroize();
        }
    }
}

/// What `oauth:start` answers (OAUTH-STATE-1, OAUTH-SESSION-1).
pub struct Started {
    /// The provider authorization URL the person opens.
    pub authorize_url: String,
    /// When the authorization expires (Unix ms).
    pub expires_at_ms: u64,
    /// The state the provider echoes to the callback (also in the URL).
    pub state: Secret,
    /// The completion key, answered only to the starting caller.
    pub completion_key: Secret,
}

impl fmt::Debug for Started {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Started")
            .field("expires_at_ms", &self.expires_at_ms)
            .finish_non_exhaustive()
    }
}

/// What the vault seals for a connection (CONN-SEALED-BINDING-1).
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Tokens {
    provider: String,
    params: BTreeMap<String, String>,
    access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    refresh_token: Option<String>,
}

impl Drop for Tokens {
    fn drop(&mut self) {
        self.access_token.zeroize();
        if let Some(refresh) = self.refresh_token.as_mut() {
            refresh.zeroize();
        }
    }
}

/// What the vault seals for a pending authorization (OAUTH-STATE-1,
/// CONN-SEALED-BINDING-1).
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Pending {
    verifier: String,
    connection: String,
    actor: String,
    key_hash: String,
    provider: String,
    params: BTreeMap<String, String>,
    scopes: Vec<String>,
    generation: i64,
    expires_ms: u64,
}

impl Drop for Pending {
    fn drop(&mut self) {
        self.verifier.zeroize();
    }
}

fn sealed_json(value: &impl Serialize) -> Result<Secret, ConnError> {
    serde_json::to_string(value)
        .map(Secret::from)
        .map_err(storage)
}

fn opened<T: for<'de> Deserialize<'de>>(secret: &Secret) -> Result<T, ConnError> {
    serde_json::from_slice(secret.expose())
        .map_err(|_parse| ConnError::Vault(VaultError::Integrity))
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
    generation: i64,
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
  "generation" INTEGER NOT NULL,
  "revision" INTEGER NOT NULL,
  "params" TEXT NOT NULL,
  "updated_ms" INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS "_norn_connection_epoch" (
  "id" TEXT PRIMARY KEY,
  "generation" INTEGER NOT NULL,
  "revision" INTEGER NOT NULL
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

fn text(value: &str) -> SqlValue {
    SqlValue::Text(value.to_owned())
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

fn state_slot(hash: &str) -> String {
    format!("oauth-state:{hash}")
}

/// The stored digest of an OAuth `state`.
#[must_use]
pub fn state_hash(state: &str) -> String {
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

fn expires_at(now_ms: u64, left: Option<std::time::Duration>) -> Option<u64> {
    left.map(|left| now_ms.saturating_add(u64::try_from(left.as_millis()).unwrap_or(u64::MAX)))
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
    oauth2::EndpointNotSet,
    oauth2::EndpointSet,
>;

/// RFC 6749 section 5.2's error codes; anything else is `provider_error`
/// (CONN-ERROR-1).
const fn oauth_code(kind: &BasicErrorResponseType) -> &'static str {
    match kind {
        BasicErrorResponseType::InvalidClient => "invalid_client",
        BasicErrorResponseType::InvalidGrant => "invalid_grant",
        BasicErrorResponseType::InvalidRequest => "invalid_request",
        BasicErrorResponseType::InvalidScope => "invalid_scope",
        BasicErrorResponseType::UnauthorizedClient => "unauthorized_client",
        BasicErrorResponseType::UnsupportedGrantType => "unsupported_grant_type",
        BasicErrorResponseType::Extension(_) => "provider_error",
    }
}

fn oauth_error(
    error: RequestTokenError<EgressError, oauth2::basic::BasicErrorResponse>,
) -> ConnError {
    match error {
        RequestTokenError::ServerResponse(response) => {
            ConnError::Provider(oauth_code(response.error()))
        }
        RequestTokenError::Request(egress) => ConnError::Egress(egress),
        RequestTokenError::Parse(_, _) => ConnError::Provider("unreadable_token_response"),
        RequestTokenError::Other(_) => ConnError::Provider("token_request_failed"),
    }
}

/// An egress failure's fixed code (CONN-ERROR-1).
const fn egress_code(error: &EgressError) -> &'static str {
    match error {
        EgressError::Forbidden(_) => "egress_forbidden",
        EgressError::Unresolved(_) => "egress_unresolved",
        EgressError::Redirect(_) => "egress_redirect",
        EgressError::Deadline => "egress_deadline",
        EgressError::Transport(_) | EgressError::Config(_) => "egress_failed",
    }
}

/// A failure's fixed code for status and audit (CONN-ERROR-1).
#[must_use]
pub const fn failure_code(error: &ConnError) -> &'static str {
    match error {
        ConnError::Provider(code) => code,
        ConnError::Egress(error) => egress_code(error),
        ConnError::ReconnectNeeded => "reconnect_needed",
        ConnError::Invalid(_)
        | ConnError::NotFound
        | ConnError::Unsupported(_)
        | ConnError::Conflict
        | ConnError::BadState
        | ConnError::Unavailable(_)
        | ConnError::Vault(_) => "failed",
    }
}

/// What a transaction decided, committed either way.
enum Decided<T> {
    Done(T),
    Refused(ConnError),
}

impl<T> Decided<T> {
    fn into_result(self) -> Result<T, ConnError> {
        match self {
            Self::Done(value) => Ok(value),
            Self::Refused(error) => Err(error),
        }
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

    /// `f` in one engine transaction on the vault's storage.
    fn transaction<T>(
        &self,
        f: impl FnOnce(&Vault<E>) -> Result<T, ConnError>,
    ) -> Result<T, ConnError> {
        self.with_vault(|vault| vault.engine().transaction(|| f(vault)))
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
                &[ms(at_ms), text(connection), text(event), text(actor), opt_text(detail)],
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
                    &[text(connection)],
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

    /// CONN-GENERATION-1: `id`'s `(generation, revision)`; `(0, 0)` before
    /// anything was written.
    fn epoch(vault: &Vault<E>, id: &str) -> Result<(i64, i64), ConnError> {
        let rows = vault
            .engine()
            .query(
                r#"SELECT "generation", "revision" FROM "_norn_connection_epoch" WHERE "id" = ?1"#,
                &[text(id)],
            )
            .map_err(storage)?;
        Ok(rows.first().map_or((0, 0), |row| {
            (
                get_int(row, 0).unwrap_or_default(),
                get_int(row, 1).unwrap_or_default(),
            )
        }))
    }

    /// The next revision of `id`, and the next generation when `replace`;
    /// inside the caller's transaction.
    fn advance(vault: &Vault<E>, id: &str, replace: bool) -> Result<(i64, i64), ConnError> {
        let (generation, revision) = Self::epoch(vault, id)?;
        let next = (
            if replace {
                generation.saturating_add(1)
            } else {
                generation
            },
            revision.saturating_add(1),
        );
        vault
            .engine()
            .execute(
                r#"INSERT OR REPLACE INTO "_norn_connection_epoch" ("id", "generation", "revision") VALUES (?1, ?2, ?3)"#,
                &[text(id), SqlValue::Integer(next.0), SqlValue::Integer(next.1)],
            )
            .map_err(storage)?;
        Ok(next)
    }

    fn record(vault: &Vault<E>, id: &str) -> Result<Option<Record>, ConnError> {
        let rows = vault
            .engine()
            .query(
                r#"SELECT "provider", "state", "last4", "scopes", "expires_at_ms", "last_used_ms", "last_error", "generation", "revision", "params" FROM "_norn_connection" WHERE "id" = ?1"#,
                &[text(id)],
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
            generation: get_int(row, 7).unwrap_or_default(),
            revision: get_int(row, 8).unwrap_or_default(),
            params: get_text(row, 9)
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
                r#"INSERT OR REPLACE INTO "_norn_connection" ("id", "provider", "state", "last4", "scopes", "expires_at_ms", "last_used_ms", "last_error", "generation", "revision", "params", "updated_ms") VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)"#,
                &[
                    text(id),
                    text(&record.provider),
                    text(record.state.as_str()),
                    opt_text(record.last4.as_deref()),
                    SqlValue::Text(serde_json::to_string(&record.scopes).unwrap_or_else(|_| "[]".to_owned())),
                    opt_ms(record.expires_at_ms),
                    opt_ms(record.last_used_ms),
                    opt_text(record.last_error.as_deref()),
                    SqlValue::Integer(record.generation),
                    SqlValue::Integer(record.revision),
                    SqlValue::Text(serde_json::to_string(&record.params).unwrap_or_else(|_| "{}".to_owned())),
                    ms(now_ms),
                ],
            )
            .map_err(storage)
    }

    /// Store `tokens` for `id` at the next revision (and generation when
    /// `replace`), inside the caller's transaction.
    fn store(
        vault: &Vault<E>,
        id: &str,
        mut record: Record,
        tokens: &Tokens,
        replace: bool,
        now_ms: u64,
    ) -> Result<Record, ConnError> {
        let plaintext = sealed_json(tokens)?;
        let (generation, revision) = Self::advance(vault, id, replace)?;
        record.generation = generation;
        record.revision = revision;
        vault.put(&slot(id), revision, plaintext.expose(), now_ms)?;
        Self::write_record(vault, id, &record, now_ms)?;
        Ok(record)
    }

    /// The provider `id`, refused when withheld (PROVIDER-WITHHELD-1).
    fn provider(&self, id: &str) -> Result<&Provider, ConnError> {
        let provider = self.providers.get(id).ok_or(ConnError::NotFound)?;
        match &provider.withheld {
            Some(reason) => Err(ConnError::Unsupported(reason.clone())),
            None => Ok(provider),
        }
    }

    /// `PUT`: store credentials for `id` (CONN-WRITE-ONLY-1) as a new
    /// generation.
    ///
    /// # Errors
    /// An unknown or withheld provider, malformed input, or vault failures.
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
            generation: 0,
            revision: 0,
            params: body.params.clone(),
        };
        let tokens = Tokens {
            provider: body.provider.clone(),
            params: body.params.clone(),
            access_token: body.access_token.clone(),
            refresh_token: body.refresh_token.clone(),
        };
        drop(body);
        self.transaction(|vault| {
            Self::store(vault, id, record, &tokens, true, now_ms)?;
            Self::audit(vault, now_ms, id, "credential.set", actor, None)
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

    fn registered(&self, provider: &Provider) -> Result<&ClientCredentials, ConnError> {
        self.clients.get(&provider.id).ok_or_else(|| {
            ConnError::Unavailable(format!("no OAuth client is configured for {}", provider.id))
        })
    }

    fn client(
        &self,
        provider: &Provider,
        params: &BTreeMap<String, String>,
        refresh: bool,
    ) -> Result<Client, ConnError> {
        let registered = self.registered(provider)?;
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
    /// with a state bound to `actor`, `id`, this cell and the connection's
    /// current generation (OAUTH-STATE-1), and the completion key only the
    /// caller receives (OAUTH-SESSION-1).
    ///
    /// # Errors
    /// An unknown or withheld provider, an unoffered scope, no client or
    /// redirect URL configured, or vault failures.
    pub fn start(
        &self,
        actor: &str,
        id: &str,
        provider_id: &str,
        scopes: Option<Vec<String>>,
        params: &BTreeMap<String, String>,
        now_ms: u64,
    ) -> Result<Started, ConnError> {
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
        let key = Secret::from(CsrfToken::new_random_len(32).secret().clone());
        let key_hash = state_hash(key.expose_str().unwrap_or_default());
        let expires_ms = now_ms.saturating_add(STATE_TTL_MS);
        self.transaction(|vault| {
            let pending = Pending {
                verifier: verifier.secret().clone(),
                connection: id.to_owned(),
                actor: actor.to_owned(),
                key_hash: key_hash.clone(),
                provider: provider.id.clone(),
                params: params.clone(),
                scopes: scopes.clone(),
                generation: Self::epoch(vault, id)?.0,
                expires_ms,
            };
            vault.put(&state_slot(&hash), 0, sealed_json(&pending)?.expose(), now_ms)?;
            vault
                .engine()
                .execute(
                    r#"INSERT INTO "_norn_oauth_state" ("state_hash", "connection", "expires_ms") VALUES (?1, ?2, ?3)"#,
                    &[text(&hash), text(id), ms(expires_ms)],
                )
                .map_err(storage)?;
            Self::audit(vault, now_ms, id, "oauth.start", actor, Some(&provider.id))
        })?;
        Ok(Started {
            authorize_url: url.to_string(),
            expires_at_ms: expires_ms,
            state: Secret::from(state.secret().clone()),
            completion_key: key,
        })
    }

    /// Delete one pending authorization, row and sealed verifier.
    fn forget_state(vault: &Vault<E>, hash: &str) -> Result<(), ConnError> {
        vault
            .engine()
            .execute(
                r#"DELETE FROM "_norn_oauth_state" WHERE "state_hash" = ?1"#,
                &[text(hash)],
            )
            .map_err(storage)?;
        Ok(vault.delete(&state_slot(hash))?)
    }

    /// OAUTH-STATE-1: consume `state` once and return its sealed pending
    /// authorization if it is `actor`'s, for `id`, under `key`, unexpired
    /// and of the connection's current generation; refused (and audited)
    /// otherwise. OAUTH-SESSION-1: a binding that doesn't match (principal,
    /// connection, key) refuses before the state is spent.
    fn consume_state(
        &self,
        actor: &str,
        id: &str,
        state: &str,
        key: &str,
        now_ms: u64,
    ) -> Result<Pending, ConnError> {
        let key_hash = state_hash(key);
        let hash = state_hash(state);
        self.transaction(|vault| {
            let rows = vault
                .engine()
                .query(
                    r#"SELECT "connection" FROM "_norn_oauth_state" WHERE "state_hash" = ?1"#,
                    &[text(&hash)],
                )
                .map_err(storage)?;
            let Some(row) = rows.first() else {
                return Ok(Decided::Refused(ConnError::BadState));
            };
            let indexed = get_text(row, 0).unwrap_or_default();
            // A sealed value that doesn't open refuses like any other.
            let pending = vault
                .get(&state_slot(&hash))
                .ok()
                .flatten()
                .and_then(|(_, secret)| opened::<Pending>(&secret).ok());
            // OAUTH-SESSION-1: only the starting session's binding spends it.
            let bound = pending.as_ref().is_none_or(|pending| {
                pending.connection == id
                    && indexed == id
                    && pending.actor == actor
                    && pending.key_hash == key_hash
            });
            if !bound {
                Self::audit(
                    vault,
                    now_ms,
                    &indexed,
                    "oauth.refused",
                    actor,
                    Some("binding"),
                )?;
                return Ok(Decided::Refused(ConnError::BadState));
            }
            // Spent by its own session (or unreadable), whatever happens next.
            Self::forget_state(vault, &hash)?;
            let current = Self::epoch(vault, id)?.0;
            let Some(pending) = pending
                .filter(|pending| pending.expires_ms > now_ms && pending.generation == current)
            else {
                Self::audit(
                    vault,
                    now_ms,
                    &indexed,
                    "oauth.refused",
                    actor,
                    Some("state"),
                )?;
                return Ok(Decided::Refused(ConnError::BadState));
            };
            Ok(Decided::Done(pending))
        })?
        .into_result()
    }

    /// Complete the authorization `state` for `id` with the provider's
    /// `code` (OAUTH-STATE-1): consumed once, in a transaction that checks
    /// the sealed actor, connection, completion `key` (OAUTH-SESSION-1),
    /// expiry and generation; then the code is
    /// exchanged with its PKCE verifier (OAUTH-DIRECT-1), and the tokens are
    /// stored only if the generation is still the one the start saw
    /// (CONN-GENERATION-1).
    ///
    /// # Errors
    /// [`ConnError::BadState`], [`ConnError::Conflict`] when the connection
    /// was replaced or disconnected meanwhile, or the provider's refusal.
    pub fn complete(
        &self,
        actor: &str,
        id: &str,
        state: &str,
        key: &str,
        code: &Secret,
        now_ms: u64,
    ) -> Result<Status, ConnError> {
        let pending = self.consume_state(actor, id, state, key, now_ms)?;
        let provider = self.provider(&pending.provider)?;
        let client = self.client(provider, &pending.params, false)?;
        let exchanged = code
            .expose_str()
            .ok_or(ConnError::BadState)
            .and_then(|code| {
                client
                    .exchange_code(AuthorizationCode::new(code.to_owned()))
                    .set_pkce_verifier(PkceCodeVerifier::new(pending.verifier.clone()))
                    .request(&|request| self.egress.send_direct(request))
                    .map_err(oauth_error)
            });
        let response = match exchanged {
            Ok(response) => response,
            Err(error) => {
                let code = failure_code(&error);
                self.transaction(|vault| {
                    Self::audit(vault, now_ms, id, "credential.failure", actor, Some(code))
                })?;
                return Err(error);
            }
        };
        let access = response.access_token().secret().clone();
        let record = Record {
            provider: provider.id.clone(),
            state: State::Connected,
            last4: last4(&access),
            scopes: response.scopes().map_or_else(
                || pending.scopes.clone(),
                |granted| granted.iter().map(|scope| scope.to_string()).collect(),
            ),
            expires_at_ms: expires_at(now_ms, response.expires_in()),
            last_used_ms: None,
            last_error: None,
            generation: 0,
            revision: 0,
            params: pending.params.clone(),
        };
        let tokens = Tokens {
            provider: provider.id.clone(),
            params: pending.params.clone(),
            access_token: access,
            refresh_token: response.refresh_token().map(|token| token.secret().clone()),
        };
        self.transaction(|vault| {
            if Self::epoch(vault, id)?.0 != pending.generation {
                Self::audit(
                    vault,
                    now_ms,
                    id,
                    "oauth.refused",
                    actor,
                    Some("superseded"),
                )?;
                return Ok(Decided::Refused(ConnError::Conflict));
            }
            Self::store(vault, id, record, &tokens, true, now_ms)?;
            Self::audit(
                vault,
                now_ms,
                id,
                "oauth.connected",
                actor,
                Some(&provider.id),
            )?;
            Ok(Decided::Done(()))
        })?
        .into_result()?;
        self.status(id)
    }

    /// `DELETE`: disconnect (CONN-GENERATION-1). In one transaction: the next
    /// generation, the credential and every pending authorization of `id`
    /// deleted, audited. Only when `revoke_grant` (GRANT-REVOKE-1) and the
    /// provider has an RFC 7009 endpoint is the old grant then revoked there
    /// with the sealed tokens (best effort, audited); nothing that call does
    /// touches a newer credential.
    ///
    /// # Errors
    /// Storage failures.
    pub fn revoke(
        &self,
        actor: &str,
        id: &str,
        revoke_grant: bool,
        now_ms: u64,
    ) -> Result<Status, ConnError> {
        let held = self.transaction(|vault| {
            // A credential that doesn't open is still deleted.
            let tokens = vault
                .get(&slot(id))
                .ok()
                .flatten()
                .and_then(|(_, secret)| opened::<Tokens>(&secret).ok());
            let pending: Vec<String> = vault
                .engine()
                .query(
                    r#"SELECT "state_hash" FROM "_norn_oauth_state" WHERE "connection" = ?1"#,
                    &[text(id)],
                )
                .map_err(storage)?
                .iter()
                .filter_map(|row| get_text(row, 0))
                .collect();
            for hash in &pending {
                Self::forget_state(vault, hash)?;
            }
            Self::advance(vault, id, true)?;
            vault.delete(&slot(id))?;
            vault
                .engine()
                .execute(
                    r#"DELETE FROM "_norn_connection" WHERE "id" = ?1"#,
                    &[text(id)],
                )
                .map_err(storage)?;
            Self::audit(vault, now_ms, id, "credential.revoke", actor, None)?;
            Ok(tokens)
        })?;
        if revoke_grant
            && let Some(tokens) = held
            && let Ok(provider) = self.provider(&tokens.provider)
            && provider.revocation_url.is_some()
        {
            let outcome = self.revoke_at_provider(provider, &tokens);
            self.transaction(|vault| {
                Self::audit(
                    vault,
                    now_ms,
                    id,
                    "credential.provider_revoke",
                    actor,
                    Some(outcome),
                )
            })?;
        }
        self.status(id)
    }

    /// RFC 7009 with the sealed refresh token (else the access token), sent
    /// directly (OAUTH-DIRECT-1); a fixed outcome code.
    fn revoke_at_provider(&self, provider: &Provider, tokens: &Tokens) -> &'static str {
        let Ok(registered) = self.registered(provider) else {
            return "no_client";
        };
        let Some(Ok(url)) = provider
            .revocation_url
            .as_ref()
            .map(|url| provider.fill(url, &tokens.params))
        else {
            return "no_revocation_endpoint";
        };
        let (token, hint) = tokens
            .refresh_token
            .as_ref()
            .map_or((&tokens.access_token, "access_token"), |refresh| {
                (refresh, "refresh_token")
            });
        let mut form = form_urlencoded::Serializer::new(String::new());
        form.append_pair("token", token)
            .append_pair("token_type_hint", hint);
        let secret = registered
            .client_secret
            .as_ref()
            .and_then(Secret::expose_str)
            .filter(|_| provider.client_auth != ClientAuth::None);
        let mut request = http::Request::post(url)
            .header("content-type", "application/x-www-form-urlencoded")
            .header("accept", "application/json");
        match (provider.client_auth, secret) {
            (ClientAuth::Basic, Some(secret)) => {
                let pair = |text: &str| {
                    form_urlencoded::byte_serialize(text.as_bytes()).collect::<String>()
                };
                let credentials = zeroize::Zeroizing::new(format!(
                    "{}:{}",
                    pair(&registered.client_id),
                    pair(secret)
                ));
                let encoded = zeroize::Zeroizing::new(
                    base64::engine::general_purpose::STANDARD.encode(credentials.as_bytes()),
                );
                request = request.header("authorization", format!("Basic {}", encoded.as_str()));
                form.append_pair("client_id", &registered.client_id);
            }
            (ClientAuth::Post, Some(secret)) => {
                form.append_pair("client_id", &registered.client_id)
                    .append_pair("client_secret", secret);
            }
            (ClientAuth::Basic | ClientAuth::Post | ClientAuth::None, _) => {
                form.append_pair("client_id", &registered.client_id);
            }
        }
        let body = Secret::from(form.finish());
        let Ok(request) = request.body(body.expose().to_vec()) else {
            return "revocation_request_malformed";
        };
        match self.egress.send_direct(request) {
            Ok(response) if response.status().is_success() => "provider_revoked",
            Ok(_) => "provider_revoke_refused",
            Err(error) => egress_code(&error),
        }
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
            self.refresh(actor, id, (record.generation, record.revision), now_ms)?;
        }
        let (record, tokens) = self.read(id)?;
        self.transaction(|vault| {
            vault
                .engine()
                .execute(
                    r#"UPDATE "_norn_connection" SET "last_used_ms" = ?1 WHERE "id" = ?2 AND "revision" = ?3"#,
                    &[ms(now_ms), text(id), SqlValue::Integer(record.revision)],
                )
                .map_err(storage)?;
            Self::audit(vault, now_ms, id, "credential.use", actor, None)
        })?;
        Ok(Secret::from(tokens.access_token.clone()))
    }

    /// The record and sealed tokens of `id`, the sealed provider and
    /// parameters checked against the row (CONN-SEALED-BINDING-1).
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
            let tokens: Tokens = opened(&secret)?;
            if tokens.provider != record.provider || tokens.params != record.params {
                return Err(ConnError::Vault(VaultError::Integrity));
            }
            Ok((record, tokens))
        })
    }

    /// REFRESH-ONE-1: one refresh of `id` at a time; a caller that finds
    /// one in flight waits for it.
    fn refresh(
        &self,
        actor: &str,
        id: &str,
        seen: (i64, i64),
        now_ms: u64,
    ) -> Result<(), ConnError> {
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

    fn refresh_now(
        &self,
        actor: &str,
        id: &str,
        seen: (i64, i64),
        now_ms: u64,
    ) -> Result<(), ConnError> {
        let (record, tokens) = self.read(id)?;
        // A refresh that finished since this caller read is reused.
        if (record.generation, record.revision) != seen {
            return Ok(());
        }
        let Some(refresh) = tokens.refresh_token.clone() else {
            return self
                .mark_reconnect(actor, id, &record, "no_refresh_token", now_ms)
                .and(Err(ConnError::ReconnectNeeded));
        };
        let provider = self.provider(&tokens.provider)?;
        let client = self.client(provider, &tokens.params, true)?;
        let refreshed = client
            .exchange_refresh_token(&RefreshToken::new(refresh))
            .request(&|request| self.egress.send_direct(request))
            .map_err(oauth_error);
        let response = match refreshed {
            Ok(response) => response,
            Err(ConnError::Provider(code)) => {
                return self
                    .mark_reconnect(actor, id, &record, code, now_ms)
                    .and(Err(ConnError::ReconnectNeeded));
            }
            Err(error) => {
                let code = failure_code(&error);
                self.transaction(|vault| {
                    // Only the credential this refresh read gets the error.
                    vault
                        .engine()
                        .execute(
                            r#"UPDATE "_norn_connection" SET "last_error" = ?1 WHERE "id" = ?2 AND "generation" = ?3 AND "revision" = ?4"#,
                            &[text(code), text(id), SqlValue::Integer(seen.0), SqlValue::Integer(seen.1)],
                        )
                        .map_err(storage)?;
                    Self::audit(vault, now_ms, id, "credential.failure", actor, Some(code))
                })?;
                return Err(error);
            }
        };
        let access = response.access_token().secret().clone();
        let next = Record {
            last4: last4(&access),
            expires_at_ms: expires_at(now_ms, response.expires_in()),
            last_error: None,
            scopes: response.scopes().map_or_else(
                || record.scopes.clone(),
                |granted| granted.iter().map(|scope| scope.to_string()).collect(),
            ),
            ..record
        };
        // A rotated refresh token replaces the old one in this same write.
        let rotated = Tokens {
            provider: tokens.provider.clone(),
            params: tokens.params.clone(),
            access_token: access,
            refresh_token: response
                .refresh_token()
                .map(|token| token.secret().clone())
                .or_else(|| tokens.refresh_token.clone()),
        };
        self.transaction(|vault| {
            if Self::epoch(vault, id)? != seen {
                Self::audit(
                    vault,
                    now_ms,
                    id,
                    "credential.failure",
                    actor,
                    Some("superseded"),
                )?;
                return Ok(Decided::Refused(ConnError::Conflict));
            }
            Self::store(vault, id, next, &rotated, false, now_ms)?;
            Self::audit(vault, now_ms, id, "credential.refresh", actor, None)?;
            Ok(Decided::Done(()))
        })?
        .into_result()
    }

    /// Mark the credential this refresh read `reconnect_needed`, only if it
    /// is still current (CONN-GENERATION-1).
    fn mark_reconnect(
        &self,
        actor: &str,
        id: &str,
        record: &Record,
        code: &'static str,
        now_ms: u64,
    ) -> Result<(), ConnError> {
        self.transaction(|vault| {
            if Self::epoch(vault, id)? == (record.generation, record.revision) {
                let mut marked = record.clone();
                marked.state = State::ReconnectNeeded;
                marked.last_error = Some(code.to_owned());
                Self::write_record(vault, id, &marked, now_ms)?;
            }
            Self::audit(vault, now_ms, id, "credential.failure", actor, Some(code))
        })
    }
}
