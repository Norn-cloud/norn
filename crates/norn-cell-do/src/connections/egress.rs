//! The one outbound HTTP client for connector calls (token exchange,
//! refresh, revocation, provider API calls).
//!
//! EGRESS-URL-1: a URL is https with no credentials in it, to a host that is
//! not internal by the shared rules ([`norn_api::net`], NET-ADDR-1); plain
//! http or an internal host only when the operator lists the host
//! (`NORN_SOURCE_ALLOWED_HOSTS`, the same list sources use).
//! EGRESS-PIN-1: a name is resolved once per connection, every address it
//! resolves to is checked after resolution (any internal address refuses the
//! whole answer unless the host is listed), and the connection is made only to
//! the addresses checked, so a second answer (DNS rebinding) is never used.
//! EGRESS-REDIRECT-1: redirects are followed here, at most
//! [`MAX_REDIRECTS`], each hop's URL checked as the first. A hop that changes
//! origin keeps only [`CROSS_ORIGIN_SAFE_HEADERS`] (deny by default, the list
//! of `OpenConnector` v1.9.4 `src/core/guarded-fetch.ts`, Apache-2.0); 303, and
//! 301/302 after a POST, become a body-less GET.
//! EGRESS-PROXY-1: with `NORN_EGRESS_PROXY` set, every call goes through that
//! HTTP CONNECT proxy (for static egress addresses); the target URL and its
//! resolved addresses are still checked here first, and the proxy resolves the
//! name again, so the proxy must enforce its own address policy. Ambient
//! `HTTP(S)_PROXY` variables are ignored.
//! EGRESS-RETRY-1: idempotent methods are retried on a transport failure or a
//! 429/502/503/504 answer, at most [`MAX_ATTEMPTS`] times with exponential
//! backoff (or `Retry-After`, capped); a POST is sent once.

use std::fmt;
use std::io;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs as _};
use std::sync::Arc;
use std::time::Duration;

use http::header::{HeaderName, LOCATION, RETRY_AFTER};
use http::{HeaderMap, Method, Request, Response};
use ureq::config::Config;
use ureq::unversioned::resolver::{ResolvedSocketAddrs, Resolver};
use ureq::unversioned::transport::{DefaultConnector, NextTimeout};

/// The server variable naming the egress proxy, `http://host:port`.
pub const PROXY_VAR: &str = "NORN_EGRESS_PROXY";
/// Most redirect hops followed.
pub const MAX_REDIRECTS: usize = 10;
/// Most attempts of an idempotent request.
pub const MAX_ATTEMPTS: u32 = 3;
/// Largest response body read, in bytes.
pub const MAX_BODY_BYTES: u64 = 2 * 1024 * 1024;

/// Headers a redirect that changes origin keeps; every other header (any
/// credential, whatever its name) is dropped. From `OpenConnector` v1.9.4
/// (`crossOriginSafeHeaders`, sha d89dc9f2, Apache-2.0).
pub const CROSS_ORIGIN_SAFE_HEADERS: [&str; 23] = [
    "accept",
    "accept-charset",
    "accept-encoding",
    "accept-language",
    "cache-control",
    "content-disposition",
    "content-encoding",
    "content-language",
    "content-length",
    "content-md5",
    "content-range",
    "content-type",
    "dnt",
    "idempotency-key",
    "if-match",
    "if-modified-since",
    "if-none-match",
    "if-unmodified-since",
    "range",
    "user-agent",
    "x-correlation-id",
    "x-request-id",
    "x-trace",
];

/// Body-describing headers dropped when a redirect turns a request into a
/// GET (the fetch spec's list, as `OpenConnector` applies it).
const BODY_HEADERS: [&str; 5] = [
    "content-encoding",
    "content-language",
    "content-length",
    "content-location",
    "content-type",
];

/// Why an outbound call did not complete. Never carries a header value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EgressError {
    /// The URL or a resolved address is refused (EGRESS-URL-1, EGRESS-PIN-1).
    Forbidden(String),
    /// The name did not resolve.
    Unresolved(String),
    /// Too many redirects, or one without a usable `Location`.
    Redirect(String),
    /// The connection or exchange failed.
    Transport(String),
    /// The configuration is malformed.
    Config(String),
}

impl fmt::Display for EgressError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Forbidden(why) => write!(f, "egress refused: {why}"),
            Self::Unresolved(host) => write!(f, "{host} did not resolve"),
            Self::Redirect(why) => write!(f, "redirect refused: {why}"),
            Self::Transport(why) => write!(f, "request failed: {why}"),
            Self::Config(why) => write!(f, "egress configuration: {why}"),
        }
    }
}

