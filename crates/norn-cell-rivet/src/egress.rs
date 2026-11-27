//! #1710 (NET-ADDR-1, EGRESS-PIN-1): the host's outbound clients. A source
//! sync's base URL is checked by its literal host when it is configured
//! (SYNC-URL-1, `norn_api::sources`), but a name can resolve to anything,
//! and again to something else at connect time. So the client a sync's
//! requests go out on resolves each name itself, refuses it when any address
//! it resolves to is internal (unless the operator lists the host in
//! `NORN_SOURCE_ALLOWED_HOSTS`), and connects only to the addresses it
//! checked: there is no second lookup to rebind. It uses no proxy (a proxy
//! would resolve the name again) and follows no redirect, as the connection
//! egress client does (`norn_cell_do::connections::egress`). A URL whose
//! host is an IP literal is never resolved, so this resolver never sees it;
//! it is covered by the configuration-time check of the base URL's literal
//! host (SYNC-URL-1), which refuses an internal address the operator does
//! not list.
//!
//! Every other outbound call (the issuer's keys, the analytics engine,
//! presigned storage reads) goes to an endpoint the operator configured, on
//! [`Outbound::any`].

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow};
use reqwest::dns::{Addrs, Name, Resolve, Resolving};

/// A hung DNS lookup must not stall a source sync indefinitely.
const DNS_TIMEOUT: Duration = Duration::from_secs(5);
/// A source page or an uploaded object, read once and bounded.
const MAX_SOURCE_BODY: usize = 20 * 1024 * 1024;
/// One outbound source or storage request may take this long.
const SOURCE_FETCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Perform exactly the request the cell planned, on `http`; the status and
/// bytes go back to the cell, unread here. A network failure is status 0.
pub(crate) async fn perform(http: &reqwest::Client, planned: &str) -> (u16, Vec<u8>) {
    let Ok(planned) = serde_json::from_str::<serde_json::Value>(planned) else {
        return (0, Vec::new());
    };
    let Some(url) = planned.get("url").and_then(serde_json::Value::as_str) else {
        return (0, Vec::new());
    };
    let method = planned
        .get("method")
        .and_then(serde_json::Value::as_str)
        .and_then(|method| reqwest::Method::from_bytes(method.as_bytes()).ok())
        .unwrap_or(reqwest::Method::GET);
    let mut request = http.request(method, url).timeout(SOURCE_FETCH_TIMEOUT);
    if let Some(headers) = planned
        .get("headers")
        .and_then(serde_json::Value::as_object)
    {
        for (name, value) in headers {
            if let Some(value) = value.as_str() {
                request = request.header(name.as_str(), value);
            }
        }
    }
    if let Some(body) = planned.get("body").and_then(serde_json::Value::as_str) {
        request = request.body(body.to_owned());
    }
    match request.send().await {
        Ok(response) => {
            let status = response.status().as_u16();
            read_bounded(response, MAX_SOURCE_BODY)
                .await
                .map_or((413, Vec::new()), |bytes| (status, bytes))
        }
        Err(_error) => (0, Vec::new()),
    }
}

/// A response body counted as it arrives; `None` once it passes `limit`.
pub(crate) async fn read_bounded(mut response: reqwest::Response, limit: usize) -> Option<Vec<u8>> {
    let mut bytes = Vec::new();
    loop {
        match response.chunk().await {
            Ok(Some(chunk)) => {
                if bytes.len().saturating_add(chunk.len()) > limit {
                    return None;
                }
                bytes.extend_from_slice(&chunk);
            }
            Ok(None) => return Some(bytes),
            Err(_error) => return None,
        }
    }
}

/// The host's outbound clients.
#[derive(Debug, Clone)]
pub struct Outbound {
    /// For the endpoints the operator configured.
    pub any: reqwest::Client,
    /// For a source sync's planned requests: resolved addresses checked and
    /// pinned.
    pub sources: reqwest::Client,
}

impl Outbound {
    /// The clients, the sync client listing the hosts of the
    /// `NORN_SOURCE_ALLOWED_HOSTS` value in `connect_vars` (a JSON object, as
    /// [`crate::config::HostConfig::connect_vars`]).
    ///
    /// # Errors
    /// The TLS backend failed to initialize, or the allowlist is malformed.
    pub fn new(connect_vars: &str) -> Result<Self> {
        let vars: serde_json::Value = serde_json::from_str(connect_vars).unwrap_or_default();
        let listed = vars
            .get(norn_api::sources::ALLOWED_HOSTS_VAR)
            .and_then(serde_json::Value::as_str)
            .map(norn_api::sources::allowed_host_list)
            .transpose()
            .map_err(|error| anyhow!(error))?
            .unwrap_or_default();
        Ok(Self {
            any: outbound_client()?,
            sources: pinned_client(listed)?,
        })
    }
}

