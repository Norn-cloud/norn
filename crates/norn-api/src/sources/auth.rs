//! SOURCE-SCOPE-1 (#1702): a connection belongs to one cell scope and
//! resolves only the server secrets its own configuration binds.
//!
//! The source configuration and the `NORN_SECRET_*` variables are server
//! configuration every cell of a host is handed. Each connection names the
//! organization and environment that own it and the secret references it may
//! resolve. A cell keeps only the connections of its own scope and only the
//! secrets they bind ([`bind`]); one secret bound by connections of two
//! scopes refuses the whole configuration. At sync time a connector's
//! `secretRef` or `clientSecretRef` resolves only when the connection binds
//! it ([`bound_secret`]), and a connection whose scope is not the cell's
//! never starts ([`owns`]). Every other case fails closed.

use std::collections::{BTreeMap, BTreeSet};

use super::{ConnectionConfig, Failure, HttpRequest, fail};
use crate::scope::CellScope;

/// Whether `config` belongs to the cell serving `scope` (SOURCE-SCOPE-1).
#[must_use]
pub(super) fn owns(scope: &CellScope, config: &ConnectionConfig) -> bool {
    config.org_id == scope.org_id && config.environment_id == scope.environment_id
}

/// SOURCE-SCOPE-1: no secret is bound by connections of two scopes, so
/// whichever cell resolves it, only one scope ever could.
///
/// # Errors
/// The first secret bound in two scopes.
fn one_scope_per_secret(connections: &[ConnectionConfig]) -> Result<(), String> {
    let mut owners: BTreeMap<String, (&str, &str)> = BTreeMap::new();
    for connection in connections {
        let owner = (
            connection.org_id.as_str(),
            connection.environment_id.as_str(),
        );
        for name in connection
            .secret_refs
            .iter()
            .map(String::as_str)
            .map(secret_env)
        {
            if owners
                .insert(name.clone(), owner)
                .is_some_and(|other| other != owner)
            {
                return Err(format!(
                    "source configuration: {name} is bound by connections of two scopes"
                ));
            }
        }
    }
    Ok(())
}

/// SOURCE-SCOPE-1: the connections of `connections` that `scope` owns, and
/// the secrets of `secrets` they bind. No other connection or secret reaches
/// the cell.
///
/// # Errors
/// A secret bound by connections of two different scopes.
pub(super) fn bind(
    scope: &CellScope,
    connections: Vec<ConnectionConfig>,
    secrets: &BTreeMap<String, String>,
) -> Result<(Vec<ConnectionConfig>, BTreeMap<String, String>), String> {
    one_scope_per_secret(&connections)?;
    let owned: Vec<ConnectionConfig> = connections
        .into_iter()
        .filter(|connection| owns(scope, connection))
        .collect();
    let names: BTreeSet<String> = owned
        .iter()
        .flat_map(|connection| connection.secret_refs.iter().map(String::as_str))
        .map(secret_env)
        .collect();
    let secrets = secrets
        .iter()
        .filter(|(name, _)| names.contains(*name))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    Ok((owned, secrets))
}

/// The server secret `reference` names, when `config` binds it
/// (SOURCE-SCOPE-1): its environment name and value, if the server has one.
fn bound_secret<'a>(
    config: &ConnectionConfig,
    secrets: &'a BTreeMap<String, String>,
    reference: &str,
) -> Result<(String, Option<&'a String>), Failure> {
    let name = secret_env(reference);
    if !config
        .secret_refs
        .iter()
        .any(|bound| secret_env(bound) == name)
    {
        return Err(fail(
            "source.secret_unbound",
            format!("{} does not bind the secret {name}", config.id),
        ));
    }
    let value = secrets.get(&name);
    Ok((name, value))
}

/// For an `oauth2` connector, the client-credentials token request (RFC 6749
/// §4.4) to the connector's relative `tokenUrl` on the connection's base URL,
/// with the client id and secret from the `clientSecretRef` secret
/// (`<id>:<secret>`), which the connection must bind.
pub(super) fn client_credentials(
    payload: &serde_json::Value,
    secrets: &BTreeMap<String, String>,
    config: &ConnectionConfig,
) -> Result<Option<HttpRequest>, Failure> {
    let auth = payload.get("auth").cloned().unwrap_or_default();
    let field = |name: &str| auth.get(name).and_then(serde_json::Value::as_str);
    if field("kind") != Some("oauth2") {
        return Ok(None);
    }
    let path = field("tokenUrl")
        .filter(|p| p.starts_with('/') && !p.contains("://"))
        .ok_or_else(|| {
            fail(
                "source.auth_unsupported",
                "an oauth2 tokenUrl is a path on the connection",
            )
        })?;
    let (name, value) = bound_secret(
        config,
        secrets,
        field("clientSecretRef").unwrap_or_default(),
    )?;
    let (id, secret) = value.and_then(|pair| pair.split_once(':')).ok_or_else(|| {
        fail(
            "source.secret_missing",
            format!("the server has no {name} (<client id>:<client secret>)"),
        )
    })?;
    Ok(Some(HttpRequest {
        method: "POST",
        url: format!("{}{path}", config.base_url.trim_end_matches('/')),
        headers: vec![(
            "Content-Type".into(),
            "application/x-www-form-urlencoded".into(),
        )],
        body: Some(format!(
            "grant_type=client_credentials&client_id={}&client_secret={}",
            crate::uploads::encode(id, false),
            crate::uploads::encode(secret, false)
        )),
    }))
}

/// The connector's declared header auth, resolved from the server secrets
/// the connection binds.
pub(super) fn auth_headers(
    payload: &serde_json::Value,
    secrets: &BTreeMap<String, String>,
    config: &ConnectionConfig,
) -> Result<Vec<(String, String)>, Failure> {
    let auth = payload.get("auth").cloned().unwrap_or_default();
    let field = |name: &str| auth.get(name).and_then(serde_json::Value::as_str);
    match field("kind") {
        Some("none") => Ok(Vec::new()),
        Some("api_key") if field("placement") == Some("header") => {
            let reference = field("secretRef").unwrap_or_default();
            let (name, value) = bound_secret(config, secrets, reference)?;
            let secret = value.filter(|s| !s.is_empty()).ok_or_else(|| {
                fail("source.secret_missing", format!("the server has no {name}"))
            })?;
            let header = field("name").unwrap_or("Authorization");
            Ok(vec![(
                header.to_owned(),
                format!("{}{secret}", field("prefix").unwrap_or_default()),
            )])
        }
        Some(_) | None => Err(fail(
            "source.auth_unsupported",
            "this connector's auth is not supported by the pull host",
        )),
    }
}

/// The environment name of a secret reference: `erpnext_rest_token` is
/// `NORN_SECRET_ERPNEXT_REST_TOKEN`.
#[must_use]
pub fn secret_env(reference: &str) -> String {
    let mut name = String::from("NORN_SECRET_");
    name.extend(reference.chars().map(|c| {
        if c.is_ascii_alphanumeric() {
            c.to_ascii_uppercase()
        } else {
            '_'
        }
    }));
    name
}