impl std::error::Error for EgressError {}

/// Name resolution, injectable so tests can answer differently per call.
pub trait Lookup: fmt::Debug + Send + Sync + 'static {
    /// The addresses `host` resolves to.
    ///
    /// # Errors
    /// A failed lookup.
    fn lookup(&self, host: &str, port: u16) -> io::Result<Vec<IpAddr>>;
}

/// The operating system's resolver.
#[derive(Debug, Default)]
pub struct SystemLookup;

impl Lookup for SystemLookup {
    fn lookup(&self, host: &str, port: u16) -> io::Result<Vec<IpAddr>> {
        Ok((host, port)
            .to_socket_addrs()?
            .map(|addr| addr.ip())
            .collect())
    }
}

/// A refusal raised inside the resolver, carried out through ureq's error.
#[derive(Debug)]
struct Refused(EgressError);

impl fmt::Display for Refused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for Refused {}

#[derive(Debug)]
struct Policy {
    allowed_hosts: Vec<String>,
    proxy: Option<(String, u16)>,
}

impl Policy {
    fn listed(&self, host: &str) -> bool {
        self.allowed_hosts.iter().any(|allowed| allowed == host)
    }

    /// EGRESS-URL-1.
    fn check_url(&self, url: &url::Url) -> Result<(), EgressError> {
        if !url.username().is_empty() || url.password().is_some() {
            return Err(EgressError::Forbidden(
                "a URL carries credentials".to_owned(),
            ));
        }
        let (Some(host), Some(name)) = (url.host(), url.host_str()) else {
            return Err(EgressError::Forbidden("a URL has no host".to_owned()));
        };
        if self.listed(name) {
            return matches!(url.scheme(), "https" | "http")
                .then_some(())
                .ok_or_else(|| EgressError::Forbidden(format!("scheme {}", url.scheme())));
        }
        if url.scheme() != "https" {
            return Err(EgressError::Forbidden(format!(
                "{name}: only https to a public host"
            )));
        }
        if norn_api::net::internal_host(&host) {
            return Err(EgressError::Forbidden(format!("{name} is internal")));
        }
        Ok(())
    }
}

/// EGRESS-PIN-1: resolves once and returns only checked addresses, which
/// ureq then connects to.
#[derive(Debug)]
struct PinnedResolver {
    lookup: Arc<dyn Lookup>,
    policy: Arc<Policy>,
}

fn host_port(uri: &http::Uri) -> Option<(String, u16)> {
    let host = uri.host()?.trim_start_matches('[').trim_end_matches(']');
    let port = uri.port_u16().or_else(|| match uri.scheme_str() {
        Some("https") => Some(443),
        Some("http") => Some(80),
        _ => None,
    })?;
    Some((host.to_ascii_lowercase(), port))
}

fn refused(error: EgressError) -> ureq::Error {
    ureq::Error::Io(io::Error::other(Refused(error)))
}

impl PinnedResolver {
    fn checked(&self, host: &str, port: u16) -> Result<Vec<IpAddr>, EgressError> {
        // The configured proxy is the operator's own; its address is not checked.
        let is_proxy = self
            .policy
            .proxy
            .as_ref()
            .is_some_and(|(proxy, proxy_port)| proxy == host && *proxy_port == port);
        let addresses = match host.parse::<IpAddr>() {
            Ok(ip) => vec![ip],
            Err(_) => self
                .lookup
                .lookup(host, port)
                .map_err(|_error| EgressError::Unresolved(host.to_owned()))?,
        };
        if addresses.is_empty() {
            return Err(EgressError::Unresolved(host.to_owned()));
        }
        let listed = is_proxy
            || self.policy.listed(
                &url::Host::parse(host).map_or_else(|_| host.to_owned(), |h| h.to_string()),
            );
        if !listed
            && let Some(internal) = addresses.iter().find(|ip| norn_api::net::internal_ip(**ip))
        {
            return Err(EgressError::Forbidden(format!(
                "{host} resolved to internal address {internal}"
            )));
        }
        Ok(addresses)
    }
}

