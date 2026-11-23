//! OAuth provider metadata, generated from `OpenConnector` v1.9.4 (commit
//! d89dc9f2, Apache-2.0; NOTICE carried in the file) by
//! `scripts/connections/extract_openconnector.py` into
//! `registry/connections/providers.json`. Each entry's `auth` is a
//! `norn_ir::ConnectorAuth::Oauth2` value, checked as one when loaded.
//!
//! PROVIDER-WITHHELD-1: an entry the generator marks `withheld` (Shopify,
//! until its adapter exists) keeps its metadata, and every connection call
//! naming it is refused with that reason before anything is stored or sent.
//! PROVIDER-URL-1: a provider URL template parameter (Shopify's
//! `shopSubdomain`) is a lowercase DNS label; the filled URL is still checked
//! by the egress client before any call.

use std::collections::BTreeMap;

use norn_ir::ConnectorAuth;
use serde::Deserialize;

const PROVIDERS_JSON: &str = include_str!("../../../../registry/connections/providers.json");

/// How the client authenticates at the token endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientAuth {
    /// `client_id` and `client_secret` in the form body.
    Post,
    /// HTTP Basic.
    Basic,
    /// A public client: `client_id` only.
    None,
}

/// One provider's OAuth metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provider {
    /// Norn's provider id, e.g. `github`.
    pub id: String,
    /// The source system, e.g. `google`.
    pub system: String,
    /// The authorization endpoint (may hold `{param}` templates).
    pub authorization_url: String,
    /// The token endpoint (may hold `{param}` templates).
    pub token_url: String,
    /// The refresh endpoint, when it differs from the token endpoint.
    pub refresh_token_url: Option<String>,
    /// The RFC 7009 revocation endpoint, if any.
    pub revocation_url: Option<String>,
    /// Scopes requested by default.
    pub scopes: Vec<String>,
    /// Further scopes a caller may request.
    pub optional_scopes: Vec<String>,
    /// How scopes are joined.
    pub scope_separator: String,
    /// Token endpoint client authentication.
    pub client_auth: ClientAuth,
    /// Whether the provider requires PKCE (Norn always sends S256).
    pub pkce_required: bool,
    /// Static authorization URL parameters.
    pub authorization_params: BTreeMap<String, String>,
    /// URL template parameters a connection must supply.
    pub url_parameters: Vec<String>,
    /// Why the host refuses to connect this provider yet
    /// (PROVIDER-WITHHELD-1); `None` when it is enabled.
    pub withheld: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct File {
    providers: Vec<Entry>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    id: String,
    system: String,
    #[serde(default)]
    withheld: Option<String>,
    auth: ConnectorAuth,
}

fn text(extra: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<String> {
    extra.get(key).and_then(|v| v.as_str()).map(str::to_owned)
}

fn texts(extra: &serde_json::Map<String, serde_json::Value>, key: &str) -> Vec<String> {
    extra
        .get(key)
        .and_then(|v| v.as_array())
        .map_or_default(|items| {
            items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
}

/// The generated providers, by id.
///
/// # Errors
/// The generated file does not parse as connector auth.
pub fn load() -> Result<BTreeMap<String, Provider>, String> {
    let file: File =
        serde_json::from_str(PROVIDERS_JSON).map_err(|e| format!("providers.json: {e}"))?;
    file.providers
        .into_iter()
        .map(|entry| {
            let ConnectorAuth::Oauth2 {
                authorization_url,
                token_url,
                scopes,
                additional,
                ..
            } = entry.auth
            else {
                return Err(format!("provider {} is not oauth2", entry.id));
            };
            let extra = serde_json::to_value(&additional)
                .ok()
                .and_then(|v| v.as_object().cloned())
                .unwrap_or_default();
            let client_auth = match text(&extra, "tokenEndpointAuthMethod").as_deref() {
                Some("client_secret_post") => ClientAuth::Post,
                Some("client_secret_basic") => ClientAuth::Basic,
                Some("none") => ClientAuth::None,
                other => return Err(format!("provider {}: client auth {other:?}", entry.id)),
            };
            let provider = Provider {
                id: entry.id.clone(),
                system: entry.system,
                authorization_url,
                token_url,
                refresh_token_url: text(&extra, "refreshTokenUrl"),
                revocation_url: text(&extra, "revocationUrl"),
                scopes,
                optional_scopes: texts(&extra, "optionalScopes"),
                scope_separator: text(&extra, "scopeSeparator").unwrap_or_else(|| " ".to_owned()),
                client_auth,
                pkce_required: extra
                    .get("pkceRequired")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
                authorization_params: extra
                    .get("authorizationParams")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default(),
                url_parameters: texts(&extra, "urlParameters"),
                withheld: entry.withheld,
            };
            Ok((entry.id, provider))
        })
        .collect()
}

fn label(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && !value.starts_with('-')
        && !value.ends_with('-')
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

impl Provider {
    /// `template` with every `{param}` filled from `params` (PROVIDER-URL-1).
    ///
    /// # Errors
    /// A missing parameter, or one that is not a lowercase DNS label.
    pub fn fill(
        &self,
        template: &str,
        params: &BTreeMap<String, String>,
    ) -> Result<String, String> {
        let mut url = template.to_owned();
        for name in &self.url_parameters {
            let value = params
                .get(name)
                .ok_or_else(|| format!("parameter {name} is required"))?;
            if !label(value) {
                return Err(format!("parameter {name} must be a lowercase DNS label"));
            }
            url = url.replace(&format!("{{{name}}}"), value);
        }
        if url.contains('{') {
            return Err("the provider URL has an unfilled parameter".to_owned());
        }
        Ok(url)
    }

    /// Whether every one of `scopes` is a declared default or optional scope.
    #[must_use]
    pub fn offers(&self, scopes: &[String]) -> bool {
        scopes
            .iter()
            .all(|scope| self.scopes.contains(scope) || self.optional_scopes.contains(scope))
    }
}