/// An outbound client that never follows a redirect.
///
/// # Errors
/// The TLS backend failed to initialize.
pub fn outbound_client() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .context("build the outbound HTTP client")
}

/// A client that resolves every name itself, refusing one that resolves to
/// an internal address unless `listed` names it, and connects only to the
/// addresses it checked; with no proxy and no redirects.
///
/// # Errors
/// The TLS backend failed to initialize.
pub fn pinned_client(listed: Vec<String>) -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .dns_resolver(Arc::new(Pinned {
            listed: Arc::from(listed),
        }))
        .build()
        .context("build the source sync HTTP client")
}

/// Resolves a name and hands the connector only addresses it checked.
struct Pinned {
    listed: Arc<[String]>,
}

impl Resolve for Pinned {
    fn resolve(&self, name: Name) -> Resolving {
        let listed = Arc::clone(&self.listed);
        Box::pin(async move {
            let host = name.as_str().to_owned();
            let resolved: Vec<SocketAddr> =
                tokio::time::timeout(DNS_TIMEOUT, tokio::net::lookup_host((host.as_str(), 0)))
                    .await
                    .map_err(|_elapsed| format!("{host} DNS lookup timed out"))??
                    .collect();
            let checked = checked(&host, resolved, &listed)?;
            let addrs: Addrs = Box::new(checked.into_iter());
            Ok::<_, Box<dyn std::error::Error + Send + Sync>>(addrs)
        })
    }
}

/// `resolved`, when `host` may be reached at them: some address, and none
/// internal unless the operator lists `host`.
///
/// # Errors
/// No address, or an internal one for a host not listed.
fn checked(
    host: &str,
    resolved: Vec<SocketAddr>,
    listed: &[String],
) -> Result<Vec<SocketAddr>, String> {
    if resolved.is_empty() {
        return Err(format!("{host} resolved to no address"));
    }
    let normalized = host.trim_end_matches('.');
    if listed
        .iter()
        .any(|entry| entry.trim_end_matches('.').eq_ignore_ascii_case(normalized))
    {
        return Ok(resolved);
    }
    match resolved
        .iter()
        .find(|address| norn_api::net::internal_ip(address.ip()))
    {
        Some(internal) => Err(format!(
            "{host} resolved to internal address {}",
            internal.ip()
        )),
        None => Ok(resolved),
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};

    use super::*;

    fn at(ip: &str) -> SocketAddr {
        SocketAddr::new(ip.parse().unwrap(), 0)
    }

    #[test]
    fn a_name_resolving_to_an_internal_address_is_refused_unless_listed() {
        let listed = vec!["metrics.internal.example".to_owned()];
        for internal in ["127.0.0.1", "10.0.0.7", "169.254.169.254", "::1", "fd00::1"] {
            // One internal address among public ones refuses the name.
            let resolved = vec![at("93.184.216.34"), at(internal)];
            assert!(
                checked("api.example.com", resolved.clone(), &listed).is_err(),
                "{internal}"
            );
            assert_eq!(
                checked("metrics.internal.example", resolved.clone(), &listed),
                Ok(resolved.clone())
            );
            assert_eq!(
                checked("METRICS.INTERNAL.EXAMPLE.", resolved.clone(), &listed),
                Ok(resolved)
            );
        }
        let public = vec![at("93.184.216.34"), at("2606:2800:220:1::1")];
        assert_eq!(checked("api.example.com", public.clone(), &[]), Ok(public));
        assert!(checked("api.example.com", Vec::new(), &listed).is_err());
    }

    /// End to end: a sync request to a name that resolves to loopback never
    /// connects unless the name is listed, and then connects to the address
    /// the resolver checked.
    #[tokio::test]
    async fn the_sync_client_connects_only_to_checked_addresses() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let served = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let _read = stream.read(&mut request).unwrap();
            stream
                .write_all(b"HTTP/1.1 204 No Content\r\nconnection: close\r\n\r\n")
                .unwrap();
        });
        let url = format!("http://localhost:{port}/");
        let unlisted = pinned_client(Vec::new()).unwrap();
        let refused = unlisted.get(&url).send().await;
        assert!(refused.is_err(), "{refused:?}");
        let listed = pinned_client(vec!["localhost".to_owned()]).unwrap();
        let answered = listed.get(&url).send().await.unwrap();
        assert_eq!(answered.status().as_u16(), 204);
        served.join().unwrap();
    }

    #[test]
    fn the_allowlist_is_read_from_the_connection_variables() {
        assert!(Outbound::new("{}").is_ok());
        assert!(Outbound::new(r#"{"NORN_SOURCE_ALLOWED_HOSTS":"a.example, b"}"#).is_ok());
        assert!(
            Outbound::new(r#"{"NORN_SOURCE_ALLOWED_HOSTS":"https://a.example:8443/x"}"#).is_err()
        );
    }
}