impl Resolver for PinnedResolver {
    fn resolve(
        &self,
        uri: &http::Uri,
        _config: &Config,
        _timeout: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, ureq::Error> {
        let (host, port) = host_port(uri)
            .ok_or_else(|| refused(EgressError::Forbidden("a URL has no host".to_owned())))?;
        let addresses = self.checked(&host, port).map_err(refused)?;
        let mut out = self.empty();
        for ip in addresses.into_iter().take(16) {
            out.push(SocketAddr::new(ip, port));
        }
        Ok(out)
    }
}

/// An outbound response, read whole.
pub type EgressResponse = Response<Vec<u8>>;

/// The egress client. Cheap to share; one per host.
pub struct Egress {
    agent: ureq::Agent,
    resolver: Arc<PinnedResolver>,
    sleep: fn(Duration),
}

impl fmt::Debug for Egress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Egress")
            .field("policy", &self.resolver.policy)
            .finish_non_exhaustive()
    }
}

/// [`Resolver`] over a shared [`PinnedResolver`], so the client and the agent
/// check with one policy.
#[derive(Debug)]
struct Shared(Arc<PinnedResolver>);

impl Resolver for Shared {
    fn resolve(
        &self,
        uri: &http::Uri,
        config: &Config,
        timeout: NextTimeout,
    ) -> Result<ResolvedSocketAddrs, ureq::Error> {
        self.0.resolve(uri, config, timeout)
    }
}

fn std_sleep(duration: Duration) {
    std::thread::sleep(duration);
}

impl Egress {
    /// A client checking with `allowed_hosts` (the parsed
    /// `NORN_SOURCE_ALLOWED_HOSTS`), resolving with `lookup`, through `proxy`
    /// (the `NORN_EGRESS_PROXY` value) when it is set.
    ///
    /// # Errors
    /// A malformed proxy URL.
    pub fn new(
        allowed_hosts: Vec<String>,
        proxy: Option<&str>,
        lookup: Arc<dyn Lookup>,
    ) -> Result<Self, EgressError> {
        Self::with_connect_timeout(allowed_hosts, proxy, lookup, Duration::from_secs(10))
    }

    /// [`Self::new`] with a connect timeout other than ten seconds.
    ///
    /// # Errors
    /// A malformed proxy URL.
    pub fn with_connect_timeout(
        allowed_hosts: Vec<String>,
        proxy: Option<&str>,
        lookup: Arc<dyn Lookup>,
        connect_timeout: Duration,
    ) -> Result<Self, EgressError> {
        let proxy = proxy
            .map(str::trim)
            .filter(|proxy| !proxy.is_empty())
            .map(|proxy| {
                ureq::Proxy::new(proxy).map_err(|_error| {
                    EgressError::Config(format!("{PROXY_VAR} is not a proxy URL"))
                })
            })
            .transpose()?;
        let policy = Arc::new(Policy {
            allowed_hosts,
            proxy: proxy
                .as_ref()
                .map(|proxy| (proxy.host().to_ascii_lowercase(), proxy.port())),
        });
        let config = Config::builder()
            .proxy(proxy)
            .max_redirects(0)
            .max_redirects_will_error(false)
            .http_status_as_error(false)
            .timeout_connect(Some(connect_timeout))
            .timeout_global(Some(Duration::from_secs(30)))
            .user_agent("norn-connect/1")
            .build();
        let resolver = Arc::new(PinnedResolver { lookup, policy });
        let agent = ureq::Agent::with_parts(
            config,
            DefaultConnector::default(),
            Shared(Arc::clone(&resolver)),
        );
        Ok(Self {
            agent,
            resolver,
            sleep: std_sleep,
        })
    }

    /// The same client with `sleep` between retries (tests pass a no-op).
    #[must_use]
    pub fn with_sleep(mut self, sleep: fn(Duration)) -> Self {
        self.sleep = sleep;
        self
    }

    /// Send `request`, following redirects (EGRESS-REDIRECT-1) and retrying
    /// idempotent methods (EGRESS-RETRY-1).
    ///
    /// # Errors
    /// [`EgressError`].
    pub fn send(&self, request: Request<Vec<u8>>) -> Result<EgressResponse, EgressError> {
        let (parts, body) = request.into_parts();
        let mut method = parts.method;
        let mut headers = parts.headers;
        let mut body = Some(body).filter(|body| !body.is_empty());
        let mut url = url::Url::parse(&parts.uri.to_string())
            .map_err(|_error| EgressError::Forbidden("not an absolute URL".to_owned()))?;
        for hop in 0..=MAX_REDIRECTS {
            self.resolver.policy.check_url(&url)?;
            if self.resolver.policy.proxy.is_some()
                && let (Some(host), Some(port)) = (url.host_str(), url.port_or_known_default())
            {
                // Through a proxy ureq does not resolve the target; check it here.
                self.resolver
                    .checked(host.trim_start_matches('[').trim_end_matches(']'), port)?;
            }
            let response = self.attempt(&method, &url, &headers, body.as_deref())?;
            let status = response.status().as_u16();
            if !matches!(status, 301 | 302 | 303 | 307 | 308) {
                return Ok(response);
            }
            let Some(location) = response
                .headers()
                .get(LOCATION)
                .and_then(|value| value.to_str().ok())
            else {
                return Ok(response);
            };
            if hop == MAX_REDIRECTS {
                return Err(EgressError::Redirect("too many redirects".to_owned()));
            }
            let next = url
                .join(location)
                .map_err(|_error| EgressError::Redirect("a Location is not a URL".to_owned()))?;
            if status == 303 || (matches!(status, 301 | 302) && method == Method::POST) {
                if method != Method::HEAD {
                    method = Method::GET;
                }
                body = None;
                for name in BODY_HEADERS {
                    headers.remove(name);
                }
            }
            if next.origin() != url.origin() {
                strip_cross_origin(&mut headers);
            }
            url = next;
        }
        Err(EgressError::Redirect("too many redirects".to_owned()))
    }

    fn attempt(
        &self,
        method: &Method,
        url: &url::Url,
        headers: &HeaderMap,
        body: Option<&[u8]>,
    ) -> Result<EgressResponse, EgressError> {
        let idempotent = matches!(
            *method,
            Method::GET | Method::HEAD | Method::PUT | Method::DELETE | Method::OPTIONS
        );
        let mut attempt: u32 = 0;
        loop {
            attempt = attempt.saturating_add(1);
            let retry_left = idempotent && attempt < MAX_ATTEMPTS;
            match self.once(method, url, headers, body) {
                Ok(response)
                    if retry_left
                        && matches!(response.status().as_u16(), 429 | 502 | 503 | 504) =>
                {
                    (self.sleep)(backoff(attempt, response.headers()));
                }
                Ok(response) => return Ok(response),
                Err(EgressError::Transport(_)) if retry_left => {
                    (self.sleep)(backoff(attempt, &HeaderMap::new()));
                }
                Err(error) => return Err(error),
            }
        }
    }

    fn once(
        &self,
        method: &Method,
        url: &url::Url,
        headers: &HeaderMap,
        body: Option<&[u8]>,
    ) -> Result<EgressResponse, EgressError> {
        let mut request = Request::builder().method(method.clone()).uri(url.as_str());
        for (name, value) in headers {
            request = request.header(name, value);
        }
        let request = request
            .body(body.map(<[u8]>::to_vec).unwrap_or_default())
            .map_err(|_error| EgressError::Transport("malformed request".to_owned()))?;
        let response = self.agent.run(request).map_err(|error| from_ureq(&error))?;
        let (parts, mut body) = response.into_parts();
        let bytes = body
            .with_config()
            .limit(MAX_BODY_BYTES)
            .read_to_vec()
            .map_err(|error| from_ureq(&error))?;
        Ok(Response::from_parts(parts, bytes))
    }
}

fn backoff(attempt: u32, headers: &HeaderMap) -> Duration {
    let base = Duration::from_millis(200_u64.saturating_mul(1 << attempt.min(6)));
    headers
        .get(RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map_or(base, |seconds| Duration::from_secs(seconds.min(30)))
}

fn from_ureq(error: &ureq::Error) -> EgressError {
    if let ureq::Error::Io(io) = error
        && let Some(Refused(inner)) = io
            .get_ref()
            .and_then(|inner| inner.downcast_ref::<Refused>())
    {
        return inner.clone();
    }
    if matches!(error, ureq::Error::HostNotFound) {
        EgressError::Unresolved("the host".to_owned())
    } else {
        EgressError::Transport(error.to_string())
    }
}

/// Keep only [`CROSS_ORIGIN_SAFE_HEADERS`].
fn strip_cross_origin(headers: &mut HeaderMap) {
    let drop: Vec<HeaderName> = headers
        .keys()
        .filter(|name| !CROSS_ORIGIN_SAFE_HEADERS.contains(&name.as_str()))
        .cloned()
        .collect();
    for name in drop {
        headers.remove(name);
    }
}
